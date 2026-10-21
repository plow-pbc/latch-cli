//! The state directory of one latch instance (`--state-dir`, default
//! `~/.latch`). Everything in it is readable by this user only. A latch
//! signed for the Keychain keeps the secrets marked * there instead of
//! in files (see `secrets.rs`):
//!
//! - `key`*: this peer's ed25519 secret key, as hex. It never leaves the machine.
//! - `plow-token`*: the Plow credential `login --token-file` was given (a
//!   host's owner token). An agent's own credential is not copied here; see
//!   `credential.rs`.
//! - `config.json`: the Plow API URL and this peer's record as Plow returned it.
//! - `audit.ndjson`, on a host: its audit log, each record sealed (see
//!   `audit.rs`); `audit.keys`*: the keys that seal it.
//! - `serve.log`, on a host: what `latch serve` logs while its dashboard
//!   has the terminal.
//! - `vault.json` and `vault.key`*, on a host: the vault and its custody key
//!   (see `vault/store.rs`).
//! - `jev.key`, on a dev host only: the key for the jev model provider
//!   (see `model.rs`).
//! - `browser/model.json`, on a host: which decision model it asks (see
//!   `model.rs`).
//! - `plugins/`, on a host: the pinned Google CLI.
//! - `shares.json`: the folders `latch serve` shares when named none (see
//!   `latch shares`).
//! - `allow-read.json`: the paths it lets callers read besides, a file of
//!   its own so that a latch from before `--allow-read`, rewriting
//!   `shares.json`, leaves them alone.
//! - `secrets.lock`: held while a Keychain latch moves secret files in (see
//!   `secrets.rs`).
//! - `latch.json`: the folder's format (see `FORMAT`), which every command
//!   checks before it reads or writes anything else.
//!
//! A host has no config file: what it shares is named on `latch serve`'s
//! command line.

use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use iroh::SecretKey;
use serde::{Deserialize, Serialize};

use crate::failure::Failure;
use crate::plow::Peer;
use crate::secrets::{Secret, Secrets};

/// What `login` remembers for later commands.
#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
    pub api_url: String,
    pub peer: Peer,
}

/// The layout of the files in a state folder. A latch reads only a folder
/// of exactly its own format: an older latch would misread a newer folder (or
/// silently drop what it does not know when it saves). Raise it only with a
/// change an older latch would misread, together with the explicit upgrade
/// for that change.
///
/// Format 2 seals the audit log. A folder of format 1 (a plaintext log) is
/// upgraded on first use by `audit::upgrade`, and marked 2 only once that is
/// done, so an upgrade cut short is picked up next time.
const FORMAT: u32 = 2;

/// What `latch serve` shares when it is named no folders, and what it lets
/// callers read besides (`--allow-read`).
#[derive(Debug, PartialEq, Default)]
pub struct Shares {
    pub folders: Vec<PathBuf>,
    pub allow_read: Vec<PathBuf>,
}

/// `shares.json`.
#[derive(Serialize, Deserialize, Default)]
struct SharedFolders {
    folders: Vec<PathBuf>,
}

/// `allow-read.json`.
#[derive(Serialize, Deserialize, Default)]
struct AllowRead {
    paths: Vec<PathBuf>,
}

/// `latch.json`: the folder's format, and which latch first marked it.
#[derive(Serialize, Deserialize)]
struct Marker {
    format: u32,
    written_by: String,
}

pub struct StateDir {
    pub path: PathBuf,
}

/// Whether `dir` is a latch state folder: it holds latch's `latch.json`
/// marker, or, made before there was one, both a key and a Plow login.
pub fn is_state_folder(dir: &Path) -> bool {
    let marked = fs::read_to_string(dir.join("latch.json"))
        .is_ok_and(|text| serde_json::from_str::<Marker>(&text).is_ok());
    marked || (dir.join("key").is_file() && dir.join("plow-token").is_file())
}

impl StateDir {
    /// `--state-dir` if given, else `~/.latch`.
    pub fn new(flag: Option<PathBuf>) -> Result<StateDir, Failure> {
        let path = match flag {
            Some(path) => path,
            None => match std::env::home_dir() {
                Some(home) => home.join(".latch"),
                None => {
                    let message = "no home directory; pass --state-dir".to_string();
                    return Err(Failure::usage("no_state_dir", message));
                }
            },
        };
        Ok(StateDir { path })
    }

    /// Creates the directory (owner-only) if it does not exist yet.
    pub fn create(&self) -> Result<(), Failure> {
        fs::create_dir_all(&self.path).map_err(|err| self.io_failure(&self.path, err))?;
        let owner_only = fs::Permissions::from_mode(0o700);
        fs::set_permissions(&self.path, owner_only)
            .map_err(|err| self.io_failure(&self.path, err))?;
        self.check_format()
    }

    /// Refuses a folder of another format than this latch's, and upgrades
    /// one of format 1 (see `FORMAT`). A folder without a marker was made
    /// before there was one, in format 1; a folder that does not exist yet
    /// is marked by `create`.
    pub fn check_format(&self) -> Result<(), Failure> {
        let path = self.path.join("latch.json");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                if !self.path.is_dir() {
                    return Ok(());
                }
                // A folder from before there was a marker is format 1.
                return self.upgraded();
            }
            Err(err) => return Err(self.io_failure(&path, err)),
        };
        let marker: Marker = serde_json::from_str(&text).map_err(|err| {
            let message = format!("{} is not valid: {err}", path.display());
            Failure::failed("bad_state", message)
        })?;
        if marker.format == 1 {
            return self.upgraded();
        }
        let fix = match marker.format.cmp(&FORMAT) {
            std::cmp::Ordering::Equal => return Ok(()),
            std::cmp::Ordering::Greater => "update this latch",
            std::cmp::Ordering::Less => "use the latch that wrote it",
        };
        let message = format!(
            "{} is in state format {}, set up by latch {}; this latch {} reads only format {FORMAT}: \
             {fix}, or pass another --state-dir. Nothing was read or changed",
            self.path.display(),
            marker.format,
            marker.written_by,
            crate::cli::version(),
        );
        Err(Failure::failed("state_format", message))
    }

    /// Where this folder keeps its secrets (see `secrets.rs`).
    pub fn secrets(&self) -> Result<Secrets, Failure> {
        Secrets::open(&self.path)
    }

    /// Upgrades a format-1 folder (see `FORMAT`) and marks it this format.
    /// An audit log that can't be converted (it fails its checks, say)
    /// leaves the folder at format 1, says so, and lets the command go on:
    /// only what writes the audit (a host, a local `rules` command) refuses
    /// it, when it opens the log. The next command tries again.
    fn upgraded(&self) -> Result<(), Failure> {
        if let Err(failure) = crate::audit::upgrade(&self.path) {
            eprintln!(
                "latch: warning: the audit log was not converted to this latch's sealed \
                 format yet, and until it is, commands that write it are refused: {}",
                failure.message
            );
            return Ok(());
        }
        let path = self.path.join("latch.json");
        let marker = Marker {
            format: FORMAT,
            written_by: crate::cli::version().to_string(),
        };
        let text = serde_json::to_string_pretty(&marker).expect("marker serializes") + "\n";
        write_private(&path, text.as_bytes()).map_err(|err| self.io_failure(&path, err))
    }

    /// Loads this peer's key, generating and saving it on first use.
    /// Returns the key and whether it was just created.
    pub fn load_or_create_key(&self) -> Result<(SecretKey, bool), Failure> {
        let key = SecretKey::generate();
        let hex = to_hex(&key.to_bytes());
        if self.secrets()?.create(Secret::Key, hex.as_bytes())? {
            Ok((key, true))
        } else {
            Ok((self.load_key()?, false))
        }
    }

    pub fn load_key(&self) -> Result<SecretKey, Failure> {
        let Some(bytes) = self.secrets()?.read(Secret::Key)? else {
            let message = format!(
                "{} has no key; run `latch login` first",
                self.path.display()
            );
            return Err(Failure::failed("state_io", message));
        };
        // `SecretKey` parses itself from the hex we wrote.
        let parsed = String::from_utf8(bytes)
            .ok()
            .and_then(|text| text.trim().parse().ok());
        parsed.ok_or_else(|| {
            let message = format!("{}: its key is not a valid key", self.path.display());
            Failure::failed("bad_state", message)
        })
    }

    /// What `latch shares set` saved, folders in order; none if it never ran.
    pub fn load_shares(&self) -> Result<Shares, Failure> {
        let folders: SharedFolders = self.load_json("shares.json")?;
        let allow_read: AllowRead = self.load_json("allow-read.json")?;
        Ok(Shares {
            folders: folders.folders,
            allow_read: allow_read.paths,
        })
    }

    pub fn save_shares(&self, shares: &Shares) -> Result<(), Failure> {
        let folders = SharedFolders {
            folders: shares.folders.clone(),
        };
        let allow_read = AllowRead {
            paths: shares.allow_read.clone(),
        };
        self.save_json("shares.json", &folders)?;
        self.save_json("allow-read.json", &allow_read)
    }

    /// The file `name` in the state folder, or the default if there is none.
    fn load_json<T: serde::de::DeserializeOwned + Default>(
        &self,
        name: &str,
    ) -> Result<T, Failure> {
        let path = self.path.join(name);
        if !path.exists() {
            return Ok(T::default());
        }
        let text = self.read(&path)?;
        serde_json::from_str(&text).map_err(|err| {
            let message = format!("{} is not valid: {err}", path.display());
            Failure::failed("bad_state", message)
        })
    }

    fn save_json(&self, name: &str, value: &impl Serialize) -> Result<(), Failure> {
        let path = self.path.join(name);
        let text = serde_json::to_string_pretty(value).expect("state serializes") + "\n";
        write_private(&path, text.as_bytes()).map_err(|err| self.io_failure(&path, err))
    }

    /// The token saved by `login --token-file`, if there is one.
    pub fn load_token(&self) -> Result<Option<String>, Failure> {
        let Some(bytes) = self.secrets()?.read(Secret::PlowToken)? else {
            return Ok(None);
        };
        let text = String::from_utf8(bytes).map_err(|_| {
            let message = format!("{}: its Plow token is not text", self.path.display());
            Failure::failed("bad_state", message)
        })?;
        Ok(Some(text.trim().to_string()))
    }

    pub fn save_token(&self, token: &str) -> Result<(), Failure> {
        self.secrets()?.write(Secret::PlowToken, token.as_bytes())
    }

    /// The saved config, or `None` before the first successful `login`.
    pub fn load_config(&self) -> Result<Option<Config>, Failure> {
        let path = self.path.join("config.json");
        if !path.exists() {
            return Ok(None);
        }
        let text = self.read(&path)?;
        let config = serde_json::from_str(&text).map_err(|err| {
            let message = format!("{} is not valid: {err}", path.display());
            Failure::failed("bad_state", message)
        })?;
        Ok(Some(config))
    }

    pub fn save_config(&self, config: &Config) -> Result<(), Failure> {
        let path = self.path.join("config.json");
        // Serializing our own plain struct cannot fail.
        let text = serde_json::to_string_pretty(config).expect("config serializes") + "\n";
        write_private(&path, text.as_bytes()).map_err(|err| self.io_failure(&path, err))
    }

    /// Like `load_config`, for commands that need a completed `login`. Also
    /// checks that the key on disk is the one Plow registered, so a replaced
    /// or restored key is caught here rather than at a host.
    pub fn require_login(&self) -> Result<Config, Failure> {
        let Some(config) = self.load_config()? else {
            let message = format!(
                "{} is not logged in; run `latch login` first",
                self.path.display()
            );
            return Err(Failure::usage("not_logged_in", message));
        };
        if self.load_key()?.public().to_string() != config.peer.key {
            let message = format!(
                "{}: the key does not match the key this instance registered with Plow",
                self.path.display()
            );
            return Err(Failure::failed("key_mismatch", message));
        }
        Ok(config)
    }

    fn read(&self, path: &Path) -> Result<String, Failure> {
        fs::read_to_string(path).map_err(|err| self.io_failure(path, err))
    }

    fn io_failure(&self, path: &Path, err: std::io::Error) -> Failure {
        Failure::failed("state_io", format!("{}: {err}", path.display()))
    }
}

/// Writes a file only its owner can read, atomically (`<name>.tmp`
/// renamed over it). Skips the write when the file already holds exactly
/// these bytes, so a repeated `login` changes nothing.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    // `mode` applies only when the file is created.
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temp, path)
}

/// Bytes as lowercase hex, two characters per byte.
pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test state lives under the repo's gitignored `.dev/`, like every dev run.
    fn test_dir() -> tempfile::TempDir {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
        fs::create_dir_all(&parent).unwrap();
        tempfile::tempdir_in(parent).unwrap()
    }

    #[test]
    fn an_older_latch_rewriting_the_folders_leaves_the_reads_alone() {
        let dir = test_dir();
        let state = StateDir {
            path: dir.path().to_path_buf(),
        };
        let shares = Shares {
            folders: vec![PathBuf::from("/work")],
            allow_read: vec![PathBuf::from("/refs")],
        };
        state.save_shares(&shares).unwrap();
        assert_eq!(state.load_shares().unwrap(), shares);
        // What a latch from before `--allow-read` writes for `shares set /other`.
        fs::write(dir.path().join("shares.json"), r#"{"folders": ["/other"]}"#).unwrap();
        assert_eq!(
            state.load_shares().unwrap(),
            Shares {
                folders: vec![PathBuf::from("/other")],
                allow_read: vec![PathBuf::from("/refs")],
            }
        );
    }

    #[test]
    fn a_folder_is_marked_once_and_one_of_another_format_is_refused() {
        let dir = test_dir();
        let state = StateDir::new(Some(dir.path().join("state"))).unwrap();
        let marker = state.path.join("latch.json");
        // Nothing to read yet, and nothing made: `create` marks the folder.
        state.check_format().unwrap();
        assert!(!state.path.exists());
        state.create().unwrap();
        let written = fs::read_to_string(&marker).unwrap();
        assert!(written.contains("\"format\": 2"), "{written}");

        // A folder from before the marker is format 1, upgraded and marked.
        fs::remove_file(&marker).unwrap();
        state.check_format().unwrap();
        assert_eq!(fs::read_to_string(&marker).unwrap(), written);

        for (format, fix) in [(3, "update this latch"), (0, "use the latch that wrote it")] {
            let other = format!(r#"{{"format": {format}, "written_by": "9.9.9"}}"#);
            fs::write(&marker, &other).unwrap();
            let refused = state.check_format().unwrap_err();
            assert_eq!(refused.code, "state_format");
            assert!(refused.message.contains(fix), "{}", refused.message);
            assert!(refused.message.contains("9.9.9"), "{}", refused.message);
            assert_eq!(fs::read_to_string(&marker).unwrap(), other);
        }
    }

    #[test]
    fn a_format_1_folder_is_marked_2_only_once_its_audit_is_sealed() {
        let dir = test_dir();
        let state = StateDir::new(Some(dir.path().join("state"))).unwrap();
        fs::create_dir(&state.path).unwrap();
        let marker = state.path.join("latch.json");
        let last = crate::audit::write_plaintext_log(&state.path, &["connect", "exec"]);
        let audit = state.path.join("audit.ndjson");

        // A tampered old log is not converted: the folder stays 1, the
        // command goes on, and opening the log to write it is refused.
        let good = fs::read_to_string(&audit).unwrap();
        fs::write(&audit, good.replacen("connect", "oops", 1)).unwrap();
        fs::write(&marker, r#"{"format": 1, "written_by": "0.1.0"}"#).unwrap();
        state.check_format().unwrap();
        assert!(
            fs::read_to_string(&marker)
                .unwrap()
                .contains("\"format\": 1")
        );
        let refused = crate::audit::AuditLog::open(audit.clone()).err().unwrap();
        assert_eq!(refused.code, "audit_broken");

        fs::write(&audit, &good).unwrap();
        state.check_format().unwrap();
        assert!(
            fs::read_to_string(&marker)
                .unwrap()
                .contains("\"format\": 2")
        );
        let sealed = fs::read_to_string(&audit).unwrap();
        assert!(!sealed.contains("old-caller"), "{sealed}");
        let records = crate::audit::read_all(&audit).unwrap();
        assert_eq!(records[0].args["last_hash"], last);
        assert_eq!(records.len(), 3);
    }

    #[test]
    fn key_is_created_once_and_private() {
        let dir = test_dir();
        let state = StateDir::new(Some(dir.path().join("state"))).unwrap();
        state.create().unwrap();

        let (first, created) = state.load_or_create_key().unwrap();
        assert!(created);
        let (again, created_again) = state.load_or_create_key().unwrap();
        assert!(!created_again);
        assert_eq!(first.public(), again.public());

        let mode = fs::metadata(state.path.join("key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let dir_mode = fs::metadata(&state.path).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o777, 0o700);
    }
}
