//! Unit tests for the pure logic — no PTY, no network.
//!
//! These cover the parts that are easy to break silently: key encoding, the
//! security checks, and the transcript filter that decides what a caller
//! actually sees from a repainting TUI.

use wired_backend::config::{load_settings, Settings};
use wired_backend::keys::{decode_escapes, encode_agent_message, resolve_key};
use wired_backend::security::{origin_valid, token_valid};
use wired_backend::transcript::{screen_to_transcript, Kind, Row, TranscriptTail};

fn row(kind: Kind, text: &str) -> Row {
    Row {
        kind,
        text: text.to_string(),
    }
}

// ── keys ────────────────────────────────────────────────────────────────

#[test]
fn resolve_key_normalises_spelling() {
    assert_eq!(resolve_key("Ctrl-C").unwrap(), b"\x03");
    assert_eq!(resolve_key("CTRL + C").unwrap(), b"\x03");
    assert_eq!(resolve_key("enter").unwrap(), b"\r");
}

#[test]
fn resolve_key_rejects_unknown() {
    let err = resolve_key("hyperspace").unwrap_err();
    assert!(err.contains("Unknown key"), "got: {err}");
}

#[test]
fn decode_escapes_cases() {
    assert_eq!(decode_escapes(r"a\nb"), b"a\nb");
    assert_eq!(decode_escapes(r"\x1b[A"), b"\x1b[A");
    assert_eq!(decode_escapes(r"tab\there"), b"tab\there");
    assert_eq!(decode_escapes(r"back\\slash"), br"back\slash");
    // An unknown escape is preserved rather than swallowed.
    assert_eq!(decode_escapes(r"C:\Users"), br"C:\Users");
}

#[test]
fn encode_agent_message_separates_submit() {
    // Enter must be a separate write or Ink CLIs read it as a paste.
    let (body, submit) = encode_agent_message("hello", true, false);
    assert_eq!(body, b"hello");
    assert_eq!(submit, b"\r");
}

#[test]
fn encode_agent_message_multiline_uses_soft_breaks() {
    let (body, submit) = encode_agent_message("one\ntwo", true, true);
    assert_eq!(body, b"one\ntwo");
    assert_eq!(submit, b"\r");
}

#[test]
fn encode_agent_message_does_not_double_submit() {
    let (body, submit) = encode_agent_message("done\r", true, false);
    assert_eq!(body, b"done\r");
    assert!(submit.is_empty());
}

#[test]
fn encode_agent_message_without_submit() {
    let (_body, submit) = encode_agent_message("draft", false, false);
    assert!(submit.is_empty());
}

// ── security ────────────────────────────────────────────────────────────

#[test]
fn token_valid_only_with_exact_match() {
    let settings = Settings {
        auth_token: "secret".into(),
        ..Default::default()
    };
    assert!(token_valid(&settings, "secret"));
    assert!(!token_valid(&settings, "secre"));
    assert!(!token_valid(&settings, ""));
}

#[test]
fn token_check_is_a_noop_when_unset() {
    let settings = Settings::default();
    assert!(token_valid(&settings, ""));
}

#[test]
fn origin_allow_list() {
    let settings = Settings::default();
    assert!(origin_valid(&settings, Some("http://localhost:5173")));
    assert!(origin_valid(&settings, Some("tauri://localhost")));
    assert!(!origin_valid(&settings, Some("https://evil.example")));
    // No Origin at all is a script, not a page.
    assert!(origin_valid(&settings, None));
}

#[test]
fn wildcard_origin_opt_in() {
    let settings = Settings {
        allow_any_origin: true,
        ..Default::default()
    };
    assert!(origin_valid(&settings, Some("https://evil.example")));
}

// `load_settings` reads process-wide env, so these run under one lock rather
// than racing each other across test threads.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_env<T>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> T) -> T {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved: Vec<(String, Option<String>)> = vars
        .iter()
        .map(|(k, _)| (k.to_string(), std::env::var(k).ok()))
        .collect();
    for (k, v) in vars {
        match v {
            Some(val) => std::env::set_var(k, val),
            None => std::env::remove_var(k),
        }
    }
    let out = f();
    for (k, v) in saved {
        match v {
            Some(val) => std::env::set_var(&k, val),
            None => std::env::remove_var(&k),
        }
    }
    out
}

#[test]
fn refuses_network_bind_without_token() {
    with_env(
        &[
            ("WIRED_HOST", Some("0.0.0.0")),
            ("WIRED_AUTH_TOKEN", None),
            ("WIRED_ALLOW_INSECURE", None),
        ],
        || {
            let err = load_settings().unwrap_err();
            assert!(err.0.contains("WIRED_AUTH_TOKEN"), "got: {}", err.0);
        },
    );
}

#[test]
fn network_bind_allowed_with_token() {
    with_env(
        &[
            ("WIRED_HOST", Some("0.0.0.0")),
            ("WIRED_AUTH_TOKEN", Some("s3cret")),
            ("WIRED_ALLOW_INSECURE", None),
        ],
        || {
            let settings = load_settings().expect("token makes the bind legal");
            assert!(settings.auth_required());
            assert!(!settings.is_loopback());
        },
    );
}

#[test]
fn network_bind_allowed_with_explicit_override() {
    with_env(
        &[
            ("WIRED_HOST", Some("0.0.0.0")),
            ("WIRED_AUTH_TOKEN", None),
            ("WIRED_ALLOW_INSECURE", Some("1")),
        ],
        || {
            let settings = load_settings().expect("explicit override is allowed");
            assert!(!settings.auth_required());
        },
    );
}

// ── transcript filtering ────────────────────────────────────────────────

#[test]
fn transcript_keeps_prose_and_drops_chrome() {
    let screen = [
        "╭─────────────── Claude Code v2.1.226 ───────────────╮",
        "│  Welcome back!                                     │",
        "╰────────────────────────────────────────────────────╯",
        "❯ summarise my git status",
        "⏺ The working tree is clean.",
        "  Sautéed for 3s",
        "? for shortcuts",
        "⏵⏵ bypass permissions on",
    ]
    .join("\n");

    let rows = screen_to_transcript(&screen, false);
    assert!(rows.contains(&row(Kind::User, "summarise my git status")));
    assert!(rows.contains(&row(Kind::Text, "The working tree is clean.")));
    // Spinner, banner and status bar are all gone.
    let texts: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
    assert!(!texts.iter().any(|t| t.contains("Sautéed")), "{texts:?}");
    assert!(!texts.iter().any(|t| t.contains("shortcuts")), "{texts:?}");
    assert!(
        !texts.iter().any(|t| t.contains("bypass permissions")),
        "{texts:?}"
    );
}

#[test]
fn trailing_composer_row_is_not_a_turn() {
    // The bottom ❯ row holds unsent text and rotating suggestions.
    let rows = screen_to_transcript("⏺ done\n❯ this is still being typed", false);
    assert!(!rows.contains(&row(Kind::User, "this is still being typed")));
}

#[test]
fn tail_does_not_resend_on_repaint() {
    let mut tail = TranscriptTail::default();
    let screen = "⏺ first line\n⏺ second line\n⏺ third line";
    assert_eq!(tail.update(screen, false, false).len(), 3);
    // The same viewport painted again must produce nothing new.
    assert!(tail.update(screen, false, false).is_empty());
}

#[test]
fn tail_emits_only_new_rows_when_viewport_scrolls() {
    let mut tail = TranscriptTail::default();
    tail.update("⏺ one\n⏺ two", false, false);
    let new = tail.update("⏺ two\n⏺ three", false, false);
    assert_eq!(new, vec![row(Kind::Text, "three")]);
}

#[test]
fn notice_is_sent_once_even_when_it_moves() {
    // A pinned warning drifts and re-truncates; it should still arrive once.
    // De-duplication keys on the first 48 characters, so this uses the shape
    // the CLIs actually produce: one long warning cut at different widths.
    let full = "⚠ Transcript saving is off — restart with CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1";
    let truncated = "⚠ Transcript saving is off — restart with CLAUDE_COD…";

    let mut tail = TranscriptTail::default();
    assert_eq!(
        tail.update(full, false, false),
        vec![row(Kind::Notice, full)]
    );
    // Same warning, redrawn lower and cut shorter — plus one genuinely new row.
    assert_eq!(
        tail.update(&format!("⏺ The tree is clean.\n{truncated}"), false, false),
        vec![row(Kind::Text, "The tree is clean.")]
    );
}

#[test]
fn bare_status_verbs_are_dropped() {
    // `working`, `thinking` and friends are spinner rows, not prose.
    let rows = screen_to_transcript("⏺ thinking\n⏺ working\n⏺ Found 3 files.", false);
    assert_eq!(rows, vec![row(Kind::Text, "Found 3 files.")]);
}

#[test]
fn approval_prompt_survives_the_box_filter() {
    // Boxed rows are chrome, except the ones asking the operator something.
    let rows = screen_to_transcript("│  Do you want to proceed? │", false);
    assert!(rows.contains(&row(Kind::Prompt, "Do you want to proceed?")));
}
