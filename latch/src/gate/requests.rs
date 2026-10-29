//! What the gatekeeper sent Plow's decision route and what it answered,
//! kept on the host for its owner to read back (`latch audit judge`): the
//! audit keeps only ids and fingerprints of what was judged.
//!
//! One file per call, `gatekeeper/requests/<hash of its id>.json`,
//! readable by the host's user only. Its id is the host's own, and the
//! audit's record of the call names it (`kept_request`), whether or not
//! Plow answered. It holds when, which layer asked, and the request body
//! and reply exactly as they went: chat and rules text, never a vault or
//! secret value, as the judge is never shown one. Each is kept 7 days.
//! Keeping it is best effort: a call that can't be kept is still judged,
//! and the host's log says why.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;
use serde_json::Value;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

use crate::browser::journal::KEEP;
use crate::failure::{Failure, code};
use crate::model::Exchange;
use crate::state::to_hex;

/// One call, as kept.
#[derive(Serialize)]
pub struct Kept<'a> {
    pub id: &'a str,
    pub time: String,
    /// `owner`, or an agent's `rules` or `chat` layer.
    pub layer: &'static str,
    pub request: &'a RawValue,
    /// Plow's reply, if one came.
    pub answer: Option<&'a RawValue>,
}

fn folder(state_dir: &Path) -> PathBuf {
    state_dir.join("gatekeeper/requests")
}

/// An id's file: named by a hash, so any id names a plain file.
fn file(state_dir: &Path, id: &str) -> PathBuf {
    let hash = to_hex(&Sha256::digest(id.as_bytes()));
    folder(state_dir).join(format!("{}.json", &hash[..32]))
}

/// A new call's id.
pub fn new_id() -> String {
    format!("g{}", to_hex(&rand::random::<[u8; 12]>()))
}

/// Keeps `exchange`, as `layer` asked it, under `id`.
pub fn keep(
    state_dir: &Path,
    id: &str,
    layer: &'static str,
    exchange: &Exchange,
) -> Result<(), String> {
    prune(state_dir);
    let kept = Kept {
        id,
        time: jiff::Timestamp::now().to_string(),
        layer,
        request: &exchange.request,
        answer: exchange.answer.as_deref(),
    };
    let text = serde_json::to_vec(&kept).expect("a kept request serializes");
    let written = || -> std::io::Result<()> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(folder(state_dir))?;
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(file(state_dir, id))?
            .write_all(&text)
    };
    written().map_err(|err| format!("gatekeeper request {id} could not be kept: {err}"))
}

/// The call `id` names, as kept.
pub fn read(state_dir: &Path, id: &str) -> Result<Value, Failure> {
    prune(state_dir);
    let text = fs::read(file(state_dir, id)).map_err(|_| {
        let mut failure = Failure::new(
            code::no_such_request,
            format!("this host keeps no gatekeeper request {id}"),
        );
        failure.next = Some("a host keeps what its gatekeeper sent for 7 days".into());
        failure
    })?;
    serde_json::from_slice(&text)
        .map_err(|err| Failure::new(code::read_failed, format!("gatekeeper request {id}: {err}")))
}

/// Removes every kept call older than `KEEP`. What can't be removed stays
/// for the next try.
fn prune(state_dir: &Path) {
    let Some(before) = SystemTime::now().checked_sub(KEEP) else {
        return;
    };
    let Ok(entries) = fs::read_dir(folder(state_dir)) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|at| at < before);
        if old {
            let _ = fs::remove_file(entry.path());
        }
    }
}
