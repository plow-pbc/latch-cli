//! The Plow directory client: register this peer, list the account's hosts,
//! (on a host) look up who a caller's key belongs to and say it is going
//! offline, remove a machine or this one, and the two calls of a login by
//! text.
//!
//! Each call is made once and never retried here, so latch never calls Plow
//! in a loop. (The host's heartbeat, which does repeat, backs off on errors.)

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::credential;
use crate::failure::{Failure, code};
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
    /// command; its operations use the local socket.
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
    /// When the code stops working, if Plow says.
    #[serde(default)]
    pub expires_at: Option<String>,
}

/// One poll of `POST /v1/auth/activate/redeem`. Only the first `verified`
/// answer carries the token; later ones say `null`.
#[derive(Deserialize)]
pub struct Redeemed {
    pub status: String,
    pub token: Option<String>,
}

/// The agent a credential belongs to, from `GET /v1/agents/me`.
#[derive(Deserialize, Clone, Debug)]
pub struct AgentIdentity {
    pub uid: String,
    pub name: String,
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

/// An agent, from `GET /v1/agents/{uid}`: its current name and the line
/// it serves.
#[derive(Deserialize, Debug)]
pub struct Agent {
    pub name: String,
    pub line: Line,
}

/// `GET /v1/connectors`: the owner's connected accounts, and where Plow's
/// pages are.
#[derive(Deserialize, Debug)]
pub struct Connectors {
    /// Google (Gmail and Calendar).
    pub gmail: Connection,
    pub web_base_url: String,
}

impl Connectors {
    /// The page where the owner connects accounts.
    pub fn connections_page(&self) -> String {
        format!("{}/connections", self.web_base_url.trim_end_matches('/'))
    }
}

/// `POST /v1/connectors/gmail/connect-code`: a single-use link that signs
/// a Google account in to the owner's Plow account. The link carries a
/// credential until it is used or expires, so this is never logged
/// (no `Debug`); Plow's `code` is in the link and not kept apart.
#[derive(Deserialize)]
pub struct ConnectLink {
    pub url: String,
    pub expires_at: String,
}

#[derive(Deserialize, Debug)]
pub struct Connection {
    pub connected: bool,
    #[serde(default)]
    pub accounts: Vec<ConnectedAccount>,
}

#[derive(Deserialize, Debug)]
pub struct ConnectedAccount {
    pub account: String,
}

/// One of the account's agents, from `GET /v1/agents`, by its uid and name.
#[derive(Deserialize, Debug, Clone)]
pub struct NamedAgent {
    pub uid: String,
    pub name: String,
}

/// One of the account's agents, from `GET /v1/agents`, in full.
#[derive(Deserialize, Debug, Clone)]
pub struct ListedAgent {
    pub uid: String,
    pub name: String,
    /// `self_hosted` for an agent its owner runs, else the cloud kind
    /// (`exe:SLUG`).
    pub provider: String,
    pub line: ListedLine,
    pub created_at: String,
    /// The cloud agent's phase (`provisioning`, `running`, `failed`, …);
    /// none for one its owner runs.
    pub status: Option<String>,
    pub failure_code: Option<String>,
    pub credential: Option<ListedCredential>,
}

/// The line an agent serves, as people read it.
#[derive(Deserialize, Debug, Clone)]
pub struct ListedLine {
    /// The phone number or address.
    pub provider_key: String,
    pub display_name: Option<String>,
    /// Its persona's contact card (a vCard, photo included); none for an
    /// unnamed line, or from a Plow that doesn't say.
    #[serde(default)]
    pub card_url: Option<String>,
}

/// A kind of agent Plow can run, from its catalog.
#[derive(Deserialize, Debug)]
pub struct AgentKind {
    /// `exe:SLUG`, or `self_hosted`.
    pub id: String,
    pub name: String,
}

/// How a cloud agent is started by text: Plow's managed number, and the
/// phrases that start each kind it offers.
#[derive(Deserialize, Debug)]
pub struct Signup {
    pub managed_phone: String,
    pub providers: Vec<SignupProvider>,
    /// The kind Plow's own pages and clients offer without asking which.
    pub default_provider: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct SignupProvider {
    /// `exe:SLUG`, as in the catalog.
    pub id: String,
    pub name: String,
    /// The texts that start it; a client offers the first.
    pub phrases: Vec<String>,
}

/// One of Plow's phone lines.
#[derive(Deserialize, Debug)]
pub struct PlowLine {
    pub uid: String,
    pub provider_type: String,
    pub provider_key: String,
    pub display_name: Option<String>,
    /// No agent of the account's holds it.
    pub available: bool,
    pub agent_uid: Option<String>,
}

/// Plow's answer to creating an agent.
#[derive(Deserialize)]
pub struct CreatedAgent {
    pub agent: ListedAgent,
    /// An own agent's credential, which Plow shows only now.
    pub token: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ListedCredential {
    /// When the agent's credential was last used, as Plow recorded it.
    pub last_seen_at: Option<String>,
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

/// How long a request to Plow may take.
const ANSWER_WITHIN: Duration = Duration::from_secs(30);

/// Plow at `api_url` took longer than `within` to answer.
pub fn no_answer(api_url: &str, within: Duration) -> Failure {
    let message = format!(
        "Plow at {api_url} didn't answer within {} s",
        within.as_secs()
    );
    Failure::new(code::plow_unreachable, message)
}

/// The HTTP client `builder` makes. Its settings are fixed; what can fail is
/// the system's part: a machine with no CA certificates (a bare container)
/// gives nothing to check a server against.
pub fn http(builder: reqwest::ClientBuilder) -> Result<reqwest::Client, Failure> {
    builder.build().map_err(|err| {
        let mut reason = err.to_string();
        let mut source = std::error::Error::source(&err);
        while let Some(err) = source {
            reason = err.to_string();
            source = err.source();
        }
        let message = format!("latch can't make HTTPS connections on this machine: {reason}");
        let mut failure = Failure::new(code::no_ca_certificates, message);
        failure.next = Some(
            "install the system's CA certificates (such as ca-certificates), then run this again"
                .into(),
        );
        failure
    })
}

impl Plow {
    pub fn new(api_url: &str, token: &str) -> Result<Plow, Failure> {
        let http = http(
            reqwest::Client::builder()
                .timeout(ANSWER_WITHIN)
                // A redirect must not carry the credential to another server.
                .redirect(reqwest::redirect::Policy::none())
                // One attempt per command: latch never calls Plow in a loop.
                .retry(reqwest::retry::never()),
        )?;
        Ok(Plow {
            http,
            api_url: api_url.trim_end_matches('/').to_string(),
            token: token.trim().to_string(),
        })
    }

    /// A client with no credential, for the calls that take none: those
    /// that log in by text.
    pub fn anonymous(api_url: &str) -> Result<Plow, Failure> {
        Plow::new(api_url, "")
    }

    /// A client using the URL saved by `login` and this peer's credential.
    pub fn from_state(state: &StateDir) -> Result<Plow, Failure> {
        let config = state.require_login()?;
        let token = credential::token(state)?;
        Plow::new(&config.api_url, &token)
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

    /// The agent this credential belongs to. 404 (`not_found`) for a
    /// credential that is not an agent's.
    pub async fn agent_me(&self) -> Result<AgentIdentity, Failure> {
        #[derive(Deserialize)]
        struct Me {
            agent: AgentIdentity,
        }
        let url = format!("{}/v1/agents/me", self.api_url);
        let response = self.send(self.http.get(url)).await?;
        let me: Me = response.json().await.map_err(|_| bad_answer())?;
        Ok(me.agent)
    }

    /// The account this credential belongs to, as an opaque uid.
    pub async fn owner_uid(&self) -> Result<String, Failure> {
        #[derive(Deserialize)]
        struct OwnerUid {
            owner_uid: String,
        }
        let url = format!("{}/v1/auth/owner-uid", self.api_url);
        let response = self.send(self.http.get(url)).await?;
        let owner: OwnerUid = response.json().await.map_err(|_| bad_answer())?;
        Ok(owner.owner_uid)
    }

    /// The account owner's display name, if they set one.
    pub async fn display_name(&self) -> Result<Option<String>, Failure> {
        #[derive(Deserialize)]
        struct Profile {
            display_name: Option<String>,
        }
        let url = format!("{}/v1/auth/profile", self.api_url);
        let response = self.send(self.http.get(url)).await?;
        let profile: Profile = response.json().await.map_err(|_| bad_answer())?;
        Ok(profile.display_name)
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

    /// Takes this machine out of the account and signs out its login: what
    /// `latch logout` does. Returns the removed machines. Owner logins only.
    pub async fn remove_self(&self) -> Result<Vec<Peer>, Failure> {
        let url = format!("{}/v1/latch/peers/self", self.api_url);
        let response = self.send(self.http.delete(url)).await?;
        let list: PeerList = response.json().await.map_err(|_| bad_answer())?;
        Ok(list.data)
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
            return Err(Failure::new(code::plow_error, message));
        }
        Ok(identity)
    }

    /// One of the account's agents, with the line it serves.
    pub async fn agent(&self, agent_uid: &str) -> Result<Agent, Failure> {
        let url = format!("{}/v1/agents/{agent_uid}", self.api_url);
        let response = self.send(self.http.get(url)).await?;
        response.json().await.map_err(|_| bad_answer())
    }

    /// A fresh Google sign-in link for the owner, as Plow wrote it: a link
    /// latch built would put the code where agent runtimes redact it.
    pub async fn google_connect_link(&self) -> Result<ConnectLink, Failure> {
        let url = format!("{}/v1/connectors/gmail/connect-code", self.api_url);
        let response = self
            .send(self.http.post(url).json(&serde_json::json!({})))
            .await?;
        response.json().await.map_err(|_| bad_answer())
    }

    /// The page in Plow where the owner connects accounts such as Google.
    pub async fn connections_page(&self) -> Result<String, Failure> {
        Ok(self.connectors().await?.connections_page())
    }

    /// Which accounts the owner has connected in Plow. Only reads.
    pub async fn connectors(&self) -> Result<Connectors, Failure> {
        let url = format!("{}/v1/connectors", self.api_url);
        let response = self.send(self.http.get(url)).await?;
        response.json().await.map_err(|_| bad_answer())
    }

    /// Every live agent in the account. Owner logins only.
    pub async fn agents(&self) -> Result<Vec<NamedAgent>, Failure> {
        self.agent_list().await
    }

    /// The kinds of agent Plow can run, its own `self_hosted` among them.
    pub async fn agent_kinds(&self) -> Result<Vec<AgentKind>, Failure> {
        let url = format!("{}/v1/agents/providers", self.api_url);
        let response = self.send(self.http.get(url)).await?;
        response.json().await.map_err(|_| bad_answer())
    }

    /// How to start a cloud agent by texting Plow's managed number: public,
    /// so asked of an `anonymous` client.
    pub async fn signup(&self) -> Result<Signup, Failure> {
        let url = format!("{}/v1/signup", self.api_url);
        let response = self.send(self.http.get(url)).await?;
        response.json().await.map_err(|_| bad_answer())
    }

    /// The file at `url`, which Plow named (a line's contact card), asked
    /// for with this client's credential: an anonymous client sends none.
    pub async fn fetch(&self, url: &str) -> Result<Vec<u8>, Failure> {
        let response = self.send(self.http.get(url)).await?;
        let body = response.bytes().await.map_err(|_| bad_answer())?;
        Ok(body.to_vec())
    }

    /// Every phone line of Plow's, and whether each is free for an agent.
    pub async fn lines(&self) -> Result<Vec<PlowLine>, Failure> {
        let url = format!("{}/v1/lines", self.api_url);
        let response = self.send(self.http.get(url)).await?;
        let list: List<PlowLine> = response.json().await.map_err(|_| bad_answer())?;
        Ok(list.data)
    }

    /// Creates an agent on `line_uid`, run by `provider` (`self_hosted`:
    /// its owner). Plow answers with it, and with its credential for an own
    /// agent.
    pub async fn create_agent(
        &self,
        line_uid: &str,
        name: &str,
        provider: &str,
    ) -> Result<CreatedAgent, Failure> {
        let url = format!("{}/v1/agents", self.api_url);
        let body = serde_json::json!({"line_uid": line_uid, "name": name, "provider": provider});
        let response = self.send(self.http.post(url).json(&body)).await?;
        response.json().await.map_err(|_| bad_answer())
    }

    /// Removes an agent; Plow answers with what it was.
    pub async fn delete_agent(&self, agent_uid: &str) -> Result<ListedAgent, Failure> {
        let url = format!("{}/v1/agents/{agent_uid}", self.api_url);
        let response = self.send(self.http.delete(url)).await?;
        response.json().await.map_err(|_| bad_answer())
    }

    /// Every live agent in the account, in Plow's (creation) order, as
    /// `T` reads them. Owner logins only.
    pub async fn agent_list<T: serde::de::DeserializeOwned>(&self) -> Result<Vec<T>, Failure> {
        let url = format!("{}/v1/agents", self.api_url);
        let response = self.send(self.http.get(url)).await?;
        response.json().await.map_err(|_| bad_answer())
    }

    /// Every chat this login may see, oldest first.
    pub async fn chats(&self) -> Result<Vec<Chat>, Failure> {
        let url = format!("{}/v1/chats", self.api_url);
        let response = self.send(self.http.get(url)).await?;
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
        let request = self.http.get(url);
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
        let response = self.send(request).await.map_err(|failure| {
            // Every account needs signing in again: said as such, by name.
            match failure
                .plow
                .as_ref()
                .and_then(|said| said.needs_sign_in.clone())
            {
                Some(accounts) => crate::google::needs_sign_in(accounts),
                // Plow's answer when the owner has no Google account connected.
                None if failure.code == code::precondition_failed.name => {
                    crate::google::not_connected()
                }
                None => failure,
            }
        })?;
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
            Err(err) if err.is_timeout() => return Err(no_answer(&self.api_url, ANSWER_WITHIN)),
            Err(err) => {
                let message = format!("could not reach Plow at {}: {err}", self.api_url);
                return Err(Failure::new(code::plow_unreachable, message));
            }
        };
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let (code, meaning) = match status.as_u16() {
            401 | 403 => (code::plow_denied, "this credential may not do that"),
            409 => (
                code::conflict,
                "that name, key or agent is taken by another peer",
            ),
            404 => (code::not_found, "Plow knows no such peer"),
            410 => (code::expired, "that has expired"),
            422 => (code::invalid_request, "Plow rejected the request's fields"),
            // Plow's answer when the account lacks what the request needs,
            // such as a connected Google account.
            412 => (
                code::precondition_failed,
                "Plow needs something set up first",
            ),
            429 => (code::rate_limited, "Plow asked us to slow down"),
            400..=499 => (code::plow_rejected, "Plow refused the request"),
            _ => (code::plow_error, "Plow could not answer"),
        };
        let message = format!("Plow answered HTTP {}: {meaning}", status.as_u16());
        let mut failure = Failure::new(code, message);
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(retry_after);
        let detail = response
            .json::<serde_json::Value>()
            .await
            .map(|mut body| body["detail"].take())
            .unwrap_or_default();
        let code = detail["code"].as_str().map(Box::from);
        let needs_sign_in =
            (detail["reason"] == "needs_reauth").then(|| crate::google::sign_in_accounts(&detail));
        failure.plow = Some(Box::new(crate::failure::PlowSaid {
            retry_after,
            code,
            needs_sign_in,
        }));
        Err(failure)
    }
}

/// Whether Plow refused the request itself: a 4xx answer other than 429.
/// Sending the same request again gets the same answer.
pub fn refused(failure: &Failure) -> bool {
    matches!(
        failure.code.as_str(),
        "plow_denied"
            | "conflict"
            | "not_found"
            | "expired"
            | "invalid_request"
            | "precondition_failed"
            | "plow_rejected"
    )
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
    Failure::new(
        code::plow_error,
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
