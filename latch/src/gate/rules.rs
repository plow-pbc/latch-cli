//! The owner's rules for agents on this host: plain text in the owner's
//! own words, one text for every agent and one per agent. The judge reads
//! them as text; where they come from is a [`RulesSource`]'s business.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::failure::Failure;

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
}

/// Where the rules come from, read at every judgment so an edit applies at
/// once. An error says why they could not be read, and the judgment fails.
pub trait RulesSource: Send + Sync {
    fn rules(&self, agent: &str) -> Result<Rules, String>;
}

/// The most the rules for one agent may come to: every agent's and its own.
pub const MAX_RULES: u64 = 8 * 1024;

/// The folder in the state directory that holds the rules files.
pub const RULES_DIR: &str = "rules";
/// The rules for every agent, in [`RULES_DIR`]. Each agent's own are in
/// `<agent_uid>.txt` beside it.
const HOST_FILE: &str = "host.txt";

/// Rules kept as files in the host's state directory, read at every
/// judgment: a missing file is no rules.
pub struct RuleFiles {
    dir: PathBuf,
}

impl RuleFiles {
    /// The rules folder in `state_dir`, checked once: every file is the
    /// host's or a Plow agent uid's, none resolves into a shared folder
    /// (where an agent could rewrite it), and no agent's rules come to
    /// more than [`MAX_RULES`].
    pub fn new(state_dir: &Path, shared: &[PathBuf]) -> Result<RuleFiles, Failure> {
        let dir = state_dir.join(RULES_DIR);
        let refuse = |message: String| Failure::usage("bad_rules", message);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(RuleFiles { dir }),
            Err(err) => return Err(refuse(format!("cannot read {}: {err}", dir.display()))),
        };
        let mut host = 0;
        let mut agents = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|err| refuse(format!("cannot read {}: {err}", dir.display())))?
                .path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let agent = name.strip_suffix(".txt").filter(|a| is_agent_uid(a));
            if name != HOST_FILE && agent.is_none() {
                return Err(refuse(format!(
                    "{} is neither {HOST_FILE} nor <agent uid>.txt, with a Plow agent uid (32 lowercase hex characters)",
                    path.display()
                )));
            }
            let real = path
                .canonicalize()
                .map_err(|err| refuse(format!("cannot read {}: {err}", path.display())))?;
            if let Some(folder) = shared.iter().find(|folder| real.starts_with(folder)) {
                return Err(refuse(format!(
                    "the rules file {} is inside the shared folder {}, where an agent could rewrite it",
                    path.display(),
                    folder.display()
                )));
            }
            let size = real
                .metadata()
                .map_err(|err| refuse(format!("cannot read {}: {err}", path.display())))?
                .len();
            match agent {
                Some(agent) => agents.push((agent.to_string(), size)),
                None => host = size,
            }
        }
        let largest = agents.iter().max_by_key(|(_, size)| *size);
        if host + largest.map_or(0, |(_, size)| *size) > MAX_RULES {
            let whose = largest.map_or("every agent".to_string(), |(agent, _)| {
                format!("agent {agent}")
            });
            return Err(refuse(format!(
                "the rules for {whose} come to more than {MAX_RULES} bytes"
            )));
        }
        Ok(RuleFiles { dir })
    }
}

impl RulesSource for RuleFiles {
    fn rules(&self, agent: &str) -> Result<Rules, String> {
        let read = |name: &str| match std::fs::read_to_string(self.dir.join(name)) {
            Ok(text) => Ok(Some(text)),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(format!("{name}: {err}")),
        };
        Ok(Rules {
            host: read(HOST_FILE)?,
            agent: match is_agent_uid(agent) {
                true => read(&format!("{agent}.txt"))?,
                false => None,
            },
        })
    }
}

/// Whether `id` has the shape of a Plow agent uid.
fn is_agent_uid(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
