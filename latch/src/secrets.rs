//! Where a state folder keeps its secrets: this peer's key, the Plow token
//! and the vault's custody key.
//!
//! A latch signed with Plow's keychain group (the release's helper bundle,
//! `latch/macos`) keeps them in the macOS data-protection Keychain, under
//! that group, readable after the first unlock and never synced. Every
//! other build (a dev build, Linux) keeps them as owner-only files in the
//! state folder, as before. The build decides, by its own signature; there
//! is no setting.
//!
//! A Keychain build never writes a secret file. If the Keychain refuses, the
//! command fails and says so. It does move secret files it finds (a folder
//! from before, or from a dev build) into the Keychain: it writes each,
//! reads it back and compares, and deletes the files only once every one is
//! confirmed. A move that is cut short leaves all the files, and the next
//! start picks it up.
//!
//! An item is filed under the state folder's real path, so another folder
//! on the same Mac, or a copy of this one, never shares its identity.

use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::failure::Failure;
use crate::state::write_private;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Secret {
    /// This peer's ed25519 secret key, as hex.
    Key,
    /// The Plow credential `login` was given.
    PlowToken,
    /// The vault's custody key, as hex.
    VaultKey,
}

impl Secret {
    const ALL: [Secret; 3] = [Secret::Key, Secret::PlowToken, Secret::VaultKey];

    /// The file it is kept in by a build without the Keychain.
    pub fn file(self) -> &'static str {
        match self {
            Secret::Key => "key",
            Secret::PlowToken => "plow-token",
            Secret::VaultKey => "vault.key",
        }
    }
}

/// The file a move into the Keychain holds locked, in the state folder.
const MOVE_LOCK: &str = "secrets.lock";

/// A Keychain-like store of named items: the real Keychain, or the tests'.
trait Store: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<Vec<u8>>, String>;
    /// Adds the item; `false` if one is already there, which is left alone.
    fn add(&self, account: &str, bytes: &[u8]) -> Result<bool, String>;
    /// Adds the item or replaces its value.
    fn set(&self, account: &str, bytes: &[u8]) -> Result<(), String>;
}

/// One state folder's secrets.
pub struct Secrets {
    dir: PathBuf,
    /// `None`: files.
    keychain: Option<Box<dyn Store>>,
}

impl Secrets {
    /// The secrets of the state folder `dir`, where this build keeps them.
    /// A Keychain build first moves any secret files there.
    pub fn open(dir: &Path) -> Result<Secrets, Failure> {
        match platform::keychain() {
            None => Ok(Secrets {
                dir: dir.to_path_buf(),
                keychain: None,
            }),
            Some(store) => Secrets::in_store(dir, store),
        }
    }

    fn in_store(dir: &Path, store: Box<dyn Store>) -> Result<Secrets, Failure> {
        let secrets = Secrets {
            dir: dir.to_path_buf(),
            keychain: Some(store),
        };
        secrets.move_files_in()?;
        Ok(secrets)
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

    /// Moves the secret files into the store: writes each, reads it back
    /// and compares, and only once every one is confirmed deletes the
    /// files. Anything short of that leaves every file in place.
    fn move_files_in(&self) -> Result<(), Failure> {
        let store = self.keychain.as_deref().expect("moves only into a store");
        if !Secret::ALL
            .iter()
            .any(|secret| self.dir.join(secret.file()).exists())
        {
            return Ok(());
        }
        // One move at a time per folder. A mover that waited reads the
        // files only now, so it never puts back a secret another one has
        // already moved, and maybe a login has since replaced.
        let lock_path = self.dir.join(MOVE_LOCK);
        let lock = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)
            .map_err(|err| io_failure(&lock_path, err))?;
        lock.lock().map_err(|err| io_failure(&lock_path, err))?;
        let mut confirmed = Vec::new();
        for secret in Secret::ALL {
            let path = self.dir.join(secret.file());
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(err) if err.kind() == ErrorKind::NotFound => continue,
                Err(err) => return Err(io_failure(&path, err)),
            };
            // Until its file is gone the file is the secret: a copy already
            // in the store is from a move cut short, and is replaced.
            let account = self.account(secret);
            store
                .set(&account, &bytes)
                .map_err(|why| self.refused("save", secret, &why))?;
            test_stop_after(confirmed.len() + 1);
            let back = store
                .get(&account)
                .map_err(|why| self.refused("read", secret, &why))?;
            if back.as_deref() != Some(&bytes[..]) {
                let message = format!(
                    "{} did not read back from the Keychain as written; every secret file was kept",
                    secret.file()
                );
                return Err(Failure::failed("keychain_failed", message));
            }
            confirmed.push(path);
        }
        for path in confirmed {
            fs::remove_file(&path).map_err(|err| io_failure(&path, err))?;
        }
        Ok(())
    }

    /// The item's account: the state folder's real path and the file name.
    fn account(&self, secret: Secret) -> String {
        let dir = fs::canonicalize(&self.dir).unwrap_or_else(|_| self.dir.clone());
        format!("{}/{}", dir.display(), secret.file())
    }

    fn refused(&self, what: &str, secret: Secret, why: &str) -> Failure {
        let message = format!(
            "the Keychain refused to {what} {} ({why}). This signed latch keeps its \
             secrets only in the Keychain, so no file was written; it needs to run as \
             the logged-in user, in their session",
            self.account(secret)
        );
        Failure::failed("keychain_failed", message)
    }
}

/// Test only, in debug builds: `LATCH_TEST_STOP_MOVE_AFTER=N` ends the
/// process right after the Nth secret is written to the Keychain during a
/// move, as a kill there would, so a test can show the next start picks
/// the move up. A release build has no such hook.
#[cfg(debug_assertions)]
fn test_stop_after(written: usize) {
    let stop = std::env::var("LATCH_TEST_STOP_MOVE_AFTER").ok();
    if stop.and_then(|n| n.parse().ok()) == Some(written) {
        std::process::exit(75);
    }
}

#[cfg(not(debug_assertions))]
fn test_stop_after(_written: usize) {}

fn io_failure(path: &Path, err: std::io::Error) -> Failure {
    Failure::failed("state_io", format!("{}: {err}", path.display()))
}

#[cfg(target_os = "macos")]
mod platform {
    use core_foundation::array::CFArray;
    use core_foundation::base::{CFType, CFTypeRef, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::data::CFData;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::{CFString, CFStringRef};
    use security_framework_sys::base::{errSecDuplicateItem, errSecItemNotFound};
    use security_framework_sys::item::{
        kSecAttrAccessGroup, kSecAttrAccount, kSecAttrLabel, kSecAttrService,
        kSecAttrSynchronizable, kSecClass, kSecClassGenericPassword, kSecReturnData,
        kSecUseDataProtectionKeychain, kSecValueData,
    };
    use security_framework_sys::keychain_item::{SecItemAdd, SecItemCopyMatching, SecItemUpdate};

    use super::Store;

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
    pub(super) fn keychain() -> Option<Box<dyn Store>> {
        entitled().then(|| Box::new(Keychain) as Box<dyn Store>)
    }

    /// Whether this process's signature grants `KEYCHAIN_GROUP`.
    fn entitled() -> bool {
        let name = CFString::from_static_string("keychain-access-groups");
        // SAFETY: plain Security calls; each Create/Copy result is owned and
        // released by its wrapper, and both are checked for null first.
        unsafe {
            let task = SecTaskCreateFromSelf(std::ptr::null());
            if task.is_null() {
                return false;
            }
            let task = CFType::wrap_under_create_rule(task);
            let value = SecTaskCopyValueForEntitlement(
                task.as_CFTypeRef(),
                name.as_concrete_TypeRef(),
                std::ptr::null_mut(),
            );
            if value.is_null() {
                return false;
            }
            let value = CFType::wrap_under_create_rule(value);
            let Some(groups) = value.downcast::<CFArray>() else {
                return false;
            };
            groups.iter().any(|group| {
                CFType::wrap_under_get_rule(*group)
                    .downcast::<CFString>()
                    .is_some_and(|group| group == KEYCHAIN_GROUP)
            })
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
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::Store;

    /// Only macOS has a Keychain; other systems keep files.
    pub(super) fn keychain() -> Option<Box<dyn Store>> {
        None
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
    /// be told to fail its Nth write or to read back something else.
    #[derive(Clone, Default)]
    struct Memory {
        items: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        writes: Arc<AtomicUsize>,
        fail_write: Option<usize>,
        garble_reads: bool,
    }

    impl Store for Memory {
        fn get(&self, account: &str) -> Result<Option<Vec<u8>>, String> {
            let found = self.items.lock().unwrap().get(account).cloned();
            Ok(if self.garble_reads {
                found.map(|_| b"something else".to_vec())
            } else {
                found
            })
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
    }

    fn in_memory(dir: &Path, memory: &Memory) -> Result<Secrets, Failure> {
        Secrets::in_store(dir, Box::new(memory.clone()))
    }

    fn files(dir: &Path) -> Secrets {
        Secrets {
            dir: dir.to_path_buf(),
            keychain: None,
        }
    }

    fn secret_files(dir: &Path) -> Vec<&'static str> {
        Secret::ALL
            .iter()
            .map(|secret| secret.file())
            .filter(|file| dir.join(file).exists())
            .collect()
    }

    /// A folder as a file build leaves it: all three secrets as files.
    fn folder_with_files() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let on_disk = files(dir.path());
        on_disk.write(Secret::Key, b"key-hex").unwrap();
        on_disk.write(Secret::PlowToken, b"plow-login").unwrap();
        on_disk.write(Secret::VaultKey, b"vault-hex").unwrap();
        dir
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
    fn secret_files_move_into_the_keychain_and_are_then_removed() {
        let dir = folder_with_files();
        let memory = Memory::default();
        let secrets = in_memory(dir.path(), &memory).unwrap();
        assert_eq!(secret_files(dir.path()), Vec::<&str>::new());
        assert_eq!(secrets.read(Secret::Key).unwrap().unwrap(), b"key-hex");
        assert_eq!(
            secrets.read(Secret::PlowToken).unwrap().unwrap(),
            b"plow-login"
        );
        assert_eq!(
            secrets.read(Secret::VaultKey).unwrap().unwrap(),
            b"vault-hex"
        );
        // Filed under the folder's real path, so a copy elsewhere shares none.
        let real = fs::canonicalize(dir.path()).unwrap();
        assert!(
            memory
                .items
                .lock()
                .unwrap()
                .contains_key(&format!("{}/key", real.display()))
        );
    }

    #[test]
    fn a_move_cut_short_keeps_the_rest_and_the_next_start_finishes_it() {
        let dir = folder_with_files();
        // The second write fails after the key's was confirmed: every file
        // stays until all three are, and nothing is lost.
        let failing = Memory {
            fail_write: Some(2),
            ..Memory::default()
        };
        let refused = in_memory(dir.path(), &failing).err().unwrap();
        assert_eq!(refused.code, "keychain_failed");
        assert_eq!(secret_files(dir.path()), ["key", "plow-token", "vault.key"]);

        let memory = Memory {
            items: failing.items.clone(),
            ..Memory::default()
        };
        let secrets = in_memory(dir.path(), &memory).unwrap();
        assert_eq!(secret_files(dir.path()), Vec::<&str>::new());
        assert_eq!(secrets.read(Secret::Key).unwrap().unwrap(), b"key-hex");
        assert_eq!(
            secrets.read(Secret::PlowToken).unwrap().unwrap(),
            b"plow-login"
        );
        assert_eq!(
            secrets.read(Secret::VaultKey).unwrap().unwrap(),
            b"vault-hex"
        );
    }

    #[test]
    fn a_second_mover_waits_and_puts_back_nothing_the_first_moved() {
        let dir = folder_with_files();
        let memory = Memory::default();
        // Another process is moving this folder's files: it holds the lock.
        let held = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.path().join(MOVE_LOCK))
            .unwrap();
        held.lock().unwrap();
        let waiting = std::thread::spawn({
            let (dir, memory) = (dir.path().to_path_buf(), memory.clone());
            move || in_memory(&dir, &memory).map(|_| ())
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!waiting.is_finished(), "a move waits for the folder's lock");

        // The other finishes its move, then a login replaces the token.
        let other = files(dir.path());
        for secret in Secret::ALL {
            let path = dir.path().join(secret.file());
            let bytes = fs::read(&path).unwrap();
            memory.set(&other.account(secret), &bytes).unwrap();
            fs::remove_file(path).unwrap();
        }
        memory
            .set(&other.account(Secret::PlowToken), b"new-login")
            .unwrap();
        drop(held);

        waiting.join().unwrap().unwrap();
        assert_eq!(
            memory
                .get(&other.account(Secret::PlowToken))
                .unwrap()
                .unwrap(),
            b"new-login"
        );
    }

    #[test]
    fn a_file_whose_copy_does_not_read_back_is_kept() {
        let dir = folder_with_files();
        let garbling = Memory {
            garble_reads: true,
            ..Memory::default()
        };
        let refused = in_memory(dir.path(), &garbling).err().unwrap();
        assert_eq!(refused.code, "keychain_failed");
        assert_eq!(secret_files(dir.path()), ["key", "plow-token", "vault.key"]);
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
