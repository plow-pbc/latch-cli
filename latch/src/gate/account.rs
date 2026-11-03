//! `latch rules list|add|edit|remove`: the owner's commands on the account's
//! rules. Every host keeps the whole list (`rules.rs`), and so does this
//! machine, whether or not it serves. A host running on an agent's login is
//! no copy: an agent can neither read nor write the owner's rules, so it is
//! never asked for its list or sent one. A command reads this machine's copy
//! and every host's it can reach at once, merges them by id (the newest
//! change of each rule winning, removals included), and brings any copy that
//! is behind up to date. A change is saved in this machine's copy first,
//! then on every host it can reach; a host that can't be reached is reported,
//! never a reason to undo it. Each host says how its copy stood before
//! (`observed`) and after (`delivery`): current, behind (it answered, and
//! lacks the newest list) or unknown (it couldn't be asked, or its answer
//! was lost).
//!
//! Agents and hosts are named by stable id. Those the account no longer has,
//! and hosts on an agent's login, are left out of what a rule shows, and a
//! rule none of whose agents, or none of whose hosts, are left is not shown
//! at all; nothing is removed for it.

use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use super::owner::{self, Account};
use super::rules::{Covers, MAX_RULES, Rule, RulesDir, budget, merge, overgrown, render};
use crate::audit::{self, AuditLog};
use crate::failure::{Failure, code};
use crate::hosts;
use crate::plow::{Peer, Plow};
use crate::state::{Config, StateDir};
use crate::wire::Request;

/// How long asking every host may take, all at once.
const WITHIN: Duration = Duration::from_secs(5);

/// The codes of a host's answer that never came, or was lost on the way:
/// whether a change was applied there is unknown.
const UNANSWERED: [&str; 8] = [
    "host_offline",
    "host_unreachable",
    "protocol_mismatch",
    "iroh",
    "outcome_unknown",
    "no_answer",
    "bad_response",
    "caller_denied",
];

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Delivery {
    /// It holds the newest list.
    Current,
    /// It answered, and lacks the newest list.
    Behind,
    /// It couldn't be asked, or its answer was lost.
    Unknown,
}

/// One copy of the list: this machine's own, or a host's.
struct Copy {
    peer: Peer,
    /// The list as it was last read there.
    held: Option<Vec<Rule>>,
    reason: Option<String>,
    observed: Delivery,
    delivery: Delivery,
}

/// Which rules `list` shows: those that apply to an agent, on a host.
#[derive(Default)]
struct Filter {
    agent: Option<String>,
    host: Option<String>,
}

/// What a change does to one rule, once the merged list is known.
type Change<'a> = Box<dyn FnOnce(&[Rule]) -> Result<Option<Rule>, Failure> + 'a>;

/// One command's reading of the account's rules.
struct Session {
    state: StateDir,
    config: Config,
    account: Account,
    store: RulesDir,
    log: AuditLog,
    /// This machine's copy first, then each host's.
    copies: Vec<Copy>,
    merged: Vec<Rule>,
}

pub async fn list(
    state: &StateDir,
    agent: Option<String>,
    host: Option<String>,
) -> Result<Value, Failure> {
    let mut session = Session::open(state).await?;
    let filter = Filter {
        agent: agent
            .map(|name| session.account.resolve(&name))
            .transpose()?,
        host: host
            .map(|name| session.host_key(&name, false))
            .transpose()?,
    };
    session.read().await?;
    session.repair("repair").await?;
    Ok(session.snapshot(&filter))
}

pub async fn add(
    state: &StateDir,
    text: String,
    agents: Option<String>,
    hosts: Option<String>,
    if_empty: bool,
) -> Result<Value, Failure> {
    if text.trim().is_empty() {
        return Err(empty_text("Enter the rule text."));
    }
    let mut session = Session::open(state).await?;
    let agents = session.agents(agents.as_deref(), None)?;
    let hosts = session.hosts(hosts.as_deref(), None)?;
    session.read().await?;
    let rule = Rule::new(&text, agents, hosts);
    let here = session.config.peer.key.clone();
    let guarded = move |now: &[Rule]| if_empty && render(now, &here, "").host.is_some();
    let change: Change = Box::new(|now| match guarded(now) {
        true => Ok(None),
        false => Ok(Some(rule)),
    });
    let changed = session.commit("add", change).await?;
    let mut done = session.snapshot(&Filter::default());
    done["change"] = match changed {
        Some(rule) => session.change("add", &rule),
        None => json!({"kind": "add", "id": null, "saved": false, "changed": false,
                       "reason": "rules_exist", "rule": null}),
    };
    Ok(done)
}

pub async fn edit(
    state: &StateDir,
    id: String,
    text: Option<String>,
    agents: Option<String>,
    hosts: Option<String>,
) -> Result<Value, Failure> {
    if text.as_deref().is_some_and(|text| text.trim().is_empty()) {
        return Err(empty_text(&format!("latch rules remove {id}")));
    }
    if text.is_none() && agents.is_none() && hosts.is_none() {
        let message = "say what to change: a new TEXT, --file, --agents or --hosts";
        return Err(Failure::new(code::bad_arguments, message.into()));
    }
    let mut session = Session::open(state).await?;
    session.read().await?;
    let unchecked = session.unchecked();
    let current = edited_from(&session.merged, &id, &unchecked)?;
    // Only what is given replaces; the rest stays as it is under the lock.
    let agents = agents
        .map(|given| session.agents(Some(&given), Some(&current.agents)))
        .transpose()?;
    let hosts = hosts
        .map(|given| session.hosts(Some(&given), Some(&current.hosts)))
        .transpose()?;
    let change: Change = Box::new(move |now| {
        let current = edited_from(now, &id, &unchecked)?;
        Ok(edit_of(&current, text.as_deref(), agents, hosts))
    });
    let changed = session.commit("edit", change).await?;
    let mut done = session.snapshot(&Filter::default());
    done["change"] = match changed {
        Some(rule) => session.change("edit", &rule),
        None => session.unchanged("edit", &current),
    };
    Ok(done)
}

pub async fn remove(state: &StateDir, id: String) -> Result<Value, Failure> {
    let mut session = Session::open(state).await?;
    session.read().await?;
    let check = session.unchecked();
    let known = session.merged.iter().find(|rule| rule.id == id).cloned();
    let Some(known) = known else {
        return Err(no_such_rule(&id, false, &check));
    };
    let change: Change = Box::new(move |now| {
        match now.iter().find(|rule| rule.id == id) {
            // Removed already: saved, and nothing changes.
            Some(rule) if rule.deleted => Ok(None),
            Some(rule) => Ok(Some(rule.removed())),
            None => Err(no_such_rule(&id, false, &check)),
        }
    });
    let changed = session.commit("remove", change).await?;
    let mut done = session.snapshot(&Filter::default());
    done["change"] = match changed {
        Some(rule) => {
            let mut change = session.change("remove", &rule);
            change["rule"] = Value::Null;
            change
        }
        None => {
            let mut change = session.unchanged("remove", &known);
            change["rule"] = Value::Null;
            change
        }
    };
    Ok(done)
}

impl Session {
    /// This machine's login and the account's directory: the owner's own
    /// device only, and a directory that couldn't be read is an error, never
    /// an empty account.
    async fn open(state: &StateDir) -> Result<Session, Failure> {
        let config = state.require_login()?;
        if config.peer.kind != "device" {
            return Err(Failure::new(
                code::owner_only,
                "Only the owner's own devices can use latch rules.".into(),
            ));
        }
        // A log that can't be written to stops the command before it changes anything.
        let log = AuditLog::open(state.path.join(audit::FILE))?;
        let account = Account::read(&Plow::from_state(state)?).await?;
        let listed: Vec<Peer> = hosts::only_hosts(account.peers.clone(), &config.peer.key)
            .into_iter()
            .filter(|host| host.kind == "device")
            .collect();
        let this_machine = listed
            .iter()
            .find(|host| host.this_machine)
            .cloned()
            .unwrap_or_else(|| Peer {
                online: None,
                this_machine: true,
                ..config.peer.clone()
            });
        let copy = |peer: Peer| Copy {
            peer,
            held: None,
            reason: None,
            observed: Delivery::Unknown,
            delivery: Delivery::Unknown,
        };
        let mut copies = vec![copy(this_machine)];
        copies.extend(
            listed
                .into_iter()
                .filter(|host| !host.this_machine)
                .map(copy),
        );
        Ok(Session {
            state: StateDir {
                path: state.path.clone(),
            },
            store: RulesDir::new(&state.path, &config.peer.key),
            config,
            account,
            log,
            copies,
            merged: Vec::new(),
        })
    }

    /// Reads every copy it can, and merges them.
    async fn read(&mut self) -> Result<(), Failure> {
        let mine = self.store.list()?;
        self.audit(json!({"command": "list", "rules": mine.len()}))?;
        self.copies[0].held = Some(mine);
        let request = rules_request(json!({"command": "list"}));
        for (index, answer) in self.ask(&request, |_| true).await {
            let copy = &mut self.copies[index];
            match answer.and_then(held) {
                Ok(rules) => copy.held = Some(rules),
                Err(failure) => copy.reason = Some(reason(&failure)),
            }
        }
        for copy in &self.copies {
            if let Some(rules) = &copy.held {
                merge(&mut self.merged, rules);
            }
        }
        for copy in &mut self.copies {
            copy.observed = match &copy.held {
                Some(rules) if same(rules, &self.merged) => Delivery::Current,
                Some(_) => Delivery::Behind,
                None => Delivery::Unknown,
            };
            copy.delivery = copy.observed;
        }
        Ok(())
    }

    /// Brings every copy that is behind up to the merged list, as
    /// `command`: this machine's here, each host's there.
    async fn repair(&mut self, command: &str) -> Result<(), Failure> {
        if self.copies[0].delivery == Delivery::Behind {
            let merged = self.merged.clone();
            self.save(command, |_| Ok(merged))?;
            self.copies[0].held = Some(self.merged.clone());
            self.copies[0].delivery = Delivery::Current;
        }
        let request = rules_request(json!({"command": command, "rules": self.merged}));
        let answers = self
            .ask(&request, |copy| copy.delivery == Delivery::Behind)
            .await;
        for (index, answer) in answers {
            let copy = &mut self.copies[index];
            match answer.and_then(held) {
                Ok(rules) => {
                    copy.delivery = match same(&rules, &self.merged) {
                        true => Delivery::Current,
                        false => Delivery::Behind,
                    };
                    copy.held = Some(rules);
                    copy.reason = None;
                }
                Err(failure) if UNANSWERED.contains(&failure.code.as_str()) => {
                    copy.delivery = Delivery::Unknown;
                    copy.reason = Some(reason(&failure));
                }
                Err(failure) => {
                    copy.delivery = Delivery::Behind;
                    copy.reason = Some(reason(&failure));
                }
            }
        }
        Ok(())
    }

    /// Makes `change` in this machine's copy, under its lock, against the
    /// merged list and anything saved here meanwhile; refused where it would
    /// push a host's rules over the budget. Then every host gets the new
    /// list. Returns the rule as saved, if anything changed.
    async fn commit(&mut self, kind: &str, change: Change<'_>) -> Result<Option<Rule>, Failure> {
        let mut changed = None;
        let merged = self.merged.clone();
        let keys: Vec<String> = self
            .copies
            .iter()
            .map(|copy| copy.peer.key.clone())
            .collect();
        let names = self.names();
        self.save(kind, |held| {
            let mut now = merged;
            merge(&mut now, held);
            let rule = change(&now)?;
            let mut after = now.clone();
            if let Some(rule) = &rule {
                merge(&mut after, std::slice::from_ref(rule));
                let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
                refuse_overgrown(&now, &after, &keys, &names)?;
            }
            changed = rule;
            Ok(after)
        })?;
        self.merged = self.store.list()?;
        let command = match changed {
            Some(_) => kind,
            None => "repair",
        };
        self.copies[0].held = Some(self.merged.clone());
        self.copies[0].delivery = Delivery::Current;
        for copy in &mut self.copies[1..] {
            if let Some(rules) = &copy.held {
                copy.delivery = match same(rules, &self.merged) {
                    true => Delivery::Current,
                    false => Delivery::Behind,
                };
            }
        }
        self.repair(command).await?;
        Ok(changed)
    }

    /// Saves in this machine's copy what `rules` says to merge into it,
    /// audited as `command`.
    fn save(
        &mut self,
        command: &str,
        rules: impl FnOnce(&[Rule]) -> Result<Vec<Rule>, Failure>,
    ) -> Result<(), Failure> {
        let mut before = Vec::new();
        let ids = self.store.change(|held| {
            before = held.to_vec();
            rules(held)
        })?;
        if ids.is_empty() {
            return Ok(());
        }
        let after = self.store.list()?;
        self.audit(json!({"command": command, "changed": owner::changes(&before, &after, &ids)}))
    }

    /// `request` sent to each host `which` picks, all at once.
    async fn ask(
        &self,
        request: &Request,
        which: impl Fn(&Copy) -> bool,
    ) -> Vec<(usize, Result<Value, Failure>)> {
        let picked: Vec<usize> = (1..self.copies.len())
            .filter(|&index| which(&self.copies[index]))
            .collect();
        let peers: Vec<Peer> = picked
            .iter()
            .map(|&index| self.copies[index].peer.clone())
            .collect();
        let mut answers = Vec::new();
        crate::dial::ask_each(&self.state, &peers, request, WITHIN, |at, answer| {
            answers.push((picked[at], answer));
        })
        .await;
        answers
    }

    fn audit(&mut self, args: Value) -> Result<(), Failure> {
        let mut record = audit::record(&self.config.peer.key, None, "rules", "allow", "ok");
        record.caller_name = Some(self.config.peer.name.clone());
        record.args = args;
        self.log.append(record)?;
        Ok(())
    }

    /// The hosts whose copies couldn't be read, by name.
    fn unchecked(&self) -> Vec<String> {
        self.copies
            .iter()
            .filter(|copy| copy.held.is_none())
            .map(|copy| copy.peer.name.clone())
            .collect()
    }

    /// Each host's name, by key.
    fn names(&self) -> Vec<(String, String)> {
        let mut names: Vec<(String, String)> = self
            .account
            .peers
            .iter()
            .map(|peer| (peer.key.clone(), peer.name.clone()))
            .collect();
        names.extend(
            self.account
                .agents
                .iter()
                .map(|agent| (agent.uid.clone(), agent.name.clone())),
        );
        names
    }

    /// `--agents`: all, or each name resolved to its uid. An edit keeps any
    /// uid already on the rule, even of an agent the account no longer has.
    fn agents(&self, given: Option<&str>, current: Option<&Covers>) -> Result<Covers, Failure> {
        self.covers(given, current, "agents", |name| {
            self.account.resolve(name).map_err(|mut failure| {
                if failure.code == code::no_such_agent.name {
                    failure.message = format!(
                        "Your change wasn't saved because no agent in your account is called {name:?}."
                    );
                    failure.next = Some("latch agents".into());
                }
                failure
            })
        })
    }

    /// `--hosts`: all, or each host's name or key resolved to its key.
    fn hosts(&self, given: Option<&str>, current: Option<&Covers>) -> Result<Covers, Failure> {
        self.covers(given, current, "hosts", |name| self.host_key(name, true))
    }

    fn covers(
        &self,
        given: Option<&str>,
        current: Option<&Covers>,
        what: &str,
        resolve: impl Fn(&str) -> Result<String, Failure>,
    ) -> Result<Covers, Failure> {
        let Some(given) = given else {
            return Ok(current.cloned().unwrap_or(Covers::All));
        };
        if given == "all" {
            return Ok(Covers::All);
        }
        let mut ids = Vec::new();
        for name in given
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            let id = match current {
                Some(current) if *current != Covers::All && current.includes(name) => {
                    name.to_string()
                }
                _ => resolve(name)?,
            };
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if ids.is_empty() {
            let message = format!("--{what} names none: give `all`, or names separated by commas");
            return Err(Failure::new(code::bad_arguments, message));
        }
        Ok(Covers::Only(ids))
    }

    /// The key of the host `name` names: its name or its key.
    fn host_key(&self, name: &str, saving: bool) -> Result<String, Failure> {
        let found = self
            .copies
            .iter()
            .map(|copy| &copy.peer)
            .filter(|peer| peer.relay_url.is_some() || peer.this_machine)
            .find(|peer| peer.name == name || peer.key == name);
        if let Some(peer) = found {
            return Ok(peer.key.clone());
        }
        let agents_login = self.account.peers.iter().find(|peer| {
            peer.kind != "device"
                && peer.relay_url.is_some()
                && (peer.name == name || peer.key == name)
        });
        if let Some(peer) = agents_login {
            let synced = format!("synced to {}: it runs on an agent's login.", peer.name);
            let message = match saving {
                true => format!("Your change wasn't saved because rules aren't {synced}"),
                false => format!("Rules aren't {synced}"),
            };
            let mut failure = Failure::new(code::unknown_host, message);
            failure.next = Some("latch rules list".into());
            return Err(failure);
        }
        let mut failure = match saving {
            true => Failure::new(
                code::unknown_host,
                format!(
                    "Your change wasn't saved because no host in your account is called {name:?}."
                ),
            ),
            false => Failure::new(
                code::unknown_host,
                format!("no host in your account is called {name:?}"),
            ),
        };
        failure.next = Some("latch status".into());
        Err(failure)
    }

    /// The rules and every copy's standing, as the commands answer.
    fn snapshot(&self, filter: &Filter) -> Value {
        let mut live: Vec<&Rule> = self.merged.iter().filter(|rule| !rule.deleted).collect();
        live.sort_by(|a, b| (a.updated, &a.id).cmp(&(b.updated, &b.id)));
        let rules: Vec<Value> = live
            .into_iter()
            .filter(|rule| {
                filter
                    .agent
                    .as_ref()
                    .is_none_or(|uid| rule.agents.includes(uid))
                    && filter
                        .host
                        .as_ref()
                        .is_none_or(|key| rule.hosts.includes(key))
            })
            .filter_map(|rule| self.shown(rule))
            .collect();
        let hosts: Vec<Value> = self
            .copies
            .iter()
            .map(|copy| {
                json!({
                    "key": copy.peer.key,
                    "name": copy.peer.name,
                    "self": copy.peer.this_machine,
                    "online": copy.peer.online,
                    "observed": copy.observed,
                    "delivery": copy.delivery,
                    "reason": copy.reason,
                    "budget": budget(&self.merged, &copy.peer.key),
                })
            })
            .collect();
        json!({"rules": rules, "hosts": hosts})
    }

    /// `rule` as the commands show it: its agents and the hosts the account
    /// still has that rules reach, and the hosts it applies on that are
    /// confirmed behind.
    /// `None` when none of its agents, or none of its hosts, are left.
    fn shown(&self, rule: &Rule) -> Option<Value> {
        let agent = |uid: &str| self.account.agents.iter().any(|agent| agent.uid == uid);
        let host = |key: &str| {
            key == self.config.peer.key
                || (self.account.peers.iter()).any(|peer| peer.key == key && peer.kind == "device")
        };
        let behind: Vec<&str> = self
            .copies
            .iter()
            .filter(|copy| copy.delivery == Delivery::Behind && rule.hosts.includes(&copy.peer.key))
            .filter(|copy| {
                let held = copy.held.iter().flatten().find(|held| held.id == rule.id);
                held.map(Rule::fingerprint) != Some(rule.fingerprint())
            })
            .map(|copy| copy.peer.key.as_str())
            .collect();
        Some(json!({
            "id": rule.id,
            "text": rule.text,
            "agents": shown(&rule.agents, "uid", agent)?,
            "hosts": shown(&rule.hosts, "key", host)?,
            "behind_hosts": behind,
        }))
    }

    /// The change made, as the commands answer it.
    fn change(&self, kind: &str, rule: &Rule) -> Value {
        json!({"kind": kind, "id": rule.id, "saved": true, "changed": true, "reason": null,
               "rule": self.shown(rule)})
    }

    /// A change that was saved already, so nothing changed.
    fn unchanged(&self, kind: &str, rule: &Rule) -> Value {
        json!({"kind": kind, "id": rule.id, "saved": true, "changed": false, "reason": null,
               "rule": self.shown(rule)})
    }
}

/// A scope as the commands show it: `"all"`, or the ids the account still
/// has, each as `{field: id}`; `None` when none are left.
fn shown(covers: &Covers, field: &str, known: impl Fn(&str) -> bool) -> Option<Value> {
    match covers {
        Covers::All => Some(json!("all")),
        Covers::Only(ids) => {
            let left: Vec<Value> = ids
                .iter()
                .filter(|id| known(id))
                .map(|id| json!({ field: id }))
                .collect();
            (!left.is_empty()).then(|| json!(left))
        }
    }
}

fn rules_request(args: Value) -> Request {
    Request {
        op: "rules".to_string(),
        args,
        chat: None,
    }
}

/// The list a host's answer holds.
fn held(answer: Value) -> Result<Vec<Rule>, Failure> {
    serde_json::from_value(answer["rules"].clone()).map_err(|err| {
        Failure::new(
            code::bad_host_answer,
            format!("the host's rules answer has no readable list: {err}"),
        )
    })
}

/// Why a host's copy couldn't be read or written: a host whose latch knows
/// no such command can't sync these rules until it is updated.
fn reason(failure: &Failure) -> String {
    match failure.code.as_str() {
        "bad_command" => code::host_outdated.name.to_string(),
        code => code.to_string(),
    }
}

/// Whether two copies hold the same version of every rule.
fn same(a: &[Rule], b: &[Rule]) -> bool {
    let versions = |rules: &[Rule]| {
        let mut versions: Vec<(String, String)> = rules
            .iter()
            .map(|rule| (rule.id.clone(), rule.fingerprint()))
            .collect();
        versions.sort();
        versions
    };
    versions(a) == versions(b)
}

/// The rule `id` in `rules`, to edit; refused as removed when it is, or
/// as not found among the copies checked (`unchecked` names those that
/// weren't).
fn edited_from(rules: &[Rule], id: &str, unchecked: &[String]) -> Result<Rule, Failure> {
    match rules.iter().find(|rule| rule.id == id) {
        Some(rule) if rule.deleted => Err(no_such_rule(id, true, unchecked)),
        Some(rule) => Ok(rule.clone()),
        None => Err(no_such_rule(id, false, unchecked)),
    }
}

/// `current` with what an edit gives: a new text, agents or hosts, each
/// replacing, the rest kept as `current` has it. `None` when that changes
/// nothing.
fn edit_of(
    current: &Rule,
    text: Option<&str>,
    agents: Option<Covers>,
    hosts: Option<Covers>,
) -> Option<Rule> {
    let edited = current.edited(
        text.unwrap_or(&current.text),
        agents.unwrap_or_else(|| current.agents.clone()),
        hosts.unwrap_or_else(|| current.hosts.clone()),
    );
    let same = (&edited.text, &edited.agents, &edited.hosts)
        == (&current.text, &current.agents, &current.hosts);
    (!same).then_some(edited)
}

fn no_such_rule(id: &str, removed: bool, unchecked: &[String]) -> Failure {
    let message = match removed {
        true => "This rule was deleted, so your change wasn't saved.",
        false => "This rule could not be found among the copies checked.",
    };
    let mut failure = Failure::new(code::no_such_rule, message.into());
    failure.next = Some("latch rules list".into());
    failure.details = json!({"id": id, "removed": removed, "unchecked_hosts": unchecked});
    failure
}

fn empty_text(next: &str) -> Failure {
    let mut failure = Failure::new(
        code::empty_rules,
        "A rule needs text, so your change wasn't saved.".into(),
    );
    failure.next = Some(next.into());
    failure
}

/// Refuses going from `before` to `after` where it would push an agent's
/// rules on any of `hosts` over the budget. `names` names agents and hosts
/// by id.
fn refuse_overgrown(
    before: &[Rule],
    after: &[Rule],
    hosts: &[&str],
    names: &[(String, String)],
) -> Result<(), Failure> {
    let found = overgrown(before, after, hosts);
    let Some(first) = found.first() else {
        return Ok(());
    };
    let name = |id: &str| {
        names
            .iter()
            .find(|(named, _)| named == id)
            .map_or(id.to_string(), |(_, name)| name.clone())
    };
    let host = name(&first.key);
    let (whose, next) = match &first.agent {
        Some(uid) => (
            format!("the rules for {} on {host}", name(uid)),
            format!("latch rules list --host {host} --agent {}", name(uid)),
        ),
        None => (
            format!("the rules on {host}"),
            format!("latch rules list --host {host}"),
        ),
    };
    let mut refused = Failure::new(
        code::rules_too_long,
        format!("Your change wasn't saved because {whose} would exceed {MAX_RULES} bytes."),
    );
    refused.next = Some(next.into());
    let hosts: Vec<Value> = found
        .iter()
        .map(|over| {
            json!({"key": over.key, "name": name(&over.key), "agent": over.agent,
                   "before_bytes": over.before_bytes, "after_bytes": over.after_bytes})
        })
        .collect();
    refused.details = json!({"limit_bytes": MAX_RULES, "hosts": hosts});
    Err(refused)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scope_shows_only_the_ids_the_account_still_has() {
        let known = |id: &str| id != "gone";
        assert_eq!(shown(&Covers::All, "uid", known), Some(json!("all")));
        let some = Covers::Only(vec!["ada".into(), "gone".into()]);
        assert_eq!(shown(&some, "uid", known), Some(json!([{"uid": "ada"}])));
        // None left: the rule isn't shown, though its ids are kept.
        let none = Covers::Only(vec!["gone".into()]);
        assert_eq!(shown(&none, "key", known), None);
    }

    #[test]
    fn an_edit_keeps_what_it_doesnt_give_as_it_is_now() {
        let first = Rule::new("one", Covers::All, Covers::All);
        // Meanwhile, another device narrowed its agents.
        let narrowed = first.edited("one", Covers::Only(vec!["ada".into()]), Covers::All);
        let text_only = edit_of(&narrowed, Some("two"), None, None).unwrap();
        assert_eq!(
            (text_only.text.as_str(), &text_only.agents),
            ("two", &Covers::Only(vec!["ada".into()]))
        );
        let hosts_only = edit_of(&narrowed, None, None, Some(Covers::Only(vec!["k".into()])));
        assert_eq!(hosts_only.unwrap().agents, narrowed.agents);
        assert_eq!(edit_of(&narrowed, Some("one"), None, None), None);
    }

    #[test]
    fn a_removed_rule_cant_be_edited_back() {
        let rule = Rule::new("one", Covers::All, Covers::All);
        let unchecked = vec!["cloud".to_string()];
        let refused = edited_from(&[rule.removed()], &rule.id, &unchecked).unwrap_err();
        assert_eq!(
            (refused.code.as_str(), &refused.details["removed"]),
            ("no_such_rule", &json!(true))
        );
        assert_eq!(
            edited_from(std::slice::from_ref(&rule), &rule.id, &unchecked).unwrap(),
            rule
        );
        let missing = edited_from(&[], &rule.id, &unchecked).unwrap_err();
        assert_eq!(missing.details["removed"], false);
        assert_eq!(missing.details["unchecked_hosts"], json!(["cloud"]));
    }

    #[test]
    fn a_host_whose_latch_knows_no_such_command_is_outdated() {
        let failure =
            |code: &str| Failure::relayed(crate::failure::Exit::Usage, code, String::new());
        assert_eq!(reason(&failure("bad_command")), "host_outdated");
        assert_eq!(reason(&failure("rules_io")), "rules_io");
    }

    #[test]
    fn copies_are_the_same_when_every_rule_is_at_the_same_version() {
        let rule = Rule::new("one", Covers::All, Covers::All);
        let newer = rule.edited("two", Covers::All, Covers::All);
        let other = Rule::new("three", Covers::All, Covers::All);
        assert!(same(
            &[rule.clone(), other.clone()],
            &[other.clone(), rule.clone()]
        ));
        assert!(!same(std::slice::from_ref(&rule), &[newer]));
        assert!(!same(&[rule], &[]));
    }
}
