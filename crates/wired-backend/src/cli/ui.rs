//! Terminal output.
//!
//! Colour when a person is reading and nothing at all when the output is being
//! piped somewhere, because `wired status | grep` should not have to strip
//! escape codes. `--json` bypasses all of this and prints what the API said.

use std::io::IsTerminal;

/// Wide enough for `wired-terminal`, which is the longest label we print.
const LABEL_WIDTH: usize = 15;
/// Wide enough for `disconnected`, so the notes beside each value line up.
const VALUE_WIDTH: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Good,
    Bad,
    /// Genuinely unknown — not the same as broken, and not worth a red dot.
    Unknown,
    None,
}

impl Mark {
    /// `None` for a check that could not be run is the shape the API uses too.
    pub fn from_ok(ok: Option<bool>) -> Self {
        match ok {
            Some(true) => Mark::Good,
            Some(false) => Mark::Bad,
            None => Mark::Unknown,
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Mark::Good => "●",
            Mark::Bad => "○",
            Mark::Unknown => "·",
            Mark::None => " ",
        }
    }

    fn color(self) -> &'static str {
        match self {
            Mark::Good => GREEN,
            Mark::Bad => RED,
            Mark::Unknown => YELLOW,
            Mark::None => "",
        }
    }
}

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BLUE: &str = "\x1b[34m";
const MAGENTA: &str = "\x1b[35m";

pub struct Ui {
    color: bool,
}

impl Ui {
    pub fn new(no_color_flag: bool) -> Self {
        // NO_COLOR is the convention; the flag and a redirected stdout are the
        // other two ways to end up plain.
        let disabled = no_color_flag
            || std::env::var_os("NO_COLOR").is_some()
            || !std::io::stdout().is_terminal();
        Ui { color: !disabled }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color && !code.is_empty() {
            format!("{code}{text}{RESET}")
        } else {
            text.to_string()
        }
    }

    pub fn bold(&self, text: &str) -> String {
        self.paint(BOLD, text)
    }

    pub fn dim(&self, text: &str) -> String {
        self.paint(DIM, text)
    }

    pub fn green(&self, text: &str) -> String {
        self.paint(GREEN, text)
    }

    pub fn red(&self, text: &str) -> String {
        self.paint(RED, text)
    }

    pub fn yellow(&self, text: &str) -> String {
        self.paint(YELLOW, text)
    }

    pub fn blue(&self, text: &str) -> String {
        self.paint(BLUE, text)
    }

    pub fn magenta(&self, text: &str) -> String {
        self.paint(MAGENTA, text)
    }

    /// A section title, with the blank line that separates it from the last one.
    pub fn heading(&self, text: &str) {
        println!("\n{}", self.bold(text));
    }

    /// `  label          ● value        detail`
    pub fn row(&self, label: &str, mark: Mark, value: &str, detail: &str) {
        let dot = self.paint(mark.color(), mark.glyph());
        // The value column is only padded when something follows it, so a lone
        // long value — a URL, a path — is not shoved off to the right.
        let value = if detail.is_empty() {
            value.to_string()
        } else {
            format!("{value:<VALUE_WIDTH$}")
        };
        let mut line = format!("  {label:<LABEL_WIDTH$} {dot} {value}");
        if !detail.is_empty() {
            line.push_str(&format!("  {}", self.dim(detail)));
        }
        println!("{}", line.trim_end());
    }

    /// A row with no status dot, for things that are facts rather than states.
    pub fn field(&self, label: &str, value: &str) {
        self.row(label, Mark::None, value, "");
    }

    pub fn note(&self, text: &str) {
        println!("  {}", self.dim(text));
    }

    pub fn warn(&self, text: &str) {
        eprintln!("{} {}", self.yellow("!"), text);
    }

    pub fn error(&self, text: &str) {
        eprintln!("{} {}", self.red("wired:"), text);
    }

    pub fn json(&self, value: &serde_json::Value) {
        println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        );
    }
}

/// `6d 3h`, `12m`, `4s` — the coarsest two units that still say something.
pub fn human_duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (d, h, m, s) = (
        seconds / 86_400,
        (seconds % 86_400) / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60,
    );
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, _) => format!("{m}m"),
        (0, _, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_use_two_units_at_most() {
        assert_eq!(human_duration(9), "9s");
        assert_eq!(human_duration(90), "1m");
        assert_eq!(human_duration(3_700), "1h 1m");
        assert_eq!(human_duration(529_200), "6d 3h");
    }

    #[test]
    fn a_negative_clock_skew_is_not_a_negative_uptime() {
        assert_eq!(human_duration(-5), "0s");
    }

    #[test]
    fn plain_ui_emits_no_escape_codes() {
        let ui = Ui { color: false };
        assert_eq!(ui.bold("hi"), "hi");
        assert_eq!(ui.dim("hi"), "hi");
    }

    #[test]
    fn marks_follow_the_apis_tri_state() {
        assert_eq!(Mark::from_ok(Some(true)), Mark::Good);
        assert_eq!(Mark::from_ok(Some(false)), Mark::Bad);
        assert_eq!(Mark::from_ok(None), Mark::Unknown);
    }
}
