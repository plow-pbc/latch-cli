//! What the host on a state folder is doing, for other latch processes on
//! this machine (`status --local`, `events`): `latch serve` writes each step
//! of its life to `serve.json`, and whether it still runs is whether it
//! still holds `serve.lock`. A serve that was killed or crashed never wrote
//! its last step; its released lock says so. With no host running, an
//! offline `latch plugins enable/disable` holds the lock while it changes
//! the connectors (`CONFIGURING`), and `latch logout` while it deletes the
//! folder's secrets (`LOGGING_OUT`): that is no host, only a busy folder.

use std::fs::{self, File};
use std::path::Path;
use std::sync::{OnceLock, mpsc};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::failure::{Failure, code};
use crate::state::write_private;

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
    /// Held by the `latch serve` in a terminal that started it.
    #[serde(default)]
    attached: bool,
}

/// How `serve.lock` begins while a `latch plugins` command holds it.
pub const CONFIGURING: &str = "latch plugins ";
/// How `serve.lock` begins while `latch logout` holds it.
pub const LOGGING_OUT: &str = "latch logout";

/// The host now: its step, and, while one runs or last ran, its process and
/// run. A serve that has just locked the folder has no run yet.
#[derive(Clone, Debug, PartialEq)]
pub struct Host {
    /// A `latch serve` holds the state folder.
    pub running: bool,
    /// No host does: a `latch plugins` command holds it for a moment.
    pub configuring: bool,
    pub state: Phase,
    pub pid: Option<u32>,
    pub run: Option<String>,
    pub error: Option<Problem>,
    /// It runs held by the `latch serve` in a terminal that started it.
    pub attached: bool,
}

/// This process's run, set when it starts serving.
static RUN: OnceLock<String> = OnceLock::new();

/// This process's run, once it has recorded its first step.
pub fn this_run() -> Option<&'static str> {
    RUN.get().map(String::as_str)
}

/// Records `phase` for this serve, `attached` or not; `error` with
/// `Phase::Error`. Written atomically (renamed into place) so a reader sees
/// one step or the next.
pub fn record(
    state_dir: &Path,
    phase: Phase,
    attached: bool,
    error: Option<&Failure>,
) -> Result<(), Failure> {
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
        attached,
    };
    let path = state_dir.join("serve.json");
    let json = serde_json::to_string(&written).expect("a step serializes") + "\n";
    write_private(&path, json.as_bytes())
        .map_err(|err| Failure::new(code::state_io, format!("{}: {err}", path.display())))
}

/// What the host on `state_dir` is doing now. Reads only; works on a folder
/// that never served, or does not exist.
pub fn now(state_dir: &Path) -> Host {
    let held = holds_lock(state_dir);
    let configuring = held
        && fs::read_to_string(state_dir.join("serve.lock"))
            .is_ok_and(|holder| holder.starts_with(CONFIGURING) || holder.starts_with(LOGGING_OUT));
    let running = held && !configuring;
    let written = fs::read(state_dir.join("serve.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Written>(&bytes).ok());
    let holder = lock_holder(state_dir);
    // Locked by a serve that has not written its first step yet: what is
    // there is an earlier run's, and this run's id is not known yet.
    let written = written.filter(|written| !running || holder == Some(written.pid));
    let Some(written) = written else {
        let state = if running {
            Phase::Starting
        } else {
            Phase::Stopped
        };
        return Host {
            running,
            configuring,
            state,
            pid: if running { holder } else { None },
            run: None,
            error: None,
            attached: false,
        };
    };
    let state = match (running, written.state) {
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
        configuring,
        state,
        pid: running.then_some(written.pid),
        run: Some(written.run),
        error,
        attached: running && written.attached,
    }
}

/// How long this latch's socket has to take a connection before the host
/// holding the folder counts as another version's.
const PROBE_WITHIN: Duration = Duration::from_millis(500);

/// The pid of a host from another latch version serving `state_dir`: it
/// holds the folder and says it serves, but nothing listens on this
/// version's socket (a socket file left by a crash does not count), so
/// nothing this latch asks reaches it.
pub fn other_version(state_dir: &Path) -> Option<u32> {
    let host = now(state_dir);
    let serving = host.running && host.state == Phase::Serving;
    let theirs = serving && !listening(&crate::transport::local_socket_path(state_dir));
    host.pid.filter(|_| theirs)
}

/// Whether something takes connections on the socket at `path`, within
/// `PROBE_WITHIN`.
fn listening(path: &Path) -> bool {
    let path = path.to_path_buf();
    let (said, answer) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = said.send(std::os::unix::net::UnixStream::connect(path).is_ok());
    });
    answer.recv_timeout(PROBE_WITHIN).unwrap_or(false)
}

/// What every command that needs the host says while one from another
/// latch version holds the folder.
pub fn other_version_running(pid: u32) -> Failure {
    let message = format!(
        "a host from another latch version is running (pid {pid}); stop it \
         (Ctrl-C in its terminal, or kill {pid}), then retry"
    );
    Failure::new(code::other_version, message)
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
    let free = file.try_lock_shared().is_ok();
    // Closing the file releases the probe's lock, if it got one.
    !free
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn a_serve_that_holds_its_lock_is_what_it_wrote_and_one_that_let_go_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let stopped = Host {
            running: false,
            configuring: false,
            state: Phase::Stopped,
            pid: None,
            run: None,
            error: None,
            attached: false,
        };
        assert_eq!(now(dir.path()), stopped);
        assert_eq!(now(&dir.path().join("missing")), stopped);

        let mut lock = File::create(dir.path().join("serve.lock")).unwrap();
        write!(lock, "latch serve (pid {}, /bin/latch)", std::process::id()).unwrap();
        lock.lock().unwrap();
        assert_eq!(now(dir.path()).state, Phase::Starting);
        record(dir.path(), Phase::Serving, true, None).unwrap();
        let serving = now(dir.path());
        assert_eq!(serving.state, Phase::Serving);
        assert_eq!(serving.pid, Some(std::process::id()));
        assert!(serving.attached);

        // A serve that wrote `serving` but holds no lock: killed. (Another
        // folder: a test's own lock may outlive its drop for an instant in a
        // worker another test is starting.)
        drop(lock);
        let killed = tempfile::tempdir().unwrap();
        record(killed.path(), Phase::Serving, true, None).unwrap();
        let ended = now(killed.path());
        assert_eq!(ended.state, Phase::Error);
        assert!(!ended.attached);
        assert_eq!(ended.error.unwrap().code, "serve_ended");
        assert_eq!(ended.run, serving.run);
        let dir = killed;

        let failure = Failure::new(code::relay_unreachable, "no relay".to_string());
        record(dir.path(), Phase::Error, false, Some(&failure)).unwrap();
        assert_eq!(now(dir.path()).error.unwrap().code, "relay_unreachable");
        record(dir.path(), Phase::Stopped, false, None).unwrap();
        assert_eq!(now(dir.path()).state, Phase::Stopped);
        assert_eq!(now(dir.path()).error, None);
    }
}
