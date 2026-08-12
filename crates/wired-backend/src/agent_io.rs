//! Putting words and answers into the live agent session.
//!
//! Three callers need this and must behave identically: the REST API, the chat
//! bridge, and the scheduler. Keeping it in one place is also what stops the
//! approval keystrokes from drifting apart between the button in the app and
//! the button in Telegram.

use std::sync::LazyLock;

use regex::Regex;

use crate::assistant::Assistant;
use crate::keys::{encode_agent_message, SUBMIT_DELAY};
use crate::pty::PtyManager;

/// Start the assistant if it is not already running.
pub async fn ensure_session(assistant: &Assistant) -> Result<(), String> {
    let assistant = assistant.clone();
    // enable() forks a PTY and may reap a previous child — keep it off the runtime.
    tokio::task::spawn_blocking(move || assistant.enable())
        .await
        .map_err(|e| e.to_string())?
        .map(|_| ())
}

/// Type a message into the agent's composer, optionally pressing Enter.
///
/// Starts the assistant first when nothing is running, which is what lets a
/// message from a phone wake a session that exited overnight.
pub async fn say(
    manager: &PtyManager,
    assistant: &Assistant,
    text: &str,
    submit: bool,
) -> Result<(), String> {
    if !manager.running() {
        ensure_session(assistant)
            .await
            .map_err(|e| format!("Could not start your assistant: {e}"))?;
    }
    if !manager.running() {
        return Err("Your assistant isn't running.".to_string());
    }

    let (body, submit_key) = encode_agent_message(text, submit, true);
    manager.write(&body)?;
    if !submit_key.is_empty() {
        // Ink CLIs read text and a trailing CR in one write as a paste.
        tokio::time::sleep(SUBMIT_DELAY).await;
        manager.write(&submit_key)?;
    }
    Ok(())
}

/// Answer the approval dialog the agent is blocked on.
///
/// Both CLIs render approvals as a numbered menu, and both treat Escape as
/// "no, and let me say why". Sending the digit rather than a cursor movement
/// means the answer does not depend on where the highlight happens to be
/// sitting — but *which* digit has to be read off the screen rather than
/// assumed. The ordinary tool prompt puts "Yes" at 1; Claude Code's first-run
/// bypass-permissions consent puts "No, exit" there and "Yes, I accept" at 2,
/// so a hardcoded `1` silently quits the CLI on the one screen a new user is
/// guaranteed to meet.
pub async fn answer(manager: &PtyManager, allow: bool) -> Result<(), String> {
    let choice = if allow { Choice::Allow } else { Choice::Deny };
    decide(manager, choice).await
}

/// What a button press means. `Pick` carries the digit of a specific row, which
/// is what lets a selection menu — `/model` and friends — be driven from a phone
/// that has no arrow keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Allow,
    Deny,
    Pick(u8),
}

/// Send one answer to whatever modal the agent is sitting on.
pub async fn decide(manager: &PtyManager, choice: Choice) -> Result<(), String> {
    if !manager.running() {
        return Err("No active agent session".to_string());
    }
    let digit = match choice {
        // Escape is "no, and let me say why" in both CLIs, and it is also the
        // only answer that is safe when we cannot read the menu.
        Choice::Deny => return manager.write(b"\x1b"),
        Choice::Pick(digit) => digit,
        Choice::Allow => affirmative_choice(&manager.screen_text()).unwrap_or(b'1'),
    };
    manager.write(&[digit])?;
    tokio::time::sleep(SUBMIT_DELAY).await;
    manager.write(b"\r")
}

/// `  ❯ 2. Yes, I accept` → (2, "Yes, I accept"), box borders and highlight
/// marker included or not.
static MENU_OPTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[\s│┃|╎]*(?:[❯>▸*]\s*)?([1-9])[.)]\s+(\S[^│┃]*?)\s*[│┃|╎]?\s*$").unwrap()
});

static AFFIRMATIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(yes|y\b|allow|accept|approve|proceed|continue|confirm|ok\b)").unwrap()
});

/// One selectable row of the modal currently on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuOption {
    pub digit: u8,
    pub label: String,
}

/// The options of the menu currently painted on screen, in order.
///
/// Only the last run numbered 1, 2, 3… is returned, so a numbered list in the
/// agent's own prose cannot be mistaken for the dialog. Empty when nothing
/// menu-shaped is visible.
pub fn menu_options(screen: &str) -> Vec<MenuOption> {
    let mut group: Vec<MenuOption> = Vec::new();
    let mut last: Vec<MenuOption> = Vec::new();

    for caps in MENU_OPTION.captures_iter(screen) {
        let digit = caps[1].as_bytes()[0];
        let option = MenuOption {
            digit,
            label: caps[2].trim().to_string(),
        };
        if digit == b'1' + group.len() as u8 {
            group.push(option);
            continue;
        }
        if !group.is_empty() {
            last = std::mem::take(&mut group);
        }
        if digit == b'1' {
            group.push(option);
        }
    }
    if !group.is_empty() {
        last = group;
    }
    last
}

/// Which digit means "go ahead" on the menu currently painted on screen.
///
/// Returns `None` when nothing menu-shaped is visible, leaving the caller its
/// default.
fn affirmative_choice(screen: &str) -> Option<u8> {
    // "Yes" before "Yes, and don't ask again" — the plain approval is the one
    // the caller asked for, and it is always the earlier of the two.
    menu_options(screen)
        .into_iter()
        .find(|option| AFFIRMATIVE.is_match(&option.label))
        .map(|option| option.digit)
}

#[cfg(test)]
mod tests {
    use super::affirmative_choice;

    /// Claude Code's first-run consent, verbatim from a server session. "Yes"
    /// is second here; answering 1 exits the CLI.
    const BYPASS_CONSENT: &str = "\
  WARNING: Claude Code running in Bypass Permissions mode

  In Bypass Permissions mode, Claude Code will not ask for your approval before running
  potentially dangerous commands.

  By proceeding, you accept all responsibility for actions taken while running in Bypass
  Permissions mode.

  https://code.claude.com/docs/en/security

  ❯ 1. No, exit
  2. Yes, I accept

  Enter to confirm · Esc to cancel";

    /// The ordinary tool prompt, where "Yes" leads.
    const TOOL_PROMPT: &str = "\
│ Bash command                                                    │
│   rm -rf build/                                                 │
│                                                                 │
│ Do you want to proceed?                                         │
│ ❯ 1. Yes                                                        │
│   2. Yes, and don't ask again for rm commands in /home/ubuntu   │
│   3. No, and tell Claude what to do differently (esc)           │";

    #[test]
    fn picks_yes_when_it_is_not_first() {
        assert_eq!(affirmative_choice(BYPASS_CONSENT), Some(b'2'));
    }

    #[test]
    fn picks_the_plain_yes_over_yes_and_dont_ask_again() {
        assert_eq!(affirmative_choice(TOOL_PROMPT), Some(b'1'));
    }

    #[test]
    fn ignores_a_numbered_list_in_the_agents_own_prose() {
        let screen = "\
● Here is what I would do:
  1. Delete the stale branches
  2. Rerun the failing test

❯";
        // Nothing affirmative in that list, so the caller keeps its default.
        assert_eq!(affirmative_choice(screen), None);
    }

    #[test]
    fn prefers_the_dialog_over_an_earlier_list() {
        let screen = "\
● Options I considered:
  1. Accept the risk
  2. Bail out

  Do you want to proceed?
  1. No, exit
  2. Yes, I accept";
        assert_eq!(affirmative_choice(screen), Some(b'2'));
    }

    #[test]
    fn no_menu_means_no_answer() {
        assert_eq!(affirmative_choice("❯ just a composer\n"), None);
    }
}

#[cfg(test)]
mod menu_tests {
    use super::{menu_options, MenuOption};

    /// `/model` on Claude Code 2.1.226, captured off a real session.
    const MODEL_PICKER: &str = "\
  Select model
  Switch between Claude models. Your pick becomes the default for new sessions.

  ❯ 1. Default (recommended) ✔  Sonnet 5 · Efficient for routine tasks
  2. Sonnet                   Sonnet 5 · Efficient for routine tasks
  3. Fable                    Fable 5 · Most capable for your hardest tasks
  · Requires usage credits
  4. Opus                     Opus 5 · Best for everyday, complex tasks
  5. Haiku                    Haiku 4.5 · Fastest for quick answers

  Enter to set as default · s to use this session only · Esc to cancel";

    #[test]
    fn reads_every_row_of_a_real_picker() {
        let options = menu_options(MODEL_PICKER);
        let digits: Vec<char> = options.iter().map(|o| o.digit as char).collect();
        assert_eq!(digits, ['1', '2', '3', '4', '5']);
    }

    /// The unnumbered "· Requires usage credits" continuation sits between two
    /// options and must not break the run.
    #[test]
    fn a_continuation_line_does_not_split_the_menu() {
        let options = menu_options(MODEL_PICKER);
        assert_eq!(options.len(), 5);
        assert!(options[3].label.starts_with("Opus"));
    }

    #[test]
    fn no_menu_means_no_options() {
        assert_eq!(
            menu_options("❯ just a composer\n"),
            Vec::<MenuOption>::new()
        );
    }
}
