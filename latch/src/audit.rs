//! The host's audit log: one JSON record per line (NDJSON), append-only, in
//! the host's state directory. It never leaves the host.
//!
//! Each record carries two hashes: `hash`, the sha256 of the record itself
//! (written with `hash` empty), and `prev`, the previous record's `hash`
//! (64 zeros for the first). Changing a record breaks its own `hash`;
//! removing or reordering records breaks the next `prev`. Either way
//! `verify` finds it, the newest record included. The log is readable by the
//! host's user only.
//!
//! Records hold who called, what they asked, the decision and the outcome;
//! never file contents, bodies, command output or tokens.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::failure::Failure;
use crate::state::to_hex;

/// The `prev` of the very first record.
const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// One audit record, in the order its fields appear on the line.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Record {
    pub time: String,
    /// The caller's key, from the connection itself.
    pub caller: String,
    /// The caller's agent uid, when Plow says the key is an agent's.
    pub agent: Option<String>,
    pub op: String,
    pub args: Value,
    /// `"allow"` or `"deny"`.
    pub decision: String,
    pub outcome: String,
    /// The gatekeeper's judgment of an agent's operation, when it judged
    /// one: verdict, probabilities, model, the chat window's ids, rules
    /// revisions. Never chat or rules text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<Value>,
    pub prev: String,
    /// This record's own sha256; also its `audit_id` in the host's answer.
    pub hash: String,
}

pub struct AuditLog {
    path: PathBuf,
    /// The last record's `hash`, for the next record's `prev`.
    last_hash: String,
    records: usize,
}

impl AuditLog {
    /// Opens (or starts) the log and checks the whole chain first. A broken
    /// chain is an error: the host refuses to run on a tampered log.
    pub fn open(path: PathBuf) -> Result<AuditLog, Failure> {
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(io_failure(&path, err)),
        };
        let (records, last_hash) = verify(&text).map_err(|problem| {
            let message = format!("{}: {problem}", path.display());
            Failure::failed("audit_broken", message)
        })?;
        Ok(AuditLog {
            path,
            last_hash,
            records,
        })
    }

    pub fn len(&self) -> usize {
        self.records
    }

    /// Appends one record: fills in `prev` and `hash`, writes the line, and
    /// flushes it to disk before returning, so a record is never lost to a
    /// crash. Returns the record's hash.
    pub fn append(&mut self, mut record: Record) -> Result<String, Failure> {
        record.prev = self.last_hash.clone();
        record.hash = hash_of(&record);
        let line = serde_json::to_string(&record).expect("a record serializes");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
            .map_err(|err| io_failure(&self.path, err))?;
        file.write_all(format!("{line}\n").as_bytes())
            .and_then(|()| file.sync_data())
            .map_err(|err| io_failure(&self.path, err))?;
        self.last_hash = record.hash.clone();
        self.records += 1;
        Ok(record.hash)
    }
}

/// Checks every record's own `hash`, and its `prev` against the record
/// before it. Returns the number of records and the last hash, or what is
/// wrong, by line number.
pub fn verify(text: &str) -> Result<(usize, String), String> {
    let mut expected_prev = GENESIS.to_string();
    let mut count = 0;
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let record: Record = serde_json::from_str(line)
            .map_err(|_| format!("line {number} is not an audit record"))?;
        if hash_of(&record) != record.hash {
            return Err(format!(
                "line {number} was changed: its hash does not match"
            ));
        }
        if record.prev != expected_prev {
            return Err(format!(
                "chain broken at line {number}: it does not follow line {index}"
            ));
        }
        expected_prev = record.hash;
        count += 1;
    }
    Ok((count, expected_prev))
}

/// The sha256 of `record` serialized with its `hash` field empty.
fn hash_of(record: &Record) -> String {
    let mut blank = record.clone();
    blank.hash = String::new();
    let json = serde_json::to_string(&blank).expect("a record serializes");
    to_hex(&Sha256::digest(json.as_bytes()))
}

/// A record to append, with the time stamped now and `prev` filled in later.
/// `args` starts empty: each operation fills in only what it may record
/// (a file's path, size and sha256; never an email body or search query).
pub fn record(
    caller: &str,
    agent: Option<String>,
    op: &str,
    decision: &str,
    outcome: &str,
) -> Record {
    Record {
        time: jiff::Timestamp::now().to_string(),
        caller: caller.to_string(),
        agent,
        op: op.to_string(),
        args: Value::Null,
        decision: decision.to_string(),
        outcome: outcome.to_string(),
        gate: None,
        prev: String::new(),
        hash: String::new(),
    }
}

fn io_failure(path: &Path, err: std::io::Error) -> Failure {
    Failure::failed("audit_io", format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log_with_three_records() -> (tempfile::TempDir, PathBuf) {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        fs::create_dir_all(&parent).unwrap();
        let dir = tempfile::tempdir_in(parent).unwrap();
        let path = dir.path().join("audit.ndjson");
        let mut log = AuditLog::open(path.clone()).unwrap();
        for op in ["connect", "exec", "file get"] {
            let entry = record("abc", None, op, "allow", "ok");
            log.append(entry).unwrap();
        }
        (dir, path)
    }

    #[test]
    fn an_untouched_log_verifies_and_reopens() {
        let (_dir, path) = log_with_three_records();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(verify(&text).unwrap().0, 3);
        assert_eq!(AuditLog::open(path).unwrap().len(), 3);
    }

    #[test]
    fn an_edited_record_is_caught_even_the_newest() {
        let (_dir, path) = log_with_three_records();
        let text = fs::read_to_string(&path).unwrap();
        for (from, to, problem) in [
            (
                "\"exec\"",
                "\"oops\"",
                "line 2 was changed: its hash does not match",
            ),
            (
                "\"file get\"",
                "\"oops\"",
                "line 3 was changed: its hash does not match",
            ),
        ] {
            fs::write(&path, text.replacen(from, to, 1)).unwrap();
            let failure = AuditLog::open(path.clone()).err().unwrap();
            assert_eq!(failure.code, "audit_broken");
            assert!(failure.message.ends_with(problem), "{}", failure.message);
        }
    }

    #[test]
    fn a_removed_record_breaks_the_chain() {
        let (_dir, path) = log_with_three_records();
        let text = fs::read_to_string(&path).unwrap();
        let without_second: Vec<&str> = text
            .lines()
            .enumerate()
            .filter(|(i, _)| *i != 1)
            .map(|(_, l)| l)
            .collect();
        fs::write(&path, without_second.join("\n") + "\n").unwrap();
        let failure = AuditLog::open(path).err().unwrap();
        assert!(
            failure
                .message
                .ends_with("chain broken at line 2: it does not follow line 1")
        );
    }
}
