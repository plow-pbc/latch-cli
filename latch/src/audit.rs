//! The host's audit log: one JSON line per record (NDJSON), append-only, in
//! the host's state directory. It never leaves the host.
//!
//! Each line holds one record sealed with AES-256-GCM under an audit key
//! (`Keys`): its `body` is the whole record (when, who called, what they
//! asked, the decision, the outcome, the gatekeeper's judgment), unreadable
//! without the key. In the clear are only what checks the chain: the key's
//! id (`kid`), the `nonce`, `prev` and `hash`. `hash` is the sha256 of the
//! line written with `hash` empty, and `prev` is the line before's `hash`
//! (64 zeros for the first). So the chain verifies without the key: a
//! changed, removed or reordered line is caught, the newest included. The
//! body is sealed with `kid` and `prev` as associated data, so it can't be
//! moved to another line either. The log is readable by the host's user
//! only.
//!
//! The keys are the state folder's secret (see `secrets.rs`), and rotate:
//! `latch audit rotate-key` starts a new key and keeps the old ones, so every
//! record still reads. A running host seals its next record with the new key.
//!
//! The log is `audit.sealed.ndjson`. A plaintext `audit.ndjson` an older
//! latch left is neither read nor changed: a fresh sealed log starts beside
//! it, and `latch status --local` names it (`old_audit_log`): it can be
//! deleted.
//!
//! Records hold who called, what they asked (an exec's whole argv), the
//! decision and the outcome; never file contents, command output or
//! tokens. What shows records to people hides credential-looking values
//! (`redact.rs`).

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::cli::AuditAction;
use crate::failure::{Failure, code};
use crate::seal::from_hex;
use crate::secrets::{Secret, Secrets};
use crate::state::{StateDir, to_hex};

/// The `prev` of the very first record.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// The sealed log, in the state folder.
pub const FILE: &str = "audit.sealed.ndjson";
/// The plaintext log an older latch kept, which this one never reads.
const PLAINTEXT: &str = "audit.ndjson";

/// One audit record as read: its sealed body, and its line's hashes.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Record {
    /// When the record was made, as the operation ended.
    pub time: String,
    /// When the operation began, for an operation the board showed running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<String>,
    /// The caller's key, from the connection itself.
    pub caller: String,
    /// The caller's agent uid, when Plow says the key is an agent's.
    pub agent: Option<String>,
    /// The caller's name as it was when it called: an agent's name in
    /// Plow, else its machine's. A label only; `caller` and `agent` are
    /// who it was.
    #[serde(default)]
    pub caller_name: Option<String>,
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
    /// The line's `prev`.
    #[serde(skip)]
    pub prev: String,
    /// The line's own sha256; also the record's `audit_id` in the host's answer.
    #[serde(skip)]
    pub hash: String,
}

/// One line on disk.
#[derive(Serialize, Deserialize, Clone)]
struct Line {
    kid: String,
    nonce: String,
    body: String,
    prev: String,
    hash: String,
}

/// The audit keys, by id, and which one seals new records.
pub struct Keys {
    current: String,
    keys: BTreeMap<String, [u8; 32]>,
}

#[derive(Serialize, Deserialize)]
struct Ring {
    current: String,
    keys: BTreeMap<String, String>,
}

impl Keys {
    /// The state folder's audit keys, made on first use. Only with the
    /// audit lock held (`lock`), so no two processes make them at once, and
    /// written whole (see `state::write_private`).
    fn ready(dir: &Path) -> Result<Keys, Failure> {
        let secrets = Secrets::open(dir)?;
        if let Some(keys) = Keys::read(&secrets)? {
            return Ok(keys);
        }
        let first = Ring {
            current: "k1".into(),
            keys: BTreeMap::from([("k1".into(), to_hex(&rand::random::<[u8; 32]>()))]),
        };
        secrets.write(
            Secret::AuditKeys,
            &serde_json::to_vec(&first).expect("keys serialize"),
        )?;
        Keys::read(&secrets)?.ok_or_else(|| failed("the audit keys could not be made".into()))
    }

    /// The state folder's audit keys, if it has any.
    pub fn existing(dir: &Path) -> Result<Option<Keys>, Failure> {
        Keys::read(&Secrets::open(dir)?)
    }

    /// Starts a new key for new records and keeps every old one. Returns
    /// the ids, newest last.
    pub fn rotate(dir: &Path) -> Result<Vec<String>, Failure> {
        let _lock = lock(dir)?;
        let secrets = Secrets::open(dir)?;
        let Some(bytes) = secrets.read(Secret::AuditKeys)? else {
            return Err(failed("this state folder has no audit keys yet".into()));
        };
        let mut ring: Ring = serde_json::from_slice(&bytes).map_err(|_| damaged())?;
        let next = (1..)
            .map(|n| format!("k{n}"))
            .find(|id| !ring.keys.contains_key(id))
            .expect("an unused id");
        ring.keys
            .insert(next.clone(), to_hex(&rand::random::<[u8; 32]>()));
        ring.current = next;
        secrets.write(
            Secret::AuditKeys,
            &serde_json::to_vec(&ring).expect("keys serialize"),
        )?;
        let mut ids: Vec<String> = ring.keys.into_keys().collect();
        ids.sort_by_key(|id| id[1..].parse::<u64>().unwrap_or(0));
        Ok(ids)
    }

    fn read(secrets: &Secrets) -> Result<Option<Keys>, Failure> {
        let Some(bytes) = secrets.read(Secret::AuditKeys)? else {
            return Ok(None);
        };
        let ring: Ring = serde_json::from_slice(&bytes).map_err(|_| damaged())?;
        let mut keys = BTreeMap::new();
        for (id, hex) in ring.keys {
            let key: [u8; 32] = from_hex(&hex)
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(damaged)?;
            keys.insert(id, key);
        }
        if !keys.contains_key(&ring.current) {
            return Err(damaged());
        }
        Ok(Some(Keys {
            current: ring.current,
            keys,
        }))
    }

    /// Seals `record` as the line after `prev`.
    fn seal(&self, record: &Record, prev: &str) -> Line {
        let body = serde_json::to_vec(record).expect("a record serializes");
        let sealed = crate::seal::seal(&self.keys[&self.current], &body, &aad(&self.current, prev));
        let mut line = Line {
            kid: self.current.clone(),
            nonce: sealed.nonce,
            body: sealed.ciphertext,
            prev: prev.to_string(),
            hash: String::new(),
        };
        line.hash = hash_of(&line);
        line
    }

    /// The record a verified line holds, by number for messages.
    fn open(&self, line: &Line, number: usize) -> Result<Record, String> {
        let Some(key) = self.keys.get(&line.kid) else {
            return Err(format!(
                "line {number} is sealed with key {}, which this host does not have",
                line.kid
            ));
        };
        let unreadable = || format!("line {number} does not open with its key");
        let body = crate::seal::open(key, &line.nonce, &line.body, &aad(&line.kid, &line.prev))
            .ok_or_else(unreadable)?;
        let mut record: Record = serde_json::from_slice(&body).map_err(|_| unreadable())?;
        record.prev = line.prev.clone();
        record.hash = line.hash.clone();
        Ok(record)
    }
}

fn aad(kid: &str, prev: &str) -> Vec<u8> {
    format!("latch audit v1|{kid}|{prev}").into_bytes()
}

pub struct AuditLog {
    path: PathBuf,
    records: usize,
    /// The last calls in the log as opened (no connections), oldest
    /// first, for `take_recent`.
    recent: Vec<Record>,
}

/// How many of the log's last calls a host's board starts with.
const RECENT_CALLS: usize = 200;

impl AuditLog {
    /// Opens (or starts) the log and checks it whole first: the chain, and
    /// every record opened with its key. A broken chain or a record that
    /// does not open is an error: the host refuses to run on a tampered
    /// log. It is opened for appending, so a log that can't be
    /// written to is an error now, before anything that needs it runs, and
    /// read under the audit lock (`lock`), so a record another process is
    /// appending, or a conversion, is read whole or not at all.
    pub fn open(path: PathBuf) -> Result<AuditLog, Failure> {
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let _lock = lock(&dir)?;
        let mut text = String::new();
        fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut file| file.read_to_string(&mut text))
            .map_err(|err| io_failure(&path, err))?;
        let keys = match Keys::existing(&dir)? {
            Some(keys) => keys,
            None if text.is_empty() => Keys::ready(&dir)?,
            None => {
                // A broken chain is the first thing to say, key or not.
                verify(&text).map_err(|problem| broken(&path, &problem))?;
                return Err(no_key(&path));
            }
        };
        let all = open_all(&keys, &text, &path)?;
        let records = all.len();
        let mut recent: Vec<Record> = all
            .into_iter()
            .rev()
            .filter(|record| record.op != "connect")
            .take(RECENT_CALLS)
            .collect();
        recent.reverse();
        Ok(AuditLog {
            path,
            records,
            recent,
        })
    }

    pub fn len(&self) -> usize {
        self.records
    }

    /// The last calls the log held when it was opened, oldest first: at
    /// most `RECENT_CALLS`, and connections left out. Given once.
    pub fn take_recent(&mut self) -> Vec<Record> {
        std::mem::take(&mut self.recent)
    }

    /// Appends one record: seals it after the file's last line, writes it,
    /// and flushes it to disk before returning, so a record is never lost
    /// to a crash. Returns its hash. Another process may append to the same
    /// log (the owner's local `latch rules`), so the record chains onto the
    /// file's own last line, read under the audit lock, and is sealed with
    /// the keys as they are now, so a `latch audit rotate-key` since takes
    /// effect at once. The log is opened only once the lock is held, so it
    /// is never a file a conversion has since replaced.
    pub fn append(&mut self, record: Record) -> Result<String, Failure> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        let _lock = lock(dir)?;
        let keys = Keys::existing(dir)?.ok_or_else(|| no_key(&self.path))?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
            .map_err(|err| io_failure(&self.path, err))?;
        let prev = last_hash(&mut file).map_err(|err| io_failure(&self.path, err))?;
        let line = keys.seal(&record, &prev);
        let text = serde_json::to_string(&line).expect("a line serializes");
        file.write_all(format!("{text}\n").as_bytes())
            .and_then(|()| file.sync_data())
            .map_err(|err| io_failure(&self.path, err))?;
        self.records += 1;
        Ok(line.hash)
    }
}

/// `latch audit …` on this machine's host state.
pub fn run(state: &StateDir, action: AuditAction) -> Result<Value, Failure> {
    let path = state.path.join(FILE);
    match action {
        AuditAction::Show { .. } => {
            let records: Vec<Value> = read_all(&path)?
                .into_iter()
                .map(|record| {
                    let mut shown = serde_json::to_value(&record).expect("a record serializes");
                    shown["id"] = json!(record.hash);
                    shown
                })
                .collect();
            Ok(json!({"records": records}))
        }
        AuditAction::Verify => {
            let text = read_locked(&path)?;
            let (records, last) = verify(&text).map_err(|problem| broken(&path, &problem))?;
            Ok(json!({"records": records, "last": last, "checked_without_the_key": true}))
        }
        AuditAction::RotateKey => {
            let keys = Keys::rotate(&state.path)?;
            Ok(json!({"current": keys.last(), "keys": keys,
                      "note": "new records use the new key, a running latch serve's included"}))
        }
    }
}

/// The `hash` of the log's last line, read from the end of the file.
fn last_hash(file: &mut File) -> std::io::Result<String> {
    let size = file.metadata()?.len();
    let mut window = 4096;
    loop {
        let start = size.saturating_sub(window);
        file.seek(SeekFrom::Start(start))?;
        let mut tail = Vec::new();
        file.read_to_end(&mut tail)?;
        let lines = tail.strip_suffix(b"\n").unwrap_or(&tail);
        let last = match lines.iter().rposition(|&b| b == b'\n') {
            Some(at) => &lines[at + 1..],
            None if start == 0 => lines,
            None => {
                window *= 2;
                continue;
            }
        };
        if last.is_empty() {
            return Ok(GENESIS.to_string());
        }
        let line: Line = serde_json::from_slice(last)
            .map_err(|_| std::io::Error::other("its last line is not an audit record"))?;
        return Ok(line.hash);
    }
}

/// Checks, without the key, every line's own `hash` and its `prev`
/// against the line before. Returns the number of lines and the last
/// hash, or what is wrong, by line number.
pub fn verify(text: &str) -> Result<(usize, String), String> {
    let mut prev = GENESIS.to_string();
    let mut count = 0;
    for (index, text) in text.lines().enumerate() {
        prev = verify_line(&prev, text.as_bytes(), index + 1)?.hash;
        count += 1;
    }
    Ok((count, prev))
}

/// Checks line `number` as the one after the line whose hash is `prev`
/// (`GENESIS` for the first).
fn verify_line(prev: &str, text: &[u8], number: usize) -> Result<Line, String> {
    let line: Line = serde_json::from_slice(text)
        .map_err(|_| format!("line {number} is not an audit record"))?;
    if hash_of(&line) != line.hash {
        return Err(format!(
            "line {number} was changed: its hash does not match"
        ));
    }
    if line.prev != prev {
        let index = number - 1;
        return Err(format!(
            "chain broken at line {number}: it does not follow line {index}"
        ));
    }
    Ok(line)
}

/// Checks line `number` as `verify_line` does, then opens it with `keys`.
pub fn read_next(keys: &Keys, prev: &str, text: &[u8], number: usize) -> Result<Record, String> {
    let line = verify_line(prev, text, number)?;
    keys.open(&line, number)
}

/// `latch audit show --follow`: after the `hello` line, every record, one
/// `record` line each, then each new one as it is written, until stopped (or, when stdin is a pipe, until
/// it closes). A log that fails its checks, or no longer begins with what
/// was printed, stops it with `audit_broken`.
pub fn follow(state: &StateDir) -> Result<(), Failure> {
    let path = state.path.join(FILE);
    let watching = |err: std::io::Error| {
        Failure::new(
            code::watch_failed,
            format!("{}: {err}", state.path.display()),
        )
    };
    state.create()?;
    let mut watch = crate::watch::Watch::new(
        &state.path,
        std::slice::from_ref(&path),
        crate::watch::stdin_is_a_pipe(),
    )
    .map_err(watching)?;
    let mut printed: Option<(usize, String)> = None;
    let mut out = std::io::stdout().lock();
    let hello = crate::events::hello();
    if writeln!(out, "{hello}").and_then(|()| out.flush()).is_err() {
        return Ok(());
    }
    loop {
        watch.arm().map_err(watching)?;
        let records = read_all(&path)?;
        let from = match &printed {
            None => 0,
            Some((count, hash)) if records.get(count - 1).is_some_and(|r| &r.hash == hash) => {
                *count
            }
            Some(_) => return Err(broken(&path, "it no longer begins with what was shown")),
        };
        for record in &records[from..] {
            let mut shown = serde_json::to_value(record).expect("a record serializes");
            shown["type"] = json!("record");
            shown["id"] = json!(record.hash);
            if writeln!(out, "{shown}").and_then(|()| out.flush()).is_err() {
                return Ok(());
            }
        }
        if let Some(last) = records.last() {
            printed = Some((records.len(), last.hash.clone()));
        }
        match watch.wait().map_err(watching)? {
            crate::watch::Woke::StdinClosed => return Ok(()),
            crate::watch::Woke::Changed => {}
        }
    }
}

/// The log's text, read under the audit lock, so a record being appended
/// is read whole or not at all and what is read is the log as written: a
/// last line cut short fails its checks. A log not started yet is empty.
fn read_locked(path: &Path) -> Result<String, Failure> {
    if !path.exists() {
        return Ok(String::new());
    }
    // Shared, through a read-only descriptor: closing it is no write, so
    // a follower watching the folder is not woken by its own reads.
    let lock = path.parent().unwrap_or(Path::new(".")).join(LOCK);
    let _lock = File::open(&lock)
        .and_then(|file| file.lock_shared().map(|()| file))
        .map_err(|err| io_failure(&lock, err))?;
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(io_failure(path, err)),
    }
}

/// Every record of the log at `path`: the chain checked, each record opened.
pub fn read_all(path: &Path) -> Result<Vec<Record>, Failure> {
    let text = read_locked(path)?;
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let keys = Keys::existing(dir)?.ok_or_else(|| no_key(path))?;
    open_all(&keys, &text, path)
}

/// Every record of the log `text` (at `path`, for messages), each line's
/// chain checked and its record opened with `keys`.
fn open_all(keys: &Keys, text: &str, path: &Path) -> Result<Vec<Record>, Failure> {
    let mut prev = GENESIS.to_string();
    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let record = read_next(keys, &prev, line.as_bytes(), index + 1)
            .map_err(|problem| broken(path, &problem))?;
        prev = record.hash.clone();
        records.push(record);
    }
    Ok(records)
}

/// The sha256 of `line` serialized with its `hash` field empty.
fn hash_of(line: &Line) -> String {
    let mut blank = line.clone();
    blank.hash = String::new();
    let json = serde_json::to_string(&blank).expect("a line serializes");
    to_hex(&Sha256::digest(json.as_bytes()))
}

/// A record to append, with the time stamped now.
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
        started: None,
        caller: caller.to_string(),
        agent,
        caller_name: None,
        op: op.to_string(),
        args: Value::Null,
        decision: decision.to_string(),
        outcome: outcome.to_string(),
        gate: None,
        prev: String::new(),
        hash: String::new(),
    }
}

/// An older latch's plaintext audit log in the folder, if there is one:
/// this latch never reads it, and it can be deleted.
pub fn plaintext(dir: &Path) -> Option<PathBuf> {
    let old = dir.join(PLAINTEXT);
    old.exists().then_some(old)
}

/// The file everything that writes the log, or rotates or first makes its
/// keys, holds locked: `audit.lock` beside it.
const LOCK: &str = "audit.lock";

/// Takes the audit lock, exclusive, until the returned file is dropped.
fn lock(dir: &Path) -> Result<File, Failure> {
    let path = dir.join(LOCK);
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|err| io_failure(&path, err))?;
    file.lock().map_err(|err| io_failure(&path, err))?;
    Ok(file)
}

fn damaged() -> Failure {
    failed("the audit keys are damaged".into())
}

fn no_key(path: &Path) -> Failure {
    failed(format!(
        "{}: there is no key to read it with",
        path.display()
    ))
}

fn failed(message: String) -> Failure {
    Failure::new(code::audit_key, message)
}

fn broken(path: &Path, problem: &str) -> Failure {
    Failure::new(code::audit_broken, format!("{}: {problem}", path.display()))
}

fn io_failure(path: &Path, err: std::io::Error) -> Failure {
    Failure::new(code::audit_io, format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir() -> tempfile::TempDir {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        fs::create_dir_all(&parent).unwrap();
        tempfile::tempdir_in(parent).unwrap()
    }

    const SECRET: &str = "synthetic-credential-5d1e";

    fn exec_record(caller: &str) -> Record {
        let mut entry = record(caller, None, "exec", "allow", "ok");
        entry.args = json!({"argv": ["curl", "-H", format!("Authorization: Bearer {SECRET}")]});
        entry
    }

    fn log_with_three_records() -> (tempfile::TempDir, PathBuf) {
        let dir = test_dir();
        let path = dir.path().join(FILE);
        let mut log = AuditLog::open(path.clone()).unwrap();
        log.append(record("caller-one", None, "connect", "allow", "connected"))
            .unwrap();
        log.append(exec_record("caller-one")).unwrap();
        log.append(record("caller-two", None, "file_get", "allow", "ok"))
            .unwrap();
        (dir, path)
    }

    #[test]
    fn a_line_on_disk_holds_no_record_content_and_the_key_reads_it_all() {
        let (_dir, path) = log_with_three_records();
        let text = fs::read_to_string(&path).unwrap();
        for content in [
            "connect",
            "exec",
            "file_get",
            "caller-one",
            "allow",
            SECRET,
            "curl",
        ] {
            assert!(!text.contains(content), "{content} is on disk in the clear");
        }
        // The chain checks without the key...
        assert_eq!(verify(&text).unwrap().0, 3);
        // ...and the key opens every record, the command whole.
        let records = read_all(&path).unwrap();
        let ops: Vec<&str> = records.iter().map(|r| r.op.as_str()).collect();
        assert_eq!(ops, ["connect", "exec", "file_get"]);
        assert_eq!(
            records[1].args["argv"][2],
            format!("Authorization: Bearer {SECRET}")
        );
        assert_eq!(AuditLog::open(path).unwrap().len(), 3);
    }

    #[test]
    fn a_changed_moved_or_removed_line_is_caught() {
        let (_dir, path) = log_with_three_records();
        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();

        // A changed body breaks its line's hash, without the key.
        let mut line: Line = serde_json::from_str(lines[2]).unwrap();
        line.body
            .replace_range(0..2, if &line.body[0..2] == "00" { "01" } else { "00" });
        let changed =
            [lines[0], lines[1], &serde_json::to_string(&line).unwrap()].join("\n") + "\n";
        assert_eq!(
            verify(&changed).unwrap_err(),
            "line 3 was changed: its hash does not match"
        );

        // A body moved onto another line, its hash made to fit, does not
        // open there: the line it was sealed for is part of the seal.
        let first: Line = serde_json::from_str(lines[0]).unwrap();
        let mut moved: Line = serde_json::from_str(lines[1]).unwrap();
        moved.body = first.body.clone();
        moved.nonce = first.nonce.clone();
        moved.hash = hash_of(&moved);
        fs::write(
            &path,
            [lines[0], &serde_json::to_string(&moved).unwrap()].join("\n") + "\n",
        )
        .unwrap();
        assert!(verify(&fs::read_to_string(&path).unwrap()).is_ok());
        for refused in [
            read_all(&path).unwrap_err(),
            AuditLog::open(path.clone()).err().unwrap(),
        ] {
            assert_eq!(refused.code, "audit_broken");
            assert!(
                refused
                    .message
                    .ends_with("line 2 does not open with its key"),
                "{}",
                refused.message
            );
        }

        // A removed line breaks the chain.
        fs::write(&path, [lines[0], lines[2]].join("\n") + "\n").unwrap();
        let refused = AuditLog::open(path).err().unwrap();
        assert!(
            refused
                .message
                .ends_with("chain broken at line 2: it does not follow line 1")
        );
    }

    #[test]
    fn two_writers_on_one_log_keep_one_chain() {
        // A running host and a local `latch rules` command both append.
        let (_dir, path) = log_with_three_records();
        let mut host = AuditLog::open(path.clone()).unwrap();
        let mut command = AuditLog::open(path.clone()).unwrap();
        host.append(record("abc", None, "exec", "allow", "ok"))
            .unwrap();
        command
            .append(record("def", None, "rules", "allow", "ok"))
            .unwrap();
        host.append(record("abc", None, "exec", "allow", "ok"))
            .unwrap();
        assert_eq!(read_all(&path).unwrap().len(), 6);
    }

    #[test]
    fn records_under_an_old_key_still_read_after_a_rotation_and_a_restart() {
        let (dir, path) = log_with_three_records();
        assert_eq!(Keys::rotate(dir.path()).unwrap(), ["k1", "k2"]);
        // A restart: the log opened afresh seals with the new key.
        let mut log = AuditLog::open(path.clone()).unwrap();
        log.append(exec_record("caller-three")).unwrap();
        let kids: Vec<String> = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Line>(line).unwrap().kid)
            .collect();
        assert_eq!(kids, ["k1", "k1", "k1", "k2"]);
        let records = read_all(&path).unwrap();
        assert_eq!(records.len(), 4);
        assert_eq!(records[3].caller, "caller-three");
        assert_eq!(records[1].caller, "caller-one");
        assert!(verify(&fs::read_to_string(&path).unwrap()).is_ok());
    }

    #[test]
    fn a_log_already_open_seals_with_the_new_key_after_a_rotation() {
        let (dir, path) = log_with_three_records();
        let mut host = AuditLog::open(path.clone()).unwrap();
        Keys::rotate(dir.path()).unwrap();
        host.append(exec_record("caller-three")).unwrap();
        let last = fs::read_to_string(&path).unwrap();
        let last = serde_json::from_str::<Line>(last.lines().last().unwrap()).unwrap();
        assert_eq!(last.kid, "k2");
        assert_eq!(read_all(&path).unwrap()[3].caller, "caller-three");
    }
}
