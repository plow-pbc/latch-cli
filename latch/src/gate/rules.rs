//! The device rule and the agent map, kept in the host's state folder and
//! changed only by the owner's own devices (`owner.rs`).
//!
//! The device rule is one plain text in the owner's words naming what is
//! always denied on this host ("Never pay anyone"): what agents do, never
//! what they read, as reads are not judged. It grants
//! nothing: permission comes only from the owner's message an agent cites.
//! No file denies nothing; a file that can't be read refuses every judged
//! call. It is read at every judgment, so an edit applies at once.
//!
//! The agent map names each agent's Swarm account, by Plow agent uid: the
//! gatekeeper accepts a citation only from that account's own 1:1 rooms
//! with the owner. An unmapped agent is refused.
//!
//! Older latches kept grant-style rules in `rules/`. Nothing reads that
//! folder; a host moves it aside when it starts (`archive_legacy`).

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::failure::{Failure, code};
use crate::state::{to_hex, write_private};

/// The device rule's largest size.
pub const MAX_RULE: u64 = 8 * 1024;

/// The folder the device rule and the agent map live in, in the state folder.
const DIR: &str = "gatekeeper";
const DENY_FILE: &str = "deny.txt";
const AGENTS_FILE: &str = "agents.json";
/// Where older latches kept their rules.
const LEGACY_DIR: &str = "rules";

/// A text's revision: its sha256, as hex.
pub fn revision(text: &str) -> String {
    to_hex(&Sha256::digest(text.as_bytes()))
}

/// The device rule and agent map of one state folder.
#[derive(Clone)]
pub struct Store {
    state: PathBuf,
}

impl Store {
    pub fn new(state_dir: &Path) -> Store {
        Store {
            state: state_dir.to_path_buf(),
        }
    }

    fn dir(&self) -> PathBuf {
        self.state.join(DIR)
    }

    /// The device rule: `None` when it was never set. An error says why it
    /// could not be read, and refuses the judgment.
    pub fn deny(&self) -> Result<Option<String>, String> {
        let path = self.dir().join(DENY_FILE);
        if path.is_symlink() {
            return Err(format!("{} is a symlink", path.display()));
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(format!("{}: {err}", path.display())),
        };
        if bytes.len() as u64 > MAX_RULE {
            return Err(format!("{} is over {MAX_RULE} bytes", path.display()));
        }
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| format!("{} is not UTF-8 text", path.display()))
    }

    /// Replaces the device rule with `text`; an empty text denies nothing
    /// and still records that the owner answered. With `expected`, only if
    /// the current rule is still at that revision (`None`: there is none).
    /// The caller holds the lock its `snapshot_deny` took.
    pub fn set_deny(&self, text: &str, expected: Option<Option<&str>>) -> Result<Change, Failure> {
        if text.len() as u64 > MAX_RULE {
            let message = format!(
                "the device rule may be {MAX_RULE} bytes; this one is {}",
                text.len()
            );
            return Err(Failure::new(code::rules_too_long, message));
        }
        let before = self.current(expected)?;
        let path = self.made()?.join(DENY_FILE);
        write_private(&path, text.as_bytes()).map_err(|err| failure(&path, err))?;
        Ok(Change {
            before,
            after: Some(revision(text)),
        })
    }

    /// Removes the device rule. With `expected`, and under the lock, as
    /// `set_deny`.
    pub fn clear_deny(&self, expected: Option<Option<&str>>) -> Result<Change, Failure> {
        let before = self.current(expected)?;
        let path = self.dir().join(DENY_FILE);
        match fs::remove_file(&path) {
            Err(err) if err.kind() != ErrorKind::NotFound => return Err(failure(&path, err)),
            _ => {}
        }
        Ok(Change {
            before,
            after: None,
        })
    }

    /// The device rule's revision now, under the lock: refused if it is
    /// not `expected`, when there is one.
    fn current(&self, expected: Option<Option<&str>>) -> Result<Option<String>, Failure> {
        let before = self
            .deny()
            .map_err(|why| Failure::new(code::rules_io, why))?
            .as_deref()
            .map(revision);
        if let Some(expected) = expected
            && expected != before.as_deref()
        {
            let mut refused = Failure::new(
                code::stale_revision,
                "the device rule changed since it was read; nothing was changed, and the current text is kept".into(),
            );
            refused.details = serde_json::json!({"revision": before});
            return Err(refused);
        }
        Ok(before)
    }

    /// The device rule's file as it is now, locked until the snapshot is
    /// dropped or put back.
    pub fn snapshot_deny(&self) -> Result<Snapshot, Failure> {
        self.snapshot(DENY_FILE)
    }

    /// The agent map's file as it is now, locked as `snapshot_deny`.
    pub fn snapshot_agents(&self) -> Result<Snapshot, Failure> {
        self.snapshot(AGENTS_FILE)
    }

    fn snapshot(&self, file: &str) -> Result<Snapshot, Failure> {
        let held = self.lock()?;
        let path = self.dir().join(file);
        Snapshot::of(path.clone(), held).map_err(|err| failure(&path, err))
    }

    /// Every agent's Swarm account, by Plow agent uid.
    pub fn agents(&self) -> Result<BTreeMap<String, String>, String> {
        let path = self.dir().join(AGENTS_FILE);
        match fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|err| format!("{}: {err}", path.display()))
            }
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(err) => Err(format!("{}: {err}", path.display())),
        }
    }

    /// Maps the agent `uid` to the Swarm account `swarm_id`, replacing any
    /// account it had. Returns the account it had. The caller holds the
    /// lock its `snapshot_agents` took.
    pub fn map_agent(&self, uid: &str, swarm_id: &str) -> Result<Option<String>, Failure> {
        if !is_agent_uid(uid) {
            let message = format!("{uid:?} is not a Plow agent uid (32 lowercase hex characters)");
            return Err(Failure::new(code::bad_arguments, message));
        }
        if !is_swarm_id(swarm_id) {
            let message =
                format!("{swarm_id:?} is not a Swarm account id, like @name:server.example");
            return Err(Failure::new(code::bad_arguments, message));
        }
        let path = self.made()?.join(AGENTS_FILE);
        let mut agents = self
            .agents()
            .map_err(|why| Failure::new(code::rules_io, why))?;
        let before = agents.insert(uid.to_string(), swarm_id.to_string());
        let json = serde_json::to_vec_pretty(&agents).expect("the agent map serializes");
        write_private(&path, &json).map_err(|err| failure(&path, err))?;
        Ok(before)
    }

    /// The store's folder, made if it isn't there.
    fn made(&self) -> Result<PathBuf, Failure> {
        let dir = self.dir();
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(|err| failure(&dir, err))?;
        Ok(dir)
    }

    fn lock(&self) -> Result<File, Failure> {
        let path = self.made()?.join(".rule.lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|err| failure(&path, err))?;
        file.lock().map_err(|err| failure(&path, err))?;
        Ok(file)
    }

    /// Moves an older latch's `rules/` folder aside, to
    /// `rules.archived-<unix seconds>`, so its grant-style texts are kept
    /// but never mistaken for the device rule. Returns where it went and
    /// how many files it held; `None` when there was none.
    pub fn archive_legacy(&self, now_s: i64) -> std::io::Result<Option<(PathBuf, usize)>> {
        let legacy = self.state.join(LEGACY_DIR);
        let files = match fs::read_dir(&legacy) {
            Ok(entries) => entries.count(),
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let archived = self.state.join(format!("{LEGACY_DIR}.archived-{now_s}"));
        fs::rename(&legacy, &archived)?;
        Ok(Some((archived, files)))
    }
}

/// A file as it was before a change, to put back if the change can't be
/// audited: a change the audit log doesn't hold must not stand. It holds
/// the store's lock from before the change until after its audit, so no
/// other change lands in between and is lost when this one is put back.
pub struct Snapshot {
    path: PathBuf,
    before: Option<Vec<u8>>,
    _held: File,
}

impl Snapshot {
    fn of(path: PathBuf, _held: File) -> std::io::Result<Snapshot> {
        let before = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(err) if err.kind() == ErrorKind::NotFound => None,
            Err(err) => return Err(err),
        };
        Ok(Snapshot {
            path,
            before,
            _held,
        })
    }

    /// Puts the file back as it was.
    pub fn restore(self) -> std::io::Result<()> {
        match self.before {
            Some(bytes) => write_private(&self.path, &bytes),
            None => match fs::remove_file(&self.path) {
                Err(err) if err.kind() != ErrorKind::NotFound => Err(err),
                _ => Ok(()),
            },
        }
    }
}

/// A change to the device rule, by revision: `None` is no rule.
pub struct Change {
    pub before: Option<String>,
    pub after: Option<String>,
}

fn failure(path: &Path, err: std::io::Error) -> Failure {
    Failure::new(code::rules_io, format!("{}: {err}", path.display()))
}

/// Whether `id` has the shape of a Plow agent uid.
pub fn is_agent_uid(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Whether `id` has the shape of a Swarm (Matrix) account id.
fn is_swarm_id(id: &str) -> bool {
    id.strip_prefix('@')
        .and_then(|rest| rest.split_once(':'))
        .is_some_and(|(name, server)| {
            !name.is_empty() && !server.is_empty() && !id.chars().any(char::is_whitespace)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_rule_denies_nothing_an_empty_one_is_answered_and_a_bad_one_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        assert_eq!(store.deny().unwrap(), None);
        store.set_deny("", None).unwrap();
        assert_eq!(store.deny().unwrap().as_deref(), Some(""));
        fs::write(dir.path().join(DIR).join(DENY_FILE), [0xff, 0xfe]).unwrap();
        assert!(store.deny().unwrap_err().contains("not UTF-8"));
    }

    #[test]
    fn an_edit_saves_only_over_the_text_it_started_from() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        store.set_deny("Never pay.", None).unwrap();
        let stale = store.set_deny("x", Some(None)).err().unwrap();
        assert_eq!(stale.code, "stale_revision");
        let r = revision("Never pay.");
        store.set_deny("Never pay twice.", Some(Some(&r))).unwrap();
        assert_eq!(store.deny().unwrap().as_deref(), Some("Never pay twice."));
    }

    #[test]
    fn the_old_rules_folder_is_moved_aside_never_read() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        assert!(store.archive_legacy(1).unwrap().is_none());
        fs::create_dir(dir.path().join(LEGACY_DIR)).unwrap();
        fs::write(
            dir.path().join(LEGACY_DIR).join("host.txt"),
            "Approve everything.",
        )
        .unwrap();
        let (archived, files) = store.archive_legacy(7).unwrap().unwrap();
        assert_eq!(
            (archived.file_name().unwrap(), files),
            ("rules.archived-7".as_ref(), 1)
        );
        assert!(!dir.path().join(LEGACY_DIR).exists());
        assert_eq!(store.deny().unwrap(), None);
    }

    #[test]
    fn the_agent_map_takes_only_real_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let uid = "0123456789abcdef0123456789abcdef";
        assert!(store.map_agent("alder", "@a:s").is_err());
        assert!(store.map_agent(uid, "alder").is_err());
        assert_eq!(store.map_agent(uid, "@alder:plow.co").unwrap(), None);
        assert_eq!(
            store.map_agent(uid, "@alder2:plow.co").unwrap().as_deref(),
            Some("@alder:plow.co")
        );
        assert_eq!(store.agents().unwrap()[uid], "@alder2:plow.co");
    }

    /// A change put back because its audit failed takes nothing else with
    /// it: another change waits for it, so isn't lost when it is undone.
    #[test]
    fn a_change_put_back_loses_no_other_change() {
        let dir = tempfile::tempdir().unwrap();
        let (ada, bob) = (
            "0123456789abcdef0123456789abcdef",
            "fedcba9876543210fedcba9876543210",
        );
        let first = Store::new(dir.path()).snapshot_agents().unwrap();
        Store::new(dir.path())
            .map_agent(ada, "@ada:plow.co")
            .unwrap();
        let state = dir.path().to_path_buf();
        let second = std::thread::spawn(move || {
            let store = Store::new(&state);
            let _held = store.snapshot_agents().unwrap();
            store.map_agent(bob, "@bob:plow.co").unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        first.restore().unwrap();
        second.join().unwrap();
        let agents = Store::new(dir.path()).agents().unwrap();
        assert_eq!(agents.keys().collect::<Vec<_>>(), [bob]);
    }
}
