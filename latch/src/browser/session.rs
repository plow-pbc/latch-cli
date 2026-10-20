//! The host's side of a browser session: it starts the session's browser
//! worker (`worker.rs`) and makes one call at a time over its stdio.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::failure::{Exit, Failure};
use crate::wire::Response;

/// The worker's environment: nothing from the host's but a plain `PATH`.
const PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// A session closes after this long without a call; its profile stays.
pub const IDLE: Duration = Duration::from_secs(30 * 60);

pub struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// The session's scratch folder: the engine's home, temp and crash
    /// folders. Removed when the session ends, by its `Drop`.
    #[allow(dead_code)]
    scratch: TempDir,
    /// What the worker said once the browser was up: engine, version, pid.
    pub ready: Value,
}

impl Session {
    /// Starts a session on `profile` and waits until its browser is up;
    /// with `headless`, its engine has no window.
    pub async fn start(state_dir: &Path, profile: &str, headless: bool) -> Result<Self, Failure> {
        let latch = std::env::current_exe()
            .map_err(|err| Failure::failed("engine_failed", err.to_string()))?;
        Self::start_with(&latch, state_dir, profile, IDLE, headless).await
    }

    /// `start`, with `latch` as the latch binary to run the worker.
    pub async fn start_with(
        latch: &Path,
        state_dir: &Path,
        profile: &str,
        idle: Duration,
        headless: bool,
    ) -> Result<Self, Failure> {
        let scratch = tempfile::Builder::new()
            .prefix("latch-browser-")
            .tempdir()
            .map_err(|err| Failure::failed("engine_failed", err.to_string()))?;
        let mut command = Command::new(latch);
        command
            .arg("--state-dir")
            .arg(state_dir)
            .arg("browser-worker")
            .arg("--profile")
            .arg(profile)
            .arg("--scratch")
            .arg(scratch.path())
            .arg("--idle-timeout")
            .arg(idle.as_secs().to_string());
        if headless {
            command.arg("--headless");
        }
        let mut child = command
            .env_clear()
            .env("PATH", PATH)
            .current_dir(scratch.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| Failure::failed("engine_failed", err.to_string()))?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        let mut session = Session {
            child,
            stdin,
            stdout,
            scratch,
            ready: Value::Null,
        };
        session.ready = session.answer().await?;
        Ok(session)
    }

    #[cfg(all(test, target_os = "macos"))]
    pub fn scratch(&self) -> &Path {
        self.scratch.path()
    }

    /// Whether the worker is still running: a session that sat idle for
    /// too long has closed itself.
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// One call (a `core::Call` as JSON), with an upload's bytes after it.
    /// Bytes the worker sends back (a screenshot, a download) come back as
    /// `bytes`.
    pub async fn call_with(&mut self, call: Value, input: Option<&[u8]>) -> Result<Reply, Failure> {
        self.send(call).await?;
        if let Some(input) = input {
            self.stdin.write_all(input).await.map_err(|_| lost())?;
        }
        let answer = self.answer().await?;
        let mut reply = Reply {
            result: answer["result"].clone(),
            audit: answer["audit"].clone(),
            bytes: None,
        };
        if let Some(size) = answer["bytes"].as_u64() {
            let mut bytes = vec![0; size as usize];
            self.stdout
                .read_exact(&mut bytes)
                .await
                .map_err(|_| lost())?;
            reply.bytes = Some(bytes);
        }
        Ok(reply)
    }

    pub async fn call(&mut self, call: Value) -> Result<Reply, Failure> {
        self.call_with(call, None).await
    }

    /// Closes the browser and waits for the worker to end.
    #[cfg(all(test, target_os = "macos"))]
    pub async fn close(mut self) -> Result<Value, Failure> {
        self.send(serde_json::json!({"op": "close"})).await?;
        let closed = self.answer().await?;
        let _ = self.child.wait().await;
        Ok(closed["result"].clone())
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
        match serde_json::from_str(&line).map_err(|_| lost())? {
            Response::Ok(value) => Ok(value),
            Response::Error {
                exit,
                code,
                message,
                details,
            } => {
                let mut failure = Failure::new(exit, &code, message);
                failure.details = details;
                Err(failure)
            }
        }
    }
}

/// A call's answer: the agent's result, the audit's record of it, and a
/// screenshot's bytes.
pub struct Reply {
    pub result: Value,
    pub audit: Value,
    pub bytes: Option<Vec<u8>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            // The worker leads its own group; the browser, in a group of its
            // own, exits when the worker's end of its pipe closes.
            // SAFETY: killpg sends a signal and touches no Rust memory.
            unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

fn lost() -> Failure {
    Failure::new(
        Exit::Failed,
        "browser_gone",
        "the browser worker stopped".into(),
    )
}
