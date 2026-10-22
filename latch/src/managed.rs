//! The managed host: `latch serve` starts the host on a state folder as a
//! service of this user's, a launchd agent on macOS and a systemd user
//! service on Linux, running `latch serve --foreground`. On macOS that is
//! the `latch` inside the app bundle (the one this command resolves to), so
//! the host's permissions, Full Disk Access among them, are the app's and
//! not the terminal's. Its log goes to `serve.log`.
//!
//! - `latch serve` holds the host's life over its local socket (see
//!   `serve/control.rs`) and shows its dashboard: Ctrl-C, or the terminal
//!   going away, stops the host.
//! - `latch serve -d` returns once the host answers on its socket.
//!   `serve -d --again` starts it with what the last start was given,
//!   kept in `serve-args.json`.
//! - `latch stop` asks the host to stop, then removes the service, so
//!   nothing starts it again; `dashboard` only watches.
//!
//! The service manager never restarts a host: a host that ended stays
//! ended until the next `latch serve`. A start or stop holds `launch.lock`,
//! so two never interleave; that a host runs is still `serve.lock`'s, which
//! every host holds however it was started.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::UnixStream;

use crate::cli::ServeArgs;
use crate::dashboard::{self, Board};
use crate::failure::{Exit, Failure};
use crate::lifecycle::{self, Phase};
use crate::state::{StateDir, to_hex};
use crate::term;
use crate::transport;
use crate::wire::{self, Request, Response};

/// How long a starting host has to answer on its socket: past the relay's
/// 15 seconds and the sandbox probe.
const READY_WITHIN: Duration = Duration::from_secs(60);
/// How long a stopping host has to let go of its state folder: its own
/// shutdown is bounded well inside this.
const STOP_WITHIN: Duration = Duration::from_secs(40);
/// How long a host that is up has to answer one question on its socket.
const ANSWER_WITHIN: Duration = Duration::from_secs(5);
/// The flags the last start gave the host, for `serve -d --again`.
pub(crate) const ARGS_FILE: &str = "serve-args.json";

#[cfg(test)]
mod saved_flags_tests {
    use super::*;

    #[test]
    fn invalid_saved_flags_require_one_explicit_start() {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        std::fs::create_dir_all(&parent).unwrap();
        let dir = tempfile::tempdir_in(parent).unwrap();
        let flags = vec!["--headless-browsers".to_string(), "--deny-exec".to_string()];
        save_given(dir.path(), &flags).unwrap();
        assert_eq!(given_before(dir.path()).unwrap(), flags);
        for flags in [
            vec!["/old/folder"],
            vec!["--allow-read=/old/refs"],
            vec!["--unknown"],
        ] {
            let flags: Vec<_> = flags.into_iter().map(str::to_string).collect();
            save_given(dir.path(), &flags).unwrap();
            let error = given_before(dir.path()).unwrap_err();
            assert!(error.message.contains("latch serve -d"), "{error:?}");
            assert!(error.message.contains("--deny-exec"), "{error:?}");
            assert!(error.message.contains("--headless-browsers"), "{error:?}");
            assert_eq!(
                std::fs::read_to_string(dir.path().join(ARGS_FILE)).unwrap(),
                json!(flags).to_string()
            );
        }
    }
}

/// `latch serve -d`: the host, started and answering.
pub async fn start(state: &StateDir, args: &ServeArgs) -> Result<Value, Failure> {
    let (dir, hello) = launch(state, args, false).await?;
    Ok(
        json!({"state_dir": dir, "pid": hello["pid"], "run": hello["run"],
              "version": hello["version"], "log": dir.join("serve.log")}),
    )
}

/// `latch serve`: starts the host, holds it and shows its dashboard until
/// Ctrl-C or the terminal goes away, then stops it.
pub async fn attach(state: &StateDir, args: &ServeArgs) -> Result<Value, Failure> {
    if !term::stdout_is_terminal() || !term::interactive() {
        let message = "`latch serve` shows the host's dashboard, so it needs a terminal; \
                       `latch serve -d` starts the host detached"
            .to_string();
        return Err(Failure::usage("no_terminal", message));
    }
    let (dir, hello) = launch(state, args, true).await?;
    // Only this host is ours to stop: by the time this serve cleans up,
    // another may serve the folder.
    let ours = hello["pid"].as_u64().map(|pid| pid as u32);
    let boards = match watch(&dir, "host_attach").await {
        Ok((_, boards)) => boards,
        Err(failure) => {
            stop_dir(&dir, ours).await?;
            return Err(failure);
        }
    };
    show(&hello, boards, "Ctrl-C stops the host").await;
    // The view is closed, so the connection that held the host is too:
    // the host is stopping. Wait for it, and remove its service.
    let _ = writeln!(std::io::stderr(), "latch: stopping the host…");
    stop_dir(&dir, ours).await
}

/// `latch dashboard`: watches the running host until Ctrl-C.
pub async fn dashboard(state: &StateDir) -> Result<(), Failure> {
    if !term::stdout_is_terminal() {
        let message = "the dashboard needs a terminal; `latch status --local` and \
                       `latch events` say what the host is doing as JSON"
            .to_string();
        return Err(Failure::usage("no_terminal", message));
    }
    let (hello, boards) = watch(&state.path, "host_watch").await.map_err(|failure| {
        match other_version(&state.path) {
            Some(pid) => other_version_running(pid),
            None => not_serving(failure),
        }
    })?;
    let footer = "Ctrl-C closes this view; the host keeps running (`latch stop` stops it)";
    show(&hello, boards, footer).await;
    Ok(())
}

/// `latch stop`.
pub async fn stop(state: &StateDir) -> Result<Value, Failure> {
    let dir = state.path.canonicalize().unwrap_or(state.path.clone());
    stop_dir(&dir, None).await
}

/// Starts the host on `state` as a service, `attached` or not, and waits
/// until it answers. Refused while a host runs there. Returns the state
/// folder's real path and the host's hello.
async fn launch(
    state: &StateDir,
    args: &ServeArgs,
    attached: bool,
) -> Result<(PathBuf, Value), Failure> {
    state.require_login()?;
    let dir = state
        .path
        .canonicalize()
        .map_err(|err| io_failure(&state.path, err))?;
    // Behind at most another start, or a stop.
    let within = Instant::now() + READY_WITHIN + STOP_WITHIN;
    let Some(_launching) = launch_lock(&dir, within).await? else {
        let message =
            "another `latch serve` or `latch stop` on this state folder did not finish".to_string();
        return Err(Failure::failed("launch_busy", message));
    };
    if lifecycle::holds_lock(&dir) {
        return Err(match other_version(&dir) {
            Some(pid) => other_version_running(pid),
            None => already_serving(&dir).await,
        });
    }
    let given = if args.again {
        given_before(&dir)?
    } else {
        let given = given_now(args);
        save_given(&dir, &given)?;
        given
    };
    let program = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(|err| Failure::failed("no_binary", format!("latch cannot find itself: {err}")))?;
    let mut argv: Vec<OsString> = vec![
        program.into(),
        "--state-dir".into(),
        dir.clone().into(),
        "serve".into(),
        "--foreground".into(),
    ];
    if attached {
        argv.push("--attached".into());
    }
    argv.extend(given.into_iter().map(OsString::from));

    let service = Service::new(&dir);
    // A host that ended on its own leaves its service registered.
    service.remove()?;
    let before = lifecycle::now(&dir).run;
    service.start(&argv)?;
    match ready(&dir, before.as_deref()).await {
        Ok(hello) => Ok((dir, hello)),
        Err(failure) => {
            service.remove()?;
            Err(failure)
        }
    }
}

/// What `args` give the host after `serve --foreground`.
fn given_now(args: &ServeArgs) -> Vec<String> {
    let mut given = Vec::new();
    if args.headless_browsers {
        given.push("--headless-browsers".to_string());
    }
    for agent in &args.deny_exec {
        given.push(match agent.as_str() {
            crate::cli::EVERY_AGENT => "--deny-exec".to_string(),
            agent => format!("--deny-exec={agent}"),
        });
    }
    given
}

/// Keeps `given` for a later `serve -d --again`.
fn save_given(dir: &Path, given: &[String]) -> Result<(), Failure> {
    let path = dir.join(ARGS_FILE);
    crate::state::write_private(&path, json!(given).to_string().as_bytes())
        .map_err(|err| io_failure(&path, err))
}

/// What the last start gave the host, for `serve -d --again`.
fn given_before(dir: &Path) -> Result<Vec<String>, Failure> {
    let path = dir.join(ARGS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let message = "no `latch serve` has run on this state folder to start again; \
                           `latch serve -d` starts one"
                .to_string();
            return Err(Failure::usage("nothing_to_start_again", message));
        }
        Err(err) => return Err(io_failure(&path, err)),
    };
    let invalid = || {
        Failure::usage(
            "bad_serve_args",
            "saved host flags are not supported; start once with `latch serve -d` plus any \
         `--deny-exec` or `--headless-browsers` flags you need"
                .to_string(),
        )
    };
    let given: Vec<String> = serde_json::from_str(&text).map_err(|_| invalid())?;
    crate::cli::Cli::try_parse_from(
        ["latch", "serve", "--foreground"]
            .into_iter()
            .chain(given.iter().map(String::as_str)),
    )
    .map_err(|_| invalid())?;
    Ok(given)
}

/// Waits until the host just started answers on its socket, or says why
/// it stopped; returns its hello.
async fn ready(dir: &Path, before: Option<&str>) -> Result<Value, Failure> {
    let started = Instant::now();
    loop {
        if let Ok(hello) = ask(dir, "host_hello", json!({})).await {
            return Ok(hello);
        }
        let host = lifecycle::now(dir);
        let ended = !host.running && matches!(host.state, Phase::Error | Phase::Stopped);
        if ended && host.run.is_some() && host.run.as_deref() != before {
            let why = host
                .error
                .map_or("it stopped".to_string(), |error| error.message);
            let message = format!("the host did not start: {why} (`latch logs` has its log)");
            return Err(Failure::failed("serve_failed", message));
        }
        if started.elapsed() > READY_WITHIN {
            let message = format!(
                "the host did not answer within {} seconds (`latch logs` has its log)",
                READY_WITHIN.as_secs()
            );
            return Err(Failure::failed("serve_failed", message));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Stops the host on `dir`, however it was started, and removes its
/// service, so nothing starts it again. With `only`, only the host with
/// that pid: one that took the folder over since is left alone.
async fn stop_dir(dir: &Path, only: Option<u32>) -> Result<Value, Failure> {
    let started = Instant::now();
    let Some(_launching) = launch_lock(dir, started + STOP_WITHIN).await? else {
        let message = format!(
            "another `latch serve` or `latch stop` on this state folder did not finish within {} seconds",
            STOP_WITHIN.as_secs()
        );
        return Err(Failure::failed("stop_failed", message));
    };
    let service = Service::new(dir);
    let host = lifecycle::now(dir);
    if host.running && only.is_some() && host.pid != only {
        return Ok(json!({"stopped": false, "state_dir": dir, "state": host.state}));
    }
    let was = host.running;
    // The host holding the folder now, by pid: a later one is not stopped.
    let mut target = host.pid;
    let (mut asked, mut removed) = (false, false);
    loop {
        let host = lifecycle::now(dir);
        if !host.running {
            break;
        }
        match (target, host.pid) {
            (Some(target), Some(pid)) if target != pid => break,
            (None, pid) => target = pid,
            _ => {}
        }
        // A host that answers stops itself. One that does not yet (it is
        // starting) is stopped by its service manager if it has one, and
        // asked again until it answers.
        if !asked && let Some(pid) = target {
            asked = ask(dir, "host_stop", json!({"pid": pid})).await.is_ok();
        }
        // A host of another latch never answers on this one's socket: the
        // pid its lock and serve.json both name is told to stop instead.
        if !asked
            && let Some(pid) = target
            && other_version(dir) == Some(pid)
        {
            // SAFETY: kill only sends a signal.
            asked = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } == 0;
        }
        if !asked && !removed {
            service.remove()?;
            removed = true;
        }
        if started.elapsed() > STOP_WITHIN {
            if let Some(pid) = other_version(dir) {
                return Err(other_version_running(pid));
            }
            let pid = target.map_or("?".to_string(), |pid| pid.to_string());
            let message = format!(
                "the host (pid {pid}) did not stop within {} seconds",
                STOP_WITHIN.as_secs()
            );
            return Err(Failure::failed("stop_failed", message));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    service.remove()?;
    let host = lifecycle::now(dir);
    Ok(json!({"stopped": was, "state_dir": dir, "state": host.state, "error": host.error}))
}

/// The refusal of a second host, saying how to see or stop the one there,
/// and when it runs another latch than this one.
async fn already_serving(dir: &Path) -> Failure {
    let mut message = format!(
        "{} is already served by a host; `latch dashboard` shows it, `latch stop` stops it",
        dir.display()
    );
    if let Ok(hello) = ask(dir, "host_hello", json!({})).await {
        message = format!("{message} (pid {})", hello["pid"]);
        message.push_str(&mismatch(&hello));
    }
    Failure::failed("already_serving", message)
}

/// The pid of a host from another latch version serving `dir`: it holds
/// the folder and says it serves, but not on this version's socket.
fn other_version(dir: &Path) -> Option<u32> {
    let host = lifecycle::now(dir);
    let serving = host.running && host.state == Phase::Serving;
    let theirs = serving && !transport::local_socket_path(dir).exists();
    host.pid.filter(|_| theirs)
}

fn other_version_running(pid: u32) -> Failure {
    let message = format!(
        "a host from another latch version is running (pid {pid}); stop it \
         (Ctrl-C in its terminal, or kill {pid}), then retry"
    );
    Failure::failed("other_version", message)
}

/// What to say when the host runs another latch than this one.
fn mismatch(hello: &Value) -> String {
    let theirs = hello["version"].as_str().unwrap_or("unknown");
    if theirs == crate::cli::version() {
        return String::new();
    }
    format!(
        "; it runs latch {theirs}, this is {}",
        crate::cli::version()
    )
}

/// Draws the host's board until the host stops or Ctrl-C (or the terminal
/// goes away).
async fn show(hello: &Value, boards: tokio::sync::mpsc::Receiver<Board>, footer: &str) {
    use tokio::signal::unix::{SignalKind, signal};
    let footer = format!("{footer}{}", mismatch(hello));
    let mut hangup = signal(SignalKind::hangup()).expect("a hangup handler");
    let mut terminate = signal(SignalKind::terminate()).expect("a terminate handler");
    let until = async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = hangup.recv() => {}
            _ = terminate.recv() => {}
        }
    };
    dashboard::show(boards, until, &footer).await;
}

/// Asks the host on `dir` for `op`'s board: its hello, and each board as
/// it comes, until the connection ends.
async fn watch(
    dir: &Path,
    op: &str,
) -> Result<(Value, tokio::sync::mpsc::Receiver<Board>), Failure> {
    let (hello, mut socket) = request(dir, op, json!({})).await?;
    let (send, boards) = tokio::sync::mpsc::channel(4);
    tokio::spawn(async move {
        // This task holds the connection: it closes it once the view
        // stops listening, and the host takes that as the watcher gone.
        loop {
            let frame = tokio::select! {
                frame = wire::read_frame(&mut socket) => frame,
                () = send.closed() => return,
            };
            let Some(board) = frame
                .ok()
                .and_then(|frame| serde_json::from_slice(&frame).ok())
            else {
                return;
            };
            if send.send(board).await.is_err() {
                return;
            }
        }
    });
    Ok((hello, boards))
}

/// One question for the host on `dir` and its answer.
async fn ask(dir: &Path, op: &str, args: Value) -> Result<Value, Failure> {
    request(dir, op, args).await.map(|(answer, _)| answer)
}

/// Sends `op` to the host on `dir`; returns its first answer, within
/// `ANSWER_WITHIN`, and the connection, for what the host sends next.
async fn request(dir: &Path, op: &str, args: Value) -> Result<(Value, UnixStream), Failure> {
    tokio::time::timeout(ANSWER_WITHIN, request_now(dir, op, args))
        .await
        .unwrap_or_else(|_| {
            let message = format!(
                "the host did not answer within {} seconds",
                ANSWER_WITHIN.as_secs()
            );
            Err(Failure::new(Exit::Unreachable, "serve_required", message))
        })
}

async fn request_now(dir: &Path, op: &str, args: Value) -> Result<(Value, UnixStream), Failure> {
    let mut socket = transport::connect_local(dir).await?;
    let request = Request {
        op: op.to_string(),
        args,
        chat: None,
    };
    let lost = |err: std::io::Error| {
        Failure::new(
            Exit::Unreachable,
            "serve_required",
            format!("the host did not answer: {err}"),
        )
    };
    let json = serde_json::to_vec(&request).expect("a request serializes");
    wire::write_frame(&mut socket, &json).await.map_err(lost)?;
    let frame = wire::read_frame(&mut socket).await.map_err(lost)?;
    let response: Response = serde_json::from_slice(&frame)
        .map_err(|_| Failure::failed("bad_response", "the host's answer is not valid".into()))?;
    Ok((response.into_result()?, socket))
}

fn not_serving(failure: Failure) -> Failure {
    if failure.code != "serve_required" {
        return failure;
    }
    let message = "no host serves this state folder; `latch serve` starts one".to_string();
    Failure::new(Exit::Unreachable, "serve_required", message)
}

/// Held while a host is started or stopped on `dir`.
/// `None` once `until` passes with another start or stop still holding it.
async fn launch_lock(dir: &Path, until: Instant) -> Result<Option<File>, Failure> {
    let path = dir.join("launch.lock");
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|err| io_failure(&path, err))?;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < until => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(err)) => return Err(io_failure(&path, err)),
        }
    }
}

fn io_failure(path: &Path, err: std::io::Error) -> Failure {
    Failure::failed("state_io", format!("{}: {err}", path.display()))
}

/// The service that runs the host on one state folder, named after the
/// folder's real path, so each folder has its own.
pub struct Service {
    label: String,
    dir: PathBuf,
}

impl Service {
    pub fn new(dir: &Path) -> Service {
        let hash = to_hex(&Sha256::digest(dir.as_os_str().as_encoded_bytes()));
        Service {
            label: format!("co.plow.latch.serve.{}", &hash[..16]),
            dir: dir.to_path_buf(),
        }
    }

    /// Whether the service manager has this service.
    pub fn registered(&self) -> bool {
        platform::registered(&self.label)
    }

    /// Runs `argv` as this service, with this process's environment, its
    /// stdout and stderr appended to `serve.log`.
    fn start(&self, argv: &[OsString]) -> Result<(), Failure> {
        platform::start(&self.label, &self.dir, argv)
    }

    /// Stops the service's process, if it runs, and forgets the service.
    fn remove(&self) -> Result<(), Failure> {
        if !self.registered() {
            return Ok(());
        }
        platform::remove(&self.label)
    }
}

/// Runs a service manager command; its own words on failure.
fn manager(command: &mut Command) -> Result<(), Failure> {
    let output = command
        .output()
        .map_err(|err| Failure::failed("service_failed", format!("{command:?}: {err}")))?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let message = format!("{command:?} failed: {said}");
    Err(Failure::failed("service_failed", message))
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::OsString;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;
    use std::process::Command;

    use super::{STOP_WITHIN, manager};
    use crate::failure::Failure;

    /// The logged-in user's session, where an agent may show windows.
    fn domain() -> String {
        // SAFETY: getuid has no arguments and cannot fail.
        format!("gui/{}", unsafe { libc::getuid() })
    }

    pub fn registered(label: &str) -> bool {
        Command::new("launchctl")
            .args(["print", &format!("{}/{label}", domain())])
            .output()
            .is_ok_and(|output| output.status.success())
    }

    /// The job is in the state folder, not `~/Library/LaunchAgents`: it is
    /// loaded for this start only, and never at login.
    pub fn start(label: &str, dir: &Path, argv: &[OsString]) -> Result<(), Failure> {
        let path = dir.join("serve.plist");
        let plist = job(label, dir, argv)?;
        let io =
            |err: std::io::Error| Failure::failed("state_io", format!("{}: {err}", path.display()));
        // Mode 600: it holds the environment `latch serve` ran in.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(io)?;
        std::io::Write::write_all(&mut file, plist.as_bytes()).map_err(io)?;
        manager(
            Command::new("launchctl")
                .arg("bootstrap")
                .arg(domain())
                .arg(&path),
        )
    }

    pub fn remove(label: &str) -> Result<(), Failure> {
        manager(Command::new("launchctl").args(["bootout", &format!("{}/{label}", domain())]))
    }

    /// The launchd job: run once, never kept alive, and given time to
    /// stop on its own after SIGTERM.
    fn job(label: &str, dir: &Path, argv: &[OsString]) -> Result<String, Failure> {
        let text = |value: &std::ffi::OsStr| {
            value.to_str().map(escape).ok_or_else(|| {
                let message = format!("{} is not valid UTF-8", value.display());
                Failure::failed("service_failed", message)
            })
        };
        let mut arguments = String::new();
        for arg in argv {
            arguments.push_str(&format!("<string>{}</string>", text(arg)?));
        }
        let mut environment = String::new();
        for (name, value) in std::env::vars_os() {
            // The plist is text: a variable that is not is left out.
            if let (Ok(name), Ok(value)) = (text(&name), text(&value)) {
                environment.push_str(&format!("<key>{name}</key><string>{value}</string>"));
            }
        }
        let log = text(dir.join("serve.log").as_os_str())?;
        Ok(format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array>{arguments}</array>
<key>EnvironmentVariables</key><dict>{environment}</dict>
<key>StandardOutPath</key><string>{log}</string>
<key>StandardErrorPath</key><string>{log}</string>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><false/>
<key>ProcessType</key><string>Interactive</string>
<key>ExitTimeOut</key><integer>{}</integer>
</dict></plist>
"#,
            STOP_WITHIN.as_secs()
        ))
    }

    fn escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::ffi::OsString;
    use std::path::Path;
    use std::process::Command;

    use super::{STOP_WITHIN, manager};
    use crate::failure::Failure;

    fn unit(label: &str) -> String {
        format!("{label}.service")
    }

    pub fn registered(label: &str) -> bool {
        Command::new("systemctl")
            .args([
                "--user",
                "show",
                "--property=LoadState",
                "--value",
                &unit(label),
            ])
            .output()
            .is_ok_and(|output| output.stdout.starts_with(b"loaded"))
    }

    /// A transient unit: there is no unit file, and once its process ends
    /// systemd forgets it (`--collect`). SIGTERM goes to the host alone,
    /// which stops its own workers (`KillMode=mixed`).
    pub fn start(label: &str, dir: &Path, argv: &[OsString]) -> Result<(), Failure> {
        let log = dir.join("serve.log");
        let mut append = OsString::from("append:");
        append.push(&log);
        let mut command = Command::new("systemd-run");
        command
            .args(["--user", "--collect", "--quiet", "--unit", &unit(label)])
            .args(["--property=KillMode=mixed", "--property=Type=exec"])
            .arg(format!(
                "--property=TimeoutStopSec={}",
                STOP_WITHIN.as_secs()
            ))
            .arg({
                let mut property = OsString::from("--property=StandardOutput=");
                property.push(&append);
                property
            })
            .arg({
                let mut property = OsString::from("--property=StandardError=");
                property.push(&append);
                property
            });
        // Each variable by name only: systemd-run copies its value from its
        // own environment, so no value shows on a command line.
        for (name, _) in std::env::vars_os() {
            let mut setenv = OsString::from("--setenv=");
            setenv.push(name);
            command.arg(setenv);
        }
        manager(command.arg("--").args(argv))
    }

    pub fn remove(label: &str) -> Result<(), Failure> {
        manager(Command::new("systemctl").args(["--user", "stop", &unit(label)]))
    }
}
