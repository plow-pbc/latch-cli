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
//! The audit record holds the argv (up to 4 KiB, as run), the workdir and
//! how it ended, never the output.
//!
//! Each run gets a fresh, empty temp folder of its own as its `TMPDIR`,
//! made in the host's OS temp folder and readable and writable by that
//! worker only (on Linux the worker's private `/tmp` plays that part), and
//! the folder is deleted when the run ends. It never lands in a shared
//! folder, which may not be writable (a shared `/`, say). A host that was
//! killed leaves its folders behind; the next host to start removes them.

use std::future::Future;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Child;
use tokio::task::JoinHandle;

use crate::log::log;
use crate::serve::Done;
use crate::state::to_hex;
use crate::wire::Response;
use crate::worker::{WorkerError, WorkerSetup, worker_error};

/// How much of each output stream the agent gets back.
const OUTPUT_CAP: usize = 256 * 1024;
/// How much of the argv the audit log keeps.
const ARGV_AUDIT_CAP: usize = 4 * 1024;
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

#[derive(Deserialize)]
struct ExecArgs {
    argv: Vec<String>,
    #[serde(default = "default_timeout")]
    timeout: u64,
    workdir: Option<String>,
    stdin: Option<Vec<u8>>,
}

fn default_timeout() -> u64 {
    60
}

/// `exec`: runs `args.argv` and returns its result. If `cut_off` finishes
/// first, the program is killed, and its outcome is the one `cut_off` gives.
pub async fn run(
    workers: &WorkerSetup,
    args: &Value,
    cut_off: impl Future<Output = &'static str>,
) -> Done {
    if !args.is_object() {
        return bad_request("exec arguments must be an object");
    }
    let args: ExecArgs = match serde_json::from_value(args.clone()) {
        Ok(args) => args,
        // A deserialization error may quote stdin, which must not be returned.
        Err(_) => return bad_request("exec arguments have invalid types"),
    };
    let argv = args.argv;
    if !argv_is_usable(&argv) {
        return bad_request("argv must be a non-empty list of strings");
    }
    let timeout = args.timeout;
    if !(1..=MAX_TIMEOUT_SECS).contains(&timeout) {
        return bad_request("timeout must be 1 to 600 seconds");
    }
    let workdir = args.workdir.as_deref();
    // The workdir as asked, or the first area, where a worker starts.
    let shown_workdir = match workdir {
        Some(dir) => Some(dir.to_string()),
        None => workers.areas.first().map(|area| area.display().to_string()),
    };
    // What the program reads on stdin: the bytes the client sent, or none.
    let input = args.stdin;
    if input.as_ref().is_some_and(|bytes| bytes.len() > MAX_STDIN) {
        return bad_request("stdin must be at most 256 KiB of bytes");
    }
    let mut audit = json!({ "argv": capped_argv(&argv), "workdir": shown_workdir });
    if let Some(bytes) = &input {
        audit["stdin"] = stdin_audit(bytes);
    }

    let tmpdir = match make_tmpdir() {
        Ok(tmpdir) => tmpdir,
        Err(err) => {
            let message = format!("{}: {err}", std::env::temp_dir().display());
            return Done::failed("tmpdir_failed", message, audit);
        }
    };
    let limits = Limits { timeout, cut_off };
    let done = run_program(workers, workdir, &tmpdir, &argv, input, limits, audit).await;
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
    workdir: Option<&str>,
    tmpdir: &Path,
    argv: &[String],
    input: Option<Vec<u8>>,
    limits: Limits<impl Future<Output = &'static str>>,
    mut audit: Value,
) -> Done {
    let started = Instant::now();
    let kill_after = limits.timeout + WORKER_GRACE;
    let started_exec = workers.start_exec(workdir, tmpdir, argv, input.is_some(), kill_after);
    let mut child = match started_exec {
        Ok(child) => child,
        Err(error) => return refused(error, audit),
    };
    let stdout = capture(child.stdout.take().expect("stdout is piped"));
    let stderr = capture(child.stderr.take().expect("stderr is piped"));
    // Fed from its own task while the output is read and the clock runs: a
    // program may read slowly, or never (then the write fails, which is
    // fine). Dropping the pipe at the end is the program's EOF.
    if let (Some(bytes), Some(mut pipe)) = (input, child.stdin.take()) {
        tokio::spawn(async move {
            let _ = pipe.write_all(&bytes).await;
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
        && let Some(error) = worker_error(&String::from_utf8_lossy(&stderr))
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

/// argv as run, but at most 4 KiB of it: whole arguments while they fit,
/// then the start of the one that does not, marked as cut.
pub fn capped_argv(argv: &[String]) -> Value {
    let mut kept = Vec::new();
    let mut room = ARGV_AUDIT_CAP;
    for arg in argv {
        if arg.len() <= room {
            room -= arg.len();
            kept.push(arg.clone());
            continue;
        }
        let mut end = room;
        while !arg.is_char_boundary(end) {
            end -= 1;
        }
        kept.push(format!("{}…", &arg[..end]));
        return json!({ "kept": kept, "truncated": true });
    }
    json!(kept)
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

fn bad_request(message: &str) -> Done {
    Done::failed("bad_request", message.to_string(), Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::Exit;
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::AsyncWriteExt;

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
            areas: vec![dir.path().to_path_buf()],
            reads: vec![],
            hidden: vec![],
            state_folders: crate::state_folders::StateFolders::none(),
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
            assert_eq!(done.audit_args["workdir"], json!(dir.path()));
        }
    }

    #[test]
    fn argv_is_audited_up_to_four_kib() {
        let short = vec!["echo".to_string(), "hi".to_string()];
        assert_eq!(capped_argv(&short), json!(["echo", "hi"]));

        let long = vec!["echo".to_string(), "x".repeat(5000)];
        let capped = capped_argv(&long);
        assert_eq!(capped["truncated"], true);
        let kept: usize = capped["kept"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap().trim_end_matches('…').len())
            .sum();
        assert_eq!(kept, ARGV_AUDIT_CAP);
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
