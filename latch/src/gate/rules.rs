//! The owner's rules for agents: the account's whole list, kept on every
//! host, each rule in the owner's own words and saying which agents and
//! which hosts it covers. A host applies the live rules that name it, and
//! the judge reads them as two texts, one for every agent and one for the
//! agent asking; where they come from is a [`RulesSource`]'s business.

use std::fs::{self, File};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::failure::{Failure, code};
use crate::state::{to_hex, write_private};

/// The owner's rules that apply to one agent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rules {
    /// Rules for every agent on this host.
    pub host: Option<String>,
    /// Rules for this agent only.
    pub agent: Option<String>,
}

impl Rules {
    pub fn is_empty(&self) -> bool {
        self.host.is_none() && self.agent.is_none()
    }

    /// How much of the budget these come to: both texts' UTF-8 bytes.
    fn bytes(&self) -> u64 {
        let len = |text: &Option<String>| text.as_deref().map_or(0, str::len);
        (len(&self.host) + len(&self.agent)) as u64
    }
}

/// Where the rules come from, read at every judgment so an edit applies at
/// once. An error says why no judgment can be made, and the judgment fails.
pub trait RulesSource: Send + Sync {
    fn rules(&self, agent: &str) -> Result<Rules, Failure>;
}

/// The same rules whoever asks: a draft's, fixed for one preview.
impl RulesSource for Rules {
    fn rules(&self, _agent: &str) -> Result<Rules, Failure> {
        Ok(self.clone())
    }
}

/// Rules to start from, by the kind of assistant: what `latch setup` offers
/// and `rules add --preset` adds. The Mac app's onboarding offers the same
/// texts (`Onboarding.decks`).
#[derive(clap::ValueEnum, Clone, Copy, PartialEq, Debug)]
pub enum Preset {
    Family,
    Work,
}

impl Preset {
    pub const ALL: [Preset; 2] = [Preset::Family, Preset::Work];

    pub fn word(self) -> &'static str {
        match self {
            Preset::Family => "family",
            Preset::Work => "work",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Preset::Family => "Family assistant",
            Preset::Work => "Work assistant",
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Preset::Family => {
                "Allow my family assistant to keep our calendar, text and email people for me, and order groceries online. Never let it share my documents or passwords with anyone."
            }
            Preset::Work => {
                "Allow my work assistant to read and send my email, keep my calendar, and work on my GitHub projects. Keep it out of my personal texts and chats."
            }
        }
    }
}

/// The most the rules for one agent on one host may come to: every
/// agent's and its own.
pub const MAX_RULES: u64 = 8 * 1024;

/// The folder in the state directory that holds the rules.
pub const RULES_DIR: &str = "rules";
/// The account's rule list, in [`RULES_DIR`].
const STORE_FILE: &str = "rules.json";
/// Before the list, a host kept its rules for every agent in this file,
/// and each agent's own in `<agent_uid>.txt` beside it.
const OLD_HOST_FILE: &str = "host.txt";

/// Which agents, or which hosts, a rule covers: all of them, those added
/// later included, or the ones named by stable id (agent uid, host key).
#[derive(Clone, Debug, PartialEq)]
pub enum Covers {
    All,
    Only(Vec<String>),
}

impl Covers {
    pub fn includes(&self, id: &str) -> bool {
        match self {
            Covers::All => true,
            Covers::Only(ids) => ids.iter().any(|named| named == id),
        }
    }
}

/// `"all"`, or the list of ids.
impl Serialize for Covers {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Covers::All => serializer.serialize_str("all"),
            Covers::Only(ids) => ids.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Covers {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Written {
            Word(String),
            Ids(Vec<String>),
        }
        match Written::deserialize(deserializer)? {
            Written::Word(word) if word == "all" => Ok(Covers::All),
            Written::Word(word) => Err(serde::de::Error::custom(format!(
                "expected \"all\" or a list of ids, not {word:?}"
            ))),
            Written::Ids(ids) => Ok(Covers::Only(ids)),
        }
    }
}

/// One of the owner's rules, as every host keeps it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Rule {
    pub id: String,
    /// Empty once the rule is removed.
    pub text: String,
    pub agents: Covers,
    pub hosts: Covers,
    /// When it last changed, in milliseconds since the epoch, by the clock
    /// of the device that changed it.
    pub updated: u64,
    /// Removed: kept, without its text, so a host that missed the removal
    /// can't bring the rule back.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
}

impl Rule {
    pub fn new(text: &str, agents: Covers, hosts: Covers) -> Rule {
        Rule {
            id: to_hex(&rand::random::<[u8; 16]>()),
            text: text.to_string(),
            agents,
            hosts,
            updated: now(),
            deleted: false,
        }
    }

    /// This rule with a new text and scope, as a change newer than it.
    pub fn edited(&self, text: &str, agents: Covers, hosts: Covers) -> Rule {
        Rule {
            text: text.to_string(),
            agents,
            hosts,
            updated: now().max(self.updated + 1),
            ..self.clone()
        }
    }

    /// This rule removed, as a change newer than it.
    pub fn removed(&self) -> Rule {
        Rule {
            text: String::new(),
            updated: now().max(self.updated + 1),
            deleted: true,
            ..self.clone()
        }
    }

    fn applies_on(&self, host: &str) -> bool {
        !self.deleted && self.hosts.includes(host)
    }

    /// Whether this version wins over `other`, another version of the same
    /// rule: the newest change, and of two changed at the same moment the
    /// one with the larger fingerprint, so every host picks the same one.
    fn newer_than(&self, other: &Rule) -> bool {
        (self.updated, self.fingerprint()) > (other.updated, other.fingerprint())
    }

    /// This version's identity: its record's sha256, as hex.
    pub fn fingerprint(&self) -> String {
        revision(&serde_json::to_string(self).expect("a rule serializes"))
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// The list as it is written down.
#[derive(Serialize, Deserialize, Default)]
struct Stored {
    rules: Vec<Rule>,
}

/// `incoming` merged into `rules` by id, the newer version of each winning,
/// removals included. Returns the ids that changed.
pub fn merge(rules: &mut Vec<Rule>, incoming: &[Rule]) -> Vec<String> {
    let mut changed = Vec::new();
    for rule in incoming {
        match rules.iter_mut().find(|held| held.id == rule.id) {
            Some(held) if rule.newer_than(held) => *held = rule.clone(),
            Some(_) => continue,
            None => rules.push(rule.clone()),
        }
        changed.push(rule.id.clone());
    }
    changed
}

/// The rules that apply on `host`, in a fixed order: oldest change first.
fn applying<'a>(rules: &'a [Rule], host: &str) -> Vec<&'a Rule> {
    let mut applying: Vec<&Rule> = rules.iter().filter(|rule| rule.applies_on(host)).collect();
    applying.sort_by(|a, b| (a.updated, &a.id).cmp(&(b.updated, &b.id)));
    applying
}

/// The rules on `host` as the judge reads them for `agent`: the texts of
/// those for every agent, and of those naming it, each joined as plain
/// paragraphs. An owner's key names no agent, so only the first applies.
pub fn render(rules: &[Rule], host: &str, agent: &str) -> Rules {
    let applying = applying(rules, host);
    let joined = |covers: &dyn Fn(&Covers) -> bool| {
        let texts: Vec<&str> = applying
            .iter()
            .filter(|rule| covers(&rule.agents))
            .map(|rule| rule.text.as_str())
            .collect();
        (!texts.is_empty()).then(|| texts.join("\n\n"))
    };
    Rules {
        host: joined(&|covers| *covers == Covers::All),
        agent: joined(
            &|covers| matches!(covers, Covers::Only(ids) if ids.iter().any(|id| id == agent)),
        ),
    }
}

/// The agents named by rules that apply on `host`, sorted.
fn named(rules: &[Rule], host: &str) -> Vec<String> {
    let mut uids: Vec<String> = applying(rules, host)
        .into_iter()
        .flat_map(|rule| match &rule.agents {
            Covers::All => Vec::new(),
            Covers::Only(ids) => ids.clone(),
        })
        .collect();
    uids.sort();
    uids.dedup();
    uids
}

/// How close the rules on one host come to [`MAX_RULES`].
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Budget {
    pub limit_bytes: u64,
    /// The rules for every agent and the largest agent's own, together.
    pub max_bytes: u64,
    pub over_budget: bool,
    /// Whose rules are over: every agent's when theirs alone are, else the
    /// agents whose own push them over.
    pub agents_over: Covers,
}

pub fn budget(rules: &[Rule], host: &str) -> Budget {
    let every = render(rules, host, "").bytes();
    let own: Vec<(String, u64)> = named(rules, host)
        .into_iter()
        .map(|uid| {
            let bytes = render(rules, host, &uid).bytes();
            (uid, bytes)
        })
        .collect();
    let max_bytes = own.iter().map(|(_, bytes)| *bytes).max().unwrap_or(every);
    let agents_over = match every > MAX_RULES {
        true => Covers::All,
        false => Covers::Only(
            own.into_iter()
                .filter(|(_, bytes)| *bytes > MAX_RULES)
                .map(|(uid, _)| uid)
                .collect(),
        ),
    };
    Budget {
        limit_bytes: MAX_RULES,
        max_bytes,
        over_budget: max_bytes > MAX_RULES,
        agents_over,
    }
}

/// Where a change would leave an agent's rules on a host over
/// [`MAX_RULES`], and bigger than before. `agent` is `None` for the rules
/// for every agent alone.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Overgrown {
    /// The host's key.
    pub key: String,
    pub agent: Option<String>,
    pub before_bytes: u64,
    pub after_bytes: u64,
}

/// What going from `before` to `after` would push over the budget on any
/// of `hosts`. A change that only shrinks rules already over it is fine,
/// so the owner can trim one rule at a time.
pub fn overgrown(before: &[Rule], after: &[Rule], hosts: &[&str]) -> Vec<Overgrown> {
    let mut found = Vec::new();
    for host in hosts {
        let mut agents = named(before, host);
        agents.extend(named(after, host));
        agents.sort();
        agents.dedup();
        let pairs = std::iter::once(None).chain(agents.into_iter().map(Some));
        for agent in pairs {
            let uid = agent.as_deref().unwrap_or_default();
            let before_bytes = render(before, host, uid).bytes();
            let after_bytes = render(after, host, uid).bytes();
            if after_bytes > MAX_RULES && after_bytes > before_bytes {
                found.push(Overgrown {
                    key: host.to_string(),
                    agent,
                    before_bytes,
                    after_bytes,
                });
            }
        }
    }
    found
}

/// The judgment a host over budget gives every caller: none. The owner
/// (`agent` is "owner") is shown the rules to trim; an agent, who can't
/// change them, is told to ask.
fn over_budget(name: &str, agent: &str, budget: &Budget) -> Failure {
    let mut refused = Failure::new(
        code::gatekeeper_unavailable,
        format!(
            "The gatekeeper on {name} could not judge this request because its rules are too long."
        ),
    );
    refused.details = json!({
        "reason": "rules_too_long",
        "host": name,
        "limit_bytes": budget.limit_bytes,
        "max_bytes": budget.max_bytes,
        "agents_over": budget.agents_over,
    });
    refused.next = Some(
        if agent == "owner" {
            format!("latch rules list --host {name}")
        } else {
            format!("Ask the owner to shorten or delete rules for {name}.")
        }
        .into(),
    );
    refused
}

/// The rules on this host, read at every judgment, as the host itself
/// keeps them in protected state.
pub struct RuleFiles {
    store: RulesDir,
    state: PathBuf,
    /// This host's name, as its refusals say it.
    name: String,
}

impl RuleFiles {
    /// The rules in `state_dir` for the host whose key is `host_key`,
    /// checked once: they stay inside protected state, where a caller
    /// cannot rewrite them, and the list can be read. A list over budget
    /// still serves, so the owner can fix it; its judgments are refused.
    /// Rules a host kept before the list are moved into it here.
    pub fn new(state_dir: &Path, host_key: &str, name: &str) -> Result<RuleFiles, Failure> {
        let refuse = |message: String| Failure::new(code::bad_rules, message);
        let state = state_dir
            .canonicalize()
            .map_err(|err| refuse(err.to_string()))?;
        let files = RuleFiles {
            store: RulesDir::new(&state, host_key),
            state,
            name: name.to_string(),
        };
        match files.protected(&files.store.dir) {
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound && !files.store.dir.is_symlink() => {
                return Ok(files);
            }
            Err(err) => return Err(refuse(err.to_string())),
        }
        files.store.list()?;
        files.read().map_err(refuse)?;
        Ok(files)
    }

    fn protected(&self, path: &Path) -> std::io::Result<PathBuf> {
        let real = path.canonicalize()?;
        if !real.starts_with(&self.state) {
            return Err(std::io::Error::other(format!(
                "{} resolves outside protected state {}",
                path.display(),
                self.state.display()
            )));
        }
        Ok(real)
    }

    fn read(&self) -> Result<Vec<Rule>, String> {
        let path = self.store.dir.join(STORE_FILE);
        match self.protected(&path).and_then(fs::read_to_string) {
            Ok(text) => parse(&text).map_err(|err| format!("{STORE_FILE}: {err}")),
            Err(err) if err.kind() == ErrorKind::NotFound && !path.is_symlink() => Ok(Vec::new()),
            Err(err) => Err(format!("{STORE_FILE}: {err}")),
        }
    }
}

impl RulesSource for RuleFiles {
    fn rules(&self, agent: &str) -> Result<Rules, Failure> {
        let rules = self.read().map_err(|why| {
            super::unavailable(&format!("the owner's rules could not be read: {why}"))
        })?;
        let budget = budget(&rules, &self.store.host);
        if budget.over_budget {
            return Err(over_budget(&self.name, agent, &budget));
        }
        Ok(render(&rules, &self.store.host, agent))
    }
}

fn parse(text: &str) -> Result<Vec<Rule>, serde_json::Error> {
    Ok(serde_json::from_str::<Stored>(text)?.rules)
}

/// A text's sha256, as hex.
fn revision(text: &str) -> String {
    to_hex(&Sha256::digest(text.as_bytes()))
}

/// The rule list in a host's state folder, as the owner's commands change
/// it: every change is made under one lock, and the list is replaced whole
/// (a temporary file, then a rename), so a judgment reads the old list or
/// the new, never part.
pub struct RulesDir {
    dir: PathBuf,
    state: PathBuf,
    /// The key of the host this folder is the state of.
    host: String,
}

impl RulesDir {
    pub fn new(state_dir: &Path, host_key: &str) -> RulesDir {
        RulesDir {
            dir: state_dir.join(RULES_DIR),
            state: state_dir.to_path_buf(),
            host: host_key.to_string(),
        }
    }

    /// The whole list, removed rules included.
    pub fn list(&self) -> Result<Vec<Rule>, Failure> {
        if self.dir.join(OLD_HOST_FILE).exists() || !self.old_agents()?.is_empty() {
            let _lock = self.lock()?;
            return self.read();
        }
        self.read()
    }

    /// One change to the list, under the lock: `change` is given the list
    /// as it is and says which rules to merge into it, the newer version
    /// of each winning, removals included. Never refused for size, so
    /// hosts always converge; refused by `change`, nothing is written.
    /// Returns the ids that changed.
    pub fn change(
        &self,
        change: impl FnOnce(&[Rule]) -> Result<Vec<Rule>, Failure>,
    ) -> Result<Vec<String>, Failure> {
        let _lock = self.lock()?;
        let mut rules = self.read()?;
        let incoming = change(&rules)?;
        let changed = merge(&mut rules, &incoming);
        if !changed.is_empty() {
            self.write(&rules)?;
        }
        Ok(changed)
    }

    /// The lock every change is made under. A host's rules from before the
    /// list are moved into it first, once.
    fn lock(&self) -> Result<File, Failure> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)
            .map_err(|err| self.failure(&self.dir, err))?;
        let path = self.dir.join(".lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|err| self.failure(&path, err))?;
        file.lock().map_err(|err| self.failure(&path, err))?;
        self.move_old()?;
        Ok(file)
    }

    /// Moves the old files into the list: the rules for every agent become
    /// one rule for every agent on this host, and each agent's one rule for
    /// that agent here, their text as it was. Each moved rule's id comes
    /// from what it moved, so a move cut short and run again adds nothing.
    /// An old file that resolves outside protected state is refused, and
    /// nothing moves.
    fn move_old(&self) -> Result<(), Failure> {
        let mut files = vec![OLD_HOST_FILE.to_string()];
        files.extend(self.old_agents()?.iter().map(|uid| format!("{uid}.txt")));
        for file in &files {
            let path = self.dir.join(file);
            if path.exists() || path.is_symlink() {
                self.protected(&path)?;
            }
        }
        let mut old = Vec::new();
        if let Some(text) = self.read_file(OLD_HOST_FILE)? {
            old.push((OLD_HOST_FILE.to_string(), Covers::All, text));
        }
        for uid in self.old_agents()? {
            let file = format!("{uid}.txt");
            if let Some(text) = self.read_file(&file)? {
                old.push((file, Covers::Only(vec![uid]), text));
            }
        }
        if old.is_empty() {
            return Ok(());
        }
        let mut rules = self.read()?;
        let moved: Vec<Rule> = old
            .iter()
            .filter(|(_, _, text)| !text.trim().is_empty())
            .map(|(file, agents, text)| Rule {
                id: revision(&format!("{} {file} {text}", self.host))[..32].to_string(),
                text: text.clone(),
                agents: agents.clone(),
                hosts: Covers::Only(vec![self.host.clone()]),
                updated: 0,
                deleted: false,
            })
            .collect();
        merge(&mut rules, &moved);
        self.write(&rules)?;
        for (file, _, _) in old {
            self.remove(&file)?;
        }
        Ok(())
    }

    /// `path` as it resolves, if inside this host's state folder.
    fn protected(&self, path: &Path) -> Result<PathBuf, Failure> {
        let refuse = |message: String| Failure::new(code::bad_rules, message);
        let state = self
            .state
            .canonicalize()
            .map_err(|err| refuse(format!("{}: {err}", self.state.display())))?;
        let real = path
            .canonicalize()
            .map_err(|err| refuse(format!("{}: {err}", path.display())))?;
        if !real.starts_with(&state) {
            return Err(refuse(format!(
                "{} resolves outside protected state {}",
                path.display(),
                state.display()
            )));
        }
        Ok(real)
    }

    /// The uids with an old `<agent_uid>.txt`.
    fn old_agents(&self) -> Result<Vec<String>, Failure> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(self.failure(&self.dir, err)),
        };
        let mut uids = Vec::new();
        for entry in entries {
            let name = entry
                .map_err(|err| self.failure(&self.dir, err))?
                .file_name();
            let name = name.to_string_lossy();
            if let Some(uid) = name.strip_suffix(".txt").filter(|uid| is_agent_uid(uid)) {
                uids.push(uid.to_string());
            }
        }
        uids.sort();
        Ok(uids)
    }

    fn read(&self) -> Result<Vec<Rule>, Failure> {
        let Some(text) = self.read_file(STORE_FILE)? else {
            return Ok(Vec::new());
        };
        parse(&text).map_err(|err| {
            let path = self.dir.join(STORE_FILE);
            Failure::new(code::bad_rules, format!("{}: {err}", path.display()))
        })
    }

    fn write(&self, rules: &[Rule]) -> Result<(), Failure> {
        let stored = Stored {
            rules: rules.to_vec(),
        };
        let text = serde_json::to_string_pretty(&stored).expect("the rules serialize");
        self.replace(STORE_FILE, &text)
    }

    fn read_file(&self, name: &str) -> Result<Option<String>, Failure> {
        let path = self.dir.join(name);
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(self.failure(&path, err)),
        }
    }

    /// Writes `name` whole (see `state::write_private`).
    fn replace(&self, name: &str, text: &str) -> Result<(), Failure> {
        let path = self.dir.join(name);
        write_private(&path, text.as_bytes()).map_err(|err| self.failure(&path, err))
    }

    fn remove(&self, name: &str) -> Result<(), Failure> {
        let path = self.dir.join(name);
        match fs::remove_file(&path) {
            Err(err) if err.kind() != ErrorKind::NotFound => Err(self.failure(&path, err)),
            _ => Ok(()),
        }
    }

    fn failure(&self, path: &Path, err: std::io::Error) -> Failure {
        Failure::new(code::rules_io, format!("{}: {err}", path.display()))
    }
}

/// The host operation that adds a rule with `text`, for every agent, or
/// for `agent` alone, on every host: for tests of a host's side.
#[cfg(test)]
pub(crate) fn adding(text: &str, agent: Option<&str>) -> serde_json::Value {
    let agents = agent.map_or(Covers::All, |uid| Covers::Only(vec![uid.to_string()]));
    json!({"command": "add", "rules": [Rule::new(text, agents, Covers::All)]})
}

/// Whether `id` has the shape of a Plow agent uid.
pub fn is_agent_uid(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod preset_tests {
    use super::Preset;

    /// The Mac app's onboarding offers each preset by the same name, with
    /// the same text, in the same order.
    #[test]
    fn the_mac_app_offers_the_same_presets() {
        let onboarding = include_str!("../../gui/Sources/PlowLatchUI/App/Onboarding.swift");
        let mut from = 0;
        for preset in Preset::ALL {
            let deck = format!(
                "Deck(name: \"{}\",\n                 rules: \"{}\",",
                preset.name(),
                preset.text()
            );
            let at = onboarding[from..]
                .find(&deck)
                .unwrap_or_else(|| panic!("Onboarding.swift has no deck {deck}"));
            from += at + deck.len();
        }
        assert_eq!(onboarding.matches("Deck(name: ").count(), Preset::ALL.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STUDIO: &str = "studio-key";
    const CLOUD: &str = "cloud-key";
    const ADA: &str = "0123456789abcdef0123456789abcdef";
    const BOB: &str = "fedcba9876543210fedcba9876543210";

    fn only(id: &str) -> Covers {
        Covers::Only(vec![id.to_string()])
    }

    #[test]
    fn a_host_applies_only_live_rules_that_name_it() {
        let every = Rule {
            updated: 1,
            ..Rule::new("Never send email.", Covers::All, Covers::All)
        };
        let cloud_only = Rule {
            updated: 2,
            ..Rule::new("Quiet after 9 PM.", Covers::All, only(CLOUD))
        };
        let ada_here = Rule::new("Only pytest.", only(ADA), only(STUDIO));
        let gone = Rule::new("Anything goes.", Covers::All, Covers::All).removed();
        let rules = [every, cloud_only, ada_here, gone];

        assert_eq!(
            render(&rules, STUDIO, ADA),
            Rules {
                host: Some("Never send email.".into()),
                agent: Some("Only pytest.".into()),
            }
        );
        assert_eq!(render(&rules, STUDIO, BOB).agent, None);
        assert_eq!(
            render(&rules, CLOUD, ADA),
            Rules {
                host: Some("Never send email.\n\nQuiet after 9 PM.".into()),
                agent: None,
            }
        );
    }

    #[test]
    fn the_newest_version_of_a_rule_wins_removals_included() {
        let first = Rule {
            updated: 10,
            ..Rule::new("one", Covers::All, Covers::All)
        };
        let edited = Rule {
            text: "two".into(),
            updated: 20,
            ..first.clone()
        };
        let removed = edited.removed();

        let mut held = vec![first.clone()];
        assert_eq!(
            merge(&mut held, std::slice::from_ref(&edited)),
            vec![first.id.clone()]
        );
        assert_eq!(held, vec![edited.clone()]);
        merge(&mut held, std::slice::from_ref(&removed));
        // A host that missed the removal can't bring the rule back.
        assert!(merge(&mut held, &[edited.clone(), first.clone()]).is_empty());
        assert_eq!(held, vec![removed.clone()]);
        assert_eq!(render(&held, STUDIO, ADA), Rules::default());

        // Two changes at the same moment: every host keeps the same one.
        let rival = Rule {
            text: "three".into(),
            ..edited.clone()
        };
        let (mut a, mut b) = (vec![edited.clone()], vec![rival.clone()]);
        merge(&mut a, &[rival]);
        merge(&mut b, &[edited]);
        assert_eq!(a, b);
    }

    #[test]
    fn a_merge_into_the_list_is_kept_newest_first_and_never_refused_for_size() {
        let dir = tempfile::tempdir().unwrap();
        let reopened = || RulesDir::new(dir.path(), STUDIO).list().unwrap();
        let store = RulesDir::new(dir.path(), STUDIO);
        let first = Rule {
            updated: 10,
            ..Rule::new("one", Covers::All, Covers::All)
        };
        let edited = Rule {
            text: "two".into(),
            updated: 20,
            ..first.clone()
        };

        assert_eq!(
            store.change(|_| Ok(vec![first.clone()])).unwrap(),
            vec![first.id.clone()]
        );
        assert_eq!(reopened(), vec![first.clone()]);
        store.change(|_| Ok(vec![edited.clone()])).unwrap();
        assert_eq!(reopened(), vec![edited.clone()]);

        // A stale version changes nothing, and nothing is written.
        assert!(
            store
                .change(|_| Ok(vec![first.clone()]))
                .unwrap()
                .is_empty()
        );
        assert_eq!(reopened(), vec![edited.clone()]);

        // A removal is kept, and a stale copy can't bring the rule back.
        let removed = edited.removed();
        store.change(|_| Ok(vec![removed.clone()])).unwrap();
        assert!(
            store
                .change(|_| Ok(vec![edited.clone()]))
                .unwrap()
                .is_empty()
        );
        assert_eq!(reopened(), vec![removed]);

        // Rules that come to more than the budget together are kept.
        let long = |agents| Rule::new(&"r".repeat(5000), agents, Covers::All);
        store.change(|_| Ok(vec![long(Covers::All)])).unwrap();
        store.change(|_| Ok(vec![long(only(ADA))])).unwrap();
        assert!(budget(&reopened(), STUDIO).over_budget);

        // Refused by the change, nothing is written.
        let refused = store.change(|_| Err(Failure::new(code::rules_io, "no".into())));
        assert_eq!(refused.unwrap_err().code, "rules_io");
        assert_eq!(reopened().len(), 3);
    }

    #[test]
    fn a_change_is_refused_only_where_it_pushes_a_pair_over_the_budget() {
        let every = Rule::new(&"e".repeat(3000), Covers::All, Covers::All);
        let ada = Rule::new(&"a".repeat(5000), only(ADA), only(STUDIO));
        let before = vec![every.clone(), ada.clone()];
        assert!(!budget(&before, STUDIO).over_budget);

        // Bob's own, on studio, would come to 3000 + 5193 there; on cloud
        // the rule doesn't apply.
        let bob = Rule::new(&"b".repeat(5193), only(BOB), only(STUDIO));
        let mut after = before.clone();
        merge(&mut after, &[bob]);
        assert_eq!(
            overgrown(&before, &after, &[STUDIO, CLOUD]),
            vec![Overgrown {
                key: STUDIO.into(),
                agent: Some(BOB.into()),
                before_bytes: 3000,
                after_bytes: 8193,
            }]
        );

        // Growing the rules for every agent pushes Ada's pair over.
        let longer = Rule {
            text: "e".repeat(3193),
            updated: every.updated + 1,
            ..every.clone()
        };
        let mut after = before.clone();
        merge(&mut after, std::slice::from_ref(&longer));
        let found = overgrown(&before, &after, &[STUDIO]);
        assert_eq!(
            found
                .iter()
                .map(|over| (over.agent.as_deref(), over.after_bytes))
                .collect::<Vec<_>>(),
            vec![(Some(ADA), 8193)]
        );

        // Over already, a change that shrinks it is fine, though it stays
        // over; one that grows it further is not.
        let mut over = after;
        let ada_longer = Rule {
            text: "a".repeat(5100),
            updated: ada.updated + 1,
            ..ada.clone()
        };
        merge(&mut over, &[ada_longer]);
        let shorter = Rule {
            text: "a".repeat(5050),
            updated: ada.updated + 2,
            ..ada.clone()
        };
        let mut trimmed = over.clone();
        merge(&mut trimmed, std::slice::from_ref(&shorter));
        assert!(overgrown(&over, &trimmed, &[STUDIO]).is_empty());
        let mut grown = over.clone();
        merge(
            &mut grown,
            &[Rule {
                text: "a".repeat(5200),
                ..shorter
            }],
        );
        assert_eq!(overgrown(&over, &grown, &[STUDIO]).len(), 1);
    }

    #[test]
    fn the_budget_names_whose_rules_are_over() {
        let ada = Rule::new(&"a".repeat(9000), only(ADA), Covers::All);
        let rules = [ada.clone()];
        let ada_over = budget(&rules, STUDIO);
        assert_eq!(
            (
                ada_over.max_bytes,
                ada_over.over_budget,
                ada_over.agents_over
            ),
            (9000, true, only(ADA))
        );
        let every = Rule::new(&"e".repeat(8200), Covers::All, Covers::All);
        assert_eq!(budget(&[every, ada], STUDIO).agents_over, Covers::All);
        assert_eq!(
            serde_json::to_value(budget(&[], STUDIO)).unwrap(),
            json!({"limit_bytes": 8192, "max_bytes": 0, "over_budget": false, "agents_over": []})
        );
    }
}
