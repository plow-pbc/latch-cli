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
//! record still reads. A running host picks up a new key when it restarts.
//!
//! A log converted from a plaintext one (state format 1, see `upgrade`)
//! opens with an anchor line, marked `anchor` in the clear, whose `prev` is
//! the old log's last hash. The old records follow it, each with the hashes
//! it had, so the old chain can still be checked, from the bodies, with the
//! key.
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

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::cli::AuditAction;
use crate::failure::Failure;
use crate::secrets::{Secret, Secrets};
use crate::state::{StateDir, to_hex};

/// The `prev` of the very first record.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// The op of a converted log's anchor record.
const UPGRADE_OP: &str = "audit_upgrade";

/// One audit record as read: its sealed body, and its line's hashes.
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
    /// A record converted from a plaintext log: the `prev` and `hash` it
    /// had there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was: Option<Was>,
    /// The line's `prev`.
    #[serde(skip)]
    pub prev: String,
    /// The line's own sha256; also the record's `audit_id` in the host's answer.
    #[serde(skip)]
    pub hash: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Was {
    pub prev: String,
    pub hash: String,
}

/// One line on disk.
#[derive(Serialize, Deserialize, Clone)]
struct Line {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    anchor: bool,
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
    /// audit lock held (`lock`), so no two processes make them at once.
    ///
    /// Keys that don't read (a write cut short by a latch before keys were
    /// written atomically, say) are made afresh only while no sealed line
    /// could need them: the log holds none. Otherwise it is an error.
    fn ready(dir: &Path) -> Result<Keys, Failure> {
        let secrets = Secrets::open(dir)?;
        match Keys::read(&secrets) {
            Ok(Some(keys)) => return Ok(keys),
            Ok(None) => {}
            Err(damaged) if has_sealed_lines(&dir.join("audit.ndjson"))? => return Err(damaged),
            Err(_) => {}
        }
        let first = Ring {
            current: "k1".into(),
            keys: BTreeMap::from([("k1".into(), to_hex(&rand::random::<[u8; 32]>()))]),
        };
        // Written whole or not at all (see `state::write_private`).
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
    fn seal(&self, record: &Record, prev: &str, anchor: bool) -> Line {
        let body = serde_json::to_vec(record).expect("a record serializes");
        let nonce: [u8; 12] = rand::random();
        let cipher = Aes256Gcm::new_from_slice(&self.keys[&self.current]).expect("a 32-byte key");
        let sealed = cipher
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &body,
                    aad: &aad(&self.current, prev),
                },
            )
            .expect("AES-GCM encryption does not fail");
        let mut line = Line {
            anchor,
            kid: self.current.clone(),
            nonce: to_hex(&nonce),
            body: to_hex(&sealed),
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
        let nonce: [u8; 12] = from_hex(&line.nonce)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(unreadable)?;
        let sealed = from_hex(&line.body).ok_or_else(unreadable)?;
        let cipher = Aes256Gcm::new_from_slice(key).expect("a 32-byte key");
        let body = cipher
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &sealed,
                    aad: &aad(&line.kid, &line.prev),
                },
            )
            .map_err(|_| unreadable())?;
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
    keys: Keys,
}

impl AuditLog {
    /// Opens (or starts) the log and checks the whole chain first, without
    /// the key. A broken chain is an error: the host refuses to run on a
    /// tampered log. It is opened for appending, so a log that can't be
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
        let (records, _) = verify(&text).map_err(|problem| broken(&path, &problem))?;
        let keys = Keys::ready(&dir)?;
        Ok(AuditLog {
            path,
            records,
            keys,
        })
    }

    pub fn len(&self) -> usize {
        self.records
    }

    /// Appends one record: seals it after the file's last line, writes it,
    /// and flushes it to disk before returning, so a record is never lost
    /// to a crash. Returns its hash. Another process may append to the same
    /// log (the owner's local `latch rules`), so the record chains onto the
    /// file's own last line, read under the audit lock. The log is opened
    /// only once the lock is held, so it is never a file a conversion has
    /// since replaced.
    pub fn append(&mut self, record: Record) -> Result<String, Failure> {
        let _lock = lock(self.path.parent().unwrap_or(Path::new(".")))?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
            .map_err(|err| io_failure(&self.path, err))?;
        let prev = last_hash(&mut file).map_err(|err| io_failure(&self.path, err))?;
        let line = self.keys.seal(&record, &prev, false);
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
    let path = state.path.join("audit.ndjson");
    match action {
        AuditAction::Show => {
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
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(err) if err.kind() == ErrorKind::NotFound => String::new(),
                Err(err) => return Err(io_failure(&path, err)),
            };
            let (records, last) = verify(&text).map_err(|problem| broken(&path, &problem))?;
            Ok(json!({"records": records, "last": last, "checked_without_the_key": true}))
        }
        AuditAction::RotateKey => {
            let keys = Keys::rotate(&state.path)?;
            Ok(json!({"current": keys.last(), "keys": keys,
                      "note": "new records use the new key; a running latch serve does once it restarts"}))
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
/// (`GENESIS` for the first; a converted log's anchor follows the old
/// log's last hash instead).
fn verify_line(prev: &str, text: &[u8], number: usize) -> Result<Line, String> {
    let line: Line = serde_json::from_slice(text)
        .map_err(|_| format!("line {number} is not an audit record"))?;
    if hash_of(&line) != line.hash {
        return Err(format!(
            "line {number} was changed: its hash does not match"
        ));
    }
    let opens_a_conversion = number == 1 && line.anchor;
    if line.prev != prev && !opens_a_conversion {
        let index = number - 1;
        return Err(format!(
            "chain broken at line {number}: it does not follow line {index}"
        ));
    }
    if line.anchor && number != 1 {
        return Err(format!(
            "line {number} is an anchor, which only a log's first line can be"
        ));
    }
    Ok(line)
}

/// Checks line `number` as `verify_line` does, then opens it with `keys`.
pub fn read_next(keys: &Keys, prev: &str, text: &[u8], number: usize) -> Result<Record, String> {
    let line = verify_line(prev, text, number)?;
    keys.open(&line, number)
}

/// Every record of the log at `path`: the chain checked, each record
/// opened, and a converted log's old chain checked from its records.
pub fn read_all(path: &Path) -> Result<Vec<Record>, Failure> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(io_failure(path, err)),
    };
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let Some(keys) = Keys::existing(dir)? else {
        return Err(failed(format!(
            "{}: there is no key to read it with",
            path.display()
        )));
    };
    let mut prev = GENESIS.to_string();
    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let record = read_next(&keys, &prev, line.as_bytes(), index + 1)
            .map_err(|problem| broken(path, &problem))?;
        prev = record.hash.clone();
        records.push(record);
    }
    old_chain(&records).map_err(|problem| broken(path, &problem))?;
    Ok(records)
}

/// For a converted log: the old records, rebuilt from their bodies, must
/// form the old chain and end at the hash the anchor follows.
fn old_chain(records: &[Record]) -> Result<(), String> {
    let Some(anchor) = records.first().filter(|first| first.op == UPGRADE_OP) else {
        return Ok(());
    };
    let mut prev = GENESIS.to_string();
    for record in records
        .iter()
        .skip(1)
        .take_while(|record| record.was.is_some())
    {
        let was = record.was.as_ref().expect("taken while some");
        let mut old = plain::Old::from(record);
        old.prev = was.prev.clone();
        if was.prev != prev || plain::hash_of(&old) != was.hash {
            return Err("the converted records do not form the old log's chain".into());
        }
        prev = was.hash.clone();
    }
    if anchor.prev != prev {
        return Err("the anchor does not follow the old log's last record".into());
    }
    Ok(())
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
        caller: caller.to_string(),
        agent,
        op: op.to_string(),
        args: Value::Null,
        decision: decision.to_string(),
        outcome: outcome.to_string(),
        gate: None,
        was: None,
        prev: String::new(),
        hash: String::new(),
    }
}

/// Converts the state folder's plaintext audit log (state format 1) into
/// an encrypted one, keeping every record and proof of the old chain: the
/// new log opens with an anchor that follows the old log's last hash, then
/// the old records, each with the hashes it had. The new log is written
/// beside the old one, checked (chain, every record opened, the old chain
/// rebuilt), and only then put in its place, so no plaintext copy is left.
/// A conversion cut short is picked up by the next one: the old log is
/// still there, or the new one already is.
///
/// Nothing else touches the log meanwhile. A running `latch serve`, of
/// this latch or an older one, would go on appending to the old log, so a
/// conversion is refused while one holds the folder's `serve.lock`, and
/// holds it itself so none starts. This latch's writers and other
/// conversions wait on the audit lock, which outlives the swap; an older
/// latch's local `rules` waits on the old log's own lock.
pub fn upgrade(dir: &Path) -> Result<(), Failure> {
    let path = dir.join("audit.ndjson");
    let temp = dir.join("audit.ndjson.upgrade");
    if !path.exists() {
        return remove_temp(&temp);
    }
    let _host = no_host_running(dir)?;
    let _lock = lock(dir)?;
    upgrade_locked(dir)
}

/// `upgrade`'s work, with the audit lock held and no host running.
fn upgrade_locked(dir: &Path) -> Result<(), Failure> {
    let path = dir.join("audit.ndjson");
    let temp = dir.join("audit.ndjson.upgrade");
    let mut file = match fs::OpenOptions::new().read(true).append(true).open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::NotFound => return remove_temp(&temp),
        Err(err) => return Err(io_failure(&path, err)),
    };
    // An older latch's writer locks the old log itself.
    file.lock().map_err(|err| io_failure(&path, err))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|err| io_failure(&path, err))?;
    if verify(&text).is_ok() {
        // Already converted (cut short after the swap), or empty.
        read_all(&path)?;
        return remove_temp(&temp);
    }
    write_converted(dir, &text, &temp)?;
    fs::rename(&temp, &path).map_err(|err| io_failure(&path, err))
}

/// Writes the converted log to `temp` and checks it as `read_all` does.
fn write_converted(dir: &Path, text: &str, temp: &Path) -> Result<(), Failure> {
    let path = dir.join("audit.ndjson");
    let (old, last) = plain::read(text).map_err(|problem| broken(&path, &problem))?;
    let keys = Keys::ready(dir)?;
    let mut anchor = record("latch", None, UPGRADE_OP, "allow", "ok");
    anchor.args = json!({"records": old.len(), "last_hash": last});
    let mut lines = vec![keys.seal(&anchor, &last, true)];
    for old in old {
        let converted = Record {
            was: Some(Was {
                prev: old.prev.clone(),
                hash: old.hash.clone(),
            }),
            ..old.into()
        };
        let prev = lines.last().expect("the anchor").hash.clone();
        lines.push(keys.seal(&converted, &prev, false));
    }
    let mut out = String::new();
    for line in &lines {
        out.push_str(&serde_json::to_string(line).expect("a line serializes"));
        out.push('\n');
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(temp)
        .map_err(|err| io_failure(temp, err))?;
    file.write_all(out.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|err| io_failure(temp, err))?;
    let records = read_all(temp)?;
    if records.len() != lines.len() {
        return Err(broken(temp, "the converted log lost records"));
    }
    Ok(())
}

/// The file every reader that writes the log, and every conversion, holds
/// locked: `audit.lock` beside it. Unlike the log, it is never replaced, so
/// a lock on it still means something across a conversion's swap.
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

/// Holds the folder's `serve.lock`, so no host runs while the log is
/// converted. A probe holds it, shared, for an instant
/// (`lifecycle::holds_lock`), so a busy lock is asked again for a second
/// before it means a host is running.
fn no_host_running(dir: &Path) -> Result<File, Failure> {
    let path = dir.join("serve.lock");
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|err| io_failure(&path, err))?;
    for _ in 0..20 {
        if file.try_lock().is_ok() {
            return Ok(file);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let message = format!(
        "{}: a latch serve is running on this folder, so its audit log can't be converted \
         now; it is, the first time latch runs after that host stops",
        dir.display()
    );
    Err(Failure::failed("host_running", message))
}

/// Whether the log at `path` holds any sealed line, which needs its key.
fn has_sealed_lines(path: &Path) -> Result<bool, Failure> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(io_failure(path, err)),
    };
    Ok(text
        .lines()
        .any(|line| serde_json::from_str::<Line>(line).is_ok()))
}

fn remove_temp(temp: &Path) -> Result<(), Failure> {
    match fs::remove_file(temp) {
        Err(err) if err.kind() != ErrorKind::NotFound => Err(io_failure(temp, err)),
        _ => Ok(()),
    }
}

/// The plaintext log of state format 1, read only to convert it.
mod plain {
    use serde::{Deserialize, Serialize};
    use serde_json::Value;
    use sha2::{Digest, Sha256};

    use super::{GENESIS, Record};
    use crate::state::to_hex;

    /// A format-1 record, in its fields' order on the line.
    #[derive(Serialize, Deserialize, Clone)]
    pub struct Old {
        pub time: String,
        pub caller: String,
        pub agent: Option<String>,
        pub op: String,
        pub args: Value,
        pub decision: String,
        pub outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub gate: Option<Value>,
        pub prev: String,
        pub hash: String,
    }

    impl From<&Record> for Old {
        fn from(record: &Record) -> Old {
            Old {
                time: record.time.clone(),
                caller: record.caller.clone(),
                agent: record.agent.clone(),
                op: record.op.clone(),
                args: record.args.clone(),
                decision: record.decision.clone(),
                outcome: record.outcome.clone(),
                gate: record.gate.clone(),
                prev: record.prev.clone(),
                hash: String::new(),
            }
        }
    }

    impl From<Old> for Record {
        fn from(old: Old) -> Record {
            Record {
                time: old.time,
                caller: old.caller,
                agent: old.agent,
                op: old.op,
                args: old.args,
                decision: old.decision,
                outcome: old.outcome,
                gate: old.gate,
                was: None,
                prev: String::new(),
                hash: String::new(),
            }
        }
    }

    /// The sha256 of `old` with its `hash` empty, as format 1 hashed it.
    pub fn hash_of(old: &Old) -> String {
        let mut blank = old.clone();
        blank.hash = String::new();
        let json = serde_json::to_string(&blank).expect("a record serializes");
        to_hex(&Sha256::digest(json.as_bytes()))
    }

    /// The records, their chain checked, and the last hash.
    pub fn read(text: &str) -> Result<(Vec<Old>, String), String> {
        let mut prev = GENESIS.to_string();
        let mut records = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let number = index + 1;
            let old: Old = serde_json::from_str(line)
                .map_err(|_| format!("line {number} is not an audit record"))?;
            if hash_of(&old) != old.hash {
                return Err(format!(
                    "line {number} was changed: its hash does not match"
                ));
            }
            if old.prev != prev {
                return Err(format!(
                    "chain broken at line {number}: it does not follow line {}",
                    number - 1
                ));
            }
            prev = old.hash.clone();
            records.push(old);
        }
        Ok((records, prev))
    }
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

fn damaged() -> Failure {
    failed("the audit keys are damaged".into())
}

fn failed(message: String) -> Failure {
    Failure::failed("audit_key", message)
}

fn broken(path: &Path, problem: &str) -> Failure {
    Failure::failed("audit_broken", format!("{}: {problem}", path.display()))
}

fn io_failure(path: &Path, err: std::io::Error) -> Failure {
    Failure::failed("audit_io", format!("{}: {err}", path.display()))
}

/// Test only: a format-1 log of `ops`, as a latch before encryption wrote
/// it. Returns its last hash.
#[cfg(test)]
pub(crate) fn write_plaintext_log(dir: &Path, ops: &[&str]) -> String {
    let mut prev = GENESIS.to_string();
    let mut text = String::new();
    for op in ops {
        let mut old = plain::Old::from(&{
            let mut entry = record("old-caller", None, "exec", "allow", "ok");
            entry.args =
                json!({"argv": ["curl", "-H", "Authorization: Bearer synthetic-credential-5d1e"]});
            entry
        });
        old.op = op.to_string();
        old.prev = prev.clone();
        old.hash = plain::hash_of(&old);
        prev = old.hash.clone();
        text.push_str(&serde_json::to_string(&old).unwrap());
        text.push('\n');
    }
    fs::write(dir.join("audit.ndjson"), &text).unwrap();
    prev
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
        let path = dir.path().join("audit.ndjson");
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
        let refused = read_all(&path).unwrap_err();
        assert!(
            refused
                .message
                .ends_with("line 2 does not open with its key"),
            "{}",
            refused.message
        );

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
    fn a_plaintext_log_becomes_a_sealed_one_anchored_to_its_last_hash() {
        let dir = test_dir();
        let last = write_plaintext_log(dir.path(), &["connect", "exec"]);
        upgrade(dir.path()).unwrap();

        let path = dir.path().join("audit.ndjson");
        let text = fs::read_to_string(&path).unwrap();
        for content in ["old-caller", "exec", SECRET] {
            assert!(
                !text.contains(content),
                "{content} is still on disk in the clear"
            );
        }
        let first: Line = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert!(first.anchor);
        assert_eq!(first.prev, last);
        assert!(!dir.path().join("audit.ndjson.upgrade").exists());

        let records = read_all(&path).unwrap();
        assert_eq!(records[0].op, UPGRADE_OP);
        assert_eq!(records[0].args, json!({"records": 2, "last_hash": last}));
        assert_eq!(records[2].was.as_ref().unwrap().hash, last);
        // New records chain on as usual.
        AuditLog::open(path.clone())
            .unwrap()
            .append(record("new-caller", None, "exec", "allow", "ok"))
            .unwrap();
        assert_eq!(read_all(&path).unwrap().len(), 4);
    }

    #[test]
    fn a_conversion_cut_short_is_picked_up_and_leaves_no_plaintext() {
        let dir = test_dir();
        let path = dir.path().join("audit.ndjson");
        let temp = dir.path().join("audit.ndjson.upgrade");
        let last = write_plaintext_log(dir.path(), &["connect", "exec"]);

        // Cut short with the sealed log written beside the old one.
        let text = fs::read_to_string(&path).unwrap();
        write_converted(dir.path(), &text, &temp).unwrap();
        assert!(temp.exists() && fs::read_to_string(&path).unwrap().contains("old-caller"));
        upgrade(dir.path()).unwrap();
        assert!(!temp.exists());
        assert!(!fs::read_to_string(&path).unwrap().contains("old-caller"));
        let first: Line =
            serde_json::from_str(fs::read_to_string(&path).unwrap().lines().next().unwrap())
                .unwrap();
        assert_eq!(first.prev, last);

        // Cut short after the swap (before the folder was marked): the next
        // start finds it done and changes nothing.
        let sealed = fs::read_to_string(&path).unwrap();
        upgrade(dir.path()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), sealed);
    }

    #[test]
    fn a_tampered_plaintext_log_is_not_converted() {
        let dir = test_dir();
        write_plaintext_log(dir.path(), &["connect", "exec"]);
        let path = dir.path().join("audit.ndjson");
        let text = fs::read_to_string(&path)
            .unwrap()
            .replacen("connect", "oops", 1);
        fs::write(&path, &text).unwrap();
        let refused = upgrade(dir.path()).unwrap_err();
        assert_eq!(refused.code, "audit_broken");
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }
}

#[cfg(test)]
mod race_tests {
    use super::*;

    fn test_dir() -> tempfile::TempDir {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        fs::create_dir_all(&parent).unwrap();
        tempfile::tempdir_in(parent).unwrap()
    }

    #[test]
    fn a_second_conversion_waits_and_keeps_what_was_appended_after_the_first() {
        let dir = test_dir();
        write_plaintext_log(dir.path(), &["connect", "exec"]);
        // A first conversion is under way: it holds the audit lock.
        let held = lock(dir.path()).unwrap();
        let second = std::thread::spawn({
            let dir = dir.path().to_path_buf();
            move || upgrade(&dir)
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !second.is_finished(),
            "a conversion waits for the audit lock"
        );

        // The first finishes, and a writer appends to the sealed log.
        upgrade_locked(dir.path()).unwrap();
        let path = dir.path().join("audit.ndjson");
        let keys = Keys::ready(dir.path()).unwrap();
        let prev = verify(&fs::read_to_string(&path).unwrap()).unwrap().1;
        let line = keys.seal(&record("after", None, "exec", "allow", "ok"), &prev, false);
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{}", serde_json::to_string(&line).unwrap()).unwrap();
        drop(held);

        second.join().unwrap().unwrap();
        let callers: Vec<String> = read_all(&path)
            .unwrap()
            .into_iter()
            .map(|record| record.caller)
            .collect();
        assert_eq!(callers, ["latch", "old-caller", "old-caller", "after"]);
    }

    #[test]
    fn a_conversion_is_refused_while_a_host_runs() {
        let dir = test_dir();
        write_plaintext_log(dir.path(), &["connect"]);
        let before = fs::read_to_string(dir.path().join("audit.ndjson")).unwrap();
        // A running `latch serve`, of any version, holds `serve.lock`.
        let host = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.path().join("serve.lock"))
            .unwrap();
        host.lock().unwrap();
        let refused = upgrade(dir.path()).unwrap_err();
        assert_eq!(refused.code, "host_running");
        assert_eq!(
            fs::read_to_string(dir.path().join("audit.ndjson")).unwrap(),
            before
        );
        drop(host);
        upgrade(dir.path()).unwrap();
        assert_eq!(read_all(&dir.path().join("audit.ndjson")).unwrap().len(), 2);
    }

    #[test]
    fn keys_cut_short_before_anything_was_sealed_are_made_afresh() {
        let dir = test_dir();
        write_plaintext_log(dir.path(), &["connect", "exec"]);
        // A first save of the keys cut short, by a latch that wrote them in
        // place: a part of a key ring, and nothing sealed with it.
        fs::write(dir.path().join("audit.keys"), r#"{"current":"k1","ke"#).unwrap();
        upgrade(dir.path()).unwrap();
        let path = dir.path().join("audit.ndjson");
        assert_eq!(read_all(&path).unwrap().len(), 3);

        // Once a line is sealed, keys that don't read are an error, never
        // replaced: that would lose the records.
        fs::write(dir.path().join("audit.keys"), r#"{"current":"k1","ke"#).unwrap();
        let refused = AuditLog::open(path).err().unwrap();
        assert_eq!(refused.code, "audit_key");
    }
}
