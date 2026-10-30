//! Each plan request's journal, in the host's state directory, so its
//! history outlives a lost reply and a host restart.
//!
//! One file per request, `browser/journal/<session hash>/<request>.jsonl`,
//! readable by the host's user only. Each line is one record, written and
//! synced before the host goes on: the request's start (its caller and
//! steps), each action before it is sent and again once its result is
//! known, each step's outcome, and the end. An action with no recorded
//! result reads as `unknown`, never `not_sent`, and a request with no end
//! that is not running reads as `interrupted`.
//!
//! It holds what the agent's own answer held: operations, targets and
//! outcomes. Beside it, `<request>/` keeps a screenshot after each action
//! (`<N>.png`, by action number) and, for a stop that asks for a
//! confirmation, of the page then (`final.png`); `do --history R --shot N`
//! streams one to the client. A request's journal and screenshots are kept
//! for 7 days, whether or not its session is still open.

use crate::state::OwnerOnly;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::failure::{Failure, code};
use crate::state::to_hex;

pub struct Journal {
    file: File,
    /// The numbers of the cards filled in the request's session: each
    /// record is written with them masked (`shown::mask_cards`).
    cards: Option<Cards>,
}

/// The numbers of the cards filled in a session, digits only.
pub type Cards = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// How long a request's journal and screenshots are kept.
pub const KEEP: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// A session's journal folder: named by a hash, never the session's name.
fn folder(state_dir: &Path, session: &str) -> PathBuf {
    let hash = to_hex(&Sha256::digest(session.as_bytes()));
    state_dir.join("browser/journal").join(&hash[..16])
}

fn lost(err: std::io::Error) -> Failure {
    Failure::new(
        code::journal_failed,
        format!(
            "the request could not be journaled ({err}); a step of it may already have run: read its history (`latch browser do --history R`) before sending it again"
        ),
    )
}

impl Journal {
    /// Starts request `request`'s journal. A request id already used in
    /// this session is refused: its history is there to read instead.
    pub fn start(
        state_dir: &Path,
        session: &str,
        caller: &str,
        request: &str,
        steps: Value,
    ) -> Result<Journal, Failure> {
        if let Some(before) = SystemTime::now().checked_sub(KEEP) {
            prune(state_dir, before);
        }
        let folder = folder(state_dir, session);
        fs::DirBuilder::new()
            .recursive(true)
            .owner_only()
            .create(&folder)
            .map_err(lost)?;
        let file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .owner_only()
            .open(folder.join(format!("{request}.jsonl")))
            .map_err(|err| match err.kind() {
                std::io::ErrorKind::AlreadyExists => Failure::new(
                    code::duplicate_request,
                    format!(
                        "request {request} was already sent to this session; read it with `latch browser do --history {request}`, never resend it"
                    ),
                ),
                _ => lost(err),
            })?;
        let mut journal = Journal { file, cards: None };
        journal.write(json!({"t": "start", "caller": caller, "steps": steps}))?;
        Ok(journal)
    }

    /// The journal, its records written with `cards` masked from here on.
    pub fn masking(self, cards: Cards) -> Journal {
        Journal {
            cards: Some(cards),
            ..self
        }
    }

    /// Appends one record and syncs it to disk.
    pub fn write(&mut self, mut record: Value) -> Result<(), Failure> {
        if let Some(cards) = &self.cards {
            let cards = cards.lock().expect("cards lock").clone();
            super::shown::mask_cards(&mut record, &cards);
        }
        let mut line = serde_json::to_vec(&record).expect("serializes");
        line.push(b'\n');
        self.file.write_all(&line).map_err(lost)?;
        self.file.sync_data().map_err(lost)
    }
}

/// Removes every request journal last written before `before`, with its
/// screenshots, and session folders left empty. What can't be removed
/// stays for the next try.
pub fn prune(state_dir: &Path, before: SystemTime) {
    let old = |path: &Path| {
        fs::metadata(path)
            .and_then(|m| m.modified())
            .is_ok_and(|at| at < before)
    };
    let Ok(sessions) = fs::read_dir(state_dir.join("browser/journal")) else {
        return;
    };
    for session in sessions.flatten() {
        let Ok(requests) = fs::read_dir(session.path()) else {
            continue;
        };
        let paths: Vec<PathBuf> = requests.flatten().map(|entry| entry.path()).collect();
        // Every path judged before any is removed: a request's screenshots
        // go with its journal; a folder with no journal, by its own age.
        let stale: Vec<&PathBuf> = paths
            .iter()
            .filter(|path| match path.extension() {
                Some(ext) if ext == "jsonl" => old(path),
                _ => {
                    let journal = path.with_extension("jsonl");
                    if journal.exists() {
                        old(&journal)
                    } else {
                        old(path)
                    }
                }
            })
            .collect();
        for path in stale {
            let _ = if path.is_dir() {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            };
        }
        let _ = fs::remove_dir(session.path());
    }
}

fn plain(request: &str) -> bool {
    !request.is_empty()
        && request
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Keeps a screenshot of request `request`'s page as `shot`, readable by
/// the host's user only.
pub fn save_shot(
    state_dir: &Path,
    session: &str,
    request: &str,
    shot: &str,
    png: &[u8],
) -> Result<(), Failure> {
    let folder = folder(state_dir, session).join(request);
    fs::DirBuilder::new()
        .recursive(true)
        .owner_only()
        .create(&folder)
        .map_err(lost)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .owner_only()
        .open(folder.join(format!("{shot}.png")))
        .map_err(lost)?;
    file.write_all(png).map_err(lost)
}

/// Request `request`'s screenshot `shot`, for the caller that sent it.
pub fn shot(
    state_dir: &Path,
    session: &str,
    caller: &str,
    request: &str,
    shot: &str,
) -> Result<Vec<u8>, Failure> {
    // Its owner is the request's: a missing or foreign request reads as none.
    history(state_dir, session, caller, request, false)?;
    let missing = || {
        Failure::new(
            code::no_such_shot,
            format!("request {request} has no screenshot {shot:?}"),
        )
    };
    if !plain(shot) {
        return Err(missing());
    }
    fs::read(
        folder(state_dir, session)
            .join(request)
            .join(format!("{shot}.png")),
    )
    .map_err(|_| missing())
}

/// The screenshot `shot` that the browser call audited as record `id`
/// names, for the host's owner (`latch audit shot`): only a screenshot the
/// record names, found by the record's session, request and shot, and only
/// if its bytes are the ones the record's sha256 says were kept.
pub fn audited(state_dir: &Path, id: &str, shot: &str) -> Result<Vec<u8>, Failure> {
    let record = crate::audit::find(state_dir, id)?.ok_or_else(|| {
        Failure::new(
            code::no_such_record,
            format!("this host's audit has no record {id}"),
        )
    })?;
    let args = &record.args;
    let unnamed = || {
        Failure::new(
            code::no_such_shot,
            format!("record {id} names no screenshot {shot:?}"),
        )
    };
    // A plan's final shot, and one after each action.
    let steps = args["step_records"].as_array().into_iter().flatten();
    let meta = std::iter::once(&args["screenshot"])
        .chain(steps.map(|step| &step["screenshot"]))
        .find(|meta| meta["shot"] == shot)
        .filter(|_| record.op == "browser")
        .ok_or_else(unnamed)?;
    let (Some(session), Some(request), Some(sha256)) = (
        args["session"].as_str(),
        args["request"].as_str(),
        meta["sha256"].as_str(),
    ) else {
        return Err(unnamed());
    };
    // The audit names a session by the first 6 bytes of its id's sha256,
    // as letters (`a` for 0 to `p` for 15); its journal folder, by the
    // first 8 in hex.
    let prefix: Option<String> = session
        .bytes()
        .map(|letter| char::from_digit(u32::from(letter.wrapping_sub(b'a')), 16))
        .collect();
    let prefix = prefix.filter(|prefix| prefix.len() == 12);
    let (Some(prefix), true, true) = (prefix, plain(request), plain(shot)) else {
        return Err(unnamed());
    };
    let gone = || {
        let mut failure = Failure::new(
            code::no_such_shot,
            format!("record {id}'s screenshot {shot} is no longer kept"),
        );
        failure.next = Some("a request's screenshots are kept on its host for 7 days".into());
        failure
    };
    let mut changed = false;
    for folder in fs::read_dir(state_dir.join("browser/journal"))
        .map_err(|_| gone())?
        .flatten()
    {
        if !folder.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let Ok(png) = fs::read(folder.path().join(request).join(format!("{shot}.png"))) else {
            continue;
        };
        if to_hex(&Sha256::digest(&png)) == sha256 {
            return Ok(png);
        }
        changed = true;
    }
    match changed {
        true => Err(Failure::new(
            code::shot_changed,
            format!(
                "record {id}'s screenshot {shot} is not the one its record keeps the sha256 of"
            ),
        )),
        false => Err(gone()),
    }
}

/// Request `request`'s history, for its caller: its status, each step's
/// outcome and each action's result.
pub fn history(
    state_dir: &Path,
    session: &str,
    caller: &str,
    request: &str,
    running: bool,
) -> Result<Value, Failure> {
    let missing = || {
        Failure::new(
            code::no_such_request,
            format!("this session has no request {request}"),
        )
    };
    if !plain(request) {
        return Err(missing());
    }
    let text = fs::read_to_string(folder(state_dir, session).join(format!("{request}.jsonl")))
        .map_err(|_| missing())?;
    // A line cut off by a crash mid-write is skipped.
    let records: Vec<Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let Some(start) = records.first().filter(|r| r["t"] == "start") else {
        return Err(missing());
    };
    // Another caller's request reads as no request at all.
    if start["caller"] != caller {
        return Err(missing());
    }
    let mut steps: Vec<Value> = start["steps"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut step| {
            step["outcome"] = json!("not_run");
            step
        })
        .collect();
    let mut actions: Vec<Map<String, Value>> = Vec::new();
    let mut end = None;
    for record in &records[1..] {
        match record["t"].as_str() {
            Some("sending") => {
                let mut action = record.as_object().cloned().unwrap_or_default();
                action.remove("t");
                action.insert("result".into(), json!("unknown"));
                actions.push(action);
            }
            Some("sent") => {
                if let Some(action) = actions
                    .iter_mut()
                    .rev()
                    .find(|a| a["action"] == record["action"])
                {
                    action.insert("result".into(), record["result"].clone());
                    if !record["target"].is_null() {
                        action.insert("target".into(), record["target"].clone());
                    }
                }
            }
            Some("step") => {
                let n = record["step"].as_u64().unwrap_or(0) as usize;
                if let Some(step) = n.checked_sub(1).and_then(|at| steps.get_mut(at)) {
                    for (key, value) in record.as_object().into_iter().flatten() {
                        if key != "t" {
                            step[key] = value.clone();
                        }
                    }
                }
            }
            Some("end") => end = Some(record.clone()),
            _ => {}
        }
    }
    let mut out = json!({"request": request, "steps": steps, "actions": actions});
    match end {
        Some(end) => {
            out["status"] = end["status"].clone();
            if !end["stop"].is_null() {
                out["stop"] = end["stop"].clone();
            }
        }
        None => out["status"] = json!(if running { "running" } else { "interrupted" }),
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_action_with_no_result_reads_unknown_and_the_journal_is_the_callers() {
        let dir = tempfile::tempdir().unwrap();
        let steps = json!([{"step": 1, "op": "click"}, {"step": 2, "op": "type"}]);
        let mut journal = Journal::start(dir.path(), "default", "k1", "r1", steps).unwrap();
        journal
            .write(json!({"t": "sending", "step": 1, "action": 1, "op": "click"}))
            .unwrap();
        // The host stops here: the click's result was never recorded.
        let read = history(dir.path(), "default", "k1", "r1", false).unwrap();
        assert_eq!(read["status"], "interrupted");
        assert_eq!(read["actions"][0]["result"], "unknown");
        assert_eq!(read["steps"][1]["outcome"], "not_run");
        assert_eq!(
            history(dir.path(), "default", "k1", "r1", true).unwrap()["status"],
            "running"
        );
        assert_eq!(
            history(dir.path(), "default", "k2", "r1", false)
                .unwrap_err()
                .code,
            "no_such_request"
        );
        let again = Journal::start(dir.path(), "default", "k1", "r1", json!([]))
            .err()
            .unwrap();
        assert_eq!(again.code, "duplicate_request");

        journal
            .write(json!({"t": "sent", "step": 1, "action": 1, "result": "dispatched"}))
            .unwrap();
        journal
            .write(json!({"t": "step", "step": 1, "outcome": "dispatched"}))
            .unwrap();
        journal
            .write(
                json!({"t": "end", "status": "stopped", "stop": {"step": 2, "code": "deadline"}}),
            )
            .unwrap();
        let read = history(dir.path(), "default", "k1", "r1", false).unwrap();
        assert_eq!(read["status"], "stopped");
        assert_eq!(read["actions"][0]["result"], "dispatched");
        assert_eq!(read["steps"][0]["outcome"], "dispatched");
        assert_eq!(read["stop"]["code"], "deadline");
        save_shot(dir.path(), "default", "r1", "final", b"png").unwrap();
        assert_eq!(
            shot(dir.path(), "default", "k1", "r1", "final").unwrap(),
            b"png"
        );
        assert_eq!(
            shot(dir.path(), "default", "k2", "r1", "final")
                .unwrap_err()
                .code,
            "no_such_request"
        );
        assert_eq!(
            shot(dir.path(), "default", "k1", "r1", "../r1")
                .unwrap_err()
                .code,
            "no_such_shot"
        );
        let mode = fs::metadata(folder(dir.path(), "default").join("r1.jsonl"))
            .unwrap()
            .permissions();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(mode.mode() & 0o777, 0o600);
    }

    #[test]
    fn a_request_is_kept_seven_days_with_its_screenshots() {
        let dir = tempfile::tempdir().unwrap();
        Journal::start(dir.path(), "default", "k1", "r1", json!([])).unwrap();
        save_shot(dir.path(), "default", "r1", "1", b"png").unwrap();
        let now = SystemTime::now();
        let eight_days = now - Duration::from_secs(8 * 24 * 60 * 60);
        File::options()
            .write(true)
            .open(folder(dir.path(), "default").join("r1.jsonl"))
            .unwrap()
            .set_modified(eight_days)
            .unwrap();
        // A request from another session, sent today, stays.
        Journal::start(dir.path(), "other", "k1", "r2", json!([])).unwrap();
        assert!(history(dir.path(), "default", "k1", "r1", false).is_err());
        assert_eq!(
            shot(dir.path(), "default", "k1", "r1", "1")
                .unwrap_err()
                .code,
            "no_such_request"
        );
        assert!(!folder(dir.path(), "default").exists());
        assert!(history(dir.path(), "other", "k1", "r2", false).is_ok());
    }

    /// A browser call's audit record with `args`, in the log in `dir`: its id.
    fn audited_call(dir: &Path, op: &str, args: Value) -> String {
        let mut log = crate::audit::AuditLog::open(dir.join(crate::audit::FILE)).unwrap();
        let mut record = crate::audit::record("k1", Some("agent-1".into()), op, "allow", "ok");
        record.args = args;
        log.append(record).unwrap()
    }

    /// A kept shot as the audit records it.
    fn meta(shot: &str, png: &[u8]) -> Value {
        json!({"shot": shot, "size": png.len(), "sha256": to_hex(&Sha256::digest(png))})
    }

    #[test]
    fn the_owner_reads_only_a_shot_an_audit_record_names_with_the_bytes_it_audited() {
        let dir = tempfile::tempdir().unwrap();
        let (dir, session) = (dir.path(), "lkjhgfdsapoiuytrewqmnbvcxzlkjhgf");
        let hash = crate::browser::host::hash_of(session);
        save_shot(dir, session, "r1", "1", b"after the click").unwrap();
        save_shot(dir, session, "r1", "final", b"the confirmation").unwrap();
        let plan = audited_call(
            dir,
            "browser",
            json!({"command": "do", "session": hash, "request": "r1",
                   "step_records": [{"step": 1, "action": 1, "screenshot": meta("1", b"after the click")}],
                   "screenshot": meta("final", b"the confirmation")}),
        );
        assert_eq!(audited(dir, &plan, "1").unwrap(), b"after the click");
        assert_eq!(audited(dir, &plan, "final").unwrap(), b"the confirmation");

        let code = |id: &str, shot: &str| audited(dir, id, shot).unwrap_err().code;
        assert_eq!(code(&plan, "2"), "no_such_shot");
        assert_eq!(code(&plan, "../r1/1"), "no_such_shot");
        assert_eq!(code(&"0".repeat(64), "1"), "no_such_record");
        // A standalone screenshot went to its caller and was never kept
        // here: its record names no shot.
        let standalone = audited_call(
            dir,
            "browser",
            json!({"command": "screenshot", "session": hash, "size": 3, "sha256": "ab"}),
        );
        assert_eq!(code(&standalone, "final"), "no_such_shot");
        // Only a browser call's shots.
        let other = audited_call(
            dir,
            "exec",
            json!({"session": hash, "request": "r1", "screenshot": meta("final", b"the confirmation")}),
        );
        assert_eq!(code(&other, "final"), "no_such_shot");
        // Names that would lead out of the journal lead nowhere.
        for (session, request) in [
            ("../../../..", "r1"),
            (hash.as_str(), "../r1"),
            ("aaaaaaaaaaaa/", "r1"),
        ] {
            let tricky = audited_call(
                dir,
                "browser",
                json!({"command": "do", "session": session, "request": request,
                       "screenshot": meta("final", b"the confirmation")}),
            );
            assert_eq!(
                code(&tricky, "final"),
                "no_such_shot",
                "{session} {request}"
            );
        }

        // Bytes other than the ones audited are refused.
        save_shot(dir, session, "r1", "1", b"another page").unwrap();
        assert_eq!(code(&plan, "1"), "shot_changed");
        // And a shot pruned with its request is gone.
        fs::remove_dir_all(folder(dir, session).join("r1")).unwrap();
        let gone = audited(dir, &plan, "final").unwrap_err();
        assert_eq!(gone.code, "no_such_shot");
        assert!(gone.next.unwrap().contains("7 days"));
    }
}
