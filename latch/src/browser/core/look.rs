//! The worker's half of descriptions, expectations and step lists: the
//! readings the host needs to resolve a target or judge a sentence, the
//! waits for typed conditions, and the review of a click on a page the
//! agent hasn't seen. The decision model itself runs in the host.

use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{Action, Core, TEXT_CHARS, document_key};
use crate::browser::driver::Driver;
use crate::failure::{Exit, Failure};

/// The most page lines a judgement gets.
const MAX_LINES: usize = 120;
/// How often a wait looks again.
const POLL: Duration = Duration::from_millis(250);

/// A typed condition, decided by latch's own code.
#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Condition {
    /// The page's URL starts with `url`.
    Url { url: String },
    /// The page's text holds `text`.
    Text { text: String },
    /// A field holds `value`, or is `checked` / `unchecked`.
    Field {
        snapshot: Option<String>,
        target: String,
        value: String,
    },
    /// An element is `visible`, `hidden`, `enabled`, `disabled`,
    /// `checked`, `unchecked` or `gone`.
    Ref {
        snapshot: Option<String>,
        target: String,
        state: String,
    },
    /// A download not among `besides` has completed.
    Download { besides: Vec<String> },
}

const STATES: [&str; 7] = [
    "visible",
    "hidden",
    "enabled",
    "disabled",
    "checked",
    "unchecked",
    "gone",
];

/// Names compare whole, with case and runs of spaces ignored: never "contains".
fn same_name(a: &str, b: &str) -> bool {
    let flat = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    flat(a) == flat(b)
}

impl<D: Driver> Core<D> {
    pub(super) fn resolve(
        &mut self,
        action: Option<Action>,
        role: Option<&str>,
        name: Option<&str>,
        audit: &mut Map<String, Value>,
    ) -> Result<Value, Failure> {
        self.no_dialog()?;
        let mut result = self.page(audit)?;
        let reading = self.read(None)?;
        let fits = |element: &Value| {
            // latch never targets a password or secret field.
            if ["secret", "filled", "disabled", "hidden"]
                .iter()
                .any(|flag| element[*flag] == true)
            {
                return false;
            }
            let has = element["role"].as_str().unwrap_or_default();
            if let Some(role) = role {
                return has == role;
            }
            match action {
                Some(Action::Type) => matches!(has, "textbox" | "searchbox" | "spinbutton"),
                Some(Action::Select) => matches!(has, "combobox" | "listbox"),
                _ => true,
            }
        };
        let mut options = Vec::new();
        let mut exact = Vec::new();
        for element in reading.elements.iter().filter(|e| fits(e)) {
            let mut option = Map::new();
            for key in ["ref", "role", "name", "row", "value", "checked", "frame"] {
                if !element[key].is_null() {
                    option.insert(key.into(), element[key].clone());
                }
            }
            if let Some(name) = name
                && same_name(element["name"].as_str().unwrap_or_default(), name)
            {
                exact.push(element["ref"].clone());
            }
            options.push(Value::Object(option));
        }
        result.insert("snapshot".into(), json!(reading.snapshot));
        result.insert("options".into(), json!(options));
        result.insert("exact".into(), json!(exact));
        if reading.truncated {
            result.insert("truncated".into(), json!(true));
        }
        Ok(Value::Object(result))
    }

    /// Where the page is, its first lines and an element list the agent
    /// sees: what a list's answer ends with.
    pub(super) fn glance(&mut self, audit: &mut Map<String, Value>) -> Result<Value, Failure> {
        let mut result = self.page(audit)?;
        if self.driver.dialog_open() {
            result.insert("blocked_by_dialog".into(), json!(true));
            return Ok(Value::Object(result));
        }
        result.insert("lines".into(), json!(self.first_lines()?));
        self.read(None)?.into_json(&mut result);
        Ok(Value::Object(result))
    }

    /// The page's first five lines.
    pub(super) fn first_lines(&mut self) -> Result<Vec<String>, Failure> {
        let main = self.main_frame()?;
        let text = self.driver.run(&main, "__latch.text(600)")?;
        Ok(text["text"]
            .as_str()
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .take(5)
            .map(str::to_string)
            .collect())
    }

    /// The page's lines, then its non-secret fields as lines, for a judgement.
    pub(super) fn lines(&mut self, audit: &mut Map<String, Value>) -> Result<Value, Failure> {
        self.no_dialog()?;
        let page = self.page(audit)?;
        let main = self.main_frame()?;
        let text = self
            .driver
            .run(&main, &format!("__latch.text({TEXT_CHARS})"))?;
        let lines: Vec<&str> = text["text"]
            .as_str()
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .take(MAX_LINES)
            .collect();
        let mut fields = Vec::new();
        for frame in self.driver.frames()? {
            if let Ok(listed) = self.driver.run(&frame, "__latch.fields(40)") {
                fields.extend(listed.as_array().cloned().unwrap_or_default());
            }
        }
        let (origin, path) = super::origin_and_path(page["url"].as_str().unwrap_or_default());
        Ok(json!({
            "page": {"origin": origin, "path": path, "title": page["title"]},
            "lines": lines,
            "fields": fields,
        }))
    }

    pub(super) fn wait(
        &mut self,
        condition: Condition,
        within_ms: u64,
        audit: &mut Map<String, Value>,
    ) -> Result<Value, Failure> {
        self.page(audit)?;
        if let Condition::Ref { state, .. } = &condition
            && !STATES.contains(&state.as_str())
        {
            return Err(Failure::usage(
                "bad_arguments",
                format!("--state is one of {}", STATES.join(", ")),
            ));
        }
        let deadline = Instant::now() + Duration::from_millis(within_ms);
        let mut events = Vec::new();
        loop {
            let mut result = Map::new();
            if self.driver.dialog_open() {
                result.insert("met".into(), json!(false));
                result.insert("blocked_by_dialog".into(), json!(true));
                result.insert("events".into(), json!(events));
                return Ok(Value::Object(result));
            }
            let (met, done, detail) = self.holds(&condition)?;
            if met || done || Instant::now() >= deadline {
                result.insert("met".into(), json!(met));
                if let Some(detail) = detail {
                    result.extend(detail.as_object().cloned().unwrap_or_default());
                }
                if !events.is_empty() {
                    result.insert("events".into(), json!(events));
                }
                return Ok(Value::Object(result));
            }
            std::thread::sleep(POLL);
            events.extend(self.driver.settle()?);
        }
    }

    /// Whether `condition` holds now; whether waiting longer can't help
    /// (its element is from a page that is gone); and what to report.
    fn holds(&mut self, condition: &Condition) -> Result<(bool, bool, Option<Value>), Failure> {
        match condition {
            Condition::Url { url } => {
                let mut ignored = Map::new();
                let page = self.page(&mut ignored)?;
                Ok((
                    page["url"]
                        .as_str()
                        .unwrap_or_default()
                        .starts_with(url.as_str()),
                    false,
                    None,
                ))
            }
            Condition::Text { text } => {
                let main = self.main_frame()?;
                let page = self
                    .driver
                    .run(&main, &format!("__latch.text({TEXT_CHARS})"))?;
                Ok((
                    page["text"]
                        .as_str()
                        .unwrap_or_default()
                        .contains(text.as_str()),
                    false,
                    None,
                ))
            }
            Condition::Field {
                snapshot,
                target,
                value,
            } => {
                let Some(element) = self.element_now(snapshot.as_deref(), target)? else {
                    return Ok((false, true, Some(json!({"stale": true}))));
                };
                if element["secret"] == true || element["filled"] == true {
                    return Err(Failure::new(
                        Exit::Denied,
                        "secret_field",
                        "that is a password or secret field; latch never reads one back".into(),
                    ));
                }
                let met = match value.as_str() {
                    "checked" => element["checked"] == true,
                    "unchecked" => element["checked"] == false,
                    want => element["value"].as_str().unwrap_or_default() == want,
                };
                Ok((met, false, None))
            }
            Condition::Ref {
                snapshot,
                target,
                state,
            } => {
                let element = self.element_now(snapshot.as_deref(), target)?;
                let Some(element) = element else {
                    let gone = state == "gone" || state == "hidden";
                    return Ok((gone, true, (!gone).then(|| json!({"stale": true}))));
                };
                let met = match state.as_str() {
                    "visible" => element["hidden"] != true,
                    "hidden" => element["hidden"] == true,
                    "enabled" => element["disabled"] != true,
                    "disabled" => element["disabled"] == true,
                    "checked" => element["checked"] == true,
                    "unchecked" => element["checked"] == false,
                    _ => false,
                };
                Ok((met, false, None))
            }
            Condition::Download { besides } => {
                let done = self
                    .driver
                    .downloads()?
                    .into_iter()
                    .find(|d| d.state == "completed" && !besides.contains(&d.download));
                Ok(match done {
                    Some(d) => (true, false, Some(json!({"download": d.download}))),
                    None => (false, false, None),
                })
            }
        }
    }

    /// The element `target` names as it is now, or `None` once its page is gone.
    fn element_now(
        &mut self,
        snapshot: Option<&str>,
        target: &str,
    ) -> Result<Option<Value>, Failure> {
        match self.bound(snapshot, target) {
            Ok((snapshot, position)) => match self.element(&snapshot, position) {
                Ok(element) => Ok(Some(element)),
                Err(failure) if failure.code == "stale_snapshot" => Ok(None),
                Err(failure) => Err(failure),
            },
            Err(failure) if failure.code == "stale_snapshot" => Ok(None),
            Err(failure) => Err(failure),
        }
    }

    /// A login wall or CAPTCHA on the page now, if there is one.
    pub(super) fn wall(&mut self) -> Result<Option<&'static str>, Failure> {
        let mut login = false;
        for frame in self.driver.frames()? {
            let Ok(found) = self.driver.run(&frame, "__latch.wall()") else {
                continue;
            };
            if found["captcha"] == true {
                return Ok(Some("captcha"));
            }
            login |= found["password"] == true;
        }
        Ok(login.then_some("login"))
    }

    /// A click's target on a page whose element list has never reached the
    /// agent goes back to the host, which judges whether the click can be
    /// undone before it is made.
    pub(super) fn seen_or_review(
        &mut self,
        snapshot: &str,
        position: usize,
        target: &str,
        element: &Value,
    ) -> Result<(), Failure> {
        let (frame, _) = self.reference(snapshot, position)?;
        if self.shown.contains(&document_key(&frame)) {
            return Ok(());
        }
        let main = self.main_frame()?;
        let title = self.driver.run(&main, "__latch.title()")?;
        let mut failure = Failure::new(
            Exit::Denied,
            "review_target",
            "the agent has not seen this page".into(),
        );
        failure.details = json!({
            "target": {
                "ref": target, "snapshot": snapshot, "role": element["role"],
                "name": element["name"], "row": element["row"],
            },
            "title": title,
        });
        Err(failure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_whole_never_in_part() {
        assert!(same_name("New  expense", "new expense"));
        assert!(!same_name("New expense report", "New expense"));
        assert!(!same_name("Amount (USD)", "amount"));
    }

    #[test]
    fn conditions_parse_from_the_hosts_lines() {
        let c: Condition =
            serde_json::from_value(json!({"kind": "field", "target": "e3", "value": "checked"}))
                .unwrap();
        assert!(matches!(c, Condition::Field { snapshot: None, .. }));
        let c: Condition =
            serde_json::from_value(json!({"kind": "download", "besides": ["d1"]})).unwrap();
        assert!(matches!(c, Condition::Download { .. }));
    }
}
