//! Desktop shell for the Wired backend.
//!
//! The backend is a library here, not a second executable: the API runs on this
//! process's own runtime. That removes the whole sidecar apparatus — no frozen
//! interpreter, no launcher process to orphan, nothing to keep alive after a
//! crash.
//!
//! It still checks whether a backend is already listening before starting one,
//! because the point of the product is an agent that outlives any one UI: a
//! systemd service or a `cargo run` session must not be duplicated or killed by
//! opening the app.
//!
//! Three things the window is responsible for, and the backend cannot be:
//!   • telling the frontend where the API actually landed, and with which token
//!   • surviving its own window being closed (the tray)
//!   • the native bits — a folder picker, a notification, a login item

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::oneshot;

const BACKEND_HOST: &str = "127.0.0.1";
const PROBE_TIMEOUT: Duration = Duration::from_millis(400);

/// Set when *we* started the backend. `None` means it was already up and
/// belongs to someone else — leave it alone on exit.
struct Backend(Mutex<Option<oneshot::Sender<()>>>);

/// Where the API ended up. Not a constant any more: the app steps past a busy
/// port rather than showing "offline" forever, and the window has to be told.
struct Endpoint(Mutex<Runtime>);

#[derive(Clone, Default, serde::Serialize)]
struct Runtime {
    port: u16,
    token: String,
}

/// Is a Wired backend already answering on the port?
///
/// A plain connect() would also succeed against an unrelated process, and then
/// we would sit next to it doing nothing while the UI showed a dead API. So ask
/// for /healthz and look for the service name it reports.
fn backend_is_running(port: u16) -> bool {
    let Ok(addr) = format!("{BACKEND_HOST}:{port}").parse::<SocketAddr>() else {
        return false;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(PROBE_TIMEOUT));

    let request =
        format!("GET /healthz HTTP/1.1\r\nHost: {BACKEND_HOST}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = String::new();
    let _ = stream.take(4096).read_to_string(&mut response);
    response.contains("200") && response.contains("wired-terminal")
}

/// GUI processes inherit a bare PATH, so a CLI installed under the user's home
/// would be invisible to the agent even though it works in a terminal.
fn widen_path() {
    let current = std::env::var_os("PATH").unwrap_or_default();
    // `split_paths`/`join_paths` rather than ':' — Windows separates with ';'
    // and the old hand-rolled split silently produced one giant garbage entry.
    let mut parts: Vec<PathBuf> = std::env::split_paths(&current).collect();
    let mut extra: Vec<PathBuf> = Vec::new();

    if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
        extra.push(home.join(".local/bin"));
        extra.push(home.join(".claude/local"));
        // Where the setup wizard installs the CLIs, so they are found on the
        // very next launch without the user doing anything. Windows npm puts
        // its shims in the prefix itself rather than a bin/ inside it.
        extra.push(home.join(".npm-global/bin"));
        extra.push(home.join(".npm-global"));
        extra.push(home.join(".bun/bin"));
        #[cfg(windows)]
        extra.push(home.join("AppData/Roaming/npm"));
    }
    #[cfg(not(windows))]
    {
        extra.push(PathBuf::from("/opt/homebrew/bin"));
        extra.push(PathBuf::from("/usr/local/bin"));
    }

    for dir in extra {
        if !parts.contains(&dir) {
            parts.push(dir);
        }
    }
    if let Ok(joined) = std::env::join_paths(parts) {
        std::env::set_var("PATH", joined);
    }
}

/// Bind the socket and start serving, reporting where we landed.
fn start_backend() -> Result<(oneshot::Sender<()>, Runtime), String> {
    widen_path();
    std::env::set_var("WIRED_HOST", BACKEND_HOST);
    // Marks this process as the packaged app (port fallback, paths). Approvals
    // are always-on either way; `WIRED_AGENT_AUTO_APPROVE=0` is the override.
    std::env::set_var("WIRED_PROFILE", "desktop");

    let settings = wired_backend::settings_from_env().map_err(|e| e.to_string())?;
    // Bind before serving so the window can be told the real port — the
    // requested one may have been taken by something else entirely.
    let (listener, settings) = tauri::async_runtime::block_on(wired_backend::bind(settings))
        .map_err(|e| e.to_string())?;

    let runtime = Runtime {
        port: settings.port,
        token: settings.auth_token.clone(),
    };
    let (tx, rx) = oneshot::channel::<()>();

    // Tauri's runtime is a Tokio runtime, so the API is just another task on it.
    tauri::async_runtime::spawn(async move {
        let shutdown = async {
            let _ = rx.await;
        };
        if let Err(e) = wired_backend::serve_on(listener, settings, shutdown).await {
            eprintln!("[wired] backend stopped: {e}");
        }
    });

    Ok((tx, runtime))
}

// ── commands the web layer cannot do for itself ─────────────────────────

/// Where to talk to the API, and with what.
///
/// This replaces `VITE_AUTH_TOKEN`, which was read at *build* time and so could
/// never be given to a packaged app afterwards.
#[tauri::command]
fn runtime_config(endpoint: State<'_, Endpoint>) -> Runtime {
    endpoint.0.lock().unwrap().clone()
}

/// Native folder picker. "Which folder may your assistant read and write?" is a
/// question with a real answer, not a path typed into a text box.
#[tauri::command]
async fn pick_folder(app: AppHandle, start: Option<String>) -> Option<String> {
    let mut builder = app.dialog().file().set_title("Choose your assistant's folder");
    if let Some(start) = start.filter(|s| !s.is_empty()) {
        builder = builder.set_directory(PathBuf::from(start));
    }
    let (tx, rx) = oneshot::channel();
    builder.pick_folder(move |folder| {
        let _ = tx.send(folder.and_then(|f| f.into_path().ok()));
    });
    rx.await
        .ok()
        .flatten()
        .map(|path| path.display().to_string())
}

/// Reveal a file or folder in the system file manager.
#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(&path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not open {path}: {e}"))
}

/// A desktop notification — for a finished task, or an approval nobody has
/// answered.
#[tauri::command]
fn notify(app: AppHandle, title: String, body: String) -> Result<(), String> {
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|e| e.to_string())
}

/// Start Wired when the user logs in — the other half of "keeps running".
#[tauri::command]
fn set_login_item(app: AppHandle, enabled: bool) -> Result<bool, String> {
    let manager = app.autolaunch();
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    result.map_err(|e| e.to_string())?;
    manager.is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
fn login_item_enabled(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

// ── tray ────────────────────────────────────────────────────────────────

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// A visible Stop, and a way back to the window once it has been closed.
///
/// Without this the backend lives and dies with the window, so an assistant
/// that is supposed to answer at 3am stops the moment its window is shut.
fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Wired", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "Stop the assistant", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Wired", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &stop, &quit])?;

    TrayIconBuilder::with_id("wired")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Wired — your assistant is running")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_window(app),
            "stop" => {
                // Same call the Stop button makes, so there is one meaning of
                // "stop" no matter where it is pressed.
                let endpoint = app.state::<Endpoint>().0.lock().unwrap().clone();
                std::thread::spawn(move || stop_assistant(&endpoint));
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let tauri::tray::TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// POST /api/agent/stop over a plain socket — no HTTP client needed for one
/// request to our own loopback port.
fn stop_assistant(endpoint: &Runtime) {
    let Ok(addr) = format!("{BACKEND_HOST}:{}", endpoint.port).parse::<SocketAddr>() else {
        return;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) else {
        return;
    };
    let auth = if endpoint.token.is_empty() {
        String::new()
    } else {
        format!("Authorization: Bearer {}\r\n", endpoint.token)
    };
    let request = format!(
        "POST /api/agent/stop HTTP/1.1\r\nHost: {BACKEND_HOST}\r\n{auth}\
         Content-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let _ = stream.write_all(request.as_bytes());
    let mut sink = String::new();
    let _ = stream.take(1024).read_to_string(&mut sink);
}

/// Download the pending update, replace this `.app` with it, and restart.
///
/// Driven from Rust rather than from `@tauri-apps/plugin-updater` because the
/// frontend reaches the shell through `window.__TAURI__` and carries no Tauri npm
/// packages; one command keeps it that way.
///
/// The artefact's minisign signature is checked against the public key in
/// `tauri.conf.json` before anything is written, which is the part that makes
/// replacing a running application safe: an unsigned or tampered archive is
/// refused, so this cannot be turned into a way to install something else.
///
/// Only returns on failure — `restart()` does not come back.
#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Result<String, String> {
    #[cfg(desktop)]
    {
        use tauri_plugin_updater::UpdaterExt;

        let updater = app
            .updater()
            .map_err(|e| format!("no updater available: {e}"))?;
        let Some(update) = updater
            .check()
            .await
            .map_err(|e| format!("could not check for an update: {e}"))?
        else {
            return Err("this is already the newest version".into());
        };

        update
            .download_and_install(|_chunk, _total| {}, || {})
            .await
            .map_err(|e| format!("could not install {}: {e}", update.version))?;

        app.restart();
    }
    #[cfg(not(desktop))]
    {
        let _ = app;
        Err("updates are a desktop feature".into())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    wired_backend::init_logging();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(Backend(Mutex::new(None)))
        .manage(Endpoint(Mutex::new(Runtime::default())))
        .invoke_handler(tauri::generate_handler![
            runtime_config,
            pick_folder,
            open_path,
            notify,
            set_login_item,
            login_item_enabled,
            install_update,
        ])
        .setup(|app| {
            // Desktop only: there is no in-place replacement of a mobile app,
            // and registering it there fails the build rather than the call.
            #[cfg(desktop)]
            app.handle()
                .plugin(tauri_plugin_updater::Builder::new().build())?;

            let wanted = wired_backend::settings_from_env()
                .map(|s| s.port)
                .unwrap_or(wired_backend::config::DEFAULT_PORT);

            if backend_is_running(wanted) {
                println!("[wired] backend already listening on {BACKEND_HOST}:{wanted}");
                *app.state::<Endpoint>().0.lock().unwrap() = Runtime {
                    port: wanted,
                    token: wired_backend::settings_store::auth_token(),
                };
            } else {
                match start_backend() {
                    Ok((stop, runtime)) => {
                        println!("[wired] backend started in-process on port {}", runtime.port);
                        *app.state::<Backend>().0.lock().unwrap() = Some(stop);
                        *app.state::<Endpoint>().0.lock().unwrap() = runtime;
                    }
                    // Not fatal: the UI has an offline state and a retry, which
                    // is a better failure than a window that refuses to open.
                    Err(err) => eprintln!("[wired] {err}"),
                }
            }

            if let Err(e) = build_tray(app.handle()) {
                eprintln!("[wired] no tray icon: {e}");
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Closing the window is not quitting: the assistant is supposed
                // to still be there in the morning. The tray has the real Quit.
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| {
        if let RunEvent::Exit = event {
            // Only ever stops a backend we started — see `Backend`. The
            // graceful-shutdown path kills the agent PTY on the way out.
            if let Some(stop) = app_handle.state::<Backend>().0.lock().unwrap().take() {
                let _ = stop.send(());
                // Give the PTY teardown a moment; the process is exiting anyway.
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    });
}
