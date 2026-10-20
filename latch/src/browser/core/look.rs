//! The worker's half of plans and checks: `find`'s candidates, the
//! readings the host needs to judge a sentence or a consequential action,
//! the waits for typed conditions, and the pinning of a plan's refs. The
//! decision model itself runs in the host.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::{Core, TEXT_CHARS, document_key};
use crate::browser::driver::{Driver, Frame};
use crate::failure::{Exit, Failure};
use crate::state::to_hex;

/// The most page lines a judgement gets.
const MAX_LINES: usize = 120;
/// The most page lines a plan's answer shows, and the most on-screen
/// lines a decision gets.
const PAGE_LINES: usize = 40;
/// The most page lines a decision may quote as evidence, on-screen ones
/// included.
const VIEW_LINES: usize = 100;
/// The most field lines a frame gives a judgement, and a decision gets.
const FIELD_LINES: usize = 40;
/// The longest page line a decision gets.
const LINE_CHARS: usize = 160;
/// How often a wait looks again.
const POLL: Duration = Duration::from_millis(250);

/// A typed condition, decided by latch's own code.
#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Condition {
    /// The page's origin is `origin`.
    Origin { origin: String },
    /// `path` is part of the page's path (never its host or query).
    Path { path: String },
    /// The page's query has parameter `name` with `value`.
    Query { name: String, value: String },
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
    /// The elements with `role` whose name is exactly `name`, in a
    /// snapshot the agent may act on.
    pub(super) fn resolve(
        &mut self,
        role: &str,
        name: &str,
        audit: &mut Map<String, Value>,
    ) -> Result<Value, Failure> {
        self.no_dialog()?;
        let mut result = self.page(audit)?;
        let reading = self.read(None)?;
        let mut candidates = Vec::new();
        for element in &reading.elements {
            // latch never targets a password or secret field.
            if element["secret"] == true || element["filled"] == true {
                continue;
            }
            if element["role"] != role
                || !same_name(element["name"].as_str().unwrap_or_default(), name)
            {
                continue;
            }
            let mut candidate = Map::new();
            for key in [
                "ref", "role", "name", "row", "value", "checked", "disabled", "frame",
            ] {
                if !element[key].is_null() {
                    candidate.insert(key.into(), element[key].clone());
                }
            }
            candidates.push(Value::Object(candidate));
        }
        result.insert("snapshot".into(), json!(reading.snapshot));
        result.insert("candidates".into(), json!(candidates));
        if reading.truncated {
            result.insert("truncated".into(), json!(true));
        }
        Ok(Value::Object(result))
    }

    /// The snapshot `target` is read in, if it holds that ref. The page
    /// may have moved on since; that is checked when the step runs.
    pub(super) fn pin(&mut self, snapshot: Option<&str>, target: &str) -> Result<Value, Failure> {
        let position = target
            .strip_prefix('e')
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| Failure::usage("bad_target", format!("{target:?} is not a ref")))?;
        let latest = self.last_shown.clone();
        let id = snapshot.or(latest.as_deref()).ok_or_else(super::stale)?;
        let found = self.snapshot(id)?;
        if found.refs.len() < position {
            return Err(Failure::failed(
                "no_such_target",
                format!("snapshot {id} has no {target}"),
            ));
        }
        Ok(json!({"snapshot": id}))
    }

    /// What acting on `target` would act on: the element, or for Enter its
    /// form's submit control (else the form); the lines of the form or
    /// section around it, which name the item, amount or recipient; the
    /// page's title; and the document and element identity a confirmation
    /// is bound to.
    pub(super) fn review(
        &mut self,
        snapshot: Option<&str>,
        target: &str,
        enter: bool,
        audit: &mut Map<String, Value>,
    ) -> Result<Value, Failure> {
        self.no_dialog()?;
        let page = self.page(audit)?;
        let (snapshot, position) = self.bound(snapshot, target)?;
        let (frame, index) = self.reference(&snapshot, position)?;
        let read = self.driver.run(
            &frame,
            &format!("__latch.review({snapshot:?}, {index}, {enter})"),
        )?;
        if read.is_null() {
            return Err(super::stale());
        }
        Ok(json!({
            "snapshot": snapshot,
            "ref": target,
            "document": document_key(&frame),
            "uid": read["uid"],
            "element": read["element"],
            "acts_on": read["acts_on"],
            "lines": read["lines"],
            "title": page["title"],
            "url": page["url"],
        }))
    }

    /// Where the page is, its first lines and an element list the agent
    /// sees: what a list's answer ends with.
    pub(super) fn glance(&mut self, audit: &mut Map<String, Value>) -> Result<Value, Failure> {
        let mut result = self.page(audit)?;
        if self.driver.dialog_open() {
            result.insert("blocked_by_dialog".into(), json!(true));
            return Ok(Value::Object(result));
        }
        result.insert("lines".into(), json!(self.first_lines(PAGE_LINES)?));
        self.read(None)?.into_json(&mut result);
        Ok(Value::Object(result))
    }

    /// The page's first `count` lines.
    pub(super) fn first_lines(&mut self, count: usize) -> Result<Vec<String>, Failure> {
        let main = self.main_frame()?;
        let text = self
            .driver
            .run(&main, &format!("__latch.text({TEXT_CHARS})"))?;
        Ok(text["text"]
            .as_str()
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .take(count)
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
        let fields = self.field_lines()?;
        let (origin, path) = super::origin_and_path(page["url"].as_str().unwrap_or_default());
        Ok(json!({
            "page": {"origin": origin, "path": path, "title": page["title"]},
            "lines": lines,
            "fields": fields,
        }))
    }

    /// Every frame's non-secret fields as lines (`combobox "Size": "Large"`).
    fn field_lines(&mut self) -> Result<Vec<String>, Failure> {
        let mut fields = Vec::new();
        for frame in self.driver.frames()? {
            if let Ok(listed) = self
                .driver
                .run(&frame, &format!("__latch.fields({FIELD_LINES})"))
            {
                fields.extend(
                    listed
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|line| line.as_str().map(str::to_string)),
                );
            }
        }
        Ok(fields)
    }

    /// The page as one pass of an instruction step sees it, in a snapshot
    /// of latch's own: its elements, each with the kind of input it takes,
    /// a dropdown's options, its form and whether something covers it; the
    /// lines on screen, then the page's other lines; its fields as lines;
    /// the scroll position; a sign-in form or CAPTCHA on it; and `state`,
    /// which changes whenever the document, a field, the lines on screen or
    /// the scroll position does.
    pub(super) fn view(&mut self, audit: &mut Map<String, Value>) -> Result<Value, Failure> {
        self.no_dialog()?;
        let mut result = self.page(audit)?;
        let wall = self.wall()?;
        let reading = self.read(None)?;
        let refs: Vec<(Frame, usize)> = self
            .snapshot(&reading.snapshot)?
            .refs
            .iter()
            .map(|r| (r.frame.clone(), r.index))
            .collect();
        let mut extras: HashMap<String, Vec<Value>> = HashMap::new();
        for (frame, _) in &refs {
            if !extras.contains_key(&frame.id) {
                let found = self
                    .driver
                    .run(frame, &format!("__latch.viewOf({:?})", reading.snapshot))
                    .ok()
                    .and_then(|found| found.as_array().cloned())
                    .unwrap_or_default();
                extras.insert(frame.id.clone(), found);
            }
        }
        let mut elements = reading.elements;
        for (element, (frame, index)) in elements.iter_mut().zip(&refs) {
            let Some(extra) = extras.get(&frame.id).and_then(|found| found.get(*index)) else {
                continue;
            };
            for (key, value) in extra.as_object().into_iter().flatten() {
                element[key] = value.clone();
            }
        }
        let main = self.main_frame()?;
        let on_screen: Vec<String> = strings(
            &self
                .driver
                .run(&main, &format!("__latch.screenLines({PAGE_LINES})"))?,
        );
        let mut seen: HashSet<String> = on_screen.iter().cloned().collect();
        let mut lines = Vec::new();
        for line in self.first_lines(usize::MAX)? {
            if on_screen.len() + lines.len() >= VIEW_LINES {
                break;
            }
            let line = clip(&line, LINE_CHARS);
            if seen.insert(line.clone()) {
                lines.push(line);
            }
        }
        let mut fields = self.field_lines()?;
        fields.truncate(FIELD_LINES);
        let scroll = self.driver.run(&main, "__latch.scroll()")?;
        let document = document_key(&main);
        let state = page_state(&document, &fields, &on_screen, &scroll["y"]);
        result.insert("snapshot".into(), json!(reading.snapshot));
        result.insert("document".into(), json!(document));
        result.insert("elements".into(), json!(elements));
        if reading.truncated {
            result.insert("truncated".into(), json!(true));
        }
        result.insert("on_screen".into(), json!(on_screen));
        result.insert("lines".into(), json!(lines));
        result.insert("fields".into(), json!(fields));
        result.insert("scroll".into(), scroll);
        result.insert("wall".into(), json!(wall));
        result.insert("state".into(), json!(state));
        Ok(Value::Object(result))
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
            Condition::Origin { .. } | Condition::Path { .. } | Condition::Query { .. } => {
                let mut ignored = Map::new();
                let page = self.page(&mut ignored)?;
                let url = url::Url::parse(page["url"].as_str().unwrap_or_default()).ok();
                Ok((url.is_some_and(|url| at(&url, condition)), false, None))
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
}

/// The page's state as the repeat guard compares it: the same document,
/// field values, lines on screen and scroll position hash the same.
fn page_state(document: &str, fields: &[String], on_screen: &[String], scroll_y: &Value) -> String {
    let state = json!([document, fields, on_screen, scroll_y]);
    to_hex(&Sha256::digest(state.to_string().as_bytes()))
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|line| line.as_str().map(str::to_string))
        .collect()
}

/// `line` cut to `max` characters, the cut marked.
fn clip(line: &str, max: usize) -> String {
    if line.chars().count() <= max {
        return line.to_string();
    }
    let kept: String = line.chars().take(max - 1).collect();
    format!("{kept}…")
}

/// A reviewed context (`Core::review`) hashed: the document, the element's
/// identity and what it acts on, the page title and the lines around it.
/// A confirmation, and an action judged on that context, hold only while it
/// is unchanged.
pub fn fingerprint(reviewed: &Value) -> String {
    let bound = json!([
        reviewed["document"],
        reviewed["uid"],
        reviewed["element"],
        reviewed["acts_on"],
        reviewed["title"],
        reviewed["lines"],
    ]);
    to_hex(&Sha256::digest(bound.to_string().as_bytes()))
}

/// Whether the page at `url` meets a URL condition, by component: the
/// origin whole, the path alone (never the host or query), one query
/// parameter.
fn at(url: &url::Url, condition: &Condition) -> bool {
    match condition {
        Condition::Origin { origin } => {
            url::Url::parse(origin).is_ok_and(|want| want.origin() == url.origin())
        }
        Condition::Path { path } => url.path().contains(path.as_str()),
        Condition::Query { name, value } => url
            .query_pairs()
            .any(|(n, v)| n == name.as_str() && v == value.as_str()),
        _ => false,
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
    fn url_checks_match_by_component() {
        let path = Condition::Path {
            path: "checkout".into(),
        };
        let url = |u: &str| url::Url::parse(u).unwrap();
        assert!(at(&url("https://shop.example/checkout?x=1"), &path));
        assert!(!at(&url("https://checkout.invalid/login"), &path));
        assert!(!at(
            &url("https://shop.example/login?next=checkout.html"),
            &path
        ));
        let origin = Condition::Origin {
            origin: "https://www.doordash.com".into(),
        };
        assert!(at(&url("https://www.doordash.com:443/store/1"), &origin));
        assert!(!at(&url("https://doordash.com/"), &origin));
        assert!(!at(&url("http://www.doordash.com/"), &origin));
        let query = Condition::Query {
            name: "q".into(),
            value: "salad bowl".into(),
        };
        assert!(at(
            &url("https://shop.example/s?page=2&q=salad+bowl"),
            &query
        ));
        assert!(!at(&url("https://shop.example/s?q=salad"), &query));
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
