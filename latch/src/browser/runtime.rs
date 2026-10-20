//! A profile runtime as the host keeps it: the one browser worker on a
//! profile (`worker.rs`), started by the first session that needs it and
//! closed once its last session has, and what the host knows about the
//! profile's sessions together: the fill pause, the sites agent code ran on
//! (kept with the profile), the sites each session acted on lately, and
//! site reservations.
//!
//! The worker makes one call at a time, for whichever session sends it, so
//! sessions take turns call by call: one session's long request never holds
//! the browser between its calls.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex as Shared;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::failure::{Exit, Failure};
use crate::wire::Response;

/// The worker's environment: nothing from the host's but a plain `PATH`.
const PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// A session closes after this long without a call; its profile stays.
pub const IDLE: Duration = Duration::from_secs(30 * 60);

/// How long a session's activity on a site counts for `shared_site`.
const SHARED_SITE: Duration = Duration::from_secs(15 * 60);

pub struct Runtime {
    pub profile: String,
    worker: Mutex<Worker>,
    /// What the worker said once the browser was up: engine, version, pid.
    pub ready: Value,
    /// A vault fill holds the browser: every session's call is answered
    /// `session_paused` at once rather than waiting for it.
    pub filling: AtomicBool,
    /// How many fill pauses have begun: a decision made before one began
    /// is not acted on.
    pub pauses: AtomicU32,
    /// The sites agent code (`eval`) has run on in this profile, in any
    /// session: a value filled there later may be seen by what it left.
    agent_code: Shared<BTreeSet<String>>,
    state_dir: PathBuf,
    next_key: AtomicU32,
    /// Sites a session reserved (`--reserve-site`), and its key.
    reserved: Shared<HashMap<String, String>>,
    /// When each session (by id hash) last acted on each site.
    acted: Shared<HashMap<String, HashMap<String, Instant>>>,
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
}

/// A call's answer: the agent's result, the audit's record of it, and a
/// screenshot's bytes.
pub struct Reply {
    pub result: Value,
    pub audit: Value,
    pub bytes: Option<Vec<u8>>,
}

impl Runtime {
    /// Starts the runtime on `profile` and waits until its browser is up;
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
        let mut worker = Worker {
            child,
            stdin,
            stdout,
            scratch,
        };
        let ready = worker.answer().await?;
        Ok(Runtime {
            profile: profile.to_string(),
            worker: Mutex::new(worker),
            ready,
            filling: AtomicBool::new(false),
            pauses: AtomicU32::new(0),
            agent_code: Shared::new(super::profile::agent_code_sites(state_dir, profile)),
            state_dir: state_dir.to_path_buf(),
            next_key: AtomicU32::new(0),
            reserved: Shared::new(HashMap::new()),
            acted: Shared::new(HashMap::new()),
        })
    }

    #[cfg(all(test, target_os = "macos"))]
    pub fn scratch(&self) -> std::path::PathBuf {
        self.worker
            .try_lock()
            .expect("no call under way")
            .scratch
            .path()
            .to_path_buf()
    }

    /// Whether the worker is still running: one whose sessions all sat
    /// idle has closed itself. One busy with a call is running.
    pub fn alive(&self) -> bool {
        match self.worker.try_lock() {
            Ok(mut worker) => matches!(worker.child.try_wait(), Ok(None)),
            Err(_) => true,
        }
    }

    /// A key for a new session, the worker's name for it.
    pub fn new_key(&self) -> String {
        format!("k{}", self.next_key.fetch_add(1, Ordering::SeqCst) + 1)
    }

    /// One call (a `core::Call` as JSON, naming its session), with an
    /// upload's bytes after it. Bytes the worker sends back (a screenshot,
    /// a download) come back as `bytes`.
    pub async fn call(&self, call: Value, input: Option<&[u8]>) -> Result<Reply, Failure> {
        let mut worker = self.worker.lock().await;
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
    }

    /// Closes the browser and waits for the worker to end.
    pub async fn close(&self) -> Result<Value, Failure> {
        let mut worker = self.worker.lock().await;
        worker.send(serde_json::json!({"op": "close"})).await?;
        let closed = worker.answer().await?;
        let _ = worker.child.wait().await;
        Ok(closed["result"].clone())
    }

    /// Agent code ran on `site`: kept with the profile, for every later fill.
    pub fn note_agent_code(&self, site: &str) {
        let mut sites = self.agent_code.lock().expect("agent code lock");
        if sites.insert(site.to_string())
            && let Err(err) =
                super::profile::keep_agent_code_sites(&self.state_dir, &self.profile, &sites)
        {
            crate::log::log(&format!(
                "browser: profile {}: could not keep the sites agent code ran on: {err}",
                self.profile
            ));
        }
    }

    /// Whether agent code ever ran on one of `sites` in this profile.
    pub fn agent_code_on(&self, sites: &[String]) -> bool {
        let ran = self.agent_code.lock().expect("agent code lock");
        sites.iter().any(|site| ran.contains(site))
    }

    /// Reserves `site` for session `key`, unless another session holds it.
    pub fn reserve(&self, site: &str, key: &str) -> bool {
        let mut reserved = self.reserved.lock().expect("reservations lock");
        let holder = reserved
            .entry(site.to_string())
            .or_insert_with(|| key.to_string());
        holder == key
    }

    /// The sites other sessions than `key` hold.
    pub fn reserved_by_others(&self, key: &str) -> Vec<String> {
        let reserved = self.reserved.lock().expect("reservations lock");
        reserved
            .iter()
            .filter(|(_, holder)| *holder != key)
            .map(|(site, _)| site.clone())
            .collect()
    }

    /// Session `key` (id hash `hash`) is gone: its reservations and its
    /// activity no longer count.
    pub fn forget(&self, key: &str, hash: &str) {
        self.reserved
            .lock()
            .expect("reservations lock")
            .retain(|_, holder| holder != key);
        for sessions in self.acted.lock().expect("activity lock").values_mut() {
            sessions.remove(hash);
        }
    }

    /// Session `hash` acted on `site` now. Answers the other sessions that
    /// acted there lately, by id hash, and how many seconds ago.
    pub fn acted(&self, site: &str, hash: &str) -> Vec<(String, u64)> {
        let mut acted = self.acted.lock().expect("activity lock");
        let sessions = acted.entry(site.to_string()).or_default();
        let now = Instant::now();
        sessions.retain(|_, at| now.duration_since(*at) < SHARED_SITE);
        let mut others: Vec<(String, u64)> = sessions
            .iter()
            .filter(|(other, _)| *other != hash)
            .map(|(other, at)| (other.clone(), now.duration_since(*at).as_secs()))
            .collect();
        others.sort();
        sessions.insert(hash.to_string(), now);
        others
    }
}

impl Worker {
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

impl Drop for Worker {
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
