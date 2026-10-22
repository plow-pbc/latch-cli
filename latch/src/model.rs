//! The decision model: a model that only chooses. It picks one option from
//! a list, or answers yes or no with a probability; it never writes text.
//! The gatekeeper and the browser both ask it, always from the host.
//!
//! A [`Provider`] is one way of reaching a model: [`Plow`], the host's
//! own Plow login asking Plow's decision route (which calls Jev on
//! OpenRouter), or, only when a host is set up for development, [`Jev`]
//! called directly with a key in a 0600 file. Nothing here bakes a key in. Every provider's answers pass the same checks before use
//! ([`Model::ask`]): a choice must be an offered option, and probabilities
//! must be finite, cover exactly the options and sum to 1. A choice below
//! its top probability gives way to the top option, and the audit keeps
//! the option ids and probabilities of that answer.
//!
//! With no provider, or one that fails, [`Model::ask`] answers why, and
//! whatever needed the decision stops before anything is sent.

use std::collections::HashMap;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::pin::Pin;
use std::time::{Duration, Instant};

use serde_json::value::RawValue;
use serde_json::{Map, Value, json};

/// One question in a call. Several go in one call; each is answered on
/// its own, so each states everything it assumes.
pub struct Question {
    pub id: String,
    pub instructions: Value,
    pub kind: Kind,
}

pub enum Kind {
    /// One of these options: (option, what it is).
    Choose(Vec<(String, String)>),
    YesNo,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Chose {
        choice: String,
        probabilities: Vec<(String, f64)>,
    },
    Yes(f64),
}

/// What a provider reports about one call, for the audit: never content.
#[derive(Debug, Clone)]
pub struct CallMeta {
    pub provider: &'static str,
    pub model: String,
    pub request_id: Option<String>,
    pub latency_ms: u64,
    /// What the call cost in US dollars, when the provider says.
    pub cost: Option<f64>,
}

/// Why a call failed: a short code the audit and the journal keep, such as
/// `out_of_credits` or `http_503`, and a sentence for the agent. Neither
/// holds a key, a credential or what was asked.
#[derive(Debug, Clone)]
pub struct CallFailed {
    pub reason: String,
    pub message: String,
}

impl CallFailed {
    pub fn new(reason: &str, message: impl Into<String>) -> CallFailed {
        CallFailed {
            reason: reason.to_string(),
            message: message.into(),
        }
    }
}

pub type Asked<'a> = Pin<
    Box<dyn Future<Output = Result<(HashMap<String, Answer>, CallMeta), CallFailed>> + Send + 'a>,
>;

/// One way of reaching a decision model.
pub trait Provider: Send + Sync {
    /// Asks every question in one call, about `state`. The answers are
    /// checked by [`Model::ask`], not here.
    fn ask<'a>(&'a self, state: &'a Value, questions: &'a [Question]) -> Asked<'a>;

    /// Like [`Provider::ask`], with the state sent exactly as this JSON
    /// text: the model reads a state differently when its keys come in
    /// another order, and a `Value` sorts them.
    fn ask_as_written<'a>(&'a self, state: &'a RawValue, questions: &'a [Question]) -> Asked<'a> {
        Box::pin(async move {
            let state: Value = serde_json::from_str(state.get())
                .map_err(|e| CallFailed::new("bad_state", e.to_string()))?;
            self.ask(&state, questions).await
        })
    }

    /// The provider's name for the audit, even of a call that failed.
    fn name(&self) -> &'static str {
        ""
    }
}

/// A shared provider is a provider: one per host, one `Model` per request.
impl<P: Provider + ?Sized> Provider for std::sync::Arc<P> {
    fn ask<'a>(&'a self, state: &'a Value, questions: &'a [Question]) -> Asked<'a> {
        (**self).ask(state, questions)
    }

    fn ask_as_written<'a>(&'a self, state: &'a RawValue, questions: &'a [Question]) -> Asked<'a> {
        (**self).ask_as_written(state, questions)
    }

    fn name(&self) -> &'static str {
        (**self).name()
    }
}

/// The decision model a request uses, and the audit's record of every call
/// it made: purpose, provider, model, its request id, latency and the kinds
/// of data sent.
pub struct Model {
    provider: Option<Box<dyn Provider>>,
    /// Why there is no provider, for `model_unavailable`'s message.
    off: String,
    /// The same as a short code, for the audit and the journal.
    reason: Option<String>,
    pub calls: Vec<Value>,
}

impl Model {
    pub fn new(provider: Box<dyn Provider>) -> Model {
        Model {
            provider: Some(provider),
            off: String::new(),
            reason: None,
            calls: Vec::new(),
        }
    }

    pub fn off(why: &str) -> Model {
        Model {
            provider: None,
            off: why.to_string(),
            reason: Some("no_model".to_string()),
            calls: Vec::new(),
        }
    }

    /// The host's model: see [`host_provider`].
    pub fn for_host(state_dir: &Path) -> Model {
        match host_provider(state_dir, PLOW_MODEL, JEV_MODEL) {
            Ok(provider) => Model::new(provider),
            Err(why) => Model::off(&why),
        }
    }

    pub fn is_on(&self) -> bool {
        self.provider.is_some()
    }

    /// Why the model is not available.
    pub fn why_off(&self) -> &str {
        &self.off
    }

    /// Why the model is not available, as a short code: a failed call's
    /// reason, or `no_model` when the host has none.
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// One call, its answers checked against its questions. `purpose` and
    /// `sent` (the kinds of data in `state`) go to the audit. A provider
    /// that fails or answers badly is off for the rest of the request.
    pub async fn ask(
        &mut self,
        purpose: &str,
        sent: &[&str],
        state: Value,
        questions: Vec<Question>,
    ) -> Result<HashMap<String, Answer>, String> {
        let state = serde_json::value::to_raw_value(&state).expect("a JSON value serializes");
        self.ask_as_written(purpose, sent, state, questions).await
    }

    /// [`Model::ask`], with the state sent exactly as this JSON text.
    pub async fn ask_as_written(
        &mut self,
        purpose: &str,
        sent: &[&str],
        state: Box<RawValue>,
        questions: Vec<Question>,
    ) -> Result<HashMap<String, Answer>, String> {
        let Some(provider) = &self.provider else {
            return Err(self.off.clone());
        };
        let started = Instant::now();
        let asked = provider.ask_as_written(&state, &questions).await;
        let mut record = json!({"purpose": purpose, "sent": sent});
        if !provider.name().is_empty() {
            record["provider"] = json!(provider.name());
        }
        let outcome = asked.and_then(|(mut answers, meta)| {
            record["provider"] = json!(meta.provider);
            record["model"] = json!(meta.model);
            record["model_request"] = json!(meta.request_id);
            record["latency_ms"] = json!(meta.latency_ms);
            if let Some(cost) = meta.cost {
                record["cost"] = json!(cost);
            }
            let corrected = check(&questions, &mut answers)
                .map_err(|why| CallFailed::new("bad_answer", why))?;
            if !corrected.is_empty() {
                record["corrected"] = json!(corrected);
            }
            Ok(answers)
        });
        if record.get("latency_ms").is_none() {
            record["latency_ms"] = json!(started.elapsed().as_millis() as u64);
        }
        record["outcome"] = json!(if outcome.is_ok() { "ok" } else { "failed" });
        if let Err(failed) = &outcome {
            record["reason"] = json!(failed.reason);
        }
        self.calls.push(record);
        outcome.map_err(|failed| {
            self.provider = None;
            self.off = format!("the decision model failed: {}", failed.message);
            self.reason = Some(failed.reason);
            failed.message
        })
    }
}

/// The most bytes one decision request may take: Plow's decision route
/// takes request bodies up to 64 KiB, and this leaves room to spare.
pub const REQUEST_BUDGET: usize = 60 * 1024;

/// A request's size as a provider sends it: its state and questions.
pub fn request_size(state: &Value, questions: &[Question]) -> usize {
    let body = json!({"model": PLOW_MODEL, "state": state, "questions": system_one(questions)});
    serde_json::to_vec(&body).map_or(usize::MAX, |bytes| bytes.len())
}

/// Every question has an answer of its kind, and every answer is sound. A
/// choice below its top probability is replaced by the first offered option
/// at the top; each replacement is returned for the audit, as option ids and
/// probabilities only.
fn check(
    questions: &[Question],
    answers: &mut HashMap<String, Answer>,
) -> Result<Vec<Value>, String> {
    let mut corrected = Vec::new();
    let sound = |p: f64| p.is_finite() && (0.0..=1.0).contains(&p);
    for question in questions {
        let id = &question.id;
        match (&question.kind, answers.get_mut(id)) {
            (Kind::YesNo, Some(Answer::Yes(p))) => {
                if !sound(*p) {
                    return Err(format!("{id}: the probability is not in [0, 1]"));
                }
            }
            (
                Kind::Choose(options),
                Some(Answer::Chose {
                    choice,
                    probabilities,
                }),
            ) => {
                let offered = |o: &str| options.iter().any(|(option, _)| option == o);
                if !offered(choice.as_str()) {
                    return Err(format!("{id}: the choice was not offered"));
                }
                if probabilities.len() != options.len()
                    || !probabilities.iter().all(|(o, _)| offered(o))
                    || !options
                        .iter()
                        .all(|(o, _)| probabilities.iter().any(|(p, _)| p == o))
                {
                    return Err(format!("{id}: the probabilities do not match the options"));
                }
                if !probabilities.iter().all(|(_, p)| sound(*p)) {
                    return Err(format!("{id}: a probability is not in [0, 1]"));
                }
                let sum: f64 = probabilities.iter().map(|(_, p)| p).sum();
                if (sum - 1.0).abs() > 0.02 {
                    return Err(format!("{id}: the probabilities do not sum to 1"));
                }
                let top = probabilities.iter().map(|(_, p)| *p).fold(0.0, f64::max);
                let chosen = probabilities
                    .iter()
                    .find(|(o, _)| o == choice)
                    .map_or(0.0, |(_, p)| *p);
                if chosen < top - 1e-9 {
                    let used = options
                        .iter()
                        .map(|(o, _)| o)
                        .find(|o| {
                            probabilities
                                .iter()
                                .any(|(p, v)| p == *o && *v >= top - 1e-9)
                        })
                        .expect("an option holds the top probability")
                        .clone();
                    let by_option: Map<String, Value> = probabilities
                        .iter()
                        .map(|(o, p)| (o.clone(), json!(p)))
                        .collect();
                    corrected.push(json!({"question": id, "choice": choice, "used": used,
                                          "probabilities": by_option}));
                    *choice = used;
                }
            }
            _ => return Err(format!("{id}: no answer of the right kind")),
        }
    }
    Ok(corrected)
}

/// A host's provider: Plow's decision route on the host's own login,
/// asking `plow_model`; jev with the dev key file, asking `jev_model`, only
/// when `browser/model.json` says `{"provider": "jev"}`.
pub fn host_provider(
    state_dir: &Path,
    plow_model: &str,
    jev_model: &str,
) -> Result<Box<dyn Provider>, String> {
    let set = std::fs::read_to_string(state_dir.join(MODEL_FILE))
        .ok()
        .map(|text| serde_json::from_str::<Value>(&text));
    match set {
        None => Ok(Box::new(Plow::for_host(state_dir)?.with_model(plow_model))),
        Some(Ok(set)) if set["provider"] == "jev" => match Jev::from_state(state_dir)? {
            Some(jev) => Ok(Box::new(jev.with_model(jev_model))),
            None => Err(format!(
                "{MODEL_FILE} asks for jev, and there is no {JEV_KEY_FILE}"
            )),
        },
        Some(Ok(set)) if set["provider"] == "plow" => {
            Ok(Box::new(Plow::for_host(state_dir)?.with_model(plow_model)))
        }
        Some(_) => Err(format!(
            "{MODEL_FILE} must say {{\"provider\": \"plow\"}} or {{\"provider\": \"jev\"}}"
        )),
    }
}

/// Which decision model a host uses, in its state directory: Plow's route
/// without one.
pub const MODEL_FILE: &str = "browser/model.json";
/// The dev-only key file for jev, in the host's state directory.
pub const JEV_KEY_FILE: &str = "jev.key";

/// Sets `state` up as a host that asks jev itself, with the key file
/// `$LATCH_TEST_JEV_KEY` names: a key file is used only on a host set up
/// for it. For the tests that call the decision model for real.
#[cfg(all(test, feature = "live-model"))]
pub fn ask_jev_for_tests(state: &Path) {
    let key = std::env::var_os("LATCH_TEST_JEV_KEY")
        .expect("LATCH_TEST_JEV_KEY names the jev key file for this test");
    let path = state.join(JEV_KEY_FILE);
    std::fs::copy(key, &path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::create_dir_all(state.join("browser")).unwrap();
    std::fs::write(state.join(MODEL_FILE), r#"{"provider": "jev"}"#).unwrap();
}
const JEV_URL: &str = "https://api.typesafe.ai/v1/systemone";
const JEV_MODEL: &str = "jev-1.13.0";

/// TypeSafe jev, pinned to one model version. Dev only: its key is a file
/// the host's owner puts in the state directory, never part of latch.
// No `Debug`: it holds the key.
pub struct Jev {
    http: reqwest::Client,
    key: String,
    model: String,
}

impl Jev {
    /// Jev, if its key file is present. A key file anyone else can read
    /// is refused.
    pub fn from_state(state_dir: &Path) -> Result<Option<Jev>, String> {
        let path = state_dir.join(JEV_KEY_FILE);
        let Ok(metadata) = std::fs::metadata(&path) else {
            return Ok(None);
        };
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "the decision model's key file {JEV_KEY_FILE} must be readable by its owner only (0600)"
            ));
        }
        let key = std::fs::read_to_string(&path)
            .map_err(|_| format!("the decision model's key file {JEV_KEY_FILE} is unreadable"))?
            .trim()
            .to_string();
        if key.is_empty() {
            return Ok(None);
        }
        Ok(Some(Jev {
            http: http(),
            key,
            model: JEV_MODEL.to_string(),
        }))
    }

    /// The same key, asking another jev version.
    pub fn with_model(self, model: &str) -> Jev {
        Jev {
            model: model.to_string(),
            ..self
        }
    }
}

/// A request body: the model, the state as written, and the questions.
#[derive(serde::Serialize)]
struct Body<'a> {
    model: &'a str,
    state: &'a RawValue,
    questions: Value,
}

impl<'a> Body<'a> {
    fn new(model: &'a str, state: &'a RawValue, questions: &[Question]) -> Body<'a> {
        Body {
            model,
            state,
            questions: system_one(questions),
        }
    }
}

/// Questions as System One takes them: `noul` for yes/no, `choice` with
/// its options as criteria.
pub fn system_one(questions: &[Question]) -> Value {
    let mut asked = Map::new();
    for question in questions {
        let mut q = json!({"instructions": question.instructions});
        match &question.kind {
            Kind::YesNo => q["type"] = json!("noul"),
            Kind::Choose(options) => {
                q["type"] = json!("choice");
                let criteria: Map<String, Value> = options
                    .iter()
                    .map(|(option, what)| (option.clone(), json!(what)))
                    .collect();
                q["criteria"] = Value::Object(criteria);
            }
        }
        asked.insert(question.id.clone(), q);
    }
    Value::Object(asked)
}

/// System One's answers, read for each question; checking them is
/// [`Model::ask`]'s.
fn system_one_answers(reply: &Value, questions: &[Question]) -> HashMap<String, Answer> {
    let mut answers = HashMap::new();
    for question in questions {
        let answer = &reply["answers"][&question.id];
        let parsed = match &question.kind {
            Kind::YesNo => answer["noul"].as_f64().map(Answer::Yes),
            Kind::Choose(_) => answer["choice"].as_str().map(|choice| Answer::Chose {
                choice: choice.to_string(),
                probabilities: answer["probabilities"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(o, p)| (o.clone(), p.as_f64().unwrap_or(f64::NAN)))
                    .collect(),
            }),
        };
        if let Some(parsed) = parsed {
            answers.insert(question.id.clone(), parsed);
        }
    }
    answers
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("HTTP client settings are valid")
}

impl Provider for Jev {
    fn name(&self) -> &'static str {
        "typesafe"
    }

    fn ask<'a>(&'a self, state: &'a Value, questions: &'a [Question]) -> Asked<'a> {
        Box::pin(async move {
            let state = serde_json::value::to_raw_value(state)
                .map_err(|e| CallFailed::new("bad_state", e.to_string()))?;
            self.ask_as_written(&state, questions).await
        })
    }

    fn ask_as_written<'a>(&'a self, state: &'a RawValue, questions: &'a [Question]) -> Asked<'a> {
        Box::pin(async move {
            let body = Body::new(&self.model, state, questions);
            let started = Instant::now();
            let response = self
                .http
                .post(JEV_URL)
                .bearer_auth(&self.key)
                .json(&body)
                .send()
                .await
                .map_err(|_| CallFailed::new("unreachable", "jev could not be reached"))?;
            let latency_ms = started.elapsed().as_millis() as u64;
            let status = response.status();
            let request_id = response
                .headers()
                .get("x-typesafe-request-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            if !status.is_success() {
                // Jev's own error text is not passed on: it could echo the request.
                let code = status.as_u16();
                return Err(CallFailed::new(
                    &format!("http_{code}"),
                    format!("jev answered HTTP {code}"),
                ));
            }
            let reply: Value = response
                .json()
                .await
                .map_err(|_| CallFailed::new("not_json", "jev's answer was not JSON"))?;
            let answers = system_one_answers(&reply, questions);
            let meta = CallMeta {
                provider: "typesafe",
                model: reply["model"].as_str().unwrap_or(&self.model).to_string(),
                request_id,
                latency_ms,
                cost: reply["usage"]["cost"].as_f64(),
            };
            Ok((answers, meta))
        })
    }
}

/// Jev as Plow's decision route asks it on OpenRouter.
pub const PLOW_MODEL: &str = "typesafe/jev-1.13";
const PLOW_ROUTE: &str = "/v1/decisions";

/// Plow's decision route, on the host's own Plow login: the host holds no
/// model key. Plow asks Jev and passes its answers back.
// No `Debug`: it holds the login.
pub struct Plow {
    http: reqwest::Client,
    url: String,
    token: String,
    model: String,
}

impl Plow {
    pub fn new(api_url: &str, token: String) -> Plow {
        Plow {
            http: http(),
            url: format!("{}{PLOW_ROUTE}", api_url.trim_end_matches('/')),
            token,
            model: PLOW_MODEL.to_string(),
        }
    }

    /// The same route and login, asking another jev version.
    pub fn with_model(self, model: &str) -> Plow {
        Plow {
            model: model.to_string(),
            ..self
        }
    }

    /// The route on the Plow this host is logged in to, with its login.
    pub fn for_host(state_dir: &Path) -> Result<Plow, String> {
        let state = crate::state::StateDir {
            path: state_dir.to_path_buf(),
        };
        let not_in = || {
            "this host is not logged in to Plow, which answers its decision-model questions: \
             the host's owner must run `latch login` on it"
                .to_string()
        };
        let config = state.load_config().ok().flatten().ok_or_else(not_in)?;
        let token = state.load_token().ok().flatten().ok_or_else(not_in)?;
        Ok(Plow::new(&config.api_url, token))
    }
}

impl Provider for Plow {
    fn name(&self) -> &'static str {
        "plow"
    }

    fn ask<'a>(&'a self, state: &'a Value, questions: &'a [Question]) -> Asked<'a> {
        Box::pin(async move {
            let state = serde_json::value::to_raw_value(state)
                .map_err(|e| CallFailed::new("bad_state", e.to_string()))?;
            self.ask_as_written(&state, questions).await
        })
    }

    fn ask_as_written<'a>(&'a self, state: &'a RawValue, questions: &'a [Question]) -> Asked<'a> {
        Box::pin(async move {
            let body = Body::new(&self.model, state, questions);
            let started = Instant::now();
            let response = self
                .http
                .post(&self.url)
                .bearer_auth(&self.token)
                .json(&body)
                .send()
                .await
                .map_err(|_| {
                    CallFailed::new(
                        "unreachable",
                        "Plow's decision route could not be reached from this host; retry once \
                         its network and Plow are up",
                    )
                })?;
            let latency_ms = started.elapsed().as_millis() as u64;
            let status = response.status();
            if !status.is_success() {
                return Err(match status.as_u16() {
                    402 => CallFailed::new(
                        "out_of_credits",
                        "this host's Plow account is out of credits for the decision model: \
                         the host's account owner must add Plow credits, and until then every \
                         retry fails the same way",
                    ),
                    401 => CallFailed::new(
                        "http_401",
                        "Plow refused this host's login: the host's owner must log it in again \
                         with `latch login` on the host",
                    ),
                    403 => CallFailed::new(
                        "not_allowed",
                        "this host's Plow login may not ask the decision model: the host's \
                         owner must log it in again with `latch login` on the host, on an \
                         account that may",
                    ),
                    code => CallFailed::new(
                        &format!("http_{code}"),
                        format!("Plow's decision route answered HTTP {code}; retry later"),
                    ),
                });
            }
            let reply: Value = response.json().await.map_err(|_| {
                CallFailed::new(
                    "not_json",
                    "Plow's decision route did not answer JSON; retry later",
                )
            })?;
            let meta = CallMeta {
                provider: "plow",
                model: reply["model"].as_str().unwrap_or(&self.model).to_string(),
                request_id: reply["id"].as_str().map(str::to_string),
                latency_ms,
                cost: reply["usage"]["cost"].as_f64(),
            };
            Ok((system_one_answers(&reply, questions), meta))
        })
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// One yes-or-no question, as any caller asks it.
    async fn ask_one(m: &mut Model) -> Result<HashMap<String, Answer>, String> {
        let question = Question {
            id: "q".into(),
            instructions: json!({"question": "Is it so?"}),
            kind: Kind::YesNo,
        };
        m.ask("test", &[], json!({}), vec![question]).await
    }

    #[test]
    fn the_jev_key_file_is_used_only_when_the_host_is_set_up_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(JEV_KEY_FILE);
        std::fs::write(&path, "made-up-key").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        // A key file alone changes nothing: the model is Plow's, on the
        // host's login, and this host has none.
        let m = Model::for_host(dir.path());
        assert!(!m.is_on());
        assert!(
            m.why_off().contains("not logged in to Plow"),
            "{}",
            m.why_off()
        );
        std::fs::create_dir_all(dir.path().join("browser")).unwrap();
        std::fs::write(dir.path().join(MODEL_FILE), r#"{"provider": "jev"}"#).unwrap();
        assert!(Model::for_host(dir.path()).is_on());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let m = Model::for_host(dir.path());
        assert!(!m.is_on());
        assert!(m.why_off().contains("0600"));
        std::fs::write(dir.path().join(MODEL_FILE), r#"{"provider": "gpt"}"#).unwrap();
        assert!(!Model::for_host(dir.path()).is_on());
    }

    /// A stand-in for Plow's decision route: answers `reply` with `status`
    /// to one request, and hands back what it was sent.
    pub fn plow_route(status: u16, reply: Value) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (sent, seen) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut head = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
                head.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            sent.send(head + &String::from_utf8(body).unwrap()).unwrap();
            let body = reply.to_string();
            let mut socket = socket;
            write!(
                socket,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        (url, seen)
    }

    #[tokio::test]
    async fn a_plow_route_that_fails_or_cannot_be_reached_turns_the_model_off() {
        let (url, _seen) = plow_route(503, json!({"detail": "unavailable"}));
        let mut m = Model::new(Box::new(Plow::new(&url, "made-up-login".into())));
        assert!(ask_one(&mut m).await.is_err());
        assert!(m.why_off().contains("HTTP 503"), "{}", m.why_off());
        assert_eq!(m.reason(), Some("http_503"));
        assert_eq!(m.calls[0]["reason"], "http_503");
        for (status, says, reason) in [
            (
                402,
                "out of credits for the decision model: the host's account owner must add Plow credits",
                "out_of_credits",
            ),
            (401, "log it in again", "http_401"),
            (
                403,
                "may not ask the decision model: the host's owner must log it in again",
                "not_allowed",
            ),
        ] {
            let (url, _seen) = plow_route(status, json!({"detail": "made-up refusal"}));
            let mut m = Model::new(Box::new(Plow::new(&url, "made-up-login".into())));
            assert!(ask_one(&mut m).await.is_err());
            assert!(m.why_off().contains(says), "{}", m.why_off());
            assert_eq!(m.reason(), Some(reason));
            assert_eq!(m.calls[0]["reason"], reason, "the audit keeps why");
        }
        assert_eq!(
            m.calls[0]["provider"], "plow",
            "a failed call still names its provider"
        );
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let mut m = Model::new(Box::new(Plow::new(&url, "made-up-login".into())));
        assert!(ask_one(&mut m).await.is_err());
        assert!(
            m.why_off().contains("could not be reached"),
            "{}",
            m.why_off()
        );
        assert!(!m.why_off().contains("made-up-login"));
        assert_eq!(m.reason(), Some("unreachable"));
    }
}
