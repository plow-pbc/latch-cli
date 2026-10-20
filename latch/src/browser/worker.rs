//! The browser worker: one long-lived process per profile runtime, run as
//! `latch browser-worker --profile P --scratch DIR [--headless]` (hidden
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
//! scratch folder.
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
//! limit (`Call::limit`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::path::Path;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use super::camoufox::Camoufox;
use super::cdp;
use super::cft::{self, Cft};
use super::core::{Answer, Call, Core};
use super::driver::{Runtime, Seat};
use super::engine::{CAMOUFOX, CFT};
use super::profile;
use crate::failure::Failure;
use crate::wire::Response;

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
        // Camoufox is pinned for macOS only, so CfT is the one engine here.
        plugin: CFT.folder(&state_dir),
        scratch: scratch.canonicalize().map_err(cft::io_failure)?,
        state_dir,
    };
    crate::sandbox::confine_browser(&view).map_err(|why| Failure::failed("sandbox_failed", why))
}

pub fn run(
    state_dir: &Path,
    profile_name: &str,
    scratch: &Path,
    idle: Duration,
    headless: bool,
    display: Option<&str>,
) -> ExitCode {
    #[cfg(target_os = "linux")]
    if let Err(failure) = confine(state_dir, profile_name, scratch) {
        answer(&mut io::stdout().lock(), Response::error(&failure), None);
        return ExitCode::FAILURE;
    }
    // A screenless Linux host's runtime gets an Xvfb display of its own,
    // up for as long as this worker.
    let private = match display {
        Some(super::display::PRIVATE) => match super::display::Display::start() {
            Ok(private) => Some(private),
            Err(failure) => {
                answer(&mut io::stdout().lock(), Response::error(&failure), None);
                return ExitCode::FAILURE;
            }
        },
        _ => None,
    };
    let display = private.as_ref().map(|d| d.name.as_str()).or(display);
    // The engine runs from its scratch folder, so every path it gets is absolute.
    let absolute = |path: &Path| path.canonicalize().map_err(cft::io_failure);
    let launched = absolute(state_dir).and_then(|state_dir| {
        let scratch = absolute(scratch)?;
        Ok(
            match profile::engine_of(&state_dir, profile_name)?.as_deref() {
                Some("camoufox") => {
                    let browser =
                        Camoufox::launch(&state_dir, profile_name, &scratch, headless, display)?;
                    let ready = json!({
                        "engine": CAMOUFOX.engine,
                        "version": CAMOUFOX.version,
                        "driver_contract": CAMOUFOX.driver_contract,
                        "pid": browser.pid(),
                    });
                    serve(ready, browser, idle)
                }
                _ => {
                    let browser =
                        Cft::launch(&state_dir, profile_name, &scratch, headless, display)?;
                    let ready = json!({
                        "engine": CFT.engine,
                        "version": CFT.version,
                        "driver_contract": CFT.driver_contract,
                        "pid": browser.pid(),
                    });
                    serve(ready, browser, idle)
                }
            },
        )
    });
    launched.unwrap_or_else(|failure| {
        answer(&mut io::stdout().lock(), Response::error(&failure), None);
        ExitCode::FAILURE
    })
}

/// Says the browser is up, then answers each session's calls until the host
/// closes the browser, stops writing, or every session has gone quiet for
/// `idle`.
fn serve<B: Runtime>(ready: Value, browser: B, idle: Duration) -> ExitCode {
    let mut stdout = io::stdout().lock();
    answer(&mut stdout, Response::Ok(ready), None);
    let browser = Rc::new(RefCell::new(browser));
    let mut sessions: HashMap<String, Live<B>> = HashMap::new();
    let mut last_call = Instant::now();

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
        if stdin.buffer().is_empty() && !cdp::readable(&*raw, wait).unwrap_or(false) {
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
        let deadline = Instant::now() + call.limit();
        browser.borrow_mut().limit(Some(deadline));
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
                    let reply = match busy {
                        Some(site) => Err(site_busy(site.as_deref())),
                        None => live.core.call(call, input).map_err(|mut failure| {
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
