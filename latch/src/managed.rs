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
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
use std::time::{Duration, Instant};

use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::cli::ServeArgs;
use crate::dashboard::{self, Board};
use crate::failure::{Failure, code};
use crate::lifecycle::{self, Phase};
use crate::serve::DETACH;
use crate::state::{OwnerOnly, StateDir, to_hex};
use crate::term;
use crate::transport::{self, LocalStream, LocalWrite};
use crate::wire::{self, Request, Response};

/// How long a starting host has to answer on its socket: past the relay's
/// first 15 seconds and the sandbox probe. One still waiting then for a
/// relay, or for calls with its key, starts by itself once it can.
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
    #[test]
    fn a_log_past_its_cap_is_kept_as_the_one_before_and_a_start_begins_anew() {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        std::fs::create_dir_all(&parent).unwrap();
        let dir = tempfile::tempdir_in(parent).unwrap();
        let (log, before) = (dir.path().join("serve.log"), dir.path().join("serve.log.1"));
        std::fs::write(&log, vec![b'x'; LOG_CAP as usize]).unwrap();
        rotate_log(dir.path()).unwrap();
        assert!(log.exists() && !before.exists(), "at its cap, it is kept");
        std::fs::write(&before, "older").unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&log)
            .and_then(|mut file| file.write_all(b"one more line\n"))
            .unwrap();
        rotate_log(dir.path()).unwrap();
        assert!(!log.exists());
        assert_eq!(std::fs::metadata(&before).unwrap().len(), LOG_CAP + 14);
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
/// How an attached `latch serve` ended.
pub enum Attached {
    /// Its host stopped, as `stop` says it.
    Stopped(Value),
    /// The viewer left the host running (`q`): what to say, in a line.
    Detached(String),
}

pub async fn attach(state: &StateDir, args: &ServeArgs) -> Result<Attached, Failure> {
    if !term::stdout_is_terminal() || !term::interactive() {
        let message =
            "`latch serve` shows the host's dashboard, so it needs a terminal".to_string();
        let mut failure = Failure::new(code::no_terminal, message);
        failure.next = Some("latch serve -d".into());
        return Err(failure);
    }
    let (dir, hello) = launch(state, args, true).await?;
    // Only this host is ours to stop: by the time this serve cleans up,
    // another may serve the folder.
    let ours = hello["pid"].as_u64().map(|pid| pid as u32);
    let (boards, mut holding) = match watch(&dir, "host_attach").await {
        Ok((_, boards, holding)) => (boards, holding),
        Err(failure) => {
            stop_dir(&dir, ours).await?;
            return Err(failure);
        }
    };
    if show(&hello, boards, true).await == dashboard::Ending::Detached {
        // `q`: the host lets go of this serve and runs on, as if started
        // with -d.
        let _ = holding.write_all(&DETACH).await;
        let _ = holding.shutdown().await;
        return Ok(Attached::Detached(format!(
            "The host runs on (pid {}). Stop it with: latch --state-dir {} stop",
            hello["pid"],
            dir.display()
        )));
    }
    // The view is closed, so the connection that held the host is too:
    // the host is stopping. Wait for it, and remove its service.
    drop(holding);
    let _ = writeln!(std::io::stderr(), "latch: stopping the host…");
    stop_dir(&dir, ours).await.map(Attached::Stopped)
}

/// `latch dashboard`: watches the running host until Ctrl-C.
pub async fn dashboard(state: &StateDir) -> Result<(), Failure> {
    if !term::stdout_is_terminal() {
        let message = "the dashboard needs a terminal".to_string();
        let mut failure = Failure::new(code::no_terminal, message);
        failure.next = Some("latch audit --follow".into());
        return Err(failure);
    }
    // The write side stays open while the view does: the host takes its
    // end as the viewer gone.
    let (hello, boards, _watching) =
        watch(&state.path, "host_watch").await.map_err(
            |failure| match lifecycle::other_version(&state.path) {
                Some(pid) => lifecycle::other_version_running(pid),
                None => not_serving(failure),
            },
        )?;
    show(&hello, boards, false).await;
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
        return Err(Failure::new(code::launch_busy, message));
    };
    let host = lifecycle::now(&dir);
    if host.configuring {
        let message = "a `latch plugins` or `latch logout` command is changing this state \
                       folder; start latch once it finishes"
            .to_string();
        return Err(Failure::new(code::state_busy, message));
    }
    if host.running {
        return Err(match lifecycle::other_version(&dir) {
            Some(pid) => lifecycle::other_version_running(pid),
            None => already_serving(&dir).await,
        });
    }
    // The host saves them again once it holds the folder.
    let given = if args.again {
        given_before(&dir)?
    } else {
        given_now(args)
    };
    let program = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(|err| Failure::new(code::no_binary, format!("latch cannot find itself: {err}")))?;
    let mut argv: Vec<OsString> = vec![
        program.into(),
        "--state-dir".into(),
        dir.clone().into(),
        "serve".into(),
        "--foreground".into(),
    ];
    argv.push(if attached { "--attached" } else { "--managed" }.into());
    argv.extend(given.into_iter().map(OsString::from));

    rotate_log(&dir)?;
    let service = Service::new(&dir);
    // A host that ended on its own leaves its service registered.
    service.remove()?;
    let before = lifecycle::now(&dir).run;
    service.start(&argv)?;
    let failure = match ready(&dir, before.as_deref()).await {
        Ok(hello) => return Ok((dir, hello)),
        Err(NotReady::Failed(failure)) => failure,
        // A detached host goes on to start by itself once those calls end;
        // removing its service would stop it. An attached one would wait
        // for a holder that has given up.
        Err(NotReady::Waiting(why)) if !attached => {
            return Err(not_ready(&format!(
                ": it is {why}, and starts by itself once that ends"
            )));
        }
        Err(NotReady::Waiting(why)) => not_ready(&format!(
            ": it was {why}; run `latch serve` again once they end"
        )),
    };
    service.remove()?;
    Err(failure)
}

/// How big `serve.log` may grow before the next start begins a new one.
const LOG_CAP: u64 = 10 * 1024 * 1024;

/// Before a start, with no host writing it: a `serve.log` over `LOG_CAP`
/// becomes `serve.log.1`, replacing the one before, and the host starts a
/// new one.
fn rotate_log(dir: &Path) -> Result<(), Failure> {
    let log = dir.join("serve.log");
    match std::fs::metadata(&log) {
        Ok(meta) if meta.len() > LOG_CAP => {
            std::fs::rename(&log, dir.join("serve.log.1")).map_err(|err| io_failure(&log, err))
        }
        _ => Ok(()),
    }
}

/// What `args` give the host after `serve --foreground`.
pub(crate) fn given_now(args: &ServeArgs) -> Vec<String> {
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
pub(crate) fn save_given(dir: &Path, given: &[String]) -> Result<(), Failure> {
    let path = dir.join(ARGS_FILE);
    crate::state::write_private(&path, json!(given).to_string().as_bytes())
        .map_err(|err| io_failure(&path, err))
}

/// What the last start gave the host, for `serve -d --again`.
fn given_before(dir: &Path) -> Result<Vec<String>, Failure> {
    match saved(dir)? {
        Some((given, _)) => Ok(given),
        None => {
            let message = "no `latch serve` has run on this state folder to start again; \
                           `latch serve -d` starts one"
                .to_string();
            Err(Failure::new(code::nothing_to_start_again, message))
        }
    }
}

/// The `--deny-exec` the host on `dir` runs with, or last ran with (none
/// before any host ran here); `None` when its saved flags cannot be read,
/// which `serve -d --again` reports.
pub fn saved_deny_exec(dir: &Path) -> Option<crate::serve::DenyExec> {
    let saved = saved(dir).ok()?;
    Some(
        saved
            .map(|(_, args)| crate::serve::DenyExec::from_flags(&args.deny_exec))
            .unwrap_or_default(),
    )
}

/// The flags the last host on `dir` saved, as given and as parsed.
fn saved(dir: &Path) -> Result<Option<(Vec<String>, ServeArgs)>, Failure> {
    let path = dir.join(ARGS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io_failure(&path, err)),
    };
    let invalid = || {
        Failure::new(
            code::bad_serve_args,
            "saved host flags are not supported; start once with `latch serve -d` plus any \
         `--deny-exec` or `--headless-browsers` flags you need"
                .to_string(),
        )
    };
    let given: Vec<String> = serde_json::from_str(&text).map_err(|_| invalid())?;
    let parsed = crate::cli::Cli::try_parse_from(
        ["latch", "serve", "--foreground"]
            .into_iter()
            .chain(given.iter().map(String::as_str)),
    )
    .map_err(|_| invalid())?;
    match parsed.command {
        Some(crate::cli::Command::Serve(args)) => Ok(Some((given, args))),
        _ => Err(invalid()),
    }
}

/// Waits until the host just started answers on its socket, or says why
/// it stopped; returns its hello.
/// Why a started host did not answer in time.
enum NotReady {
    Failed(Failure),
    /// It is still starting, waiting for this: it answers once that ends.
    Waiting(String),
}

/// The host did not answer within `READY_WITHIN`, for the reason `why`
/// adds.
fn not_ready(why: &str) -> Failure {
    let message = format!(
        "the host did not answer within {} seconds{why} (`latch logs` has its log)",
        READY_WITHIN.as_secs()
    );
    Failure::new(code::serve_failed, message)
}

async fn ready(dir: &Path, before: Option<&str>) -> Result<Value, NotReady> {
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
            return Err(NotReady::Failed(Failure::new(code::serve_failed, message)));
        }
        if started.elapsed() > READY_WITHIN {
            return Err(match host.waiting {
                Some(why) => NotReady::Waiting(why),
                None => NotReady::Failed(not_ready("")),
            });
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
        return Err(Failure::new(code::stop_failed, message));
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
    // How many browser sessions the host said the stop ends, once it
    // answered: their callers must open new ones.
    let mut sessions = None;
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
        if !asked
            && let Some(pid) = target
            && let Ok(stopping) = ask(dir, "host_stop", json!({"pid": pid})).await
        {
            asked = true;
            sessions = stopping["browser_sessions"].as_u64();
        }
        // A host of another latch never answers on this one's socket: its
        // service manager stops it if it has one; otherwise its owner does.
        if !asked
            && !removed
            && !service.registered()
            && let Some(pid) = lifecycle::other_version(dir)
        {
            return Err(lifecycle::other_version_running(pid));
        }
        if !asked && !removed {
            service.remove()?;
            removed = true;
        }
        if started.elapsed() > STOP_WITHIN {
            if let Some(pid) = lifecycle::other_version(dir) {
                return Err(lifecycle::other_version_running(pid));
            }
            let pid = target.map_or("?".to_string(), |pid| pid.to_string());
            let message = format!(
                "the host (pid {pid}) did not stop within {} seconds",
                STOP_WITHIN.as_secs()
            );
            return Err(Failure::new(code::stop_failed, message));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    service.remove()?;
    let host = lifecycle::now(dir);
    let mut stopped =
        json!({"stopped": was, "state_dir": dir, "state": host.state, "error": host.error});
    if let Some(sessions) = sessions {
        stopped["browser_sessions_ended"] = json!(sessions);
    }
    Ok(stopped)
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
    Failure::new(code::already_serving, message)
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
async fn show(
    hello: &Value,
    boards: tokio::sync::mpsc::Receiver<Board>,
    attached: bool,
) -> dashboard::Ending {
    let theirs = hello["version"].as_str().unwrap_or("unknown");
    let notice = (theirs != crate::cli::version()).then(|| {
        format!(
            "Version mismatch: the host runs latch {theirs}; this is latch {}",
            crate::cli::version()
        )
    });
    // Install every handler before the first frame advertises Ctrl-C. On
    // Windows the terminal going away is the console closing, which
    // `Terminate` hears.
    #[cfg(unix)]
    let (mut interrupt, mut hangup) = {
        use tokio::signal::unix::{SignalKind, signal};
        let interrupt = signal(SignalKind::interrupt()).expect("an interrupt handler");
        (
            interrupt,
            signal(SignalKind::hangup()).expect("a hangup handler"),
        )
    };
    #[cfg(windows)]
    let mut interrupt = tokio::signal::windows::ctrl_c().expect("an interrupt handler");
    let mut terminate = crate::serve::Terminate::listen().expect("a terminate handler");
    let until = async move {
        #[cfg(unix)]
        let hung_up = hangup.recv();
        #[cfg(windows)]
        let hung_up = std::future::pending::<Option<()>>();
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = hung_up => {}
            () = terminate.recv() => {}
        }
    };
    dashboard::show(boards, until, attached, notice).await
}

/// Asks the host on `dir` for `op`'s board: its hello, each board as it
/// comes until the connection ends, and the connection's write side, whose
/// end the host takes as the watcher gone.
async fn watch(
    dir: &Path,
    op: &str,
) -> Result<(Value, tokio::sync::mpsc::Receiver<Board>, LocalWrite), Failure> {
    let (hello, socket) = request(dir, op, json!({})).await?;
    let (mut reading, writing) = socket.into_split();
    let (send, boards) = tokio::sync::mpsc::channel(4);
    tokio::spawn(async move {
        loop {
            let frame = tokio::select! {
                frame = wire::read_frame(&mut reading) => frame,
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
    Ok((hello, boards, writing))
}

/// One question for the host on `dir` and its answer.
async fn ask(dir: &Path, op: &str, args: Value) -> Result<Value, Failure> {
    request(dir, op, args).await.map(|(answer, _)| answer)
}

/// Sends `op` to the host on `dir`; returns its first answer, within
/// `ANSWER_WITHIN`, and the connection, for what the host sends next.
async fn request(dir: &Path, op: &str, args: Value) -> Result<(Value, LocalStream), Failure> {
    tokio::time::timeout(ANSWER_WITHIN, request_now(dir, op, args))
        .await
        .unwrap_or_else(|_| {
            let message = format!(
                "the host did not answer within {} seconds",
                ANSWER_WITHIN.as_secs()
            );
            Err(Failure::new(code::serve_required, message))
        })
}

async fn request_now(dir: &Path, op: &str, args: Value) -> Result<(Value, LocalStream), Failure> {
    let mut socket = transport::connect_local(dir).await?;
    let request = Request {
        op: op.to_string(),
        args,
        chat: None,
    };
    let lost = |err: std::io::Error| {
        Failure::new(
            code::serve_required,
            format!("the host did not answer: {err}"),
        )
    };
    let json = serde_json::to_vec(&request).expect("a request serializes");
    wire::write_frame(&mut socket, &json).await.map_err(lost)?;
    let frame = wire::read_frame(&mut socket).await.map_err(lost)?;
    let response: Response = serde_json::from_slice(&frame)
        .map_err(|_| Failure::new(code::bad_response, "the host's answer is not valid".into()))?;
    Ok((response.into_result()?, socket))
}

fn not_serving(failure: Failure) -> Failure {
    if failure.code != "serve_required" {
        return failure;
    }
    let message = "no host serves this state folder; `latch serve` starts one".to_string();
    Failure::new(code::serve_required, message)
}

/// Held while a host is started or stopped on `dir`.
/// `None` once `until` passes with another start or stop still holding it.
async fn launch_lock(dir: &Path, until: Instant) -> Result<Option<File>, Failure> {
    let path = dir.join("launch.lock");
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .owner_only()
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
    Failure::new(code::state_io, format!("{}: {err}", path.display()))
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
        self.registered_by(None)
            .expect("with no deadline the service manager is waited for")
    }

    /// `registered`, or `None` if the service manager has not answered by
    /// `until`: then its command is killed and reaped.
    pub fn registered_by(&self, until: Option<Instant>) -> Option<bool> {
        platform::registered(&self.label, until)
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
        // A host that just ended may take its service with it meanwhile
        // (systemd forgets a transient unit once its process ends): gone
        // is what was wanted.
        match platform::remove(&self.label) {
            Err(_) if !self.registered() => Ok(()),
            removed => removed,
        }
    }
}

/// Runs `argv` once as a launchd job of its own, as the host runs, so that
/// macOS treats it as the app's and not as the terminal's; returns the line
/// it printed. Its `NAME.plist` and `NAME.log` are in `files`, a new empty
/// folder of the caller's, never in the state folder. The job is named
/// after state folder `dir` and after `files`, so runs at the same time on
/// one state folder never stop each other's.
#[cfg(target_os = "macos")]
pub async fn run_as_app(
    dir: &Path,
    files: &Path,
    name: &str,
    argv: &[OsString],
    within: Duration,
) -> Result<String, Failure> {
    let hash = to_hex(&Sha256::digest(dir.as_os_str().as_encoded_bytes()));
    let run: String = files
        .file_name()
        .map(|run| run.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let label = format!("co.plow.latch.{name}.{}.{run}", &hash[..16]);
    let output = files.join(format!("{name}.log"));
    platform::bootstrap(&label, files, name, argv)?;
    let deadline = Instant::now() + within;
    let printed = loop {
        // One line, written whole, as it ends.
        let printed = std::fs::read_to_string(&output).unwrap_or_default();
        if printed.ends_with('\n') {
            break Ok(printed);
        }
        if Instant::now() > deadline {
            let message = format!("{name} did not answer within {} s", within.as_secs());
            break Err(Failure::new(code::no_answer, message));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    platform::remove(&label)?;
    printed
}

/// A command still running at its deadline.
#[cfg(unix)]
struct TimedOut;

/// `command`'s output, `None` if it could not run. With `until`, a command
/// still running then is killed and reaped, so none is left behind.
#[cfg(unix)]
fn output_by(
    command: &mut Command,
    until: Option<Instant>,
) -> Result<Option<std::process::Output>, TimedOut> {
    let Some(until) = until else {
        return Ok(command.output().ok());
    };
    let spawned = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = spawned else {
        return Ok(None);
    };
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(child.wait_with_output().ok()),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(TimedOut);
            }
        }
    }
}

/// Runs a service manager command; its own words on failure.
#[cfg(unix)]
fn manager(command: &mut Command) -> Result<(), Failure> {
    let output = command
        .output()
        .map_err(|err| Failure::new(code::service_failed, format!("{command:?}: {err}")))?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let message = format!("{command:?} failed: {said}");
    Err(Failure::new(code::service_failed, message))
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::OsString;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;
    use std::process::Command;

    use std::time::Instant;

    use super::{STOP_WITHIN, manager, output_by};
    use crate::failure::{Failure, code};

    /// The logged-in user's session, where an agent may show windows.
    fn domain() -> String {
        // SAFETY: getuid has no arguments and cannot fail.
        format!("gui/{}", unsafe { libc::getuid() })
    }

    pub fn registered(label: &str, until: Option<Instant>) -> Option<bool> {
        let mut command = Command::new("launchctl");
        command.args(["print", &format!("{}/{label}", domain())]);
        let output = output_by(&mut command, until).ok()?;
        Some(output.is_some_and(|output| output.status.success()))
    }

    /// The job is in the state folder, not `~/Library/LaunchAgents`: it is
    /// loaded for this start only, and never at login.
    pub fn start(label: &str, dir: &Path, argv: &[OsString]) -> Result<(), Failure> {
        bootstrap(label, dir, "serve", argv)
    }

    /// Loads the job `NAME.plist` in `dir`, which runs `argv` once with its
    /// output appended to `NAME.log` there.
    pub fn bootstrap(
        label: &str,
        dir: &Path,
        name: &str,
        argv: &[OsString],
    ) -> Result<(), Failure> {
        let path = dir.join(format!("{name}.plist"));
        let plist = job(label, &dir.join(format!("{name}.log")), argv)?;
        let io = |err: std::io::Error| {
            Failure::new(code::state_io, format!("{}: {err}", path.display()))
        };
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
    fn job(label: &str, log: &Path, argv: &[OsString]) -> Result<String, Failure> {
        let text = |value: &std::ffi::OsStr| {
            value.to_str().map(escape).ok_or_else(|| {
                let message = format!("{} is not valid UTF-8", value.display());
                Failure::new(code::service_failed, message)
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
        let log = text(log.as_os_str())?;
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

/// Windows has no service manager a user may use without an administrator,
/// so the host is a background process of the user's own: detached, with no
/// console, its output appended to `serve.log`, and its pid in a file named
/// after the service, which is how it is found again.
#[cfg(windows)]
mod platform {
    use std::ffi::OsString;
    use std::fs::OpenOptions;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::Instant;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS, OpenProcess, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    };

    use crate::failure::{Failure, code};

    fn pid_file(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{label}.pid"))
    }

    /// The service's process, while it runs: a handle that may wait on it
    /// and end it.
    fn running(label: &str) -> Option<HANDLE> {
        let pid: u32 = std::fs::read_to_string(pid_file(label))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        // SAFETY: OpenProcess takes plain values; a null handle is a failure.
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            return None;
        }
        // SAFETY: `process` is a live handle this function owns.
        let alive = unsafe { WaitForSingleObject(process, 0) } == WAIT_TIMEOUT;
        if alive {
            return Some(process);
        }
        // SAFETY: as above; it is closed once.
        unsafe { CloseHandle(process) };
        None
    }

    pub fn registered(label: &str, _until: Option<Instant>) -> Option<bool> {
        let process = running(label);
        if let Some(process) = process {
            // SAFETY: `running` handed over this live handle.
            unsafe { CloseHandle(process) };
        }
        Some(process.is_some())
    }

    pub fn start(label: &str, dir: &Path, argv: &[OsString]) -> Result<(), Failure> {
        let failed =
            |err: std::io::Error| Failure::new(code::service_failed, format!("{argv:?}: {err}"));
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("serve.log"))
            .map_err(failed)?;
        let child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(failed)?)
            .stderr(log)
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .map_err(failed)?;
        std::fs::write(pid_file(label), child.id().to_string()).map_err(failed)
    }

    pub fn remove(label: &str) -> Result<(), Failure> {
        if let Some(process) = running(label) {
            // SAFETY: `running` handed over this live handle; it is closed once.
            unsafe {
                TerminateProcess(process, 1);
                CloseHandle(process);
            }
        }
        match std::fs::remove_file(pid_file(label)) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(Failure::new(
                code::service_failed,
                format!("{}: {err}", pid_file(label).display()),
            )),
            _ => Ok(()),
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::ffi::OsString;
    use std::path::Path;
    use std::process::Command;

    use std::time::Instant;

    use super::{STOP_WITHIN, manager, output_by};
    use crate::failure::Failure;

    fn unit(label: &str) -> String {
        format!("{label}.service")
    }

    pub fn registered(label: &str, until: Option<Instant>) -> Option<bool> {
        let mut command = Command::new("systemctl");
        command.args([
            "--user",
            "show",
            "--property=LoadState",
            "--value",
            &unit(label),
        ]);
        let output = output_by(&mut command, until).ok()?;
        Some(output.is_some_and(|output| output.stdout.starts_with(b"loaded")))
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
