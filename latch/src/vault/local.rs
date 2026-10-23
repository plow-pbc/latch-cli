//! Owner management on this state folder; only phone links need a live serve.

use std::fs;
use std::io::Read;
use std::path::Path;

use serde_json::{Map, Value, json};

use super::{client, host, import};
use crate::audit::{self, AuditLog};
use crate::failure::Failure;
use crate::state::StateDir;

fn audited(
    state: &StateDir,
    command: &str,
    action: impl FnOnce(&host::Vault, &mut Map<String, Value>) -> Result<Value, Failure>,
) -> Result<Value, Failure> {
    let config = state.require_login()?;
    // Refuse a broken or unwritable audit before changing the vault.
    let mut log = AuditLog::open(state.path.join(audit::FILE))?;
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
    record.caller_name = Some(config.peer.name.clone());
    record.args = done.audit_args;
    let id = log.append(record)?;
    crate::serve::with_audit_id(done.response, id).into_result()
}

pub(super) fn call(state: &StateDir, args: Value) -> Result<Value, Failure> {
    let command = args["command"].as_str().unwrap_or_default();
    audited(state, command, |vault, audit| {
        host::manage(vault, &args, audit)
    })
}

/// An export in `path`, parsed and marked against the vault.
fn read_export(vault: &host::Vault, path: &Path) -> Result<import::Parsed, Failure> {
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
    Ok(parsed)
}

/// What an export holds, as an import would show it. It changes nothing,
/// so, like a rules preview, it is not audited.
pub(super) fn preview(state: &StateDir, path: &Path) -> Result<Value, Failure> {
    let config = state.require_login()?;
    host::owner_only(&config.peer.kind)?;
    let parsed = read_export(&host::Vault::new(state.path.clone()), path)?;
    Ok(json!({"preview": import::preview(&parsed)}))
}

/// Imports the rows `pick` names, or those picked at a prompt; with
/// `expect`, only if they still mean what that preview showed.
pub(super) fn import(
    state: &StateDir,
    path: &Path,
    pick: Option<&str>,
    expect: Option<&str>,
) -> Result<Value, Failure> {
    audited(state, "import", |vault, audit| {
        let mut parsed = read_export(vault, path)?;
        audit.insert("source".into(), json!(parsed.source));
        audit.insert("rows".into(), json!(parsed.items.len()));
        let preview = import::preview(&parsed);
        let picks = client::choose(&preview, pick)?;
        // No lock is held while the owner chooses. Commit checks current revisions.
        let outcome = vault.with(|store| match expect {
            Some(expect) => import::commit_expecting(store, &mut parsed, &picks, expect),
            None => Ok(import::commit(store, &parsed, &picks)),
        })?;
        audit.insert("picked".into(), json!(picks.len()));
        audit.insert("result".into(), json!({"saved": outcome.saved, "updated": outcome.updated, "duplicates": outcome.duplicates, "failed": outcome.failed.len()}));
        let mut result = json!(outcome);
        result["source"] = preview["source"].clone();
        result["picked"] = json!(picks);
        Ok(result)
    })
}

pub(super) async fn link(state: &StateDir, args: Value) -> Result<Value, Failure> {
    let mut peer = state.require_config()?.peer;
    host::owner_only(&peer.kind)?;
    peer.this_machine = true;
    crate::dial::call(
        state,
        &peer,
        &crate::wire::Request {
            op: "vault".into(),
            args,
            chat: None,
        },
        crate::dial::ANSWER_WITHIN,
    )
    .await
}
