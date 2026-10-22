//! AES-256-GCM sealing for what latch keeps encrypted at rest: the vault
//! and the audit log. A sealed value is a random 96-bit nonce and the
//! ciphertext, each as hex, bound to associated data that says what it is
//! (and, in the audit log, where it belongs).

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use serde::{Deserialize, Serialize};

use crate::state::to_hex;

#[derive(Serialize, Deserialize, Clone)]
pub struct Sealed {
    pub nonce: String,
    pub ciphertext: String,
}

/// Seals `plain` under `key`, bound to `aad`, with a fresh random nonce.
pub fn seal(key: &[u8; 32], plain: &[u8], aad: &[u8]) -> Sealed {
    let nonce: [u8; 12] = rand::random();
    let ciphertext = Aes256Gcm::new(key.into())
        .encrypt(&Nonce::from(nonce), Payload { msg: plain, aad })
        .expect("AES-GCM encryption does not fail");
    Sealed {
        nonce: to_hex(&nonce),
        ciphertext: to_hex(&ciphertext),
    }
}

/// What `seal` sealed, or `None` when the hex is damaged or it does not
/// open with `key` and `aad`.
pub fn open(key: &[u8; 32], nonce: &str, ciphertext: &str, aad: &[u8]) -> Option<Vec<u8>> {
    let nonce: [u8; 12] = from_hex(nonce)?.try_into().ok()?;
    let msg = from_hex(ciphertext)?;
    Aes256Gcm::new(key.into())
        .decrypt(&Nonce::from(nonce), Payload { msg: &msg, aad })
        .ok()
}

/// The bytes `to_hex` wrote, or `None` when `text` is not hex.
pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sealed_value_opens_only_with_its_key_and_its_associated_data() {
        let key = [7u8; 32];
        let sealed = seal(&key, b"made-up-secret", b"what it is");
        let opened = open(&key, &sealed.nonce, &sealed.ciphertext, b"what it is");
        assert_eq!(opened.as_deref(), Some(&b"made-up-secret"[..]));
        assert!(open(&[8u8; 32], &sealed.nonce, &sealed.ciphertext, b"what it is").is_none());
        assert!(open(&key, &sealed.nonce, &sealed.ciphertext, b"something else").is_none());
        assert!(open(&key, "zz", &sealed.ciphertext, b"what it is").is_none());
    }

    #[test]
    fn hex_reads_back_and_anything_else_is_refused() {
        assert_eq!(
            from_hex(&to_hex(&[0, 0xab, 0xff])),
            Some(vec![0, 0xab, 0xff])
        );
        for bad in ["abc", "zz", "aéb"] {
            assert_eq!(from_hex(bad), None, "{bad}");
        }
    }
}
