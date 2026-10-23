//! `latch agents [--host NAME]`: the account's agents, as Plow lists them,
//! for their owner. With `--host`, each agent's standing on that host as its
//! rules read says: whether it has rules of its own there, and whether the
//! host refuses it `exec`. The standing comes from `rules show`, on this
//! machine's state or from the host named, never from anywhere else.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::AgentsArgs;
use crate::dial::{self, ANSWER_WITHIN};
use crate::failure::{Exit, Failure};
use crate::hosts;
use crate::plow::{ListedAgent, Plow};
use crate::state::StateDir;
use crate::wire::Request;

#[derive(Serialize)]
pub struct AgentsOutput {
    /// The host `--host` named, or `null` without it.
    pub host: Option<HostShown>,
    pub agents: Vec<AgentShown>,
}

#[derive(Serialize)]
pub struct HostShown {
    pub name: String,
    pub online: bool,
}

#[derive(Serialize)]
pub struct AgentShown {
    pub uid: String,
    pub name: String,
    /// `cloud` (Plow runs it) or `own` (its owner does).
    pub kind: &'static str,
    /// Plow's catalog id for a cloud agent (`exe:SLUG`).
    pub cloud_kind: Option<String>,
    pub line: LineShown,
    pub created_at: String,
    /// A cloud agent's phase, as Plow says it; `null` for an own agent.
    pub status: Option<String>,
    pub failure_code: Option<String>,
    /// When its credential was last used: not proof it is running now.
    pub last_seen_at: Option<String>,
    /// On the `--host` host; `null` without it.
    pub standing: Option<Standing>,
}

#[derive(Serialize)]
pub struct LineShown {
    pub number: String,
    pub name: Option<String>,
}

#[derive(Serialize)]
pub struct Standing {
    pub own_rules: bool,
    /// `null` when the host cannot tell (its saved flags are unreadable).
    pub exec_blocked: Option<bool>,
}

pub async fn run(state: &StateDir, args: AgentsArgs) -> Result<AgentsOutput, Failure> {
    let config = state.require_login()?;
    if config.peer.kind != "device" {
        let message = "agents are listed by their owner, from the owner's own machines";
        return Err(Failure::new(Exit::Denied, "owner_only", message.into()));
    }
    let plow = Plow::from_state(state)?;
    let listed: Vec<ListedAgent> = plow.agent_list().await?;
    let (host, rules) = match args.host {
        None => (None, None),
        Some(name) => {
            let (host, rules) = rules_on(state, &plow, name).await?;
            (Some(host), Some(rules))
        }
    };
    let rules = match (&host, rules) {
        (Some(host), Some(rules)) => Some(HostRules::read(&host.name, rules)?),
        _ => None,
    };
    let agents = listed
        .into_iter()
        .map(|agent| {
            let standing = rules.as_ref().map(|rules| rules.standing(&agent.uid));
            shown(agent, standing)
        })
        .collect();
    Ok(AgentsOutput { host, agents })
}

/// The host `name` names and its rules, as `rules show` reads them there.
async fn rules_on(
    state: &StateDir,
    plow: &Plow,
    name: String,
) -> Result<(HostShown, Value), Failure> {
    let show = json!({"command": "show"});
    if hosts::local_target(state, Some(&name))? {
        let config = state.require_config()?;
        // The same read `rules show` makes, but a list is not a command on
        // the rules, so it is not audited.
        let done =
            crate::gate::owner::run(&state.path, plow, &config.peer.name, "device", &show).await;
        let rules = done.response.into_result()?;
        let online = crate::status::local(state)?.serve.ready;
        return Ok((HostShown { name, online }, rules));
    }
    let host = hosts::resolve(state, Some(name)).await?;
    let request = Request {
        op: "rules".to_string(),
        args: show,
        chat: None,
    };
    // Answering at all is what online means here: an offline host fails.
    let rules = dial::call(state, &host, &request, ANSWER_WITHIN).await?;
    let host = HostShown {
        name: host.name,
        online: true,
    };
    Ok((host, rules))
}

/// What a host's `rules show` answer says about each agent's standing,
/// read strictly: an answer without a readable `deny_exec` is an error, and
/// only the host's own `null` (it cannot tell) leaves `exec_blocked` unknown.
#[derive(Debug, PartialEq)]
struct HostRules {
    /// The agents with rules of their own there.
    own_rules: Vec<String>,
    deny_exec: Option<DenyExecRead>,
}

#[derive(Deserialize, Debug, PartialEq)]
struct DenyExecRead {
    every_agent: bool,
    agent_uids: Vec<String>,
}

#[derive(Deserialize)]
struct RulesRead {
    agents: Vec<AgentRulesRead>,
}

#[derive(Deserialize)]
struct AgentRulesRead {
    agent_uid: String,
    text: Option<String>,
}

impl HostRules {
    fn read(host: &str, rules: Value) -> Result<HostRules, Failure> {
        let unreadable = |what: &str| {
            let message = format!(
                "{host}'s rules answer has no readable {what}, so its agents' standing is \
                 unknown; it may run an older latch: `latch update` there"
            );
            Failure::failed("bad_host_answer", message)
        };
        let deny_exec = match rules.get("deny_exec") {
            None => return Err(unreadable("deny_exec")),
            Some(Value::Null) => None,
            Some(deny) => {
                Some(serde_json::from_value(deny.clone()).map_err(|_| unreadable("deny_exec"))?)
            }
        };
        let read: RulesRead = serde_json::from_value(rules).map_err(|_| unreadable("agents"))?;
        let own_rules = read
            .agents
            .into_iter()
            .filter(|agent| agent.text.is_some())
            .map(|agent| agent.agent_uid)
            .collect();
        Ok(HostRules {
            own_rules,
            deny_exec,
        })
    }

    fn standing(&self, uid: &str) -> Standing {
        Standing {
            own_rules: self.own_rules.iter().any(|own| own == uid),
            exec_blocked: self.deny_exec.as_ref().map(|deny| {
                deny.every_agent || deny.agent_uids.iter().any(|blocked| blocked == uid)
            }),
        }
    }
}

fn shown(agent: ListedAgent, standing: Option<Standing>) -> AgentShown {
    let own = agent.provider == "self_hosted";
    AgentShown {
        uid: agent.uid,
        name: agent.name,
        kind: if own { "own" } else { "cloud" },
        cloud_kind: (!own).then_some(agent.provider),
        line: LineShown {
            number: agent.line.provider_key,
            name: agent.line.display_name,
        },
        created_at: agent.created_at,
        status: agent.status,
        failure_code: agent.failure_code,
        last_seen_at: agent
            .credential
            .and_then(|credential| credential.last_seen_at),
        standing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UID: &str = "0123456789abcdef0123456789abcdef";

    fn read(rules: Value) -> Result<HostRules, Failure> {
        HostRules::read("studio", rules)
    }

    #[test]
    fn standing_is_read_from_rules_and_deny_exec() {
        let rules = read(
            json!({"agents": [{"agent_uid": UID, "text": "Only tests."}],
            "deny_exec": {"every_agent": false, "agent_uids": [UID]}}),
        )
        .unwrap();
        let standing = rules.standing(UID);
        assert_eq!(
            (standing.own_rules, standing.exec_blocked),
            (true, Some(true))
        );
        let other = rules.standing("fedcba9876543210fedcba9876543210");
        assert_eq!((other.own_rules, other.exec_blocked), (false, Some(false)));
    }

    #[test]
    fn exec_blocked_is_unknown_only_when_the_host_says_so() {
        let rules = read(json!({"agents": [], "deny_exec": null})).unwrap();
        assert_eq!(rules.standing(UID).exec_blocked, None);
    }

    #[test]
    fn a_missing_or_malformed_deny_exec_is_an_error_never_false() {
        for rules in [
            json!({"agents": []}),
            json!({"agents": [], "deny_exec": {}}),
            json!({"agents": [], "deny_exec": {"every_agent": "yes", "agent_uids": []}}),
            json!({"agents": [], "deny_exec": {"every_agent": false}}),
            json!({"agents": [], "deny_exec": {"every_agent": false, "agent_uids": "all"}}),
            json!({"deny_exec": {"every_agent": false, "agent_uids": []}}),
        ] {
            let error = read(rules.clone()).unwrap_err();
            assert_eq!(error.code, "bad_host_answer", "{rules}");
            assert!(error.message.contains("studio"), "{error:?}");
        }
    }
}
