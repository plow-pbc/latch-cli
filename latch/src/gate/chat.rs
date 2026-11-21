//! Reading an agent's chat with its owner, through Plow's routes on the
//! host's own login: the agent's line, the chats on that line, and the
//! chosen chat's messages, newest first, back to its start or until they
//! could not fit one decision request.
//!
//! A message is the owner's when Plow classifies its sender as the member
//! whose role is `owner`. Everyone else (members, agents, Plow) is
//! context, labelled so.

use jiff::Timestamp;

use super::rules::ApprovalsLink;
use super::unavailable;
use crate::failure::{Failure, code};
use crate::model::REQUEST_BUDGET;
use crate::plow::{Message, Plow, Sender};

/// The chat the judge reads.
pub struct Conversation {
    pub chat: String,
    /// Oldest first.
    pub messages: Vec<Said>,
    /// Whether older messages were not read.
    pub more_before: bool,
}

/// One message, as the judge sees it.
pub struct Said {
    pub uid: String,
    pub from: String,
    pub owner: bool,
    pub at: Timestamp,
    pub text: String,
}

/// The chat `agent` acts in, on its own line or, when its owner linked its
/// approvals (`link`), on the linked agent's line, never its own: the
/// linked chat, else `named` if it is on that line, else the line's one
/// chat. `None` when its own line has no chat. Refused `which_chat` when
/// the agent must name one, or named one not on the line; and
/// `approvals_link_broken` when the linked agent is gone or its line has no
/// chat, or the linked chat left it.
pub async fn read(
    plow: &Plow,
    agent: &str,
    link: Option<&ApprovalsLink>,
    named: Option<&str>,
) -> Result<Option<Conversation>, Failure> {
    let failed = |failure: Failure| unavailable(&failure.message);
    let chat = match link {
        None => {
            let on_line = on_line(plow, agent).await.map_err(failed)?;
            match choose(named, &on_line, "this agent's")? {
                Some(chat) => chat,
                None => return Ok(None),
            }
        }
        Some(link) => {
            let asked = match (&link.chat, named) {
                (Some(linked), Some(named)) if linked != named => {
                    return Err(which_chat(format!(
                        "this agent's approvals are read from chat {linked}, as its owner linked them; omit --plow-chat and $LATCH_PLOW_CHAT, or name that chat"
                    )));
                }
                (Some(linked), _) => Some(linked.as_str()),
                (None, named) => named,
            };
            let on_line = match on_line(plow, &link.agent_uid).await {
                // Plow answers this on the owner's login: an agent that is
                // gone, or not the owner's, is one it does not know.
                Err(failure) if failure.code == code::not_found.name => {
                    return Err(broken(
                        agent,
                        link,
                        "Plow no longer has that agent among the owner's",
                        &format!("latch rules link-approvals TEXTED_AGENT_UID --agent {agent}"),
                    ));
                }
                result => result.map_err(failed)?,
            };
            // A linked line with no chat the host can read is no grant of
            // rules-only judgment: the owner meant this chat to be read.
            if on_line.is_empty() {
                return Err(broken(
                    agent,
                    link,
                    "the owner has no chat on that agent's line that this host can read",
                    &format!(
                        "latch rules link-approvals {} --agent {agent}",
                        link.agent_uid
                    ),
                ));
            }
            if let Some(linked) = &link.chat
                && !on_line.contains(linked)
            {
                return Err(broken(
                    agent,
                    link,
                    &format!("chat {linked} is no longer on that agent's line"),
                    &format!(
                        "latch rules link-approvals {} --agent {agent}",
                        link.agent_uid
                    ),
                ));
            }
            choose(asked, &on_line, "the linked agent's")?
                .expect("a line with a chat has one to choose")
        }
    };

    let mut newest_first = Vec::new();
    let mut bytes = 0;
    let mut more_before = true;
    while more_before && bytes <= REQUEST_BUDGET {
        let after = newest_first.last().map(|m: &Message| m.uid.as_str());
        let page = plow.messages(&chat, after).await.map_err(|failure| {
            unavailable(&format!(
                "Plow did not give the chat's messages: {}",
                failure.message
            ))
        })?;
        more_before = page.has_more && !page.messages.is_empty();
        bytes += page.messages.iter().map(|m| m.body.len()).sum::<usize>();
        newest_first.extend(page.messages);
    }
    let messages = newest_first
        .into_iter()
        .rev()
        .map(said)
        .collect::<Result<_, _>>()?;
    Ok(Some(Conversation {
        chat,
        messages,
        more_before,
    }))
}

fn said(message: Message) -> Result<Said, Failure> {
    let at = message
        .created_at
        .parse()
        .map_err(|_| unavailable("Plow gave a message with no readable time"))?;
    let owner = matches!(&message.sender, Sender::Member { role, .. } if role == "owner");
    let from = match &message.sender {
        _ if owner => "OWNER".to_string(),
        Sender::Agent { relationship } if relationship == "self" => "AGENT".to_string(),
        Sender::Agent { .. } => "ANOTHER AGENT (not the owner)".to_string(),
        Sender::Member { display_name, .. } => format!("MEMBER {display_name} (not the owner)"),
        Sender::System {} => "PLOW".to_string(),
    };
    Ok(Said {
        uid: message.uid,
        from,
        owner,
        at,
        text: message.body,
    })
}

/// The owner's chats on `agent`'s line, as Plow lists them to the host.
/// Plow's own failure as it is (`not_found`: no such agent), with what was
/// being asked.
pub async fn on_line(plow: &Plow, agent: &str) -> Result<Vec<String>, Failure> {
    let asking = |what: &str| {
        let what = what.to_string();
        move |mut failure: Failure| {
            failure.message = format!("{what}: {}", failure.message);
            failure
        }
    };
    let line = plow
        .agent(agent)
        .await
        .map_err(asking("Plow did not say which line this agent serves"))?
        .line;
    let chats = plow
        .chats()
        .await
        .map_err(asking("Plow did not list the chats"))?;
    Ok(chats
        .into_iter()
        .filter(|c| c.line().is_some_and(|l| l.uid == line.uid))
        .map(|c| c.uid)
        .collect())
}

/// The chat on a line with `chats` (`whose` they are, as a refusal says):
/// `named` if it is one of them, else the only one.
fn choose(named: Option<&str>, chats: &[String], whose: &str) -> Result<Option<String>, Failure> {
    match (named, chats) {
        (Some(named), _) if chats.iter().any(|c| c == named) => Ok(Some(named.to_string())),
        (Some(named), _) => Err(which_chat(format!(
            "chat {named} is not one of {whose} chats ({}); name one of those with --plow-chat or $LATCH_PLOW_CHAT",
            listed(chats)
        ))),
        (None, []) => Ok(None),
        (None, [only]) => Ok(Some(only.clone())),
        (None, _) => Err(which_chat(format!(
            "{whose} line has {} chats ({}); name the one you act for with --plow-chat or $LATCH_PLOW_CHAT",
            chats.len(),
            listed(chats)
        ))),
    }
}

/// A link of `agent`'s that no longer leads to a chat: nothing is judged
/// until the owner runs `fix`, or removes the link.
fn broken(agent: &str, link: &ApprovalsLink, why: &str, fix: &str) -> Failure {
    let unlink = format!("latch rules unlink-approvals --agent {agent}");
    let mut failure = Failure::new(
        code::approvals_link_broken,
        format!(
            "This agent's owner linked its approvals to agent {}'s line, but {why}, so nothing was judged. Ask the owner to run, on this host, `{fix}`, or `{unlink}` to read this agent's own line again.",
            link.agent_uid
        ),
    );
    failure.next = Some(fix.into());
    failure.details = serde_json::json!({"approvals_from": link.agent_uid, "chat": link.chat,
                                         "fix": fix, "unlink": unlink});
    failure
}

fn which_chat(message: String) -> Failure {
    Failure::new(code::which_chat, message)
}

fn listed(chats: &[String]) -> String {
    if chats.is_empty() {
        "it has none".to_string()
    } else {
        chats.join(", ")
    }
}
