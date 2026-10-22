//! The host's side of `exec`: run one program in a sandboxed worker
//! (`worker.rs`) and report how it ended and what it printed.
//!
//! The program gets an argv list, never a shell line, and an empty stdin
//! unless the client sent some (`--stdin`, at most 256 KiB), which a task
//! of its own writes to the program and then closes. Each output stream is
//! kept up to 256 KiB (the rest is read and dropped, and `truncated` is
//! set), so a runaway command cannot flood the agent. At the time limit the
//! whole process group (the program and everything it started) is killed.
//! So is it, cut off, when the host stops (`interrupted`) or the caller's
//! connection closes (`caller_gone`): nobody would get its result. A host
//! that dies without stopping takes the program with it (see
//! `WorkerSetup::start_exec`).
//! The audit record holds the whole argv, as run, the workdir and how it
//! ended, never the output. The audit log is encrypted; whatever shows the
//! argv to people hides credential-looking values first (`redact.rs`).
//!
//! Each run gets a fresh, empty temp folder of its own as its `TMPDIR`,
//! made in the host's OS temp folder and readable and writable by that
//! worker only (on Linux the worker's private `/tmp` plays that part), and
//! the folder is deleted when the run ends. It never lands in a shared
//! folder, which may not be writable (a shared `/`, say). A host that was
//! killed leaves its folders behind; the next host to start removes them.

use std::collections::BTreeMap;
use std::future::Future;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Child;
use tokio::task::JoinHandle;

use crate::failure::Failure;
use crate::log::log;
use crate::serve::Done;
use crate::state::to_hex;
use crate::wire::Response;
use crate::worker::{WorkerError, WorkerSetup, authenticated_worker_error};

/// How much of each output stream the agent gets back.
const OUTPUT_CAP: usize = 256 * 1024;
/// The longest `--timeout` a host accepts.
const MAX_TIMEOUT_SECS: u64 = 600;
/// The most stdin a call may bring (`latch exec --stdin`).
pub const MAX_STDIN: usize = 256 * 1024;
/// After the program ends, how long to wait for its output pipes to close
/// (a process that left the group could otherwise hold them open).
const DRAIN_GRACE: Duration = Duration::from_secs(5);
/// How long after its timeout the worker kills a program itself, if the
/// host has not by then.
const WORKER_GRACE: u64 = 5;

#[derive(Deserialize, Serialize)]
pub(crate) struct ExecArgs {
    pub argv: Vec<String>,
    #[serde(default = "default_timeout")]
    timeout: u64,
    pub workdir: Option<String>,
    stdin: Option<Vec<u8>>,
    #[serde(default)]
    pub writes: Vec<PathBuf>,
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub secret_env: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct PrivateExec {
    pub secret_env: BTreeMap<String, String>,
    pub error_token: String,
}

pub fn parse_secret_env(values: &[String]) -> Result<BTreeMap<String, String>, Failure> {
    let mut secrets = BTreeMap::new();
    for value in values {
        let (name, value) = value.split_once('=').ok_or_else(bad_secret_env)?;
        if secrets
            .insert(name.to_string(), value.to_string())
            .is_some()
        {
            return Err(bad_secret_env());
        }
    }
    validate_secret_env(&secrets)?;
    Ok(secrets)
}

fn bad_secret_env() -> Failure {
    Failure::failed("bad_secret_env", "use distinct NAME=VALUE entries with valid environment names; HOME, PATH, TMPDIR, LATCH_*, LD_* and DYLD_* are reserved".into())
}

pub(crate) fn validate_secret_env(secrets: &BTreeMap<String, String>) -> Result<(), Failure> {
    for (name, value) in secrets {
        let mut bytes = name.bytes();
        let valid = bytes
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if !valid
            || ["HOME", "PATH", "TMPDIR"].contains(&name.as_str())
            || ["LATCH_", "LD_", "DYLD_"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
            || value.contains('\0')
        {
            return Err(bad_secret_env());
        }
    }
    Ok(())
}

pub(crate) fn secret_metadata(secrets: &BTreeMap<String, String>) -> Value {
    Value::Array(
        secrets
            .iter()
            .map(|(name, value)| json!({"name": name, "secret": true, "length": value.len()}))
            .collect(),
    )
}

/// A validated request whose resolved paths are the ones the judge sees.
#[derive(Serialize)]
#[serde(transparent)]
pub struct PreparedExec(ExecArgs);

fn default_timeout() -> u64 {
    60
}

/// Resolves host path fields before judgment, without creating anything.
pub fn normalize(workers: &WorkerSetup, value: &Value) -> Result<PreparedExec, Failure> {
    let bad = |message: &str| Failure::failed("bad_request", message.to_string());
    if !value.is_object() {
        return Err(bad("exec arguments must be an object"));
    }
    let mut args: ExecArgs = serde_json::from_value(value.clone())
        .map_err(|_| bad("exec arguments have invalid types"))?;
    validate_secret_env(&args.secret_env)?;
    if !argv_is_usable(&args.argv) {
        return Err(bad("argv must be a non-empty list of strings"));
    }
    if !(1..=MAX_TIMEOUT_SECS).contains(&args.timeout) {
        return Err(bad("timeout must be 1 to 600 seconds"));
    }
    if args
        .stdin
        .as_ref()
        .is_some_and(|bytes| bytes.len() > MAX_STDIN)
    {
        return Err(bad("stdin must be at most 256 KiB of bytes"));
    }
    for path in &mut args.writes {
        *path = resolve(&workers.home, path)?;
        check_path(workers, path, true)?;
        if !path.exists() && !path.parent().is_some_and(Path::is_dir) {
            return Err(bad(
                "a missing write path must have an existing parent folder",
            ));
        }
    }
    let workdir = match args.workdir.as_deref() {
        Some(path) => resolve(&workers.home, Path::new(path))?,
        None => args
            .writes
            .first()
            .filter(|path| !path.is_file())
            .cloned()
            .unwrap_or_else(|| workers.home.clone()),
    };
    check_path(workers, &workdir, false)?;
    if !workdir.is_dir() && (workdir.exists() || !args.writes.contains(&workdir)) {
        return Err(bad(
            "workdir must be an existing folder or a declared new write folder",
        ));
    }
    args.workdir = Some(
        workdir
            .to_str()
            .ok_or_else(|| bad("host paths must be valid UTF-8"))?
            .to_owned(),
    );
    if args.writes.iter().any(|path| path.to_str().is_none()) {
        return Err(bad("host paths must be valid UTF-8"));
    }
    Ok(PreparedExec(args))
}

fn resolve(home: &Path, path: &Path) -> Result<PathBuf, Failure> {
    let path = match path.strip_prefix("~") {
        Ok(rest) => home.join(rest),
        Err(_) if path.is_relative() => home.join(path),
        Err(_) => path.to_path_buf(),
    };
    let mut above = path.as_path();
    let mut missing = Vec::new();
    loop {
        match above.canonicalize() {
            Ok(mut real) => {
                for name in missing.iter().rev() {
                    real.push(name);
                }
                return Ok(real);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = above.file_name() else { break };
                missing.push(name.to_os_string());
                let Some(parent) = above.parent() else { break };
                above = parent;
            }
            Err(_) => break,
        }
    }
    Err(Failure::failed(
        "bad_path",
        "the host path cannot be resolved".into(),
    ))
}

fn check_path(workers: &WorkerSetup, path: &Path, write: bool) -> Result<(), Failure> {
    let hidden = path.starts_with(&workers.state_dir)
        || workers.hidden.iter().any(|hidden| {
            let hidden = hidden.canonicalize().unwrap_or_else(|_| hidden.clone());
            let cache = cfg!(target_os = "macos")
                && hidden == workers.home.join("Library")
                && path.starts_with(workers.home.join("Library/Caches"));
            path.starts_with(hidden) && !cache
        });
    if !path.starts_with(&workers.home) || (write && path == workers.home) || hidden {
        return Err(Failure::new(crate::failure::Exit::Denied, "bad_path",
            "the path must be inside the host's home and outside hidden state; home itself cannot be a write target".into()));
    }
    Ok(())
}

/// Only existing, plain cache directories receive implicit write access.
pub(crate) fn caches(workers: &WorkerSetup) -> Vec<PathBuf> {
    [
        ".cache",
        ".npm",
        ".cargo/registry",
        ".cargo/git",
        "go/pkg/mod",
    ]
    .into_iter()
    .chain(cfg!(target_os = "macos").then_some("Library/Caches"))
    .map(|name| workers.home.join(name))
    .filter(|path| {
        check_path(workers, path, true).is_ok()
            && path
                .ancestors()
                .take_while(|p| *p != workers.home)
                .all(|p| std::fs::symlink_metadata(p).is_ok_and(|meta| meta.is_dir()))
    })
    .collect()
}

/// `exec`: runs `args.argv` and returns its result. If `cut_off` finishes
/// first, the program is killed, and its outcome is the one `cut_off` gives.
pub async fn run(
    workers: &WorkerSetup,
    prepared: PreparedExec,
    cut_off: impl Future<Output = &'static str>,
) -> Done {
    let mut args = prepared.0;
    let timeout = args.timeout;
    // What the program reads on stdin: the bytes the client sent, or none.
    let input = args.stdin.take();
    let mut audit = json!({ "argv": args.argv, "workdir": args.workdir,
                           "writes": args.writes, "network": args.network,
                           "secret_env": secret_metadata(&args.secret_env) });
    if let Some(bytes) = &input {
        audit["stdin"] = stdin_audit(bytes);
    }

    for path in args
        .writes
        .iter()
        .map(PathBuf::as_path)
        .chain(args.workdir.as_deref().map(Path::new))
    {
        if !resolve(&workers.home, path).is_ok_and(|real| real == path) {
            return Done::failed("bad_path", "the path changed after judgment".into(), audit);
        }
    }

    for path in &args.writes {
        if !path.exists()
            && let Err(error) = create_write_dir(path)
        {
            return Done::failed("write_path_failed", error.to_string(), audit);
        }
    }

    let tmpdir = match make_tmpdir() {
        Ok(tmpdir) => tmpdir,
        Err(err) => {
            let message = format!("{}: {err}", std::env::temp_dir().display());
            return Done::failed("tmpdir_failed", message, audit);
        }
    };
    let limits = Limits { timeout, cut_off };
    let done = run_program(workers, &args, &tmpdir, input, limits, audit).await;
    // `remove_dir_all` deletes links the program left there, never what
    // they point to.
    if let Err(err) = std::fs::remove_dir_all(&tmpdir) {
        log(&format!(
            "exec: could not remove {}: {err}",
            tmpdir.display()
        ));
    }
    done
}

fn create_write_dir(path: &Path) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent folder"))?;
    let folder = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(parent)?;
    if crate::sandbox::opened_path(&folder)? != parent {
        return Err(std::io::Error::other(
            "the write parent changed after judgment",
        ));
    }
    let name = std::ffi::CString::new(
        path.file_name()
            .ok_or_else(|| std::io::Error::other("no folder name"))?
            .as_bytes(),
    )
    .map_err(std::io::Error::other)?;
    // SAFETY: the file owns a live directory descriptor and name is one NUL-
    // terminated component. Anchoring mkdirat there cannot follow a swapped parent.
    if unsafe { libc::mkdirat(folder.as_raw_fd(), name.as_ptr(), 0o777) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// What the audit keeps of an exec's stdin: its size and sha256, never
/// its content.
pub fn stdin_audit(bytes: &[u8]) -> Value {
    json!({ "size": bytes.len(), "sha256": to_hex(&Sha256::digest(bytes)) })
}

/// A new, empty folder `latch-exec-<host pid>-<random>` in the OS temp
/// folder, for this user only. Its real path, since the sandbox matches
/// real paths (macOS's temp folder is behind the `/var` link).
fn make_tmpdir() -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let name = format!(
        "latch-exec-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>()
    );
    let path = std::env::temp_dir().join(name);
    // `create` (not `recursive`) fails if the name already exists.
    std::fs::DirBuilder::new().mode(0o700).create(&path)?;
    path.canonicalize()
}

/// Removes the temp folders that killed hosts left: this user's
/// `latch-exec-<pid>-…` folders in the OS temp folder whose host no longer
/// runs. Those of hosts still running (this one, or another on another
/// state folder) stay.
pub fn sweep_tmpdirs() {
    let temp = std::env::temp_dir();
    let Ok(entries) = std::fs::read_dir(&temp) else {
        return;
    };
    // SAFETY: std has no UID API. getuid takes no arguments, accesses no Rust
    // memory, and returns our real UID for the ownership check below.
    let me = unsafe { libc::getuid() };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let host = name
            .to_str()
            .and_then(|name| name.strip_prefix("latch-exec-"))
            .and_then(|rest| rest.split_once('-'))
            .and_then(|(pid, _)| pid.parse::<libc::pid_t>().ok());
        let Some(host) = host.filter(|pid| *pid > 0) else {
            continue;
        };
        let Ok(meta) = entry.path().symlink_metadata() else {
            continue;
        };
        if !meta.is_dir() || meta.uid() != me || running(host) {
            continue;
        }
        match std::fs::remove_dir_all(entry.path()) {
            Ok(()) => log(&format!(
                "exec: removed {}, left by a host that was killed",
                entry.path().display()
            )),
            Err(err) => log(&format!(
                "exec: could not remove {}: {err}",
                entry.path().display()
            )),
        }
    }
}

/// Whether a process `pid` runs (one of another user counts too).
fn running(pid: libc::pid_t) -> bool {
    // SAFETY: std cannot probe an arbitrary PID. Signal 0 checks existence
    // without delivering a signal; this scalar-only call accesses no Rust memory.
    let found = unsafe { libc::kill(pid, 0) } == 0;
    found || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// When a program must stop: at `timeout` seconds, or when `cut_off`
/// finishes, with the outcome it gives.
struct Limits<F> {
    timeout: u64,
    cut_off: F,
}

/// How the wait for a program ended.
enum End {
    Exited,
    TimedOut,
    /// Cut off, with this outcome.
    Cut(&'static str),
}

/// Starts the program and waits for it, within `limits`.
async fn run_program(
    workers: &WorkerSetup,
    args: &ExecArgs,
    tmpdir: &Path,
    input: Option<Vec<u8>>,
    limits: Limits<impl Future<Output = &'static str>>,
    mut audit: Value,
) -> Done {
    let started = Instant::now();
    let kill_after = limits.timeout + WORKER_GRACE;
    let private = PrivateExec {
        secret_env: args.secret_env.clone(),
        error_token: to_hex(&rand::random::<[u8; 32]>()),
    };
    let started_exec = workers.start_exec(args, tmpdir, &private.error_token, kill_after);
    let mut child = match started_exec {
        Ok(child) => child,
        Err(error) => return refused(error, audit),
    };
    let stdout = capture(child.stdout.take().expect("stdout is piped"));
    let stderr = capture(child.stderr.take().expect("stderr is piped"));
    // Fed from its own task while the output is read and the clock runs: a
    // program may read slowly, or never (then the write fails, which is
    // fine). Dropping the pipe at the end is the program's EOF.
    if let Some(mut pipe) = child.stdin.take() {
        let header = serde_json::to_vec(&private).expect("private exec request serializes");
        tokio::spawn(async move {
            if crate::wire::write_frame(&mut pipe, &header).await.is_ok()
                && let Some(bytes) = input
            {
                let _ = pipe.write_all(&bytes).await;
            }
        });
    }
    let (status, end) = wait_or_kill(&mut child, limits).await;
    let timed_out = matches!(end, End::TimedOut);
    // Both streams get the same grace, side by side.
    let ((stdout, stdout_cut), (stderr, stderr_cut)) = tokio::join!(
        collected(stdout, DRAIN_GRACE),
        collected(stderr, DRAIN_GRACE)
    );
    let duration_ms = started.elapsed().as_millis() as u64;

    // The worker failed before the program started: no execution result.
    if status.code() == Some(125)
        && let Some(error) =
            authenticated_worker_error(&String::from_utf8_lossy(&stderr), &private.error_token)
    {
        return refused(error, audit);
    }

    let exit_code = status.code();
    let signal = status.signal().map(signal_name);
    audit["exit_code"] = json!(exit_code);
    audit["signal"] = json!(signal);
    audit["timed_out"] = json!(timed_out);
    audit["duration_ms"] = json!(duration_ms);
    // How much it wrote to stderr (never what), so the host's view can mark it.
    audit["stderr_bytes"] = json!(stderr.len());
    if let End::Cut(outcome) = end {
        let message = match outcome {
            "interrupted" => "the host stopped while the program ran, and killed it",
            _ => "the caller went away while the program ran, so the host killed it",
        };
        return Done {
            response: Response::Error {
                exit: crate::failure::Exit::Unknown,
                details: Value::Null,
                code: outcome.to_string(),
                message: format!("{message} after {duration_ms} ms"),
            },
            audit_args: audit,
            outcome: outcome.to_string(),
        };
    }
    let result = json!({
        "writes": args.writes,
        "network": args.network,
        "exit_code": exit_code,
        "signal": signal,
        "timed_out": timed_out,
        "stdout": String::from_utf8_lossy(&stdout),
        "stderr": String::from_utf8_lossy(&stderr),
        "truncated": stdout_cut || stderr_cut,
    });
    Done {
        response: Response::Ok(result),
        audit_args: audit,
        outcome: if timed_out { "timed_out" } else { "ok" }.to_string(),
    }
}

fn argv_is_usable(argv: &[String]) -> bool {
    !argv.is_empty() && !argv[0].is_empty()
}

/// Waits for the program, within `limits`. Past them, kills the program's
/// whole process group and returns its (killed) status, and why.
async fn wait_or_kill(
    child: &mut Child,
    limits: Limits<impl Future<Output = &'static str>>,
) -> (ExitStatus, End) {
    let end = tokio::select! {
        biased;
        () = tokio::time::sleep(Duration::from_secs(limits.timeout)) => End::TimedOut,
        status = child.wait() => match status {
            Ok(status) => return (status, End::Exited),
            Err(_) => End::TimedOut,
        },
        outcome = limits.cut_off => End::Cut(outcome),
    };
    if let Some(pid) = child.id() {
        // The worker started the program as the leader of a new process
        // group whose id is its pid, so this reaches every process in it.
        // SAFETY: std's child kill cannot reach descendants. This live child's
        // PID names the group we created for it; killpg takes only scalar arguments.
        unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
    }
    let status = child
        .wait()
        .await
        .unwrap_or_else(|_| ExitStatus::from_raw(libc::SIGKILL));
    (status, end)
}

/// What has been read from one output stream so far. A reader task fills
/// it while `run` may already be looking (see `collected`), so it is shared
/// through an `Arc<Mutex<…>>`: `Arc` lets both hold it, `Mutex` lets one at
/// a time change it.
#[derive(Default)]
struct Captured {
    /// The first `OUTPUT_CAP` bytes.
    kept: Vec<u8>,
    /// Whether anything beyond them was dropped.
    cut: bool,
}

/// A stream being read in the background: its output so far, and the task.
struct Capture {
    captured: Arc<Mutex<Captured>>,
    task: JoinHandle<()>,
}

/// Starts reading `stream` in the background.
fn capture(stream: impl AsyncRead + Unpin + Send + 'static) -> Capture {
    let captured = Arc::new(Mutex::new(Captured::default()));
    let task = tokio::spawn(read_capped(stream, captured.clone()));
    Capture { captured, task }
}

/// Reads a stream to its end into `captured`, keeping the first
/// `OUTPUT_CAP` bytes and noting whether more came.
async fn read_capped(mut stream: impl AsyncRead + Unpin, captured: Arc<Mutex<Captured>>) {
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        let mut captured = captured.lock().expect("capture lock");
        let room = OUTPUT_CAP - captured.kept.len();
        captured.kept.extend_from_slice(&buffer[..read.min(room)]);
        captured.cut |= read > room;
    }
}

/// The output of a stream, once its reader reached the end, or after
/// `grace`, whichever comes first. A stream still open after the grace (a
/// background process the program left behind holds it) keeps everything
/// read so far, and is marked cut, since more may follow.
async fn collected(capture: Capture, grace: Duration) -> (Vec<u8>, bool) {
    let abort = capture.task.abort_handle();
    let finished = tokio::time::timeout(grace, capture.task).await.is_ok();
    if !finished {
        abort.abort();
    }
    let mut captured = capture.captured.lock().expect("capture lock");
    let kept = std::mem::take(&mut captured.kept);
    (kept, captured.cut || !finished)
}

/// A signal's usual name, e.g. 11 -> "SIGSEGV".
fn signal_name(signal: i32) -> String {
    let name = match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGSYS => "SIGSYS",
        other => return format!("SIG{other}"),
    };
    name.to_string()
}

/// The worker refused or failed before the program ran: a refusal by the
/// sandbox (or a workdir outside the areas, or a host sharing no folders)
/// is a denial, anything else a failure. There is no execution result.
fn refused(error: WorkerError, audit_args: Value) -> Done {
    let refusal = matches!(
        error.code.as_str(),
        "sandbox_denied" | "workdir_outside_areas" | "no_folders"
    );
    if !refusal {
        return Done::failed(&error.code, error.message, audit_args);
    }
    Done {
        response: Response::Error {
            exit: crate::failure::Exit::Denied,
            details: Value::Null,
            code: error.code.clone(),
            message: error.message,
        },
        audit_args,
        outcome: error.code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::Exit;
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::AsyncWriteExt;

    async fn run(
        workers: &WorkerSetup,
        args: &Value,
        cut_off: impl Future<Output = &'static str>,
    ) -> Done {
        match normalize(workers, args) {
            Ok(prepared) => super::run(workers, prepared, cut_off).await,
            Err(failure) => Done {
                response: Response::error(&failure),
                audit_args: Value::Null,
                outcome: failure.code,
            },
        }
    }

    #[tokio::test]
    async fn an_elapsed_timeout_wins_over_a_ready_exit() {
        let mut children = Vec::new();
        let mut inputs = Vec::new();
        for _ in 0..32 {
            let mut child = tokio::process::Command::new("/bin/sh")
                .args(["-c", "read line"])
                .stdin(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            inputs.push(child.stdin.take().unwrap());
            children.push(child);
        }
        let mut waits: Vec<_> = children
            .iter_mut()
            .map(|child| {
                Box::pin(wait_or_kill(
                    child,
                    Limits {
                        timeout: 1,
                        cut_off: std::future::pending(),
                    },
                ))
            })
            .collect();
        std::future::poll_fn(|cx| {
            for wait in &mut waits {
                assert!(wait.as_mut().poll(cx).is_pending());
            }
            std::task::Poll::Ready(())
        })
        .await;
        // Like a frozen host: both the exits and the timers become ready
        // before it is able to poll the waits again.
        drop(inputs);
        std::thread::sleep(Duration::from_millis(1100));
        tokio::time::sleep(Duration::from_millis(10)).await;
        for wait in waits {
            let (_, end) = wait.await;
            assert!(
                matches!(end, End::TimedOut),
                "reported completion after timeout"
            );
        }
    }

    fn worker_setup() -> (tempfile::TempDir, WorkerSetup) {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        std::fs::create_dir_all(&parent).unwrap();
        let dir = tempfile::tempdir_in(parent).unwrap();
        let program = dir.path().join("worker");
        std::fs::write(&program, "#!/bin/sh\necho started > worker-started\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let workers = WorkerSetup {
            home: dir.path().canonicalize().unwrap(),
            areas: vec![dir.path().to_path_buf()],
            reads: vec![],
            hidden: vec![],
            state_dir: dir.path().join("state"),
            program,
        };
        (dir, workers)
    }

    #[tokio::test]
    async fn malformed_exec_args_never_start_a_worker() {
        let (dir, workers) = worker_setup();
        for (field, values) in [
            (
                "secret_env",
                vec![json!(null), json!([]), json!({"TOKEN": 7})],
            ),
            ("network", vec![json!(null), json!("false"), json!(1)]),
            (
                "writes",
                vec![json!(null), json!("project"), json!([false])],
            ),
            (
                "timeout",
                vec![
                    json!(null),
                    json!("1"),
                    json!(true),
                    json!(1.5),
                    json!(-1),
                    json!(0),
                    json!(601),
                ],
            ),
            (
                "workdir",
                vec![json!(1), json!(false), json!([]), json!({})],
            ),
            (
                "argv",
                vec![
                    json!(null),
                    json!("true"),
                    json!([1]),
                    json!([]),
                    json!([""]),
                ],
            ),
            (
                "stdin",
                vec![
                    json!("text"),
                    json!(true),
                    json!([-1]),
                    json!([256]),
                    json!(vec![0; MAX_STDIN + 1]),
                ],
            ),
        ] {
            for (case, value) in values.into_iter().enumerate() {
                let mut args = json!({"argv": ["unused"]});
                args[field] = value;
                let done = run(&workers, &args, std::future::pending()).await;
                assert!(
                    !dir.path().join("worker-started").exists(),
                    "{field} case {case} started a worker"
                );
                let Response::Error { exit, code, .. } = done.response else {
                    panic!("{field} case {case} was accepted");
                };
                assert_eq!((exit, code.as_str()), (Exit::Failed, "bad_request"));
                assert_eq!(done.audit_args, Value::Null);
            }
        }
    }

    #[tokio::test]
    async fn positional_exec_args_never_start_a_worker() {
        let (dir, workers) = worker_setup();
        let done = run(
            &workers,
            &json!([["unused"], 60, null, null]),
            std::future::pending(),
        )
        .await;
        assert!(!dir.path().join("worker-started").exists());
        assert!(matches!(
            done.response,
            Response::Error { code, .. } if code == "bad_request"
        ));
        assert_eq!(done.audit_args, Value::Null);
    }

    #[tokio::test]
    async fn omitted_and_null_optional_exec_args_still_run() {
        let (dir, workers) = worker_setup();
        for args in [
            json!({"argv": ["unused"]}),
            json!({"argv": ["unused"], "workdir": null, "stdin": null}),
        ] {
            let done = run(&workers, &args, std::future::pending()).await;
            assert!(matches!(done.response, Response::Ok(_)));
            assert!(dir.path().join("worker-started").exists());
            assert_eq!(done.audit_args["workdir"], json!(workers.home));
        }
    }

    #[tokio::test]
    async fn declared_projects_support_cargo_build_and_npm_install() {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        std::fs::create_dir_all(&parent).unwrap();
        let home = std::env::home_dir().unwrap().canonicalize().unwrap();
        // A gate container can mount the repository outside its HOME (/tmp).
        let parent = parent.canonicalize().unwrap();
        let dir = tempfile::tempdir_in(if parent.starts_with(&home) {
            &parent
        } else {
            &home
        })
        .unwrap();
        let root = dir.path().canonicalize().unwrap();
        let state = root.join("state");
        std::fs::create_dir(&state).unwrap();
        let binary = std::env::current_exe().unwrap();
        let workers = WorkerSetup {
            home: home.clone(),
            areas: vec![],
            reads: vec![],
            hidden: crate::sandbox::never_readable(&home),
            state_dir: state,
            program: binary.parent().unwrap().parent().unwrap().join("latch"),
        };
        let rust = root.join("rust-project");
        std::fs::create_dir_all(rust.join("src")).unwrap();
        std::fs::write(
            rust.join("Cargo.toml"),
            "[package]\nname='latch_exec_probe'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();
        std::fs::write(rust.join("src/lib.rs"), "pub fn answer() -> u32 { 42 }\n").unwrap();
        let node = root.join("node-project");
        std::fs::create_dir(&node).unwrap();
        std::fs::write(
            node.join("package.json"),
            r#"{"name":"latch-exec-probe","version":"1.0.0","private":true}"#,
        )
        .unwrap();
        let rustup = std::env::var_os("RUSTUP_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".rustup"));
        for (project, argv) in [
            (
                &rust,
                vec![
                    "/usr/bin/env".to_string(),
                    format!("RUSTUP_HOME={}", rustup.display()),
                    format!("CARGO_HOME={}", rust.join("cache").display()),
                    "cargo".into(),
                    "build".into(),
                    "--offline".into(),
                ],
            ),
            (
                &node,
                vec![
                    "/usr/bin/env".into(),
                    format!("npm_config_cache={}", node.join("cache").display()),
                    "npm".into(),
                    "install".into(),
                    "--ignore-scripts".into(),
                    "--no-audit".into(),
                    "--no-fund".into(),
                ],
            ),
        ] {
            let args = json!({"argv": argv, "writes": [project], "network": true, "timeout": 60});
            let done = run(&workers, &args, std::future::pending()).await;
            let Response::Ok(result) = done.response else {
                panic!("{:?}", done.response)
            };
            assert_eq!(result["exit_code"], 0, "{result}");
        }
        assert!(rust.join("target/debug/liblatch_exec_probe.rlib").is_file());
        assert!(node.join("package-lock.json").is_file());
    }

    #[tokio::test]
    async fn secret_values_are_not_in_the_workers_process_arguments() {
        let (dir, mut workers) = worker_setup();
        let binary = std::env::current_exe().unwrap();
        workers.program = binary.parent().unwrap().parent().unwrap().join("latch");
        std::fs::create_dir(&workers.state_dir).unwrap();
        let tmp = dir.path().join("tmp");
        std::fs::create_dir(&tmp).unwrap();
        let marker = "worker-argv-private-marker-4a7d";
        let value = normalize(
            &workers,
            &json!({"argv": ["true"], "secret_env": {"TOKEN": marker}}),
        )
        .unwrap();
        let args = value.0;
        let mut child = workers
            .start_exec(&args, &tmp, "startup-test-token", 5)
            .unwrap();
        let pid = child.id().unwrap();
        #[cfg(target_os = "linux")]
        let arguments = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
        #[cfg(target_os = "macos")]
        let arguments = {
            let output = std::process::Command::new("/bin/ps")
                .args(["-p", &pid.to_string(), "-o", "command="])
                .output()
                .unwrap();
            assert!(output.status.success());
            output.stdout
        };
        let arguments = String::from_utf8_lossy(&arguments);
        assert!(arguments.contains("exec"));
        assert!(!arguments.contains(marker));
        drop(child.stdin.take());
        child.wait().await.unwrap();
    }

    #[tokio::test]
    async fn output_is_kept_up_to_the_cap() {
        let grace = Duration::from_secs(1);
        let big = std::io::Cursor::new(vec![b'a'; OUTPUT_CAP + 10]);
        let (kept, cut) = collected(capture(big), grace).await;
        assert_eq!((kept.len(), cut), (OUTPUT_CAP, true));
        let small = std::io::Cursor::new(b"small".to_vec());
        let (kept, cut) = collected(capture(small), grace).await;
        assert_eq!((kept, cut), (b"small".to_vec(), false));
    }

    #[tokio::test]
    async fn output_read_before_the_grace_ran_out_is_kept() {
        // Like `sh -c 'printf hello; sleep 20 &'`: the program printed and
        // exited, but a background child still holds the pipe open.
        let (mut writer, reader) = tokio::io::duplex(1024);
        writer.write_all(b"hello").await.unwrap();
        let (kept, cut) = collected(capture(reader), Duration::from_millis(200)).await;
        assert_eq!(kept, b"hello");
        assert!(cut, "the stream was still open, so more may have followed");
        drop(writer);
    }

    #[test]
    fn signals_have_their_usual_names() {
        assert_eq!(signal_name(libc::SIGSEGV), "SIGSEGV");
        assert_eq!(signal_name(libc::SIGKILL), "SIGKILL");
    }
}
