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
//! - `plow-url`: the Plow a saved token belongs to, written with it, so a
//!   login that fails after the token arrived is retried against that Plow.
//! - `audit.sealed.ndjson`, on a host: its audit log, each record sealed
//!   (see `audit.rs`); `audit.keys`*: the keys that seal it; `audit.lock`:
//!   held by what writes it. A plaintext `audit.ndjson` from an older latch
//!   is never read, and every command says it can be deleted.
//! - `serve.log`, on a host: what a managed host logs (`latch logs`).
//! - `serve.plist` (macOS) and `launch.lock`, on a host: the managed host's
//!   service, and the lock that starts and stops it one at a time (see
//!   `managed.rs`).
//! - `settings.json`: this machine's host settings (see `settings.rs`);
//!   `keep-awake.json`, on a host: what it holds (see `keep_awake.rs`).
//! - `vault.json` and `vault.key`*, on a host: the vault and its custody key
//!   (see `vault/store.rs`).
//! - `browser/`, on a host: open sessions' profiles, the model's settings
//!   and each session's journal.
//! - `plugins/`, on a host: the pinned engines and the Google CLI.
//! - `latch.json`: the folder's format (see `FORMAT`), which every command
//!   checks before it reads or writes anything else.

use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use iroh::SecretKey;
use serde::{Deserialize, Serialize};

use crate::failure::{Failure, code};
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
/// silently drop what it does not know when it saves), and no newer latch
/// converts older folders. Raise it with a change an older latch would misread.
const FORMAT: u32 = 2;

/// `latch.json`: the folder's format, and which latch first marked it.
#[derive(Serialize, Deserialize)]
struct Marker {
    format: u32,
    written_by: String,
}

pub struct StateDir {
    pub path: PathBuf,
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
                    return Err(Failure::new(code::no_state_dir, message));
                }
            },
        };
        Ok(StateDir { path })
    }

    /// Creates the directory (owner-only) if it does not exist yet.
    pub fn create(&self) -> Result<(), Failure> {
        self.check_format()?;
        fs::create_dir_all(&self.path).map_err(|err| self.io_failure(&self.path, err))?;
        let owner_only = fs::Permissions::from_mode(0o700);
        fs::set_permissions(&self.path, owner_only)
            .map_err(|err| self.io_failure(&self.path, err))?;
        if !self.path.join("latch.json").exists() {
            self.save_json(
                "latch.json",
                &Marker {
                    format: FORMAT,
                    written_by: crate::cli::version().to_string(),
                },
            )?;
        }
        Ok(())
    }

    /// Refuses unmarked peer state and folders of another format.
    /// Missing, empty or plugin-only folders are initialized only by `create`.
    pub fn check_format(&self) -> Result<(), Failure> {
        let path = self.path.join("latch.json");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                match fs::read_dir(&self.path) {
                    Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
                    Ok(mut entries) => {
                        if entries.all(|entry| {
                            entry.is_ok_and(|entry| {
                                entry.file_name() == "plugins" && entry.path().is_dir()
                            })
                        }) {
                            return Ok(());
                        }
                    }
                    Err(err) => return Err(self.io_failure(&self.path, err)),
                }
                return Err(Failure::new(
                    code::state_format,
                    format!(
                        "{} has no latch.json format marker; pass a fresh --state-dir. Nothing was read or changed",
                        self.path.display()
                    ),
                ));
            }
            Err(err) => return Err(self.io_failure(&path, err)),
        };
        let marker: Marker = serde_json::from_str(&text).map_err(|err| {
            let message = format!("{} is not valid: {err}", path.display());
            Failure::new(code::bad_state, message)
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
        Err(Failure::new(code::state_format, message))
    }

    /// Where this folder keeps its secrets (see `secrets.rs`).
    pub fn secrets(&self) -> Result<Secrets, Failure> {
        Secrets::open(&self.path)
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
            return Err(Failure::new(code::state_io, message));
        };
        // `SecretKey` parses itself from the hex we wrote.
        let parsed = String::from_utf8(bytes)
            .ok()
            .and_then(|text| text.trim().parse().ok());
        parsed.ok_or_else(|| {
            let message = format!("{}: its key is not a valid key", self.path.display());
            Failure::new(code::bad_state, message)
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
            Failure::new(code::bad_state, message)
        })?;
        Ok(Some(text.trim().to_string()))
    }

    /// Saves a Plow token and the Plow it belongs to.
    pub fn save_token(&self, token: &str, api_url: &str) -> Result<(), Failure> {
        self.secrets()?.write(Secret::PlowToken, token.as_bytes())?;
        let path = self.path.join("plow-url");
        write_private(&path, format!("{api_url}\n").as_bytes())
            .map_err(|err| self.io_failure(&path, err))
    }

    /// The Plow the saved token belongs to, if one was recorded with it.
    pub fn load_plow_url(&self) -> Result<Option<String>, Failure> {
        let path = self.path.join("plow-url");
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(self.read(&path)?.trim().to_string()))
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
            Failure::new(code::bad_state, message)
        })?;
        Ok(Some(config))
    }

    pub fn save_config(&self, config: &Config) -> Result<(), Failure> {
        let path = self.path.join("config.json");
        // Serializing our own plain struct cannot fail.
        let text = serde_json::to_string_pretty(config).expect("config serializes") + "\n";
        write_private(&path, text.as_bytes()).map_err(|err| self.io_failure(&path, err))
    }

    /// Like `load_config`, for commands that need a completed `login`. Reads
    /// no secret: a call to this machine's own host over its socket needs
    /// none.
    pub fn require_config(&self) -> Result<Config, Failure> {
        self.load_config()?.ok_or_else(|| {
            let message = format!(
                "{} is not logged in; run `latch login` first",
                self.path.display()
            );
            Failure::new(code::not_logged_in, message)
        })
    }

    /// `require_config`, and checks that the key on disk is the one Plow
    /// registered, so a replaced or restored key is caught here rather than
    /// at a host.
    pub fn require_login(&self) -> Result<Config, Failure> {
        let config = self.require_config()?;
        if self.load_key()?.public().to_string() != config.peer.key {
            let message = format!(
                "{}: the key does not match the key this instance registered with Plow",
                self.path.display()
            );
            return Err(Failure::new(code::key_mismatch, message));
        }
        Ok(config)
    }

    fn read(&self, path: &Path) -> Result<String, Failure> {
        fs::read_to_string(path).map_err(|err| self.io_failure(path, err))
    }

    fn io_failure(&self, path: &Path, err: std::io::Error) -> Failure {
        Failure::new(code::state_io, format!("{}: {err}", path.display()))
    }
}

/// Writes a file only its owner can read, atomically and durably: into a
/// temporary file of this write's own beside it (0600), synced, renamed
/// over it, and the folder synced, so the new file is on disk before
/// anything that relies on it (audit records sealed under a new key) is.
/// Concurrent writers never share a temporary file. Skips the write when
/// the file already holds exactly these bytes, so a repeated `login`
/// changes nothing.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".");
    let mut temp = tempfile::Builder::new()
        .prefix(&name)
        .suffix(".tmp")
        .tempfile_in(dir)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|err| err.error)?;
    fs::File::open(dir)?.sync_all()
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
    fn a_folder_is_marked_once_and_one_of_another_format_is_refused() {
        let dir = test_dir();
        let state = StateDir::new(Some(dir.path().join("state"))).unwrap();
        let marker = state.path.join("latch.json");
        // Nothing to read yet, and nothing made: `create` marks the folder.
        state.check_format().unwrap();
        assert!(!state.path.exists());
        state.create().unwrap();
        let written = fs::read_to_string(&marker).unwrap();
        assert!(
            written.contains(&format!("\"format\": {FORMAT}")),
            "{written}"
        );
        state.check_format().unwrap();
        state.create().unwrap();
        assert_eq!(fs::read_to_string(&marker).unwrap(), written);

        fs::remove_file(&marker).unwrap();
        fs::create_dir(state.path.join("plugins")).unwrap();
        fs::write(state.path.join("key"), "private state").unwrap();
        assert_eq!(state.check_format().unwrap_err().code, "state_format");
        assert_eq!(state.create().unwrap_err().code, "state_format");
        assert!(!marker.exists());

        for (format, fix) in [
            (FORMAT + 1, "update this latch"),
            (FORMAT - 1, "use the latch that wrote it"),
        ] {
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
    fn two_writers_at_once_each_leave_a_whole_file() {
        let dir = test_dir();
        let path = dir.path().join("config.json");
        // Big enough that one writer's bytes can land amid the other's.
        let contents = [vec![b'a'; 1 << 20], vec![b'b'; 1 << 20]];
        std::thread::scope(|scope| {
            for bytes in &contents {
                let path = &path;
                scope.spawn(move || {
                    for _ in 0..50 {
                        write_private(path, bytes).unwrap();
                        let _ = fs::remove_file(path);
                    }
                });
            }
            scope.spawn(|| {
                for _ in 0..500 {
                    if let Ok(read) = fs::read(&path) {
                        assert!(contents.contains(&read), "a mixed or partial file");
                    }
                }
            });
        });
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert!(names.len() <= 1, "a temporary file was left: {names:?}");
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
