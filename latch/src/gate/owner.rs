//! The `rules` operation: a host's side of the owner's commands on the
//! account's rules, from the owner's own devices only (an agent's key gets
//! exit 4 `owner_only`, for every command, `list` included). `list` reads
//! this host's whole list; `repair`, `add`, `edit` and `remove` merge rules
//! into it, the newer version of each winning, removals included. `latch
//! rules` asks every host it can reach, and works on this machine's own
//! list itself. The audit gets the command, and the ids that changed with
//! their versions before and after; never text.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value, json};

use super::rules::{Rule, RulesDir};
use crate::failure::{Failure, code};
use crate::plow::{NamedAgent, Peer, Plow};
use crate::serve::Done;

/// The commands that merge rules into a host's list, as they are audited:
/// a repair brings a host up to date; the rest are the owner's changes.
pub const MERGES: [&str; 4] = ["repair", "add", "edit", "remove"];

/// `rules`: one owner command on the list in `state_dir`, for the host
/// whose key is `host_key`. `kind` is the caller's kind as Plow says.
pub async fn run(state_dir: &Path, host_key: &str, kind: &str, args: &Value) -> Done {
    let command = args["command"].as_str().unwrap_or_default();
    let mut audit = Map::new();
    audit.insert("command".into(), json!(command));
    let result = match owner_only(kind, "the rules") {
        Ok(()) => {
            let dir = RulesDir::new(state_dir, host_key);
            match command {
                "list" => dir.list().map(|rules| {
                    audit.insert("rules".into(), json!(rules.len()));
                    json!({ "rules": rules })
                }),
                command if MERGES.contains(&command) => {
                    merge_into(&dir, args, &mut audit).map(|rules| json!({ "rules": rules }))
                }
                _ => Err(Failure::new(
                    code::bad_command,
                    format!("unknown rules command {command:?}"),
                )),
            }
        }
        Err(failure) => Err(failure),
    };
    crate::vault::host::done(result, audit)
}

/// Merges the rules `args` carries into `dir`'s list. Returns the list.
fn merge_into(
    dir: &RulesDir,
    args: &Value,
    audit: &mut Map<String, Value>,
) -> Result<Vec<Rule>, Failure> {
    let incoming: Vec<Rule> = serde_json::from_value(args["rules"].clone())
        .map_err(|err| Failure::new(code::bad_arguments, format!("the rules to merge: {err}")))?;
    let mut before = Vec::new();
    let changed = dir.change(|held| {
        before = held.to_vec();
        Ok(incoming)
    })?;
    let after = dir.list()?;
    audit.insert("changed".into(), changes(&before, &after, &changed));
    Ok(after)
}

/// What changed, as the audit keeps it: each id with its version's
/// fingerprint before (`null`: none) and after, and whether it is removed.
pub fn changes(before: &[Rule], after: &[Rule], ids: &[String]) -> Value {
    let version = |rules: &[Rule], id: &str| rules.iter().find(|rule| rule.id == id).cloned();
    let changes: Vec<Value> = ids
        .iter()
        .map(|id| {
            let after = version(after, id);
            json!({
                "id": id,
                "before": version(before, id).map(|rule| rule.fingerprint()),
                "after": after.as_ref().map(Rule::fingerprint),
                "removed": after.is_some_and(|rule| rule.deleted),
            })
        })
        .collect();
    json!(changes)
}

/// Only the owner's own devices may use `what` (the rules, a host's
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

/// The agent `from` names, resolved as `--agent` is for `rules try`: its
/// uid, and its current name in Plow.
pub async fn agent(plow: &Plow, from: &str) -> Result<(String, Option<String>), Failure> {
    let account = Account::read(plow).await?;
    let uid = account.resolve(from)?;
    let name = account.name(&uid);
    Ok((uid, name))
}

/// The account as `--agent` is resolved against: its agents, its peers, and
/// the agent each agent peer belongs to.
pub(crate) struct Account {
    pub(crate) agents: Vec<NamedAgent>,
    pub(crate) peers: Vec<Peer>,
    /// Agent uids by peer key, for the agents' peers.
    agent_of: HashMap<String, String>,
}

impl Account {
    pub(crate) async fn read(plow: &Plow) -> Result<Account, Failure> {
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

    /// The agent uid `name` stands for.
    pub(crate) fn resolve(&self, name: &str) -> Result<String, Failure> {
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
                Err(Failure::new(
                    code::not_an_agent,
                    format!(
                        "{name:?} is a device: `--agent` names an agent, and a device is judged on the rules for every agent"
                    ),
                ))
            }
            [] => Err(Failure::new(
                code::no_such_agent,
                format!("no agent or agent's latch in this account is called {name:?}"),
            )),
            many => {
                let mut refused = Failure::new(
                    code::ambiguous_agent,
                    format!(
                        "{} agents go by {name:?}: {}; name one by its uid with --agent",
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
    pub(crate) fn name(&self, uid: &str) -> Option<String> {
        let agent = self.agents.iter().find(|agent| agent.uid == uid)?;
        Some(agent.name.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::Exit;
    use crate::gate::rules::Covers;
    use crate::wire::Response;

    const KEY: &str = "studio-key";

    async fn run_as(dir: &Path, kind: &str, args: Value) -> Done {
        run(dir, KEY, kind, &args).await
    }

    fn answer(done: &Done) -> Value {
        match &done.response {
            Response::Ok(value) => value.clone(),
            other => panic!("{other:?}"),
        }
    }

    fn refusal(done: &Done) -> (Exit, String) {
        match &done.response {
            Response::Error { exit, code, .. } => (*exit, code.clone()),
            other => panic!("answered: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_host_merges_what_it_is_sent_and_lists_its_whole_list() {
        let dir = tempfile::tempdir().unwrap();
        let first = Rule::new("Never send email.", Covers::All, Covers::All);
        let added = run_as(
            dir.path(),
            "device",
            json!({"command": "add", "rules": [first]}),
        )
        .await;
        assert_eq!(answer(&added)["rules"], json!([first]));
        let removed = first.removed();
        let repair = json!({"command": "repair", "rules": [removed]});
        run_as(dir.path(), "device", repair).await;

        // A stale copy changes nothing: the removal stays, text and all gone.
        let stale = run_as(
            dir.path(),
            "device",
            json!({"command": "edit", "rules": [first]}),
        )
        .await;
        assert_eq!(answer(&stale)["rules"], json!([removed]));
        let listed = run_as(dir.path(), "device", json!({"command": "list"})).await;
        assert_eq!(answer(&listed)["rules"], json!([removed]));
    }

    #[tokio::test]
    async fn every_merge_is_audited_by_version_never_by_text() {
        let dir = tempfile::tempdir().unwrap();
        let rule = Rule::new("Only pytest.", Covers::All, Covers::All);
        let added = run_as(
            dir.path(),
            "device",
            json!({"command": "add", "rules": [rule]}),
        )
        .await;
        assert_eq!(
            added.audit_args,
            json!({"command": "add", "changed": [
                {"id": rule.id, "before": null, "after": rule.fingerprint(), "removed": false}]})
        );
        let removed = rule.removed();
        let done = run_as(
            dir.path(),
            "device",
            json!({"command": "remove", "rules": [removed]}),
        )
        .await;
        assert_eq!(
            done.audit_args,
            json!({"command": "remove", "changed": [
                {"id": rule.id, "before": rule.fingerprint(), "after": removed.fingerprint(),
                 "removed": true}]})
        );
        let listed = run_as(dir.path(), "device", json!({"command": "list"})).await;
        assert_eq!(listed.audit_args, json!({"command": "list", "rules": 1}));
        assert!(!added.audit_args.to_string().contains("pytest"));
    }

    #[tokio::test]
    async fn an_agent_gets_owner_only_for_every_command() {
        let dir = tempfile::tempdir().unwrap();
        let rule = Rule::new("Anything goes.", Covers::All, Covers::All);
        for command in ["list", "repair", "add", "edit", "remove"] {
            let done = run_as(
                dir.path(),
                "agent",
                json!({"command": command, "rules": [rule]}),
            )
            .await;
            assert_eq!(
                refusal(&done),
                (Exit::Denied, "owner_only".into()),
                "{command}"
            );
        }
        assert!(RulesDir::new(dir.path(), KEY).list().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_old_commands_and_unreadable_rules_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        for command in ["show", "set", "clear", "clear_at_revision"] {
            let done = run_as(
                dir.path(),
                "device",
                json!({"command": command, "text": "x"}),
            )
            .await;
            assert_eq!(
                refusal(&done),
                (Exit::Usage, "bad_command".into()),
                "{command}"
            );
        }
        let bad = json!({"command": "repair", "rules": [{"id": "r1"}]});
        let done = run_as(dir.path(), "device", bad).await;
        assert_eq!(refusal(&done), (Exit::Usage, "bad_arguments".into()));
    }
}
