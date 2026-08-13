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

    match &cli.command {
        Command::Status => cmd::status(ui, &target, &supervisor, json).await,
        Command::Start { provider } => cmd::start(ui, &target, &supervisor, provider.clone()).await,
        Command::Stop { agent_only } => cmd::stop(ui, &target, &supervisor, *agent_only).await,
        Command::Restart => cmd::restart(ui, &target, &supervisor).await,
        Command::Logs { follow, lines } => {
            supervisor.logs(*follow, *lines)?;
            Ok(cmd::EXIT_OK)
        }
        Command::Ask { text, wait } => cmd::ask(ui, &target, text, *wait, json).await,
        Command::Watch => cmd::watch(ui, &target).await,
        Command::Approve { allow } => cmd::approve(ui, &target, *allow).await,
        Command::Doctor { log } => cmd::doctor(ui, &target, *log, json).await,
        Command::Update { check_only, yes } => cmd::update(ui, &target, *check_only, *yes).await,
        Command::Pair(sub) => cmd::pair(ui, &target, sub, json).await,
        Command::Schedule(sub) => cmd::schedule(ui, &target, sub, json).await,
        // Handled before the runtime was built.
        Command::Remote(_) | Command::Serve | Command::Help(_) | Command::Version => {
            Ok(cmd::EXIT_OK)
        }
    }
}
