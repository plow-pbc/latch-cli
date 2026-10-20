//! Reading an agent's chat with its owner, through Plow's routes on the
//! host's own login: the agent's line, the chats on that line, and the
//! chosen chat's messages, newest first, back to its start or until they
//! could not fit one decision request.
//!
//! A message is the owner's when Plow classifies its sender as the member
//! whose role is `owner`. Everyone else (members, agents, Plow) is
//! context, labelled so.

use jiff::Timestamp;

use super::unavailable;
use crate::browser::model::REQUEST_BUDGET;
use crate::failure::{Exit, Failure};
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

/// The chat `agent` acts in: `named` if it is on the agent's line, else
/// the line's one chat. `None` when the line has no chat. Refused
/// `which_chat` when the agent must name one, or named one not on its line.
pub async fn read(
    plow: &Plow,
    agent: &str,
    named: Option<&str>,
) -> Result<Option<Conversation>, Failure> {
    let failed = |what: &str| {
        let what = what.to_string();
        move |failure: Failure| unavailable(&format!("{what}: {}", failure.message))
    };
    let line = plow
        .agent(agent)
        .await
        .map_err(failed("Plow did not say which line this agent serves"))?
        .line;
    let chats = plow
        .chats()
        .await
        .map_err(failed("Plow did not list the chats"))?;
    let on_line: Vec<String> = chats
        .into_iter()
        .filter(|c| c.line().is_some_and(|l| l.uid == line.uid))
        .map(|c| c.uid)
        .collect();
    let chat = match (named, on_line.as_slice()) {
        (Some(named), _) if on_line.iter().any(|c| c == named) => named.to_string(),
        (Some(named), _) => {
            return Err(which_chat(format!(
                "chat {named} is not one of this agent's chats ({}); name one of those with --chat or $LATCH_CHAT",
                listed(&on_line)
            )));
        }
        (None, []) => return Ok(None),
        (None, [only]) => only.clone(),
        (None, _) => {
            return Err(which_chat(format!(
                "this agent is in {} chats ({}); name the one you act for with --chat or $LATCH_CHAT",
                on_line.len(),
                listed(&on_line)
            )));
        }
    };

    let mut newest_first = Vec::new();
    let mut bytes = 0;
    let mut more_before = true;
    while more_before && bytes <= REQUEST_BUDGET {
        let after = newest_first.last().map(|m: &Message| m.uid.as_str());
        let page = plow
            .messages(&chat, after)
            .await
            .map_err(failed("Plow did not give the chat's messages"))?;
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

fn which_chat(message: String) -> Failure {
    Failure::new(Exit::Usage, "which_chat", message)
}

fn listed(chats: &[String]) -> String {
    if chats.is_empty() {
        "it has none".to_string()
    } else {
        chats.join(", ")
    }
}
