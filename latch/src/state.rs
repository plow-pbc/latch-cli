//! The state directory of one latch instance (`--state-dir`, default
//! `~/.latch`). Everything in it is readable by this user only:
//!
//! - `key`: this peer's ed25519 secret key, as hex. It never leaves the machine.
//! - `plow-token`: the Plow credential `login --token-file` was given (a
//!   host's owner token). An agent's own credential is not copied here; see
//!   `credential.rs`.
//! - `config.json`: the Plow API URL and this peer's record as Plow returned it.
//! - `audit.ndjson`, on a host: its audit log (see `audit.rs`).
//! - `serve.log`, on a host: what `latch serve` logs while its dashboard
//!   has the terminal.
//! - `vault.json` and `vault.key`, on a host: the vault and its custody key
//!   (see `vault/store.rs`).
//! - `jev.key`, on a dev host only: the key for the jev model provider
//!   (see `model.rs`).
//! - `browser/`, on a host: profiles, the model's settings and each
//!   session's journal.
//! - `plugins/`, on a host: the pinned engines and the Google CLI.
//! - `shares.json`: the folders `latch serve` shares when named none (see
//!   `latch shares`).
//! - `allow-read.json`: the paths it lets callers read besides, a file of
//!   its own so that a latch from before `--allow-read`, rewriting
//!   `shares.json`, leaves them alone.
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

/// What `login` remembers for later commands.
#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
    pub api_url: String,
    pub peer: Peer,
}

/// The layout of the files in a state folder. A latch reads only a folder
/// of exactly its own format: an older latch would misread a newer folder (or
/// silently drop what it does not know when it saves), and no newer latch
/// converts older folders yet. Raise it only with a change an older latch
/// would misread, together with the explicit upgrade for that change.
const FORMAT: u32 = 1;

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

    /// Refuses a folder of another format than this latch's. A folder
    /// without a marker was made before there was one, in format 1, and is
    /// marked now; a folder that does not exist yet is marked by `create`.
    pub fn check_format(&self) -> Result<(), Failure> {
        let path = self.path.join("latch.json");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                if !self.path.is_dir() {
                    return Ok(());
                }
                let marker = Marker {
                    format: FORMAT,
                    written_by: crate::cli::version().to_string(),
                };
                let text = serde_json::to_string_pretty(&marker).expect("marker serializes") + "\n";
                return write_private(&path, text.as_bytes())
                    .map_err(|err| self.io_failure(&path, err));
            }
            Err(err) => return Err(self.io_failure(&path, err)),
        };
        let marker: Marker = serde_json::from_str(&text).map_err(|err| {
            let message = format!("{} is not valid: {err}", path.display());
            Failure::failed("bad_state", message)
        })?;
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

    /// Loads this peer's key, generating and saving it on first use.
    /// Returns the key and whether it was just created.
    pub fn load_or_create_key(&self) -> Result<(SecretKey, bool), Failure> {
        let path = self.path.join("key");
        let key = SecretKey::generate();
        // `create_new` refuses to open a file that already exists, so an
        // identity is never overwritten, even if two logins race.
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path);
        match file {
            Ok(mut file) => {
                let hex = to_hex(&key.to_bytes());
                file.write_all(hex.as_bytes())
                    .map_err(|err| self.io_failure(&path, err))?;
                Ok((key, true))
            }
            Err(err) if err.kind() == ErrorKind::AlreadyExists => Ok((self.load_key()?, false)),
            Err(err) => Err(self.io_failure(&path, err)),
        }
    }

    pub fn load_key(&self) -> Result<SecretKey, Failure> {
        let path = self.path.join("key");
        let text = self.read(&path)?;
        // `SecretKey` parses itself from the hex we wrote.
        text.trim().parse().map_err(|_| {
            let message = format!("{} does not hold a valid key", path.display());
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
        let path = self.path.join("plow-token");
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(self.read(&path)?.trim().to_string()))
    }

    pub fn save_token(&self, token: &str) -> Result<(), Failure> {
        let path = self.path.join("plow-token");
        write_private(&path, token.as_bytes()).map_err(|err| self.io_failure(&path, err))
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
    /// or restored key file is caught here rather than at a host.
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
                "{}: the key file does not match the key this instance registered with Plow",
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

/// Writes a file only its owner can read. Skips the write when the file
/// already holds exactly these bytes, so a repeated `login` changes nothing.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    // `mode` only applies when the file is created; `set_permissions` covers
    // a file that already existed with looser permissions.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)
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
        assert!(written.contains("\"format\": 1"), "{written}");

        // A folder from before the marker is format 1, and is marked.
        fs::remove_file(&marker).unwrap();
        state.check_format().unwrap();
        assert_eq!(fs::read_to_string(&marker).unwrap(), written);

        for (format, fix) in [(2, "update this latch"), (0, "use the latch that wrote it")] {
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
