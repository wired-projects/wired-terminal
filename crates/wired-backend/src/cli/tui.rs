//! The slash-command TUI.
//!
//! A bare `wired` in a terminal is this, not `status`. The commands are the
//! same ones the argv parser already knows — the slash is how you pick one
//! without leaving the prompt. Piped, or with `--json`, the old default still
//! holds, because a cron line cannot answer a prompt.

use std::io::{self, Write};

use super::args::{self, Command};
use super::client::Result;
use super::cmd;
use super::profile::{Config, Target};
use super::service::Supervisor;
use super::ui::Ui;
use crate::VERSION;

/// Everyday commands on the opening screen. `/help` lists the rest.
const MENU: &[(&str, &str)] = &[
    ("/status", "service, agent, API and chat"),
    ("/ask …", "send a task and print the reply"),
    ("/watch", "live transcript (ctrl-c detaches)"),
    ("/approve", "answer a blocked prompt"),
    ("/setup", "guided first run"),
    ("/help", "every command"),
    ("/quit", "leave"),
];

const MORE: &[(&str, &str)] = &[
    ("/start", "start the service, then the agent"),
    ("/stop", "stop the service (`/stop --agent` keeps it)"),
    ("/restart", "restart the service"),
    ("/logs -f", "journal, or the log file"),
    ("/doctor", "every setup check, with an exit code"),
    ("/update", "is a newer version out"),
    ("/folder", "where the agent works"),
    ("/telegram", "bot token, or `off`"),
    ("/pair", "Telegram pairing requests"),
    ("/schedule", "scheduled tasks"),
    ("/remote", "saved servers for `--remote`"),
    ("/uninstall", "take this install off the machine"),
];

#[derive(Debug)]
pub enum Slash {
    Empty,
    Quit,
    Help(Option<String>),
    Version,
    Run(Command),
}

/// Only called with a terminal on stdin: `main` owns that decision, so the
/// rule and its fallback to `status` sit in one place.
pub async fn run(
    ui: &Ui,
    target: &Target,
    supervisor: &Supervisor,
    config: &mut Config,
) -> Result<i32> {
    banner(ui, target);
    menu(ui, MENU);
    ui.note("same commands as argv, with a slash");
    println!();

    let stdin = io::stdin();
    let prompt = ui.dim(">");
    let mut line = String::new();
    loop {
        print!("{prompt} ");
        let _ = io::stdout().flush();
        line.clear();
        let n = stdin
            .read_line(&mut line)
            .map_err(|e| format!("could not read: {e}"))?;
        if n == 0 {
            println!();
            break;
        }

        match parse_slash(&line) {
            Ok(Slash::Empty) => {}
            Ok(Slash::Quit) => break,
            Ok(Slash::Help(None)) => {
                println!();
                menu(ui, MENU);
                menu(ui, MORE);
            }
            Ok(Slash::Help(Some(topic))) => print!("{}", args::help_for(&topic)),
            Ok(Slash::Version) => println!("wired {VERSION}"),
            Ok(Slash::Run(command)) => {
                println!();
                if let Err(message) =
                    cmd::execute(ui, &command, target, supervisor, config, false).await
                {
                    ui.error(&message);
                }
                println!();
            }
            Err(message) => ui.warn(&message),
        }
    }
    Ok(cmd::EXIT_OK)
}

fn banner(ui: &Ui, target: &Target) {
    println!();
    println!("  {}  {}", ui.bold("Wired"), ui.dim(VERSION));
    ui.note(&target.describe());
    println!();
}

fn menu(ui: &Ui, rows: &[(&str, &str)]) {
    for (command, hint) in rows {
        println!("  {:<16} {}", command, ui.dim(hint));
    }
    println!();
}

/// Turn one typed line into a command. The slash is required so a sentence
/// that happens to start with a command name is not swallowed.
pub fn parse_slash(line: &str) -> std::result::Result<Slash, String> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(Slash::Empty);
    }
    let rest = match line.strip_prefix('/') {
        Some(rest) => rest.trim_start(),
        None => {
            return Err("slash commands start with / — try /help".into());
        }
    };
    if rest.is_empty() {
        return Ok(Slash::Help(None));
    }

    let mut words = split_words(rest);
    let Some(head) = words.first_mut() else {
        return Ok(Slash::Help(None));
    };
    head.make_ascii_lowercase();
    // `?` is the one spelling the argv parser has never had to know about.
    if head == "?" {
        "help".clone_into(head);
    }
    match head.as_str() {
        "quit" | "exit" | "q" => return Ok(Slash::Quit),
        "version" | "v" => return Ok(Slash::Version),
        "tui" | "repl" | "shell" => return Err("already in the TUI".into()),
        "serve" => {
            return Err("serve takes over the process — run `wired serve` from the shell".into());
        }
        _ => {}
    }

    let cli = args::parse(words)?;
    if cli.global.is_process_level() {
        return Err(
            "those flags belong on the wired command itself: `wired --remote pilot`".into(),
        );
    }
    match cli.command {
        // `/help ask` and `/status --help` are the same question.
        Command::Help(topic) => Ok(Slash::Help(topic)),
        Command::Version => Ok(Slash::Version),
        // The words are caught above; this is their flag spellings falling through.
        Command::Tui { .. } | Command::Serve => Ok(Slash::Help(None)),
        command => Ok(Slash::Run(command)),
    }
}

/// Quote-aware split so `/ask "hello there"` is one sentence, not two words
/// with quote marks still attached.
fn split_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for c in s.chars() {
        match c {
            '"' => in_quote = !in_quote,
            c if c.is_whitespace() && !in_quote => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slash(line: &str) -> Slash {
        parse_slash(line).unwrap_or_else(|e| panic!("{line}: {e}"))
    }

    #[test]
    fn blank_and_slash_alone() {
        assert!(matches!(slash(""), Slash::Empty));
        assert!(matches!(slash("   "), Slash::Empty));
        assert!(matches!(slash("/"), Slash::Help(None)));
        assert!(matches!(slash("/help"), Slash::Help(None)));
        match slash("/help ask") {
            Slash::Help(Some(topic)) => assert_eq!(topic, "ask"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn quit_and_version() {
        assert!(matches!(slash("/quit"), Slash::Quit));
        assert!(matches!(slash("/exit"), Slash::Quit));
        assert!(matches!(slash("/q"), Slash::Quit));
        assert!(matches!(slash("/version"), Slash::Version));
    }

    #[test]
    fn status_and_ask_reuse_the_argv_parser() {
        assert!(matches!(slash("/status"), Slash::Run(Command::Status)));
        assert!(matches!(slash("/ST"), Slash::Run(Command::Status)));
        match slash("/ask summarise git status") {
            Slash::Run(Command::Ask { text, wait }) => {
                assert_eq!(text, "summarise git status");
                assert_eq!(wait, 90.0);
            }
            other => panic!("{other:?}"),
        }
        match slash("/ask \"hello there\"") {
            Slash::Run(Command::Ask { text, .. }) => assert_eq!(text, "hello there"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn flags_on_known_commands_still_parse() {
        assert!(matches!(
            slash("/approve --deny"),
            Slash::Run(Command::Approve { allow: false })
        ));
        assert!(matches!(
            slash("/logs -f"),
            Slash::Run(Command::Logs { follow: true, .. })
        ));
        match slash("/folder /srv/wired") {
            Slash::Run(Command::Folder(Some(path))) => assert_eq!(path, "/srv/wired"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_sentence_without_a_slash_is_refused() {
        let err = parse_slash("status").unwrap_err();
        assert!(err.contains("start with /"), "{err}");
    }

    #[test]
    fn serve_and_nested_tui_are_refused() {
        assert!(parse_slash("/serve").unwrap_err().contains("serve"));
        assert!(parse_slash("/tui").unwrap_err().contains("already"));
    }

    #[test]
    fn help_reaches_the_topic_either_way() {
        // `--help` on a command is the same question as `/help <command>`,
        // so the topic has to survive the argv parser.
        for line in ["/help status", "/status --help"] {
            match slash(line) {
                Slash::Help(Some(topic)) => assert_eq!(topic, "status", "{line}"),
                other => panic!("{line}: {other:?}"),
            }
        }
        assert!(matches!(slash("/?"), Slash::Help(None)));
        match slash("/? ask") {
            Slash::Help(Some(topic)) => assert_eq!(topic, "ask"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn target_flags_stay_on_the_outer_command() {
        let err = parse_slash("/status --remote pilot").unwrap_err();
        assert!(err.contains("wired --remote"), "{err}");
        // Every global counts, not just the ones that pick a host.
        assert!(parse_slash("/status --no-color").is_err());
        assert!(parse_slash("/status --json").is_err());
    }

    #[test]
    fn unknown_slash_names_itself() {
        let err = parse_slash("/frobnicate").unwrap_err();
        assert!(err.contains("frobnicate"), "{err}");
    }
}
