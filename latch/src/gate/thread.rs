//! Where the gatekeeper reads the owner's message an agent cites, and the
//! owner's later messages in its thread (or its room, when it has no
//! thread). The host reads them itself, as one of the owner's own devices;
//! the agent supplies only the reference, never a message's text.

use std::future::Future;
use std::pin::Pin;

use jiff::Timestamp;

/// A cited message: `<room>/<event>`.
#[derive(Clone, Debug, PartialEq)]
pub struct IntentRef {
    pub room: String,
    pub event: String,
}

impl IntentRef {
    /// `<room>/<event>`, split at its last `/`: Swarm room and event ids
    /// hold no `/`, but a room alias may.
    pub fn parse(text: &str) -> Option<IntentRef> {
        let (room, event) = text.trim().rsplit_once('/')?;
        (!room.is_empty() && !event.is_empty() && !event.contains(char::is_whitespace)).then(|| {
            IntentRef {
                room: room.to_string(),
                event: event.to_string(),
            }
        })
    }

    /// The ledger's key for every action taken on this intent.
    pub fn key(&self) -> String {
        format!("swarm:{}/{}", self.room, self.event)
    }
}

/// One message as the judge reads it.
#[derive(Clone, Debug)]
pub struct Said {
    pub text: String,
    pub at: Timestamp,
}

/// Someone else's message, labelled by the role their account has, read
/// as context only.
#[derive(Clone, Debug)]
pub struct Context {
    pub role: String,
    pub text: String,
    pub at: Timestamp,
}

/// The cited message and what followed it.
#[derive(Clone, Debug)]
pub struct Thread {
    /// The cited message, and whether the server says the owner (or a
    /// phone account that speaks for the owner) sent it.
    pub cited: Said,
    pub sender_is_owner: bool,
    /// The owner's messages after the cited one, oldest first.
    pub later_owner: Vec<Said>,
    /// Everyone else's messages around it, oldest first. May be capped.
    pub context: Vec<Context>,
    /// Whether the room is the agent's own 1:1 room with the owner: its
    /// Swarm DM with them, or its iMessage-bridge room.
    pub is_owner_dm: bool,
    /// Whether the owner closed the thread (✅ or ❌ on its root).
    pub closed: bool,
    /// Whether every owner message after the cited one was read.
    pub complete: bool,
}

/// A read's outcome: the thread, `None` when there is no such message in
/// that room, or why it could not be read (the reader's error, a timeout,
/// a message it could not decrypt).
pub type Reading<'a> = Pin<Box<dyn Future<Output = Result<Option<Thread>, String>> + Send + 'a>>;

/// Reads a cited intent for the agent whose Swarm account is `agent`.
pub trait ThreadSource: Send + Sync {
    fn read<'a>(&'a self, intent: &'a IntentRef, agent: &'a str) -> Reading<'a>;
}

/// The host's source until it can read Swarm: every citation is refused,
/// as unread, so nothing an agent cites is ever taken on trust.
pub struct NoReader;

impl ThreadSource for NoReader {
    fn read<'a>(&'a self, _intent: &'a IntentRef, _agent: &'a str) -> Reading<'a> {
        Box::pin(async { Err("this host has no Swarm reader set up yet".into()) })
    }
}

#[cfg(test)]
mod tests {
    use super::IntentRef;

    #[test]
    fn a_reference_is_a_room_and_an_event() {
        let r = IntentRef::parse("!abc:plow.co/$ev1").unwrap();
        assert_eq!(
            (r.room.as_str(), r.event.as_str()),
            ("!abc:plow.co", "$ev1")
        );
        assert_eq!(r.key(), "swarm:!abc:plow.co/$ev1");
        for bad in ["", "room", "/ev", "room/", "room/e v"] {
            assert_eq!(IntentRef::parse(bad), None, "{bad:?}");
        }
    }
}
