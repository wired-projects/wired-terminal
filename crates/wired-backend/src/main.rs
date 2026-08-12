//! The standalone server: one binary, no interpreter, no venv.

fn main() -> std::process::ExitCode {
    wired_backend::init_logging();

    let settings = match wired_backend::settings_from_env() {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!("wired: refusing to start\n{err}");
            return std::process::ExitCode::from(2);
        }
    };

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("wired: could not start the runtime: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let result = runtime.block_on(async {
        wired_backend::serve(settings, async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
    });

    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wired: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
