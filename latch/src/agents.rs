//! `latch agents`: the account's agents, for their owner. Without a
//! subcommand, the list as Plow gives it; with `--host`, each agent's
//! standing on that host as its rules read says: whether it has rules of its
//! own there, and whether the host refuses it `exec`. The standing comes from
//! `rules show`, on this machine's state or from the host named, never from
//! anywhere else.
//!
//! `show`, `catalog`, `lines`, `new` and `remove` read and change the account
//! through Plow, which runs and assigns everything: latch never waits for a
//! cloud agent to start, never picks a line itself and never resends a change
//! whose answer was lost.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::{AgentsAction, AgentsArgs};
use crate::dial::{self, ANSWER_WITHIN};
use crate::failure::{Failure, code};
use crate::hosts;
use crate::login;
use crate::plow::{ListedAgent, Plow};
use crate::state::StateDir;
use crate::term;
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
    /// Its persona's contact card, for `agents card`.
    pub card_url: Option<String>,
}

#[derive(Serialize)]
pub struct Standing {
    pub own_rules: bool,
    /// `null` when the host cannot tell (its saved flags are unreadable).
    pub exec_blocked: Option<bool>,
}

pub async fn run(state: &StateDir, args: AgentsArgs) -> Result<Value, Failure> {
    let config = state.require_login()?;
    if config.peer.kind != "device" {
        let message = "agents are managed by their owner, from the owner's own machines";
        return Err(Failure::new(code::owner_only, message.into()));
    }
    let plow = Plow::from_state(state)?;
    managed(state, &plow, &config.api_url, args)
        .await
        .map_err(|mut failure| {
            if failure.code == "plow_denied" {
                failure.next = Some("Use an owner login that can manage agents on Plow.".into());
            }
            failure
        })
}

async fn managed(
    state: &StateDir,
    plow: &Plow,
    api_url: &str,
    args: AgentsArgs,
) -> Result<Value, Failure> {
    Ok(match args.action {
        // Plain strings and lists, so this cannot fail.
        None => serde_json::to_value(list(state, plow, args.host).await?).expect("serializes"),
        Some(AgentsAction::Show { agent }) => {
            let agent = resolve(plow, &agent).await?;
            json!({"agent": agent_shown(agent, None)})
        }
        Some(AgentsAction::Catalog) => catalog(plow).await?,
        Some(AgentsAction::StartByText) => start_by_text(api_url).await?,
        Some(AgentsAction::Lines) => lines(plow).await?,
        Some(AgentsAction::New {
            cloud,
            token_file,
            name,
            line,
            ..
        }) => new(plow, api_url, cloud, token_file, &name, &line).await?,
        Some(AgentsAction::Card { agent, output }) => card(plow, api_url, &agent, &output).await?,
        Some(AgentsAction::Remove { agent, yes }) => remove(plow, &agent, yes).await?,
    })
}

async fn list(
    state: &StateDir,
    plow: &Plow,
    host: Option<String>,
) -> Result<AgentsOutput, Failure> {
    let listed: Vec<ListedAgent> = plow.agent_list().await?;
    let (host, rules) = match host {
        None => (None, None),
        Some(name) => {
            let (host, rules) = rules_on(state, plow, name).await?;
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
            agent_shown(agent, standing)
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
        let online = crate::status::local(state)?.this_machine.hosting.ready;
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
            Failure::new(code::bad_host_answer, message)
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

fn agent_shown(agent: ListedAgent, standing: Option<Standing>) -> AgentShown {
    let own = agent.provider == "self_hosted";
    AgentShown {
        uid: agent.uid,
        name: agent.name,
        kind: if own { "own" } else { "cloud" },
        cloud_kind: (!own).then_some(agent.provider),
        line: LineShown {
            number: agent.line.provider_key,
            name: agent.line.display_name,
            card_url: agent.line.card_url,
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

/// The agent `named` names: a 32-hex uid is that agent's; anything else is
/// an exact name, which must be one agent's.
async fn resolve(plow: &Plow, named: &str) -> Result<ListedAgent, Failure> {
    let agents: Vec<ListedAgent> = plow.agent_list().await?;
    let is_uid = named.len() == 32 && named.bytes().all(|b| b.is_ascii_hexdigit());
    let mut found: Vec<ListedAgent> = agents
        .into_iter()
        .filter(|agent| {
            if is_uid {
                agent.uid == named
            } else {
                agent.name == named
            }
        })
        .collect();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => {
            let message = format!("no agent in this account is called {named:?}");
            let mut failure = Failure::new(code::no_such_agent, message);
            failure.next = Some("latch agents".into());
            Err(failure)
        }
        n => {
            let message = format!("{n} agents are called {named:?}: name one by its uid");
            let mut failure = Failure::new(code::ambiguous_agent, message);
            let agents: Vec<Value> = found
                .iter()
                .map(|agent| json!({"uid": agent.uid, "name": agent.name}))
                .collect();
            failure.details = json!({"agents": agents});
            Err(failure)
        }
    }
}

/// The kinds of cloud agent Plow offers.
async fn catalog(plow: &Plow) -> Result<Value, Failure> {
    let kinds: Vec<Value> = plow
        .agent_kinds()
        .await?
        .into_iter()
        .filter(|kind| kind.id != OWN)
        .map(|kind| json!({"id": kind.id, "name": kind.name}))
        .collect();
    Ok(json!({"kinds": kinds}))
}

/// What to text Plow's managed number to start each kind of cloud agent:
/// the kind's first phrase, which Plow's own pages offer too, ready for a
/// phone's camera (`smsto`) and for Messages on a Mac (`sms_link`).
async fn start_by_text(api_url: &str) -> Result<Value, Failure> {
    // Public, so it's asked without the owner's credential.
    let signup = Plow::anonymous(api_url)?.signup().await?;
    let number = signup.managed_phone;
    let kinds: Vec<Value> = signup
        .providers
        .into_iter()
        .filter_map(|kind| {
            let phrase = kind.phrases.into_iter().next()?;
            Some(json!({"id": kind.id, "name": kind.name,
                        "smsto": login::smsto(&number, &phrase),
                        "sms_link": login::sms_link(&number, &phrase), "phrase": phrase}))
        })
        .collect();
    Ok(json!({"number": number, "default": signup.default_provider, "kinds": kinds}))
}

/// Saves the contact card of `named`'s line to `output`, fetched from
/// where Plow says it is without the owner's credential: it is public.
async fn card(plow: &Plow, api_url: &str, named: &str, output: &Path) -> Result<Value, Failure> {
    let agent = resolve(plow, named).await?;
    let Some(url) = agent.line.card_url else {
        let message = format!("{}'s line has no contact card", agent.name);
        return Err(Failure::new(code::no_card, message));
    };
    let vcard = Plow::anonymous(api_url)?.fetch(&url).await?;
    let staged = crate::transfer::Staged::beside(output)?;
    let mut file = staged.file()?;
    let written = async {
        use tokio::io::AsyncWriteExt;
        file.write_all(&vcard).await?;
        file.flush().await
    };
    written
        .await
        .map_err(|err| Failure::new(code::local_file, format!("{}: {err}", output.display())))?;
    staged.place(output)?;
    Ok(json!({"agent": agent.uid, "card_url": url, "path": output, "size": vcard.len()}))
}

/// Plow's provider id for an agent its owner runs.
const OWN: &str = "self_hosted";

/// Plow's phone lines: the ones an agent can answer on, free or held.
async fn lines(plow: &Plow) -> Result<Value, Failure> {
    let lines: Vec<Value> = plow
        .lines()
        .await?
        .into_iter()
        .filter(|line| line.provider_type == "imessage")
        .map(|line| {
            json!({"uid": line.uid, "number": line.provider_key, "name": line.display_name,
                   "available": line.available, "agent_uid": line.agent_uid})
        })
        .collect();
    Ok(json!({"lines": lines}))
}

/// Creates an agent on `line`: run by Plow (`cloud`, a catalog kind) or by
/// its owner, whose credential goes to `token_file`, a new private file
/// reserved before Plow is asked, so a credential is never left with nowhere
/// to go. Returns at once.
async fn new(
    plow: &Plow,
    api_url: &str,
    cloud: Option<String>,
    token_file: Option<PathBuf>,
    name: &str,
    line: &str,
) -> Result<Value, Failure> {
    let name = name.trim();
    if name.is_empty() || name.chars().any(char::is_control) {
        let message = "an agent's name must have text and no control characters";
        return Err(Failure::new(code::bad_name, message.into()));
    }
    if token_file.as_deref() == Some(Path::new("-")) {
        let message = "an agent's credential is never printed: give --token-file a file";
        return Err(Failure::new(code::usage, message.into()));
    }
    let provider = match &cloud {
        Some(kind) => {
            let kinds = plow.agent_kinds().await?;
            if kind == OWN || !kinds.iter().any(|known| &known.id == kind) {
                let message = format!("Plow offers no cloud agent {kind:?}");
                let mut failure = Failure::new(code::unknown_cloud_kind, message);
                failure.next = Some("latch agents catalog".into());
                return Err(failure);
            }
            kind.as_str()
        }
        None => OWN,
    };
    let line_uid = line_named(plow, line).await?;
    let reserved = match &token_file {
        Some(path) => Some(TokenFile::reserve(path)?),
        None => None,
    };
    let created = match plow.create_agent(&line_uid, name, provider).await {
        Ok(created) => created,
        Err(failure) => {
            if let Some(reserved) = reserved {
                reserved.abandon();
            }
            return Err(not_created(failure, line));
        }
    };
    let uid = created.agent.uid.clone();
    let token_file = match reserved {
        Some(reserved) => Some(reserved.keep(created.token.as_deref(), &uid)?),
        None => None,
    };
    let own = token_file.is_some();
    Ok(json!({
        "agent": agent_shown(created.agent, None),
        "token_file": token_file,
        "api_url": own.then_some(api_url),
    }))
}

/// The uid of the line `named` names: its uid, or its number.
async fn line_named(plow: &Plow, named: &str) -> Result<String, Failure> {
    let lines = plow.lines().await?;
    match lines
        .into_iter()
        .find(|line| line.uid == named || line.provider_key == named)
    {
        Some(line) => Ok(line.uid),
        None => {
            let message = format!("Plow has no line {named:?}");
            let mut failure = Failure::new(code::no_such_line, message);
            failure.next = Some("latch agents lines".into());
            Err(failure)
        }
    }
}

/// Why Plow made no agent, or that it may have made one.
fn not_created(failure: Failure, line: &str) -> Failure {
    match (
        failure.code.as_str(),
        failure.plow.as_ref().and_then(|plow| plow.code.as_deref()),
    ) {
        ("conflict", Some("AGENT_EXISTS")) => {
            let message = format!("line {line} already has an agent; pick a free one");
            let mut failure = Failure::new(code::line_taken, message);
            failure.next = Some("latch agents lines".into());
            failure
        }
        (code, _) if lost(code) => {
            let message = "Plow may or may not have created the agent: its answer was lost; look before trying again";
            let mut failure = Failure::new(code::outcome_unknown, message.into());
            failure.next = Some("latch agents".into());
            failure
        }
        _ => failure,
    }
}

/// A failure after the request may have reached Plow: it may have done it.
fn lost(code: &str) -> bool {
    matches!(code, "plow_unreachable" | "plow_error")
}

/// Removes `named`, once its owner says so.
async fn remove(plow: &Plow, named: &str, yes: bool) -> Result<Value, Failure> {
    let agent = resolve(plow, named).await?;
    let own = agent.provider == OWN;
    let what = format!("{} ({})", agent.name, agent.line.provider_key);
    if !yes {
        if !term::interactive() {
            let message = format!("removing {what} signs it out of Plow");
            let mut failure = Failure::new(code::confirmation_required, message);
            failure.next = Some(
                format!(
                    "latch agents remove {} --yes",
                    crate::login::shell_quoted(named)
                )
                .into(),
            );
            return Err(failure);
        }
        let program = if own {
            " The program you run for it keeps running until you stop it."
        } else {
            ""
        };
        let question = format!(
            "Remove {what}? Its credential stops working at once, its line is freed and its \
             chats are closed.{program}"
        );
        if !term::confirm(&question) {
            let message = format!("kept {what}");
            return Err(Failure::new(code::not_removed, message));
        }
    }
    let uid = agent.uid.clone();
    match plow.delete_agent(&uid).await {
        Ok(removed) => Ok(json!({"removed": agent_shown(removed, None)})),
        Err(failure) if lost(&failure.code) => {
            let message = format!(
                "Plow may or may not have removed {what}: its answer was lost; look before trying again"
            );
            let mut failure = Failure::new(code::outcome_unknown, message);
            failure.next = Some(format!("latch agents show {uid}").into());
            Err(failure)
        }
        Err(failure) => Err(failure),
    }
}

/// An own agent's credential file: created empty, private and new before
/// the agent is, then written once with the credential.
struct TokenFile {
    path: PathBuf,
    file: std::fs::File,
}

impl TokenFile {
    fn reserve(path: &Path) -> Result<TokenFile, Failure> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let opened = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            // A link there is "already exists", never followed.
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path);
        match opened {
            Ok(file) => Ok(TokenFile { path, file }),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                let message = format!(
                    "{} already exists; the credential goes only to a new file: name another",
                    path.display()
                );
                Err(Failure::new(code::token_file_exists, message))
            }
            Err(err) => {
                let message = format!("{} cannot be made: {err}", path.display());
                Err(Failure::new(code::token_file_unwritable, message))
            }
        }
    }

    /// Plow made no agent: the empty file goes.
    fn abandon(self) {
        let _ = std::fs::remove_file(&self.path);
    }

    /// Writes `token` and returns the file's path. Plow made `agent_uid`,
    /// so a credential that cannot be kept is said plainly, and the agent
    /// left for its owner to remove.
    fn keep(mut self, token: Option<&str>, agent_uid: &str) -> Result<PathBuf, Failure> {
        let written = match token {
            Some(token) => self
                .file
                .write_all(format!("{token}\n").as_bytes())
                .and_then(|()| self.file.sync_all())
                .map_err(|err| err.to_string()),
            None => Err("Plow sent no credential".to_string()),
        };
        match written {
            Ok(()) => Ok(self.path),
            Err(why) => {
                let file_removed = std::fs::remove_file(&self.path).is_ok();
                let message = format!(
                    "the agent was created, but its credential could not be saved to {}: {why}",
                    self.path.display()
                );
                let mut failure = Failure::new(code::token_not_saved, message);
                failure.details = json!({"agent_uid": agent_uid, "path": self.path,
                    "file_removed": file_removed, "next": format!("latch agents show {agent_uid}")});
                Err(failure)
            }
        }
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
