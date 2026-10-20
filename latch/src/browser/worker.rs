//! The browser worker: one long-lived process per session, run as
//! `latch browser-worker --profile P --scratch DIR [--headless]` (hidden
//! from `--help`).
//! It launches the engine on its profile and drives it over the CDP pipe.
//!
//! On macOS it is not inside latch's Seatbelt: a Seatbelt-confined process
//! can only re-apply its own profile, and every Chrome helper applies its
//! own, so a confined Chrome could only run with its sandbox off. Chrome's
//! sandbox stays on for every web-content process instead. What latch still
//! controls: the pinned build, checked before launch; a recipe that never
//! opens a debug port or turns the sandbox off; the profile locked to this
//! session; and an environment of nothing but `PATH`, a home, a temp folder
//! and a crash-dump folder, all in the session's scratch folder.
//!
//! It talks to the host over its standard streams, one JSON line each way
//! per call, every answer a `wire::Response`:
//! - first, unasked: `{"ok": {"engine", "version", "driver_contract",
//!   "pid"}}` once the browser is up, or the error that stopped it;
//! - then one `core::Call` per line (`{"op": "observe", …}`), each
//!   answered `{"ok": {"result": R, "audit": A}}`: R for the agent, A for
//!   the host's audit record. When bytes follow the line (a screenshot, a
//!   download), the answer says how many: `"bytes": N`. An upload's call
//!   is followed by its file's `size` bytes;
//! - `{"op": "close"}` closes the browser, waits for every process it
//!   started to end, answers and exits.
//!
//! The end of stdin, or no call for `--idle-timeout` seconds, closes the
//! browser the same way.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::json;

use super::cdp;
use super::cft::{self, Cft};
use super::core::{Call, Core};
use super::driver::Driver;
use super::engine::CFT;
use crate::failure::Failure;
use crate::wire::Response;

pub fn run(
    state_dir: &Path,
    profile_name: &str,
    scratch: &Path,
    idle: Duration,
    headless: bool,
) -> ExitCode {
    let mut stdout = io::stdout().lock();
    // The engine runs from its scratch folder, so every path it gets is absolute.
    let absolute = |path: &Path| path.canonicalize().map_err(cft::io_failure);
    let launched = absolute(state_dir)
        .and_then(|state_dir| Cft::launch(&state_dir, profile_name, &absolute(scratch)?, headless));
    let browser = match launched {
        Ok(browser) => browser,
        Err(failure) => {
            answer(&mut stdout, Response::error(&failure), None);
            return ExitCode::FAILURE;
        }
    };
    let ready = json!({
        "engine": CFT.engine,
        "version": CFT.version,
        "driver_contract": CFT.driver_contract,
        "pid": browser.pid(),
    });
    answer(&mut stdout, Response::Ok(ready), None);
    let mut core = Core::new(browser);

    // Standard input unbuffered by std, so `buffer()` below is the only
    // buffer: a line already in it is not waited for again.
    // SAFETY: fd 0 stays open for the life of the process.
    let raw = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
    let mut stdin = BufReader::new(&*raw);
    loop {
        let waiting = stdin.buffer().is_empty();
        if waiting && !cdp::readable(&*raw, idle).unwrap_or(false) {
            break;
        }
        let mut line = String::new();
        if stdin.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let call = match serde_json::from_str::<Call>(&line) {
            Ok(call) => call,
            Err(err) => {
                let failure = Failure::usage("bad_request", err.to_string());
                answer(&mut stdout, Response::error(&failure), None);
                continue;
            }
        };
        if matches!(call, Call::Close) {
            let closed = core.driver.close();
            let reply = json!({"result": closed, "audit": {}});
            answer(&mut stdout, Response::Ok(reply), None);
            return ExitCode::SUCCESS;
        }
        // An upload's bytes follow its line.
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
        match core.call(call, input) {
            Ok(done) => {
                let mut reply = json!({"result": done.result, "audit": done.audit});
                if let Some(bytes) = &done.bytes {
                    reply["bytes"] = json!(bytes.len());
                }
                answer(&mut stdout, Response::Ok(reply), done.bytes.as_deref());
            }
            Err(mut failure) => {
                if failure.code == "browser_gone" {
                    answer(&mut stdout, Response::error(&failure), None);
                    break;
                }
                core.reading_for(&mut failure);
                answer(&mut stdout, Response::error(&failure), None);
            }
        }
    }
    core.driver.close();
    ExitCode::SUCCESS
}

fn answer(stdout: &mut impl Write, response: Response, bytes: Option<&[u8]>) {
    let line = serde_json::to_string(&response).expect("response serializes");
    let _ = writeln!(stdout, "{line}");
    if let Some(bytes) = bytes {
        let _ = stdout.write_all(bytes);
    }
    let _ = stdout.flush();
}
