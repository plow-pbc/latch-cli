//! The host's audit log: one JSON line per record (NDJSON), append-only, in
//! the host's state directory. The log stays on the host; the owner's own
//! devices read its records with `latch audit --host` (`serve/reads.rs`).
//!
//! Each line holds one record sealed with AES-256-GCM under an audit key
//! (`Keys`): its `body` is the whole record (when, who called, what they
//! asked, the decision, the outcome, the gatekeeper's judgment), unreadable
//! without the key. In the clear are only what checks the chain: the key's
//! id (`kid`), the `nonce`, `prev` and `hash`. `hash` is the sha256 of the
//! line written with `hash` empty, and `prev` is the line before's `hash`
//! (64 zeros for the first). So the chain verifies without the key: a
//! changed, removed or reordered line is caught. Lines cut off the end are
//! not, since nothing keeps the newest `hash` apart from the log. The
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
    /// How long parts of it took, for a remote caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timings: Option<Timings>,
    /// The line's `prev`.
    #[serde(skip)]
    pub prev: String,
    /// The line's own sha256; also the record's `audit_id` in the host's answer.
    #[serde(skip)]
    pub hash: String,
}

/// How long parts of an operation took, in milliseconds.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct Timings {
    /// Plow saying who the caller is: on connect, and again before each
    /// operation.
    pub caller_check_ms: u64,
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

/// `latch audit verify` and `latch audit rotate-key`, on this machine's
/// host state, and `latch audit judge` and `latch audit shot`, on this
/// machine's host or another.
pub async fn run(state: &StateDir, action: AuditAction) -> Result<Value, Failure> {
    let path = state.path.join(FILE);
    match action {
        AuditAction::Judge { request, host } => match crate::remote::other_host(state, host).await?
        {
            Some(host) => {
                let request = crate::wire::Request {
                    op: "audit_judge".into(),
                    args: json!({"request": request}),
                    chat: None,
                };
                let mut kept =
                    crate::dial::call(state, &host, &request, crate::dial::ANSWER_WITHIN).await?;
                // Printed as the host prints it, without its record of this read.
                if let Some(kept) = kept.as_object_mut() {
                    kept.remove("audit_id");
                }
                Ok(kept)
            }
            None => crate::gate::requests::read(&state.path, &request),
        },
        AuditAction::Shot {
            record,
            shot,
            output,
            host,
        } => {
            let (size, sha256) = match crate::remote::other_host(state, host).await? {
                Some(host) => {
                    let request = crate::wire::Request {
                        op: "audit_shot".into(),
                        args: json!({"record": record, "shot": shot}),
                        chat: None,
                    };
                    let link = crate::dial::connect(state, &host).await?;
                    let got = crate::transfer::download(&link.connection, &request, &output).await;
                    link.close();
                    let (start, _, sha256) = got?;
                    (start["size"].clone(), sha256)
                }
                None => {
                    let png = crate::browser::journal::audited(&state.path, &record, &shot)?;
                    let staged = crate::transfer::Staged::beside(&output)?;
                    let mut file = staged.file()?;
                    let written = async {
                        use tokio::io::AsyncWriteExt;
                        file.write_all(&png).await?;
                        file.flush().await
                    };
                    written.await.map_err(|err| {
                        Failure::new(code::local_file, format!("{}: {err}", output.display()))
                    })?;
                    staged.place(&output)?;
                    (json!(png.len()), to_hex(&Sha256::digest(&png)))
                }
            };
            Ok(
                json!({"record": record, "shot": shot, "path": output, "size": size, "sha256": sha256}),
            )
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

/// Which records `latch audit` reads, as its flags say: the newest
/// `last` (50 without it), or `all`; those before the record `before`; or
/// with `follow`, every one after the record `after`. A record is named by
/// its id; `GENESIS` stands for "before the first".
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Range {
    pub last: Option<usize>,
    pub all: bool,
    pub before: Option<String>,
    pub after: Option<String>,
    pub follow: bool,
}

/// How many records a read without `--last` or `--all` shows.
const PAGE: usize = 50;

impl Range {
    /// The same rules as the command's flags, also for a range that came
    /// over the wire, and that each id is one.
    pub fn check(&self) -> Result<(), Failure> {
        let usage = |message: &str| Err(Failure::new(code::usage, message.to_string()));
        for (flag, id) in [("--before", &self.before), ("--after", &self.after)] {
            let is_id = |id: &String| {
                id.len() == 64
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            };
            if let Some(id) = id
                && !is_id(id)
            {
                return usage(&format!(
                    "{flag} takes a record's id: 64 hex characters, a record line's `cursor`"
                ));
            }
        }
        if self.last == Some(0) {
            return usage("--last takes a positive number");
        }
        if self.all && (self.last.is_some() || self.before.is_some()) {
            return usage("--all goes with neither --last nor --before");
        }
        if self.before.is_some() && (self.follow || self.after.is_some()) {
            return usage("--before goes with neither --follow nor --after");
        }
        if self.after.is_some() && (!self.follow || self.last.is_some() || self.all) {
            return usage("--after goes with --follow only");
        }
        Ok(())
    }
}

/// `latch audit`: the records `range` picks from the log in `dir`, as JSON
/// lines given to `send` until it returns false (or, following, until
/// `hangup` closes): `hello`, a `snapshot` with the id before the first one
/// and whether older ones exist, a `record` line each, then `caught_up`;
/// with `follow`, then each new record as it is written. `whose` names the
/// log in an error (`studio's`).
///
/// A record names an anchor the log does not hold: `cursor_expired`. A log
/// that fails its checks, or no longer begins with what was sent, stops it
/// with `audit_broken`.
pub fn read(
    dir: &Path,
    whose: &str,
    range: &Range,
    hangup: Option<std::os::fd::RawFd>,
    mut send: impl FnMut(&Value) -> bool,
) -> Result<(), Failure> {
    range.check()?;
    let path = dir.join(FILE);
    let watching =
        |err: std::io::Error| Failure::new(code::watch_failed, format!("{}: {err}", dir.display()));
    // Armed before the first look, so nothing written after it is missed.
    let mut watch = match range.follow {
        true => Some(
            crate::watch::Watch::new(dir, std::slice::from_ref(&path), hangup).map_err(watching)?,
        ),
        false => None,
    };
    if let Some(watch) = &mut watch {
        watch.arm().map_err(watching)?;
    }
    let text = read_locked(&path)?;
    let records = open_text(&text, &path)?;
    let index = |id: &str| -> Result<usize, Failure> {
        if id == GENESIS {
            return Ok(0);
        }
        match records.iter().position(|record| record.hash == id) {
            Some(at) => Ok(at + 1),
            None => {
                let message = format!("that record isn't in {whose} audit");
                let mut failure = Failure::new(code::cursor_expired, message);
                failure.next = Some("read it again without --after or --before".into());
                Err(failure)
            }
        }
    };
    let (start, end) = match (&range.after, &range.before) {
        (Some(after), _) => (index(after)?, records.len()),
        (None, before) => {
            let end = match before {
                // Not the anchor itself: the records before it.
                Some(before) => index(before)?.saturating_sub(1),
                None => records.len(),
            };
            let start = match range.all {
                true => 0,
                false => end.saturating_sub(range.last.unwrap_or(PAGE)),
            };
            (start, end)
        }
    };
    let id_before = |at: usize| match at {
        0 => GENESIS.to_string(),
        at => records[at - 1].hash.clone(),
    };
    let has_older = start > 0;
    let mut lines = vec![
        crate::stream::hello(),
        json!({"type": "snapshot", "cursor": id_before(start), "has_older": has_older}),
    ];
    lines.extend(records[start..end].iter().map(record_line));
    lines.push(json!({"type": "caught_up", "cursor": id_before(end), "has_older": has_older}));
    for line in &lines {
        if !send(line) {
            return Ok(());
        }
    }
    let Some(mut watch) = watch else {
        return Ok(());
    };
    // What was sent is `shown`, the log's text so far. Each later look must
    // still begin with exactly that text, compared byte for byte rather
    // than opened again; only the lines after it are opened.
    let mut count = records.len();
    let mut prev = records
        .last()
        .map_or(GENESIS.to_string(), |record| record.hash.clone());
    let mut shown = text;
    loop {
        match watch.wait().map_err(watching)? {
            crate::watch::Woke::HungUp => return Ok(()),
            crate::watch::Woke::Changed => {}
        }
        watch.arm().map_err(watching)?;
        let text = read_locked(&path)?;
        let Some(new) = text.strip_prefix(shown.as_str()) else {
            return Err(broken(&path, "it no longer begins with what was shown"));
        };
        if new.is_empty() {
            continue;
        }
        let keys = Keys::existing(dir)?.ok_or_else(|| no_key(&path))?;
        let records = open_after(&keys, &prev, count, new, &path)?;
        for record in &records {
            if !send(&record_line(record)) {
                return Ok(());
            }
        }
        count += records.len();
        if let Some(record) = records.last() {
            prev = record.hash.clone();
        }
        shown.push_str(new);
    }
}

/// One record as `latch audit` shows it, its arguments with
/// credential-looking values hidden (`redact.rs`), best effort, and its
/// `command` as the board shows it.
fn record_line(record: &Record) -> Value {
    let shown = json!({
        "id": record.hash, "time": record.time, "caller": record.caller, "caller_name": record.caller_name,
        "agent": record.agent, "op": record.op, "command": crate::dashboard::recorded_command(record),
        "decision": record.decision, "outcome": record.outcome,
        "details": {"started": record.started, "args": crate::redact::shown(&record.op, &record.args),
                    "gate": record.gate, "timings": record.timings},
    });
    json!({"type": "record", "cursor": record.hash, "record": shown})
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

/// Record `id` of the log in `dir`, if the log holds it: the lines up to
/// it checked against the chain without the key, and only it opened.
pub fn find(dir: &Path, id: &str) -> Result<Option<Record>, Failure> {
    let path = dir.join(FILE);
    let text = read_locked(&path)?;
    let mut prev = GENESIS.to_string();
    for (index, text) in text.lines().enumerate() {
        let line = verify_line(&prev, text.as_bytes(), index + 1)
            .map_err(|problem| broken(&path, &problem))?;
        if line.hash == id {
            let keys = Keys::existing(dir)?.ok_or_else(|| no_key(&path))?;
            let record = keys.open(&line, index + 1);
            return record.map(Some).map_err(|problem| broken(&path, &problem));
        }
        prev = line.hash;
    }
    Ok(None)
}

/// Every record of the log at `path`: the chain checked, each record opened.
#[cfg(test)]
pub fn read_all(path: &Path) -> Result<Vec<Record>, Failure> {
    open_text(&read_locked(path)?, path)
}

/// Every record of the log's whole `text`, read from `path`, opened with
/// the keys beside it.
fn open_text(text: &str, path: &Path) -> Result<Vec<Record>, Failure> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let keys = Keys::existing(dir)?.ok_or_else(|| no_key(path))?;
    open_all(&keys, text, path)
}

/// Every record of the log `text` (at `path`, for messages), each line's
/// chain checked and its record opened with `keys`.
fn open_all(keys: &Keys, text: &str, path: &Path) -> Result<Vec<Record>, Failure> {
    open_after(keys, GENESIS, 0, text, path)
}

/// The records of `text`, the lines that follow line `number` (whose hash
/// is `prev`) of the log at `path`: each line's chain checked and its
/// record opened with `keys`.
fn open_after(
    keys: &Keys,
    prev: &str,
    number: usize,
    text: &str,
    path: &Path,
) -> Result<Vec<Record>, Failure> {
    let mut prev = prev.to_string();
    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let record = read_next(keys, &prev, line.as_bytes(), number + index + 1)
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
        timings: None,
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

    /// `latch audit shot` on this machine's host state: the PNG a plan's
    /// record names, saved where asked, with no host running.
    #[tokio::test]
    async fn a_shot_is_read_here_without_a_host() {
        let dir = test_dir();
        let state = StateDir {
            path: dir.path().to_path_buf(),
        };
        let (session, png) = (
            "lkjhgfdsapoiuytrewqmnbvcxzlkjhgf",
            b"\x89PNG a synthetic page",
        );
        crate::browser::journal::save_shot(dir.path(), session, "r1", "final", png).unwrap();
        let sha256 = to_hex(&Sha256::digest(png));
        let mut plan = record(
            "agent-key",
            Some("agent-1".into()),
            "browser",
            "allow",
            "ok",
        );
        plan.args = json!({"command": "do", "session": crate::browser::host::hash_of(session),
                           "request": "r1", "screenshot": {"shot": "final", "size": png.len(), "sha256": sha256}});
        let id = AuditLog::open(dir.path().join(FILE))
            .unwrap()
            .append(plan)
            .unwrap();
        let output = dir.path().join("stop.png");
        let shot = |shot: &str| AuditAction::Shot {
            record: id.clone(),
            shot: shot.into(),
            output: output.clone(),
            host: None,
        };
        let got = run(&state, shot("final")).await.unwrap();
        assert_eq!(fs::read(&output).unwrap(), png);
        assert_eq!(
            got,
            json!({"record": id, "shot": "final", "path": output, "size": png.len(), "sha256": sha256})
        );
        assert_eq!(
            run(&state, shot("1")).await.unwrap_err().code,
            "no_such_shot"
        );
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

    /// `latch audit` gives each record the command the board shows for it,
    /// from the same function, its credentials hidden as there.
    #[test]
    fn a_record_shows_the_command_the_board_shows_its_secrets_hidden() {
        let mut exec = exec_record("caller-one");
        exec.args = json!({"argv": ["curl", "-H", format!("Authorization: Bearer {SECRET}")],
            "writes": ["/work/site"], "workdir": "/work/site", "network": true,
            "secret_env": [{"name": "TOKEN", "length": 26}]});
        let mut browser = record("caller-one", None, "browser", "allow", "ok");
        browser.args =
            json!({"command": "open", "origin": "https://shop.example", "path": "/cart"});
        for (record, expected) in [
            (
                exec,
                "latch exec --write /work/site --network --secret-env TOKEN=••• curl -H",
            ),
            (browser, "latch browser open https://shop.example/cart"),
        ] {
            let shown = &record_line(&record)["record"]["command"];
            let board = crate::dashboard::recorded(&record, "host-key").unwrap().2;
            assert_eq!(
                shown,
                &json!(crate::dashboard::recorded_command(&record)),
                "one function"
            );
            assert!(shown.as_str().unwrap().starts_with(expected), "{shown}");
            assert!(
                shown.as_str().unwrap().ends_with(&board.command),
                "{shown} vs {}",
                board.command
            );
            assert!(!shown.to_string().contains(SECRET), "{shown}");
        }
        // A connection is no command; any other call names its kind.
        let connect = record("caller-one", None, "connect", "allow", "connected");
        assert_eq!(record_line(&connect)["record"]["command"], Value::Null);
        let audit = record("caller-one", None, "audit", "allow", "ok");
        assert_eq!(record_line(&audit)["record"]["command"], "latch audit");
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
    fn find_opens_only_the_record_it_finds() {
        let (dir, path) = log_with_three_records();
        let dir = dir.path();
        let records = read_all(&path).unwrap();
        let found = find(dir, &records[1].hash).unwrap().unwrap();
        assert_eq!(found.hash, records[1].hash);
        assert_eq!(found.op, records[1].op);
        assert!(find(dir, &"0".repeat(64)).unwrap().is_none());

        // A later line that would not open is never opened; an earlier
        // line changed still breaks the chain to it.
        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let mut last: Line = serde_json::from_str(lines[2]).unwrap();
        last.kid = "gone".into();
        last.hash = hash_of(&last);
        let tail = serde_json::to_string(&last).unwrap();
        fs::write(&path, [lines[0], lines[1], &tail].join("\n") + "\n").unwrap();
        assert_eq!(
            find(dir, &records[1].hash).unwrap().unwrap().hash,
            records[1].hash
        );
        fs::write(&path, [lines[1], lines[2]].join("\n") + "\n").unwrap();
        assert_eq!(
            find(dir, &records[2].hash).unwrap_err().code,
            "audit_broken"
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

    /// The lines `latch audit` gives for `range`, or its failure.
    fn lines(dir: &Path, range: &Range) -> Result<Vec<Value>, Failure> {
        let mut lines = Vec::new();
        read(dir, "studio's", range, None, |line| {
            lines.push(line.clone());
            true
        })?;
        Ok(lines)
    }

    fn ids(lines: &[Value]) -> Vec<String> {
        lines
            .iter()
            .filter(|line| line["type"] == "record")
            .map(|line| line["cursor"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn a_read_pages_back_from_the_newest_and_masks_the_arguments() {
        let (dir, path) = log_with_three_records();
        let all: Vec<String> = read_all(&path)
            .unwrap()
            .into_iter()
            .map(|r| r.hash)
            .collect();

        let newest = lines(
            dir.path(),
            &Range {
                last: Some(2),
                ..Range::default()
            },
        )
        .unwrap();
        assert_eq!(newest[0], json!({"type": "hello", "schema": 1}));
        assert_eq!(
            newest[1],
            json!({"type": "snapshot", "cursor": all[0], "has_older": true})
        );
        assert_eq!(ids(&newest), all[1..]);
        assert_eq!(
            newest.last().unwrap(),
            &json!({"type": "caught_up", "cursor": all[2], "has_older": true})
        );
        let exec = &newest[2]["record"];
        assert_eq!(exec["op"], "exec");
        assert_eq!(exec["id"], all[1]);
        assert!(exec["details"]["args"]["argv"].is_array(), "{exec}");
        assert!(!newest.iter().any(|line| line.to_string().contains(SECRET)));

        // The page before the first row: the one oldest record, and no more.
        let before = Range {
            before: Some(all[1].clone()),
            ..Range::default()
        };
        let earlier = lines(dir.path(), &before).unwrap();
        assert_eq!(ids(&earlier), all[..1]);
        assert_eq!(
            earlier[1],
            json!({"type": "snapshot", "cursor": GENESIS, "has_older": false})
        );
        assert_eq!(earlier.last().unwrap()["cursor"], all[0]);

        let every = lines(
            dir.path(),
            &Range {
                all: true,
                ..Range::default()
            },
        )
        .unwrap();
        assert_eq!(ids(&every), all);
    }

    #[test]
    fn a_follower_resumes_after_its_last_id_and_an_unknown_one_has_expired() {
        let (dir, path) = log_with_three_records();
        let all: Vec<String> = read_all(&path)
            .unwrap()
            .into_iter()
            .map(|r| r.hash)
            .collect();
        let after = |id: &str| Range {
            after: Some(id.into()),
            follow: true,
            ..Range::default()
        };
        // Ends at once: the watch's hang-up is already closed.
        let (hangup, closed) = std::io::pipe().unwrap();
        drop(closed);
        let mut resumed = Vec::new();
        let fd = std::os::fd::AsRawFd::as_raw_fd(&hangup);
        read(dir.path(), "studio's", &after(&all[0]), Some(fd), |line| {
            resumed.push(line.clone());
            true
        })
        .unwrap();
        assert_eq!(ids(&resumed), all[1..]);
        assert_eq!(resumed[1]["cursor"], all[0]);

        // Nothing after the newest: caught up at the anchor itself.
        let (hangup, closed) = std::io::pipe().unwrap();
        drop(closed);
        let mut none = Vec::new();
        let fd = std::os::fd::AsRawFd::as_raw_fd(&hangup);
        read(dir.path(), "studio's", &after(&all[2]), Some(fd), |line| {
            none.push(line.clone());
            true
        })
        .unwrap();
        assert_eq!(none.last().unwrap()["cursor"], all[2]);
        assert_eq!(ids(&none), Vec::<String>::new());

        let expired = lines(dir.path(), &after(&"f".repeat(64))).unwrap_err();
        assert_eq!(
            (expired.exit, expired.code.as_str()),
            (crate::failure::Exit::Usage, "cursor_expired")
        );
        assert!(
            expired.message.contains("studio's audit"),
            "{}",
            expired.message
        );
        let bad = Range {
            before: Some("f".repeat(64)),
            ..Range::default()
        };
        assert_eq!(lines(dir.path(), &bad).unwrap_err().code, "cursor_expired");
    }

    /// A follower of a long log reads only what is new on each wake, so a
    /// new record reaches it far sooner than the whole log takes to open.
    #[test]
    fn a_follower_of_a_long_log_reads_only_the_new_records() {
        let dir = test_dir();
        let path = dir.path().join(FILE);
        let mut log = AuditLog::open(path.clone()).unwrap();
        let keys = Keys::existing(dir.path()).unwrap().unwrap();
        let mut prev = GENESIS.to_string();
        let mut text = String::new();
        for _ in 0..10_000 {
            let line = keys.seal(&exec_record("caller-one"), &prev);
            prev = line.hash.clone();
            text += &format!("{}\n", serde_json::to_string(&line).unwrap());
        }
        fs::write(&path, &text).unwrap();

        let (hangup, closed) = std::io::pipe().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let started = std::time::Instant::now();
        let follower = {
            let dir = dir.path().to_path_buf();
            std::thread::spawn(move || {
                let fd = std::os::fd::AsRawFd::as_raw_fd(&hangup);
                let range = Range {
                    follow: true,
                    ..Range::default()
                };
                read(&dir, "studio's", &range, Some(fd), |line| {
                    tx.send(line.clone()).is_ok()
                })
            })
        };
        let next = |kind: &str| loop {
            let line = rx.recv_timeout(std::time::Duration::from_secs(60)).unwrap();
            if line["type"] == kind {
                return line;
            }
        };
        next("caught_up");
        let opening = started.elapsed();

        for caller in ["caller-two", "caller-three"] {
            let appended = std::time::Instant::now();
            let id = log.append(exec_record(caller)).unwrap();
            let line = next("record");
            let took = appended.elapsed();
            assert_eq!(line["record"]["id"], id);
            assert!(
                took < opening / 10,
                "a new record took {took:?}; the whole log opened in {opening:?}"
            );
        }

        // A record already shown, changed in place to the same length with
        // every later line kept, stops the follower: the log no longer
        // begins with what was shown.
        let log = fs::read_to_string(&path).unwrap();
        let at = log.find("\"nonce\":\"").unwrap() + "\"nonce\":\"".len();
        let flipped = if &log[at..at + 1] == "0" { "1" } else { "0" };
        fs::write(&path, format!("{}{flipped}{}", &log[..at], &log[at + 1..])).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !follower.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        drop(closed);
        let stopped = follower
            .join()
            .unwrap()
            .expect_err("it followed a changed log");
        assert_eq!(stopped.code, "audit_broken", "{}", stopped.message);
    }

    #[test]
    fn an_empty_log_reads_as_caught_up_at_the_genesis() {
        let dir = test_dir();
        let empty = lines(dir.path(), &Range::default()).unwrap();
        assert_eq!(
            empty,
            [
                json!({"type": "hello", "schema": 1}),
                json!({"type": "snapshot", "cursor": GENESIS, "has_older": false}),
                json!({"type": "caught_up", "cursor": GENESIS, "has_older": false}),
            ]
        );
    }
}
