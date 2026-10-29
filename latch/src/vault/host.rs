//! The host's vault commands, shared by owner devices and local management.
//! Agents get exit 4. Store transactions use a file lock across processes.
//!
//! The audit records which item and kind, never a value.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::transport::{RecvStream, SendStream};
use serde_json::{Map, Value, json};

use super::import;
use super::item::{Item, Raw, kind_code};
use super::link::{self, Here};
use super::store::{Backend, OwnStore};
use crate::failure::{Failure, code};
use crate::serve::Done;
use crate::wire::{self, Response};

/// How long a previewed import waits for the owner's pick; past it, or
/// when the owner's connection drops, the export is dropped unsaved.
const PICK_WITHIN: Duration = Duration::from_secs(15 * 60);

/// The host's vault location and open phone links.
pub struct Vault {
    dir: PathBuf,
    /// Open phone links, by the sha256 of their code (see `link.rs`).
    pub(super) links: Mutex<HashMap<String, super::link::Request>>,
}

impl Vault {
    pub fn new(dir: PathBuf) -> Arc<Vault> {
        Arc::new(Vault {
            dir,
            links: Mutex::new(HashMap::new()),
        })
    }

    /// Every item's metadata, without creating a vault where none exists.
    pub fn list_if_present(&self) -> Vec<super::item::Meta> {
        if !self.dir.join("vault.json").exists() {
            return Vec::new();
        }
        self.with(|store| Ok(store.list())).unwrap_or_default()
    }

    /// Opens the current store and runs `f` under the shared file lock.
    pub fn with<T>(
        &self,
        f: impl FnOnce(&mut OwnStore) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        // Lock a stable sidecar: vault.json itself is replaced atomically on save.
        let path = self.dir.join("vault.lock");
        let io = |err| Failure::new(code::vault_failed, format!("{}: {err}", path.display()));
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(io)?;
        lock.lock().map_err(io)?;
        let mut store = OwnStore::open(&self.dir)?;
        f(&mut store)
    }
}

/// `vault`: one owner command, or an agent's list. `kind` is the caller's
/// kind as Plow says.
pub async fn run(
    vault: &Vault,
    here: &Here,
    kind: &str,
    args: Value,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Done {
    let command = args["command"].as_str().unwrap_or_default().to_string();
    let mut audit = Map::new();
    audit.insert("command".into(), json!(command));
    // The gatekeeper has judged an agent's list before it gets here.
    if kind == "agent" && command == "list" {
        return done(vault.with(|store| Ok(for_agent(&store.list()))), audit);
    }
    if let Err(failure) = owner_only(kind) {
        return done(Err(failure), audit);
    }
    let result = match command.as_str() {
        "link" => link::open(vault, here, &args, &mut audit),
        "import" => import(vault, &args, send, recv, &mut audit).await,
        _ => manage(vault, &args, &mut audit),
    };
    done(result, audit)
}

/// Non-streaming management, after the caller has passed the owner check.
pub(super) fn manage(
    vault: &Vault,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    match args["command"].as_str().unwrap_or_default() {
        "list" => vault.with(|store| Ok(json!({"items": store.list()}))),
        "show" => show(vault, args, audit),
        "add" => add(vault, args, audit),
        "edit" => edit(vault, args, audit),
        "remove" => remove(vault, args, audit),
        command => Err(Failure::new(
            code::bad_command,
            format!("unknown vault command {command:?}"),
        )),
    }
}

/// How an agent signs in with a login the owner saved.
const SIGN_IN: &str = "To sign in with a saved login, open the sign-in page of a site it is saved for and run `latch browser fill` there without --item: latch uses the login saved for that site, or lists the ones saved there to choose from; --item takes an item's id (vN), never its title. No value is ever shown: a fill puts it in the page";

/// The vault as an agent sees it: each item's id, title and saved sites,
/// never a value or a username, and how to sign in with one.
fn for_agent(items: &[super::item::Meta]) -> Value {
    let items: Vec<Value> = items
        .iter()
        .map(|meta| json!({"item": meta.item, "title": meta.title, "sites": meta.sites}))
        .collect();
    json!({"items": items, "next": SIGN_IN})
}

/// Only the owner's own devices may use the vault's commands, but for an
/// agent's list; an agent's key, whoever's agent, gets exit 4, and how to
/// sign in without them.
pub(super) fn owner_only(kind: &str) -> Result<(), Failure> {
    if kind == "device" {
        return Ok(());
    }
    Err(Failure::new(
        code::owner_only,
        format!(
            "the vault takes commands from the owner's own devices only, but for an agent's `latch vault list --host HOST`: each item's id, title and saved sites. {SIGN_IN}"
        ),
    ))
}

pub(crate) fn done(result: Result<Value, Failure>, audit: Map<String, Value>) -> Done {
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

/// The item `args` describes (kind, title, sites, values by label), not
/// yet checked.
pub(super) fn item_from(args: &Value) -> Result<Item, Failure> {
    let bad = |message: String| Failure::new(code::bad_item, message);
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
    Ok(Item {
        id: String::new(),
        revision: 0,
        sites: strings(&args["sites"]),
        other_top_sites: strings(&args["other_top_sites"]),
        raw,
    })
}

pub(super) fn add(
    vault: &Vault,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let item = item_from(args)?;
    let meta = vault.with(|store| store.add(item))?;
    audit.insert("item".into(), json!(meta.item));
    audit.insert("kind".into(), json!(meta.kind));
    audit.insert("revision".into(), json!(meta.revision));
    Ok(json!({"item": meta}))
}

/// Changes an item at its current revision: the title, the sites (each
/// list given replaces the old one) and fields by label, where an empty
/// value clears the field. The kind never changes. The audit gets the
/// names of the fields that changed, never a value.
pub fn edit(vault: &Vault, args: &Value, audit: &mut Map<String, Value>) -> Result<Value, Failure> {
    let bad = |message: String| Failure::new(code::bad_item, message);
    let (Some(id), Some(revision)) = (args["item"].as_str(), args["revision"].as_u64()) else {
        return Err(Failure::new(
            code::bad_arguments,
            "edit needs the item and its revision".into(),
        ));
    };
    audit.insert("item".into(), json!(id));
    let values: Vec<(String, String)> = args["values"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(label, value)| Some((label.clone(), value.as_str()?.to_string())))
        .collect();
    let (meta, changed) = vault.with(|store| {
        let current = store
            .item(id)
            .ok_or_else(|| Failure::new(code::no_such_item, format!("there is no item {id}")))?;
        if let Some(kind) = args["kind"].as_str()
            && kind_code(kind) != Some(current.raw.kind)
        {
            return Err(bad("an item's kind can't change".into()));
        }
        let mut edited = current.clone();
        if let Some(title) = args["title"].as_str() {
            edited.raw.name = title.to_string();
        }
        if args["sites"].is_array() {
            edited.sites = strings(&args["sites"]);
        }
        if args["other_top_sites"].is_array() {
            edited.other_top_sites = strings(&args["other_top_sites"]);
        }
        for (label, value) in &values {
            edited.raw.set(label, value).map_err(bad)?;
        }
        edited.validate()?;
        let mut changed = Vec::new();
        if edited.raw.name != current.raw.name {
            changed.push("title".to_string());
        }
        if edited.sites != current.sites {
            changed.push("sites".to_string());
        }
        if edited.other_top_sites != current.other_top_sites {
            changed.push("other_top_sites".to_string());
        }
        for (label, _) in &values {
            if edited.raw.get(label) != current.raw.get(label) && !changed.contains(label) {
                changed.push(label.clone());
            }
        }
        Ok((store.update(edited, revision)?, changed))
    })?;
    audit.insert("kind".into(), json!(meta.kind));
    audit.insert("changed".into(), json!(changed));
    audit.insert("revision".into(), json!(meta.revision));
    Ok(json!({"item": meta, "changed": changed}))
}

/// One item's metadata and the values of its fields the classifier does
/// not hold secret, by label; a secret field is in the metadata's
/// `fields`, never here.
fn show(vault: &Vault, args: &Value, audit: &mut Map<String, Value>) -> Result<Value, Failure> {
    let id = args["item"].as_str().unwrap_or_default();
    audit.insert("item".into(), json!(id));
    let item = vault.with(|store| {
        store
            .item(id)
            .ok_or_else(|| Failure::new(code::no_such_item, format!("there is no item {id}")))
    })?;
    let meta = item.meta();
    let values: Map<String, Value> = meta
        .fields
        .iter()
        .filter(|field| !field.hidden)
        .filter_map(|field| {
            let value = super::classify::read(&item.raw, &field.label)?;
            Some((field.label.clone(), json!(value)))
        })
        .collect();
    Ok(json!({"item": meta, "values": values}))
}

fn remove(vault: &Vault, args: &Value, audit: &mut Map<String, Value>) -> Result<Value, Failure> {
    let (Some(item), Some(revision)) = (args["item"].as_str(), args["revision"].as_u64()) else {
        return Err(Failure::new(
            code::bad_arguments,
            "remove needs the item and its revision".into(),
        ));
    };
    audit.insert("item".into(), json!(item));
    audit.insert("revision".into(), json!(revision));
    vault.with(|store| store.remove(item, revision))?;
    Ok(json!({"removed": item}))
}

fn lost() -> Failure {
    Failure::new(
        code::connection_lost,
        "the import broke off; nothing was saved".into(),
    )
}

/// An export sent on the stream after the host's go-ahead, parsed and
/// marked against the vault.
async fn receive(
    vault: &Vault,
    args: &Value,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Result<import::Parsed, Failure> {
    let size = args["size"]
        .as_u64()
        .ok_or_else(|| Failure::new(code::bad_arguments, "the export's size is missing".into()))?;
    if size > 300 * 1024 * 1024 {
        return Err(Failure::new(
            code::file_too_large,
            "that export is too large".into(),
        ));
    }
    wire::write_frame(send, &frame(json!({})))
        .await
        .map_err(|_| lost())?;
    let mut bytes = Vec::with_capacity(size as usize);
    wire::copy(recv, &mut bytes, size)
        .await
        .map_err(|_| lost())?;
    let mut parsed = import::parse(&bytes)?;
    drop(bytes);
    vault.with(|store| import::mark_against(store, &mut parsed))?;
    Ok(parsed)
}

fn frame(value: Value) -> Vec<u8> {
    serde_json::to_vec(&Response::Ok(value)).expect("serializes")
}

/// `preview`: what an export sent on the stream holds, as `import` shows
/// it. It changes nothing, so serve answers it unaudited, as a rules
/// preview; the export is dropped as soon as it is answered.
pub async fn preview(
    vault: &Vault,
    kind: &str,
    args: &Value,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Response {
    let previewed = async {
        owner_only(kind)?;
        let parsed = receive(vault, args, send, recv).await?;
        Ok(json!({"preview": import::preview(&parsed)}))
    };
    match previewed.await {
        Ok(answer) => Response::Ok(answer),
        Err(failure) => Response::error(&failure),
    }
}

/// One import on one stream: the host's go-ahead, the export's bytes,
/// the preview, the owner's picks, then the outcome. The export lives only
/// while this runs.
async fn import(
    vault: &Vault,
    args: &Value,
    send: &mut SendStream,
    recv: &mut RecvStream,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let mut parsed = receive(vault, args, send, recv).await?;
    audit.insert("source".into(), json!(parsed.source));
    audit.insert("rows".into(), json!(parsed.items.len()));
    wire::write_frame(send, &frame(json!({"preview": import::preview(&parsed)})))
        .await
        .map_err(|_| lost())?;
    let picked = match tokio::time::timeout(PICK_WITHIN, wire::read_frame(recv)).await {
        Ok(Ok(json)) => serde_json::from_slice::<Value>(&json).map_err(|_| lost())?,
        Ok(Err(_)) => return Err(lost()),
        Err(_) => {
            return Err(Failure::new(
                code::import_expired,
                "no pick came in time; nothing was saved; run `latch vault import` again".into(),
            ));
        }
    };
    let picks: Vec<usize> = picked["picks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_u64().map(|p| p as usize))
        .collect();
    let outcome = vault.with(|store| match picked["expect"].as_str() {
        Some(expect) => import::commit_expecting(store, &mut parsed, &picks, expect),
        None => Ok(import::commit(store, &parsed, &picks)),
    })?;
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
    use crate::failure::Exit;

    #[test]
    fn an_edit_changes_what_it_names_at_its_revision_and_audits_only_names() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().to_path_buf());
        let mut audit = Map::new();
        add(
            &vault,
            &json!({"kind": "login", "title": "Shop", "sites": ["shop.example"],
                    "values": {"username": "ada", "password": "made-up-old-4j2x"}}),
            &mut audit,
        )
        .unwrap();
        let mut audit = Map::new();
        let edited = edit(
            &vault,
            &json!({"item": "v1", "revision": 1, "title": "Shop (new)",
                    "sites": ["www.shop.example", "shop-pay.example"],
                    "values": {"username": "ada", "password": "made-up-new-8q7w",
                               "totp": "JBSWY3DPEHPK3PXP"}}),
            &mut audit,
        )
        .unwrap();
        assert_eq!(
            edited["changed"],
            json!(["title", "sites", "password", "totp"])
        );
        assert_eq!(edited["item"]["revision"], 2);
        assert_eq!(
            edited["item"]["sites"],
            json!(["shop.example", "shop-pay.example"])
        );
        assert_eq!(
            Value::Object(audit.clone()),
            json!({"item": "v1", "kind": "login", "revision": 2,
                   "changed": ["title", "sites", "password", "totp"]})
        );
        for shown in [edited.to_string(), Value::Object(audit).to_string()] {
            for value in ["made-up-old-4j2x", "made-up-new-8q7w", "JBSWY3DPEHPK3PXP"] {
                assert!(!shown.contains(value), "{value} in {shown}");
            }
        }
        let release = |label| vault.with(|s| s.release("v1", label)).unwrap();
        assert_eq!(release("password"), "made-up-new-8q7w");
        assert_eq!(release("totp"), "JBSWY3DPEHPK3PXP");

        let refused = |args: Value| edit(&vault, &args, &mut Map::new()).unwrap_err();
        let stale = refused(json!({"item": "v1", "revision": 1, "title": "Again"}));
        assert_eq!(
            (stale.exit, stale.code.as_str()),
            (Exit::Denied, "stale_revision")
        );
        let kind = refused(json!({"item": "v1", "revision": 2, "kind": "card"}));
        assert_eq!(kind.code, "bad_item");
        assert!(kind.message.contains("kind can't change"));
        assert_eq!(
            refused(json!({"item": "v1", "revision": 2, "values": {"code": "1"}})).code,
            "bad_item"
        );
        // A login keeps a site.
        assert_eq!(
            refused(json!({"item": "v1", "revision": 2, "sites": []})).code,
            "bad_item"
        );
        // Nothing refused changed the item.
        let meta = vault.with(|s| Ok(s.list())).unwrap();
        assert_eq!(
            (meta[0].revision, meta[0].title.as_str()),
            (2, "Shop (new)")
        );
    }

    #[test]
    fn an_edit_clears_a_site_list_where_the_kind_allows() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().to_path_buf());
        add(
            &vault,
            &json!({"kind": "card", "title": "Visa", "sites": ["shop.example"],
                    "values": {"number": "4242424242424242"}}),
            &mut Map::new(),
        )
        .unwrap();
        add(
            &vault,
            &json!({"kind": "login", "title": "Shop", "sites": ["shop.example"],
                    "other_top_sites": ["pay.example"], "values": {"password": "made-up-6t1v"}}),
            &mut Map::new(),
        )
        .unwrap();
        let mut audit = Map::new();
        let card = edit(
            &vault,
            &json!({"item": "v1", "revision": 1, "sites": []}),
            &mut audit,
        )
        .unwrap();
        assert_eq!(card["item"]["sites"], json!([]));
        assert_eq!(audit["changed"], json!(["sites"]));
        let login = edit(
            &vault,
            &json!({"item": "v2", "revision": 1, "other_top_sites": []}),
            &mut Map::new(),
        )
        .unwrap();
        assert_eq!(login["changed"], json!(["other_top_sites"]));
        assert!(login["item"].get("other_top_sites").is_none());
        assert_eq!(login["item"]["sites"], json!(["shop.example"]));
    }

    #[test]
    fn only_owner_devices_reach_the_vault() {
        assert!(owner_only("device").is_ok());
        for kind in ["agent", "", "Device"] {
            let refused = owner_only(kind).unwrap_err();
            assert_eq!(
                (refused.exit, refused.code.as_str()),
                (Exit::Denied, "owner_only")
            );
            // What an agent may do instead: list, and fill by the page's site.
            assert!(
                refused.message.contains("`latch vault list --host HOST`")
                    && refused
                        .message
                        .contains("run `latch browser fill` there without --item"),
                "{refused:?}"
            );
        }
    }

    /// One import over a real stream, from the client's side: the go-ahead,
    /// the bytes, the preview, then `picks`, or a dropped connection.
    async fn import_over_a_stream(vault: &Vault, picks: Option<Value>) -> Result<Value, Failure> {
        use iroh::RelayMode;
        let bind = |alpns: Vec<Vec<u8>>| {
            crate::dial::endpoint()
                .relay_mode(RelayMode::Disabled)
                .alpns(alpns)
                .bind()
        };
        let host = bind(vec![wire::ALPN.to_vec()]).await.unwrap();
        let client = bind(vec![]).await.unwrap();
        let accepting = host.accept();
        let (accepted, connection) =
            tokio::join!(async { accepting.await.unwrap().await.unwrap() }, async {
                client.connect(host.addr(), wire::ALPN).await.unwrap()
            });
        let export = fs_export();
        let serving = async {
            let (send, mut recv) = accepted.accept_bi().await.unwrap();
            let request: Value =
                serde_json::from_slice(&wire::read_frame(&mut recv).await.unwrap()).unwrap();
            let args = request["args"].clone();
            let (mut send, mut recv) = (send.into(), recv.into());
            let result = import(vault, &args, &mut send, &mut recv, &mut Map::new()).await;
            if let Ok(outcome) = &result {
                let answer = serde_json::to_vec(&Response::Ok(outcome.clone())).unwrap();
                wire::write_frame(&mut send, &answer).await.unwrap();
                send.finish().unwrap();
                if let SendStream::Remote(send) = &mut send {
                    let _ = send.stopped().await;
                }
            }
            result
        };
        let owner = async {
            let (mut send, mut recv) = connection.open_bi().await.unwrap();
            let next = |json: Vec<u8>| serde_json::from_slice::<Value>(&json).unwrap();
            let request =
                json!({"op": "vault", "args": {"command": "import", "size": export.len()}});
            wire::write_frame(&mut send, &serde_json::to_vec(&request).unwrap())
                .await
                .unwrap();
            assert_eq!(
                next(wire::read_frame(&mut recv).await.unwrap()),
                json!({"ok": {}})
            );
            wire::copy(&mut &export[..], &mut send, export.len() as u64)
                .await
                .unwrap();
            let preview = next(wire::read_frame(&mut recv).await.unwrap());
            assert_eq!(
                preview["ok"]["preview"]["items"].as_array().unwrap().len(),
                2
            );
            match &picks {
                Some(picks) => {
                    let picked = serde_json::to_vec(&json!({"picks": picks})).unwrap();
                    wire::write_frame(&mut send, &picked).await.unwrap();
                    send.finish().unwrap();
                    let _ = wire::read_frame(&mut recv).await.unwrap();
                }
                None => connection.close(0u32.into(), b"gone"),
            }
        };
        let (result, ()) = tokio::join!(serving, owner);
        client.close().await;
        host.close().await;
        result
    }

    fn fs_export() -> Vec<u8> {
        std::fs::read(format!(
            "{}/tests/fixtures/import/chrome.csv",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn an_import_is_one_stream_and_a_dropped_owner_saves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::new(dir.path().to_path_buf());

        let dropped = import_over_a_stream(&vault, None).await.unwrap_err();
        assert_eq!(dropped.code, "connection_lost");
        assert!(vault.list_if_present().is_empty());

        let outcome = import_over_a_stream(&vault, Some(json!([1, 2])))
            .await
            .unwrap();
        assert_eq!(outcome["saved"], 2);
        assert_eq!(vault.list_if_present().len(), 2);
    }
}

#[cfg(test)]
mod local_tests {
    use super::*;

    #[test]
    fn vault_transactions_reopen_edits_and_keep_both_writers_items() {
        let dir = tempfile::tempdir().unwrap();
        let serving = Vault::new(dir.path().into());
        let local = Vault::new(dir.path().into());
        assert!(serving.with(|s| Ok(s.list())).unwrap().is_empty());
        let args = json!({"kind": "login", "title": "the owner", "sites": ["example.com"], "values": {"username": "the owner", "password": "synthetic-local-value"}});
        add(&local, &args, &mut Map::new()).unwrap();
        assert_eq!(
            serving.with(|s| s.release("v1", "password")).unwrap(),
            "synthetic-local-value"
        );
        add(&serving, &args, &mut Map::new()).unwrap();
        assert_eq!(local.with(|s| Ok(s.list().len())).unwrap(), 2);
    }
}
