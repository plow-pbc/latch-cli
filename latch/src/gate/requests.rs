//! What the gatekeeper sent Plow's decision route and what it answered,
//! kept on the host for its owner to read back (`latch audit judge`): the
//! audit keeps only ids and fingerprints of what was judged.
//!
//! One file per call, `gatekeeper/requests/<hash of its id>.json`,
//! readable by the host's user only and sealed with the host's audit key
//! (`audit.rs`), bound to its id. Its id is the host's own, and the
//! audit's record of the call names it (`kept_request`), whether or not
//! Plow answered. It holds when, which layer asked, and the request body
//! and reply exactly as they went: chat and rules text, never a vault or
//! secret value, as the judge is never shown one, though a credential
//! typed into a command or plan is in them, as in the audit. Reading one
//! back shows it whole: exactly what the judge saw. Each is kept 7 days.
//! Keeping it is best effort: a call that can't be kept is still judged,
//! and the host's log says why.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

use crate::audit::Keys;
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

/// What a kept call is sealed bound to.
fn aad(id: &str) -> String {
    format!("latch gatekeeper request v1|{id}")
}

/// A kept call on disk: its key's id and the sealed call.
#[derive(Serialize, Deserialize)]
struct Sealed {
    kid: String,
    #[serde(flatten)]
    sealed: crate::seal::Sealed,
}

/// A new call's id.
pub fn new_id() -> String {
    format!("g{}", to_hex(&rand::random::<[u8; 12]>()))
}

/// Keeps `exchange`, as `layer` asked it, under `id`: `false` on a
/// machine that has never served, which keeps no audit and so no key.
pub fn keep(
    state_dir: &Path,
    id: &str,
    layer: &'static str,
    exchange: &Exchange,
) -> Result<bool, String> {
    prune(state_dir);
    let kept = Kept {
        id,
        time: jiff::Timestamp::now().to_string(),
        layer,
        request: &exchange.request,
        answer: exchange.answer.as_deref(),
    };
    let not_kept = |why: String| format!("gatekeeper request {id} could not be kept: {why}");
    let Some(keys) = Keys::existing(state_dir).map_err(|failure| not_kept(failure.message))? else {
        return Ok(false);
    };
    let (kid, sealed) = keys.seal_bytes(
        &serde_json::to_vec(&kept).expect("a kept request serializes"),
        &aad(id),
    );
    let text = serde_json::to_vec(&Sealed { kid, sealed }).expect("a sealed request serializes");
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
    written().map_err(|err| not_kept(err.to_string()))?;
    Ok(true)
}

/// The call `id` names, opened with the audit key: exactly what the
/// judge was sent, for the owner.
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
    let unreadable = || {
        Failure::new(
            code::read_failed,
            format!("gatekeeper request {id} does not open with this host's audit key"),
        )
    };
    let sealed: Sealed = serde_json::from_slice(&text).map_err(|_| unreadable())?;
    let keys = Keys::existing(state_dir)?.ok_or_else(unreadable)?;
    let plain = keys
        .open_bytes(&sealed.kid, &sealed.sealed, &aad(id))
        .ok_or_else(unreadable)?;
    let kept: Value = serde_json::from_slice(&plain).map_err(|_| unreadable())?;
    Ok(kept)
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
