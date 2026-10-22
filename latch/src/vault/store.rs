//! The vault's backend interface, and latch's own store behind it.
//!
//! The own store is one file in the host's state directory, `vault.json`,
//! written only by `latch serve`, atomically (a temporary file renamed over
//! it). Every item is sealed with AES-256-GCM under a random **data key**;
//! the data key is sealed under a **custody key**, kept with the state
//! folder's other secrets (see `secrets.rs`): in the Keychain for a signed
//! macOS latch, else in `vault.key` beside it, owner-only (0600). The state
//! directory is one `exec` and file workers can never read.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[cfg(test)]
use super::classify;
use super::item::{Item, Meta};
use crate::failure::{Exit, Failure};
use crate::seal::{Sealed, from_hex, seal};
use crate::secrets::{Secret, Secrets};
use crate::state::{to_hex, write_private};

/// What every vault backend offers. Deciding a release (which item, into
/// which page) is never a backend's job; it hands over one value when the
/// vault has decided.
pub trait Backend: Send {
    /// Every item's metadata; never a value.
    fn list(&self) -> Vec<Meta>;
    fn add(&mut self, item: Item) -> Result<Meta, Failure>;
    /// Replaces the item whose revision is `revision`; an edit against an
    /// older revision is refused.
    fn update(&mut self, item: Item, revision: u64) -> Result<Meta, Failure>;
    fn remove(&mut self, id: &str, revision: u64) -> Result<(), Failure>;
    /// The whole item, values included, for the vault's own edits (an
    /// import updating a changed password); never handed to an agent.
    fn item(&self, id: &str) -> Option<Item>;
    /// Exactly one value: the field `label` of item `id`. Tests only: a
    /// fill reads its values from the item it loaded (`browser::fill`).
    #[cfg(test)]
    fn release(&self, id: &str, label: &str) -> Result<String, Failure>;
    /// Compares secrets without handing them out: which of `password` and
    /// `totp` differ from the item's.
    fn differs(&self, id: &str, password: &str, totp: &str) -> Result<(bool, bool), Failure>;
}

const FILE: &str = "vault.json";
/// The `vault.json` layout this latch reads and writes; a vault of another
/// version is refused, never misread or rewritten without what it holds.
const VERSION: u32 = 1;
/// Binds every sealed blob to what it is.
const DATA_KEY_AAD: &[u8] = b"latch vault data key v1";
const ITEMS_AAD: &[u8] = b"latch vault items v1";

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    data_key: Sealed,
    items: Sealed,
    /// The number the next item's id gets, so a removed id is never given
    /// again.
    next_id: u64,
}

pub struct OwnStore {
    dir: PathBuf,
    data_key: [u8; 32],
    items: Vec<Item>,
    next_id: u64,
}

fn failed(message: String) -> Failure {
    Failure::failed("vault_failed", message)
}

fn open(key: &[u8; 32], sealed: &Sealed, aad: &[u8]) -> Result<Vec<u8>, Failure> {
    crate::seal::open(key, &sealed.nonce, &sealed.ciphertext, aad)
        .ok_or_else(|| failed("the vault could not be opened with its custody key".into()))
}

/// Reads the custody key of the vault in `dir`. It is read on each use,
/// not kept.
fn custody(dir: &Path) -> Result<[u8; 32], Failure> {
    let Some(bytes) = Secrets::open(dir)?.read(Secret::VaultKey)? else {
        return Err(failed(format!(
            "{}: the vault has no custody key",
            dir.display()
        )));
    };
    let text = String::from_utf8(bytes).map_err(|_| failed("the custody key is damaged".into()))?;
    from_hex(text.trim())
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| failed("the custody key is damaged".into()))
}

impl OwnStore {
    /// Opens the vault in `dir` (the host's state directory), creating an
    /// empty one with new keys on first use.
    pub fn open(dir: &Path) -> Result<OwnStore, Failure> {
        let path = dir.join(FILE);
        if !path.exists() {
            let custody: [u8; 32] = rand::random();
            Secrets::open(dir)?.write(Secret::VaultKey, to_hex(&custody).as_bytes())?;
            let mut store = OwnStore {
                dir: dir.to_path_buf(),
                data_key: rand::random(),
                items: Vec::new(),
                next_id: 1,
            };
            store.save()?;
            return Ok(store);
        }
        let custody = custody(dir)?;
        let bytes = fs::read(&path).map_err(|err| failed(format!("{}: {err}", path.display())))?;
        // The version first: another version may lay the rest out differently.
        #[derive(Deserialize)]
        struct Versioned {
            version: u32,
        }
        let Versioned { version } = serde_json::from_slice(&bytes)
            .map_err(|_| failed("the vault file is damaged".into()))?;
        if version != VERSION {
            let fix = if version > VERSION {
                "update this latch"
            } else {
                "use the latch that wrote it"
            };
            let message = format!(
                "{} is vault version {}; this latch reads only version {VERSION}: {fix}",
                path.display(),
                version
            );
            return Err(Failure::failed("vault_version", message));
        }
        let file: File = serde_json::from_slice(&bytes)
            .map_err(|_| failed("the vault file is damaged".into()))?;
        let data_key: [u8; 32] = open(&custody, &file.data_key, DATA_KEY_AAD)?
            .try_into()
            .map_err(|_| failed("the vault's data key is damaged".into()))?;
        let items: Vec<Item> = serde_json::from_slice(&open(&data_key, &file.items, ITEMS_AAD)?)
            .map_err(|_| failed("the vault's items are damaged".into()))?;
        Ok(OwnStore {
            dir: dir.to_path_buf(),
            data_key,
            items,
            next_id: file.next_id,
        })
    }

    fn save(&mut self) -> Result<(), Failure> {
        let custody = custody(&self.dir)?;
        let items = serde_json::to_vec(&self.items).expect("items serialize");
        let file = File {
            version: VERSION,
            data_key: seal(&custody, &self.data_key, DATA_KEY_AAD),
            items: seal(&self.data_key, &items, ITEMS_AAD),
            next_id: self.next_id,
        };
        let path = self.dir.join(FILE);
        write_private(&path, &serde_json::to_vec(&file).expect("file serializes"))
            .map_err(|err| failed(format!("{}: {err}", path.display())))
    }

    fn find(&self, id: &str) -> Result<usize, Failure> {
        self.items
            .iter()
            .position(|item| item.id == id)
            .ok_or_else(|| Failure::usage("no_such_item", format!("there is no item {id}")))
    }

    fn at_revision(&self, at: usize, revision: u64) -> Result<(), Failure> {
        if self.items[at].revision != revision {
            return Err(Failure::new(
                Exit::Denied,
                "stale_revision",
                format!(
                    "item {} is at revision {}, not {revision}; list it again",
                    self.items[at].id, self.items[at].revision
                ),
            ));
        }
        Ok(())
    }
}

impl Backend for OwnStore {
    fn list(&self) -> Vec<Meta> {
        self.items.iter().map(Item::meta).collect()
    }

    fn add(&mut self, mut item: Item) -> Result<Meta, Failure> {
        item.validate()?;
        item.id = format!("v{}", self.next_id);
        item.revision = 1;
        self.next_id += 1;
        self.items.push(item);
        if let Err(failure) = self.save() {
            self.items.pop();
            return Err(failure);
        }
        Ok(self.items.last().expect("just added").meta())
    }

    fn update(&mut self, mut item: Item, revision: u64) -> Result<Meta, Failure> {
        let at = self.find(&item.id)?;
        self.at_revision(at, revision)?;
        item.validate()?;
        item.revision = revision + 1;
        let old = std::mem::replace(&mut self.items[at], item);
        if let Err(failure) = self.save() {
            self.items[at] = old;
            return Err(failure);
        }
        Ok(self.items[at].meta())
    }

    fn remove(&mut self, id: &str, revision: u64) -> Result<(), Failure> {
        let at = self.find(id)?;
        self.at_revision(at, revision)?;
        let old = self.items.remove(at);
        if let Err(failure) = self.save() {
            self.items.insert(at, old);
            return Err(failure);
        }
        Ok(())
    }

    fn item(&self, id: &str) -> Option<Item> {
        self.items.iter().find(|item| item.id == id).cloned()
    }

    #[cfg(test)]
    fn release(&self, id: &str, label: &str) -> Result<String, Failure> {
        let item = &self.items[self.find(id)?];
        // An authenticator key is no form field: the vault computes codes
        // from it, so it is released only to the vault itself.
        let value = if label == classify::TOTP {
            item.raw
                .login
                .as_ref()
                .and_then(|l| l.totp.clone())
                .filter(|t| !t.is_empty())
        } else {
            classify::read(&item.raw, label)
        };
        value.ok_or_else(|| {
            Failure::usage(
                "no_such_field",
                format!("item {id} has no field {label:?}; list it to see its fields"),
            )
        })
    }

    fn differs(&self, id: &str, password: &str, totp: &str) -> Result<(bool, bool), Failure> {
        let item = &self.items[self.find(id)?];
        let login = item.raw.login.clone().unwrap_or_default();
        let password_differs = login.password.unwrap_or_default() != password;
        let stored = login.totp.unwrap_or_default();
        // One key in two spellings (bare base32, or the otpauth link) is no
        // change; a source without a key does not remove the stored one.
        let totp_differs = !totp.is_empty() && !super::totp::same_key(&stored, totp);
        Ok((password_differs, totp_differs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::item::{LOGIN, Login, Raw};

    fn login(password: &str) -> Item {
        Item {
            id: String::new(),
            revision: 0,
            sites: vec!["example.com".into()],
            other_top_sites: vec![],
            raw: Raw {
                kind: LOGIN,
                name: "Example".into(),
                login: Some(Login {
                    username: Some("lovelace".into()),
                    password: Some(password.into()),
                    ..Login::default()
                }),
                ..Raw::default()
            },
        }
    }

    #[test]
    fn items_are_sealed_on_disk_and_reopen_with_the_custody_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = OwnStore::open(dir.path()).unwrap();
        let meta = store.add(login("made-up-pass-4417")).unwrap();
        assert_eq!((meta.item.as_str(), meta.revision), ("v1", 1));
        let on_disk = fs::read_to_string(dir.path().join(FILE)).unwrap();
        for value in ["made-up-pass-4417", "lovelace", "Example", "example.com"] {
            assert!(
                !on_disk.contains(value),
                "{value} is in the file in the clear"
            );
        }
        use std::os::unix::fs::PermissionsExt;
        for file in [FILE, Secret::VaultKey.file()] {
            let mode = fs::metadata(dir.path().join(file))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{file}");
        }
        let reopened = OwnStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.release("v1", "password").unwrap(),
            "made-up-pass-4417"
        );
        assert_eq!(reopened.list(), store.list());
        // Another custody key opens nothing.
        fs::write(dir.path().join(Secret::VaultKey.file()), to_hex(&[7u8; 32])).unwrap();
        assert_eq!(
            OwnStore::open(dir.path()).err().unwrap().code,
            "vault_failed"
        );
    }

    #[test]
    fn a_removed_id_is_never_given_again_even_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = OwnStore::open(dir.path()).unwrap();
        store.add(login("made-up-pass-1")).unwrap();
        let second = store.add(login("made-up-pass-2")).unwrap();
        assert_eq!(second.item, "v2");
        store.remove("v2", second.revision).unwrap();
        drop(store);
        let mut reopened = OwnStore::open(dir.path()).unwrap();
        assert_eq!(reopened.add(login("made-up-pass-3")).unwrap().item, "v3");
    }

    #[test]
    fn a_vault_without_its_next_id_is_refused_without_changes() {
        let dir = tempfile::tempdir().unwrap();
        OwnStore::open(dir.path()).unwrap();
        let path = dir.path().join(FILE);
        let mut file: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        file.as_object_mut().unwrap().remove("next_id");
        let bytes = serde_json::to_vec(&file).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert_eq!(
            OwnStore::open(dir.path()).err().unwrap().code,
            "vault_failed"
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn a_vault_of_another_version_is_refused_and_left_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        OwnStore::open(dir.path())
            .unwrap()
            .add(login("made-up-pass-4417"))
            .unwrap();
        let path = dir.path().join(FILE);
        let mut file: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        // A newer layout: another version, its items no longer where they were.
        file["version"] = 2.into();
        file["sealed_items"] = file["items"].take();
        file.as_object_mut().unwrap().remove("items");
        let newer = serde_json::to_vec(&file).unwrap();
        fs::write(&path, &newer).unwrap();

        let refused = OwnStore::open(dir.path()).err().unwrap();
        assert_eq!(refused.code, "vault_version");
        assert!(refused.message.contains("version 2"), "{}", refused.message);
        assert!(
            refused.message.contains("update this latch"),
            "{}",
            refused.message
        );
        assert_eq!(fs::read(&path).unwrap(), newer);
    }

    #[test]
    fn items_saved_with_bound_profiles_still_open() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = OwnStore::open(dir.path()).unwrap();
        store.add(login("made-up-pass-4417")).unwrap();
        // Items once named the browser profiles that could use them.
        let mut items = serde_json::to_value(&store.items).unwrap();
        items[0]["profiles"] = serde_json::json!(["default"]);
        let path = dir.path().join(FILE);
        let mut file: File = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        file.items = seal(
            &store.data_key,
            &serde_json::to_vec(&items).unwrap(),
            ITEMS_AAD,
        );
        fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        let reopened = OwnStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.release("v1", "password").unwrap(),
            "made-up-pass-4417"
        );
    }

    #[test]
    fn an_edit_against_an_old_revision_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = OwnStore::open(dir.path()).unwrap();
        store.add(login("one")).unwrap();
        let mut edited = login("two");
        edited.id = "v1".into();
        assert_eq!(store.update(edited.clone(), 1).unwrap().revision, 2);
        let stale = store.update(edited, 1).err().unwrap();
        assert_eq!(
            (stale.exit, stale.code.as_str()),
            (Exit::Denied, "stale_revision")
        );
        assert_eq!(store.remove("v1", 1).err().unwrap().code, "stale_revision");
        store.remove("v1", 2).unwrap();
        assert!(store.list().is_empty());
        assert_eq!(
            store.release("v1", "password").err().unwrap().code,
            "no_such_item"
        );
    }
}
