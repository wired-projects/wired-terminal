//! Argument parsing, by hand.
//!
//! The backend's promise is one binary with nothing underneath it, and a parser
//! for a dozen subcommands is smaller than the dependency that would replace it.
//!
//! Global flags are pulled out in a pre-pass rather than declared per command,
//! so `wired --remote pilot status` and `wired status --remote pilot` are the
//! same command. Everything after `--` is positional, which is how you send a
//! task that begins with a dash.

pub const USAGE: &str = "\
wired — manage a Wired Terminal agent, locally or over SSH

USAGE
  wired <command> [options]

THE AGENT
  status                 service, agent, API and chat in one screen
  ask <text>             send a task and print the reply
  watch                  follow the live transcript (ctrl-c to detach)
  approve [--deny]       answer the approval the agent is waiting on

THE SERVICE
  start [--provider X]   start the service, then the agent session
  stop [--agent]         stop the service (--agent stops only the session)
  restart                restart the service and wait for the API
  update [--check]       is a newer version out, and install it if so
  logs [-f] [-n N]       journalctl, or the log file when there is no systemd
  serve                  run the backend here, in the foreground

SETUP
  doctor [--log]         diagnostics: CLI, sign-in, folder, chat, ports
  telegram [<token>|off] set the bot token and connect, or switch it off
  pair [approve|deny <code>] [unpair <chat>] [reset]
                         Telegram pairing requests
  schedule [run|delete <id>]
                         scheduled tasks
  remote <add|list|remove|default>
                         saved servers for --remote

OPTIONS
  -r, --remote <name>    run against a saved server, over an SSH tunnel
      --url <url>        run against this API base URL instead
      --token <token>    bearer token (default: discovered, see `wired doctor`)
      --json             print the raw API response
      --no-color         no ANSI colour (also honours NO_COLOR)
  -h, --help             this text, or `wired <command> --help`
  -V, --version

EXAMPLES
  wired status
  wired ask \"summarise my git status\"
  wired --remote pilot restart
  wired remote add pilot ubuntu@149.118.134.139
";

#[derive(Debug, Default, Clone)]
pub struct Global {
    /// Saved server to run against. `None` means this machine.
    pub remote: Option<String>,
    /// Explicit API base, e.g. `http://127.0.0.1:8000`. Wins over `remote`.
    pub url: Option<String>,
    pub token: Option<String>,
    pub json: bool,
    pub no_color: bool,
}

#[derive(Debug, Clone)]
pub enum Command {
    Status,
    Start {
        provider: Option<String>,
    },
    Stop {
        agent_only: bool,
    },
    Restart,
    Logs {
        follow: bool,
        lines: usize,
    },
    Serve,
    Ask {
        text: String,
        wait: f64,
    },
    Watch,
    Approve {
        allow: bool,
    },
    Doctor {
        log: bool,
    },
    Update {
        check_only: bool,
        yes: bool,
    },
    Pair(Pair),
    /// Switch the Telegram bridge on with a bot token, or off again. `None` is
    /// "ask me for the token", so onboarding on a server is one command and no
    /// hand-written JSON.
    Telegram(Telegram),
    Schedule(ScheduleCmd),
    Remote(RemoteCmd),
    Help(Option<String>),
    Version,
}

#[derive(Debug, Clone)]
pub enum Telegram {
    /// Report what the bridge is doing; prompt for a token if there is none.
    Show,
    /// Set the bot token and connect. `None` means read it from the terminal
    /// without echoing, so a token never lands in shell history.
    On(Option<String>),
    /// Stop the bridge but keep the token — `pair reset` is what forgets it.
    Off,
}

#[derive(Debug, Clone)]
pub enum Pair {
    List,
    Approve(String),
    Deny(String),
    Unpair(i64),
    /// Forget the bot token and every chat paired to it. `yes` skips the
    /// confirmation, which a script has no way to answer.
    Reset {
        yes: bool,
    },
}

#[derive(Debug, Clone)]
pub enum ScheduleCmd {
    List,
    Run(String),
    Delete(String),
}

#[derive(Debug, Clone)]
pub enum RemoteCmd {
    List,
    /// `name`, `[user@]host`, API port, SSH port, token.
    Add {
        name: String,
        host: String,
        port: u16,
        ssh_port: Option<u16>,
        token: Option<String>,
        unit: Option<String>,
    },
    Remove(String),
    Default(String),
}

#[derive(Debug, Clone)]
pub struct Cli {
    pub global: Global,
    pub command: Command,
}

pub type ParseResult<T> = Result<T, String>;

/// Split `--flag=value` into its halves; `--flag` alone yields `None`.
fn split_inline(arg: &str) -> (&str, Option<&str>) {
    match arg.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (arg, None),
    }
}

fn take_value(
    name: &str,
    inline: Option<&str>,
    rest: &mut std::vec::IntoIter<String>,
) -> ParseResult<String> {
    match inline {
        Some(v) if !v.is_empty() => Ok(v.to_string()),
        _ => rest
            .next()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{name} needs a value")),
    }
}

pub fn parse(argv: Vec<String>) -> ParseResult<Cli> {
    let mut global = Global::default();
    let mut words: Vec<String> = Vec::new();
    let mut help = false;
    let mut version = false;

    // Words after `--` are kept apart all the way to the subcommand, because a
    // parser that re-reads them would take `wired ask -- --wait an hour` for a
    // malformed flag rather than the sentence it is.
    let mut literal: Vec<String> = Vec::new();

    let mut it = argv.into_iter();
    while let Some(arg) = it.next() {
        if arg == "--" {
            literal.extend(it.by_ref());
            break;
        }
        let (name, inline) = split_inline(&arg);
        match name {
            "-r" | "--remote" => global.remote = Some(take_value(name, inline, &mut it)?),
            "--url" => global.url = Some(take_value(name, inline, &mut it)?),
            "--token" => global.token = Some(take_value(name, inline, &mut it)?),
            "--json" => global.json = true,
            "--no-color" | "--no-colour" => global.no_color = true,
            "-h" | "--help" => help = true,
            "-V" | "--version" => version = true,
            _ => words.push(arg),
        }
    }

    if version {
        return Ok(Cli {
            global,
            command: Command::Version,
        });
    }

    let mut words = words.into_iter();
    // `wired -- ask …` is odd but legal: the command is still the first word.
    let head = words
        .next()
        .or_else(|| (!literal.is_empty()).then(|| literal.remove(0)));
    let Some(head) = head else {
        let command = if help {
            Command::Help(None)
        } else {
            // A bare `wired` is nearly always "what is going on right now".
            Command::Status
        };
        return Ok(Cli { global, command });
    };

    // `wired status --help` explains status; `wired help status` does the same.
    if help && head != "help" {
        return Ok(Cli {
            global,
            command: Command::Help(Some(head)),
        });
    }

    let rest: Vec<String> = words.collect();
    if !literal.is_empty() && !matches!(head.as_str(), "ask" | "send") {
        return Err(format!("{head}: `--` is only meaningful for `wired ask`"));
    }

    let command = match head.as_str() {
        "help" => Command::Help(rest.into_iter().next()),
        "status" | "st" => Command::Status,
        "start" | "up" => parse_start(rest)?,
        "stop" | "down" => parse_stop(rest)?,
        "restart" => Command::Restart,
        "logs" | "log" => parse_logs(rest)?,
        "serve" => Command::Serve,
        "ask" | "send" => parse_ask(rest, literal)?,
        "watch" | "tail" => Command::Watch,
        "approve" | "yes" => parse_approve(rest)?,
        "doctor" | "check" => parse_doctor(rest)?,
        "update" | "upgrade" => parse_update(rest)?,
        "pair" => parse_pair(rest)?,
        "telegram" | "chat" => parse_telegram(rest)?,
        "schedule" | "schedules" => parse_schedule(rest)?,
        "remote" | "remotes" => parse_remote(rest)?,
        other => return Err(format!("unknown command: {other} (try `wired --help`)")),
    };

    Ok(Cli { global, command })
}

fn reject_extra(what: &str, rest: &[String]) -> ParseResult<()> {
    match rest.first() {
        Some(extra) => Err(format!("{what}: unexpected argument {extra}")),
        None => Ok(()),
    }
}

fn parse_start(rest: Vec<String>) -> ParseResult<Command> {
    let mut provider = None;
    let mut it = rest.into_iter();
    while let Some(arg) = it.next() {
        let (name, inline) = split_inline(&arg);
        match name {
            "--provider" | "-p" => provider = Some(take_value(name, inline, &mut it)?),
            other => return Err(format!("start: unexpected argument {other}")),
        }
    }
    // `shell` is the last thing `probe_providers` reports: a plain shell in the
    // PTY, which is how you check the plumbing without spending agent tokens.
    if let Some(p) = &provider {
        if !wired_backend::providers::ASSISTANT_PROVIDERS.contains(&p.as_str()) && p != "shell" {
            let known = wired_backend::providers::ASSISTANT_PROVIDERS.join(", ");
            return Err(format!("start: unknown provider {p} ({known} or shell)"));
        }
    }
    Ok(Command::Start { provider })
}

fn parse_stop(rest: Vec<String>) -> ParseResult<Command> {
    let mut agent_only = false;
    for arg in &rest {
        match arg.as_str() {
            "--agent" => agent_only = true,
            other => return Err(format!("stop: unexpected argument {other}")),
        }
    }
    Ok(Command::Stop { agent_only })
}

fn parse_logs(rest: Vec<String>) -> ParseResult<Command> {
    let mut follow = false;
    let mut lines = 60usize;
    let mut it = rest.into_iter();
    while let Some(arg) = it.next() {
        let (name, inline) = split_inline(&arg);
        match name {
            "-f" | "--follow" => follow = true,
            "-n" | "--lines" => {
                let raw = take_value(name, inline, &mut it)?;
                lines = raw
                    .parse()
                    .map_err(|_| format!("logs: {raw} is not a line count"))?;
            }
            other => return Err(format!("logs: unexpected argument {other}")),
        }
    }
    Ok(Command::Logs { follow, lines })
}

fn parse_ask(rest: Vec<String>, literal: Vec<String>) -> ParseResult<Command> {
    // Default: long enough for the agent to actually finish a small task, short
    // enough that a wedged session does not hold the terminal all afternoon.
    let mut wait = 90.0;
    let mut parts: Vec<String> = Vec::new();
    let mut it = rest.into_iter();
    while let Some(arg) = it.next() {
        let (name, inline) = split_inline(&arg);
        match name {
            "--wait" | "-w" => {
                let raw = take_value(name, inline, &mut it)?;
                wait = raw
                    .parse()
                    .map_err(|_| format!("ask: {raw} is not a number of seconds"))?;
            }
            // Fire and forget: the reply still lands in `wired watch` and chat.
            "--no-wait" => wait = 0.0,
            _ => parts.push(arg),
        }
    }

    // Anything the user protected with `--` goes on the end verbatim.
    parts.extend(literal);

    // Unquoted words are joined rather than rejected: `wired ask what changed`
    // is what people type, and quoting is a shell habit, not an intention.
    let text = parts.join(" ").trim().to_string();
    if text.is_empty() {
        return Err("ask: nothing to send — `wired ask \"summarise my git status\"`".into());
    }
    Ok(Command::Ask { text, wait })
}

fn parse_approve(rest: Vec<String>) -> ParseResult<Command> {
    let mut allow = true;
    for arg in &rest {
        match arg.as_str() {
            "--deny" | "--no" => allow = false,
            "--allow" | "--yes" => allow = true,
            other => return Err(format!("approve: unexpected argument {other}")),
        }
    }
    Ok(Command::Approve { allow })
}

fn parse_doctor(rest: Vec<String>) -> ParseResult<Command> {
    let mut log = false;
    for arg in &rest {
        match arg.as_str() {
            "--log" | "--logs" => log = true,
            other => return Err(format!("doctor: unexpected argument {other}")),
        }
    }
    Ok(Command::Doctor { log })
}

fn parse_update(rest: Vec<String>) -> ParseResult<Command> {
    let mut check_only = false;
    let mut yes = false;
    for arg in &rest {
        match arg.as_str() {
            "--check" | "--dry-run" => check_only = true,
            "-y" | "--yes" => yes = true,
            other => return Err(format!("update: unexpected argument {other}")),
        }
    }
    Ok(Command::Update { check_only, yes })
}

fn parse_telegram(rest: Vec<String>) -> ParseResult<Command> {
    let mut it = rest.into_iter();
    let Some(head) = it.next() else {
        return Ok(Command::Telegram(Telegram::Show));
    };
    let rest: Vec<String> = it.collect();
    let sub = match head.as_str() {
        "off" | "disable" => {
            reject_extra("telegram off", &rest)?;
            Telegram::Off
        }
        // `on` with nothing after it prompts, which is the safe way to paste a
        // token: it stays out of the shell history and off the process list.
        "on" | "enable" => {
            let mut rest = rest.into_iter();
            let token = rest.next();
            let leftover: Vec<String> = rest.collect();
            reject_extra("telegram on", &leftover)?;
            Telegram::On(token)
        }
        // A bare token is the shape people will reach for, and BotFather's
        // tokens are unmistakable, so accept it rather than being pedantic.
        token if token.contains(':') => {
            reject_extra("telegram", &rest)?;
            Telegram::On(Some(token.to_string()))
        }
        other => {
            return Err(format!(
                "telegram: expected a bot token, `on`, or `off` (got {other})"
            ))
        }
    };
    Ok(Command::Telegram(sub))
}

fn parse_pair(rest: Vec<String>) -> ParseResult<Command> {
    let mut it = rest.into_iter();
    let Some(head) = it.next() else {
        return Ok(Command::Pair(Pair::List));
    };
    let rest: Vec<String> = it.collect();
    let sub = match head.as_str() {
        "list" => {
            reject_extra("pair list", &rest)?;
            Pair::List
        }
        "approve" | "deny" => {
            let mut rest = rest.into_iter();
            let code = rest
                .next()
                .ok_or_else(|| format!("pair {head}: needs a pairing code"))?;
            reject_extra("pair", &rest.collect::<Vec<_>>())?;
            if head == "approve" {
                Pair::Approve(code)
            } else {
                Pair::Deny(code)
            }
        }
        "reset" => {
            let mut yes = false;
            for arg in &rest {
                match arg.as_str() {
                    "--yes" | "-y" => yes = true,
                    other => return Err(format!("pair reset: unexpected argument {other}")),
                }
            }
            Pair::Reset { yes }
        }
        "unpair" => {
            let mut rest = rest.into_iter();
            let raw = rest
                .next()
                .ok_or_else(|| "pair unpair: needs a chat id".to_string())?;
            reject_extra("pair unpair", &rest.collect::<Vec<_>>())?;
            Pair::Unpair(
                raw.parse()
                    .map_err(|_| format!("pair unpair: {raw} is not a chat id"))?,
            )
        }
        other => return Err(format!("pair: unknown subcommand {other}")),
    };
    Ok(Command::Pair(sub))
}

fn parse_schedule(rest: Vec<String>) -> ParseResult<Command> {
    let mut it = rest.into_iter();
    let Some(head) = it.next() else {
        return Ok(Command::Schedule(ScheduleCmd::List));
    };
    let rest: Vec<String> = it.collect();
    let sub = match head.as_str() {
        "list" => {
            reject_extra("schedule list", &rest)?;
            ScheduleCmd::List
        }
        "run" | "delete" | "rm" => {
            let mut rest = rest.into_iter();
            let id = rest
                .next()
                .ok_or_else(|| format!("schedule {head}: needs a schedule id"))?;
            reject_extra("schedule", &rest.collect::<Vec<_>>())?;
            if head == "run" {
                ScheduleCmd::Run(id)
            } else {
                ScheduleCmd::Delete(id)
            }
        }
        other => return Err(format!("schedule: unknown subcommand {other}")),
    };
    Ok(Command::Schedule(sub))
}

fn parse_remote(rest: Vec<String>) -> ParseResult<Command> {
    let mut it = rest.into_iter();
    let Some(head) = it.next() else {
        return Ok(Command::Remote(RemoteCmd::List));
    };
    let sub = match head.as_str() {
        "list" => {
            reject_extra("remote list", &it.collect::<Vec<_>>())?;
            RemoteCmd::List
        }
        "add" => {
            let name = it
                .next()
                .ok_or_else(|| "remote add: needs a name".to_string())?;
            let host = it
                .next()
                .ok_or_else(|| "remote add: needs [user@]host".to_string())?;
            if name.starts_with('-') || host.starts_with('-') {
                return Err("remote add: usage is `remote add <name> <[user@]host>`".into());
            }
            let mut port = 8000u16;
            let mut ssh_port = None;
            let mut token = None;
            let mut unit = None;
            while let Some(arg) = it.next() {
                let (flag, inline) = split_inline(&arg);
                match flag {
                    "--port" => {
                        let raw = take_value(flag, inline, &mut it)?;
                        port = raw
                            .parse()
                            .map_err(|_| format!("remote add: {raw} is not a port"))?;
                    }
                    "--ssh-port" => {
                        let raw = take_value(flag, inline, &mut it)?;
                        ssh_port = Some(
                            raw.parse()
                                .map_err(|_| format!("remote add: {raw} is not a port"))?,
                        );
                    }
                    "--token" => token = Some(take_value(flag, inline, &mut it)?),
                    "--unit" | "--service" => unit = Some(take_value(flag, inline, &mut it)?),
                    other => return Err(format!("remote add: unexpected argument {other}")),
                }
            }
            RemoteCmd::Add {
                name,
                host,
                port,
                ssh_port,
                token,
                unit,
            }
        }
        "remove" | "rm" => {
            let name = it
                .next()
                .ok_or_else(|| "remote remove: needs a name".to_string())?;
            reject_extra("remote remove", &it.collect::<Vec<_>>())?;
            RemoteCmd::Remove(name)
        }
        "default" | "use" => {
            let name = it
                .next()
                .ok_or_else(|| "remote default: needs a name".to_string())?;
            reject_extra("remote default", &it.collect::<Vec<_>>())?;
            RemoteCmd::Default(name)
        }
        other => return Err(format!("remote: unknown subcommand {other}")),
    };
    Ok(Command::Remote(sub))
}

/// Per-command help. Short on purpose: the top-level text is the reference.
pub fn help_for(topic: &str) -> String {
    let body = match topic {
        "status" => "wired status\n\n  Service state, agent session, API address and chat pairing.\n  Works without the API answering — the service row comes from systemd.\n\n  --json   the raw /api/health response",
        "start" => "wired start [--provider claude|grok|codex|gemini]\n\n  Starts the service if it is not running, waits for the API, then starts\n  the agent session with keep-alive on.",
        "stop" => "wired stop [--agent]\n\n  Stops the service. --agent leaves the service up and stops only the\n  agent session, which keep-alive will not restart until you `wired start`.",
        "restart" => "wired restart\n\n  Restarts the service and waits for /healthz to answer again.",
        "logs" => "wired logs [-f] [-n N]\n\n  journalctl -u wired-terminal when systemd is running it, otherwise the\n  log file in the data directory (`wired doctor` prints the path).\n\n  -f, --follow    keep printing as lines arrive\n  -n, --lines N   how much history to show (default 60)",
        "serve" => "wired serve\n\n  Runs the backend in this terminal, in the foreground, with whatever\n  WIRED_* variables are set. Ctrl-C stops it. Meant for development —\n  on a server the systemd unit does this.",
        "ask" => "wired ask <text> [--wait SECONDS | --no-wait]\n\n  Types the text into the live agent session and prints what comes back.\n  Words are joined, so quotes are optional.\n\n  --wait N    how long to collect the reply (default 90)\n  --no-wait   send and return immediately",
        "watch" => "wired watch\n\n  Follows the transcript the way the /reader page does: one line per turn,\n  no spinners or repaints. Ctrl-C detaches and leaves the agent running.",
        "approve" => "wired approve [--deny]\n\n  Answers the approval dialog the agent is blocked on. Reads the menu\n  before answering, so it picks the option that means yes rather than\n  assuming a position.",
        "update" => "wired update [--check] [--yes]\n\n  Asks the published manifest whether a newer version is out, then\n  reinstalls from source and restarts the service.\n\n  --check  only say what is out; change nothing\n  --yes    do not ask before reinstalling\n\n  Exit codes with --check: 0 up to date, 2 an update is available. That is\n  what makes it usable from cron.\n\n  A desktop install updates by downloading the new app, so there this prints\n  the link rather than pretending it can replace a running .app.",
        "doctor" => "wired doctor [--log]\n\n  The setup checks: agent CLI installed, signed in, working folder\n  writable, ports, chat bridge. Exits non-zero if a check failed.\n\n  --log    also print recent log lines",
        "telegram" | "chat" => "wired telegram                 what the bridge is doing\nwired telegram <token>         set the bot token and connect\nwired telegram on              prompt for the token, without echoing it\nwired telegram off             stop the bridge, keep the token\n\n  Make a bot first: message @BotFather in Telegram, /newbot, answer the two\n  prompts. It replies with a token like 8123456789:AAH...\n\n  `wired telegram on` with no token prompts for one and does not echo it, so\n  it stays out of your shell history and off the process list. That is the\n  one to use over ssh.\n\n  Then message the bot from your phone and `wired pair` to let it in.\n  `off` keeps the token so `wired telegram on` reconnects; `pair reset`\n  forgets it entirely.",
        "pair" => "wired pair                    pending requests and paired chats\nwired pair approve <code>     allow a chat to drive the agent\nwired pair deny <code>\nwired pair unpair <chat-id>   revoke one that was allowed\nwired pair reset [--yes]      forget the bot token and unpair everything\n\n  `unpair` leaves the bot running, so that phone can pair again with a fresh\n  code. `reset` throws the token away too — use it when rotating to a new bot,\n  and revoke the old token in BotFather afterwards.",
        "schedule" => "wired schedule                list scheduled tasks and when they next run\nwired schedule run <id>       run one now\nwired schedule delete <id>",
        "remote" => "wired remote add <name> <[user@]host> [--port 8000] [--ssh-port 22]\n                                     [--token X] [--unit wired-terminal]\nwired remote list\nwired remote remove <name>\nwired remote default <name>   used when --remote is not given\n\n  A remote is reached by opening an SSH tunnel to its loopback API for the\n  duration of the command, so the server does not need an open port.\n  Service commands (start/stop/restart/logs) run over ssh instead.\n\n  \
                     `status` uses both — one ssh for systemd's view of the unit, one for\n  \
                     the tunnel — so keep the key in an agent, or type the passphrase twice.",
        other => return format!("no help for `{other}`\n\n{USAGE}"),
    };
    format!("{body}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(line: &str) -> Cli {
        parse(line.split_whitespace().map(String::from).collect()).expect(line)
    }

    #[test]
    fn bare_invocation_is_status() {
        assert!(matches!(parse_ok("").command, Command::Status));
    }

    #[test]
    fn global_flags_land_on_either_side_of_the_command() {
        let before = parse_ok("--remote pilot status");
        let after = parse_ok("status --remote pilot");
        assert_eq!(before.global.remote.as_deref(), Some("pilot"));
        assert_eq!(after.global.remote.as_deref(), Some("pilot"));
    }

    #[test]
    fn inline_values_are_accepted() {
        let cli = parse_ok("--remote=pilot --token=abc status");
        assert_eq!(cli.global.remote.as_deref(), Some("pilot"));
        assert_eq!(cli.global.token.as_deref(), Some("abc"));
    }

    #[test]
    fn ask_joins_unquoted_words() {
        match parse_ok("ask what changed today").command {
            Command::Ask { text, wait } => {
                assert_eq!(text, "what changed today");
                assert_eq!(wait, 90.0);
            }
            other => panic!("expected ask, got {other:?}"),
        }
    }

    #[test]
    fn double_dash_protects_leading_dashes() {
        match parse(
            ["ask", "--", "--wait", "is", "a", "word", "here"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        )
        .unwrap()
        .command
        {
            Command::Ask { text, wait } => {
                assert_eq!(text, "--wait is a word here");
                assert_eq!(wait, 90.0);
            }
            other => panic!("expected ask, got {other:?}"),
        }
    }

    #[test]
    fn words_before_and_after_a_double_dash_join_in_order() {
        match parse(
            ["ask", "rename", "--", "--force", "in", "the", "docs"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        )
        .unwrap()
        .command
        {
            Command::Ask { text, .. } => assert_eq!(text, "rename --force in the docs"),
            other => panic!("expected ask, got {other:?}"),
        }
    }

    #[test]
    fn a_double_dash_on_a_command_that_takes_no_text_is_an_error() {
        let err = parse(["logs", "--", "-f"].iter().map(|s| s.to_string()).collect()).unwrap_err();
        assert!(err.contains("only meaningful"), "{err}");
    }

    #[test]
    fn ask_with_no_text_explains_itself() {
        let err = parse(vec!["ask".into()]).unwrap_err();
        assert!(err.contains("nothing to send"), "{err}");
    }

    #[test]
    fn command_help_beats_top_level_help() {
        match parse_ok("logs --help").command {
            Command::Help(Some(topic)) => assert_eq!(topic, "logs"),
            other => panic!("expected help, got {other:?}"),
        }
        match parse_ok("--help").command {
            Command::Help(None) => {}
            other => panic!("expected bare help, got {other:?}"),
        }
    }

    #[test]
    fn unknown_command_names_itself() {
        let err = parse(vec!["frobnicate".into()]).unwrap_err();
        assert!(err.contains("frobnicate"), "{err}");
    }

    #[test]
    fn remote_add_parses_host_and_ports() {
        match parse_ok("remote add pilot ubuntu@149.118.134.139 --port 8080 --ssh-port 2222")
            .command
        {
            Command::Remote(RemoteCmd::Add {
                name,
                host,
                port,
                ssh_port,
                ..
            }) => {
                assert_eq!(name, "pilot");
                assert_eq!(host, "ubuntu@149.118.134.139");
                assert_eq!(port, 8080);
                assert_eq!(ssh_port, Some(2222));
            }
            other => panic!("expected remote add, got {other:?}"),
        }
    }

    #[test]
    fn pair_reset_needs_an_explicit_yes_to_skip_the_prompt() {
        match parse_ok("pair reset").command {
            Command::Pair(Pair::Reset { yes }) => assert!(!yes),
            other => panic!("expected pair reset, got {other:?}"),
        }
        match parse_ok("pair reset --yes").command {
            Command::Pair(Pair::Reset { yes }) => assert!(yes),
            other => panic!("expected pair reset, got {other:?}"),
        }
    }

    #[test]
    fn approve_defaults_to_yes_and_deny_flips_it() {
        assert!(matches!(
            parse_ok("approve").command,
            Command::Approve { allow: true }
        ));
        assert!(matches!(
            parse_ok("approve --deny").command,
            Command::Approve { allow: false }
        ));
    }

    #[test]
    fn logs_rejects_a_non_numeric_count() {
        let err = parse(vec!["logs".into(), "-n".into(), "lots".into()]).unwrap_err();
        assert!(err.contains("line count"), "{err}");
    }
}
