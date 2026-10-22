//! The worker: a separate, sandboxed process that does a host's file reads
//! and writes, runs `exec`'s programs and calls Google. The host never opens an agent's
//! paths or starts its programs itself.
//!
//! A worker is the latch binary run again as `latch worker …` (hidden from
//! `--help`), with a clean environment. A Google worker gets its selected
//! token alone; other workers receive no credentials. Its
//! first act is to confine itself (`sandbox.rs`); if that fails, it stops.
//!
//! It talks to the host over its standard streams:
//! - `read PATH`: one line `{"size": N}` on stdout, then the file's N bytes;
//! - `write --size N [--name NAME] PATH`: one line `{"ready": true, "path": …}`
//!   (into PATH/NAME if PATH is a folder) once it can write
//!   there, then it takes N bytes from stdin and only then puts the file in
//!   place (all or nothing, see `write`) and exits 0;
//! - `exec [--workdir DIR] -- ARGV…`: moves into DIR (which must be inside an
//!   area) and becomes the program, which inherits the sandbox; from then on
//!   its output and exit are the program's own;
//! - `google PROGRAM`: accepts a JSON job and input bytes, then returns
//!   gog JSON, download metadata and file bytes from its private call folder;
//! - `probe`: confines itself and exits 0, so the host knows at startup
//!   that the sandbox can be installed;
//! - `access PATH` (macOS): one line saying whether PATH can be listed,
//!   that is whether the host has Full Disk Access for it;
//! - on any failure of the worker itself: a last line
//!   `{"worker_error": {"code": ..., "message": ...}}` on stderr and exit 125
//!   (the code tools like `env` use for "the wrapper failed, not the
//!   program"). A path the sandbox refuses gives the code `sandbox_denied`.

use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};

use crate::cli::{WorkerAction, WorkerArgs};
use crate::sandbox::{self, Policy};

/// What every worker a host starts is allowed: the host's areas, minus its
/// state directory.
pub struct WorkerSetup {
    pub areas: Vec<PathBuf>,
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

/// The environment every worker gets: just enough for programs to run.
/// Nothing else from the host's environment is passed on.
/// Homebrew's folders first, as in a macOS user's shell; they do not exist
/// on Linux and cost nothing there.
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
            return Err(error("no_areas", "this host shares no folders"));
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
            areas: vec![folder.to_path_buf()],
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

    /// Runs `latch worker access -- FOLDER` in a worker with no areas: it
    /// writes nowhere, and reads what a file worker may.
    #[cfg(target_os = "macos")]
    pub async fn access(
        &self,
        folder: &Path,
    ) -> Result<crate::messages::access::Access, WorkerError> {
        let restricted = WorkerSetup {
            areas: Vec::new(),
            state_dir: self.state_dir.clone(),
            program: self.program.clone(),
        };
        let mut command = restricted.command(Path::new("/"))?;
        command
            .arg("access")
            .arg("--")
            .arg(folder)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let output = command
            .output()
            .await
            .map_err(|err| error("worker_failed", &err.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(worker_error(&stderr).unwrap_or_else(|| {
                error("worker_failed", "the worker stopped without saying why")
            }));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|_| error("worker_failed", "invalid worker output"))
    }

    /// Starts `latch worker exec [--workdir DIR] -- ARGV…`. The program runs
    /// in a process group of its own, so a timeout can stop it together with
    /// every process it started.
    /// `tmpdir` becomes the program's `TMPDIR`.
    pub fn start_exec(
        &self,
        workdir: Option<&str>,
        tmpdir: &Path,
        argv: &[String],
        with_stdin: bool,
    ) -> Result<Child, WorkerError> {
        let Some(first_area) = self.areas.first() else {
            return Err(error("no_areas", "this host shares no folders"));
        };
        let mut command = self.command(first_area)?;
        command.arg("--tmpdir").arg(tmpdir).arg("exec");
        if let Some(workdir) = workdir {
            command.arg("--workdir").arg(workdir);
        }
        command.arg("--").args(argv);
        command
            .env("TMPDIR", tmpdir)
            .process_group(0)
            // Without input, the program's stdin is empty (`/dev/null`).
            .stdin(if with_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
            .spawn()
            .map_err(|err| error("worker_failed", &err.to_string()))
    }

    /// `latch worker --area … --denied …`, run from the first area with an
    /// empty environment apart from `PATH` and `HOME`.
    fn command(&self, first_area: &Path) -> Result<Command, WorkerError> {
        let mut command = Command::new(&self.program);
        command.arg("worker");
        for area in &self.areas {
            command.arg("--area").arg(area);
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

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            // Workers lead their own process group. Cancellation must also
            // stop gog, which may otherwise finish a write after we time out.
            // SAFETY: killpg sends a signal and touches no Rust memory.
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

/// The worker process itself (`latch worker …`). Confines itself first.
pub fn run(args: WorkerArgs) -> ExitCode {
    let policy = Policy {
        areas: args.areas,
        state_dir: args.denied,
        exec: matches!(args.action, WorkerAction::Exec { .. }),
        tmpdir: args.tmpdir,
        program: match &args.action {
            WorkerAction::Google { program } => Some(program.clone()),
            _ => None,
        },
        break_for_test: args.break_sandbox_for_test,
    };
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
        #[cfg(target_os = "macos")]
        WorkerAction::Access { path } => {
            let access = crate::messages::access::probe(path);
            println!(
                "{}",
                serde_json::to_string(&access).expect("access serializes")
            );
            return ExitCode::SUCCESS;
        }
        WorkerAction::Read { path } => read(path),
        WorkerAction::Write { size, name, path } => write(path, name.as_deref(), *size),
        WorkerAction::Exec { workdir, argv } => {
            return exec(&policy.areas, workdir.as_deref(), argv);
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
fn exec(areas: &[PathBuf], workdir: Option<&Path>, argv: &[String]) -> ExitCode {
    if let Some(dir) = workdir {
        // The real directory, symlinks resolved; it must be inside an area.
        // A directory the sandbox will not even let us look up (as Seatbelt
        // does for most paths outside the areas) is outside the areas too,
        // so every system reports the same code.
        let outside = || {
            let message = format!("{} is not inside the host's areas", dir.display());
            fail("workdir_outside_areas", &message)
        };
        let real = match dir.canonicalize() {
            Ok(real) => real,
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => return outside(),
            Err(err) => return fail(io_code(&err), &format!("{}: {err}", dir.display())),
        };
        if !areas.iter().any(|area| real.starts_with(area)) {
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
    let err = std::process::Command::new(program).args(args).exec();
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
    let line = serde_json::json!({ "worker_error": { "code": code, "message": message } });
    eprintln!("{line}");
    ExitCode::from(WORKER_FAILED)
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
