//! Virtual terminal screen for readable API output.
//!
//! The agent CLIs are full-screen TUIs: they paint with CSI cursor moves.
//! Stripping ANSI concatenates garbage. `vt100` replays the byte stream into a
//! screen buffer we can dump as plain lines.

use std::sync::LazyLock;

use regex::Regex;

static SPINNER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏◐◓◑◒⣾⣽⣻⢿⡿⣟⣯⣷]+").unwrap());
static BOX_ONLY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\s╭╮╯╰┌┐└┘├┤┬┴┼─│━┃═║╔╗╚╝╠╣╦╩╬▀▄█░▒▓]+$").unwrap());
static LEADING_PAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ {2,}").unwrap());

pub struct VtScreen {
    parser: vt100::Parser,
    pub cols: u16,
    pub rows: u16,
}

impl VtScreen {
    pub fn new(cols: u16, rows: u16) -> Self {
        let cols = cols.max(20);
        let rows = rows.max(8);
        Self {
            // No scrollback: the transcript layer reconstructs history from
            // successive dumps, and a scrollback buffer would double-count it.
            parser: vt100::Parser::new(rows, cols, 0),
            cols,
            rows,
        }
    }

    pub fn reset(&mut self, cols: u16, rows: u16) {
        *self = Self::new(cols, rows);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(20);
        let rows = rows.max(8);
        self.cols = cols;
        self.rows = rows;
        self.parser.screen_mut().set_size(rows, cols);
    }

    /// `vt100` owns the incremental UTF-8 state, so a multi-byte character
    /// split across two `read()`s is reassembled rather than becoming U+FFFD.
    pub fn feed(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.parser.process(data);
    }

    pub fn display_lines(&self, strip_empty_edges: bool) -> Vec<String> {
        let screen = self.parser.screen();
        let mut lines: Vec<String> = screen
            .rows(0, self.cols)
            .map(|row| {
                let line = row.trim_end().replace('\t', " ");
                SPINNER.replace_all(&line, "").into_owned()
            })
            .collect();

        if strip_empty_edges {
            while lines.first().is_some_and(|l| l.trim().is_empty()) {
                lines.remove(0);
            }
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
        }
        lines
    }

    pub fn dump(&self, compact: bool) -> String {
        let mut out: Vec<String> = Vec::new();
        let mut blank = false;

        for line in self.display_lines(true) {
            let s = line.trim_end().to_string();
            // drop pure box-frame lines (optional readability)
            if !s.is_empty() && BOX_ONLY.is_match(&s) {
                continue;
            }
            // trim heavy left padding but keep indent structure lightly
            let s = LEADING_PAD.replace(&s, "  ").into_owned();
            if s.trim().is_empty() {
                if compact && !blank {
                    out.push(String::new());
                }
                blank = true;
                continue;
            }
            out.push(s);
            blank = false;
        }
        out.join("\n").trim_end().to_string()
    }
}
