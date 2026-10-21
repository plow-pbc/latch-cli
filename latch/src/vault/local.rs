//! Owner management on this state folder; only phone links need a live serve.

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use tokio::net::{UnixListener, UnixStream};

use super::{client, host, import};
use crate::audit::{self, AuditLog};
use crate::failure::{Exit, Failure};
use crate::state::StateDir;
use crate::wire::{self, Response};

fn audited(
    state: &StateDir,
    command: &str,
    action: impl FnOnce(&host::Vault, &mut Map<String, Value>) -> Result<Value, Failure>,
) -> Result<Value, Failure> {
    let config = state.require_login()?;
    // Refuse a broken or unwritable audit before changing the vault.
    let mut log = AuditLog::open(state.path.join("audit.ndjson"))?;
    let vault = host::Vault::new(state.path.clone());
    let mut args = Map::new();
    args.insert("command".into(), json!(command));
    let outcome = host::owner_only(&config.peer.kind).and_then(|()| action(&vault, &mut args));
    let done = host::done(outcome, args);
    let mut record = audit::record(
        &config.peer.key,
        None,
        "vault",
        crate::serve::decision(&done.response),
        &done.outcome,
    );
    record.args = done.audit_args;
    let id = log.append(record)?;
    result(crate::serve::with_audit_id(done.response, id))
}

pub(super) fn call(state: &StateDir, args: Value) -> Result<Value, Failure> {
    let command = args["command"].as_str().unwrap_or_default();
    audited(state, command, |vault, audit| {
        host::manage(vault, &args, audit)
    })
}

pub(super) fn import(state: &StateDir, path: &Path) -> Result<Value, Failure> {
    audited(state, "import", |vault, audit| {
        let io = |err| Failure::usage("local_file", format!("{}: {err}", path.display()));
        let mut bytes = Vec::new();
        fs::File::open(path)
            .map_err(io)?
            .take(300 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(io)?;
        if bytes.len() > 300 * 1024 * 1024 {
            return Err(Failure::usage(
                "file_too_large",
                "that export is too large".into(),
            ));
        }
        let mut parsed = import::parse(&bytes)?;
        drop(bytes);
        vault.with(|store| import::mark_against(store, &mut parsed))?;
        let preview = import::preview(&parsed);
        let picks = client::choose(&preview)?;
        // No lock is held while the owner chooses. Commit checks current revisions.
        let outcome = vault.with(|store| Ok(import::commit(store, &parsed, &picks)))?;
        audit.insert("source".into(), json!(parsed.source));
        audit.insert("rows".into(), json!(parsed.items.len()));
        audit.insert("picked".into(), json!(picks.len()));
        audit.insert("result".into(), json!({"saved": outcome.saved, "updated": outcome.updated, "duplicates": outcome.duplicates, "failed": outcome.failed.len()}));
        let mut result = json!(outcome);
        result["source"] = preview["source"].clone();
        result["picked"] = json!(picks);
        Ok(result)
    })
}

pub(super) fn result(response: Response) -> Result<Value, Failure> {
    match response {
        Response::Ok(value) => Ok(value),
        Response::Error {
            exit,
            code,
            message,
            details,
        } => {
            let mut failure = Failure::new(exit, &code, message);
            failure.details = details;
            Err(failure)
        }
    }
}

pub(super) async fn link(state: &StateDir, args: Value) -> Result<Value, Failure> {
    host::owner_only(&state.require_login()?.peer.kind)?;
    let mut socket = UnixStream::connect(state.path.join("vault-link.sock")).await.map_err(|err| {
        if matches!(err.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused) {
            Failure::new(Exit::Unreachable, "serve_required", "phone links need this machine's live receiver; start `latch serve` on this state folder, then retry `latch vault link`".into())
        } else {
            Failure::failed("local_socket", format!("could not reach this machine's phone-link receiver: {err}"))
        }
    })?;
    let lost = |_| {
        Failure::new(
            Exit::Unknown,
            "connection_lost",
            "the local phone-link receiver did not answer; check before retrying".into(),
        )
    };
    wire::write_frame(
        &mut socket,
        &serde_json::to_vec(&args).expect("args serialize"),
    )
    .await
    .map_err(lost)?;
    let bytes = tokio::time::timeout(crate::dial::ANSWER_WITHIN, wire::read_frame(&mut socket))
        .await
        .map_err(|_| lost(std::io::Error::from(std::io::ErrorKind::TimedOut)))?
        .map_err(lost)?;
    let response = serde_json::from_slice(&bytes).map_err(|_| {
        Failure::failed(
            "bad_response",
            "the local phone-link receiver sent an invalid response".into(),
        )
    })?;
    result(response)
}

/// The socket grants only link creation; its directory and mode restrict it to the owner.
pub(crate) struct LinkSocket {
    pub listener: UnixListener,
    path: PathBuf,
}

impl LinkSocket {
    pub fn bind(state: &Path) -> Result<Self, Failure> {
        let path = state.join("vault-link.sock");
        let io = |err| Failure::failed("local_socket", format!("{}: {err}", path.display()));
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(io(err)),
        }
        let listener = UnixListener::bind(&path).map_err(io)?;
        let socket = Self {
            listener,
            path: path.clone(),
        };
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(io)?;
        Ok(socket)
    }
}

impl Drop for LinkSocket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
