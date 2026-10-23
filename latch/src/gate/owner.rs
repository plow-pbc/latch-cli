//! The `rules` operation: the owner's commands on a host's rules, from the
//! owner's own devices only (an agent's key gets exit 4 `owner_only`, for
//! every command, `show` included). `latch rules` sends it over latch's
//! connection with `--host`, and runs it on this machine's own host state
//! without.
//!
//! `--from NAME` is resolved here, once, to an agent uid: an agent's name,
//! the peer name of the latch it calls from, or the uid itself. The audit
//! gets the scope, the uid and the revisions before and after; never text.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value, json};

use super::rules::{Change, RulesDir, Scope, is_agent_uid, revision};
use crate::failure::{Exit, Failure};
use crate::plow::{NamedAgent, Peer, Plow};
use crate::serve::Done;

/// What `show` always says.
pub const WARNING: &str =
    "Freeform rules are judged, not guaranteed. --deny-exec is enforced in code.";

/// `rules`: one owner command on the rules in `state_dir`, for the host
/// named `host`. `kind` is the caller's kind as Plow says.
pub async fn run(state_dir: &Path, plow: &Plow, host: &str, kind: &str, args: &Value) -> Done {
    let command = args["command"].as_str().unwrap_or_default();
    let mut audit = Map::new();
    audit.insert("command".into(), json!(command));
    let result = match owner_only(kind, "the rules") {
        Ok(()) => {
            let dir = RulesDir::new(state_dir);
            let from = args["from"].as_str();
            match command {
                "show" => show(state_dir, &dir, plow, host, from).await,
                "set" | "edit" => set(&dir, plow, host, command, args, &mut audit).await,
                "clear" => clear(&dir, plow, host, from, &mut audit).await,
                _ => Err(Failure::usage(
                    "bad_command",
                    format!("unknown rules command {command:?}"),
                )),
            }
        }
        Err(failure) => Err(failure),
    };
    crate::vault::host::done(result, audit)
}

/// Only the owner's own devices may use `what` (the rules, a host's
/// audit): an agent's key gets exit 4.
pub(crate) fn owner_only(kind: &str, what: &str) -> Result<(), Failure> {
    if kind == "device" {
        return Ok(());
    }
    Err(Failure::new(
        Exit::Denied,
        "owner_only",
        format!("only the owner's own devices may use {what}"),
    ))
}

/// The agent `from` names, resolved as `--from` is for every rules
/// command: its uid, and its current name in Plow.
pub async fn agent(plow: &Plow, from: &str) -> Result<(String, Option<String>), Failure> {
    let account = Account::read(plow).await?;
    let uid = account.resolve(from, false)?;
    let name = account.name(&uid);
    Ok((uid, name))
}

async fn set(
    dir: &RulesDir,
    plow: &Plow,
    host: &str,
    command: &str,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let text = args["text"]
        .as_str()
        .ok_or_else(|| Failure::usage("bad_arguments", "the rules' text is missing".into()))?;
    let (scope, name, peer) = match args["from"].as_str() {
        None => (Scope::Host, None, None),
        Some(from) => {
            let account = Account::read(plow).await?;
            let uid = account.resolve(from, false)?;
            let name = account.name(&uid);
            let peer = account.peer_key(&uid);
            (Scope::Agent(uid), name, peer)
        }
    };
    note_scope(audit, &scope);
    // An edit saves only over the text it started from.
    let expected = (command == "edit").then(|| args["revision"].as_str());
    let change = dir.set(&scope, text, expected, peer.as_deref())?;
    note_change(audit, &change);
    let mut done = described(host, &scope, name);
    done["revision"] = json!(change.after);
    done["applies"] = json!("applies to the next judgment");
    Ok(done)
}

async fn clear(
    dir: &RulesDir,
    plow: &Plow,
    host: &str,
    from: Option<&str>,
    audit: &mut Map<String, Value>,
) -> Result<Value, Failure> {
    let (scope, name) = match from {
        None => (Scope::Host, None),
        Some(from) => {
            let account = Account::read(plow).await?;
            let uid = account.resolve(from, true)?;
            let name = account.name(&uid);
            (Scope::Agent(uid), name)
        }
    };
    note_scope(audit, &scope);
    let change = dir.clear(&scope)?;
    note_change(audit, &change);
    let mut done = described(host, &scope, name);
    done["cleared"] = json!(change.before.is_some());
    done["applies"] = json!("applies to the next judgment");
    Ok(done)
}

/// The rules for every agent and each agent's own (or only `from`'s),
/// with each agent's current name, a mark on rules whose agent no longer
/// exists, and a note when its latch moved machines since they were set;
/// and the agents `--deny-exec` refuses exec here.
async fn show(
    state_dir: &Path,
    dir: &RulesDir,
    plow: &Plow,
    host: &str,
    from: Option<&str>,
) -> Result<Value, Failure> {
    let mut uids = dir.agents()?;
    let mut agents = Vec::new();
    if from.is_some() || !uids.is_empty() {
        let account = Account::read(plow).await?;
        if let Some(from) = from {
            uids = vec![account.resolve(from, true)?];
        }
        for uid in uids {
            let text = dir.text(&Scope::Agent(uid.clone()))?;
            let mut shown = json!({
                "agent_uid": uid,
                "agent_name": account.name(&uid),
                "text": text,
                "revision": text.as_deref().map(revision),
            });
            if account.name(&uid).is_none() {
                shown["deleted"] = json!(true);
                shown["note"] = json!(
                    "this agent no longer exists in Plow (re-provisioned or deleted): set its rules again with --from its new name, and clear these with --from its old uid"
                );
            } else if text.is_some() {
                match (dir.peer(&uid)?, account.peer_key(&uid)) {
                    (None, _) => shown["note"] = json!("no latch registered when set"),
                    (Some(then), Some(now)) if then != now => {
                        shown["moved"] = json!(true);
                        shown["note"] =
                            json!("this agent's latch has moved machines since its rules were set");
                    }
                    _ => {}
                }
            }
            agents.push(shown);
        }
    }
    let every = dir.text(&Scope::Host)?;
    Ok(json!({
        "host": host,
        "every_agent": every.as_deref().map(|text| json!({"text": text, "revision": revision(text)})),
        "agents": agents,
        "deny_exec": crate::managed::saved_deny_exec(state_dir),
        "warning": WARNING,
    }))
}

/// What a change was to: the host, and which text.
fn described(host: &str, scope: &Scope, name: Option<String>) -> Value {
    match scope {
        Scope::Host => json!({"host": host, "scope": "every agent"}),
        Scope::Agent(uid) => {
            json!({"host": host, "scope": "agent", "agent_uid": uid, "agent_name": name})
        }
    }
}

fn note_scope(audit: &mut Map<String, Value>, scope: &Scope) {
    let (scope, uid) = match scope {
        Scope::Host => ("every agent", None),
        Scope::Agent(uid) => ("agent", Some(uid)),
    };
    audit.insert("scope".into(), json!(scope));
    audit.insert("agent_uid".into(), json!(uid));
}

fn note_change(audit: &mut Map<String, Value>, change: &Change) {
    audit.insert("before".into(), json!(change.before));
    audit.insert("after".into(), json!(change.after));
}

/// The account as `--from` is resolved against: its agents, its peers, and
/// the agent each agent peer belongs to.
struct Account {
    agents: Vec<NamedAgent>,
    peers: Vec<Peer>,
    /// Agent uids by peer key, for the agents' peers.
    agent_of: HashMap<String, String>,
}

impl Account {
    async fn read(plow: &Plow) -> Result<Account, Failure> {
        let agents = plow.agents().await?;
        let peers = plow.list_peers().await?;
        let mut agent_of = HashMap::new();
        for peer in peers.iter().filter(|peer| peer.kind == "agent") {
            if let Some(uid) = plow.lookup(&peer.key).await?.agent_uid {
                agent_of.insert(peer.key.clone(), uid);
            }
        }
        Ok(Account {
            agents,
            peers,
            agent_of,
        })
    }

    /// The agent uid `name` stands for. With `any_uid`, a uid no live agent
    /// has is taken as it is, so rules left by a deleted agent can be
    /// shown and cleared.
    fn resolve(&self, name: &str, any_uid: bool) -> Result<String, Failure> {
        let by_agent = self
            .agents
            .iter()
            .filter(|agent| agent.name == name || agent.uid == name)
            .map(|agent| agent.uid.clone());
        let by_peer = self
            .peers
            .iter()
            .filter(|peer| peer.name == name)
            .filter_map(|peer| self.agent_of.get(&peer.key).cloned());
        let mut uids: Vec<String> = by_agent.chain(by_peer).collect();
        uids.sort();
        uids.dedup();
        match uids.as_slice() {
            [uid] => Ok(uid.clone()),
            [] if self
                .peers
                .iter()
                .any(|peer| peer.name == name && peer.kind == "device") =>
            {
                Err(Failure::usage(
                    "not_an_agent",
                    format!(
                        "{name:?} is a device: `--from` names an agent, and a device is judged on the rules for every agent"
                    ),
                ))
            }
            [] if any_uid && is_agent_uid(name) => Ok(name.to_string()),
            [] => Err(Failure::usage(
                "no_such_agent",
                format!("no agent or agent's latch in this account is called {name:?}"),
            )),
            many => {
                let mut refused = Failure::usage(
                    "ambiguous_agent",
                    format!(
                        "{} agents go by {name:?}: {}; name one by its uid with --from",
                        many.len(),
                        many.join(", ")
                    ),
                );
                refused.details = json!({"agents": many});
                Err(refused)
            }
        }
    }

    /// The agent's current name in Plow; `None` once it no longer exists.
    fn name(&self, uid: &str) -> Option<String> {
        let agent = self.agents.iter().find(|agent| agent.uid == uid)?;
        Some(agent.name.clone())
    }

    /// The key of the latch the agent calls from now, if it has one.
    fn peer_key(&self, uid: &str) -> Option<String> {
        let (key, _) = self.agent_of.iter().find(|(_, agent)| *agent == uid)?;
        Some(key.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::gate::rules::{RuleFiles, Rules, RulesSource};
    use crate::wire::Response;

    const ADA: &str = "0123456789abcdef0123456789abcdef";
    const BOB: &str = "fedcba9876543210fedcba9876543210";
    const ADA_KEY: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

    /// What the synthetic Plow holds: agents by uid and name, and peers as
    /// (key, kind, name, agent uid).
    #[derive(Default)]
    struct World {
        agents: Vec<(String, String)>,
        peers: Vec<(String, String, String, Option<String>)>,
    }

    impl World {
        fn answer(&self, target: &str) -> (u16, Value) {
            let peer = |(key, kind, name, _): &(String, String, String, Option<String>)| {
                json!({"key": key, "kind": kind, "name": name, "relay_url": null,
                       "last_seen_at": null, "online": false})
            };
            if target == "/v1/agents" {
                let agents: Vec<Value> = self
                    .agents
                    .iter()
                    .map(|(uid, name)| json!({"uid": uid, "name": name, "provider": "self"}))
                    .collect();
                return (200, json!(agents));
            }
            if target == "/v1/latch/peers" {
                let peers: Vec<Value> = self.peers.iter().map(peer).collect();
                return (200, json!({"data": peers}));
            }
            let key = target.strip_prefix("/v1/latch/peers/").unwrap_or_default();
            match self.peers.iter().find(|(k, ..)| k == key) {
                Some(found) => {
                    let mut identity = peer(found);
                    identity["agent_uid"] = json!(found.3);
                    identity["permissions"] = json!({"call": true});
                    (200, identity)
                }
                None => (404, json!({})),
            }
        }
    }

    /// The account the tests start from: Ada (also the name of the latch
    /// she calls from, `ada-latch`), Bob, and the owner's mac.
    fn world() -> World {
        World {
            agents: vec![(ADA.into(), "Ada".into()), (BOB.into(), "Bob".into())],
            peers: vec![
                (
                    ADA_KEY.into(),
                    "agent".into(),
                    "ada-latch".into(),
                    Some(ADA.into()),
                ),
                ("d".repeat(64), "device".into(), "mac".into(), None),
            ],
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
        Plow::new(&url, "made-up-login")
    }

    /// A host's state folder, its Plow, and the world behind it.
    struct Host {
        dir: tempfile::TempDir,
        world: Arc<Mutex<World>>,
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
                world,
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

        /// The rules a judgment reads for `agent`.
        fn judged(&self, agent: &str) -> Rules {
            RuleFiles::new(self.dir.path())
                .unwrap()
                .rules(agent)
                .unwrap()
        }
    }

    fn set(from: Option<&str>, text: &str) -> Value {
        json!({"command": "set", "from": from, "text": text})
    }

    fn refusal(result: Result<Value, (Exit, String, String)>) -> (Exit, String) {
        let (exit, code, _) = result.unwrap_err();
        (exit, code)
    }

    #[tokio::test]
    async fn set_replaces_the_text_and_says_what_it_set() {
        let host = Host::new();
        host.owner(set(None, "Never send email.")).await.unwrap();
        let done = host.owner(set(None, "Run tests in ~/work.")).await.unwrap();
        assert_eq!(
            done,
            json!({"host": "studio", "scope": "every agent",
                   "revision": revision("Run tests in ~/work."),
                   "applies": "applies to the next judgment"})
        );
        assert_eq!(
            host.judged(ADA).host.as_deref(),
            Some("Run tests in ~/work.")
        );

        let done = host.owner(set(Some("Ada"), "Only pytest.")).await.unwrap();
        assert_eq!(
            done,
            json!({"host": "studio", "scope": "agent", "agent_uid": ADA, "agent_name": "Ada",
                   "revision": revision("Only pytest."),
                   "applies": "applies to the next judgment"})
        );
        assert_eq!(host.judged(ADA).agent.as_deref(), Some("Only pytest."));
        assert_eq!(host.judged(BOB).agent, None);
    }

    #[tokio::test]
    async fn from_takes_an_agent_name_a_peer_name_or_a_uid_and_never_a_device() {
        let host = Host::new();
        for (from, text) in [("Ada", "one"), ("ada-latch", "two"), (ADA, "three")] {
            let done = host.owner(set(Some(from), text)).await.unwrap();
            assert_eq!(done["agent_uid"], ADA, "{from}");
            assert_eq!(host.judged(ADA).agent.as_deref(), Some(text), "{from}");
        }

        host.world
            .lock()
            .unwrap()
            .agents
            .push(("1".repeat(32), "Ada".into()));
        let (exit, code, message) = host.owner(set(Some("Ada"), "four")).await.unwrap_err();
        assert_eq!((exit, code.as_str()), (Exit::Usage, "ambiguous_agent"));
        assert!(
            message.contains(ADA) && message.contains(&"1".repeat(32)),
            "{message}"
        );
        assert_eq!(host.judged(ADA).agent.as_deref(), Some("three"));

        assert_eq!(
            refusal(host.owner(set(Some("mac"), "five")).await),
            (Exit::Usage, "not_an_agent".into())
        );
        assert_eq!(
            refusal(host.owner(set(Some("Carol"), "six")).await),
            (Exit::Usage, "no_such_agent".into())
        );
        // A uid no agent has is no agent to set rules for.
        assert_eq!(
            refusal(host.owner(set(Some(&"2".repeat(32)), "seven")).await),
            (Exit::Usage, "no_such_agent".into())
        );
    }

    #[tokio::test]
    async fn a_rename_after_set_leaves_the_rules_with_the_same_agent() {
        let host = Host::new();
        host.owner(set(Some("Ada"), "Only pytest.")).await.unwrap();
        host.world.lock().unwrap().agents[0].1 = "Ada Lovelace".into();
        assert_eq!(host.judged(ADA).agent.as_deref(), Some("Only pytest."));
        let shown = host.owner(json!({"command": "show"})).await.unwrap();
        assert_eq!(shown["agents"][0]["agent_uid"], ADA);
        assert_eq!(shown["agents"][0]["agent_name"], "Ada Lovelace");
        assert_eq!(shown["agents"][0]["text"], "Only pytest.");
    }

    #[tokio::test]
    async fn an_edit_saves_only_over_the_revision_it_read() {
        let host = Host::new();
        let edit = |revision: Value, text: &str| json!({"command": "edit", "from": "Ada", "text": text, "revision": revision});
        // Nothing yet: an edit from no text saves.
        host.owner(edit(Value::Null, "one")).await.unwrap();
        host.owner(edit(json!(revision("one")), "two"))
            .await
            .unwrap();
        let (exit, code, message) = host
            .owner(edit(json!(revision("one")), "three"))
            .await
            .unwrap_err();
        assert_eq!((exit, code.as_str()), (Exit::Denied, "stale_revision"));
        assert!(message.contains("current text is kept"), "{message}");
        assert_eq!(host.judged(ADA).agent.as_deref(), Some("two"));
        assert_eq!(
            refusal(host.owner(edit(Value::Null, "four")).await),
            (Exit::Denied, "stale_revision".into())
        );
    }

    #[tokio::test]
    async fn empty_rules_are_refused() {
        let host = Host::new();
        host.owner(set(None, "Never buy anything.")).await.unwrap();
        for text in ["", " \n\t"] {
            assert_eq!(
                refusal(host.owner(set(None, text)).await),
                (Exit::Usage, "empty_rules".into())
            );
        }
        assert_eq!(
            host.judged(ADA).host.as_deref(),
            Some("Never buy anything.")
        );
    }

    #[tokio::test]
    async fn rules_past_8_kib_together_are_refused_and_change_nothing() {
        let host = Host::new();
        let text = |bytes: usize| "r".repeat(bytes);
        host.owner(set(None, &text(3000))).await.unwrap();
        host.owner(set(Some("Ada"), &text(5000))).await.unwrap();
        host.owner(set(Some("Bob"), &text(1000))).await.unwrap();
        let before = host.judged(ADA);

        // Ada's own past the limit with the host's.
        assert_eq!(
            refusal(host.owner(set(Some("Ada"), &text(5193))).await),
            (Exit::Usage, "rules_too_long".into())
        );
        // The host's past it with Ada's, though fine with Bob's.
        assert_eq!(
            refusal(host.owner(set(None, &text(3193))).await),
            (Exit::Usage, "rules_too_long".into())
        );
        assert_eq!(host.judged(ADA), before);
        assert_eq!(host.judged(BOB).agent, Some(text(1000)));

        host.owner(set(None, &text(3192))).await.unwrap();
        host.owner(set(Some("Bob"), &text(5000))).await.unwrap();
    }

    #[tokio::test]
    async fn an_interrupted_write_leaves_the_old_text() {
        let host = Host::new();
        host.owner(set(None, "Never send email.")).await.unwrap();
        let rules = host.dir.path().join("rules");
        // A write stopped before its rename leaves only its temporary file:
        // judgments, and a restarted host, still read the old text.
        std::fs::write(rules.join("host.txt.interrupted.tmp"), "Anyth").unwrap();
        assert_eq!(host.judged(ADA).host.as_deref(), Some("Never send email."));
        // A write that fails partway changes nothing.
        let mode = |mode| {
            std::fs::set_permissions(&rules, std::os::unix::fs::PermissionsExt::from_mode(mode))
                .unwrap()
        };
        mode(0o500);
        assert_eq!(
            refusal(host.owner(set(None, "Anything goes.")).await),
            (Exit::Failed, "rules_io".into())
        );
        mode(0o700);
        assert_eq!(host.judged(ADA).host.as_deref(), Some("Never send email."));
        host.owner(set(None, "Anything goes.")).await.unwrap();
        assert_eq!(host.judged(ADA).host.as_deref(), Some("Anything goes."));
    }

    #[tokio::test]
    async fn clear_removes_only_its_own_text() {
        let host = Host::new();
        host.owner(set(None, "Never send email.")).await.unwrap();
        host.owner(set(Some("Ada"), "Only pytest.")).await.unwrap();
        host.owner(set(Some("Bob"), "Only git.")).await.unwrap();

        let done = host
            .owner(json!({"command": "clear", "from": "Ada"}))
            .await
            .unwrap();
        assert_eq!(done["cleared"], true);
        assert_eq!(host.judged(ADA).agent, None);
        assert_eq!(host.judged(ADA).host.as_deref(), Some("Never send email."));
        assert_eq!(host.judged(BOB).agent.as_deref(), Some("Only git."));
        assert!(!host.dir.path().join(format!("rules/{ADA}.peer")).exists());

        host.owner(json!({"command": "clear"})).await.unwrap();
        assert_eq!(host.judged(BOB).host, None);
        assert_eq!(host.judged(BOB).agent.as_deref(), Some("Only git."));
    }

    #[tokio::test]
    async fn an_agent_gets_owner_only_for_every_command() {
        let host = Host::new();
        host.owner(set(None, "Never send email.")).await.unwrap();
        for args in [
            json!({"command": "show"}),
            set(None, "Anything goes."),
            json!({"command": "edit", "text": "Anything goes.", "revision": revision("Never send email.")}),
            json!({"command": "clear"}),
            json!({"command": "show", "from": "Ada"}),
        ] {
            let done = host.done("agent", args.clone()).await;
            let Response::Error { exit, code, .. } = done.response else {
                panic!("{args}: answered");
            };
            assert_eq!(
                (exit, code.as_str()),
                (Exit::Denied, "owner_only"),
                "{args}"
            );
        }
        assert_eq!(host.judged(ADA).host.as_deref(), Some("Never send email."));
    }

    #[tokio::test]
    async fn show_marks_deleted_agents_and_moved_latches() {
        let host = Host::new();
        host.owner(set(None, "Never send email.")).await.unwrap();
        host.owner(set(Some("Ada"), "Only pytest.")).await.unwrap();
        // Bob has no latch when his rules are set.
        host.owner(set(Some("Bob"), "Only git.")).await.unwrap();
        host.world
            .lock()
            .unwrap()
            .agents
            .push(("3".repeat(32), "Cy".into()));
        host.owner(set(Some("Cy"), "Read only.")).await.unwrap();

        let shown = host.owner(json!({"command": "show"})).await.unwrap();
        assert_eq!(shown["warning"], WARNING);
        assert_eq!(
            shown["every_agent"],
            json!({"text": "Never send email.", "revision": revision("Never send email.")})
        );
        let agent = |uid: &str| {
            shown["agents"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["agent_uid"] == uid)
                .unwrap()
                .clone()
        };
        // Unchanged: nothing to note.
        assert_eq!(
            agent(ADA),
            json!({"agent_uid": ADA, "agent_name": "Ada", "text": "Only pytest.",
                   "revision": revision("Only pytest.")})
        );
        assert_eq!(agent(BOB)["note"], "no latch registered when set");

        {
            let mut world = host.world.lock().unwrap();
            // Ada's latch registers a new key on another machine; Cy is deleted.
            world.peers[0].0 = "b".repeat(64);
            world.agents.retain(|(_, name)| name != "Cy");
        }
        let shown = host.owner(json!({"command": "show"})).await.unwrap();
        let agent = |uid: &str| {
            shown["agents"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["agent_uid"] == uid)
                .unwrap()
                .clone()
        };
        assert_eq!(agent(ADA)["moved"], true);
        assert!(
            agent(ADA)["note"]
                .as_str()
                .unwrap()
                .contains("moved machines")
        );
        let cy = agent(&"3".repeat(32));
        assert_eq!(
            (cy["deleted"].clone(), cy["agent_name"].clone()),
            (json!(true), Value::Null)
        );
        assert_eq!(cy["text"], "Read only.");

        // `--from` shows one agent's beside every agent's, and a deleted
        // agent's rules are shown and cleared by its uid.
        let one = host
            .owner(json!({"command": "show", "from": "3".repeat(32)}))
            .await
            .unwrap();
        assert_eq!(one["agents"].as_array().unwrap().len(), 1);
        assert_eq!(one["every_agent"]["text"], "Never send email.");
        host.owner(json!({"command": "clear", "from": "3".repeat(32)}))
            .await
            .unwrap();
        assert_eq!(host.judged(&"3".repeat(32)).agent, None);
    }

    #[tokio::test]
    async fn every_change_is_audited_by_revision_never_by_text() {
        let host = Host::new();
        let first = host.done("device", set(Some("Ada"), "Only pytest.")).await;
        assert_eq!(
            first.audit_args,
            json!({"command": "set", "scope": "agent", "agent_uid": ADA,
                   "before": null, "after": revision("Only pytest.")})
        );
        let edit = json!({"command": "edit", "from": ADA, "text": "Only git.",
                          "revision": revision("Only pytest.")});
        assert_eq!(
            host.done("device", edit).await.audit_args,
            json!({"command": "edit", "scope": "agent", "agent_uid": ADA,
                   "before": revision("Only pytest."), "after": revision("Only git.")})
        );
        let cleared = host.done("device", json!({"command": "clear"})).await;
        assert_eq!(
            cleared.audit_args,
            json!({"command": "clear", "scope": "every agent", "agent_uid": null,
                   "before": null, "after": null})
        );
        let refused = host
            .done("device", set(Some("Ada"), &"r".repeat(9000)))
            .await;
        assert_eq!(refused.outcome, "rules_too_long");
        assert!(!refused.audit_args.to_string().contains("rrrr"));
    }
}
