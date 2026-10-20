//! `handoff`: one session handed to the owner, who acts in its browser
//! window (a CAPTCHA, a texted code, a passkey, a sign-in) and hands it
//! back with `latch browser handoff done` on the host.
//!
//! The agent's `handoff` call waits until then, at most `--within` seconds
//! (600 by default). Meanwhile:
//! - that session is paused: every other call on it is answered
//!   `session_paused`, while the runtime's other sessions go on;
//! - its window alone comes to the front: the one thing in latch that
//!   activates the browser;
//! - the pending handoff is a file in `browser/handoff/`, named by the
//!   session's id hash and holding its caller, reason and deadline, and the
//!   host's dashboard shows it (or its log says so).
//!
//! `handoff done` removes that file. It runs on the host, against the
//! host's state directory, which file and exec workers can never reach. When
//! the handoff ends, by `done` or by its deadline, every ref from before it
//! is refused and the answer carries a fresh look at the page.
//!
//! A host with no screen a person could use (a screenless Linux host, its
//! browsers on a private display, or a host whose browsers run headless)
//! answers `needs_owner` with reason `no_screen` at once.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use super::host::{Browsers, Entry, paused};
use super::steps::call as worker;
use crate::failure::{Exit, Failure};

/// How long a handoff waits for the owner by default, and at most.
pub const WITHIN: u64 = 600;
const MAX_WITHIN: u64 = 3600;
/// How often a waiting handoff looks for `done`.
const POLL: Duration = Duration::from_millis(250);
pub const REASONS: [&str; 4] = ["captcha", "2fa", "passkey", "signin"];

/// Where pending handoffs are kept, in the host's state directory.
pub fn folder(state_dir: &Path) -> PathBuf {
    state_dir.join("browser/handoff")
}

/// Whether a person at this host could act in its browser windows.
pub fn screen(headless: bool) -> bool {
    if headless {
        return false;
    }
    // A Mac host runs in its owner's login session, on its screen. A Linux
    // host has one only with a display of its own: a runtime's private
    // Xvfb display is nobody's screen.
    cfg!(target_os = "macos")
        || ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty()))
}

/// The answer on a host with no screen: only the owner could do this, and
/// not at this host.
pub fn no_screen(why: &str) -> Failure {
    let mut failure = Failure::failed(
        "needs_owner",
        format!("{why}, and this host has no screen for its owner to use; nothing was handed off"),
    );
    failure.details = json!({"reason": "no_screen"});
    failure
}

/// `handoff --reason R [--within S]`: waits for the owner's `done`.
pub(super) async fn run(
    browsers: &Browsers,
    entry: &Entry,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let reason = args["reason"].as_str().unwrap_or_default();
    if !REASONS.contains(&reason) {
        return Err(Failure::usage(
            "bad_arguments",
            format!("--reason is one of {}", REASONS.join(", ")),
        ));
    }
    let within = args["within"].as_u64().unwrap_or(WITHIN);
    if !(1..=MAX_WITHIN).contains(&within) {
        return Err(Failure::usage(
            "bad_arguments",
            format!("--within is between 1 and {MAX_WITHIN} seconds"),
        ));
    }
    audit.insert("reason".into(), json!(reason));
    if !browsers.screen() {
        return Err(no_screen(&format!("this needs the owner ({reason})")));
    }
    if entry
        .runtime
        .filling
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return Err(paused());
    }
    if !entry.begin_handoff() {
        return Err(handed_off());
    }
    let file = folder(&browsers.state_dir()).join(format!("{}.json", entry.hash));
    let pending = json!({
        "session": entry.hash,
        "caller": entry.owner_hint(),
        "reason": reason,
        "since": jiff::Timestamp::now().to_string(),
        "within_s": within,
    });
    let outcome = wait(browsers, entry, &file, &pending, within, audit).await;
    let _ = fs::remove_file(&file);
    browsers.notice_handoffs();
    // Whatever happened, the agent gets the session back, and nothing from
    // before the handoff still names an element.
    let forgot = worker(entry, json!({"op": "forget"}), audit).await;
    entry.end_handoff();
    let waited = outcome?;
    forgot?;
    audit.insert("waited_s".into(), json!(waited.as_secs()));
    let (page, _) = worker(entry, json!({"op": "glance"}), audit).await?;
    Ok(json!({"handed_back": true, "reason": reason, "waited_s": waited.as_secs(), "page": page}))
}

/// Posts the pending handoff, brings its window forward, and waits for the
/// owner to remove it: how long that took, or `timeout`.
async fn wait(
    browsers: &Browsers,
    entry: &Entry,
    file: &Path,
    pending: &Value,
    within: u64,
    audit: &mut Map<String, Value>,
) -> Result<Duration, Failure> {
    let written = fs::create_dir_all(file.parent().expect("in a folder"))
        .and_then(|()| fs::write(file, pending.to_string()));
    written.map_err(|err| Failure::failed("handoff_failed", err.to_string()))?;
    browsers.notice_handoffs();
    worker(entry, json!({"op": "front"}), audit).await?;
    let started = Instant::now();
    loop {
        if !file.exists() {
            return Ok(started.elapsed());
        }
        if started.elapsed() >= Duration::from_secs(within) {
            let mut failure = Failure::failed(
                "timeout",
                format!(
                    "the owner did not hand the session back within {within}s; it is yours again, and refs from before the handoff are refused"
                ),
            );
            failure.details = json!({"reason": pending["reason"]});
            return Err(failure);
        }
        if !entry.runtime.alive() {
            return Err(Failure::failed(
                "browser_gone",
                "the browser closed during the handoff".into(),
            ));
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Refused while the owner has the session.
pub fn handed_off() -> Failure {
    Failure::failed(
        "session_paused",
        "the owner has this session (a handoff); nothing can be read or done until it is handed back"
            .into(),
    )
}

/// The pending handoffs, oldest first.
pub fn pending(state_dir: &Path) -> Vec<Value> {
    let mut all: Vec<Value> = fs::read_dir(folder(state_dir))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|f| f.path().extension().is_some_and(|e| e == "json"))
        .filter_map(|f| fs::read_to_string(f.path()).ok())
        .filter_map(|text| serde_json::from_str(&text).ok())
        .collect();
    all.sort_by(|a: &Value, b: &Value| {
        a["since"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["since"].as_str().unwrap_or_default())
    });
    all
}

/// One dashboard line for a pending handoff.
pub fn line(pending: &Value) -> String {
    format!(
        "handoff pending: session {} ({}) for {} · `latch browser handoff done`",
        pending["session"].as_str().unwrap_or_default(),
        pending["reason"].as_str().unwrap_or_default(),
        pending["caller"].as_str().unwrap_or_default(),
    )
}

/// `latch browser handoff done [--session HASH]`, on the host: hands the
/// session back. Without `--session`, there must be exactly one pending.
pub fn done(state_dir: &Path, session: Option<&str>) -> Result<Value, Failure> {
    let all = pending(state_dir);
    let hashes: Vec<&str> = all.iter().filter_map(|p| p["session"].as_str()).collect();
    let hash = match session {
        Some(hash) if hashes.contains(&hash) => hash,
        Some(hash) => {
            return Err(Failure::failed(
                "no_handoff",
                format!("no handoff is pending for session {hash}; pending: {hashes:?}"),
            ));
        }
        None => match hashes.as_slice() {
            [one] => one,
            [] => {
                return Err(Failure::failed(
                    "no_handoff",
                    "no handoff is pending on this host".into(),
                ));
            }
            _ => {
                let mut failure = Failure::new(
                    Exit::Usage,
                    "which_handoff",
                    "several handoffs are pending: name one with --session".into(),
                );
                failure.details = json!({"pending": all});
                return Err(failure);
            }
        },
    };
    fs::remove_file(folder(state_dir).join(format!("{hash}.json")))
        .map_err(|err| Failure::failed("handoff_failed", err.to_string()))?;
    Ok(json!({"handed_back": hash}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn done_hands_back_the_one_pending_or_the_one_named() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(done(dir.path(), None).unwrap_err().code, "no_handoff");
        fs::create_dir_all(folder(dir.path())).unwrap();
        for (hash, since) in [
            ("aaaa", "2026-10-19T10:00:00Z"),
            ("bbbb", "2026-10-19T10:05:00Z"),
        ] {
            let pending =
                json!({"session": hash, "caller": "k1", "reason": "captcha", "since": since});
            fs::write(
                folder(dir.path()).join(format!("{hash}.json")),
                pending.to_string(),
            )
            .unwrap();
        }
        assert_eq!(
            line(&pending(dir.path())[0]),
            "handoff pending: session aaaa (captcha) for k1 · `latch browser handoff done`"
        );
        assert_eq!(done(dir.path(), None).unwrap_err().code, "which_handoff");
        assert_eq!(
            done(dir.path(), Some("cccc")).unwrap_err().code,
            "no_handoff"
        );
        assert_eq!(
            done(dir.path(), Some("bbbb")).unwrap()["handed_back"],
            "bbbb"
        );
        assert_eq!(done(dir.path(), None).unwrap()["handed_back"], "aaaa");
        assert!(pending(dir.path()).is_empty());
    }

    #[test]
    fn a_headless_host_has_no_screen_to_hand_off_on() {
        assert!(!screen(true));
        if cfg!(target_os = "macos") {
            assert!(screen(false));
        }
        let failure = no_screen("this needs the owner (captcha)");
        assert_eq!(
            (failure.code.as_str(), &failure.details["reason"]),
            ("needs_owner", &json!("no_screen"))
        );
    }
}
