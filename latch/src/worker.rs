//! The worker: a separate, sandboxed process that does a host's file reads
//! and writes, runs `exec`'s programs and calls Google. The host never opens an agent's
//! paths or starts its programs itself.
//!
//! A worker is the latch binary run again as `latch worker …` (hidden from
//! `--help`), with a clean environment. A Google worker gets its selected
//! token alone; exec can receive explicit secrets over its private request
//! pipe. These enter only the final program's environment, after confinement.
//! If confinement fails, the worker stops without starting the program.
//!
//! It talks to the host over its standard streams:
//! - `read PATH`: one line `{"size": N}` on stdout, then the file's N bytes;
//! - `write --size N [--name NAME] PATH`: one line `{"ready": true, "path": …}`
//!   (into PATH/NAME if PATH is a folder) once it can write
//!   there, then it takes N bytes from stdin and only then puts the file in
//!   place (all or nothing, see `write`) and exits 0;
//! - `exec [--workdir DIR] -- ARGV…`: moves into readable DIR and becomes the
//!   program, which inherits the sandbox; from then on
//!   its output and exit are the program's own;
//! - `google PROGRAM`: accepts a JSON job and input bytes, then returns
//!   gog JSON, download metadata and file bytes from its private call folder;
//! - `probe`: confines itself and exits 0, so the host knows at startup
//!   that the sandbox can be installed;
//! - on any failure of the worker itself: a last line
//!   `{"worker_error": {"code": ..., "message": ...}}` on stderr and exit 125
//!   (the code tools like `env` use for "the wrapper failed, not the
//!   program"). A path the sandbox refuses gives the code `sandbox_denied`.

use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};

use crate::cli::{WorkerAction, WorkerArgs};
use crate::sandbox::{self, Policy};

/// What every worker a host starts is allowed: the host's areas and the
/// owner's read-only opt-ins, minus its state directory and the hidden paths.
pub struct WorkerSetup {
    pub home: PathBuf,
    pub areas: Vec<PathBuf>,
    pub reads: Vec<PathBuf>,
    pub hidden: Vec<PathBuf>,
    pub state_dir: PathBuf,
    /// The latch binary each worker runs: the host's own.
    pub program: PathBuf,
}

/// A failure the worker reported on stderr.
#[derive(Deserialize, Debug)]
pub struct WorkerError {
    pub code: String,
    pub message: String,
}

#[derive(Deserialize)]
struct ErrorLine {
    worker_error: WorkerError,
    token: Option<String>,
}

thread_local! {
    static ERROR_TOKEN: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The exit status of a worker that failed before doing its job.
const WORKER_FAILED: u8 = 125;

/// The worker's own error, if `stderr`'s last line is one. System libraries
/// (or, for `exec`, the program) may have printed other lines before it.
pub fn worker_error(stderr: &str) -> Option<WorkerError> {
    let last = stderr.lines().last()?;
    serde_json::from_str::<ErrorLine>(last)
        .ok()
        .map(|line| line.worker_error)
}

/// Only the trusted worker knows this call's token. Program stderr is data.
pub fn authenticated_worker_error(stderr: &str, token: &str) -> Option<WorkerError> {
    let line: ErrorLine = serde_json::from_str(stderr.lines().last()?).ok()?;
    (line.token.as_deref() == Some(token)).then_some(line.worker_error)
}

/// The environment every worker gets: just enough for programs to run.
/// Nothing else from the host's environment is passed on.
/// Homebrew's folders first, as in a macOS user's shell; they do not exist
/// on Linux and cost nothing there.
/// Why file workers refuse on a host that shares no folders.
const NO_FOLDERS: &str = "this host shares no folders; the owner can add some with \
    `latch shares set ~/code` and restart `latch serve`";
const PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// A started worker: its stdout (for the first line and any file bytes)
/// and the process itself.
pub struct Running {
    pub child: Child,
    pub stdout: BufReader<ChildStdout>,
}

impl WorkerSetup {
    /// Starts `latch worker <action> [--size N] <path>` and waits for its first line:
    /// `{"size": N}` for `read`, `{"ready": true}` for `write`. Returns the
    /// running worker and that line, or the worker's error.
    pub async fn start(
        &self,
        action: &str,
        size: Option<u64>,
        name: Option<&str>,
        path: &str,
    ) -> Result<(Running, serde_json::Value), WorkerError> {
        let Some(first_area) = self.areas.first() else {
            return Err(error("no_folders", NO_FOLDERS));
        };
        let mut command = self.command(first_area)?;
        command.arg(action);
        if let Some(size) = size {
            command.arg("--size").arg(size.to_string());
        }
        if let Some(name) = name {
            command.arg("--name").arg(name);
        }
        // `--` so a path starting with `-` is never read as a flag.
        command.arg("--").arg(path);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|err| error("worker_failed", &err.to_string()))?;

        let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        let mut running = Running { child, stdout };
        match running.line().await {
            Ok(first) => Ok((running, first)),
            Err(_) => Err(running.failure().await),
        }
    }

    /// Uses the same launcher, narrowed to one disposable call folder.
    pub async fn google(
        &self,
        program: &Path,
        folder: &Path,
        token: &str,
        job: &crate::google::process::Job,
    ) -> Result<(Running, serde_json::Value), WorkerError> {
        let restricted = WorkerSetup {
            home: self.home.clone(),
            areas: vec![folder.to_path_buf()],
            reads: vec![],
            hidden: self.hidden.clone(),
            state_dir: self.state_dir.clone(),
            program: self.program.clone(),
        };
        let mut command = restricted.command(folder)?;
        command
            .arg("google")
            .arg("--")
            .arg(program)
            .env_clear()
            .env("GOG_ACCESS_TOKEN", token)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|err| error("worker_failed", &err.to_string()))?;
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        let mut running = Running { child, stdout };
        let mut bytes = serde_json::to_vec(job).expect("job serializes");
        bytes.push(b'\n');
        running
            .child
            .stdin
            .as_mut()
            .expect("piped")
            .write_all(&bytes)
            .await
            .map_err(|_| error("worker_failed", "worker input closed"))?;
        match running.line().await {
            Ok(first) => Ok((running, first)),
            Err(_) => Err(running.failure().await),
        }
    }
}

impl WorkerSetup {
    /// Runs a throwaway worker that only confines itself, with the policy a
    /// file worker gets (areas, the state directory denied, no network), and
    /// exits. `Ok` if the sandbox could be installed.
    pub async fn probe(&self) -> Result<(), WorkerError> {
        let mut command = self.command(self.areas.first().map_or(Path::new("/"), |area| area))?;
        command
            .arg("probe")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let output = command
            .output()
            .await
            .map_err(|err| error("worker_failed", &err.to_string()))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(worker_error(&stderr)
            .unwrap_or_else(|| error("sandbox_failed", &sandbox::startup_error(&stderr))))
    }

    /// Starts `latch worker exec [--workdir DIR] -- ARGV…`. The program runs
    /// in a process group of its own, so a timeout can stop it together with
    /// every process it started. It dies with this host, however the host
    /// ends, and the worker kills it `kill_after` seconds in whatever the
    /// host does (see `sandbox::supervise`).
    /// `tmpdir` becomes the program's `TMPDIR`.
    pub fn start_exec(
        &self,
        args: &crate::exec::ExecArgs,
        tmpdir: &Path,
        error_token: &str,
        kill_after: u64,
    ) -> Result<Child, WorkerError> {
        let caches = crate::exec::caches(self);
        let restricted = WorkerSetup {
            home: self.home.clone(),
            areas: args.writes.iter().cloned().chain(caches).collect(),
            reads: vec![self.home.clone()],
            hidden: self.hidden.clone(),
            state_dir: self.state_dir.clone(),
            program: self.program.clone(),
        };
        let mut command = restricted.command(&self.home)?;
        command.arg("--home").arg(&self.home);
        if args.network {
            command.arg("--network");
        }
        command
            .arg("--error-token")
            .arg(error_token)
            .arg("--tmpdir")
            .arg(tmpdir)
            .arg("--kill-after")
            .arg(kill_after.to_string())
            .args(["exec", "--private-request"]);
        #[cfg(target_os = "linux")]
        die_with_host(&mut command);
        if let Some(workdir) = &args.workdir {
            command.arg("--workdir").arg(workdir);
        }
        command.arg("--").args(&args.argv);
        command
            .env(
                "PATH",
                std::env::var_os("PATH").unwrap_or_else(|| PATH.into()),
            )
            .env("TMPDIR", tmpdir)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
            .spawn()
            .map_err(|err| error("worker_failed", &err.to_string()))
    }

    /// `latch worker --area … --read … --hidden … --denied …`, run from the
    /// first area with an empty environment apart from `PATH` and `HOME`.
    fn command(&self, first_area: &Path) -> Result<Command, WorkerError> {
        let mut command = Command::new(&self.program);
        command.arg("worker");
        for area in &self.areas {
            command.arg("--area").arg(area);
        }
        for read in &self.reads {
            command.arg("--read").arg(read);
        }
        for hidden in &self.hidden {
            command.arg("--hidden").arg(hidden);
        }
        command.arg("--denied").arg(&self.state_dir);
        command
            .env_clear()
            .env("PATH", PATH)
            .env("HOME", first_area)
            .current_dir(first_area);
        Ok(command)
    }
}

/// On Linux, the kernel kills the worker the moment this host's thread that
/// started it ends: tokio's worker threads, which start workers, end only
/// with the host. bubblewrap (`--die-with-parent`) then kills the sandbox
/// and the program in it.
#[cfg(target_os = "linux")]
pub fn die_with_host(command: &mut Command) {
    let host = std::process::id() as libc::pid_t;
    // SAFETY: pre_exec is needed to set Linux's parent-death signal in the
    // child. The closure uses only scalar syscalls and allocation-free OS error
    // constructors, with no Rust locks or allocation between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            // The host died before that took hold: we were handed on.
            if libc::getppid() != host {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            // Workers lead their own process group. Cancellation must also
            // stop gog, which may otherwise finish a write after we time out.
            // SAFETY: std's child kill cannot reach descendants. The live worker
            // leads the group we created; killpg takes only scalar arguments.
            unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

impl Running {
    pub async fn line(&mut self) -> Result<serde_json::Value, WorkerError> {
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .await
            .map_err(|_| error("worker_failed", "worker output closed"))?;
        serde_json::from_str(&line).map_err(|_| error("worker_failed", "invalid worker output"))
    }

    /// Waits for the worker to exit. `Ok` if it succeeded, else its error.
    pub async fn finish(mut self) -> Result<(), WorkerError> {
        // Close our end of its stdin, so a `write` worker sees the end.
        drop(self.child.stdin.take());
        match self.child.wait().await {
            Ok(status) if status.success() => Ok(()),
            _ => Err(self.failure().await),
        }
    }

    /// The error the worker printed on stderr before it exited.
    async fn failure(mut self) -> WorkerError {
        let _ = self.child.wait().await;
        let mut text = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_string(&mut text).await;
        }
        worker_error(&text)
            .unwrap_or_else(|| error("worker_failed", "the worker stopped without saying why"))
    }
}

fn error(code: &str, message: &str) -> WorkerError {
    WorkerError {
        code: code.to_string(),
        message: message.to_string(),
    }
}

/// The worker process itself (`latch worker …`). Only the workload is confined;
/// its supervisor stays outside the sandbox.
pub fn run(args: WorkerArgs) -> ExitCode {
    ERROR_TOKEN.with(|token| *token.borrow_mut() = args.error_token.clone());
    let private_request = matches!(
        &args.action,
        WorkerAction::Exec {
            private_request: true,
            ..
        }
    );
    let mut private = None;
    // bubblewrap has already installed the filesystem view on re-entry.
    // Read before seccomp and the inner supervisor so its failures are authenticated.
    #[cfg(target_os = "linux")]
    if private_request && std::env::var_os("LATCH_BWRAP_INNER").is_some() {
        private = match read_private_exec() {
            Ok(job) => Some(job),
            Err(exit) => return exit,
        };
    }
    let policy = Policy {
        areas: args.areas,
        reads: args.reads,
        hidden: args.hidden,
        state_dir: args.denied,
        exec: matches!(args.action, WorkerAction::Exec { .. }),
        home: args.home,
        network: args.network,
        tmpdir: args.tmpdir,
        kill_after: args.kill_after,
        program: match &args.action {
            WorkerAction::Google { program } => Some(program.clone()),
            _ => None,
        },
        break_for_test: args.break_sandbox_for_test,
    };
    if policy.exec {
        let host = match sandbox::watch_host() {
            Ok(host) => host,
            Err(message) => return fail("host_gone", &message),
        };
        if let Err(message) = sandbox::supervise(host, policy.kill_after) {
            return fail("sandbox_failed", &message);
        }
    }
    if let Err(message) = sandbox::confine(&policy) {
        return fail("sandbox_failed", &message);
    }
    let result = match &args.action {
        WorkerAction::Google { program } => {
            return match crate::google::process::run(program) {
                Ok(()) => ExitCode::SUCCESS,
                Err(failure) => fail(&failure.code, &failure.message),
            };
        }
        // Confined, which is all a probe has to show.
        WorkerAction::Probe => return ExitCode::SUCCESS,
        WorkerAction::Read { path } => read(path),
        WorkerAction::Write { size, name, path } => write(path, name.as_deref(), *size),
        WorkerAction::Exec { workdir, argv, .. } => {
            if private_request && private.is_none() {
                private = match read_private_exec() {
                    Ok(job) => Some(job),
                    Err(exit) => return exit,
                };
            }
            return exec(
                &policy
                    .reads
                    .iter()
                    .chain(&policy.areas)
                    .cloned()
                    .collect::<Vec<_>>(),
                workdir.as_deref(),
                argv,
                private.as_ref().map(|job| &job.secret_env),
            );
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err((path, err)) => fail(io_code(&err), &format!("{}: {err}", path.display())),
    }
}

/// Moves into `workdir` and replaces this process with the program. Only
/// returns (with an error) if that fails; otherwise the program's own exit
/// becomes the worker's.
fn exec(
    areas: &[PathBuf],
    workdir: Option<&Path>,
    argv: &[String],
    secrets: Option<&std::collections::BTreeMap<String, String>>,
) -> ExitCode {
    if let Some(dir) = workdir {
        // The real directory, symlinks resolved; it must be inside an area.
        // One the sandbox will not even let us look up (Seatbelt refuses it,
        // Linux's view lacks it) is judged by the nearest folder above it
        // that we can, so every system reports the same code.
        let outside = || {
            let message = format!("{} is not inside the host's areas", dir.display());
            fail("workdir_outside_areas", &message)
        };
        let inside = |real: &Path| areas.iter().any(|area| real.starts_with(area));
        let real = match dir.canonicalize() {
            Ok(real) => real,
            Err(err) => {
                let seen = dir.ancestors().skip(1).find_map(|above| {
                    let above = if above.as_os_str().is_empty() {
                        Path::new(".")
                    } else {
                        above
                    };
                    above.canonicalize().ok()
                });
                if seen.is_some_and(|above| inside(&above)) {
                    return fail(io_code(&err), &format!("{}: {err}", dir.display()));
                }
                return outside();
            }
        };
        if !inside(&real) {
            return outside();
        }
        if let Err(err) = std::env::set_current_dir(&real) {
            return fail(io_code(&err), &format!("{}: {err}", dir.display()));
        }
    }
    let Some((program, args)) = argv.split_first() else {
        return fail("bad_request", "no program to run");
    };
    // `exec` replaces this process; the sandbox stays on the program and on
    // everything it starts.
    let mut command = std::process::Command::new(program);
    command.args(args);
    if let Some(secrets) = secrets {
        command.envs(secrets);
    }
    let err = command.exec();
    let code = if err.kind() == io::ErrorKind::NotFound {
        "program_not_found"
    } else {
        io_code(&err)
    };
    fail(code, &format!("{program}: {err}"))
}

/// Sends the file's size, then its bytes, to stdout.
fn read(path: &Path) -> Result<(), (PathBuf, io::Error)> {
    let failed = |err| (path.to_path_buf(), err);
    let mut file = File::open(path).map_err(failed)?;
    let metadata = file.metadata().map_err(failed)?;
    if metadata.is_dir() {
        return Err(failed(io::Error::new(
            io::ErrorKind::IsADirectory,
            "is a directory",
        )));
    }
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{}", serde_json::json!({ "size": metadata.len() })).map_err(failed)?;
    io::copy(&mut file, &mut stdout).map_err(failed)?;
    stdout.flush().map_err(failed)
}

/// Writes `size` bytes from stdin to `path`, all or nothing. The bytes go
/// into a temporary file beside the target (so the sandbox checks the same
/// directory), which is renamed over the target only once all of them have
/// arrived and reached the disk. A transfer that breaks off leaves the old
/// file, or no file, exactly as it was.
///
/// If `path` is a folder, the file goes inside it under `name` (the client's
/// file name), as `cp` does. The path it lands at is in the "ready" line.
fn write(path: &Path, name: Option<&str>, size: u64) -> Result<(), (PathBuf, io::Error)> {
    let inside;
    let path = match name {
        // Only a plain name: never `..` or a path of its own.
        Some(name) if path.is_dir() && Path::new(name).file_name() == Some(name.as_ref()) => {
            inside = path.join(name);
            inside.as_path()
        }
        _ => path,
    };
    let failed = |err| (path.to_path_buf(), err);
    let temp = temp_path(path).map_err(failed)?;
    // `create_new` never reuses (or follows a link at) an existing name.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(failed)?;
    let result = fill(&mut file, path, size).and_then(|()| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(failed)
}

/// Says "ready", copies stdin into `file`, and checks exactly `size` bytes
/// came before stdin ended.
fn fill(file: &mut File, path: &Path, size: u64) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    let ready = serde_json::json!({ "ready": true, "path": path });
    writeln!(stdout, "{ready}")?;
    stdout.flush()?;
    let written = io::copy(&mut io::stdin().lock(), file)?;
    if written != size {
        let message = format!("expected {size} bytes, got {written}");
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, message));
    }
    // Make sure the bytes are on disk before the file takes the target's place.
    file.sync_all()
}

/// `dir/name` becomes `dir/.name.latch-<process id>.part`.
fn temp_path(path: &Path) -> io::Result<PathBuf> {
    let Some(name) = path.file_name() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no file name"));
    };
    let temp = format!(
        ".{}.latch-{}.part",
        name.to_string_lossy(),
        std::process::id()
    );
    Ok(path.with_file_name(temp))
}

/// The error code for an I/O failure.
fn io_code(err: &io::Error) -> &'static str {
    match err.kind() {
        // Linux shows paths outside the areas read-only.
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => "sandbox_denied",
        io::ErrorKind::NotFound => "not_found",
        io::ErrorKind::IsADirectory => "is_a_directory",
        io::ErrorKind::UnexpectedEof => "transfer_incomplete",
        _ => "io_error",
    }
}

fn fail(code: &str, message: &str) -> ExitCode {
    let token = ERROR_TOKEN.with(|token| token.borrow().clone());
    let line =
        serde_json::json!({ "worker_error": { "code": code, "message": message }, "token": token });
    eprintln!("{line}");
    ExitCode::from(WORKER_FAILED)
}

fn read_private_exec() -> Result<crate::exec::PrivateExec, ExitCode> {
    // Stdin buffers reads; a plain duplicated file leaves the program's
    // bytes in the pipe when exec replaces this process.
    let fd = io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .map_err(|_| fail("bad_request", "private exec input is unavailable"))?;
    let job: crate::exec::PrivateExec = crate::wire::read_frame_sync(&mut File::from(fd))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or_else(|| fail("bad_request", "invalid private exec request"))?;
    ERROR_TOKEN.with(|token| *token.borrow_mut() = Some(job.error_token.clone()));
    crate::exec::validate_secret_env(&job.secret_env).map_err(|_| {
        fail(
            "bad_secret_env",
            "invalid or reserved secret environment name",
        )
    })?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn dropping_a_google_worker_stops_its_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");
        let mut child = Command::new("/bin/sh")
            .args([
                "-c",
                "(sleep 0.2; echo survived > \"$1\") & echo ready; wait",
                "sh",
            ])
            .arg(&marker)
            .process_group(0)
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut running = Running { child, stdout };
        let mut ready = String::new();
        running.stdout.read_line(&mut ready).await.unwrap();
        assert_eq!(ready, "ready\n");
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(30), running.finish()).await;
        assert!(
            result.is_err(),
            "the worker was still running at its deadline"
        );
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert!(
            !marker.exists(),
            "gog must not survive its worker being cancelled"
        );
    }
}
