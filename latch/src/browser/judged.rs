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
//! The judge sees an item's kind, title and the sites it is saved for, and
//! the fields a fill maps, never a value.

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
    /// The sites the item is saved for, which a login fills only on: the
    /// judge sets them beside the page's site.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    sites: Vec<String>,
    /// Other top-level sites a login may be filled under.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    other_top_sites: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub(super) struct FileShown {
    name: String,
    size: u64,
}

/// Judges `action` for the caller's gate: no gate (a caller in the
/// browser's own tests) is never judged. The audit keeps what was judged,
/// whole, as `judged` (a plan's steps as written, like an exec's argv),
/// and the judgment as `gate`.
pub(super) async fn judge(
    gate: Option<Gate<'_>>,
    action: Action,
    audit: &mut Map<String, Value>,
) -> Result<(), Failure> {
    let Some(gate) = gate else {
        return Ok(());
    };
    audit.insert(
        "judged".into(),
        serde_json::to_value(&action).expect("an action serializes"),
    );
    let judgment = gate.judge(&action).await;
    audit.insert("gate".into(), judgment.gate);
    judgment.verdict
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

/// A vault item's kind, title and sites; an item the vault lacks is shown
/// by its handle, and is refused later by the step that uses it.
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
            sites: meta.sites,
            other_top_sites: meta.other_top_sites,
        },
        None => ItemShown {
            kind: "unknown".into(),
            title: id.to_string(),
            sites: vec![],
            other_top_sites: vec![],
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

/// A login fill as `rules try --fill-login` previews it: the item's kind
/// and title as given, never looked up in the vault, on the site of `url`,
/// as a login saved for that site.
pub(crate) fn previewed_fill(
    url: &str,
    kind: &str,
    title: &str,
    fields: Vec<String>,
) -> impl Serialize + use<> {
    let site = sites::site_of(url);
    Action::VaultFill {
        item: ItemShown {
            kind: kind.to_string(),
            title: title.to_string(),
            sites: site.iter().cloned().collect(),
            other_top_sites: vec![],
        },
        site,
        fields,
        confirmed: false,
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

    /// What the gatekeeper keeps of a vault fill it judged holds the
    /// item's kind, title and sites, never a value.
    #[tokio::test]
    async fn a_kept_vault_fill_holds_no_vault_value() {
        use crate::gate::{Caller, Gatekeeper, fake, rules::RuleFiles};
        use crate::vault::item::{Item, LOGIN, Raw};
        let parent = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        std::fs::create_dir_all(&parent).unwrap();
        let dir = tempfile::tempdir_in(parent).unwrap();
        let state = dir.path().to_path_buf();
        let browsers = Browsers::new(
            state.clone(),
            crate::vault::host::Vault::new(state.clone()),
            true,
        );
        let (username, password) = ("morgan@example.com", "made-up-pass-3f81");
        let fields = [
            ("username".into(), username.into()),
            ("password".into(), password.into()),
        ];
        let raw = Raw::from_labels(LOGIN, "Shop login", &fields).unwrap();
        let item = Item {
            id: String::new(),
            revision: 0,
            sites: vec!["shop.example".into()],
            other_top_sites: vec![],
            raw,
        };
        let added = browsers.vault.with(|store| store.add(item)).unwrap();
        let args =
            json!({"item": added.item, "fields": [["username", ["e1"]], ["password", ["e2"]]]});
        let action = fill(&browsers, &args, Some("shop.example".into()), false);

        let world = fake::World {
            decide: Some(|_| Some([0.9, 0.08, 0.02])),
            ..Default::default()
        };
        let plow = fake::serve(world);
        let provider = crate::model::Plow::new(&plow.url, "made-up-login".into());
        let rules = Box::new(RuleFiles::new(&state).unwrap());
        let keeper = Gatekeeper::new(plow.client(), std::sync::Arc::new(provider), rules)
            .keeping_requests(&state);
        let action = serde_json::value::to_raw_value(&action).unwrap();
        let judgment = keeper.judge(Caller::Owner, &action).await;
        let id = judgment.gate["kept_request"].as_str().unwrap();
        let kept = crate::gate::requests::read(&state, id).unwrap().to_string();
        assert!(kept.contains("Shop login"), "{kept}");
        assert!(
            !kept.contains(password) && !kept.contains(username),
            "{kept}"
        );
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
