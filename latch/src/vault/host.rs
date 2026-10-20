//! The host's side of `latch vault`: the `vault` operation, for owner
//! devices only. An agent's key gets exit 4. The owner's commands reach the
//! vault the same way agents' calls reach the browser, over latch's own
//! connection, even from the host's own machine (through a client state
//! of its own): nothing edits the vault's files directly.
//!
//! The audit records which item and kind, never a value.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::endpoint::{RecvStream, SendStream};
use serde_json::{Map, Value, json};

use super::import::{self, Parsed};
use super::item::{Item, Raw, kind_code};
use super::store::{Backend, OwnStore};
use crate::failure::{Exit, Failure};
use crate::serve::Done;
use crate::wire::{self, Response};

/// How long a previewed import waits for the owner's pick.
const PICK_WITHIN: Duration = Duration::from_secs(15 * 60);

/// The host's one vault, opened on first use.
pub struct Vault {
    dir: PathBuf,
    store: Mutex<Option<OwnStore>>,
    /// Previewed imports awaiting a pick, by id, with who previewed them.
    imports: Mutex<HashMap<String, (String, Instant, Parsed)>>,
    next_import: Mutex<u64>,
}

impl Vault {
    pub fn new(dir: PathBuf) -> Arc<Vault> {
        Arc::new(Vault {
            dir,
            store: Mutex::new(None),
            imports: Mutex::new(HashMap::new()),
            next_import: Mutex::new(0),
        })
    }

    /// Every item's metadata, without creating a vault where none exists.
    pub fn list_if_present(&self) -> Vec<super::item::Meta> {
        let present = self.store.lock().expect("vault lock").is_some()
            || self.dir.join("vault.json").exists();
        if !present {
            return Vec::new();
        }
        self.with(|store| Ok(store.list())).unwrap_or_default()
    }

    /// Runs `f` on the store, opening it first if needed.
    pub fn with<T>(
        &self,
        f: impl FnOnce(&mut OwnStore) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        let mut store = self.store.lock().expect("vault lock");
        if store.is_none() {
            *store = Some(OwnStore::open(&self.dir)?);
        }
        f(store.as_mut().expect("opened"))
    }
}

/// `vault`: one owner command. `kind` is the caller's kind as Plow says.
pub async fn run(
    vault: &Vault,
    caller: &str,
    kind: &str,
    args: Value,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Done {
    let command = args["command"].as_str().unwrap_or_default().to_string();
    let mut audit = Map::new();
    audit.insert("command".into(), json!(command));
    if let Err(failure) = owner_only(kind) {
        return done(Err(failure), audit);
    }
    let result = match command.as_str() {
        "list" => vault.with(|store| Ok(json!({"items": store.list()}))),
        "add" => add(vault, &args, &mut audit),
        "remove" => remove(vault, &args, &mut audit),
        "import_preview" => preview(vault, caller, &args, send, recv, &mut audit).await,
        "import_commit" => commit(vault, caller, &args, &mut audit),
        _ => Err(Failure::usage(
            "bad_command",
            format!("unknown vault command {command:?}"),
        )),
    };
    done(result, audit)
}

/// Only the owner's own devices may use the vault's commands; an agent's
/// key, whoever's agent, gets exit 4.
fn owner_only(kind: &str) -> Result<(), Failure> {
    if kind == "device" {
        return Ok(());
    }
    Err(Failure::new(
        Exit::Denied,
        "owner_only",
        "the vault takes commands from the owner's own devices only".into(),
    ))
}

fn done(result: Result<Value, Failure>, audit: Map<String, Value>) -> Done {
    match result {
        Ok(value) => Done {
            response: Response::Ok(value),
            audit_args: Value::Object(audit),
            outcome: "ok".into(),
        },
        Err(failure) => Done {
            outcome: failure.code.clone(),
            response: Response::error(&failure),
            audit_args: Value::Object(audit),
        },
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

fn add(vault: &Vault, args: &Value, audit: &mut Map<String, Value>) -> Result<Value, Failure> {
    let bad = |message: String| Failure::usage("bad_item", message);
    let kind = args["kind"]
        .as_str()
        .and_then(kind_code)
        .ok_or_else(|| bad("--kind is login, card, identity or note".into()))?;
    let title = args["title"].as_str().unwrap_or_default();
    let values: Vec<(String, String)> = args["values"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(label, value)| Some((label.clone(), value.as_str()?.to_string())))
        .collect();
    let raw = Raw::from_labels(kind, title, &values).map_err(bad)?;
    let item = Item {
        id: String::new(),
        revision: 0,
        sites: strings(&args["sites"]),
        other_top_sites: strings(&args["other_top_sites"]),
        raw,
    };
    let meta = vault.with(|store| store.add(item))?;
    audit.insert("item".into(), json!(meta.item));
    audit.insert("kind".into(), json!(meta.kind));
    audit.insert("revision".into(), json!(meta.revision));
    Ok(json!({"item": meta}))
}

fn remove(vault: &Vault, args: &Value, audit: &mut Map<String, Value>) -> Result<Value, Failure> {
    let (Some(item), Some(revision)) = (args["item"].as_str(), args["revision"].as_u64()) else {
        return Err(Failure::usage(
            "bad_arguments",
            "remove needs the item and its revision".into(),
        ));
    };
    audit.insert("item".into(), json!(item));
    audit.insert("revision".into(), json!(revision));
    vault.with(|store| store.remove(item, revision))?;
    Ok(json!({"removed": item}))
}

/// Reads an export the client sends after the host's go-ahead, marks it
/// against the vault, and keeps it for the owner's pick.
async fn preview(
    vault: &Vault,
    caller: &str,
    args: &Value,
    send: &mut SendStream,
    recv: &mut RecvStream,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let size = args["size"]
        .as_u64()
        .ok_or_else(|| Failure::usage("bad_arguments", "the export's size is missing".into()))?;
    if size > 300 * 1024 * 1024 {
        return Err(Failure::usage(
            "file_too_large",
            "that export is too large".into(),
        ));
    }
    let go = serde_json::to_vec(&Response::Ok(json!({}))).expect("serializes");
    let lost = || {
        Failure::new(
            Exit::Unknown,
            "connection_lost",
            "the export broke off".into(),
        )
    };
    wire::write_frame(send, &go).await.map_err(|_| lost())?;
    let mut bytes = Vec::with_capacity(size as usize);
    wire::copy(recv, &mut bytes, size)
        .await
        .map_err(|_| lost())?;
    let mut parsed = import::parse(&bytes)?;
    drop(bytes);
    vault.with(|store| import::mark_against(store, &mut parsed))?;
    let shown = import::preview(&parsed);
    audit.insert("source".into(), json!(parsed.source));
    audit.insert("rows".into(), json!(parsed.items.len()));
    let id = {
        let mut next = vault.next_import.lock().expect("lock");
        *next += 1;
        format!("i{next}")
    };
    let mut imports = vault.imports.lock().expect("lock");
    imports.retain(|_, (_, at, _)| at.elapsed() < PICK_WITHIN);
    imports.insert(id.clone(), (caller.to_string(), Instant::now(), parsed));
    Ok(json!({"import": id, "preview": shown}))
}

fn commit(
    vault: &Vault,
    caller: &str,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let id = args["import"].as_str().unwrap_or_default();
    let picks: Vec<usize> = args["picks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_u64().map(|p| p as usize))
        .collect();
    let parsed = {
        let mut imports = vault.imports.lock().expect("lock");
        match imports.remove(id) {
            Some((who, at, parsed)) if who == caller && at.elapsed() < PICK_WITHIN => parsed,
            _ => {
                return Err(Failure::usage(
                    "no_such_import",
                    "that import is gone; run `latch vault import` again".into(),
                ));
            }
        }
    };
    let outcome = vault.with(|store| Ok(import::commit(store, &parsed, &picks)))?;
    audit.insert("source".into(), json!(parsed.source));
    audit.insert("picked".into(), json!(picks.len()));
    audit.insert(
        "result".into(),
        json!({"saved": outcome.saved, "updated": outcome.updated, "duplicates": outcome.duplicates, "failed": outcome.failed.len()}),
    );
    Ok(json!(outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_owner_devices_reach_the_vault() {
        assert!(owner_only("device").is_ok());
        for kind in ["agent", "", "Device"] {
            let refused = owner_only(kind).unwrap_err();
            assert_eq!(
                (refused.exit, refused.code.as_str()),
                (Exit::Denied, "owner_only")
            );
        }
    }
}
