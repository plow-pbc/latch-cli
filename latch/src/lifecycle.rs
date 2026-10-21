//! What the host on a state folder is doing, for other latch processes on
//! this machine (`status --local`, `events`): `latch serve` writes each step
//! of its life to `serve.json`, and whether it still runs is whether it
//! still holds `serve.lock`. A serve that was killed or crashed never wrote
//! its last step; its released lock says so.

use std::fs::{self, File};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::failure::Failure;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Locked the state folder; not reachable yet.
    Starting,
    /// Online and accepting callers.
    Serving,
    /// Stopped cleanly, or never started.
    Stopped,
    /// Stopped by a failure, or ended without stopping.
    Error,
}

/// A failure as other processes see it: code and message only.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Problem {
    pub code: String,
    pub message: String,
}

/// `serve.json`: the step a serve last wrote.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct Written {
    state: Phase,
    pid: u32,
    /// This serve's run, unique per start: a later run is another host.
    run: String,
    since: String,
    #[serde(default)]
    error: Option<Problem>,
}

/// The host now: its step, and, while one runs or last ran, its process and run.
#[derive(Clone, Debug, PartialEq)]
pub struct Host {
    /// A `latch serve` holds the state folder.
    pub running: bool,
    pub state: Phase,
    pub pid: Option<u32>,
    pub run: Option<String>,
    pub error: Option<Problem>,
}

/// This process's run, set when it starts serving.
static RUN: OnceLock<String> = OnceLock::new();

/// Records `phase` for this serve; `error` with `Phase::Error`. Written
/// atomically (renamed into place) so a reader sees one step or the next.
pub fn record(state_dir: &Path, phase: Phase, error: Option<&Failure>) -> Result<(), Failure> {
    let run = RUN.get_or_init(|| {
        let started = jiff::Timestamp::now().as_millisecond();
        format!("{}-{started}", std::process::id())
    });
    let written = Written {
        state: phase,
        pid: std::process::id(),
        run: run.clone(),
        since: jiff::Timestamp::now().to_string(),
        error: error.map(|failure| Problem {
            code: failure.code.clone(),
            message: failure.message.clone(),
        }),
    };
    let path = state_dir.join("serve.json");
    let temp = state_dir.join("serve.json.tmp");
    let io =
        |err: std::io::Error| Failure::failed("state_io", format!("{}: {err}", path.display()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)
        .map_err(io)?;
    let json = serde_json::to_string(&written).expect("a step serializes") + "\n";
    file.write_all(json.as_bytes()).map_err(io)?;
    fs::rename(&temp, &path).map_err(io)
}

/// What the host on `state_dir` is doing now. Reads only; works on a folder
/// that never served, or does not exist.
pub fn now(state_dir: &Path) -> Host {
    let running = holds_lock(state_dir);
    let written = fs::read(state_dir.join("serve.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Written>(&bytes).ok());
    let Some(written) = written else {
        let state = if running {
            Phase::Starting
        } else {
            Phase::Stopped
        };
        return Host {
            running,
            state,
            pid: None,
            run: None,
            error: None,
        };
    };
    let state = match (running, written.state) {
        // Locked by a serve that has not written its first step yet: what
        // is there is an earlier run's.
        (true, _) if lock_holder(state_dir) != Some(written.pid) => Phase::Starting,
        (true, phase) => phase,
        (false, Phase::Starting | Phase::Serving) => Phase::Error,
        (false, phase) => phase,
    };
    let error = match (state, written.error) {
        (Phase::Error, Some(problem)) => Some(problem),
        (Phase::Error, None) => Some(Problem {
            code: "serve_ended".to_string(),
            message: "latch serve ended without stopping: it was killed or crashed".to_string(),
        }),
        _ => None,
    };
    Host {
        running,
        state,
        pid: running.then_some(written.pid),
        run: Some(written.run),
        error,
    }
}

/// The pid `serve.lock` names (`latch serve (pid N, …)`).
fn lock_holder(state_dir: &Path) -> Option<u32> {
    let text = fs::read_to_string(state_dir.join("serve.lock")).ok()?;
    let after = text.split_once("(pid ")?.1;
    after.split([',', ')']).next()?.trim().parse().ok()
}

/// Whether a `latch serve` holds this folder's lock. The probe takes a
/// shared lock for an instant, so a serve starting at that very moment
/// waits for it (see `serve::lock_state`) rather than refusing.
pub fn holds_lock(state_dir: &Path) -> bool {
    let Ok(file) = File::open(state_dir.join("serve.lock")) else {
        return false;
    };
    // SAFETY: flock only reads the descriptor, which `file` keeps open.
    let free = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0;
    // Closing the file releases the probe's lock, if it got one.
    !free
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_serve_that_holds_its_lock_is_what_it_wrote_and_one_that_let_go_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let stopped = Host {
            running: false,
            state: Phase::Stopped,
            pid: None,
            run: None,
            error: None,
        };
        assert_eq!(now(dir.path()), stopped);
        assert_eq!(now(&dir.path().join("missing")), stopped);

        let mut lock = File::create(dir.path().join("serve.lock")).unwrap();
        write!(lock, "latch serve (pid {}, /bin/latch)", std::process::id()).unwrap();
        // SAFETY: as in `holds_lock`.
        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
        assert_eq!(now(dir.path()).state, Phase::Starting);
        record(dir.path(), Phase::Serving, None).unwrap();
        let serving = now(dir.path());
        assert_eq!(serving.state, Phase::Serving);
        assert_eq!(serving.pid, Some(std::process::id()));

        // Let go without a last step: as if killed.
        drop(lock);
        let ended = now(dir.path());
        assert_eq!(ended.state, Phase::Error);
        assert_eq!(ended.error.unwrap().code, "serve_ended");
        assert_eq!(ended.run, serving.run);

        let failure = Failure::failed("relay_unreachable", "no relay".to_string());
        record(dir.path(), Phase::Error, Some(&failure)).unwrap();
        assert_eq!(now(dir.path()).error.unwrap().code, "relay_unreachable");
        record(dir.path(), Phase::Stopped, None).unwrap();
        assert_eq!(now(dir.path()).state, Phase::Stopped);
        assert_eq!(now(dir.path()).error, None);
    }
}
