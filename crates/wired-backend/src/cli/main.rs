//! `wired` — the command line for a Wired Terminal agent.
//!
//! Same crate as the server, so the two cannot drift apart about where settings
//! live or what the API returns. Everything here is a reading of an endpoint
//! that already exists; `docs/server.md` documents the `curl` each one replaces.

mod args;
mod client;
mod cmd;
mod profile;
mod service;
mod tui;
mod ui;

use args::Command;
use profile::Config;
use service::Supervisor;
use ui::Ui;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cli = match args::parse(argv) {
        Ok(cli) => cli,
        Err(message) => {
            // No Ui yet — the parser failed before we knew about --no-color.
            eprintln!("wired: {message}");
            return std::process::ExitCode::from(1);
        }
    };

    let ui = Ui::new(cli.global.no_color);

    // The three commands that never touch a network stack answer here, so
    // `wired --help` works on a machine with nothing installed.
    match &cli.command {
        Command::Version => {
            println!("wired {VERSION}");
            return std::process::ExitCode::SUCCESS;
        }
        Command::Help(topic) => {
            match topic {
                Some(topic) => print!("{}", args::help_for(topic)),
                None => print!("{}", args::USAGE),
            }
            return std::process::ExitCode::SUCCESS;
        }
        Command::Serve => {
            return match service::serve_foreground() {
                Ok(code) => std::process::ExitCode::from(code.clamp(0, 255) as u8),
                Err(message) => {
                    ui.error(&message);
                    std::process::ExitCode::from(1)
                }
            };
        }
        _ => {}
    }

    // Only now is a runtime worth building. Current-thread: this process makes
    // one request at a time and then exits.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            ui.error(&format!("could not start the runtime: {e}"));
            return std::process::ExitCode::from(1);
        }
    };

    match runtime.block_on(run(&ui, cli)) {
        Ok(code) => std::process::ExitCode::from(code.clamp(0, 255) as u8),
        Err(message) => {
            ui.error(&message);
            std::process::ExitCode::from(1)
        }
    }
}

async fn run(ui: &Ui, cli: args::Cli) -> client::Result<i32> {
    let mut config = Config::load();

    // Remotes are edited locally and never reach the network.
    if let Command::Remote(sub) = &cli.command {
        return cmd::remote(ui, &mut config, sub);
    }

    let target = profile::resolve(&cli.global, &config)?;
    let supervisor = Supervisor::for_target(&target);
    let json = cli.global.json;

    if let Command::Tui { from_bare } = cli.command {
        // The whole tty rule lives here, so `tui::run` can assume a terminal.
        // A pipe or `--json` on a bare `wired` is still `status`, which is what
        // scripts and cron already expect; `wired tui` spelled out says so
        // rather than silently becoming something else.
        let tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
        if json || (from_bare && !tty) {
            return cmd::status(ui, &target, &supervisor, json).await;
        }
        if !tty {
            return Err("the slash TUI needs a terminal — pipe `wired status` instead".into());
        }
        return tui::run(ui, &target, &supervisor, &mut config).await;
    }

    cmd::execute(ui, &cli.command, &target, &supervisor, &mut config, json).await
}
