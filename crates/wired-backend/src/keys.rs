//! Keyboard payload encoding for the PTY.
//!
//! Turning an HTTP body into the bytes a terminal expects: named keys, escape
//! sequences, and the one non-obvious rule about how agent CLIs read a submit.

use std::time::Duration;

/// Ink-based CLIs (Claude Code) debounce their stdin: text and a trailing CR
/// arriving in one write read as a paste, so the message lands in the composer
/// and is never submitted. Enter has to be its own write, after a short gap.
pub const SUBMIT_DELAY: Duration = Duration::from_millis(150);

const KEY_MAP: &[(&str, &[u8])] = &[
    ("enter", b"\r"),
    ("return", b"\r"),
    ("cr", b"\r"),
    ("lf", b"\n"),
    ("newline", b"\n"),
    ("nl", b"\n"),
    ("crlf", b"\r\n"),
    ("tab", b"\t"),
    ("esc", b"\x1b"),
    ("escape", b"\x1b"),
    ("space", b" "),
    ("backspace", b"\x7f"),
    ("bs", b"\x7f"),
    ("delete", b"\x1b[3~"),
    ("up", b"\x1b[A"),
    ("down", b"\x1b[B"),
    ("right", b"\x1b[C"),
    ("left", b"\x1b[D"),
    ("home", b"\x1b[H"),
    ("end", b"\x1b[F"),
    ("ctrl+c", b"\x03"),
    ("ctrl+d", b"\x04"),
    ("ctrl+z", b"\x1a"),
    ("ctrl+l", b"\x0c"),
    ("ctrl+u", b"\x15"),
    ("ctrl+w", b"\x17"),
    ("ctrl+a", b"\x01"),
    ("ctrl+e", b"\x05"),
    ("ctrl+j", b"\n"),
    ("ctrl+m", b"\r"),
];

/// Look up a named key, normalising `Ctrl-C` / `ctrl c` / `CTRL+C`.
pub fn resolve_key(name: &str) -> Result<Vec<u8>, String> {
    let key: String = name
        .trim()
        .to_ascii_lowercase()
        .replace(' ', "")
        .replace('-', "+");
    KEY_MAP
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_vec())
        .ok_or_else(|| format!("Unknown key '{name}'. Try: enter, lf, ctrl+c"))
}

fn simple_escape(c: char) -> Option<u8> {
    match c {
        'n' => Some(0x0A),
        'r' => Some(0x0D),
        't' => Some(0x09),
        'e' | 'E' => Some(0x1B),
        '\\' => Some(0x5C),
        _ => None,
    }
}

/// Interpret `\n`, `\r`, `\t`, `\e` and `\xNN` in a JSON string body.
///
/// An unrecognised escape is passed through verbatim rather than dropped, so a
/// Windows path in a prompt does not silently lose characters.
pub fn decode_escapes(text: &str) -> Vec<u8> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<u8> = Vec::with_capacity(text.len());
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];
        if ch != '\\' || i + 1 >= chars.len() {
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            i += 1;
            continue;
        }

        let next = chars[i + 1];
        if let Some(byte) = simple_escape(next) {
            out.push(byte);
            i += 2;
        } else if (next == 'x' || next == 'X') && i + 3 < chars.len() {
            let hex: String = chars[i + 2..i + 4].iter().collect();
            match u8::from_str_radix(&hex, 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 4;
                }
                Err(_) => {
                    out.push(b'\\');
                    i += 1;
                }
            }
        } else {
            out.push(b'\\');
            i += 1;
        }
    }
    out
}

/// Return `(body, submit_key)` — write them `SUBMIT_DELAY` apart.
pub fn encode_agent_message(text: &str, submit: bool, multiline: bool) -> (Vec<u8>, Vec<u8>) {
    // Newlines are already LF here; the split/join in the Python original was
    // a no-op on the wire, so the body is simply the text as bytes.
    let _ = multiline;
    let body = text.as_bytes().to_vec();

    if body.ends_with(b"\r") || body.ends_with(b"\n") {
        return (body, Vec::new());
    }
    let key = if submit { b"\r".to_vec() } else { Vec::new() };
    (body, key)
}
