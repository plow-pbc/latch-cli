//! A synthetic Plow for tests: the account's one agent and its peers, who
//! a caller's key is, and a decision route answering by a rule; and a
//! synthetic thread reader. No real message is ever read.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

pub const AGENT: &str = "0123456789abcdef0123456789abcdef";
pub const LINE: &str = "line_agent";

/// The probabilities of COVERED, NOT_COVERED and FORBIDDEN for a state,
/// or none for a decision route that fails: COVERED answers whether the
/// cited intent covers the action, FORBIDDEN whether the device rule
/// denies it.
pub type Decide = fn(&Value) -> Option<[f64; 3]>;

/// What the synthetic Plow holds: who each caller's key is, and how the
/// decision route answers.
#[derive(Clone, Default)]
pub struct World {
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
    fn answer(&mut self, method: &str, target: &str, body: &[u8]) -> (u16, Value) {
        let path = target.split_once('?').map_or(target, |(path, _)| path);
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
        if method == "POST" && path == "/v1/decisions" {
            let asked: Value = serde_json::from_slice(body).unwrap();
            self.asked.push(asked["state"].clone());
            let Some([c, _, f]) = self.decide.and_then(|decide| decide(&asked["state"])) else {
                return (503, json!({"detail": "unavailable"}));
            };
            // COVERED is the intent's yes, FORBIDDEN the device rule's.
            let answers: serde_json::Map<String, Value> = asked["questions"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(id, question)| {
                    let p = if id == "denied" { f } else { c };
                    // A choice (`why`) answers its first option.
                    let answer = match question["criteria"].as_object() {
                        Some(options) => {
                            let first = options.keys().next().cloned().unwrap_or_default();
                            let probabilities: serde_json::Map<String, Value> = options
                                .keys()
                                .map(|o| (o.clone(), json!(f64::from(u8::from(*o == first)))))
                                .collect();
                            json!({"type": "choice", "choice": first, "probabilities": probabilities})
                        }
                        None => json!({"type": "noul", "noul": p}),
                    };
                    (id.clone(), answer)
                })
                .collect();
            return (
                200,
                json!({"id": "req_fake", "model": asked["model"], "answers": answers,
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
        (404, json!({"detail": "not found"}))
    }
}

/// A running synthetic Plow: its URL, its world (which a test may change
/// between calls), and the requests it saw, as "METHOD /path?query".
#[derive(Clone)]
pub struct FakePlow {
    pub url: String,
    pub world: Arc<Mutex<World>>,
    pub seen: Arc<Mutex<Vec<String>>>,
}

impl FakePlow {
    pub fn client(&self) -> crate::plow::Plow {
        crate::plow::Plow::new(&self.url, "made-up-login").unwrap()
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
    let (shared, log) = (world.clone(), seen.clone());
    std::thread::spawn(move || {
        for socket in listener.incoming() {
            let mut socket = socket.unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
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
    FakePlow { url, world, seen }
}

/// The citation tests' agents send: a message in the agent's own DM.
pub const CITED: &str = "!dm:test/$asked";
/// The Swarm account the test agent is mapped to.
pub const AGENT_SWARM: &str = "@agent:test";

/// A thread reader answering every citation with `read`: by default the
/// owner's `asked`, in the agent's own DM, nothing after it.
pub struct Reader(pub Result<Option<crate::gate::thread::Thread>, String>);

impl Reader {
    pub fn asked(text: &str) -> Reader {
        Reader(Ok(Some(crate::gate::thread::Thread {
            room: "!dm:test".into(),
            event: "$asked".into(),
            cited: crate::gate::thread::Said {
                text: text.into(),
                at: Timestamp::now() - SignedDuration::from_mins(30),
            },
            sender_is_owner: true,
            later_owner: vec![],
            context: vec![],
            is_owner_dm: true,
            closed: false,
            redacted: false,
            complete: true,
        })))
    }
}

impl crate::gate::thread::ThreadSource for Reader {
    fn read<'a>(
        &'a self,
        _intent: &'a crate::gate::thread::IntentRef,
        _agent: &'a str,
    ) -> crate::gate::thread::Reading<'a> {
        let read = self.0.clone();
        Box::pin(async move { read })
    }
}
