//! Screen dump → conversation transcript, for the live tail endpoint.
//!
//! The agent CLIs are full-screen TUIs: they repaint a fixed viewport
//! on every frame. Diffing whole screen dumps therefore streams banner art, box
//! borders, spinner frames and re-prints of the same sentence — which is what
//! made `curl -N /api/agent/output/stream` unreadable.
//!
//! Two pieces:
//!   * `screen_to_transcript` keeps only conversation rows (chrome dropped)
//!   * `TranscriptTail` remembers what was already sent, so a repaint that
//!     scrolls the viewport does not re-emit lines
//!
//! The patterns below are empirical — they encode what these two CLIs actually
//! paint, not what a terminal is allowed to paint. Change them against real
//! output, not intuition.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Text,
    User,
    Prompt,
    Notice,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::User => "user",
            Kind::Prompt => "prompt",
            Kind::Notice => "notice",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Row {
    pub kind: Kind,
    pub text: String,
}

impl Row {
    fn new(kind: Kind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
        }
    }
}

/// Rules and corners — these genuinely delimit a box.
const FRAME_CHARS: &str = "─━│┃┄┅┆┇┈┉┊┋┌┐└┘├┤┬┴┼╭╮╯╰═║╔╗╚╝╠╣╦╩╬";
/// Blocks and shading. NOT framing: Grok paints a █ scrollbar down the right
/// margin, so treating these as a border made every content row look boxed.
const DECOR_CHARS: &str = "▀▄█▌▐░▒▓▗▖▘▝▞▚▛▜▟▙";

fn is_frame(c: char) -> bool {
    FRAME_CHARS.contains(c)
}

fn is_decor(c: char) -> bool {
    DECOR_CHARS.contains(c)
}

static BOX_ONLY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^[\s{}{}]*$",
        regex::escape(FRAME_CHARS),
        regex::escape(DECOR_CHARS)
    ))
    .unwrap()
});

static BRAILLE_ONLY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\s\x{2800}-\x{28FF}]*$").unwrap());

// Right-hand gutters the TUIs paint onto otherwise real content rows.
static TAIL_TIMESTAMP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s{2,}\d{1,2}:\d{2}\s*(?:[AaPp][Mm])?\s*$").unwrap());
static TAIL_TOKENS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s{2,}[\d.]+\s*[KkMm]?\s*/\s*[\d.]+\s*[KkMm]?\s*$").unwrap());

static USER_TURN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[❯›>]\s+(.*\S)\s*$").unwrap());
// \s* not \s+: a frame caught mid-render can paint the bullet before its text,
// and a lone "⏺" is not a transcript line.
static BULLET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[⏺●○◉❙✱✽∴⎿·•]+\s*").unwrap());
// Standing warnings the CLIs pin under the composer, e.g. "⚠ Transcript
// saving is off". Worth surfacing once, but they are not transcript body.
static NOTICE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[⚠⚡✗✘⛔]\s*\S").unwrap());
// The composer's rotating grey hint, e.g. ❯ Try "how do I log an error?"
static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)^try\s+["“']"#).unwrap());

// Spinner / "thinking" rows. Claude randomises the verb ("Baked for 1s",
// "Noodling…"), so match the shape rather than a word list.
static STATUS_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        ^(?:[^\w\s]{1,3}\s*)?            # optional leading glyph
        # Claude randomises the verb, so match the shape. Not [a-z]+: the verbs
        # are accented and hyphenated ('Sautéed for 1s', 'Fiddle-faddling…').
        (?:
            [^\W\d_][\w'-]* \s+ for \s+ [\d.]+ \s* [sm]\b     # 'Brewed for 9s'
          | [^\W\d_][\w'-]* …? \s* \( \s* \d+ \s* [sm]\b      # 'Puttering… (12s · ↑ 1.2k…'
          | (?:
                [^\W\d_][\w'-]* \s* (?:…|\.\.\.)              # 'Swirling…' bare verb
              | (?:thinking|responding|working|waiting\x20for\x20response) \b [.…]*
            )
            # …and the live counters the CLI pins after it. Without this the
            # row is only chrome while the verb sits alone on the line, so
            # 'Responding… 5.0s  32s ↓94.0k [stop]' streamed to the phone once
            # every repaint — and, arriving every few seconds, chopped the
            # answer around it into one-line messages.
            (?:
                \s | [·|,()] | [\d.]+ \s* [smh]\b
              | [↑↓⇣⇡] \s* [\d.]+ \s* [km]? \b
              | \[ [a-z]+ \] | tokens? | esc\x20to\x20interrupt
            )*
            \s* $
        )",
    )
    .unwrap()
});

// The live counter gutter pinned to the working row: a phase timer, a session
// timer, a token count, and the interrupt hint.
//
// Matched on its own, without caring what leads it. The verb-led patterns above
// cannot cover `Run Web search: 1.0s  6.3s ⇣14.0k [stop]` or `Fetch https://…
// 0.1s  16s ⇣86.0k [stop]`, because a tool name and its argument are not a
// verb — and those rows were reaching the phone once per repaint. Two adjacent
// timers *and* a counter or a [stop] is a meter; prose does not end that way.
static COUNTER_TAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        [\d.]+ \s* [sm]\b \s+ [\d.]+ \s* [smh]\b         # '0.1s  16s'
        (?:
            \s* [↑↓⇣⇡] \s* [\d.]+ \s* [km]? \b          # '⇣86.0k'
          | \s* \[ (?:stop|stable) \]
        )+
        \s* $",
    )
    .unwrap()
});

static CHROME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        ^(?:
            ⎇\s                                        # grok git/status bar
          | [^\w\s]\s* [\w.\-/]+ \s+ ~/ \S* \s*$        # '<branch glyph> main ~/path'
          | [\d.]+\s*[km]?\s*/\s*[\d.]+\s*[km]?\s*$     # bare token budget
          | ⏵+\s                                        # claude permission-mode line
          | [❯›>]\s*$                                   # empty composer
          | [^\w\s]{1,2}\s*$                            # lone glyph row, e.g. ▼ scroll hint
          | (?:shift\+tab|ctrl\+[a-z]+|alt\+[a-z]+)\s*[·:]
          | (?:[^\w\s]{1,3}\s*)? tip:\s                 # claude's rotating hint line
          | starting\x20session\b
          | \[(?:stop|stable)\]$
        )
        | (?:\?\s*for\x20shortcuts | esc\x20to\x20interrupt | to\x20cycle\) )
        | (?:ctrl|alt|cmd)\+\S+\s*$                     # trailing keyboard hint
        | (?:·\s*|\s{2,}) / [a-z][a-z0-9-]{0,15} \s*$   # status gutter: '… · /effort'
        ",
    )
    .unwrap()
});

// Framed rows are chrome *unless* they are asking the operator something —
// approval dialogs live in boxes too, and swallowing them would leave an
// HTTP-driven session silently stuck.
static PROMPT_HINT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\?\s*$|\(\s*y\s*/\s*n\s*\)|^\s*[❯›>]?\s*\d+[.)]\s+\S)").unwrap()
});

// Not every modal is drawn in a box. Claude Code's slash-command pickers —
// `/model` is the one everybody meets — paint a bare numbered list with a
// keyboard footer, and treating those rows as ordinary prose is what let a
// stray Enter change a persistent setting from a phone. Two independent
// tells, either of which is enough:
//
//   * a footer that names the keys, "Esc to cancel" / "Enter to confirm"
//   * a highlighted numbered row, "❯ 1. Default (recommended)"
//
// Both are things these CLIs paint and prose does not, which matters because
// the agent writes numbered lists of its own all the time.
static MODAL_FOOTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
          esc \s+ to \s+ cancel
        | enter \s+ to \s+ (?: confirm | set | select | choose )
        | to \s+ use \s+ this \s+ session \s+ only
        | ←/→ \s+ to \s+ adjust
        ",
    )
    .unwrap()
});

static MENU_ROW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[❯›>▸]?\s*\d+[.)]\s+\S").unwrap());

static MENU_CURSOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[❯›>▸]\s*\d+[.)]\s+\S").unwrap());

static MULTI_SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r" {2,}").unwrap());

/// Is a modal picker open on this screen?
///
/// Whole-dump question, deliberately: one numbered line proves nothing, but a
/// numbered line on a screen that also carries a keyboard footer is a menu.
fn has_modal(dump: &str) -> bool {
    let mut numbered = false;
    let mut tell = false;
    for raw in dump.split('\n') {
        let (content, _) = unframe(&normalize(raw));
        if content.is_empty() {
            continue;
        }
        if MENU_ROW.is_match(&content) {
            numbered = true;
        }
        if MENU_CURSOR.is_match(&content) || MODAL_FOOTER.is_match(&content) {
            tell = true;
        }
    }
    numbered && tell
}

/// Drop the painted right-hand gutter and squeeze alignment padding.
fn normalize(raw: &str) -> String {
    let s = raw.replace('\t', " ");
    let s = s.trim_end();
    // Scrollbar column first — it sits outboard of the timestamp gutter and
    // would otherwise anchor the tail patterns to the wrong end of the line.
    let s = s.trim_end_matches(|c: char| is_decor(c) || c == ' ');
    let s = TAIL_TIMESTAMP.replace(s, "");
    let s = TAIL_TOKENS.replace(&s, "");
    // Keep a hint of indentation, lose the column padding.
    MULTI_SPACE.replace_all(&s, "  ").trim_end().to_string()
}

/// Strip box borders. Returns (content, was_framed).
///
/// Catches side borders (`│ … │`) and titled edges alike — Claude's
/// `╭─── Claude Code v2.1.226 ───╮` and Grok's model footer are both box rows
/// with text baked into the rule.
fn unframe(line: &str) -> (String, bool) {
    let s = line.trim().trim_matches(|c: char| is_decor(c)).trim();
    let mut framed = false;
    let mut s = s.to_string();

    if s.chars().next().is_some_and(is_frame) {
        framed = true;
        s = s.trim_start_matches(is_frame).trim_start().to_string();
    }
    if s.chars().next_back().is_some_and(is_frame) {
        framed = true;
        s = s.trim_end_matches(is_frame).trim_end().to_string();
    }
    (s.trim().to_string(), framed)
}

fn is_chrome(line: &str) -> bool {
    CHROME.is_match(line) || STATUS_LINE.is_match(line) || COUNTER_TAIL.is_match(line)
}

/// Turn one virtual-screen dump into conversation rows.
pub fn screen_to_transcript(dump: &str, keep_chrome: bool) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let modal = !keep_chrome && has_modal(dump);

    for raw in dump.split('\n') {
        let line = normalize(raw);
        if keep_chrome {
            if !line.trim().is_empty() {
                rows.push(Row::new(Kind::Text, line.trim_end()));
            }
            continue;
        }

        let (content, framed) = unframe(&line);
        if content.is_empty() || BOX_ONLY.is_match(&content) || BRAILLE_ONLY.is_match(&content) {
            continue;
        }
        if framed {
            // Banners and the composer live in boxes; so do approval prompts.
            if PROMPT_HINT.is_match(&content) {
                rows.push(Row::new(Kind::Prompt, content));
            }
            continue;
        }
        // Checked ahead of USER_TURN: the highlight on the selected row is the
        // same `❯` glyph that marks what you typed, so "❯ 1. Default" was being
        // relayed as the sender's own message — which is to say, dropped. That
        // is why option 1 went missing from the phone.
        if modal && MENU_ROW.is_match(&content) {
            rows.push(Row::new(Kind::Prompt, content));
            continue;
        }
        if modal && MODAL_FOOTER.is_match(&content) {
            rows.push(Row::new(Kind::Prompt, content));
            continue;
        }
        if let Some(turn) = USER_TURN.captures(&content) {
            let said = turn.get(1).map(|m| m.as_str()).unwrap_or("");
            if !PLACEHOLDER.is_match(said) {
                rows.push(Row::new(Kind::User, said));
            }
            continue;
        }
        if NOTICE.is_match(&content) {
            rows.push(Row::new(Kind::Notice, content));
            continue;
        }

        // Strip the speaker bullet *before* testing for chrome: Claude renders
        // its rotating hint as "· Tip: …", and an anchored ^tip: never sees it.
        let content = BULLET.replace(&content, "").to_string();
        if content.is_empty() || is_chrome(&content) {
            continue;
        }
        rows.push(Row::new(Kind::Text, content));
    }

    // The bottom-most `❯` row is the composer, not a turn: Claude Code paints
    // rotating grey suggestions there (including generated follow-ups that read
    // exactly like something you typed), and text entered but not yet submitted
    // lives there too. Neither has been sent. Pinned notices render *below* the
    // composer, so look past them. Once the agent replies the row is no longer
    // trailing and streams normally.
    for i in (0..rows.len()).rev() {
        if rows[i].kind == Kind::Notice {
            continue;
        }
        if rows[i].kind == Kind::User {
            rows.remove(i);
        }
        break;
    }
    rows
}

/// Rows of `rows` not yet in `sent`, assuming the viewport scrolls up.
pub fn align(sent: &[Row], rows: &[Row]) -> Vec<Row> {
    if rows.is_empty() {
        return Vec::new();
    }
    if sent.is_empty() {
        return rows.to_vec();
    }
    // Longest overlap between the tail of what we sent and the head of the screen.
    for k in (1..=sent.len().min(rows.len())).rev() {
        if sent[sent.len() - k..] == rows[..k] {
            return rows[k..].to_vec();
        }
    }
    let seen: HashSet<&Row> = sent.iter().collect();
    rows.iter().filter(|r| !seen.contains(r)).cloned().collect()
}

/// Streams a repainting viewport as an append-only transcript.
pub struct TranscriptTail {
    window: usize,
    sent: Vec<Row>,
    notices: HashSet<String>,
}

impl Default for TranscriptTail {
    fn default() -> Self {
        Self::new(400)
    }
}

impl TranscriptTail {
    pub fn new(window: usize) -> Self {
        Self {
            window,
            sent: Vec::new(),
            notices: HashSet::new(),
        }
    }

    pub fn reset(&mut self) {
        self.sent.clear();
        self.notices.clear();
    }

    /// Pinned notices get truncated to fit the viewport, so the same warning
    /// arrives as several different strings ('… restart with CLAUDE_COD…' vs
    /// the full line). Key on the stable head.
    fn notice_key(text: &str) -> String {
        text.trim_end_matches(['…', ' ', '.'])
            .trim()
            .chars()
            .take(48)
            .collect()
    }

    pub fn update(&mut self, dump: &str, hold_last: bool, keep_chrome: bool) -> Vec<Row> {
        let mut rows = screen_to_transcript(dump, keep_chrome);
        if hold_last && !rows.is_empty() {
            // The bottom row may still be mid-render; let it settle a frame.
            rows.pop();
        }
        // A pinned notice drifts up and down the screen as the transcript grows;
        // position-based alignment would read each move as a new line.
        rows.retain(|r| {
            r.kind != Kind::Notice || !self.notices.contains(&Self::notice_key(&r.text))
        });

        let tail_start = self.sent.len().saturating_sub(self.window);
        let new = align(&self.sent[tail_start..], &rows);

        if !new.is_empty() {
            for row in &new {
                if row.kind == Kind::Notice {
                    self.notices.insert(Self::notice_key(&row.text));
                }
            }
            self.sent.extend(new.iter().cloned());
            let overflow = self.sent.len() as isize - (self.window * 2) as isize;
            if overflow > 0 {
                self.sent.drain(..overflow as usize);
            }
        }
        new
    }
}

#[cfg(test)]
mod tests {
    use super::{screen_to_transcript, Kind};

    /// `/model` on Claude Code 2.1.226, captured off a real session. Unframed,
    /// and the selected row carries the same `❯` that marks a user turn.
    const MODEL_PICKER: &str = "\
❯ /model

  Select model
  Switch between Claude models. Your pick becomes the default for new sessions. For other/previous
  model names, specify with --model.

  ❯ 1. Default (recommended) ✔  Sonnet 5 · Efficient for routine tasks
  2. Sonnet                   Sonnet 5 · Efficient for routine tasks
  3. Fable                    Fable 5 · Most capable for your hardest and longest-running tasks
  · Requires usage credits
  4. Opus                     Opus 5 · Best for everyday, complex tasks · ~2× usage vs Sonnet
  5. Haiku                    Haiku 4.5 · Fastest for quick answers

  ● High effort (default) ←/→ to adjust

  Enter to set as default · s to use this session only · Esc to cancel";

    fn prompts(dump: &str) -> Vec<String> {
        screen_to_transcript(dump, false)
            .into_iter()
            .filter(|row| row.kind == Kind::Prompt)
            .map(|row| row.text)
            .collect()
    }

    #[test]
    fn an_unframed_picker_is_a_prompt() {
        let prompts = prompts(MODEL_PICKER);
        assert!(
            prompts.iter().any(|p| p.contains("2. Sonnet")),
            "menu rows should be prompts, got {prompts:?}"
        );
    }

    /// The regression the phone actually showed: the list started at 2 because
    /// `❯ 1. …` was being read as the sender's own message.
    #[test]
    fn the_highlighted_row_is_not_mistaken_for_a_user_turn() {
        let rows = screen_to_transcript(MODEL_PICKER, false);
        assert!(
            rows.iter()
                .any(|r| r.kind == Kind::Prompt && r.text.contains("1. Default")),
            "option 1 went missing: {rows:?}"
        );
        assert!(
            !rows
                .iter()
                .any(|r| r.kind == Kind::User && r.text.contains("Default")),
            "the highlight was read as a user turn: {rows:?}"
        );
    }

    #[test]
    fn the_keyboard_footer_survives() {
        assert!(prompts(MODEL_PICKER)
            .iter()
            .any(|p| p.contains("Esc to cancel")));
    }

    /// The agent writes numbered lists constantly; none of them are menus.
    #[test]
    fn a_numbered_list_in_prose_is_not_a_menu() {
        let dump = "\
● Here is the plan:

  1. Delete the stale branches
  2. Rerun the failing test
  3. Push

❯";
        assert!(
            prompts(dump).is_empty(),
            "prose was treated as a modal: {:?}",
            prompts(dump)
        );
    }

    fn text(dump: &str) -> Vec<String> {
        screen_to_transcript(dump, false)
            .into_iter()
            .filter(|row| row.kind == Kind::Text)
            .map(|row| row.text)
            .collect()
    }

    /// Verbatim off the phone: every one of these was relayed to Telegram as a
    /// message of its own, several times a minute, for the length of an answer.
    #[test]
    fn a_progress_row_with_live_counters_is_chrome() {
        for row in [
            "Thinking… 5.0s  26s ↓94.0k [stop]",
            "Responding… 2.0s  29s ↓94.0k [stop]",
            "Responding… 23s  50s ↓94.0k [stop]",
            "✳ Puttering… (12s · ↑ 1.2k tokens · esc to interrupt)",
            "Responding…",
            "Brewed for 9s",
        ] {
            assert!(
                text(row).is_empty(),
                "{row:?} reached the transcript as {:?}",
                text(row)
            );
        }
    }

    /// Every distinct progress row recovered from the pilot's own recorded
    /// transcripts — 34 of the 82 rows it relayed in one session were these.
    /// The last two are Grok's tool rows, where a tool name and a URL lead
    /// instead of a verb, so only the counter gutter identifies them.
    #[test]
    fn every_progress_shape_the_pilot_recorded_is_chrome() {
        for row in [
            "Waiting for response… 0.2s  0.2s ⇣13.9k [stop]",
            "Responding… 0.7s  27s ⇣14.0k [stop]",
            "Thinking… 2.0s  22s ⇣94.0k [stop]",
            "Run Web search: 1.0s  6.3s ⇣14.0k [stop]",
            "Fetch https://www.machines.com.my/collections/macbook-air 0.1s  16s ⇣86.0k [stop]",
        ] {
            assert!(
                text(row).is_empty(),
                "{row:?} reached the transcript as {:?}",
                text(row)
            );
        }
    }

    /// The verbs are ordinary words and the agent writes them; only the shape
    /// of the counter row makes it chrome.
    #[test]
    fn prose_that_trails_off_is_not_a_status_row() {
        for row in [
            "Thinking… I would start with the failing test",
            "Working… on it, 3 files left",
            "Waiting… 5 minutes should be enough",
        ] {
            assert_eq!(text(row), vec![row.to_string()]);
        }
    }

    /// A boxed approval dialog kept working the way it always did.
    #[test]
    fn a_framed_approval_is_still_a_prompt() {
        let dump = "\
╭──────────────────────────────────────╮
│ Do you want to proceed?              │
│ ❯ 1. Yes                             │
│   2. No, tell Claude what to do      │
╰──────────────────────────────────────╯";
        assert!(prompts(dump).iter().any(|p| p.contains("1. Yes")));
    }
}
