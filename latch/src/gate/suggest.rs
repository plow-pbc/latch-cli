//! `latch audit suggest RECORD`: the device-rule edit that would rule one
//! of the gatekeeper's judgments the other way next time, checked with the
//! judge before it is offered.
//!
//! Templates only, each judged as the owner's own call against the rule as
//! it is now (`Gatekeeper::preview`): an allowed request, the rule and
//! "Never …"; one the rule denied, the rule without each of its first three
//! sentences, the rule and "You may …", then no rule when it is one
//! sentence. The first that flips the ruling is the suggestion. A request
//! another check found a problem with can't be let through by a rule,
//! which only denies: the answer is to ask for it in the message.

use std::path::Path;

use serde_json::value::RawValue;
use serde_json::{Value, json};

use super::swarm::SwarmReader;
use super::{CLEAR_P, Gatekeeper, requests, rules};
use crate::failure::{Failure, code};

/// The suggestion for the record `id` on this machine's host, judged with
/// its own login.
pub async fn run(state_dir: &Path, id: &str) -> Result<Value, Failure> {
    let record = crate::audit::find(state_dir, id)?.ok_or_else(|| {
        Failure::new(
            code::no_such_record,
            format!("this host's audit has no record {id}"),
        )
    })?;
    let gate = judgment(&record);
    // The action exactly as the judge was shown it, from the call the host
    // kept (7 days).
    let action = gate["kept_request"]
        .as_str()
        .and_then(|kept| requests::read(state_dir, kept, false).ok())
        .map(|kept| kept["request"]["state"]["proposed_action"].clone())
        .filter(|action| !action.is_null())
        .map(|action| serde_json::value::to_raw_value(&action).expect("an action serializes"));
    let summary = crate::dashboard::recorded_command(&record).map(|command| summary(&command));
    if !matches!(verdict(&gate), Some(Some(_))) || action.is_none() || summary.is_none() {
        // Asked nothing: no login needed for what no rule can change.
        return Ok(answer(verdict(&gate), None));
    }
    let provider = Gatekeeper::provider_for_host(state_dir)
        .map_err(|why| Failure::new(code::not_logged_in, why))?;
    let keeper = Gatekeeper::new(provider, Box::new(SwarmReader::new(state_dir)), state_dir);
    keeper
        .suggestion(&gate, summary.as_deref(), action.as_deref())
        .await
}

/// The judgment that decided `record`: the step a plan's gatekeeper
/// refused (a fill judged on its own) before the plan's own, a browser
/// step's in its args, else the record's.
pub(super) fn judgment(record: &crate::audit::Record) -> Value {
    let refused_step = record.args["step_records"].as_array().and_then(|steps| {
        steps
            .iter()
            .rev()
            .find(|step| step["dispatch"] == "refused" && step["gate"].is_object())
    });
    if let Some(step) = refused_step {
        return step["gate"].clone();
    }
    record
        .gate
        .clone()
        .filter(|gate| !gate["verdict"].is_null())
        .unwrap_or_else(|| record.args["gate"].clone())
}

/// How a judgment can be turned: `None` for a request the exfiltration or
/// chat check refused (ask in the message), `Some(None)` for one no rule
/// edit turns, else `Some(Some(refused))`.
pub(super) fn verdict(gate: &Value) -> Option<Option<bool>> {
    let found = ["exfiltration", "unconnected"]
        .iter()
        .any(|check| gate["p"][check].as_f64().is_some_and(|p| p >= CLEAR_P));
    match gate["verdict"].as_str() {
        Some("exfiltration" | "unconnected") => None,
        _ if found => None,
        Some("allow") => Some(Some(false)),
        Some("denied_by_rule") => Some(Some(true)),
        _ => Some(None),
    }
}

/// `suggestion`: `rule` (with `rule`, the edit, and `revision`, the rule
/// it edits, null when none), `ask_in_message`, or null.
pub(super) fn answer(
    verdict: Option<Option<bool>>,
    rule: Option<(String, Option<String>)>,
) -> Value {
    match (verdict, rule) {
        (None, _) => json!({"suggestion": "ask_in_message"}),
        (_, Some((rule, revision))) => {
            json!({"suggestion": "rule", "rule": rule, "revision": revision})
        }
        _ => json!({"suggestion": null}),
    }
}

impl Gatekeeper {
    /// The checked edit of this host's rule that would rule `action` the
    /// other way: offered only while the rule now still rules as `gate`
    /// did, and only an edit the judge says flips it.
    pub async fn suggestion(
        &self,
        gate: &Value,
        summary: Option<&str>,
        action: Option<&RawValue>,
    ) -> Result<Value, Failure> {
        let verdict = verdict(gate);
        let (Some(Some(refused)), Some(summary), Some(action)) = (verdict, summary, action) else {
            return Ok(answer(verdict, None));
        };
        let stored = self
            .store
            .deny()
            .map_err(|why| Failure::new(code::read_failed, why))?;
        // The revision `rules show` gives: a blank rule saved has one too.
        let revision = stored.as_deref().map(rules::revision);
        let rule = stored.unwrap_or_default();
        if self.preview(&rule, None, action).await?.denied != refused {
            return Ok(answer(verdict, None));
        }
        for candidate in candidates(&rule, summary, refused) {
            if self.preview(&candidate, None, action).await?.denied != refused {
                return Ok(answer(verdict, Some((candidate, revision))));
            }
        }
        Ok(answer(verdict, None))
    }
}

/// A request in a few words, for a rule to name: `latch exec ls` is
/// "run `ls`", any other command "use `…`" without its `latch`.
fn summary(command: &str) -> String {
    match command.strip_prefix("latch exec ") {
        Some(line) => format!("run `{line}`"),
        None => format!(
            "use `{}`",
            command.strip_prefix("latch ").unwrap_or(command)
        ),
    }
}

/// The edits to try, in order.
pub(super) fn candidates(rule: &str, summary: &str, refused: bool) -> Vec<String> {
    let rule = rule.trim();
    let gap = if rule.contains('\n') { "\n" } else { " " };
    let adding = |sentence: String| {
        if rule.is_empty() {
            sentence
        } else {
            format!("{rule}{gap}{sentence}")
        }
    };
    if !refused {
        return vec![adding(format!("Never {summary}."))];
    }
    let sentences = sentences(rule);
    let mut tried: Vec<String> = Vec::new();
    if sentences.len() > 1 {
        for cut in 0..sentences.len().min(3) {
            let kept: Vec<&str> = sentences
                .iter()
                .enumerate()
                .filter(|(at, _)| *at != cut)
                .map(|(_, sentence)| sentence.as_str())
                .collect();
            tried.push(kept.join(gap));
        }
    }
    tried.push(adding(format!("You may {summary}.")));
    if sentences.len() == 1 {
        tried.push(String::new());
    }
    tried
}

/// A rule's sentences: each ends at `.`, `!` or `?` followed by a space or
/// a new line, or at a new line.
fn sentences(rule: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    let mut chars = rule.chars().peekable();
    while let Some(c) = chars.next() {
        current.push(c);
        let ends = c == '\n'
            || (matches!(c, '.' | '!' | '?')
                && chars.peek().is_none_or(|next| next.is_whitespace()));
        if ends {
            let sentence = current.trim().to_string();
            if !sentence.is_empty() {
                sentences.push(sentence);
            }
            current.clear();
        }
    }
    let rest = current.trim();
    if !rest.is_empty() {
        sentences.push(rest.to_string());
    }
    sentences
}
