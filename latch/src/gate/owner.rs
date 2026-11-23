//! The `rules` operation: the owner's commands on a host's device rule
//! and agent map, from the owner's own devices only (an agent's key gets
//! exit 4 `owner_only`, for every command, `show` included). `latch rules`
//! and `latch agents map` send it over latch's connection with `--host`,
//! and run it on this machine's own host state without.
//!
//! The audit gets each change's revisions before and after, and the
//! agent uid and Swarm account a mapping names; never the rule's text.

use std::path::Path;

use serde_json::{Map, Value, json};

use super::rules::{Change, Snapshot, Store, revision};
use crate::failure::{Failure, code};
use crate::plow::Plow;
use crate::serve::Done;

/// What `show` always says.
pub const WARNING: &str = "The device rule only denies: an agent acts only on the owner's message it cites. It is judged, not guaranteed; --deny-exec is enforced in code.";

/// `rules`: one owner command on the device rule or agent map in
/// `state_dir`, for the host named `host`. `kind` is the caller's kind as
/// Plow says.
pub async fn run(state_dir: &Path, plow: &Plow, host: &str, kind: &str, args: &Value) -> Done {
    changing(state_dir, plow, host, kind, args).await.0
}

/// `run`, and for a change it made, the file as it was, locked: the caller
/// audits the change and, if the audit can't be written, puts the file
/// back, then drops it.
pub async fn changing(
    state_dir: &Path,
    plow: &Plow,
    host: &str,
    kind: &str,
    args: &Value,
) -> (Done, Option<Snapshot>) {
    let command = args["command"].as_str().unwrap_or_default();
    let mut audit = Map::new();
    audit.insert("command".into(), json!(command));
    let store = Store::new(state_dir);
    let mut snapshot = None;
    let result = match owner_only(kind, "the device rule") {
        Err(failure) => Err(failure),
        Ok(()) if !args["from"].is_null() => Err(Failure::new(
            code::bad_arguments,
            "a host has one device rule, for every agent: per-agent rules are gone".into(),
        )),
        Ok(()) => match command {
            "show" => show(state_dir, &store, host),
            "set" | "edit" | "clear" | "clear_at_revision" => {
                let expected = matches!(command, "edit" | "clear_at_revision")
                    .then(|| args["revision"].as_str());
                let text = match command {
                    "set" | "edit" => args["text"].as_str().ok_or_else(|| {
                        Failure::new(
                            code::bad_arguments,
                            "the device rule's text is missing".into(),
                        )
                    }),
                    _ => Ok(""),
                };
                text.and_then(|text| {
                    let held = store.snapshot_deny()?;
                    let change = match command {
                        "set" | "edit" => store.set_deny(text, expected),
                        _ => store.clear_deny(expected),
                    }?;
                    snapshot = Some(held);
                    Ok(changed(host, &change, &mut audit))
                })
            }
            "map_agent" => map_agent(&store, plow, host, args, &mut audit, &mut snapshot).await,
            _ => Err(Failure::new(
                code::bad_command,
                format!("unknown rules command {command:?}"),
            )),
        },
    };
    (crate::vault::host::done(result, audit), snapshot)
}

/// Puts back a change whose audit record couldn't be written; a failure
/// to put it back is logged, never silent.
pub fn undo(snapshot: Option<Snapshot>) {
    if let Some(snapshot) = snapshot
        && let Err(err) = snapshot.restore()
    {
        crate::log::log(&format!(
            "rules: an unaudited change could not be put back: {err}"
        ));
    }
}

/// Only the owner's own devices may use `what` (the device rule, a host's
/// audit): an agent's key gets exit 4.
pub(crate) fn owner_only(kind: &str, what: &str) -> Result<(), Failure> {
    if kind == "device" {
        return Ok(());
    }
    Err(Failure::new(
        code::owner_only,
        format!("only the owner's own devices may use {what}"),
    ))
}

fn show(state_dir: &Path, store: &Store, host: &str) -> Result<Value, Failure> {
    let rule = store
        .deny()
        .map_err(|why| Failure::new(code::rules_io, why))?;
    let agents = store
        .agents()
        .map_err(|why| Failure::new(code::rules_io, why))?;
    Ok(json!({
        "host": host,
        "device_rule": rule.as_deref().map(|text| json!({"text": text, "revision": revision(text)})),
        "agents": agents,
        "deny_exec": crate::managed::saved_deny_exec(state_dir),
        "warning": WARNING,
    }))
}

fn changed(host: &str, change: &Change, audit: &mut Map<String, Value>) -> Value {
    audit.insert("before".into(), json!(change.before));
    audit.insert("after".into(), json!(change.after));
    json!({
        "host": host,
        "revision": change.after,
        "cleared": change.after.is_none() && change.before.is_some(),
        "applies": "applies to the next judgment",
    })
}

/// Maps the agent `agent` names (its name or its uid) to the Swarm
/// account `swarm_id`.
async fn map_agent(
    store: &Store,
    plow: &Plow,
    host: &str,
    args: &Value,
    audit: &mut Map<String, Value>,
    snapshot: &mut Option<Snapshot>,
) -> Result<Value, Failure> {
    let (Some(agent), Some(swarm_id)) = (args["agent"].as_str(), args["swarm_id"].as_str()) else {
        let message = "map_agent needs an agent and a swarm_id".to_string();
        return Err(Failure::new(code::bad_arguments, message));
    };
    // Plow is asked before the map is locked: a slow answer holds up no
    // other change.
    let agent = crate::agents::resolve(plow, agent).await?;
    let held = store.snapshot_agents()?;
    let before = store.map_agent(&agent.uid, swarm_id)?;
    *snapshot = Some(held);
    audit.insert("agent_uid".into(), json!(agent.uid));
    audit.insert("swarm_id".into(), json!(swarm_id));
    audit.insert("before".into(), json!(before));
    Ok(json!({
        "host": host,
        "agent_uid": agent.uid,
        "agent_name": agent.name,
        "swarm_id": swarm_id,
        "replaced": before,
    }))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::failure::Exit;
    use crate::wire::Response;

    const ADA: &str = "0123456789abcdef0123456789abcdef";
    const BOB: &str = "fedcba9876543210fedcba9876543210";

    /// The synthetic Plow's agents, by uid and name.
    struct World {
        agents: Vec<(String, String)>,
    }

    impl World {
        fn answer(&self, target: &str) -> (u16, Value) {
            if target != "/v1/agents" {
                return (404, json!({}));
            }
            let agents: Vec<Value> = self
                .agents
                .iter()
                .map(|(uid, name)| {
                    json!({"uid": uid, "name": name, "provider": "self_hosted",
                           "line": {"uid": "line", "provider_key": "+14155550100", "display_name": null},
                           "created_at": "2026-11-01T00:00:00Z", "status": null,
                           "failure_code": null, "credential": null})
                })
                .collect();
            (200, json!(agents))
        }
    }

    /// The account the tests start from: Ada and Bob.
    fn world() -> World {
        World {
            agents: vec![(ADA.into(), "Ada".into()), (BOB.into(), "Bob".into())],
        }
    }

    fn plow(world: &Arc<Mutex<World>>) -> Plow {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let world = world.clone();
        std::thread::spawn(move || {
            for socket in listener.incoming() {
                let mut socket = socket.unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                let target = first.split(' ').nth(1).unwrap().to_string();
                let (status, body) = world.lock().unwrap().answer(&target);
                let body = body.to_string();
                let _ = write!(
                    socket,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Plow::new(&url, "made-up-login").unwrap()
    }

    /// A host's state folder and its Plow.
    struct Host {
        dir: tempfile::TempDir,
        plow: Plow,
    }

    impl Host {
        fn new() -> Host {
            let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
            std::fs::create_dir_all(&parent).unwrap();
            let world = Arc::new(Mutex::new(world()));
            Host {
                dir: tempfile::tempdir_in(parent).unwrap(),
                plow: plow(&world),
            }
        }

        async fn done(&self, kind: &str, args: Value) -> Done {
            run(self.dir.path(), &self.plow, "studio", kind, &args).await
        }

        /// The owner's command, as its result or its error.
        async fn owner(&self, args: Value) -> Result<Value, (Exit, String, String)> {
            match self.done("device", args).await.response {
                Response::Ok(value) => Ok(value),
                Response::Error {
                    exit,
                    code,
                    message,
                    ..
                } => Err((exit, code, message)),
            }
        }
    }

    fn refusal(result: Result<Value, (Exit, String, String)>) -> (Exit, String) {
        let (exit, code, _) = result.unwrap_err();
        (exit, code)
    }

    #[tokio::test]
    async fn the_owner_sets_edits_shows_and_clears_the_device_rule() {
        let host = Host::new();
        let shown = host.owner(json!({"command": "show"})).await.unwrap();
        assert_eq!(shown["device_rule"], Value::Null);
        let set = host
            .owner(json!({"command": "set", "text": "Never touch my tax returns."}))
            .await
            .unwrap();
        assert_eq!(set["revision"], revision("Never touch my tax returns."));
        let stale = host
            .owner(json!({"command": "edit", "text": "x", "revision": revision("other")}))
            .await;
        assert_eq!(refusal(stale).1, "stale_revision");
        let shown = host.owner(json!({"command": "show"})).await.unwrap();
        assert_eq!(shown["device_rule"]["text"], "Never touch my tax returns.");
        // An empty rule is an answer: it denies nothing, and stays set.
        host.owner(json!({"command": "set", "text": ""}))
            .await
            .unwrap();
        assert_eq!(
            Store::new(host.dir.path()).deny().unwrap().as_deref(),
            Some("")
        );
        let cleared = host.owner(json!({"command": "clear"})).await.unwrap();
        assert_eq!(cleared["cleared"], true);
        assert_eq!(Store::new(host.dir.path()).deny().unwrap(), None);
    }

    /// A change whose audit record can't be written is put back: the rule
    /// and the agent map are as they were, and a refused change has
    /// nothing to put back.
    #[tokio::test]
    async fn an_unaudited_change_is_put_back() {
        let host = Host::new();
        host.owner(json!({"command": "set", "text": "Never pay."}))
            .await
            .unwrap();
        let changing = |args: Value| {
            let dir = host.dir.path().to_path_buf();
            let plow = host.plow.clone();
            async move { changing(&dir, &plow, "studio", "device", &args).await }
        };
        let store = Store::new(host.dir.path());
        for (args, kept) in [
            (
                json!({"command": "set", "text": "Anything goes."}),
                Some("Never pay."),
            ),
            (json!({"command": "clear"}), Some("Never pay.")),
        ] {
            let (done, snapshot) = changing(args.clone()).await;
            assert!(matches!(done.response, Response::Ok(_)), "{args}");
            undo(snapshot);
            assert_eq!(store.deny().unwrap().as_deref(), kept, "{args}");
        }
        let (_, snapshot) =
            changing(json!({"command": "map_agent", "agent": "Ada", "swarm_id": "@ada:plow.co"}))
                .await;
        undo(snapshot);
        assert!(store.agents().unwrap().is_empty());
        let (_, nothing) =
            changing(json!({"command": "edit", "text": "x", "revision": "stale"})).await;
        assert!(nothing.is_none());
    }

    #[tokio::test]
    async fn per_agent_rules_are_refused() {
        let host = Host::new();
        let refused = host
            .owner(json!({"command": "set", "from": "Ada", "text": "Only git."}))
            .await;
        assert_eq!(refusal(refused).1, "bad_arguments");
    }

    #[tokio::test]
    async fn the_owner_maps_an_agent_by_name_to_its_swarm_account() {
        let host = Host::new();
        let mapped = host
            .owner(json!({"command": "map_agent", "agent": "Ada", "swarm_id": "@ada:plow.co"}))
            .await
            .unwrap();
        assert_eq!(
            (mapped["agent_uid"].as_str(), mapped["agent_name"].as_str()),
            (Some(ADA), Some("Ada"))
        );
        assert_eq!(
            Store::new(host.dir.path()).agents().unwrap()[ADA],
            "@ada:plow.co"
        );
    }

    #[tokio::test]
    async fn an_agent_can_neither_read_nor_change_the_rule_or_the_agent_map() {
        let host = Host::new();
        for args in [
            json!({"command": "show"}),
            json!({"command": "set", "text": ""}),
            json!({"command": "clear"}),
            json!({"command": "map_agent", "agent": ADA, "swarm_id": "@evil:plow.co"}),
            json!({"command": "map_agent", "agent": BOB, "swarm_id": "@bob:plow.co"}),
        ] {
            let done = host.done("agent", args.clone()).await;
            let Response::Error { exit, code, .. } = done.response else {
                panic!("an agent ran {args}");
            };
            assert_eq!(
                (exit, code.as_str()),
                (Exit::Denied, "owner_only"),
                "{args}"
            );
        }
        assert!(Store::new(host.dir.path()).agents().unwrap().is_empty());
        assert_eq!(Store::new(host.dir.path()).deny().unwrap(), None);
    }
}
