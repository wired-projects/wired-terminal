//! Simple, reliable plain-text cleanup for PTY output.
//!
//! Does NOT try to reconstruct a perfect chat UI. Just:
//!   1. Strip ANSI / control sequences
//!   2. Honour CR overwrites (status lines)
//!   3. Drop spinner glyphs
//!   4. Collapse noisy whitespace
//!
//! Readable enough for curl; won't destroy real sentences. Only the raw debug
//! path uses this — the readable endpoints render the VT screen instead.

use std::sync::LazyLock;

use regex::Regex;

// ANSI / OSC / CSI / C0 (keep \t \n \r)
static ANSI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)",
        r"|\x1b\[[0-?]*[ -/]*[@-~]",
        r"|\x1b[PX^_][^\x1b]*\x1b\\",
        r"|\x1b[()][0-9A-Za-z]",
        r"|\x1b[@-Z\\-_]",
        r"|\x{9b}[0-?]*[ -/]*[@-~]",
        r"|[\x00-\x08\x0b\x0c\x0e-\x1a\x1c-\x1f]",
    ))
    .unwrap()
});

static SPINNER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏◐◓◑◒⣾⣽⣻⢿⡿⣟⣯⣷]+").unwrap());
static BRAILLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\x{2800}-\x{28FF}]+").unwrap());

// Lines that are ONLY chrome
static ONLY_NOISE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)^(",
        r"waiting for response\.?",
        r"|wating for response\.?",
        r"|thought for [\d.]+s?",
        r"|worked for [\d.]+s?",
        r"|responding\.?",
        r"|thinking\.?",
        r"|starting session\.?",
        r"|\[stop\]|\[stable\]",
        r"|\d{1,2}:\d{2}\s*(?:[ap]m)?",
        r"|[\d.]+\s*[km]?\s*/\s*[\d.]+\s*[km]?",
        r"|⇣[\d.]+k?",
        r")$",
    ))
    .unwrap()
});

static THOUGHT_FOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(Thought for\s*[\d.]+s?)").unwrap());
static WORKED_FOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(Worked for\s*[\d.]+s?)").unwrap());
static PROMPT_GLYPH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([❯>])\s*").unwrap());
static RUN_OF_SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]{2,}").unwrap());
static TRAILING_CLOCK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+\d{1,2}:\d{2}\s*(?:[AaPp][Mm])?\s*$").unwrap());
static BOX_ONLY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\s─│┌┐└┘├┤┬┴┼╭╮╯╰━┃┏┓┗┛═║╔╗╚╝▀▄█░▒▓]+$").unwrap());

pub fn strip_ansi(text: &str) -> String {
    ANSI.replace_all(text, "").into_owned()
}

/// Last CR segment wins per line (status-bar rewrites).
pub fn apply_cr(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let mut out: Vec<String> = Vec::new();

    for row in text.split('\n') {
        if !row.contains('\r') {
            out.push(row.trim_end().to_string());
            continue;
        }
        let mut buf = String::new();
        for part in row.split('\r') {
            if part.is_empty() {
                continue;
            }
            // A shorter repaint leaves the tail of the previous one visible.
            buf = if part.chars().count() >= buf.chars().count() {
                part.to_string()
            } else {
                let keep: String = buf.chars().skip(part.chars().count()).collect();
                format!("{part}{keep}")
            };
        }
        out.push(buf.trim_end().to_string());
    }
    out.join("\n")
}

pub fn clean_terminal_text(raw: &str, keep_chrome: bool) -> String {
    if raw.is_empty() {
        return String::new();
    }

    let t = strip_ansi(raw);
    let t = apply_cr(&t);
    let t = SPINNER.replace_all(&t, "").into_owned();
    let t = BRAILLE.replace_all(&t, "").into_owned();

    // Soft-break very long TUI rows on common markers (keeps original spaces)
    let t = if keep_chrome {
        t
    } else {
        let t = THOUGHT_FOR.replace_all(&t, "\n$1\n").into_owned();
        let t = WORKED_FOR.replace_all(&t, "\n$1\n").into_owned();
        PROMPT_GLYPH.replace_all(&t, "\n$1 ").into_owned()
    };

    let mut lines: Vec<String> = Vec::new();
    let mut prev: Option<String> = None;

    for line in t.split('\n') {
        // collapse horizontal runs of spaces but keep single spaces
        let line = RUN_OF_SPACES.replace_all(line, " ").trim().to_string();
        // trailing timestamps like "2:28 PM"
        let line = TRAILING_CLOCK.replace(&line, "").trim().to_string();

        if line.is_empty() || line == "◆" || line == "❯" || line == ">" {
            continue;
        }
        if !keep_chrome && ONLY_NOISE.is_match(&line) {
            continue;
        }
        if !keep_chrome && BOX_ONLY.is_match(&line) {
            continue;
        }
        if prev.as_deref() == Some(line.as_str()) {
            continue;
        }
        prev = Some(line.clone());
        lines.push(line);
    }

    lines.join("\n").trim().to_string()
}

pub fn clean_terminal_bytes(data: &[u8], keep_chrome: bool) -> String {
    clean_terminal_text(&String::from_utf8_lossy(data), keep_chrome)
}
