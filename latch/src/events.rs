//! `latch events`: what the host on this state folder does, as JSON lines,
//! for an app. First a snapshot (the host's step and recent activity, with
//! a cursor), then with `--follow` each change as it happens: the host's
//! step (`lifecycle.rs`) and each new verified audit record. It asks no one
//! and never polls: it sleeps until the kernel says the folder or the host
//! changed (`watch.rs`).
//!
//! A cursor is `RUN:COUNT:HASH`: the host's run, how many audit records
//! came before, and the last one's hash. `--after CURSOR` picks up right
//! after it. A cursor of another run (the host restarted), or one the log
//! no longer matches (it was replaced) or too far behind, gets `reset` and
//! a fresh snapshot instead, so the app replaces what it holds.
//!
//! `latch logs` follows the host's log the same way.

use std::fs::File;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::audit::{self, Record};
use crate::cli::EventsArgs;
use crate::failure::Failure;
use crate::lifecycle::{self, Host};
use crate::state::StateDir;
use crate::watch::{self, Watch, Woke};

/// How many recent records a fresh snapshot replays.
const REPLAY: usize = 50;
/// How far behind a resumed cursor may be before it gets a fresh snapshot.
const RESUME_LIMIT: usize = 1000;
const NO_RECORD: &str = "0";

pub fn run(state: &StateDir, args: EventsArgs) -> Result<(), Failure> {
    // A folder that does not exist yet has a stopped host and no activity;
    // making it lets the watch start before the first login.
    state.create()?;
    let audit = state.path.join(audit::FILE);
    let watching = |err: std::io::Error| {
        Failure::failed("watch_failed", format!("{}: {err}", state.path.display()))
    };
    let mut watch =
        Watch::new(&state.path, &[audit], watch::stdin_is_a_pipe()).map_err(watching)?;
    let mut follower = Follower::new(&state.path, !args.lifecycle_only);
    let after = args.after.as_deref().and_then(Cursor::parse);
    let resumed = match &after {
        Some(cursor) => follower.resume(cursor),
        None => None,
    };
    let opening = match resumed {
        Some(entries) => vec![follower.snapshot(entries)],
        None if args.after.is_some() => {
            let entries = follower.fresh()?;
            vec![json!({"type": "reset"}), follower.snapshot(entries)]
        }
        None => {
            let entries = follower.fresh()?;
            vec![follower.snapshot(entries)]
        }
    };
    let mut out = std::io::stdout().lock();
    for line in opening {
        if !emit(&mut out, &line) {
            return Ok(());
        }
    }
    if !args.follow {
        return Ok(());
    }

    // Arm, look, then wait: what changes after the look wakes the wait.
    loop {
        watch.arm().map_err(watching)?;
        for line in follower.changes()? {
            if !emit(&mut out, &line) {
                return Ok(());
            }
        }
        watch.process(follower.host.pid);
        match watch.wait().map_err(watching)? {
            Woke::StdinClosed => return Ok(()),
            Woke::Changed => {}
        }
    }
}

/// `latch logs`: the host's log (`serve.log`); with `follow`, then what is
/// added to it, as it is.
pub fn logs(state: &StateDir, follow: bool) -> Result<(), Failure> {
    let path = state.path.join("serve.log");
    let mut watch = start_watch(state, &path)?;
    let mut offset = 0;
    let mut out = std::io::stdout().lock();
    loop {
        watch.arm().map_err(|err| watch_failed(state, err))?;
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                return Err(Failure::failed(
                    "log_io",
                    format!("{}: {err}", path.display()),
                ));
            }
        };
        // A log that was started over is printed from its start.
        if bytes.len() < offset {
            offset = 0;
        }
        if out
            .write_all(&bytes[offset..])
            .and_then(|()| out.flush())
            .is_err()
        {
            return Ok(());
        }
        offset = bytes.len();
        if !follow || !sleep(&mut watch, state)? {
            return Ok(());
        }
    }
}

fn start_watch(state: &StateDir, file: &Path) -> Result<Watch, Failure> {
    state.create()?;
    Watch::new(&state.path, &[file.to_path_buf()], watch::stdin_is_a_pipe())
        .map_err(|err| watch_failed(state, err))
}

/// Sleeps until the folder changes; false once whoever started us is gone.
fn sleep(watch: &mut Watch, state: &StateDir) -> Result<bool, Failure> {
    match watch.wait().map_err(|err| watch_failed(state, err))? {
        Woke::StdinClosed => Ok(false),
        Woke::Changed => Ok(true),
    }
}

fn watch_failed(state: &StateDir, err: std::io::Error) -> Failure {
    Failure::failed("watch_failed", format!("{}: {err}", state.path.display()))
}

/// Writes one line; false once nobody reads our stdout.
fn emit(out: &mut impl Write, line: &Value) -> bool {
    writeln!(out, "{line}").and_then(|()| out.flush()).is_ok()
}

struct Cursor {
    run: String,
    count: usize,
    hash: String,
}

impl Cursor {
    fn parse(text: &str) -> Option<Cursor> {
        let mut parts = text.rsplitn(3, ':');
        let hash = parts.next()?.to_string();
        let count = parts.next()?.parse().ok()?;
        let run = parts.next()?.to_string();
        Some(Cursor { run, count, hash })
    }
}

/// What has been sent: the host as last reported, and how far into the
/// audit log.
struct Follower {
    dir: PathBuf,
    activity: bool,
    host: Host,
    log: Tail,
}

/// A position in the audit log: records and bytes read, and the last
/// record's hash, which the next one must chain onto.
#[derive(Default)]
struct Tail {
    count: usize,
    offset: u64,
    last: Option<String>,
}

impl Follower {
    fn new(dir: &Path, activity: bool) -> Follower {
        Follower {
            dir: dir.to_path_buf(),
            activity,
            host: lifecycle::now(dir),
            log: Tail::default(),
        }
    }

    fn cursor(&self) -> String {
        let run = self.host.run.as_deref().unwrap_or("-");
        let hash = self
            .log
            .last
            .as_deref()
            .map_or(NO_RECORD, |hash| &hash[..16]);
        format!("{run}:{}:{hash}", self.log.count)
    }

    /// The whole log read, keeping its last `REPLAY` records. A log that
    /// fails its own checks from the first record on is refused, as the
    /// host refuses it.
    fn fresh(&mut self) -> Result<Vec<Record>, Failure> {
        self.log = Tail::default();
        let Some(mut records) = self.read_new()? else {
            let path = self.dir.join(audit::FILE);
            let message = format!("{}: the audit log fails its checks", path.display());
            return Err(Failure::failed("audit_broken", message));
        };
        let keep = records.len().saturating_sub(REPLAY);
        Ok(records.split_off(keep))
    }

    /// The records after `cursor`, if it is still this run's and the log
    /// still holds it within reach.
    fn resume(&mut self, cursor: &Cursor) -> Option<Vec<Record>> {
        if self.host.run.as_deref().unwrap_or("-") != cursor.run {
            return None;
        }
        self.log = Tail::default();
        let records = self.read_new().ok()??;
        let before = match cursor.count {
            0 => NO_RECORD,
            count => &records.get(count - 1)?.hash[..16],
        };
        if before != cursor.hash || records.len() - cursor.count > RESUME_LIMIT {
            return None;
        }
        Some(records[cursor.count..].to_vec())
    }

    fn snapshot(&self, records: Vec<Record>) -> Value {
        let activity: Vec<Value> = if self.activity {
            records.iter().map(entry).collect()
        } else {
            Vec::new()
        };
        json!({"type": "snapshot", "cursor": self.cursor(), "host": host(&self.host),
               "activity": activity})
    }

    /// Everything that changed since the last look, in order: new records,
    /// then the host's step. A host that restarted, or a log that no longer
    /// continues what was sent, starts over with `reset` and a snapshot.
    fn changes(&mut self) -> Result<Vec<Value>, Failure> {
        let now = lifecycle::now(&self.dir);
        if self.host.run.is_some() && now.run != self.host.run {
            // Another run: once it has written its first step, start over.
            // Until then (an instant), nothing is sent; its step wakes us.
            if now.run.is_some() {
                return self.restart(now);
            }
            if now.running {
                return Ok(Vec::new());
            }
        }
        let mut lines = Vec::new();
        match self.read_new()? {
            Some(records) => {
                let run = self.host.run.as_deref().unwrap_or("-").to_string();
                let before = self.log.count - records.len();
                for (index, record) in records.iter().enumerate() {
                    if self.activity {
                        let cursor = format!("{run}:{}:{}", before + index + 1, &record.hash[..16]);
                        lines.push(json!({"type": "activity", "cursor": cursor,
                                          "entry": entry(record)}));
                    }
                }
            }
            None => return self.restart(lifecycle::now(&self.dir)),
        }
        let reported = |host: &Host| (host.state, host.error.clone(), host.run.clone());
        if reported(&now) != reported(&self.host) {
            self.host = now;
            let mut line = host(&self.host);
            line["type"] = json!("host");
            line["cursor"] = json!(self.cursor());
            lines.push(line);
        } else {
            self.host = now;
        }
        Ok(lines)
    }

    /// `reset`, then a fresh snapshot of `now` and the log.
    fn restart(&mut self, now: Host) -> Result<Vec<Value>, Failure> {
        self.host = now;
        let entries = self.fresh()?;
        Ok(vec![json!({"type": "reset"}), self.snapshot(entries)])
    }

    /// The complete records written since the last read, or `None` when
    /// the log no longer continues what was read (shorter, or a record
    /// that was changed or does not chain on).
    fn read_new(&mut self) -> Result<Option<Vec<Record>>, Failure> {
        let path = self.dir.join(audit::FILE);
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return Ok((self.log.count == 0).then(Vec::new));
            }
            Err(err) => return Err(io_failure(&path, err)),
        };
        let io = |err| io_failure(&path, err);
        let size = file.metadata().map_err(io)?.len();
        if size < self.log.offset {
            return Ok(None);
        }
        file.seek(SeekFrom::Start(self.log.offset)).map_err(io)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(io)?;
        // A record being written is read once it ends its line.
        let complete = bytes
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |at| at + 1);
        let mut records = Vec::new();
        if complete == 0 {
            return Ok(Some(records));
        }
        // Read on each look, so a key rotated meanwhile is there too.
        let Some(keys) = audit::Keys::existing(&self.dir)? else {
            let message = format!("{}: there is no key to read it with", path.display());
            return Err(Failure::failed("audit_key", message));
        };
        for line in bytes[..complete]
            .split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
        {
            // The same checks as the host's: each line's own hash, and that
            // it follows the last one read (or opens the log); then its
            // record, opened with the key.
            let prev = self.log.last.as_deref().unwrap_or(audit::GENESIS);
            let Ok(record) = audit::read_next(&keys, prev, line, self.log.count + 1) else {
                return Ok(None);
            };
            self.log.last = Some(record.hash.clone());
            self.log.count += 1;
            records.push(record);
        }
        self.log.offset += complete as u64;
        Ok(Some(records))
    }
}

/// The host as events carry it: its step, and why it failed if it did.
fn host(host: &Host) -> Value {
    let mut line = json!({"state": host.state});
    if let Some(problem) = &host.error {
        line["error"] = json!(problem.message);
        line["error_code"] = json!(problem.code);
    }
    line
}

/// One activity entry: an audit record without its arguments. Its id is
/// the record's hash, the `audit_id` the caller got back.
fn entry(record: &Record) -> Value {
    json!({"id": record.hash, "time": record.time, "caller": record.caller,
           "caller_name": null, "agent": record.agent, "op": record.op,
           "decision": record.decision, "outcome": record.outcome})
}

fn io_failure(path: &Path, err: std::io::Error) -> Failure {
    Failure::failed("audit_io", format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{self, AuditLog};

    fn append(dir: &Path, op: &str) -> String {
        let mut log = AuditLog::open(dir.join(audit::FILE)).unwrap();
        log.append(audit::record("k1", None, op, "allow", "ok"))
            .unwrap()
    }

    #[test]
    fn a_half_written_record_waits_and_a_replaced_log_resets() {
        let dir = tempfile::tempdir().unwrap();
        append(dir.path(), "exec");
        let mut follower = Follower::new(dir.path(), true);
        assert_eq!(follower.fresh().unwrap().len(), 1);
        let cursor = Cursor::parse(&follower.cursor()).unwrap();
        assert_eq!((cursor.run.as_str(), cursor.count), ("-", 1));

        // Half a record: nothing yet; the rest of it: one entry.
        let path = dir.path().join(audit::FILE);
        let whole = fs_line(dir.path(), "file_get");
        let (start, rest) = whole.split_at(20);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(start.as_bytes())
            .unwrap();
        assert_eq!(follower.changes().unwrap(), Vec::<Value>::new());
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(rest.as_bytes())
            .unwrap();
        let lines = follower.changes().unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["entry"]["op"], "file_get");
        assert!(lines[0]["cursor"].as_str().unwrap().starts_with("-:2:"));

        // Another log in its place: start over.
        std::fs::remove_file(&path).unwrap();
        append(dir.path(), "vault");
        let lines = follower.changes().unwrap();
        assert_eq!(lines[0], json!({"type": "reset"}));
        assert_eq!(lines[1]["activity"][0]["op"], "vault");
        assert!(follower.resume(&cursor).is_none());
    }

    #[test]
    fn a_changed_record_is_never_sent_and_a_log_that_fails_its_checks_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        append(dir.path(), "exec");
        let mut follower = Follower::new(dir.path(), true);
        follower.fresh().unwrap();
        let path = dir.path().join(audit::FILE);

        // A record that chains on but was edited after it was hashed.
        let mut edited: Value = serde_json::from_str(&fs_line(dir.path(), "exec")).unwrap();
        let body = edited["body"].as_str().unwrap().to_string();
        let flipped = if body.starts_with('0') { "1" } else { "0" };
        edited["body"] = json!(format!("{flipped}{}", &body[1..]));
        let edited = format!("{edited}\n");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(edited.as_bytes())
            .unwrap();
        assert_eq!(follower.changes().unwrap_err().code, "audit_broken");

        // A first record that does not open the log.
        let mut record: Value = serde_json::from_str(
            std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        record["prev"] = json!("ab".repeat(32));
        std::fs::write(&path, format!("{record}\n")).unwrap();
        assert_eq!(
            Follower::new(dir.path(), true).fresh().unwrap_err().code,
            "audit_broken"
        );
    }

    #[test]
    fn an_entry_is_the_same_shape_as_before_and_carries_no_command() {
        let dir = tempfile::tempdir().unwrap();
        let secret = "synthetic-events-credential-2b9c";
        let mut log = AuditLog::open(dir.path().join(audit::FILE)).unwrap();
        let mut exec = audit::record("k1", Some("agent-uid".into()), "exec", "allow", "ok");
        exec.args = json!({"argv": ["curl", "-H", format!("Authorization: Bearer {secret}")]});
        let id = log.append(exec).unwrap();

        let mut follower = Follower::new(dir.path(), true);
        let entries = follower.fresh().unwrap();
        let line = follower.snapshot(entries);
        let entry = &line["activity"][0];
        assert_eq!(
            entry,
            &json!({"id": id, "time": entry["time"], "caller": "k1", "caller_name": null,
                    "agent": "agent-uid", "op": "exec", "decision": "allow", "outcome": "ok"})
        );
        assert!(!line.to_string().contains(secret), "{line}");
    }

    /// The line `append` would write next, without writing it.
    fn fs_line(dir: &Path, op: &str) -> String {
        let path = dir.join(audit::FILE);
        let before = std::fs::read_to_string(&path).unwrap();
        append(dir, op);
        let after = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, &before).unwrap();
        after[before.len()..].to_string()
    }
}
