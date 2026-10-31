//! The browser worker: one long-lived process per session runtime, run as
//! `latch browser-worker --engine E --profile DIR --scratch DIR [--headless]
//! [--allow-local-browsing]` (hidden from `--help`). It launches engine E
//! on the session's profile in DIR (`profile.rs`) and drives it, CfT over
//! the CDP pipe and Camoufox through its helper.
//!
//! On macOS it is not inside latch's Seatbelt: a Seatbelt-confined process
//! can only re-apply its own profile, and every Chrome or Firefox helper
//! applies its own, so a confined engine could only run with its sandbox
//! off. The engine's sandbox stays on for every web-content process
//! instead. What latch still controls: the pinned build, checked before
//! launch; a recipe that never opens a debug port or turns the sandbox off;
//! a profile of the session's own; and a minimal environment (`PATH`, a
//! home, a temp folder and the engine's own settings), all in the runtime's
//! scratch folder; and where it goes: only web addresses, and the network
//! only through the worker's proxy, which refuses the host's local
//! addresses (`egress.rs`), so neither the state folder's files nor the
//! host's own services are in a page's reach.
//!
//! On Linux it confines itself first, in bubblewrap (`sandbox::BrowserView`):
//! of the state directory it sees only its profile and the engine's plugin
//! folder, and no areas. Chrome's own sandbox nests inside it.
//!
//! On Windows, as on macOS, it is not in latch's sandbox: Chrome's own
//! sandbox cannot start inside a worker's AppContainer (its crash handler's
//! pipe is refused there, and its sandboxed processes fail to start), so
//! Chrome's sandbox stays on for every web-content process instead, with
//! the same controls as on macOS. Chrome and everything it starts run in a
//! job the worker holds (`cft.rs`), which ends them when the worker ends;
//! the host keeps the worker in its own job (`sandbox::tie_to_host`).
//! Camoufox's helper and Firefox run the same way (`camoufox.rs`).
//!
//! It talks to the host over its standard streams, one JSON line each way
//! per call, every answer a `wire::Response`:
//! - first, unasked: `{"ok": {"engine", "version", "driver_contract",
//!   "pid", "groups"}}` once the browser is up (`groups`: the process
//!   groups the engine's processes lead), or the error that stopped it;
//! - then one `core::Call` per line (`{"op": "observe", …}`), answered
//!   `{"ok": {"result": R, "audit": A}}`: R for the agent, A for the host's
//!   audit record. When bytes follow the line (a screenshot, a download),
//!   the answer says how many: `"bytes": N`. An upload's call is followed
//!   by its file's `size` bytes;
//! - `{"op": "close"}` closes the browser, waits for every process it
//!   started to end, answers and exits.
//!
//! The session ends, and the browser closes, `--idle-timeout` seconds after
//! its last call ended, and at the end of stdin. Every call's waits for the
//! browser end by its limit (`Call::limit`, from `--call-limit`), and a
//! browser that leaves several calls in a row unanswered is closed as gone.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::mem::ManuallyDrop;
#[cfg(unix)]
use std::os::fd::FromRawFd;
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;
use std::process::ExitCode;
#[cfg(unix)]
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use super::camoufox::Camoufox;
use super::cdp;
use super::cft::{self, Cft};
use super::core::{Call, Core};
use super::driver::Runtime;
use super::egress;
use super::engine::{CAMOUFOX, CFT};
use crate::failure::{Failure, code};
use crate::wire::Response;

/// Calls in a row that time out on a browser that did not answer before
/// the worker counts it gone: it closes it and ends, and the next session
/// starts a new one.
const UNANSWERED_IN_A_ROW: usize = 3;

/// The only `PATH` an engine gets.
#[cfg(unix)]
const PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
#[cfg(windows)]
use crate::worker::PATH;

/// The environment an engine's processes get (Chrome, or Camoufox's
/// helper and browser), and nothing else: a plain `PATH`, and a home and a
/// temp folder in the session's `scratch` folder; on Windows first what
/// Windows needs to start a program, as a worker gets it, with the user's
/// own folders in `scratch` too.
pub fn environment(scratch: &Path) -> BTreeMap<&'static str, OsString> {
    let mut env = BTreeMap::from([("PATH", OsString::from(PATH))]);
    #[cfg(unix)]
    env.extend([
        ("HOME", scratch.into()),
        ("TMPDIR", scratch.join("tmp").into()),
    ]);
    #[cfg(windows)]
    {
        for name in crate::worker::WINDOWS_ENV {
            if let Some(value) = std::env::var_os(name) {
                env.insert(name, value);
            }
        }
        for name in ["USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
            env.insert(name, scratch.into());
        }
        for name in ["TEMP", "TMP"] {
            env.insert(name, scratch.join("tmp").into());
        }
    }
    env
}

/// The process groups the engine's processes lead (the worker starts each
/// engine in a group of its own), for `die_with_host`.
#[cfg(unix)]
static ENGINE_GROUPS: Mutex<Vec<libc::pid_t>> = Mutex::new(Vec::new());

/// Notes the process group `leader` leads, which this worker started.
#[cfg(unix)]
pub fn started_group(leader: u32) {
    let mut groups = ENGINE_GROUPS
        .lock()
        .unwrap_or_else(|held| held.into_inner());
    groups.push(leader as libc::pid_t);
}

/// The process groups the engine's processes lead.
#[cfg(unix)]
fn engine_groups() -> Vec<libc::pid_t> {
    ENGINE_GROUPS
        .lock()
        .unwrap_or_else(|held| held.into_inner())
        .clone()
}

/// None on Windows, where a job holds the engine's processes.
#[cfg(windows)]
fn engine_groups() -> Vec<u32> {
    Vec::new()
}

/// Kills the engine's processes and this worker the moment the host that
/// started it exits, however it exits and even in the middle of a call:
/// a watch of its own, on a thread of its own. (The end of stdin says the
/// same, but only once the call under way is over.) On Linux the host
/// has the kernel kill this worker, and bubblewrap then kills everything
/// in its sandbox (see `Runtime::start_with`).
#[cfg(target_os = "macos")]
fn die_with_host() -> Result<(), Failure> {
    let watch = crate::sandbox::watch_host().map_err(|why| Failure::new(code::host_gone, why))?;
    std::thread::spawn(move || {
        if !crate::sandbox::wait_for_host(watch) {
            return;
        }
        let groups = ENGINE_GROUPS
            .lock()
            .unwrap_or_else(|held| held.into_inner());
        for group in groups.iter() {
            // SAFETY: signals only, to groups this worker started.
            unsafe { libc::killpg(*group, libc::SIGKILL) };
        }
        std::process::exit(1);
    });
    Ok(())
}

/// Confines this worker to its session runtime's view. The outer copy
/// becomes bubblewrap and never returns; the copy inside returns once its
/// filter is on.
#[cfg(target_os = "linux")]
fn confine(state_dir: &Path, engine: &str, profile: &Path, scratch: &Path) -> Result<(), Failure> {
    let state_dir = state_dir.canonicalize().map_err(cft::io_failure)?;
    let view = crate::sandbox::BrowserView {
        profile: profile.canonicalize().map_err(cft::io_failure)?,
        plugin: match engine {
            "camoufox" => CAMOUFOX.folder(&state_dir),
            _ => CFT.folder(&state_dir),
        },
        scratch: scratch.canonicalize().map_err(cft::io_failure)?,
        state_dir,
    };
    crate::sandbox::confine_browser(&view).map_err(|why| Failure::new(code::sandbox_failed, why))
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    state_dir: &Path,
    engine: &str,
    profile: &Path,
    scratch: &Path,
    idle: Duration,
    call_limit: Duration,
    headless: bool,
    private_display: bool,
    local: bool,
    page_isolation_off: bool,
) -> ExitCode {
    #[cfg(target_os = "linux")]
    if let Err(failure) = confine(state_dir, engine, profile, scratch) {
        answer(&mut io::stdout().lock(), Response::error(&failure), None);
        return ExitCode::FAILURE;
    }
    #[cfg(target_os = "macos")]
    if let Err(failure) = die_with_host() {
        answer(&mut io::stdout().lock(), Response::error(&failure), None);
        return ExitCode::FAILURE;
    }
    // Chrome inherits its pipe and nothing else: not the worker's own
    // streams, the host's pipes.
    #[cfg(windows)]
    for handle in [
        io::stdin().as_raw_handle(),
        io::stdout().as_raw_handle(),
        io::stderr().as_raw_handle(),
    ] {
        use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
        // SAFETY: clears a flag on this process's own standard handles.
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
    }
    // A headed Linux runtime gets an Xvfb display of its own, up for as
    // long as this worker.
    let private = if private_display {
        match super::display::Display::start() {
            Ok(private) => Some(private),
            Err(failure) => {
                answer(&mut io::stdout().lock(), Response::error(&failure), None);
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    let display = private.as_ref();
    // The engine reaches the network only through this proxy.
    let proxy = match egress::Proxy::start(local) {
        Ok(proxy) => proxy.url(),
        Err(err) => {
            let failure = cft::io_failure(err);
            answer(&mut io::stdout().lock(), Response::error(&failure), None);
            return ExitCode::FAILURE;
        }
    };
    // The engine runs from its scratch folder, so every path it gets is absolute.
    let absolute = |path: &Path| path.canonicalize().map_err(cft::io_failure);
    let launched = absolute(state_dir).and_then(|state_dir| {
        let scratch = absolute(scratch)?;
        let profile = absolute(profile)?;
        Ok(match engine {
            "camoufox" => {
                let browser =
                    Camoufox::launch(&state_dir, &profile, &scratch, headless, display, &proxy)?;
                let ready = json!({
                    "engine": CAMOUFOX.engine,
                    "version": CAMOUFOX.version,
                    "driver_contract": CAMOUFOX.driver_contract,
                    "pid": browser.pid(),
                    "groups": engine_groups(),
                });
                serve(ready, browser, idle, call_limit, local)
            }
            "cft" => {
                let browser = Cft::launch(
                    &state_dir,
                    &profile,
                    &scratch,
                    headless,
                    display,
                    &proxy,
                    page_isolation_off,
                )?;
                let ready = json!({
                    "engine": CFT.engine,
                    "version": CFT.version,
                    "driver_contract": CFT.driver_contract,
                    "pid": browser.pid(),
                    "groups": engine_groups(),
                });
                serve(ready, browser, idle, call_limit, local)
            }
            _ => {
                return Err(Failure::new(
                    code::bad_engine,
                    format!("no browser engine {engine}"),
                ));
            }
        })
    });
    launched.unwrap_or_else(|failure| {
        answer(&mut io::stdout().lock(), Response::error(&failure), None);
        ExitCode::FAILURE
    })
}

/// How often the engine is tended between calls while it needs it.
const TEND: Duration = Duration::from_millis(20);

/// Says the browser is up, then answers the session's calls until the host
/// closes the browser, stops writing, or the session has gone quiet for
/// `idle` after its last call.
fn serve<B: Runtime>(
    ready: Value,
    browser: B,
    idle: Duration,
    call_limit: Duration,
    local: bool,
) -> ExitCode {
    let mut stdout = io::stdout().lock();
    answer(&mut stdout, Response::Ok(ready), None);
    let mut core = Core::new(browser);
    let mut last_call = Instant::now();
    let mut unanswered = Unanswered::default();

    // Standard input unbuffered by std, so `buffer()` below is the only
    // buffer: a line already in it is not waited for again.
    // SAFETY: fd 0 stays open for the life of the process.
    #[cfg(unix)]
    let raw = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
    // SAFETY: the standard input handle stays open for the life of the
    // process, and is never closed through this copy.
    #[cfg(windows)]
    let raw = ManuallyDrop::new(unsafe { File::from_raw_handle(io::stdin().as_raw_handle()) });
    let mut stdin = BufReader::new(&*raw);
    loop {
        let due = last_call + idle;
        // A secret a fill left on the page is emptied when its time is up,
        // between calls.
        let clear = core.clear_at();
        let wait = clear
            .map_or(due, |at| at.min(due))
            .saturating_duration_since(Instant::now());
        // The engine is tended when it writes (`Runtime::wakes`), and while
        // it holds requests for latch to check, every few milliseconds, not
        // left until the next call.
        let tending = core.browser().tend();
        let slice = if tending { wait.min(TEND) } else { wait };
        let wakes = core.browser().wakes();
        if stdin.buffer().is_empty() && !cdp::readable_or(&*raw, wakes, slice).unwrap_or(false) {
            if clear.is_some_and(|at| Instant::now() >= at) {
                let deadline = Instant::now() + call_limit;
                core.browser().limit(Some(deadline));
                core.limit(deadline);
                core.clear_due();
                core.browser().limit(None);
                continue;
            }
            if Instant::now() < due {
                continue;
            }
            break;
        }
        let mut line = String::new();
        if stdin.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let call = match serde_json::from_str::<Value>(&line).and_then(Call::deserialize) {
            Ok(call) => call,
            Err(err) => {
                let failure = Failure::new(code::bad_request, err.to_string());
                answer(&mut stdout, Response::error(&failure), None);
                continue;
            }
        };
        // An upload's bytes follow its line, even when the call is refused.
        let input = match &call {
            Call::Upload { size, .. } => {
                let mut bytes = vec![0; *size as usize];
                if stdin.read_exact(&mut bytes).is_err() {
                    break;
                }
                Some(bytes)
            }
            _ => None,
        };
        if let Call::Close = call {
            let closed = core.browser().close();
            let reply = json!({"result": closed, "audit": {}});
            answer(&mut stdout, Response::Ok(reply), None);
            return ExitCode::SUCCESS;
        }
        // Every wait for the engine in this call, together, ends by then.
        let deadline = Instant::now() + call.limit(call_limit);
        core.browser().limit(Some(deadline));
        core.browser().unanswered();
        core.limit(deadline);
        // An address that is not on the web is never sent.
        let reply = match call
            .loads()
            .and_then(|url| egress::requested(url, local).err())
        {
            Some(failure) => Err(failure),
            None => core.call(call, input).map_err(|mut failure| {
                if failure.code != "browser_gone" {
                    core.reading_for(&mut failure);
                }
                failure
            }),
        };
        core.browser().limit(None);
        // Idle from the end of its call: a call that took longer than the
        // idle timeout does not end the session.
        last_call = Instant::now();
        let missed = core.browser().unanswered();
        let timed_out = matches!(&reply, Err(failure) if failure.code == "timeout");
        let reply = if unanswered.after(timed_out, missed) {
            Err(stuck())
        } else {
            reply
        };
        match reply {
            Ok(done) => {
                let mut reply = json!({"result": done.result, "audit": done.audit});
                if let Some(bytes) = &done.bytes {
                    reply["bytes"] = json!(bytes.len());
                }
                answer(&mut stdout, Response::Ok(reply), done.bytes.as_deref());
            }
            Err(failure) => {
                answer(&mut stdout, Response::error(&failure), None);
                if failure.code == "browser_gone" {
                    break;
                }
            }
        }
    }
    core.browser().close();
    ExitCode::SUCCESS
}

/// Calls in a row that timed out on a browser that left a command
/// unanswered.
#[derive(Default)]
struct Unanswered(usize);

impl Unanswered {
    /// Counts a call that ended, and says whether the browser is now gone.
    fn after(&mut self, timed_out: bool, missed: bool) -> bool {
        // Any other end, an error included, is the browser answering.
        if timed_out && missed {
            self.0 += 1;
        } else {
            self.0 = 0;
        }
        self.0 >= UNANSWERED_IN_A_ROW
    }
}

fn stuck() -> Failure {
    Failure::new(
        code::browser_gone,
        format!(
            "the browser left {UNANSWERED_IN_A_ROW} calls in a row unanswered; latch closed it, and the next session starts a new one"
        ),
    )
}

fn answer(stdout: &mut impl Write, response: Response, bytes: Option<&[u8]>) {
    let line = serde_json::to_string(&response).expect("response serializes");
    let _ = writeln!(stdout, "{line}");
    if let Some(bytes) = bytes {
        let _ = stdout.write_all(bytes);
    }
    let _ = stdout.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_browser_is_gone_after_unanswered_calls_in_a_row_only() {
        let mut count = Unanswered::default();
        // Two unanswered calls, then one the browser answered, if only
        // with an error: the count starts again.
        assert!(!count.after(true, true));
        assert!(!count.after(true, true));
        assert!(!count.after(false, false));
        assert!(!count.after(true, true));
        assert!(!count.after(true, true));
        // A call that timed out with every command answered (a page that
        // never loaded) is the browser answering too.
        assert!(!count.after(true, false));
        assert!(!count.after(true, true));
        assert!(!count.after(true, true));
        assert!(count.after(true, true));
    }
}
