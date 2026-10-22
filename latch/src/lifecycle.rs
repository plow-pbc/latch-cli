//! What the host on a state folder is doing, for other latch processes on
//! this machine (`status --local`, `events`): `latch serve` writes each step
//! of its life to `serve.json`, and whether it still runs is whether it
//! still holds `serve.lock`. A serve that was killed or crashed never wrote
//! its last step; its released lock says so.

use std::fs::{self, File};
use std::path::Path;
use std::sync::{OnceLock, mpsc};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::failure::Failure;
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

/// The host now: its step, and, while one runs or last ran, its process and
/// run. A serve that has just locked the folder has no run yet.
#[derive(Clone, Debug, PartialEq)]
pub struct Host {
    /// A `latch serve` holds the state folder.
    pub running: bool,
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
        .map_err(|err| Failure::failed("state_io", format!("{}: {err}", path.display())))
}

/// What the host on `state_dir` is doing now. Reads only; works on a folder
/// that never served, or does not exist.
pub fn now(state_dir: &Path) -> Host {
    let running = holds_lock(state_dir);
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

/// How far a process's start may be from its run's: the run is named as
/// the serve starts, a moment after the process did.
const STARTED_WITHIN_MS: i64 = 1000;

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

/// Whether `pid` is still the host serve.json on `state_dir` records: a
/// latch executable that started as its run did (a run is `PID-MILLIS`,
/// named as the serve starts). A pid since given to another process is not.
pub fn recorded_instance(state_dir: &Path, pid: u32) -> bool {
    let Some(run) = now(state_dir).run else {
        return false;
    };
    let Some(began) = run
        .strip_prefix(&format!("{pid}-"))
        .and_then(|millis| millis.parse::<i64>().ok())
    else {
        return false;
    };
    let Some((exe, started)) = process::started(pid) else {
        return false;
    };
    let latch = exe.file_name().is_some_and(|name| name == "latch");
    latch && (began - started).abs() <= STARTED_WITHIN_MS + process::RESOLUTION_MS
}

/// When a process started, and from which executable.
#[cfg(target_os = "macos")]
mod process {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    /// How finely the system tells a process's start.
    pub const RESOLUTION_MS: i64 = 0;

    /// `pid`'s executable and its start, in milliseconds since the epoch.
    pub fn started(pid: u32) -> Option<(PathBuf, i64)> {
        // SAFETY: proc_bsdinfo is a plain C struct that proc_pidinfo fills in.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // SAFETY: writes at most `size` bytes into `info`, which we own.
        let got = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        if got != size {
            return None;
        }
        let started = info.pbi_start_tvsec as i64 * 1000 + info.pbi_start_tvusec as i64 / 1000;
        let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: writes at most `path.len()` bytes into `path`, which we own.
        let length = unsafe {
            libc::proc_pidpath(
                pid as libc::c_int,
                path.as_mut_ptr().cast(),
                path.len() as u32,
            )
        };
        if length <= 0 {
            return None;
        }
        path.truncate(length as usize);
        Some((PathBuf::from(OsString::from_vec(path)), started))
    }
}

/// When a process started, and from which executable.
#[cfg(target_os = "linux")]
mod process {
    use std::fs;
    use std::path::PathBuf;

    /// How finely the system tells a process's start: the boot time it
    /// counts from is in whole seconds.
    pub const RESOLUTION_MS: i64 = 1000;

    /// `pid`'s executable and its start, in milliseconds since the epoch.
    pub fn started(pid: u32) -> Option<(PathBuf, i64)> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // Its start is field 22, in clock ticks since boot; the fields
        // after the command's closing parenthesis begin at field 3.
        let ticks: i64 = stat
            .rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(19)?
            .parse()
            .ok()?;
        let boot: i64 = fs::read_to_string("/proc/stat")
            .ok()?
            .lines()
            .find_map(|line| line.strip_prefix("btime "))?
            .trim()
            .parse()
            .ok()?;
        // SAFETY: sysconf reads a system setting.
        let hertz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as i64;
        if hertz <= 0 {
            return None;
        }
        // A replaced executable still names the file it ran.
        let exe = fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        let exe = exe.to_string_lossy();
        let exe = PathBuf::from(exe.strip_suffix(" (deleted)").unwrap_or(&exe));
        Some((exe, boot * 1000 + ticks * 1000 / hertz))
    }
}

/// What every command that needs the host says while one from another
/// latch version holds the folder.
pub fn other_version_running(pid: u32) -> Failure {
    let message = format!(
        "a host from another latch version is running (pid {pid}); stop it \
         (Ctrl-C in its terminal, or kill {pid}), then retry"
    );
    Failure::new(crate::failure::Exit::Unreachable, "other_version", message)
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

        let failure = Failure::failed("relay_unreachable", "no relay".to_string());
        record(dir.path(), Phase::Error, false, Some(&failure)).unwrap();
        assert_eq!(now(dir.path()).error.unwrap().code, "relay_unreachable");
        record(dir.path(), Phase::Stopped, false, None).unwrap();
        assert_eq!(now(dir.path()).state, Phase::Stopped);
        assert_eq!(now(dir.path()).error, None);
    }
}
