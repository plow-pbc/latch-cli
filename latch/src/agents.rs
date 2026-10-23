//! `latch agents [--host NAME]`: the account's agents, as Plow lists them,
//! for their owner. With `--host`, each agent's standing on that host as its
//! rules read says: whether it has rules of its own there, and whether the
//! host refuses it `exec`. The standing comes from `rules show`, on this
//! machine's state or from the host named, never from anywhere else.

use serde::Serialize;
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
    let agents = listed
        .into_iter()
        .map(|agent| {
            let standing = rules.as_ref().map(|rules| standing(rules, &agent.uid));
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

/// `uid`'s standing in a `rules show` answer.
fn standing(rules: &Value, uid: &str) -> Standing {
    let own_rules = rules["agents"].as_array().is_some_and(|agents| {
        agents
            .iter()
            .any(|agent| agent["agent_uid"] == uid && agent["text"].is_string())
    });
    let deny = &rules["deny_exec"];
    let exec_blocked = (!deny.is_null()).then(|| {
        deny["every_agent"] == true
            || deny["agent_uids"]
                .as_array()
                .is_some_and(|uids| uids.iter().any(|blocked| blocked == uid))
    });
    Standing {
        own_rules,
        exec_blocked,
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
