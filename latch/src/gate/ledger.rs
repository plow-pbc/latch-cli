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

/// One allowed action.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Done {
    pub at: jiff::Timestamp,
    pub action: String,
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
        text.lines()
            .map(|line| serde_json::from_str(line).map_err(|err| format!("ledger: {err}")))
            .collect()
    }

    /// Records `action`, allowed `at`.
    pub fn record(&self, at: jiff::Timestamp, action: &str) -> Result<(), String> {
        let failed = |err: std::io::Error| format!("{}: {err}", self.path.display());
        let dir = self.path.parent().expect("a ledger file has a folder");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(failed)?;
        let mut cut = action.len().min(KEPT_ACTION);
        while !action.is_char_boundary(cut) {
            cut -= 1;
        }
        let done = Done {
            at,
            action: action[..cut].to_string(),
        };
        let mut line = serde_json::to_string(&done).expect("a ledger entry serializes");
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
