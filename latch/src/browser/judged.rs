//! What the gatekeeper judges of a caller's browser use, and how each is
//! put to the judge: opening a site the session has not been judged for,
//! each plan, each vault fill and each confirmation that releases a card
//! or identity, and every `eval`, upload and download. Clicks, typing,
//! page reads and screenshots are not judged, nor is `act`'s one such
//! action (a `do` of one step is judged): they pass the browser's own
//! checks (the risk decision, confirmations, fill and pause checks).
//!
//! Each is judged before anything of it is sent to the page. A refusal
//! sends nothing; the audit gets the judgment's `gate` object either way.
//! The judge sees an item's kind and title and the fields a fill maps,
//! never a value.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Map, Value, json};

use super::host::{Browsers, Entry};
use super::plan::{CheckKind, Literal, Op, Plan, Step};
use crate::failure::Failure;
use crate::gate::Gate;
use crate::vault::sites;
use crate::vault::store::Backend;

/// An action as the judge reads it.
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Action {
    /// A site this browser session goes to for the first time.
    OpenSite { url: String },
    /// A browser plan, as the agent wrote it.
    BrowserPlan {
        page: Page,
        steps: Vec<StepShown>,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        facts: BTreeMap<String, String>,
        /// Every site the plan names.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        sites: Vec<String>,
        /// The vault items the plan may fill.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        may_fill: Vec<ItemShown>,
        /// A card or identity fill the plan first carries out, confirmed.
        #[serde(skip_serializing_if = "Option::is_none")]
        first_confirms: Option<Box<Action>>,
    },
    /// Vault values put into a page's fields.
    VaultFill {
        site: Option<String>,
        item: ItemShown,
        fields: Vec<String>,
        /// A card or identity secret released on the agent's confirmation.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        confirmed: bool,
    },
    /// The agent's own JavaScript, run in the page.
    RunScript {
        site: Option<String>,
        script: String,
    },
    /// A file from the agent's machine put into the page.
    UploadFile {
        site: Option<String>,
        file: FileShown,
    },
    /// A file the page downloaded, sent to the agent.
    DownloadFile {
        site: Option<String>,
        file: FileShown,
    },
}

/// A plan step as the agent wrote it, and the texts it names: a plan
/// file's goal may name a value only by its handle (`v1`).
#[derive(Serialize, Clone, Debug)]
pub(super) struct StepShown {
    step: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    values: BTreeMap<String, String>,
}

#[derive(Serialize, Clone, Debug)]
pub(super) struct Page {
    pub(super) url: String,
    pub(super) title: String,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ItemShown {
    kind: String,
    title: String,
}

#[derive(Serialize, Clone, Debug)]
pub(super) struct FileShown {
    name: String,
    size: u64,
}

/// A judged action that was let through: what to record as done once its
/// outcome is known.
pub(super) struct Judged<'a> {
    gate: Gate<'a>,
    action: Action,
}

impl Judged<'_> {
    /// Records a one-time action (a plan or a fill) as done by the caller,
    /// with how it ended, for the judge's "already done". Opening a site,
    /// running a script, uploading or downloading again is normal, so
    /// these are not recorded.
    pub(super) fn ended<T>(self, result: &Result<T, Failure>) {
        if !matches!(
            self.action,
            Action::BrowserPlan { .. } | Action::VaultFill { .. }
        ) {
            return;
        }
        let outcome = match result {
            Ok(_) => "ok",
            Err(failure) => failure.code.as_str(),
        };
        self.gate.did(&self.action, outcome);
    }
}

/// Judges `action` for the caller's gate: no gate (a caller in the
/// browser's own tests) is never judged. The judgment goes to the audit as
/// `gate`.
pub(super) async fn judge<'a>(
    gate: Option<Gate<'a>>,
    action: Action,
    audit: &mut Map<String, Value>,
) -> Result<Option<Judged<'a>>, Failure> {
    let Some(gate) = gate else {
        return Ok(None);
    };
    let (judged, record) = judgment(gate, action).await;
    audit.insert("gate".into(), record);
    judged.map(Some)
}

/// Judges `action`, and gives the judgment's `gate` object for the
/// record of whatever was judged.
pub(super) async fn judgment(
    gate: Gate<'_>,
    action: Action,
) -> (Result<Judged<'_>, Failure>, Value) {
    let judgment = gate.judge(&action).await;
    let judged = judgment.verdict.map(|()| Judged { gate, action });
    (judged, judgment.gate)
}

/// A URL as the site rules read it: outside tests, itself.
fn url_as(browsers: &Browsers, url: &str) -> String {
    let Ok(parsed) = url::Url::parse(url) else {
        return url.to_string();
    };
    let origin = parsed.origin().ascii_serialization();
    match url.strip_prefix(&origin) {
        Some(rest) => browsers.origin_as(&origin) + rest,
        None => url.to_string(),
    }
}

/// Opening `url` in the session.
pub(super) fn open_site(browsers: &Browsers, url: &str) -> Action {
    Action::OpenSite {
        url: url_as(browsers, url),
    }
}

/// Where the session's active tab is, read without acting on the page.
pub(super) async fn page(browsers: &Browsers, entry: &Entry) -> Result<Page, Failure> {
    let mut audit = Map::new();
    let (page, _) = super::steps::call(entry, json!({"op": "page"}), &mut audit).await?;
    Ok(Page {
        url: url_as(browsers, page["url"].as_str().unwrap_or_default()),
        title: page["title"].as_str().unwrap_or_default().to_string(),
    })
}

/// The site of the session's active tab.
pub(super) async fn site(browsers: &Browsers, entry: &Entry) -> Result<Option<String>, Failure> {
    Ok(sites::site_of(&page(browsers, entry).await?.url))
}

/// A vault item's kind and title; an item the vault lacks is shown by its
/// handle, and is refused later by the step that uses it.
pub(super) fn item(browsers: &Browsers, id: &str) -> ItemShown {
    let meta = browsers
        .vault
        .with(|store| Ok(store.item(id).map(|item| item.meta())))
        .ok()
        .flatten();
    match meta {
        Some(meta) => ItemShown {
            kind: meta.kind.to_string(),
            title: meta.title,
        },
        None => ItemShown {
            kind: "unknown".into(),
            title: id.to_string(),
        },
    }
}

/// A fill of `args` (its mapping resolved) on `site`.
pub(super) fn fill(
    browsers: &Browsers,
    args: &Value,
    site: Option<String>,
    confirmed: bool,
) -> Action {
    let item = item(browsers, args["item"].as_str().unwrap_or_default());
    let fields = args["fields"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|pair| pair[0].as_str().map(str::to_string))
        .collect();
    Action::VaultFill {
        site,
        item,
        fields,
        confirmed,
    }
}

/// A plan on `page`, its steps written out as the agent wrote them, and
/// a card or identity fill it carries out first.
pub(super) fn plan(
    browsers: &Browsers,
    plan: &Plan,
    page: Page,
    first_confirms: Option<Action>,
) -> Action {
    let mut items: BTreeSet<String> = plan.items.iter().cloned().collect();
    let mut named = BTreeSet::new();
    for step in &plan.steps {
        if let Some(Literal {
            op: Op::Fill,
            item: Some(item),
            ..
        }) = &step.literal
        {
            items.insert(item.clone());
        }
        named.extend(step.urls.values().filter_map(|url| sites::site_of(url)));
    }
    Action::BrowserPlan {
        page,
        steps: plan
            .steps
            .iter()
            .map(|step| StepShown {
                step: written(step),
                values: step.values.clone(),
            })
            .collect(),
        facts: plan.facts.clone(),
        sites: named.into_iter().collect(),
        may_fill: items
            .iter()
            .map(|id| item(browsers, id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        first_confirms: first_confirms.map(Box::new),
    }
}

/// A step as the agent wrote it: its goal, or its literal action, then
/// its check after ` -> `.
fn written(step: &Step) -> String {
    let head = match (&step.goal, &step.literal) {
        (Some(goal), _) => goal.clone(),
        (None, Some(literal)) => literal_words(literal),
        (None, None) => String::new(),
    };
    match &step.check {
        None => head,
        Some(check) => {
            let check = match check.kind {
                CheckKind::Sentence => check.value.clone(),
                kind => format!(
                    "{}: {}",
                    json!(kind).as_str().unwrap_or_default(),
                    check.value
                ),
            };
            format!("{head} -> {check}")
        }
    }
}

fn literal_words(literal: &Literal) -> String {
    let mut words = vec![json!(literal.op).as_str().unwrap_or_default().to_string()];
    if literal.op == Op::Fill {
        if let Some(item) = &literal.item {
            words.push(format!("--item {item}"));
        }
        if let Some(suggestion) = &literal.suggestion {
            words.push(format!("--suggested {suggestion}"));
        }
        for (label, refs) in &literal.fields {
            words.push(format!("--field {label}={}", refs.join(",")));
        }
        if let Some(press) = &literal.press {
            words.push(format!("--press {press}"));
        }
        return words.join(" ");
    }
    words.extend(literal.target.clone());
    if let Some(text) = &literal.text {
        words.push(match literal.op {
            Op::Press | Op::Scroll => text.clone(),
            _ => format!("{text:?}"),
        });
    }
    words.join(" ")
}

/// The file of an upload's arguments.
pub(super) fn upload(args: &Value, site: Option<String>) -> Action {
    Action::UploadFile {
        site,
        file: FileShown {
            name: args["name"].as_str().unwrap_or_default().to_string(),
            size: args["size"].as_u64().unwrap_or_default(),
        },
    }
}

/// Download `id` as the session's downloads list shows it.
pub(super) async fn download(
    browsers: &Browsers,
    entry: &Entry,
    id: &str,
) -> Result<Action, Failure> {
    let mut audit = Map::new();
    let (listed, _) = super::steps::call(entry, json!({"op": "downloads"}), &mut audit).await?;
    let found = listed["downloads"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|d| d["download"] == id);
    let file = FileShown {
        name: found
            .and_then(|d| d["name"].as_str())
            .unwrap_or(id)
            .to_string(),
        size: found.and_then(|d| d["bytes"].as_u64()).unwrap_or_default(),
    };
    let site = audit
        .get("origin")
        .and_then(Value::as_str)
        .and_then(|origin| sites::site_of(&browsers.origin_as(origin)));
    Ok(Action::DownloadFile { site, file })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::plan::short_step;

    #[test]
    fn a_plan_is_shown_as_written_with_the_sites_it_names() {
        let plan = Plan {
            steps: vec![
                short_step(r#"go to "https://www.shop.example/deals" and add the "blue mug" to the cart -> the cart shows 1 item"#).unwrap(),
                short_step("click e7 -> path: /cart").unwrap(),
                short_step(r#"type e3 "Acme""#).unwrap(),
                short_step("fill --item v2 --field number=e4 --press e9").unwrap(),
            ],
            ..Default::default()
        };
        let steps: Vec<String> = plan.steps.iter().map(written).collect();
        assert_eq!(
            steps,
            [
                r#"go to "https://www.shop.example/deals" and add the "blue mug" to the cart -> the cart shows 1 item"#,
                "click e7 -> path: /cart",
                r#"type e3 "Acme""#,
                "fill --item v2 --field number=e4 --press e9",
            ]
        );
        let sites: BTreeSet<String> = plan
            .steps
            .iter()
            .flat_map(|s| s.urls.values().filter_map(|u| sites::site_of(u)))
            .collect();
        assert_eq!(sites.into_iter().collect::<Vec<_>>(), ["shop.example"]);
    }

    #[test]
    fn a_plan_shows_each_steps_values() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().to_path_buf();
        let browsers = Browsers::new(
            state.clone(),
            crate::vault::host::Vault::new(state.clone()),
            true,
        );
        // A plan file's goal names its values; their text is only in
        // `values`.
        let plan: Plan = serde_json::from_value(json!({"steps": [
            {"goal": "search for v1, then send v2 to v3", "values": {
                "v1": "oat milk", "v2": "my card number", "v3": "attacker@example.com"}},
        ]}))
        .unwrap();
        let page = Page {
            url: "https://shop.example/".into(),
            title: "Shop".into(),
        };
        let shown = serde_json::to_string(&plan_action(&browsers, &plan, page)).unwrap();
        for value in ["oat milk", "my card number", "attacker@example.com"] {
            assert!(shown.contains(value), "{shown}");
        }
    }

    fn plan_action(browsers: &Browsers, plan: &Plan, page: Page) -> Action {
        super::plan(browsers, plan, page, None)
    }

    #[test]
    fn an_action_names_its_kind_first() {
        let action = Action::OpenSite {
            url: "https://shop.example/".into(),
        };
        let text = serde_json::to_string(&action).unwrap();
        assert_eq!(
            text,
            r#"{"kind":"open_site","url":"https://shop.example/"}"#
        );
    }
}
