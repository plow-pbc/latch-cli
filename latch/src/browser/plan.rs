//! The canonical plan: what `do --plan-file` takes, what the short form
//! (`do STEP…`) and `act` compile to, and the one check of its shape that
//! runs on the client and again on the host before anything is sent.
//!
//! A step is a literal action on refs (`click`, `type`, `select`, `press`,
//! `fill`; `act` adds `scroll` and `back`) or a goal in words. Each may
//! carry one check: a typed check latch decides (`origin`, `path`, `query`,
//! `text`, `field`, `download`, `new text`), or a sentence the decision
//! model judges.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The most steps one plan holds.
pub const MAX_STEPS: usize = 10;
/// A plan's deadline in seconds: by default, and at most.
pub const WITHIN: f64 = 120.0;
pub const MAX_WITHIN: f64 = 300.0;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub steps: Vec<Step>,
    /// Named texts a goal step may type where it names them, as `{NAME}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub facts: BTreeMap<String, String>,
    /// Vault item handles the plan may use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Step {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub urls: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub literal: Option<Literal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<Check>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Click,
    Type,
    Select,
    Press,
    Scroll,
    Back,
    Fill,
}

/// One exact action. `ref` and `snapshot` name the element; `text` is
/// what `type` types, the option `select` picks, the key `press` presses
/// or the way `scroll` goes. A `fill` maps item fields to refs, or takes
/// a suggestion latch made.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Literal {
    pub op: Op,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub press: Option<String>,
    /// A `fill` that takes one of latch's suggestions, by its id, in place
    /// of `fields`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    Origin,
    Path,
    Query,
    Text,
    Field,
    Download,
    Sentence,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub kind: CheckKind,
    pub value: String,
    /// It must not hold when the step starts, and must hold after.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub require_change: bool,
}

impl Check {
    /// `origin: …`, `path: …`, `query: k=v`, `text: …`, `new text: …`,
    /// `field: eN=…`, `download: complete`; anything else is a sentence.
    pub fn parse(text: &str) -> Result<Check, String> {
        let text = text.trim();
        let typed = [
            ("origin:", CheckKind::Origin, false),
            ("path:", CheckKind::Path, false),
            ("query:", CheckKind::Query, false),
            ("new text:", CheckKind::Text, true),
            ("text:", CheckKind::Text, false),
            ("field:", CheckKind::Field, false),
            ("download:", CheckKind::Download, false),
        ];
        let check = typed
            .iter()
            .find_map(|(prefix, kind, change)| {
                text.strip_prefix(prefix).map(|value| Check {
                    kind: *kind,
                    value: value.trim().to_string(),
                    require_change: *change,
                })
            })
            .unwrap_or(Check {
                kind: CheckKind::Sentence,
                value: text.to_string(),
                require_change: false,
            });
        check.validate()?;
        Ok(check)
    }

    fn validate(&self) -> Result<(), String> {
        let value = self.value.as_str();
        if value.trim().is_empty() {
            return Err("a check needs a value".into());
        }
        match self.kind {
            CheckKind::Origin => {
                let url = url::Url::parse(value).map_err(|_| {
                    format!("origin: takes an origin such as https://shop.example, not {value:?}")
                })?;
                if !matches!(url.scheme(), "http" | "https")
                    || url.path() != "/"
                    || url.query().is_some()
                {
                    return Err(format!(
                        "origin: takes an origin such as https://shop.example, not {value:?}"
                    ));
                }
            }
            CheckKind::Query if !value.contains('=') => {
                return Err("query: takes name=value".into());
            }
            CheckKind::Field => match value.split_once('=') {
                Some((target, _)) if is_ref(target) => {}
                _ => return Err("field: takes eN=<value|checked|unchecked>".into()),
            },
            CheckKind::Download if value != "complete" => {
                return Err("download: takes complete".into());
            }
            CheckKind::Sentence | CheckKind::Download if self.require_change => {
                return Err("only typed checks take require_change".into());
            }
            _ => {}
        }
        Ok(())
    }
}

pub fn is_ref(target: &str) -> bool {
    target
        .strip_prefix('e')
        .is_some_and(|n| n.parse::<u32>().is_ok_and(|n| n > 0))
}

impl Plan {
    /// The whole plan's shape, checked before anything runs: step count,
    /// what each step names, refs, URLs, fill mappings and check syntax.
    /// The error names the step.
    pub fn validate(&self) -> Result<(), String> {
        if self.steps.len() > MAX_STEPS {
            return Err(format!(
                "a plan holds at most {MAX_STEPS} steps; this one has {}",
                self.steps.len()
            ));
        }
        if let Some(within) = self.within
            && !(within > 0.0 && within <= MAX_WITHIN)
        {
            return Err(format!("within is between 0 and {MAX_WITHIN} seconds"));
        }
        for (n, step) in self.steps.iter().enumerate() {
            step.validate()
                .map_err(|message| format!("step {}: {message}", n + 1))?;
            for name in step.goal.as_deref().map(fact_refs).unwrap_or_default() {
                if !self.facts.contains_key(&name) {
                    return Err(format!(
                        "step {}: {{{name}}} names no fact of this plan; give it with --fact {name}=TEXT",
                        n + 1
                    ));
                }
            }
        }
        Ok(())
    }

    /// Each goal step with the facts it names (`{NAME}`) among its values:
    /// a step types only its own values, and a fact only where it is named.
    pub fn resolve_facts(&mut self) {
        let facts = self.facts.clone();
        for step in &mut self.steps {
            for name in step.goal.as_deref().map(fact_refs).unwrap_or_default() {
                if let Some(text) = facts.get(&name) {
                    step.values.insert(name, text.clone());
                }
            }
        }
    }

    pub fn within(&self) -> f64 {
        self.within.unwrap_or(WITHIN)
    }
}

impl Step {
    fn validate(&self) -> Result<(), String> {
        if let Some(check) = &self.check {
            check.validate()?;
        }
        match (&self.goal, &self.literal) {
            (Some(goal), None) => {
                if goal.trim().is_empty() {
                    return Err("a goal needs words".into());
                }
                for url in self.urls.values() {
                    let parsed = url::Url::parse(url)
                        .map_err(|_| format!("{url:?} is not an absolute URL"))?;
                    if !matches!(parsed.scheme(), "http" | "https") {
                        return Err(format!("{url:?} is not an http or https URL"));
                    }
                }
                Ok(())
            }
            (None, Some(literal)) => {
                if !self.values.is_empty() || !self.urls.is_empty() {
                    return Err("values and urls go with a goal, not a literal step".into());
                }
                literal.validate()
            }
            _ => Err("a step is either a goal or a literal action".into()),
        }
    }
}

impl Literal {
    pub fn click(target: &str) -> Literal {
        Literal {
            op: Op::Click,
            target: Some(target.into()),
            snapshot: None,
            text: None,
            item: None,
            fields: BTreeMap::new(),
            press: None,
            suggestion: None,
        }
    }

    /// What each operation takes, and nothing else.
    fn validate(&self) -> Result<(), String> {
        let op = serde_json::to_value(self.op).unwrap_or_default();
        let op = op.as_str().unwrap_or_default();
        let target = self.target.as_deref();
        if let Some(target) = target
            && !is_ref(target)
        {
            return Err(format!(
                "{target:?} is not a ref (eN); to name an element in words, write the step as a goal"
            ));
        }
        let fill_only = self.item.is_some()
            || !self.fields.is_empty()
            || self.press.is_some()
            || self.suggestion.is_some();
        if self.op != Op::Fill && fill_only {
            return Err(format!("{op} takes no item, fields or press"));
        }
        let text = self.text.as_deref();
        match self.op {
            Op::Click if target.is_none() => Err("click needs a ref".into()),
            Op::Click if text.is_some() => Err("click takes no text".into()),
            Op::Type | Op::Select | Op::Press if target.is_none() => {
                Err(format!("{op} needs a ref"))
            }
            Op::Type if text.is_none() => Err("type needs the text to type".into()),
            Op::Select if text.is_none_or(str::is_empty) => {
                Err("select needs the option to choose".into())
            }
            Op::Press if text.is_none_or(str::is_empty) => {
                Err("press needs a key, such as Enter".into())
            }
            Op::Scroll if !matches!(text, None | Some("up") | Some("down")) => {
                Err("scroll goes up or down".into())
            }
            Op::Back if target.is_some() || text.is_some() => {
                Err("back takes no ref or text".into())
            }
            Op::Fill => {
                if target.is_some() || text.is_some() {
                    return Err(
                        "fill takes --item, --field, --suggested and --press, not a ref or text"
                            .into(),
                    );
                }
                if self.item.as_deref().is_none_or(str::is_empty) {
                    return Err("fill needs --item".into());
                }
                match (&self.suggestion, self.fields.is_empty()) {
                    (None, true) => {
                        return Err(
                            "fill needs --field FIELD=eN for each field, or --suggested S".into(),
                        );
                    }
                    (Some(_), false) => {
                        return Err("fill takes --field or --suggested, not both".into());
                    }
                    (Some(id), true)
                        if !(id.len() > 1
                            && id.starts_with('s')
                            && id[1..].chars().all(|c| c.is_ascii_hexdigit())) =>
                    {
                        return Err(format!(
                            "--suggested takes a suggestion id from observe, such as s3f2a, not {id:?}"
                        ));
                    }
                    _ => {}
                }
                for (label, refs) in &self.fields {
                    if refs.is_empty() || !refs.iter().all(|r| is_ref(r)) {
                        return Err(format!("--field {label}= takes refs: eN[,eN…]"));
                    }
                }
                if self.press.as_deref().is_some_and(|p| !is_ref(p)) {
                    return Err("--press takes a ref".into());
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// One short-form step: `click e7 -> path: /cart`, `type e3 "Acme"`,
/// `fill --item v2 --field number=e4 --press e9`, or a goal in words with
/// its values in double quotes. The check follows the last ` -> ` outside
/// quotes.
pub fn short_step(text: &str) -> Result<Step, String> {
    let (head, check) = split_check(text)?;
    let check = check.map(Check::parse).transpose()?;
    let words = words(head)?;
    let literal = match words.first().map(|(w, _)| w.as_str()) {
        Some(op @ ("click" | "type" | "select" | "press"))
            if words.get(1).is_some_and(|(w, quoted)| !quoted && is_ref(w)) =>
        {
            if words.len() > 3 {
                return Err(format!("{op} takes a ref and at most one value"));
            }
            let op = match op {
                "click" => Op::Click,
                "type" => Op::Type,
                "select" => Op::Select,
                _ => Op::Press,
            };
            let text = match words.get(2) {
                None => None,
                Some((text, true)) => Some(text.clone()),
                // A key needs no quotes: `press e4 Enter`.
                Some((key, false)) if op == Op::Press => Some(key.clone()),
                Some((text, false)) => {
                    return Err(format!(
                        "the text to type or choose goes in double quotes: \"{text}\""
                    ));
                }
            };
            Some(Literal {
                text,
                op,
                ..Literal::click(&words[1].0)
            })
        }
        Some("fill") if words.get(1).is_some_and(|(w, _)| w.starts_with("--")) => {
            Some(fill_words(&words[1..])?)
        }
        _ => None,
    };
    let step = match literal {
        Some(literal) => Step {
            literal: Some(literal),
            check,
            ..Step::default()
        },
        None => {
            let mut values = BTreeMap::new();
            let mut urls = BTreeMap::new();
            for (n, (word, _)) in words.iter().filter(|(_, quoted)| *quoted).enumerate() {
                let name = format!("v{}", n + 1);
                if url::Url::parse(word).is_ok_and(|u| matches!(u.scheme(), "http" | "https")) {
                    urls.insert(name.clone(), word.clone());
                }
                values.insert(name, word.clone());
            }
            Step {
                goal: Some(head.trim().to_string()),
                values,
                urls,
                check,
                literal: None,
            }
        }
    };
    step.validate()?;
    Ok(step)
}

fn fill_words(words: &[(String, bool)]) -> Result<Literal, String> {
    let mut literal = Literal {
        op: Op::Fill,
        target: None,
        ..Literal::click("e1")
    };
    let mut at = words.iter();
    while let Some((flag, _)) = at.next() {
        let (value, _) = at.next().ok_or(format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--item" => literal.item = Some(value.clone()),
            "--press" => literal.press = Some(value.clone()),
            "--suggested" => literal.suggestion = Some(value.clone()),
            "--field" => {
                let (label, refs) = value
                    .rsplit_once('=')
                    .ok_or(format!("--field takes LABEL=eN, not {value:?}"))?;
                literal.fields.insert(
                    label.to_string(),
                    refs.split(',').map(|r| r.trim().to_string()).collect(),
                );
            }
            other => {
                return Err(format!(
                    "fill takes --item, --field, --suggested and --press, not {other}"
                ));
            }
        }
    }
    Ok(literal)
}

/// The facts a goal names as `{NAME}` outside quotes.
pub fn fact_refs(goal: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut quoted = false;
    let mut chars = goal.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if !quoted && OPEN_QUOTES.contains(&c) {
            quoted = true;
        } else if quoted && CLOSE_QUOTES.contains(&c) {
            quoted = false;
        } else if !quoted && c == '{' {
            let rest = &goal[at + 1..];
            if let Some(end) = rest.find('}') {
                let name = &rest[..end];
                if !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    if !names.iter().any(|n| n == name) {
                        names.push(name.to_string());
                    }
                    while chars.peek().is_some_and(|(i, _)| *i <= at + 1 + end) {
                        chars.next();
                    }
                }
            }
        }
    }
    names
}

const OPEN_QUOTES: [char; 2] = ['"', '\u{201c}'];
const CLOSE_QUOTES: [char; 2] = ['"', '\u{201d}'];

/// The step before its last ` -> ` outside quotes, and the check after it.
fn split_check(text: &str) -> Result<(&str, Option<&str>), String> {
    let mut quoted = false;
    let mut last = None;
    for (at, c) in text.char_indices() {
        if !quoted && OPEN_QUOTES.contains(&c) {
            quoted = true;
        } else if quoted && CLOSE_QUOTES.contains(&c) {
            quoted = false;
        } else if !quoted && text[at..].starts_with(" -> ") {
            last = Some(at);
        }
    }
    if quoted {
        return Err("a double quote is not closed".into());
    }
    Ok(match last {
        Some(at) => (&text[..at], Some(&text[at + 4..])),
        None => (text, None),
    })
}

/// Words split on spaces; text in double quotes (straight or curly) is
/// one word, marked as quoted.
fn words(text: &str) -> Result<Vec<(String, bool)>, String> {
    let mut words = Vec::new();
    let mut chars = text.chars();
    let mut word = String::new();
    while let Some(c) = chars.next() {
        if OPEN_QUOTES.contains(&c) {
            if !word.is_empty() {
                words.push((std::mem::take(&mut word), false));
            }
            let mut value = String::new();
            loop {
                match chars.next() {
                    Some(c) if CLOSE_QUOTES.contains(&c) => break,
                    Some(c) => value.push(c),
                    None => return Err("a double quote is not closed".into()),
                }
            }
            words.push((value, true));
        } else if c.is_whitespace() {
            if !word.is_empty() {
                words.push((std::mem::take(&mut word), false));
            }
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        words.push((word, false));
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_check_follows_the_last_arrow_outside_quotes() {
        let step = short_step(r#"add the note "A -> B""#).unwrap();
        assert_eq!(step.values["v1"], "A -> B");
        assert_eq!(step.check, None);
        let step = short_step("click e7 -> path: /checkout").unwrap();
        assert_eq!(step.literal.unwrap(), Literal::click("e7"));
        assert_eq!(
            step.check.unwrap(),
            Check {
                kind: CheckKind::Path,
                value: "/checkout".into(),
                require_change: false
            }
        );
        let step = short_step("type e3 \u{201c}A -> B\u{201d} -> text: Saved").unwrap();
        assert_eq!(step.literal.unwrap().text.unwrap(), "A -> B");
        assert_eq!(step.check.unwrap().value, "Saved");
    }

    #[test]
    fn the_short_form_compiles_to_literal_steps_and_goals() {
        let step = short_step(r#"select e5 "Large" -> new text: Size L"#).unwrap();
        assert_eq!(
            serde_json::to_value(&step).unwrap(),
            json!({"literal": {"op": "select", "ref": "e5", "text": "Large"},
                   "check": {"kind": "text", "value": "Size L", "require_change": true}})
        );
        let step = short_step("press e4 Enter").unwrap();
        assert_eq!(step.literal.unwrap().text.unwrap(), "Enter");
        let step =
            short_step("fill --item v2 --field number=e4 --field expiry=e5,e6 --press e9").unwrap();
        let literal = step.literal.unwrap();
        assert_eq!(literal.fields["expiry"], ["e5", "e6"]);
        assert_eq!(literal.press.unwrap(), "e9");
        let step =
            short_step(r#"open "https://shop.example/cart" and remove "Sharp Objects""#).unwrap();
        assert!(step.goal.is_some());
        assert_eq!(step.urls.len(), 1);
        assert_eq!(step.values.len(), 2);
        assert!(
            short_step("type e3 Acme")
                .unwrap_err()
                .contains("double quotes")
        );
        assert!(
            short_step("fill --item v2")
                .unwrap_err()
                .contains("--field")
        );
        assert!(short_step("click e1 -> field: amount=3").is_err());
    }

    #[test]
    fn a_step_types_a_fact_only_where_it_names_it() {
        let step = |text: &str| short_step(text).unwrap();
        let mut plan = Plan {
            steps: vec![
                step("deliver to {street} -> text: Saved"),
                step("add a note for the driver"),
                step("add the note \"{street}\""),
            ],
            facts: [("street".to_string(), "12 Elm Road".to_string())].into(),
            ..Plan::default()
        };
        plan.validate().unwrap();
        plan.resolve_facts();
        assert_eq!(plan.steps[0].values["street"], "12 Elm Road");
        assert!(plan.steps[1].values.is_empty());
        // Inside quotes it is the step's own text, as written.
        assert_eq!(plan.steps[2].values.len(), 1);
        assert_eq!(plan.steps[2].values["v1"], "{street}");
        plan.steps.push(step("deliver to {town}"));
        assert!(
            plan.validate()
                .unwrap_err()
                .contains("{town} names no fact")
        );
    }

    #[test]
    fn checks_parse_by_component() {
        assert!(Check::parse("origin: https://www.doordash.com").is_ok());
        assert!(Check::parse("origin: https://www.doordash.com/cart").is_err());
        assert!(Check::parse("query: salad").is_err());
        assert!(Check::parse("download: started").is_err());
        assert_eq!(
            Check::parse("the cart shows two items").unwrap().kind,
            CheckKind::Sentence
        );
    }

    #[test]
    fn a_plan_is_checked_whole_and_names_the_bad_step() {
        let good = Step {
            literal: Some(Literal::click("e1")),
            ..Step::default()
        };
        let bad = Step {
            literal: Some(Literal {
                op: Op::Type,
                ..Literal::click("e2")
            }),
            ..Step::default()
        };
        let plan = Plan {
            steps: vec![good.clone(), bad],
            ..Plan::default()
        };
        assert_eq!(
            plan.validate().unwrap_err(),
            "step 2: type needs the text to type"
        );
        let plan = Plan {
            steps: vec![good.clone(); 11],
            ..Plan::default()
        };
        assert!(plan.validate().unwrap_err().contains("at most 10"));
        let named = Step {
            literal: Some(Literal::click("Place order")),
            ..Step::default()
        };
        assert!(named.validate().unwrap_err().contains("not a ref"));
        let plan: Result<Plan, _> = serde_json::from_value(
            json!({"steps": [{"literal": {"op": "click", "ref": "e1", "at": [1, 2]}}]}),
        );
        assert!(plan.is_err(), "unknown fields are refused");
    }
}
