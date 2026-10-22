//! The browser worker: one long-lived process per profile runtime, run as
//! `latch browser-worker --profile P --scratch DIR [--headless]
//! [--allow-local-browsing]` (hidden
//! from `--help`). It launches the engine its profile is bound to (a
//! profile not made yet is made for CfT) and drives it, CfT over the CDP
//! pipe and Camoufox through its helper, for every session on the profile:
//! each has its own windows, tabs, snapshots, downloads and dialogs, and
//! the browser's logins are shared.
//!
//! On macOS it is not inside latch's Seatbelt: a Seatbelt-confined process
//! can only re-apply its own profile, and every Chrome or Firefox helper
//! applies its own, so a confined engine could only run with its sandbox
//! off. The engine's sandbox stays on for every web-content process
//! instead. What latch still controls: the pinned build, checked before
//! launch; a recipe that never opens a debug port or turns the sandbox off;
//! the profile locked to this runtime; and a minimal environment (`PATH`, a
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
//! It talks to the host over its standard streams, one JSON line each way
//! per call, every answer a `wire::Response`:
//! - first, unasked: `{"ok": {"engine", "version", "driver_contract",
//!   "pid"}}` once the browser is up, or the error that stopped it;
//! - then one `core::Call` per line (`{"op": "observe", …}`), each for the
//!   session its `"session"` key names, and answered `{"ok": {"result": R,
//!   "audit": A}}`: R for the agent, A for the host's audit record. When
//!   bytes follow the line (a screenshot, a download), the answer says how
//!   many: `"bytes": N`. An upload's call is followed by its file's `size`
//!   bytes. `"reserved"` lists the sites other sessions hold: an action
//!   there is refused with `site_busy` before it is sent;
//! - `{"op": "begin"}` starts the call's session with a window of its own,
//!   and `{"op": "end"}` closes that session's tabs;
//! - `{"op": "close"}` closes the browser, waits for every process it
//!   started to end, answers and exits.
//!
//! While a vault fill holds one session (from its first step until its
//! cleanup is confirmed), every other session's call is refused with
//! `session_paused`. A session ends `--idle-timeout` seconds after its
//! last call ended; the browser closes when the last one has, and at the
//! end of stdin. Every call's waits for the browser end by its
//! limit (`Call::limit`, from `--call-limit`), and a browser that leaves
//! several calls in a row unanswered is closed as gone.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::path::Path;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use super::camoufox::Camoufox;
use super::cdp;
use super::cft::{self, Cft};
use super::core::{Answer, Call, Core};
use super::driver::{Runtime, Seat};
use super::egress;
use super::engine::{CAMOUFOX, CFT};
use super::profile;
use crate::failure::Failure;
use crate::wire::Response;

/// Calls in a row that time out on a browser that did not answer before
/// the worker counts it gone: it closes it and ends, and the next session
/// starts a new one.
const UNANSWERED_IN_A_ROW: usize = 3;

/// The process groups the engine's processes lead (the worker starts each
/// engine in a group of its own), for `die_with_host`.
static ENGINE_GROUPS: Mutex<Vec<libc::pid_t>> = Mutex::new(Vec::new());

/// Notes the process group `leader` leads, which this worker started.
pub fn started_group(leader: u32) {
    let mut groups = ENGINE_GROUPS
        .lock()
        .unwrap_or_else(|held| held.into_inner());
    groups.push(leader as libc::pid_t);
}

/// Kills the engine's processes and this worker the moment the host that
/// started it exits, however it exits and even in the middle of a call:
/// a watch of its own, on a thread of its own. (The end of stdin says the
/// same, but only once the call under way is over.) On Linux the host
/// has the kernel kill this worker, and bubblewrap then kills everything
/// in its sandbox (see `Runtime::start_with`).
#[cfg(target_os = "macos")]
fn die_with_host() -> Result<(), Failure> {
    let watch = crate::sandbox::watch_host().map_err(|why| Failure::failed("host_gone", why))?;
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

/// Confines this worker to its profile runtime's view. The outer copy
/// becomes bubblewrap and never returns; the copy inside returns once its
/// filter is on.
#[cfg(target_os = "linux")]
fn confine(state_dir: &Path, profile_name: &str, scratch: &Path) -> Result<(), Failure> {
    let state_dir = state_dir.canonicalize().map_err(cft::io_failure)?;
    // Made here if new (for CfT, as the launch below would), so the view
    // has it to bind.
    let engine = profile::engine_of(&state_dir, profile_name)?;
    let profile = profile::open(
        &state_dir,
        profile_name,
        engine.as_deref().unwrap_or(&CFT.engine),
    )?;
    let view = crate::sandbox::BrowserView {
        profile: profile.dir,
        plugin: match engine.as_deref() {
            Some("camoufox") => CAMOUFOX.folder(&state_dir),
            _ => CFT.folder(&state_dir),
        },
        scratch: scratch.canonicalize().map_err(cft::io_failure)?,
        state_dir,
    };
    crate::sandbox::confine_browser(&view).map_err(|why| Failure::failed("sandbox_failed", why))
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    state_dir: &Path,
    profile_name: &str,
    scratch: &Path,
    idle: Duration,
    call_limit: Duration,
    headless: bool,
    private_display: bool,
    local: bool,
    page_isolation_off: bool,
) -> ExitCode {
    #[cfg(target_os = "linux")]
    if let Err(failure) = confine(state_dir, profile_name, scratch) {
        answer(&mut io::stdout().lock(), Response::error(&failure), None);
        return ExitCode::FAILURE;
    }
    #[cfg(target_os = "macos")]
    if let Err(failure) = die_with_host() {
        answer(&mut io::stdout().lock(), Response::error(&failure), None);
        return ExitCode::FAILURE;
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
    let display = private.as_ref().map(|d| d.name.as_str());
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
        Ok(
            match profile::engine_of(&state_dir, profile_name)?.as_deref() {
                Some("camoufox") => {
                    let browser = Camoufox::launch(
                        &state_dir,
                        profile_name,
                        &scratch,
                        headless,
                        display,
                        &proxy,
                    )?;
                    let ready = json!({
                        "engine": CAMOUFOX.engine,
                        "version": CAMOUFOX.version,
                        "driver_contract": CAMOUFOX.driver_contract,
                        "pid": browser.pid(),
                    });
                    serve(ready, browser, idle, call_limit, local)
                }
                _ => {
                    let browser = Cft::launch(
                        &state_dir,
                        profile_name,
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
                    });
                    serve(ready, browser, idle, call_limit, local)
                }
            },
        )
    });
    launched.unwrap_or_else(|failure| {
        answer(&mut io::stdout().lock(), Response::error(&failure), None);
        ExitCode::FAILURE
    })
}

/// How often the engine is tended between calls while it needs it.
const TEND: Duration = Duration::from_millis(20);

/// Says the browser is up, then answers each session's calls until the host
/// closes the browser, stops writing, or every session has gone quiet for
/// `idle`.
fn serve<B: Runtime>(
    ready: Value,
    browser: B,
    idle: Duration,
    call_limit: Duration,
    local: bool,
) -> ExitCode {
    let mut stdout = io::stdout().lock();
    answer(&mut stdout, Response::Ok(ready), None);
    let browser = Rc::new(RefCell::new(browser));
    let mut sessions: HashMap<String, Live<B>> = HashMap::new();
    let mut last_call = Instant::now();
    let mut unanswered = Unanswered::default();

    // Standard input unbuffered by std, so `buffer()` below is the only
    // buffer: a line already in it is not waited for again.
    // SAFETY: fd 0 stays open for the life of the process.
    let raw = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
    let mut stdin = BufReader::new(&*raw);
    loop {
        // Until the next session would idle out; with none, the browser.
        let due = sessions
            .values()
            .map(|live| live.last + idle)
            .min()
            .unwrap_or(last_call + idle);
        let wait = due.saturating_duration_since(Instant::now());
        // While the engine holds requests for latch to check, it is tended
        // every few milliseconds, not left until the next call.
        let tending = browser.borrow_mut().tend();
        let slice = if tending { wait.min(TEND) } else { wait };
        if stdin.buffer().is_empty() && !cdp::readable(&*raw, slice).unwrap_or(false) {
            if tending && Instant::now() < due {
                continue;
            }
            let now = Instant::now();
            let idled: Vec<String> = sessions
                .iter()
                .filter(|(_, live)| live.last + idle <= now)
                .map(|(key, _)| key.clone())
                .collect();
            for key in idled {
                sessions.remove(&key);
                browser.borrow_mut().end(&key);
            }
            if sessions.is_empty() {
                break;
            }
            continue;
        }
        let mut line = String::new();
        if stdin.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        last_call = Instant::now();
        let parsed = serde_json::from_str::<Value>(&line).and_then(|value| {
            let call = Call::deserialize(&value)?;
            Ok((value, call))
        });
        let (line, call) = match parsed {
            Ok(parsed) => parsed,
            Err(err) => {
                let failure = Failure::usage("bad_request", err.to_string());
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
        let key = line["session"].as_str().unwrap_or_default().to_string();
        // Every wait for the engine in this call, together, ends by then.
        let deadline = Instant::now() + call.limit(call_limit);
        browser.borrow_mut().limit(Some(deadline));
        browser.borrow_mut().unanswered();
        let held = sessions
            .iter()
            .any(|(other, live)| *other != key && live.core.is_paused());
        let reply = match call {
            Call::Close => {
                let closed = browser.borrow_mut().close();
                let reply = json!({"result": closed, "audit": {}});
                answer(&mut stdout, Response::Ok(reply), None);
                return ExitCode::SUCCESS;
            }
            Call::Begin if held => Err(paused()),
            Call::Begin => Seat::begin(&browser, &key).map(|seat| {
                sessions.insert(
                    key,
                    Live {
                        core: Core::new(seat),
                        last: Instant::now(),
                    },
                );
                Answer {
                    result: json!({}),
                    audit: json!({}),
                    bytes: None,
                }
            }),
            Call::End => {
                sessions.remove(&key);
                browser.borrow_mut().end(&key);
                Ok(Answer {
                    result: json!({"closed": true}),
                    audit: json!({}),
                    bytes: None,
                })
            }
            call => match sessions.get_mut(&key) {
                None => Err(Failure::failed(
                    "no_such_session",
                    "that session has ended".into(),
                )),
                // Listing a session's tabs, and retiring its refs after a
                // handoff, are the calls a pause lets by: neither reads or
                // touches a page.
                Some(_) if held && !matches!(call, Call::Tabs | Call::Forget) => Err(paused()),
                Some(live) => {
                    live.core.limit(deadline);
                    // An address that is not on the web is never sent.
                    let refused = call
                        .loads()
                        .and_then(|url| egress::requested(url, local).err());
                    let reserved: Vec<&str> = line["reserved"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    let busy = if reserved.is_empty() {
                        None
                    } else {
                        match live.core.sites_of(&call) {
                            Some(sites) => sites
                                .into_iter()
                                .find(|site| reserved.contains(&site.as_str()))
                                .map(Some),
                            // Where it leads is not known: it might be there.
                            None => Some(None),
                        }
                    };
                    let reply = match (refused, busy) {
                        (Some(failure), _) => Err(failure),
                        (None, Some(site)) => Err(site_busy(site.as_deref())),
                        (None, None) => live.core.call(call, input).map_err(|mut failure| {
                            if failure.code != "browser_gone" {
                                live.core.reading_for(&mut failure);
                            }
                            failure
                        }),
                    };
                    // Idle from the end of its call: a call that took
                    // longer than the idle timeout does not end it.
                    live.last = Instant::now();
                    reply
                }
            },
        };
        browser.borrow_mut().limit(None);
        last_call = Instant::now();
        let missed = browser.borrow_mut().unanswered();
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
    browser.borrow_mut().close();
    ExitCode::SUCCESS
}

/// One session on the browser, and when it was last called.
struct Live<B: Runtime> {
    core: Core<Seat<B>>,
    last: Instant,
}

/// Refused while another session's fill holds the browser. Its audit
/// record rides in the details, as the core's refusals do.
fn paused() -> Failure {
    let mut failure = Failure::failed(
        "session_paused",
        "a vault fill holds this browser; nothing can be read or done until it finishes".into(),
    );
    failure.details = json!({"audit": {}});
    failure
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
    Failure::failed(
        "browser_gone",
        format!(
            "the browser left {UNANSWERED_IN_A_ROW} calls in a row unanswered; latch closed it, and the next session starts a new one"
        ),
    )
}

/// Refused before it is sent: the call would act on `site`, which another
/// session reserved, or (None) on a page that is not known.
fn site_busy(site: Option<&str>) -> Failure {
    let message = match site {
        Some(site) => {
            format!("another session on this profile has reserved {site}; nothing was sent")
        }
        None => "another session on this profile has reserved a site, and the page Back leads to is not known; nothing was sent".to_string(),
    };
    let mut failure = Failure::failed("site_busy", message);
    failure.details = json!({"site": site, "audit": {"site_busy": site}});
    failure
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
