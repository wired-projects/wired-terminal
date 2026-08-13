//! Starting and stopping the thing that runs the agent.
//!
//! Three answers, because there are three ways a Wired backend ends up running:
//! systemd installed it (a server), you started it yourself (a laptop), or it
//! is on another machine and every verb becomes an `ssh`. `Target` decides
//! which, so no command has to ask.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
// Only the unix stop path polls for the process to go away; on Windows this
// would be an unused import, which `clippy -D warnings` rejects.
#[cfg(unix)]
use std::time::Instant;

use super::client::Result;
use super::profile::Target;

pub struct State {
    pub running: bool,
    /// `running`, `stopped`, `not installed`, `unknown`.
    pub detail: String,
    pub pid: Option<u32>,
    pub uptime: Option<i64>,
    pub manager: &'static str,
}

impl State {
    fn stopped(detail: &str, manager: &'static str) -> State {
        State {
            running: false,
            detail: detail.to_string(),
            pid: None,
            uptime: None,
            manager,
        }
    }
}

pub enum Supervisor {
    Systemd {
        unit: String,
    },
    /// A backend we started ourselves, tracked by a pid file.
    Process,
    Remote {
        host: String,
        ssh_port: Option<u16>,
        unit: String,
    },
    /// `--url` points somewhere we can talk to but not administer.
    Unmanaged,
}

impl Supervisor {
    pub fn for_target(target: &Target) -> Supervisor {
        match target {
            Target::Url { .. } => Supervisor::Unmanaged,
            Target::Remote { remote, .. } => Supervisor::Remote {
                host: remote.host.clone(),
                ssh_port: remote.ssh_port,
                unit: remote.unit().to_string(),
            },
            Target::Local { unit, .. } => {
                if systemd_manages(unit) {
                    Supervisor::Systemd { unit: unit.clone() }
                } else {
                    Supervisor::Process
                }
            }
        }
    }

    pub fn state(&self) -> State {
        match self {
            Supervisor::Systemd { unit } => systemd_state(&systemctl_show(unit, None)),
            Supervisor::Process => process_state(),
            Supervisor::Remote {
                host,
                ssh_port,
                unit,
            } => {
                let command = remote_state_command(unit);
                match ssh_capture(host, *ssh_port, &[&command]) {
                    Ok(out) => {
                        let lines: Vec<String> = out.lines().map(str::to_string).collect();
                        systemd_state(&lines)
                    }
                    Err(e) => State::stopped(&e, "ssh"),
                }
            }
            Supervisor::Unmanaged => State::stopped("not managed from here", "-"),
        }
    }

    pub fn start(&self) -> Result<()> {
        match self {
            Supervisor::Systemd { unit } => systemctl(unit, "start"),
            Supervisor::Process => process_start(),
            Supervisor::Remote {
                host,
                ssh_port,
                unit,
            } => ssh_run(host, *ssh_port, &["sudo", "systemctl", "start", unit], true),
            Supervisor::Unmanaged => Err(unmanaged()),
        }
    }

    pub fn stop(&self) -> Result<()> {
        match self {
            Supervisor::Systemd { unit } => systemctl(unit, "stop"),
            Supervisor::Process => process_stop(),
            Supervisor::Remote {
                host,
                ssh_port,
                unit,
            } => ssh_run(host, *ssh_port, &["sudo", "systemctl", "stop", unit], true),
            Supervisor::Unmanaged => Err(unmanaged()),
        }
    }

    pub fn restart(&self) -> Result<()> {
        match self {
            Supervisor::Systemd { unit } => systemctl(unit, "restart"),
            Supervisor::Process => {
                // Ignore a failure to stop something that was not running.
                let _ = process_stop();
                process_start()
            }
            Supervisor::Remote {
                host,
                ssh_port,
                unit,
            } => ssh_run(
                host,
                *ssh_port,
                &["sudo", "systemctl", "restart", unit],
                true,
            ),
            Supervisor::Unmanaged => Err(unmanaged()),
        }
    }

    pub fn logs(&self, follow: bool, lines: usize) -> Result<()> {
        match self {
            Supervisor::Systemd { unit } => {
                let mut cmd = Command::new("journalctl");
                cmd.arg("-u").arg(unit).arg("-n").arg(lines.to_string());
                if follow {
                    cmd.arg("-f");
                }
                run_inherited(cmd)
            }
            Supervisor::Process | Supervisor::Unmanaged => tail_log_file(follow, lines),
            Supervisor::Remote {
                host,
                ssh_port,
                unit,
            } => {
                let n = lines.to_string();
                let mut args = vec!["journalctl", "-u", unit.as_str(), "-n", n.as_str()];
                if follow {
                    args.push("-f");
                }
                ssh_run(host, *ssh_port, &args, follow)
            }
        }
    }

    /// Can this supervisor start and stop anything, or only report?
    ///
    /// `wired --url …` can talk to an agent perfectly well without being able
    /// to see the machine under it, and that is not a fault to colour red.
    pub fn is_managed(&self) -> bool {
        !matches!(self, Supervisor::Unmanaged)
    }

    /// The name for the first row of `wired status`.
    pub fn label(&self) -> String {
        match self {
            Supervisor::Systemd { unit } | Supervisor::Remote { unit, .. } => unit.clone(),
            Supervisor::Process | Supervisor::Unmanaged => "wired-backend".to_string(),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Supervisor::Systemd { unit } => format!("systemd ({unit})"),
            Supervisor::Process => "this shell".to_string(),
            Supervisor::Remote { host, unit, .. } => format!("systemd on {host} ({unit})"),
            Supervisor::Unmanaged => "not managed from here".to_string(),
        }
    }
}

fn unmanaged() -> String {
    "--url points at an API, not a machine — run this on the host, or use --remote".to_string()
}

// ── systemd ─────────────────────────────────────────────────────────────

fn show_args(unit: &str) -> Vec<&str> {
    vec![
        "systemctl",
        "show",
        unit,
        "--property=LoadState",
        "--property=ActiveState",
        "--property=SubState",
        "--property=MainPID",
        "--property=ExecMainStartTimestampMonotonic",
    ]
}

/// Wrap for `sh -c`, which is what ssh hands its argument to.
fn shell_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', r"'\''"))
}

/// The same properties, plus the far side's own seconds-since-boot.
///
/// systemd reports the start time as a monotonic stamp — microseconds since
/// *that machine* booted — so converting it with our `/proc/uptime` would be
/// wrong on another Linux box and impossible on a mac, which has no such file.
fn remote_state_command(unit: &str) -> String {
    format!(
        "systemctl show {} {}; printf 'BootUptime=%s\\n' \"$(cut -d' ' -f1 /proc/uptime)\"",
        shell_quote(unit),
        show_args(unit)[3..].join(" ")
    )
}

/// Is this machine running the unit under systemd at all?
fn systemd_manages(unit: &str) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    // PID 1 being systemd is what separates a real server from a container that
    // merely has the binary installed.
    if !std::path::Path::new("/run/systemd/system").exists() {
        return false;
    }
    systemctl_show(unit, Some("LoadState"))
        .iter()
        .any(|line| line.starts_with("LoadState=") && !line.ends_with("not-found"))
}

fn systemctl_show(unit: &str, property: Option<&str>) -> Vec<String> {
    let mut cmd = Command::new("systemctl");
    cmd.arg("show").arg(unit);
    match property {
        Some(p) => {
            cmd.arg(format!("--property={p}"));
        }
        None => {
            for arg in show_args(unit).into_iter().skip(3) {
                cmd.arg(arg);
            }
        }
    }
    cmd.output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn systemd_state(lines: &[String]) -> State {
    let value = |key: &str| -> Option<String> {
        lines
            .iter()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .map(str::to_string)
    };

    if lines.is_empty() {
        return State::stopped("unknown", "systemd");
    }
    if value("LoadState").as_deref() == Some("not-found") {
        return State::stopped("not installed", "systemd");
    }

    let active = value("ActiveState").unwrap_or_else(|| "unknown".into());
    let running = active == "active";
    let pid = value("MainPID")
        .and_then(|p| p.parse::<u32>().ok())
        .filter(|p| *p > 0);
    // `BootUptime` is present only over ssh, where the far side measured it.
    let since_boot = value("BootUptime")
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(boot_uptime_seconds);
    let uptime = value("ExecMainStartTimestampMonotonic")
        .and_then(|us| us.parse::<f64>().ok())
        .filter(|us| *us > 0.0)
        .zip(since_boot)
        .map(|(started_us, now)| (now - started_us / 1_000_000.0) as i64);

    let detail = match (running, value("SubState")) {
        (true, Some(sub)) if sub != "running" => format!("{active} ({sub})"),
        (true, _) => "running".to_string(),
        (false, Some(sub)) if sub == "failed" => "failed".to_string(),
        (false, _) => active,
    };

    State {
        running,
        detail,
        pid,
        uptime,
        manager: "systemd",
    }
}

/// Seconds since boot, which is the clock systemd's monotonic stamps use.
fn boot_uptime_seconds() -> Option<f64> {
    std::fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn systemctl(unit: &str, verb: &str) -> Result<()> {
    let mut cmd = if is_root() {
        Command::new("systemctl")
    } else {
        // The unit is a system unit; without root, systemctl would only prompt
        // through polkit, which does not exist over ssh.
        let mut cmd = Command::new("sudo");
        cmd.arg("systemctl");
        cmd
    };
    cmd.arg(verb).arg(unit);
    run_inherited(cmd)
}

fn is_root() -> bool {
    #[cfg(unix)]
    {
        // Safe: geteuid is a read of the calling process's own identity.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

// ── a backend we started ourselves ──────────────────────────────────────

fn pid_file() -> PathBuf {
    wired_backend::paths::data_dir().join("wired-backend.pid")
}

/// `pid\nstarted-unix-seconds` — the second line is how `status` knows an uptime
/// without asking the kernel for a process start time on three platforms.
fn read_pid_file() -> Option<(u32, Option<i64>)> {
    let raw = std::fs::read_to_string(pid_file()).ok()?;
    let mut lines = raw.lines();
    let pid = lines.next()?.trim().parse().ok()?;
    let started = lines.next().and_then(|s| s.trim().parse().ok());
    Some((pid, started))
}

fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Signal 0 checks for existence and permission without delivering.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn process_state() -> State {
    match read_pid_file() {
        Some((pid, started)) if alive(pid) => State {
            running: true,
            detail: "running".into(),
            pid: Some(pid),
            uptime: started.map(|s| now_seconds() - s),
            manager: "process",
        },
        // A pid file left behind by a crash is not a running service.
        Some(_) => State::stopped("stopped", "process"),
        None => State::stopped("stopped", "process"),
    }
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// The backend binary, wherever this install put it.
pub fn backend_binary() -> Result<PathBuf> {
    let mut tried: Vec<String> = Vec::new();
    let name = if cfg!(windows) {
        "wired-backend.exe"
    } else {
        "wired-backend"
    };

    if let Some(explicit) = std::env::var_os("WIRED_BACKEND_BIN") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Ok(path);
        }
        tried.push(path.display().to_string());
    }

    // Installed next to us is the normal case: both binaries ship together.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let path = dir.join(name);
            if path.is_file() {
                return Ok(path);
            }
            tried.push(path.display().to_string());
            // …and a `cargo run` build has them one level up from `deps/`.
            if let Some(parent) = dir.parent() {
                let path = parent.join(name);
                if path.is_file() {
                    return Ok(path);
                }
            }
        }
    }

    for candidate in [
        PathBuf::from("/opt/wired-terminal/bin").join(name),
        PathBuf::from("crates/wired-backend/target/release").join(name),
        PathBuf::from("crates/wired-backend/target/debug").join(name),
    ] {
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate.display().to_string());
    }

    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    Err(format!(
        "could not find the {name} binary — set WIRED_BACKEND_BIN, or build it with\n  \
         cargo build --release --manifest-path crates/wired-backend/Cargo.toml\nlooked in: {}",
        tried.join(", ")
    ))
}

#[cfg(unix)]
fn process_start() -> Result<()> {
    use std::os::unix::process::CommandExt;

    if let Some((pid, _)) = read_pid_file() {
        if alive(pid) {
            return Ok(());
        }
    }

    let binary = backend_binary()?;
    let log_dir = wired_backend::paths::log_dir();
    wired_backend::paths::ensure_dir(&log_dir)
        .map_err(|e| format!("could not create {}: {e}", log_dir.display()))?;
    let out_path = log_dir.join("wired-backend.out");
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out_path)
        .map_err(|e| format!("could not open {}: {e}", out_path.display()))?;
    let err = out
        .try_clone()
        .map_err(|e| format!("could not open {}: {e}", out_path.display()))?;

    let mut cmd = Command::new(&binary);
    cmd.stdin(Stdio::null()).stdout(out).stderr(err);
    // A new session, so closing this terminal does not SIGHUP the agent —
    // "always on" has to survive the shell that started it.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", binary.display()))?;

    let path = pid_file();
    if let Some(parent) = path.parent() {
        let _ = wired_backend::paths::ensure_dir(parent);
    }
    std::fs::write(&path, format!("{}\n{}\n", child.id(), now_seconds()))
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

#[cfg(unix)]
fn process_stop() -> Result<()> {
    let Some((pid, _)) = read_pid_file() else {
        return Err("nothing to stop — no backend was started from here".into());
    };
    if !alive(pid) {
        let _ = std::fs::remove_file(pid_file());
        return Err("nothing to stop — the backend is not running".into());
    }

    // SIGTERM first: the backend's shutdown flushes the transcript and closes
    // the PTY, and SIGKILL would lose the tail of the conversation.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if !alive(pid) {
            let _ = std::fs::remove_file(pid_file());
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    let _ = std::fs::remove_file(pid_file());
    Ok(())
}

#[cfg(not(unix))]
fn process_start() -> Result<()> {
    Err(
        "starting the backend from the CLI is only supported on macOS and Linux — \
         use the desktop app on Windows"
            .into(),
    )
}

#[cfg(not(unix))]
fn process_stop() -> Result<()> {
    process_start()
}

/// Run the backend here, in this terminal, until it exits.
pub fn serve_foreground() -> Result<i32> {
    let binary = backend_binary()?;

    // Replace this process rather than supervising one. `wired serve` is a way
    // of spelling `wired-backend`, and a wrapper in between would swallow the
    // exit status and leave the server orphaned if anything signalled the
    // wrapper alone.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // exec only returns on failure.
        let e = Command::new(&binary).exec();
        Err(format!("could not run {}: {e}", binary.display()))
    }
    #[cfg(not(unix))]
    {
        let status = Command::new(&binary)
            .status()
            .map_err(|e| format!("could not run {}: {e}", binary.display()))?;
        Ok(status.code().unwrap_or(1))
    }
}

// ── logs without a journal ──────────────────────────────────────────────

fn newest_log_file() -> Option<PathBuf> {
    let today = wired_backend::diagnostics::log_file();
    if today.is_file() {
        return Some(today);
    }
    // Yesterday's file is the useful one when the service died overnight.
    let mut files: Vec<PathBuf> = std::fs::read_dir(wired_backend::paths::log_dir())
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("wired.log"))
        })
        .collect();
    files.sort();
    files.pop()
}

fn tail_log_file(follow: bool, lines: usize) -> Result<()> {
    let Some(path) = newest_log_file() else {
        return Err(format!(
            "no log file yet in {}",
            wired_backend::paths::log_dir().display()
        ));
    };

    let body = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let start = body.lines().count().saturating_sub(lines);
    for line in body.lines().skip(start) {
        println!("{line}");
    }
    if !follow {
        return Ok(());
    }

    // Poll rather than watch: one file, a human waiting, and no new dependency.
    let mut offset = body.len() as u64;
    loop {
        std::thread::sleep(Duration::from_millis(400));
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        // A rotated file starts over; follow it from the top rather than seeking
        // past the end of a shorter file.
        if meta.len() < offset {
            offset = 0;
        }
        if meta.len() == offset {
            continue;
        }
        if let Ok(fresh) = std::fs::read_to_string(&path) {
            if let Some(new) = fresh.get(offset as usize..) {
                print!("{new}");
            }
            offset = fresh.len() as u64;
        }
    }
}

// ── running other people's programs ─────────────────────────────────────

fn run_inherited(mut cmd: Command) -> Result<()> {
    let name = cmd.get_program().to_string_lossy().to_string();
    let status = cmd.status().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("{name} is not installed"),
        _ => format!("could not run {name}: {e}"),
    })?;
    // Ctrl-C out of `journalctl -f` is a normal exit, not a failure.
    if status.success() || status.code().is_none() {
        Ok(())
    } else {
        Err(format!("{name} exited with {status}"))
    }
}

fn ssh_command(host: &str, ssh_port: Option<u16>, tty: bool) -> Command {
    let mut cmd = Command::new("ssh");
    if tty {
        // sudo and journalctl -f both want a terminal.
        cmd.arg("-t");
    }
    if let Some(port) = ssh_port {
        cmd.arg("-p").arg(port.to_string());
    }
    cmd.arg(host);
    cmd
}

fn ssh_run(host: &str, ssh_port: Option<u16>, argv: &[&str], tty: bool) -> Result<()> {
    let mut cmd = ssh_command(host, ssh_port, tty);
    cmd.arg("--").args(argv);
    run_inherited(cmd)
}

fn ssh_capture(host: &str, ssh_port: Option<u16>, argv: &[&str]) -> Result<String> {
    let mut cmd = ssh_command(host, ssh_port, false);
    let output = cmd
        .arg("--")
        .args(argv)
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "ssh is not installed — remotes need it".to_string(),
            _ => format!("could not run ssh: {e}"),
        })?;
    if !output.status.success() {
        return Err(format!("ssh to {host} failed"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(raw: &str) -> Vec<String> {
        raw.lines().map(str::to_string).collect()
    }

    #[test]
    fn an_active_unit_reads_as_running() {
        let state = systemd_state(&lines(
            "LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=4417\n\
             ExecMainStartTimestampMonotonic=0\n",
        ));
        assert!(state.running);
        assert_eq!(state.detail, "running");
        assert_eq!(state.pid, Some(4417));
    }

    #[test]
    fn a_missing_unit_says_so_rather_than_stopped() {
        let state = systemd_state(&lines("LoadState=not-found\nActiveState=inactive\n"));
        assert!(!state.running);
        assert_eq!(state.detail, "not installed");
    }

    #[test]
    fn a_crashed_unit_reads_as_failed() {
        let state = systemd_state(&lines(
            "LoadState=loaded\nActiveState=failed\nSubState=failed\nMainPID=0\n",
        ));
        assert!(!state.running);
        assert_eq!(state.detail, "failed");
        assert_eq!(state.pid, None);
    }

    #[test]
    fn no_output_from_systemctl_is_unknown_not_stopped() {
        let state = systemd_state(&[]);
        assert_eq!(state.detail, "unknown");
    }

    #[test]
    fn a_remote_uptime_is_measured_with_the_far_sides_clock() {
        // 200_000s since that machine booted, service started at 20_000s.
        let state = systemd_state(&lines(
            "LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=292916\n\
             ExecMainStartTimestampMonotonic=20000000000\nBootUptime=200000.42\n",
        ));
        assert_eq!(state.uptime, Some(180_000));
    }

    #[test]
    fn the_remote_command_asks_for_the_boot_clock_too() {
        let command = remote_state_command("wired-terminal");
        assert!(command.contains("'wired-terminal'"), "{command}");
        assert!(command.contains("--property=ActiveState"), "{command}");
        assert!(command.contains("/proc/uptime"), "{command}");
    }

    #[test]
    fn a_unit_name_cannot_break_out_of_the_remote_command() {
        let command = remote_state_command("evil'; rm -rf /; echo '");
        assert!(!command.contains("; rm -rf /; echo ';"), "{command}");
        assert!(command.contains(r"'\''"), "{command}");
    }

    #[test]
    fn a_starting_unit_keeps_its_substate() {
        let state = systemd_state(&lines(
            "LoadState=loaded\nActiveState=active\nSubState=start-pre\nMainPID=0\n",
        ));
        assert_eq!(state.detail, "active (start-pre)");
    }
}
