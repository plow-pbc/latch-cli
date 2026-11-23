//! Every action the gatekeeper allowed on one cited intent, so the judge
//! sees what was already done for it: a one-time "buy this" is spent once.
//!
//! A host judges one intent's actions one at a time: [`Ledger::hold`]
//! takes the intent's lock, and the judgment reads and records under it,
//! so two concurrent calls on a one-time intent can't both be allowed.
//! Only one host runs on a state folder, so an in-process lock is the
//! host's lock. Another host gating the same agent keeps its own ledger.
//!
//! One file per intent, `gatekeeper/ledger/<sha256 of its key>.jsonl`,
//! readable by the host's user only.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as Lock, OwnedMutexGuard};

use crate::state::to_hex;

/// The most of one action the ledger keeps, for the judge to read back.
const KEPT_ACTION: usize = 2 * 1024;

/// One allowed action, by its id (none on an entry from before ids), and
/// whether a confirmation has carried it on, which only one may.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Done {
    pub at: jiff::Timestamp,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip)]
    pub continued: bool,
}

/// A line of the ledger: an allowed action, or the mark that the one
/// `continued` names was carried on.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Line {
    Done(Done),
    Continued {
        at: jiff::Timestamp,
        continued: String,
    },
}

pub struct Ledger {
    dir: PathBuf,
    locks: Mutex<HashMap<String, Arc<Lock<()>>>>,
}

/// One intent's lock, held across its judgment and its record.
pub struct Held {
    path: PathBuf,
    _guard: OwnedMutexGuard<()>,
}

impl Ledger {
    pub fn new(state_dir: &std::path::Path) -> Ledger {
        Ledger {
            dir: state_dir.join("gatekeeper").join("ledger"),
            locks: Mutex::new(HashMap::new()),
        }
    }

    /// Waits for, then holds, the lock of the intent `key`.
    pub async fn hold(&self, key: &str) -> Held {
        let lock = self
            .locks
            .lock()
            .expect("the ledger's locks")
            .entry(key.to_string())
            .or_default()
            .clone();
        Held {
            path: self
                .dir
                .join(format!("{}.jsonl", to_hex(&Sha256::digest(key.as_bytes())))),
            _guard: lock.lock_owned().await,
        }
    }
}

impl Held {
    /// The actions already allowed on this intent, oldest first.
    pub fn done(&self) -> Result<Vec<Done>, String> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(format!("{}: {err}", self.path.display())),
        };
        let lines = text
            .lines()
            .map(|line| serde_json::from_str(line).map_err(|err| format!("ledger: {err}")))
            .collect::<Result<Vec<Line>, String>>()?;
        let mut done = Vec::new();
        for line in lines {
            match line {
                Line::Done(entry) => done.push(entry),
                Line::Continued { continued, .. } => {
                    for entry in &mut done {
                        entry.continued |= entry.id.as_deref() == Some(&continued);
                    }
                }
            }
        }
        Ok(done)
    }

    /// Records `action`, allowed `at`; its new id comes back.
    pub fn record(&self, at: jiff::Timestamp, action: &str) -> Result<String, String> {
        let mut cut = action.len().min(KEPT_ACTION);
        while !action.is_char_boundary(cut) {
            cut -= 1;
        }
        let id = to_hex(&rand::random::<[u8; 8]>());
        self.append(&Line::Done(Done {
            at,
            action: action[..cut].to_string(),
            id: Some(id.clone()),
            continued: false,
        }))?;
        Ok(id)
    }

    /// Marks the action `id` carried on, `at`.
    pub fn continued(&self, at: jiff::Timestamp, id: &str) -> Result<(), String> {
        self.append(&Line::Continued {
            at,
            continued: id.to_string(),
        })
    }

    fn append(&self, line: &Line) -> Result<(), String> {
        let failed = |err: std::io::Error| format!("{}: {err}", self.path.display());
        let dir = self.path.parent().expect("a ledger file has a folder");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(failed)?;
        let mut line = serde_json::to_string(line).expect("a ledger line serializes");
        line.push('\n');
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
            .and_then(|mut file| file.write_all(line.as_bytes()))
            .map_err(failed)
    }
}
