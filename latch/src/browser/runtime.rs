//! A session runtime as the host keeps it: the browser worker (`worker.rs`)
//! one session runs on, started on a profile of its own (`profile.rs`) when
//! the session opens, and what the host knows of it: the fill pause and the
//! sites agent code ran on. The profile is removed when the runtime ends.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Stdio;
use std::sync::Mutex as Shared;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use super::core::{self, Call};
use crate::failure::{Failure, code};
use crate::wire::Response;
use crate::worker::OwnGroup;

/// The worker's environment: nothing from the host's but a plain `PATH`.
const PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// A session closes after this long without a call.
pub const IDLE: Duration = Duration::from_secs(30 * 60);

/// How long past its own limit (`Call::limit`) a call's answer may take:
/// the worker ends its waits by the limit, and answers soon after.
const ANSWER_GRACE: Duration = Duration::from_secs(30);

/// How long the whole of `call` may take: its limit, and the grace.
fn within(call: &Value) -> Duration {
    Call::deserialize(call).map_or(core::CALL, |call| call.limit(core::CALL)) + ANSWER_GRACE
}

pub struct Runtime {
    worker: Mutex<Worker>,
    /// What the worker said once the browser was up: engine, version, pid.
    pub ready: Value,
    /// A vault fill holds the browser: every call is answered
    /// `session_paused` at once rather than waiting for it.
    pub filling: AtomicBool,
    /// How many fill pauses have begun: a decision made before one began
    /// is not acted on.
    pub pauses: AtomicU32,
    /// The sites agent code (`eval`) has run on in this runtime: a value
    /// filled there later may be seen by what it left.
    agent_code: Shared<BTreeSet<String>>,
    /// How long a call (its line, upload, answer and bytes) may take.
    within: fn(&Value) -> Duration,
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// The runtime's scratch folder: the engine's home, temp and crash
    /// folders, staged downloads and a folder per session. Removed when the
    /// runtime ends, by its `Drop`.
    #[cfg_attr(not(all(test, target_os = "macos")), allow(dead_code))]
    scratch: TempDir,
    /// The session's profile, removed after the worker is stopped.
    #[cfg_attr(not(all(test, target_os = "macos")), allow(dead_code))]
    profile: TempDir,
}

/// A call's answer: the agent's result, the audit's record of it, and a
/// screenshot's bytes.
pub struct Reply {
    pub result: Value,
    pub audit: Value,
    pub bytes: Option<Vec<u8>>,
}

impl Runtime {
    /// Starts the runtime on `engine` (`cft` or `camoufox`), on a new
    /// profile, and waits until its browser is up; with `headless`, its
    /// engine has no window.
    pub async fn start(state_dir: &Path, engine: &str, headless: bool) -> Result<Self, Failure> {
        let latch = std::env::current_exe()
            .map_err(|err| Failure::new(code::engine_failed, err.to_string()))?;
        Self::start_with(&latch, state_dir, engine, IDLE, headless).await
    }

    /// `start`, with `latch` as the latch binary to run the worker.
    pub async fn start_with(
        latch: &Path,
        state_dir: &Path,
        engine: &str,
        idle: Duration,
        headless: bool,
    ) -> Result<Self, Failure> {
        let failed = |err: std::io::Error| Failure::new(code::engine_failed, err.to_string());
        let scratch = tempfile::Builder::new()
            .prefix("latch-browser-")
            .tempdir()
            .map_err(failed)?;
        let profile = super::profile::make(state_dir).map_err(failed)?;
        let mut command = Command::new(latch);
        command
            .arg("--state-dir")
            .arg(state_dir)
            .arg("browser-worker")
            .arg("--engine")
            .arg(engine)
            .arg("--profile")
            .arg(profile.path())
            .arg("--scratch")
            .arg(scratch.path())
            .arg("--idle-timeout")
            .arg(idle.as_secs().to_string());
        if headless {
            command.arg("--headless");
        }
        if super::egress::local_allowed(state_dir)? {
            command.arg("--allow-local-browsing");
        }
        #[cfg(unix)]
        if super::display::for_runtime(headless) {
            command.arg("--private-display");
        }
        if crate::sandbox::page_isolation_off() {
            command.arg("--page-isolation-off");
        }
        // The worker dies with this host: on Linux the kernel kills it (and
        // bubblewrap, which it becomes, everything inside); on macOS it
        // watches the host itself (see `browser::worker`).
        #[cfg(target_os = "linux")]
        crate::worker::die_with_host(&mut command);
        let mut child = command
            .env_clear()
            .env("PATH", PATH)
            .current_dir(scratch.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .own_group()
            .kill_on_drop(true)
            .spawn()
            .map_err(failed)?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        let mut worker = Worker {
            child,
            stdin,
            stdout,
            scratch,
            profile,
        };
        let ready = worker.answer().await?;
        Ok(Runtime {
            worker: Mutex::new(worker),
            ready,
            filling: AtomicBool::new(false),
            pauses: AtomicU32::new(0),
            agent_code: Shared::default(),
            within,
        })
    }

    #[cfg(all(test, any(target_os = "macos", feature = "linux-browser-tests")))]
    pub fn scratch(&self) -> std::path::PathBuf {
        self.worker
            .try_lock()
            .expect("no call under way")
            .scratch
            .path()
            .to_path_buf()
    }

    #[cfg(all(test, any(target_os = "macos", feature = "linux-browser-tests")))]
    pub fn profile(&self) -> std::path::PathBuf {
        self.worker
            .try_lock()
            .expect("no call under way")
            .profile
            .path()
            .to_path_buf()
    }

    /// Whether the worker is still running: one whose session sat idle has
    /// closed itself. One busy with a call is running.
    pub fn alive(&self) -> bool {
        match self.worker.try_lock() {
            Ok(mut worker) => matches!(worker.child.try_wait(), Ok(None)),
            Err(_) => true,
        }
    }

    /// One call (a `core::Call` as JSON), with an
    /// upload's bytes after it. Bytes the worker sends back (a screenshot,
    /// a download) come back as `bytes`.
    /// A worker that has not finished the call well past its limit is
    /// stuck, and is stopped.
    pub async fn call(&self, call: Value, input: Option<&[u8]>) -> Result<Reply, Failure> {
        let within = (self.within)(&call);
        let mut worker = self.worker.lock().await;
        // The whole exchange: a worker that stops reading its upload or
        // sending its bytes is as stuck as one that never answers.
        let exchange = async {
            worker.send(call).await?;
            if let Some(input) = input {
                worker.stdin.write_all(input).await.map_err(|_| lost())?;
            }
            let answer = worker.answer().await?;
            let mut reply = Reply {
                result: answer["result"].clone(),
                audit: answer["audit"].clone(),
                bytes: None,
            };
            if let Some(size) = answer["bytes"].as_u64() {
                let mut bytes = vec![0; size as usize];
                worker
                    .stdout
                    .read_exact(&mut bytes)
                    .await
                    .map_err(|_| lost())?;
                reply.bytes = Some(bytes);
            }
            Ok(reply)
        };
        match tokio::time::timeout(within, exchange).await {
            Ok(reply) => reply,
            Err(_) => {
                worker.stop();
                let _ = worker.child.wait().await;
                Err(Failure::new(
                    code::browser_gone,
                    format!(
                        "the browser did not finish a call within {} s, and was stopped",
                        within.as_secs()
                    ),
                ))
            }
        }
    }

    /// Closes the browser and waits for the worker to end.
    pub async fn close(&self) -> Result<Value, Failure> {
        let mut worker = self.worker.lock().await;
        worker.send(serde_json::json!({"op": "close"})).await?;
        let closed = worker.answer().await?;
        let _ = worker.child.wait().await;
        Ok(closed["result"].clone())
    }

    /// Agent code ran on `site`, for every later fill in this runtime.
    pub fn note_agent_code(&self, site: &str) {
        self.agent_code
            .lock()
            .expect("agent code lock")
            .insert(site.to_string());
    }

    /// Whether agent code ran on one of `sites` in this runtime.
    pub fn agent_code_on(&self, sites: &[String]) -> bool {
        let ran = self.agent_code.lock().expect("agent code lock");
        sites.iter().any(|site| ran.contains(site))
    }
}

impl Worker {
    fn stop(&mut self) {
        #[cfg(windows)]
        let _ = self.child.start_kill();
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            // The worker leads its own group; the browser, in a group of its
            // own, exits when the worker's end of its pipe closes.
            // SAFETY: killpg sends a signal and touches no Rust memory.
            unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
        }
    }

    async fn send(&mut self, call: Value) -> Result<(), Failure> {
        let mut line = serde_json::to_vec(&call).expect("JSON");
        line.push(b'\n');
        self.stdin.write_all(&line).await.map_err(|_| lost())
    }

    async fn answer(&mut self) -> Result<Value, Failure> {
        let mut line = String::new();
        let read = self.stdout.read_line(&mut line).await.map_err(|_| lost())?;
        if read == 0 {
            return Err(lost());
        }
        serde_json::from_str::<Response>(&line)
            .map_err(|_| lost())?
            .into_result()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn lost() -> Failure {
    Failure::new(code::browser_gone, "the browser worker stopped".into())
}

// Their workers are sh scripts: Unix only.
#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    use serde_json::json;

    use super::*;

    /// A runtime whose worker is `script`, a shell script that has
    /// answered its first line; each call may take 300 ms.
    async fn stuck(script: &str) -> (TempDir, Runtime) {
        let dir = tempfile::tempdir().unwrap();
        let worker = dir.path().join("worker");
        let ready = r#"echo '{"ok": {"pid": 1}}'"#;
        std::fs::write(&worker, format!("#!/bin/sh\n{ready}\n{script}\n")).unwrap();
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut runtime = Runtime::start_with(&worker, dir.path(), "cft", IDLE, true)
            .await
            .unwrap();
        runtime.within = |_| Duration::from_millis(300);
        (dir, runtime)
    }

    async fn stopped_in_time(runtime: &Runtime, input: Option<&[u8]>) {
        let started = Instant::now();
        let call = runtime.call(json!({"op": "upload"}), input);
        let failure = tokio::time::timeout(Duration::from_secs(5), call)
            .await
            .expect("the call ended by its limit")
            .err()
            .unwrap();
        assert_eq!(failure.code, "browser_gone");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!runtime.alive());
    }

    #[tokio::test]
    async fn a_worker_that_takes_no_upload_is_stopped_by_the_calls_limit() {
        // It never reads: the upload fills the pipe and the write waits.
        let (_dir, runtime) = stuck("sleep 600").await;
        stopped_in_time(&runtime, Some(&vec![0; 4 << 20])).await;
    }

    #[tokio::test]
    async fn a_worker_that_never_sends_its_bytes_is_stopped_by_the_calls_limit() {
        // It answers with bytes to come, and sends none.
        let (_dir, runtime) =
            stuck(r#"echo '{"ok": {"result": {}, "audit": {}, "bytes": 100}}'; sleep 600"#).await;
        stopped_in_time(&runtime, None).await;
    }

    #[tokio::test]
    async fn a_worker_that_never_answers_is_stopped_by_the_calls_limit() {
        let (_dir, runtime) = stuck("sleep 600").await;
        stopped_in_time(&runtime, None).await;
    }
}
