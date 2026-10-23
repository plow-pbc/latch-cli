//! The owner's rules for agents on this host: plain text in the owner's
//! own words, one text for every agent and one per agent. The judge reads
//! them as text; where they come from is a [`RulesSource`]'s business.

use std::fs::{self, File};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::failure::{Exit, Failure};
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
}

/// Where the rules come from, read at every judgment so an edit applies at
/// once. An error says why they could not be read, and the judgment fails.
pub trait RulesSource: Send + Sync {
    fn rules(&self, agent: &str) -> Result<Rules, String>;
}

/// The same rules whoever asks: a draft's, fixed for one preview.
impl RulesSource for Rules {
    fn rules(&self, _agent: &str) -> Result<Rules, String> {
        Ok(self.clone())
    }
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
    state: PathBuf,
}

impl RuleFiles {
    /// The rules folder in `state_dir`, checked once: every `.txt` file is
    /// the host's or a Plow agent uid's, all stay inside protected state,
    /// where a caller cannot rewrite them, and no agent's rules come to
    /// more than [`MAX_RULES`].
    pub fn new(state_dir: &Path) -> Result<RuleFiles, Failure> {
        let refuse = |message: String| Failure::usage("bad_rules", message);
        let state = state_dir
            .canonicalize()
            .map_err(|err| refuse(err.to_string()))?;
        let mut files = RuleFiles {
            dir: state.join(RULES_DIR),
            state,
        };
        files.dir = match files.protected(&files.dir) {
            Ok(dir) => dir,
            Err(err) if err.kind() == ErrorKind::NotFound && !files.dir.is_symlink() => {
                return Ok(files);
            }
            Err(err) => return Err(refuse(err.to_string())),
        };
        let entries = fs::read_dir(&files.dir).map_err(|err| refuse(err.to_string()))?;
        let mut host = 0;
        let mut agents = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|err| refuse(format!("cannot read {}: {err}", files.dir.display())))?
                .path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            // Only `.txt` files are rules; others here are not the gatekeeper's.
            let Some(stem) = name.strip_suffix(".txt") else {
                continue;
            };
            let agent = Some(stem).filter(|a| is_agent_uid(a));
            if name != HOST_FILE && agent.is_none() {
                return Err(refuse(format!(
                    "{} is neither {HOST_FILE} nor <agent uid>.txt, with a Plow agent uid (32 lowercase hex characters)",
                    path.display()
                )));
            }
            let real = files
                .protected(&path)
                .map_err(|err| refuse(err.to_string()))?;
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
}

impl RulesSource for RuleFiles {
    fn rules(&self, agent: &str) -> Result<Rules, String> {
        let read = |name: &str| {
            let path = self.dir.join(name);
            match self.protected(&path).and_then(fs::read_to_string) {
                Ok(text) => Ok(Some(text)),
                Err(err) if err.kind() == ErrorKind::NotFound && !path.is_symlink() => Ok(None),
                Err(err) => Err(format!("{name}: {err}")),
            }
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

/// Which of the owner's texts a command is about.
#[derive(Clone, Debug, PartialEq)]
pub enum Scope {
    /// The rules for every agent on this host.
    Host,
    /// The rules for the agent with this uid.
    Agent(String),
}

impl Scope {
    fn file(&self) -> String {
        match self {
            Scope::Host => HOST_FILE.to_string(),
            Scope::Agent(uid) => format!("{uid}.txt"),
        }
    }
}

/// A text's revision: its sha256, as hex.
pub fn revision(text: &str) -> String {
    to_hex(&Sha256::digest(text.as_bytes()))
}

/// A change to one text, by revision: `None` is no text.
pub struct Change {
    pub before: Option<String>,
    pub after: Option<String>,
}

/// The rules folder as the owner's commands change it: every change is
/// made under one lock, and each file is replaced whole (a temporary file,
/// then a rename), so a judgment reads the old text or the new, never part.
pub struct RulesDir {
    dir: PathBuf,
}

impl RulesDir {
    pub fn new(state_dir: &Path) -> RulesDir {
        RulesDir {
            dir: state_dir.join(RULES_DIR),
        }
    }

    pub fn text(&self, scope: &Scope) -> Result<Option<String>, Failure> {
        self.read(&scope.file())
    }

    /// The peer key stored for `uid` when its rules were set, if it had one.
    pub fn peer(&self, uid: &str) -> Result<Option<String>, Failure> {
        self.read(&format!("{uid}.peer"))
    }

    /// The uids that have rules of their own.
    pub fn agents(&self) -> Result<Vec<String>, Failure> {
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

    /// Replaces `scope`'s text with `text`. With `expected`, only if the
    /// current text is still at that revision (`None`: there is none). An
    /// agent's `peer`, its peer key now, is stored beside its text.
    /// Refused, nothing changes.
    pub fn set(
        &self,
        scope: &Scope,
        text: &str,
        expected: Option<Option<&str>>,
        peer: Option<&str>,
    ) -> Result<Change, Failure> {
        if text.trim().is_empty() {
            let message = "the rules are empty; to remove them, use `latch rules clear`";
            return Err(Failure::usage("empty_rules", message.into()));
        }
        let _lock = self.lock()?;
        let before = self.current(scope, expected)?;
        let others = match scope {
            Scope::Host => {
                let mut largest = 0;
                for uid in self.agents()? {
                    let text = self.text(&Scope::Agent(uid))?.unwrap_or_default();
                    largest = largest.max(text.len());
                }
                largest
            }
            Scope::Agent(_) => self.text(&Scope::Host)?.map_or(0, |text| text.len()),
        };
        if (text.len() + others) as u64 > MAX_RULES {
            let message = format!(
                "the rules for every agent and one agent's own may come to {MAX_RULES} bytes together; these would come to {}",
                text.len() + others
            );
            return Err(Failure::usage("rules_too_long", message));
        }
        if let Scope::Agent(uid) = scope {
            let peer_file = format!("{uid}.peer");
            match peer {
                Some(key) => self.replace(&peer_file, key)?,
                None => self.remove(&peer_file)?,
            }
        }
        self.replace(&scope.file(), text)?;
        Ok(Change {
            before,
            after: Some(revision(text)),
        })
    }

    /// Removes `scope`'s text, and an agent's stored peer key. With
    /// `expected`, only if the text is still at that revision, as `set`.
    pub fn clear(&self, scope: &Scope, expected: Option<Option<&str>>) -> Result<Change, Failure> {
        let _lock = self.lock()?;
        let before = self.current(scope, expected)?;
        self.remove(&scope.file())?;
        if let Scope::Agent(uid) = scope {
            self.remove(&format!("{uid}.peer"))?;
        }
        Ok(Change {
            before,
            after: None,
        })
    }

    /// `scope`'s revision now, under the lock: refused if it is not
    /// `expected`, when there is one.
    fn current(
        &self,
        scope: &Scope,
        expected: Option<Option<&str>>,
    ) -> Result<Option<String>, Failure> {
        let before = self.text(scope)?.as_deref().map(revision);
        if let Some(expected) = expected
            && expected != before.as_deref()
        {
            let mut refused = Failure::new(
                Exit::Denied,
                "stale_revision",
                "the rules changed since they were read; nothing was changed, and the current text is kept".into(),
            );
            refused.details = serde_json::json!({"revision": before});
            return Err(refused);
        }
        Ok(before)
    }

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
        Ok(file)
    }

    fn read(&self, name: &str) -> Result<Option<String>, Failure> {
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
        Failure::failed("rules_io", format!("{}: {err}", path.display()))
    }
}

/// Whether `id` has the shape of a Plow agent uid.
pub fn is_agent_uid(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
