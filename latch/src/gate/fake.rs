//! A synthetic Plow for tests: the account's one agent and its peers, who
//! a caller's key is, the agent's line, its chats and their messages, and a
//! decision route answering by a rule.
//! No real chat is ever read.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

pub const AGENT: &str = "0123456789abcdef0123456789abcdef";
pub const LINE: &str = "line_agent";

/// The probabilities of COVERED, NOT_COVERED and FORBIDDEN for a state,
/// or none for a decision route that fails.
pub type Decide = fn(&Value) -> Option<[f64; 3]>;

/// What the synthetic Plow holds: each chat's line and messages (oldest
/// first), who each caller's key is, and how the decision route answers.
#[derive(Clone, Default)]
pub struct World {
    pub chats: Vec<(String, String)>,
    pub messages: HashMap<String, Vec<Value>>,
    /// A path answered with HTTP 500.
    pub broken: Option<String>,
    /// Holds a path's answer until the test releases it.
    pub paused: Option<(String, Arc<std::sync::atomic::AtomicBool>)>,
    /// `GET /v1/latch/peers/{key}`'s answer, by key.
    pub callers: HashMap<String, Value>,
    pub decide: Option<Decide>,
    /// Every state the decision route was asked about, in order.
    pub asked: Vec<Value>,
    /// The owner's connected Google accounts, each given a made-up token.
    pub google_accounts: Vec<String>,
    /// Those of them Google wants the owner to sign in with again: Plow
    /// mints them no token.
    pub google_reauth: Vec<String>,
    /// The Google token route holds its answer while this is set.
    pub google_paused: bool,
}

impl World {
    pub fn with_chat(chat: &str, messages: Vec<Value>) -> World {
        let mut world = World::default();
        world.add(chat, LINE, messages);
        world
    }

    pub fn add(&mut self, chat: &str, line: &str, messages: Vec<Value>) {
        self.chats.push((chat.into(), line.into()));
        self.messages.insert(chat.into(), messages);
    }

    fn answer(&mut self, method: &str, target: &str, body: &[u8]) -> (u16, Value) {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        if let Some((paused, released)) = &self.paused
            && paused == path
        {
            while !released.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        if self.broken.as_deref() == Some(path) {
            return (500, json!({"detail": "broken"}));
        }
        let query: HashMap<String, String> = url::form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
        if method == "POST" && path == "/v1/decisions" {
            let asked: Value = serde_json::from_slice(body).unwrap();
            self.asked.push(asked["state"].clone());
            let Some([c, n, f]) = self.decide.and_then(|decide| decide(&asked["state"])) else {
                return (503, json!({"detail": "unavailable"}));
            };
            let top = if c >= n && c >= f {
                "COVERED"
            } else if n >= f {
                "NOT_COVERED"
            } else {
                "FORBIDDEN"
            };
            return (
                200,
                json!({"id": "req_fake", "model": asked["model"],
                       "answers": {"decision": {"type": "choice", "choice": top,
                           "probabilities": {"COVERED": c, "NOT_COVERED": n, "FORBIDDEN": f}}},
                       "usage": {"input_tokens": 1000, "output_tokens": 0, "cost": 0.0001}}),
            );
        }
        if method == "POST" && path == "/v1/connectors/gmail/access-token" {
            // Plow's answer with no Google account connected.
            if self.google_accounts.is_empty() {
                return (412, json!({"detail": "No Gmail account connected."}));
            }
            let (again, working): (Vec<&String>, Vec<&String>) = self
                .google_accounts
                .iter()
                .partition(|account| self.google_reauth.contains(account));
            if working.is_empty() {
                return (
                    401,
                    json!({"detail": {"reason": "needs_reauth", "accounts": again,
                                      "message": "Reconnect to grant access again."}}),
                );
            }
            let accounts: Vec<Value> = working
                .iter()
                .map(|account| {
                    json!({"account": account, "access_token": "made-up-token",
                           "capabilities": {"mail_read": true, "calendar_read": true}})
                })
                .collect();
            let degraded: Vec<Value> = again
                .iter()
                .map(|account| json!({"account": account, "reason": "needs_reauth"}))
                .collect();
            return (
                200,
                json!({"data": {"accounts": accounts, "degraded": degraded}}),
            );
        }
        if let Some(identity) = path
            .strip_prefix("/v1/latch/peers/")
            .and_then(|key| self.callers.get(key))
        {
            return (200, identity.clone());
        }
        if method == "GET" && path == "/v1/connectors" {
            let accounts: Vec<Value> = self
                .google_accounts
                .iter()
                .map(|account| json!({"account": account}))
                .collect();
            return (
                200,
                json!({"gmail": {"connected": !accounts.is_empty(), "accounts": accounts},
                       "web_base_url": "https://app.example.com/"}),
            );
        }
        if path == "/v1/agents" {
            return (
                200,
                json!([{"uid": AGENT, "name": "Juno", "provider": "exe:juno",
                        "line": {"uid": LINE, "provider_type": "imessage",
                                 "provider_key": "+14155550100", "display_name": null},
                        "created_at": "2026-10-20T10:00:00Z", "status": "running",
                        "failure_code": null, "credential": null}]),
            );
        }
        if path == "/v1/latch/peers" {
            let peers: Vec<Value> = self
                .callers
                .values()
                .map(|identity| {
                    json!({"key": identity["key"], "kind": identity["kind"],
                    "name": identity["name"], "relay_url": null, "last_seen_at": null,
                    "online": false})
                })
                .collect();
            return (200, json!({"data": peers}));
        }
        if path == format!("/v1/agents/{AGENT}") {
            return (
                200,
                json!({"uid": AGENT, "name": "Juno", "provider": "exe",
                       "line": {"uid": LINE, "provider_type": "imessage", "provider_key": "+14155550100"}}),
            );
        }
        if path == "/v1/chats" {
            let data: Vec<Value> = self
                .chats
                .iter()
                .map(|(chat, line)| json!({"uid": chat, "object": "chat", "status": "active", "trusted": true,
                    "participants": [
                        {"type": "agent", "relationship": "self",
                         "line": {"uid": line, "provider_type": "imessage", "provider_key": "+14155550100"}},
                        {"type": "member", "uid": "cp_owner", "object": "chat_participant", "status": "active",
                         "display_name": "Morgan", "role": "owner", "provider_type": "imessage",
                         "provider_key": "+14155550199"},
                    ]}))
                .collect();
            return (
                200,
                json!({"object": "list", "data": data, "has_more": false, "url": "/v1/chats"}),
            );
        }
        let chat = path
            .strip_prefix("/v1/chats/")
            .and_then(|rest| rest.strip_suffix("/messages"));
        if let Some(messages) = chat.and_then(|chat| self.messages.get(chat)) {
            assert_eq!(query["format"], "text_decorations");
            let limit: usize = query["limit"].parse().unwrap();
            let newest_first: Vec<&Value> = messages.iter().rev().collect();
            let start = match query.get("starting_after") {
                Some(uid) => newest_first.iter().position(|m| m["uid"] == **uid).unwrap() + 1,
                None => 0,
            };
            let page: Vec<&Value> = newest_first
                .iter()
                .skip(start)
                .take(limit)
                .copied()
                .collect();
            let has_more = start + page.len() < newest_first.len();
            return (
                200,
                json!({"object": "list", "data": page, "has_more": has_more, "url": path}),
            );
        }
        (404, json!({"detail": "not found"}))
    }
}

/// A running synthetic Plow: its URL, its world (which a test may change
/// between calls), the requests it saw, as "METHOD /path?query", and each
/// request's `Authorization` headers, in the same order.
#[derive(Clone)]
pub struct FakePlow {
    pub url: String,
    pub world: Arc<Mutex<World>>,
    pub seen: Arc<Mutex<Vec<String>>>,
    pub authorizations: Arc<Mutex<Vec<Vec<String>>>>,
}

impl FakePlow {
    pub fn client(&self) -> crate::plow::Plow {
        crate::plow::Plow::new(&self.url, "made-up-login")
    }

    /// How many requests it saw whose line contains `part`.
    pub fn saw(&self, part: &str) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.contains(part))
            .count()
    }
}

pub fn serve(world: World) -> FakePlow {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let world = Arc::new(Mutex::new(world));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let authorizations = Arc::new(Mutex::new(Vec::new()));
    let (shared, log, auth_log) = (world.clone(), seen.clone(), authorizations.clone());
    std::thread::spawn(move || {
        for socket in listener.incoming() {
            let mut socket = socket.unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let mut length = 0;
            let mut authorization = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("authorization")
                {
                    authorization.push(value.trim().to_string());
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let mut words = first.split(' ');
            let (method, target) = (words.next().unwrap(), words.next().unwrap());
            log.lock().unwrap().push(format!("{method} {target}"));
            auth_log.lock().unwrap().push(authorization);
            while target.contains("access-token") && shared.lock().unwrap().google_paused {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let (status, answer) = shared.lock().unwrap().answer(method, target, &body);
            let answer = answer.to_string();
            let _ = write!(
                socket,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                answer.len()
            );
        }
    });
    FakePlow {
        url,
        world,
        seen,
        authorizations,
    }
}

pub fn message(uid: &str, sender: Value, body: &str, minutes_ago: i64) -> Value {
    let at = Timestamp::now() - SignedDuration::from_mins(minutes_ago);
    json!({"uid": uid, "object": "message", "chat_uid": "chat_a", "direction": "inbound",
           "body": body, "mentions": null, "effect": null, "status": "received",
           "sender": sender, "reactions": [], "attachments": [], "created_at": at.to_string()})
}

pub fn owner() -> Value {
    json!({"type": "member", "uid": "cp_owner", "display_name": "Morgan", "role": "owner",
           "provider_key": "+14155550199"})
}

pub fn member(name: &str) -> Value {
    json!({"type": "member", "uid": "cp_x", "display_name": name, "role": "member",
           "provider_key": "+14155550142"})
}

pub fn agent_self() -> Value {
    json!({"type": "agent", "relationship": "self",
           "line": {"uid": LINE, "provider_type": "imessage", "provider_key": "+14155550100"}})
}
