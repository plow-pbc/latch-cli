//! The Plow directory client: register this peer, list the account's hosts,
//! (on a host) look up who a caller's key belongs to and say it is going
//! offline, remove a machine, and the two calls of a login by text.
//!
//! Each call is made once and never retried here, so latch never calls Plow
//! in a loop. (The host's heartbeat, which does repeat, backs off on errors.)

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::credential;
use crate::failure::{Exit, Failure};
use crate::state::StateDir;

/// A peer as Plow describes it. Fields Plow adds that latch does not use
/// are ignored when parsing.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Peer {
    pub key: String,
    pub kind: String,
    pub name: String,
    pub relay_url: Option<String>,
    pub last_seen_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub online: Option<bool>,
    /// Set by latch, never by Plow: this is the machine running the
    /// command, which cannot call itself.
    #[serde(rename = "self", default, skip_serializing_if = "is_false")]
    pub this_machine: bool,
}

/// Body of `POST /v1/latch/peers`. Absent fields are left out of the JSON.
#[derive(Serialize, Clone, Debug)]
pub struct Registration {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
    /// Asks Plow to move this login's own machine name to this key. Only
    /// `latch login` sets it; a heartbeat never does, so a host whose name
    /// moved away cannot take it back.
    #[serde(skip_serializing_if = "is_false")]
    pub take_over: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// A login by text, as `POST /v1/auth/activate` starts it: the code the
/// person texts, the number they text it to, and the secret latch polls with.
#[derive(Deserialize)]
pub struct Activation {
    pub display_code: String,
    pub activation_secret: String,
    pub send_to: String,
}

/// One poll of `POST /v1/auth/activate/redeem`. Only the first `verified`
/// answer carries the token; later ones say `null`.
#[derive(Deserialize)]
pub struct Redeemed {
    pub status: String,
    pub token: Option<String>,
}

#[derive(Deserialize)]
struct PeerList {
    data: Vec<Peer>,
}

/// Who a caller's key belongs to, from `GET /v1/latch/peers/{key}` (§5.3).
/// Plow only answers for live peers in the host's own account.
#[derive(Deserialize, Debug)]
pub struct Identity {
    pub key: String,
    pub kind: String,
    pub name: String,
    pub agent_uid: Option<String>,
    pub permissions: Permissions,
}

#[derive(Deserialize, Debug)]
pub struct Permissions {
    pub call: bool,
}

/// An agent, from `GET /v1/agents/{uid}`: the line it serves.
#[derive(Deserialize, Debug)]
pub struct Agent {
    pub line: Line,
}

/// One of the account's agents, from `GET /v1/agents`, by its uid and name.
#[derive(Deserialize, Debug, Clone)]
pub struct NamedAgent {
    pub uid: String,
    pub name: String,
}

/// A messaging line (a phone number or a mailbox).
#[derive(Deserialize, Debug, Clone)]
pub struct Line {
    pub uid: String,
}

/// A chat, from `GET /v1/chats`. Its line is the agent participant whose
/// relationship is `self`.
#[derive(Deserialize, Debug)]
pub struct Chat {
    pub uid: String,
    pub participants: Vec<Participant>,
}

impl Chat {
    pub fn line(&self) -> Option<&Line> {
        self.participants.iter().find_map(|p| match p {
            Participant::Agent { line, relationship } if relationship == "self" => Some(line),
            _ => None,
        })
    }
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Participant {
    Agent {
        line: Line,
        relationship: String,
    },
    #[serde(other)]
    Other,
}

/// A chat message, from `GET /v1/chats/{uid}/messages`.
#[derive(Deserialize, Debug, Clone)]
pub struct Message {
    pub uid: String,
    pub body: String,
    pub sender: Sender,
    pub created_at: String,
}

/// Who sent a message, as Plow classifies it. A member's `role` is `owner`
/// when the sender is the account the chat belongs to.
#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Sender {
    Agent { relationship: String },
    Member { display_name: String, role: String },
    System {},
}

pub struct MessagePage {
    pub messages: Vec<Message>,
    pub has_more: bool,
}

#[derive(Deserialize)]
struct List<T> {
    data: Vec<T>,
    #[serde(default)]
    has_more: bool,
}

// No `Debug`: this struct holds the Plow credential, and a debug print of
// it must never be possible. `Clone` is cheap: the HTTP client inside is
// shared, not copied, so the host's heartbeat and caller checks share it.
#[derive(Clone)]
pub struct Plow {
    http: reqwest::Client,
    api_url: String,
    token: String,
}

impl Plow {
    pub fn new(api_url: &str, token: &str) -> Plow {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            // A redirect must not carry the credential to another server.
            .redirect(reqwest::redirect::Policy::none())
            // One attempt per command: latch never calls Plow in a loop.
            .retry(reqwest::retry::never())
            .build()
            .expect("HTTP client settings are valid");
        Plow {
            http,
            api_url: api_url.trim_end_matches('/').to_string(),
            token: token.trim().to_string(),
        }
    }

    /// A client with no credential, for the calls that take none: those
    /// that log in by text.
    pub fn anonymous(api_url: &str) -> Plow {
        Plow::new(api_url, "")
    }

    /// A client using the URL saved by `login` and this peer's credential.
    pub fn from_state(state: &StateDir) -> Result<Plow, Failure> {
        let config = state.require_login()?;
        let token = credential::token(state)?;
        Ok(Plow::new(&config.api_url, &token))
    }

    /// Registers this peer, or refreshes it if it already is (§5.1).
    pub async fn register(&self, registration: &Registration) -> Result<Peer, Failure> {
        let url = format!("{}/v1/latch/peers", self.api_url);
        let request = self.http.post(url).json(registration);
        let response = self.send(request).await?;
        response.json().await.map_err(|_| bad_answer())
    }

    /// Every live peer in the account, machines and agents, online or not.
    pub async fn list_peers(&self) -> Result<Vec<Peer>, Failure> {
        let url = format!("{}/v1/latch/peers", self.api_url);
        let request = self.http.get(url);
        let response = self.send(request).await?;
        let list: PeerList = response.json().await.map_err(|_| bad_answer())?;
        Ok(list.data)
    }

    /// Starts a login by text for a machine called `name`.
    pub async fn activate(&self, name: &str) -> Result<Activation, Failure> {
        let url = format!("{}/v1/auth/activate", self.api_url);
        let request = self
            .http
            .post(url)
            .json(&serde_json::json!({ "name": name }));
        let response = self.send(request).await?;
        response.json().await.map_err(|_| bad_answer())
    }

    /// Asks whether the code was texted yet. An expired code is a failure
    /// with code `expired`, an unknown one `not_found`.
    pub async fn redeem(&self, activation_secret: &str) -> Result<Redeemed, Failure> {
        let url = format!("{}/v1/auth/activate/redeem", self.api_url);
        let body = serde_json::json!({ "activation_secret": activation_secret });
        let response = self.send(self.http.post(url).json(&body)).await?;
        response.json().await.map_err(|_| bad_answer())
    }

    /// Removes one of the account's machines and signs out the login that
    /// registered it. Returns the removed peer. Owner logins only.
    pub async fn remove_peer(&self, key: &str) -> Result<Peer, Failure> {
        let url = format!("{}/v1/latch/peers/{key}", self.api_url);
        let request = self.http.delete(url);
        let response = self.send(request).await?;
        response.json().await.map_err(|_| bad_answer())
    }

    /// Tells Plow this peer is going offline, so it is listed offline now
    /// rather than when its heartbeat runs out. Only the credential that
    /// registered the peer may say so.
    pub async fn go_offline(&self, key: &str) -> Result<Peer, Failure> {
        let url = format!("{}/v1/latch/peers/{key}/offline", self.api_url);
        let request = self.http.post(url);
        let response = self.send(request).await?;
        response.json().await.map_err(|_| bad_answer())
    }

    /// Who `key` is: a live peer in this account, or a failure (404 for an
    /// unknown key, another account's key, or a revoked one).
    pub async fn lookup(&self, key: &str) -> Result<Identity, Failure> {
        let url = format!("{}/v1/latch/peers/{key}", self.api_url);
        let request = self.http.get(url);
        let response = self.send(request).await?;
        let identity: Identity = response.json().await.map_err(|_| bad_answer())?;
        // Trust the answer only if it is about the key we asked about.
        if identity.key != key {
            let message = "Plow answered about a different key".to_string();
            return Err(Failure::failed("plow_error", message));
        }
        Ok(identity)
    }

    /// One of the account's agents, with the line it serves.
    pub async fn agent(&self, agent_uid: &str) -> Result<Agent, Failure> {
        let url = format!("{}/v1/agents/{agent_uid}", self.api_url);
        let response = self
            .send(self.http.get(url).bearer_auth(&self.token))
            .await?;
        response.json().await.map_err(|_| bad_answer())
    }

    /// Every live agent in the account. Owner logins only.
    pub async fn agents(&self) -> Result<Vec<NamedAgent>, Failure> {
        let url = format!("{}/v1/agents", self.api_url);
        let response = self.send(self.http.get(url)).await?;
        response.json().await.map_err(|_| bad_answer())
    }

    /// Every chat this login may see, oldest first.
    pub async fn chats(&self) -> Result<Vec<Chat>, Failure> {
        let url = format!("{}/v1/chats", self.api_url);
        let response = self
            .send(self.http.get(url).bearer_auth(&self.token))
            .await?;
        let list: List<Chat> = response.json().await.map_err(|_| bad_answer())?;
        Ok(list.data)
    }

    /// One page of a chat's messages, newest first, older than
    /// `starting_after` when given, each body as the literal text sent.
    pub async fn messages(
        &self,
        chat_uid: &str,
        starting_after: Option<&str>,
    ) -> Result<MessagePage, Failure> {
        let url = format!("{}/v1/chats/{chat_uid}/messages", self.api_url);
        let mut query = vec![("limit", "100"), ("format", "text_decorations")];
        if let Some(uid) = starting_after {
            query.push(("starting_after", uid));
        }
        let url = url::Url::parse_with_params(&url, &query).map_err(|_| bad_answer())?;
        let request = self.http.get(url).bearer_auth(&self.token);
        let response = self.send(request).await?;
        let page: List<Message> = response.json().await.map_err(|_| bad_answer())?;
        Ok(MessagePage {
            messages: page.data,
            has_more: page.has_more,
        })
    }

    /// Fresh short-lived credentials for the owner's connected Google accounts.
    pub async fn google_tokens(&self) -> Result<crate::google::Tokens, Failure> {
        #[derive(Deserialize)]
        struct Envelope {
            data: crate::google::Tokens,
        }
        let request = self
            .http
            .post(format!("{}/v1/connectors/gmail/access-token", self.api_url))
            .json(&serde_json::json!({"all":true}));
        let response = self.send(request).await?;
        response
            .json::<Envelope>()
            .await
            .map(|e| e.data)
            .map_err(|_| bad_answer())
    }

    /// Sends a request with this client's credential, and turns "could not
    /// connect" and non-2xx answers into failures. Plow's own error text is
    /// never shown: a server body could echo a credential or someone's data.
    /// The status says enough. With no credential (a login by text, or a
    /// cloud agent whose proxy adds its own) no `Authorization` is sent.
    async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response, Failure> {
        let request = if self.token.is_empty() {
            request
        } else {
            request.bearer_auth(&self.token)
        };
        let response = match request.send().await {
            Ok(response) => response,
            Err(err) => {
                let message = format!("could not reach Plow at {}: {err}", self.api_url);
                return Err(Failure::failed("plow_unreachable", message));
            }
        };
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let (exit, code, meaning) = match status.as_u16() {
            401 | 403 => (
                Exit::Denied,
                "plow_denied",
                "this credential may not do that",
            ),
            409 => (
                Exit::Usage,
                "conflict",
                "that name, key or agent is taken by another peer",
            ),
            404 => (Exit::Failed, "not_found", "Plow knows no such peer"),
            410 => (Exit::Failed, "expired", "that has expired"),
            422 => (
                Exit::Usage,
                "invalid_request",
                "Plow rejected the request's fields",
            ),
            429 => (Exit::Failed, "rate_limited", "Plow asked us to slow down"),
            _ => (Exit::Failed, "plow_error", "Plow could not answer"),
        };
        let message = format!("Plow answered HTTP {}: {meaning}", status.as_u16());
        let mut failure = Failure::new(exit, code, message);
        failure.retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(retry_after);
        Err(failure)
    }
}

/// The `Retry-After` header as a number of seconds, if Plow sent one.
/// A `Retry-After` header as a wait: either a number of seconds (`120`) or
/// an HTTP date (`Wed, 15 Oct 2026 05:00:00 GMT`), which is turned into the
/// time left until then (zero if it has passed).
fn retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = jiff::civil::DateTime::strptime("%a, %d %b %Y %H:%M:%S GMT", value).ok()?;
    let deadline = date.to_zoned(jiff::tz::TimeZone::UTC).ok()?.timestamp();
    let seconds = deadline.as_second() - jiff::Timestamp::now().as_second();
    Some(Duration::from_secs(seconds.max(0) as u64))
}

fn bad_answer() -> Failure {
    Failure::failed(
        "plow_error",
        "Plow's answer was not what latch expected".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_reads_seconds_and_http_dates() {
        assert_eq!(retry_after("45"), Some(Duration::from_secs(45)));
        let in_90s = jiff::Timestamp::now() + Duration::from_secs(90);
        let header = in_90s.strftime("%a, %d %b %Y %H:%M:%S GMT").to_string();
        let wait = retry_after(&header).unwrap().as_secs();
        assert!((88..=90).contains(&wait), "{wait}");
        assert_eq!(
            retry_after("Mon, 01 Jan 2024 00:00:00 GMT"),
            Some(Duration::ZERO)
        );
        assert_eq!(retry_after("soon"), None);
    }
}
