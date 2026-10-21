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

use std::fs::File;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::audit::Record;
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
    let audit = state.path.join("audit.ndjson");
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

    /// The whole log read, keeping its last `REPLAY` records.
    fn fresh(&mut self) -> Result<Vec<Record>, Failure> {
        self.log = Tail::default();
        let mut records = self.read_new()?.unwrap_or_default();
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
    /// then the host's step. A log that no longer continues what was sent
    /// starts over with `reset` and a snapshot.
    fn changes(&mut self) -> Result<Vec<Value>, Failure> {
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
            None => {
                self.host = lifecycle::now(&self.dir);
                let entries = self.fresh()?;
                return Ok(vec![json!({"type": "reset"}), self.snapshot(entries)]);
            }
        }
        let now = lifecycle::now(&self.dir);
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

    /// The complete records written since the last read, or `None` when
    /// the log no longer continues what was read (shorter, or a record
    /// that does not chain on).
    fn read_new(&mut self) -> Result<Option<Vec<Record>>, Failure> {
        let path = self.dir.join("audit.ndjson");
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
        for line in bytes[..complete]
            .split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
        {
            let Ok(record) = serde_json::from_slice::<Record>(line) else {
                return Ok(None);
            };
            let chains = match &self.log.last {
                Some(last) => record.prev == *last,
                None => self.log.count == 0,
            };
            if !chains {
                return Ok(None);
            }
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
        let mut log = AuditLog::open(dir.join("audit.ndjson")).unwrap();
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
        let path = dir.path().join("audit.ndjson");
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

    /// The line `append` would write next, without writing it.
    fn fs_line(dir: &Path, op: &str) -> String {
        let path = dir.join("audit.ndjson");
        let before = std::fs::read_to_string(&path).unwrap();
        append(dir, op);
        let after = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, &before).unwrap();
        after[before.len()..].to_string()
    }
}
