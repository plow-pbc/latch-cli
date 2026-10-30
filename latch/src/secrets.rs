//! Where a state folder keeps its secrets: this peer's key, the Plow token
//! and the vault's custody key.
//!
//! A latch signed with Plow's keychain group (the release's Plow Latch.app,
//! `latch/macos`) keeps them in the macOS data-protection Keychain, under
//! that group, readable after the first unlock and never synced. Every
//! other build (a dev build, Linux) keeps them as owner-only files in the
//! state folder, as before. The build decides, by its own signature; there
//! is no setting.
//!
//! A Keychain build never writes a secret file, and never reads one either:
//! secret files a dev build left are not its own. If the Keychain refuses,
//! the command fails and says so.
//!
//! An item is filed under the state folder's real path, so another folder
//! on the same Mac, or a copy of this one, never shares its identity.

use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::failure::{Failure, code};
use crate::state::write_private;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Secret {
    /// This peer's ed25519 secret key, as hex.
    Key,
    /// The Plow credential `login` was given.
    PlowToken,
    /// The vault's custody key, as hex.
    VaultKey,
    /// The audit log's keys, by id (see `audit.rs`).
    AuditKeys,
}

impl Secret {
    pub const ALL: [Secret; 4] = [
        Secret::Key,
        Secret::PlowToken,
        Secret::VaultKey,
        Secret::AuditKeys,
    ];
    /// This peer's login, without which it is logged out. Its key is its
    /// identity, which a login again keeps.
    pub const LOGIN: [Secret; 1] = [Secret::PlowToken];

    /// The file it is kept in by a build without the Keychain.
    pub fn file(self) -> &'static str {
        match self {
            Secret::Key => "key",
            Secret::PlowToken => "plow-token",
            Secret::VaultKey => "vault.key",
            Secret::AuditKeys => "audit.keys",
        }
    }
}

/// A Keychain-like store of named items: the real Keychain, or the tests'.
trait Store: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, String>;
    /// Adds the item; `false` if one is already there, which is left alone.
    fn add(&self, account: &str, bytes: &[u8]) -> Result<bool, String>;
    /// Adds the item or replaces its value.
    fn set(&self, account: &str, bytes: &[u8]) -> Result<(), String>;
    /// Deletes the item; `false` if there was none.
    fn remove(&self, account: &str) -> Result<bool, String>;
}

/// One state folder's secrets.
pub struct Secrets {
    dir: PathBuf,
    /// `None`: files.
    keychain: Option<Box<dyn Store>>,
}

impl Secrets {
    /// The secrets of the state folder `dir`, where this build keeps them.
    pub fn open(dir: &Path) -> Result<Secrets, Failure> {
        Ok(Secrets {
            dir: dir.to_path_buf(),
            keychain: platform::keychain()?,
        })
    }

    pub fn read(&self, secret: Secret) -> Result<Option<Vec<u8>>, Failure> {
        let Some(store) = &self.keychain else {
            let path = self.dir.join(secret.file());
            return match fs::read(&path) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
                Err(err) => Err(io_failure(&path, err)),
            };
        };
        store
            .get(&self.account(secret))
            .map_err(|why| self.refused("read", secret, &why))
    }

    /// Saves `secret`, replacing what was there.
    pub fn write(&self, secret: Secret, bytes: &[u8]) -> Result<(), Failure> {
        let Some(store) = &self.keychain else {
            let path = self.dir.join(secret.file());
            return write_private(&path, bytes).map_err(|err| io_failure(&path, err));
        };
        store
            .set(&self.account(secret), bytes)
            .map_err(|why| self.refused("save", secret, &why))
    }

    /// Saves `secret` only if there is none yet; `false` if there was, which
    /// is kept. An identity is never overwritten, even if two logins race.
    pub fn create(&self, secret: Secret, bytes: &[u8]) -> Result<bool, Failure> {
        let Some(store) = &self.keychain else {
            let path = self.dir.join(secret.file());
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path);
            return match file {
                Ok(mut file) => file
                    .write_all(bytes)
                    .and_then(|()| file.sync_all())
                    .map(|()| true)
                    .map_err(|err| io_failure(&path, err)),
                Err(err) if err.kind() == ErrorKind::AlreadyExists => Ok(false),
                Err(err) => Err(io_failure(&path, err)),
            };
        };
        store
            .add(&self.account(secret), bytes)
            .map_err(|why| self.refused("save", secret, &why))
    }

    /// Deletes each of `secrets` this folder has, where this build keeps
    /// them. Returns the files they are named by, of those there were.
    pub fn remove_each(&self, secrets: &[Secret]) -> Result<Vec<&'static str>, Failure> {
        let mut removed = Vec::new();
        for &secret in secrets {
            let was = match &self.keychain {
                Some(store) => store
                    .remove(&self.account(secret))
                    .map_err(|why| self.refused("delete", secret, &why))?,
                None => match fs::remove_file(self.dir.join(secret.file())) {
                    Ok(()) => true,
                    Err(err) if err.kind() == ErrorKind::NotFound => false,
                    Err(err) => return Err(io_failure(&self.dir.join(secret.file()), err)),
                },
            };
            if was {
                removed.push(secret.file());
            }
        }
        Ok(removed)
    }

    /// Whether this build keeps its secrets in the Keychain.
    pub fn in_keychain(&self) -> bool {
        self.keychain.is_some()
    }

    /// The item's account: the state folder's real path and the file name.
    fn account(&self, secret: Secret) -> String {
        format!("{}/{}", real_path(&self.dir).display(), secret.file())
    }

    fn refused(&self, what: &str, secret: Secret, why: &str) -> Failure {
        let message = format!(
            "the Keychain refused to {what} {} ({why}). This signed latch keeps its \
             secrets only in the Keychain, so no file was written; it needs to run as \
             the logged-in user, in their session",
            self.account(secret)
        );
        Failure::new(code::keychain_failed, message)
    }
}

/// `dir`'s real path; for a folder that is gone, its real parent's, so its
/// items are still found by the path they were filed under.
fn real_path(dir: &Path) -> PathBuf {
    let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    if let Ok(real) = fs::canonicalize(&dir) {
        return real;
    }
    match (dir.parent(), dir.file_name()) {
        (Some(parent), Some(name)) => fs::canonicalize(parent)
            .map(|parent| parent.join(name))
            .unwrap_or(dir),
        _ => dir,
    }
}

fn io_failure(path: &Path, err: std::io::Error) -> Failure {
    Failure::new(code::state_io, format!("{}: {err}", path.display()))
}

#[cfg(target_os = "macos")]
mod platform {
    use core_foundation::array::CFArray;
    use core_foundation::base::{CFType, CFTypeRef, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::data::CFData;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::error::CFError;
    use core_foundation::string::{CFString, CFStringRef};
    use security_framework_sys::base::{errSecDuplicateItem, errSecItemNotFound};
    use security_framework_sys::item::{
        kSecAttrAccessGroup, kSecAttrAccount, kSecAttrLabel, kSecAttrService,
        kSecAttrSynchronizable, kSecClass, kSecClassGenericPassword, kSecReturnData,
        kSecUseDataProtectionKeychain, kSecValueData,
    };
    use security_framework_sys::keychain_item::{
        SecItemAdd, SecItemCopyMatching, SecItemDelete, SecItemUpdate,
    };

    use super::Store;
    use crate::failure::{Failure, code};

    /// The Keychain access group Plow's signed builds share with the Mac app.
    const KEYCHAIN_GROUP: &str = "3559PD337Z.co.plow.latch";
    const SERVICE: &str = "co.plow.latch";

    #[allow(non_upper_case_globals)]
    unsafe extern "C" {
        static kSecAttrAccessible: CFStringRef;
        static kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly: CFStringRef;
        fn SecTaskCreateFromSelf(allocator: CFTypeRef) -> CFTypeRef;
        fn SecTaskCopyValueForEntitlement(
            task: CFTypeRef,
            entitlement: CFStringRef,
            error: *mut CFTypeRef,
        ) -> CFTypeRef;
    }

    /// The Keychain, if this build is signed with Plow's keychain group.
    pub(super) fn keychain() -> Result<Option<Box<dyn Store>>, Failure> {
        Ok(chosen(granted_groups())?.then(|| Box::new(Keychain) as Box<dyn Store>))
    }

    /// Whether to keep secrets in the Keychain, given the keychain groups
    /// this process's signature grants (`None`: none). A signature macOS
    /// could not read is an error, never taken for an unsigned build.
    fn chosen(groups: Result<Option<Vec<String>>, String>) -> Result<bool, Failure> {
        match groups {
            Ok(None) => Ok(false),
            Ok(Some(groups)) => Ok(groups.iter().any(|group| group == KEYCHAIN_GROUP)),
            Err(why) => {
                let message = format!(
                    "macOS could not tell which keychain groups this latch is signed for \
                     ({why}), so it can't tell where its secrets belong; nothing was \
                     read or written"
                );
                Err(Failure::new(code::keychain_failed, message))
            }
        }
    }

    /// The keychain groups this process's signature grants.
    fn granted_groups() -> Result<Option<Vec<String>>, String> {
        let name = CFString::from_static_string("keychain-access-groups");
        // SAFETY: plain Security calls; each Create/Copy result (task, value,
        // error) is owned and released by its wrapper once non-null.
        unsafe {
            let task = SecTaskCreateFromSelf(std::ptr::null());
            if task.is_null() {
                return Err("SecTaskCreateFromSelf returned nothing".into());
            }
            let task = CFType::wrap_under_create_rule(task);
            let mut error: CFTypeRef = std::ptr::null();
            let value = SecTaskCopyValueForEntitlement(
                task.as_CFTypeRef(),
                name.as_concrete_TypeRef(),
                &mut error,
            );
            let value = (!value.is_null()).then(|| CFType::wrap_under_create_rule(value));
            if !error.is_null() {
                let error = CFType::wrap_under_create_rule(error);
                let why = error
                    .downcast::<CFError>()
                    .map_or("an unknown error".to_string(), |error| {
                        error.description().to_string()
                    });
                return Err(why);
            }
            let Some(value) = value else {
                return Ok(None);
            };
            let Some(groups) = value.downcast::<CFArray>() else {
                return Err("its keychain-access-groups entitlement is not a list".into());
            };
            Ok(Some(
                groups
                    .iter()
                    .filter_map(|group| {
                        CFType::wrap_under_get_rule(*group)
                            .downcast::<CFString>()
                            .map(|group| group.to_string())
                    })
                    .collect(),
            ))
        }
    }

    struct Keychain;

    /// The query naming one item.
    fn item(account: &str) -> Vec<(CFString, CFType)> {
        // SAFETY: the `kSec*` constants are valid static CFStrings.
        unsafe {
            vec![
                (
                    CFString::wrap_under_get_rule(kSecClass),
                    CFString::wrap_under_get_rule(kSecClassGenericPassword).as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecUseDataProtectionKeychain),
                    CFBoolean::true_value().as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecAttrAccessGroup),
                    CFString::new(KEYCHAIN_GROUP).as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecAttrService),
                    CFString::new(SERVICE).as_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecAttrAccount),
                    CFString::new(account).as_CFType(),
                ),
            ]
        }
    }

    fn status(code: i32) -> Result<(), String> {
        if code == 0 {
            Ok(())
        } else {
            Err(format!("status {code}"))
        }
    }

    impl Store for Keychain {
        fn get(&self, account: &str) -> Result<Option<Vec<u8>>, String> {
            let mut query = item(account);
            // SAFETY: a valid static CFString.
            query.push((
                unsafe { CFString::wrap_under_get_rule(kSecReturnData) },
                CFBoolean::true_value().as_CFType(),
            ));
            let query = CFDictionary::from_CFType_pairs(&query);
            let mut found: CFTypeRef = std::ptr::null();
            // SAFETY: `query` is a valid dictionary; `found` is owned below.
            let code = unsafe { SecItemCopyMatching(query.as_concrete_TypeRef(), &mut found) };
            if code == errSecItemNotFound {
                return Ok(None);
            }
            status(code)?;
            if found.is_null() {
                return Err("the Keychain returned no data".into());
            }
            // SAFETY: a Copy result, owned here.
            let found = unsafe { CFType::wrap_under_create_rule(found) };
            let data = found
                .downcast::<CFData>()
                .ok_or("the Keychain returned something other than data")?;
            Ok(Some(data.bytes().to_vec()))
        }

        fn add(&self, account: &str, bytes: &[u8]) -> Result<bool, String> {
            let mut attributes = item(account);
            // SAFETY: valid static CFStrings.
            unsafe {
                attributes.push((
                    CFString::wrap_under_get_rule(kSecValueData),
                    CFData::from_buffer(bytes).as_CFType(),
                ));
                attributes.push((
                    CFString::wrap_under_get_rule(kSecAttrAccessible),
                    CFString::wrap_under_get_rule(kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly)
                        .as_CFType(),
                ));
                attributes.push((
                    CFString::wrap_under_get_rule(kSecAttrSynchronizable),
                    CFBoolean::false_value().as_CFType(),
                ));
                attributes.push((
                    CFString::wrap_under_get_rule(kSecAttrLabel),
                    CFString::new(&format!("latch: {account}")).as_CFType(),
                ));
            }
            let attributes = CFDictionary::from_CFType_pairs(&attributes);
            // SAFETY: a valid dictionary; no result is asked for.
            let code =
                unsafe { SecItemAdd(attributes.as_concrete_TypeRef(), std::ptr::null_mut()) };
            if code == errSecDuplicateItem {
                return Ok(false);
            }
            status(code).map(|()| true)
        }

        fn set(&self, account: &str, bytes: &[u8]) -> Result<(), String> {
            if self.add(account, bytes)? {
                return Ok(());
            }
            let query = CFDictionary::from_CFType_pairs(&item(account));
            // SAFETY: a valid static CFString.
            let key = unsafe { CFString::wrap_under_get_rule(kSecValueData) };
            let change =
                CFDictionary::from_CFType_pairs(&[(key, CFData::from_buffer(bytes).as_CFType())]);
            // SAFETY: two valid dictionaries.
            status(unsafe {
                SecItemUpdate(query.as_concrete_TypeRef(), change.as_concrete_TypeRef())
            })
        }

        fn remove(&self, account: &str) -> Result<bool, String> {
            let query = CFDictionary::from_CFType_pairs(&item(account));
            // SAFETY: a valid dictionary.
            let code = unsafe { SecItemDelete(query.as_concrete_TypeRef()) };
            if code == errSecItemNotFound {
                return Ok(false);
            }
            status(code).map(|()| true)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_signature_macos_could_not_read_fails_and_is_never_taken_as_unsigned() {
            let refused = chosen(Err("the task's signature is invalid".into()))
                .err()
                .unwrap();
            assert_eq!(refused.code, "keychain_failed");
            assert!(
                refused.message.contains("nothing was read or written"),
                "{}",
                refused.message
            );
            assert!(!chosen(Ok(None)).unwrap());
            assert!(!chosen(Ok(Some(vec!["3559PD337Z.other".into()]))).unwrap());
            assert!(chosen(Ok(Some(vec![KEYCHAIN_GROUP.into()]))).unwrap());
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::Store;

    /// Only macOS has a Keychain; other systems keep files.
    pub(super) fn keychain() -> Result<Option<Box<dyn Store>>, crate::failure::Failure> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// The tests' Keychain: items in memory, shared by its clones, that can
    /// be told to fail its Nth write.
    #[derive(Clone, Default)]
    struct Memory {
        items: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        writes: Arc<AtomicUsize>,
        fail_write: Option<usize>,
    }

    impl Store for Memory {
        fn get(&self, account: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(self.items.lock().unwrap().get(account).cloned())
        }

        fn add(&self, account: &str, bytes: &[u8]) -> Result<bool, String> {
            if self.items.lock().unwrap().contains_key(account) {
                return Ok(false);
            }
            self.set(account, bytes).map(|()| true)
        }

        fn set(&self, account: &str, bytes: &[u8]) -> Result<(), String> {
            let writes = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_write == Some(writes) {
                return Err("status -34018".into());
            }
            self.items
                .lock()
                .unwrap()
                .insert(account.into(), bytes.to_vec());
            Ok(())
        }

        fn remove(&self, account: &str) -> Result<bool, String> {
            Ok(self.items.lock().unwrap().remove(account).is_some())
        }
    }

    fn in_memory(dir: &Path, memory: &Memory) -> Result<Secrets, Failure> {
        Ok(Secrets {
            dir: dir.to_path_buf(),
            keychain: Some(Box::new(memory.clone())),
        })
    }

    #[test]
    fn this_dev_build_keeps_secrets_in_owner_only_files() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Secrets::open(dir.path()).unwrap();
        assert!(secrets.keychain.is_none());
        assert!(secrets.create(Secret::Key, b"first").unwrap());
        assert!(!secrets.create(Secret::Key, b"second").unwrap());
        secrets.write(Secret::PlowToken, b"plow-login").unwrap();
        assert_eq!(secrets.read(Secret::Key).unwrap().unwrap(), b"first");
        assert_eq!(secrets.read(Secret::VaultKey).unwrap(), None);
        for file in ["key", "plow-token"] {
            let mode = fs::metadata(dir.path().join(file))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{file}");
        }
    }

    #[test]
    fn a_keychain_build_writes_no_files_and_never_overwrites_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let memory = Memory::default();
        let secrets = in_memory(dir.path(), &memory).unwrap();
        assert!(secrets.create(Secret::Key, b"first").unwrap());
        assert!(!secrets.create(Secret::Key, b"second").unwrap());
        secrets.write(Secret::PlowToken, b"plow-login").unwrap();
        secrets.write(Secret::VaultKey, b"vault-hex").unwrap();
        assert_eq!(secrets.read(Secret::Key).unwrap().unwrap(), b"first");
        assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn a_keychain_build_never_reads_secret_files_a_dev_build_left() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("key"), b"dev-key").unwrap();
        let secrets = in_memory(dir.path(), &Memory::default()).unwrap();
        assert_eq!(secrets.read(Secret::Key).unwrap(), None);
        assert_eq!(fs::read(dir.path().join("key")).unwrap(), b"dev-key");
    }

    #[test]
    fn removing_a_folders_secrets_deletes_its_items_only_and_leaves_files_alone() {
        let (dir, other) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        fs::write(dir.path().join("plow-token"), b"dev-token").unwrap();
        let memory = Memory::default();
        let secrets = in_memory(dir.path(), &memory).unwrap();
        secrets.create(Secret::Key, b"key").unwrap();
        secrets.write(Secret::PlowToken, b"plow-login").unwrap();
        secrets.write(Secret::AuditKeys, b"audit").unwrap();
        let theirs = in_memory(other.path(), &memory).unwrap();
        theirs.create(Secret::Key, b"their-key").unwrap();

        assert_eq!(secrets.remove_each(&Secret::LOGIN).unwrap(), ["plow-token"]);
        assert_eq!(secrets.read(Secret::AuditKeys).unwrap().unwrap(), b"audit");
        assert_eq!(
            secrets.remove_each(&Secret::ALL).unwrap(),
            ["key", "audit.keys"]
        );
        assert_eq!(
            secrets.remove_each(&Secret::ALL).unwrap(),
            Vec::<&str>::new()
        );
        for secret in Secret::ALL {
            assert_eq!(secrets.read(secret).unwrap(), None, "{secret:?}");
        }
        assert_eq!(theirs.read(Secret::Key).unwrap().unwrap(), b"their-key");
        assert_eq!(
            fs::read(dir.path().join("plow-token")).unwrap(),
            b"dev-token"
        );
    }

    #[test]
    fn a_deleted_folder_s_items_are_found_by_the_path_they_were_filed_under() {
        let parent = tempfile::tempdir().unwrap();
        let link = parent.path().join("link");
        fs::create_dir(parent.path().join("real")).unwrap();
        std::os::unix::fs::symlink("real", &link).unwrap();
        let dir = link.join("state");
        fs::create_dir(&dir).unwrap();
        let memory = Memory::default();
        in_memory(&dir, &memory)
            .unwrap()
            .create(Secret::Key, b"key")
            .unwrap();
        fs::remove_dir(&dir).unwrap();

        let secrets = in_memory(&dir, &memory).unwrap();
        assert_eq!(secrets.read(Secret::Key).unwrap().unwrap(), b"key");
        assert_eq!(secrets.remove_each(&[Secret::Key]).unwrap(), ["key"]);
        assert!(memory.items.lock().unwrap().is_empty());
    }

    #[test]
    fn a_keychain_that_refuses_fails_the_command_and_writes_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let refusing = Memory {
            fail_write: Some(1),
            ..Memory::default()
        };
        let secrets = in_memory(dir.path(), &refusing).unwrap();
        let refused = secrets.create(Secret::Key, b"first").err().unwrap();
        assert_eq!(refused.code, "keychain_failed");
        assert!(
            refused.message.contains("no file was written"),
            "{}",
            refused.message
        );
        assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
    }
}
