//! A Windows secret file's bytes: sealed with DPAPI for this Windows user,
//! and for one item (the state folder's real path and the file's name,
//! given as `account`), so a file copied to another folder, or read by
//! another user, does not open. The file holds `MAGIC` and the sealed blob.
//!
//! This file needs only std and windows-sys, so its tests can run on their
//! own on Windows.

use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};

/// What a sealed file starts with.
pub const MAGIC: &[u8] = b"latch-dpapi-1\n";

/// `bytes`, sealed for this user and `account`, as a file keeps them.
pub fn seal(bytes: &[u8], account: &str) -> Result<Vec<u8>, String> {
    let mut sealed = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };
    // SAFETY: both blobs point at live slices for the call; DPAPI
    // allocates `sealed`, which `take` frees.
    let ok = unsafe {
        CryptProtectData(
            &blob(bytes),
            null(),
            &blob(account.as_bytes()),
            null(),
            null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut sealed,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok([MAGIC, &take(sealed)].concat())
}

/// A secret file's bytes, opened for this user and `account`. A file that
/// does not open (another user's, another item's, changed, or never
/// sealed) is an error.
pub fn open(bytes: &[u8], account: &str) -> Result<Vec<u8>, String> {
    let Some(sealed) = bytes.strip_prefix(MAGIC) else {
        return Err("it is not sealed".into());
    };
    let mut opened = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };
    // SAFETY: as in `seal`; no description is asked for.
    let ok = unsafe {
        CryptUnprotectData(
            &blob(sealed),
            null_mut(),
            &blob(account.as_bytes()),
            null(),
            null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut opened,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(take(opened))
}

fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    }
}

/// Copies out a blob DPAPI allocated, then wipes and frees it.
fn take(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if blob.pbData.is_null() {
        return Vec::new();
    }
    // SAFETY: DPAPI's output, `cbData` bytes at `pbData`, freed once here.
    unsafe {
        let bytes = std::slice::from_raw_parts(blob.pbData, blob.cbData as usize).to_vec();
        for at in 0..blob.cbData as usize {
            std::ptr::write_volatile(blob.pbData.add(at), 0);
        }
        LocalFree(blob.pbData.cast());
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT: &str = r"\\?\C:\Users\someone\.latch/plow-token";

    #[test]
    fn a_sealed_secret_opens_for_its_item_and_is_not_in_the_file() {
        let sealed = seal(b"plow-login-token", ACCOUNT).unwrap();
        assert!(sealed.starts_with(MAGIC));
        assert!(!sealed.windows(16).any(|w| w == b"plow-login-token"));
        assert_eq!(open(&sealed, ACCOUNT).unwrap(), b"plow-login-token");
    }

    #[test]
    fn a_file_in_the_clear_does_not_open() {
        assert!(open(b"0123abcd", ACCOUNT).is_err());
    }

    #[test]
    fn a_sealed_file_copied_to_another_item_does_not_open() {
        let sealed = seal(b"key-hex", ACCOUNT).unwrap();
        let other = r"\\?\C:\Users\someone\other/plow-token";
        assert!(open(&sealed, other).is_err());
        assert!(open(&sealed, &ACCOUNT.replace("plow-token", "key")).is_err());
    }

    #[test]
    fn a_changed_sealed_file_does_not_open() {
        let mut sealed = seal(b"vault-key-hex", ACCOUNT).unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert!(open(&sealed, ACCOUNT).is_err());
        sealed.truncate(MAGIC.len() + 4);
        assert!(open(&sealed, ACCOUNT).is_err());
    }

    #[test]
    fn an_empty_secret_seals_and_opens() {
        let sealed = seal(b"", ACCOUNT).unwrap();
        assert_eq!(open(&sealed, ACCOUNT).unwrap(), b"");
    }
}
