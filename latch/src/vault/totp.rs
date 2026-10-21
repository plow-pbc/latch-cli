//! Authenticator codes, computed on the host from a login's stored key
//! (the seed never leaves the vault). The key may be the bare base32 a site
//! prints or the whole `otpauth://totp/...` link its QR code holds; a link's
//! digits, period and hash win over the defaults. Rules ported from the
//! Latch app; the codes themselves come from `totp-rs`, checked here
//! against RFC 6238's test vectors.

use totp_rs::{Algorithm, Builder};
use url::Url;

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

pub struct Params {
    secret: Vec<u8>,
    digits: u8,
    period: u64,
    algorithm: Algorithm,
}

/// RFC 4648 base32, forgiving the spellings sites use (lower case, spaces,
/// hyphens, padding) but not a character that is not base32. Errors never
/// quote the key.
fn base32(raw: &str) -> Result<Vec<u8>, String> {
    let clean: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_uppercase();
    let clean = clean.trim_end_matches('=');
    if clean.is_empty() {
        return Err("that authenticator key is empty".into());
    }
    if clean.bytes().all(|b| b.is_ascii_digit()) {
        return Err("that is a code, not a key: the key is the longer letters-and-numbers string the site shows under its QR code".into());
    }
    let mut bytes = Vec::new();
    let (mut value, mut bits) = (0u32, 0u32);
    for c in clean.bytes() {
        let at = ALPHABET
            .iter()
            .position(|a| *a == c)
            .ok_or("that authenticator key has a character base32 does not use")?;
        value = (value << 5) | at as u32;
        bits += 5;
        if bits >= 8 {
            bytes.push((value >> (bits - 8)) as u8);
            bits -= 8;
        }
        value &= (1 << bits) - 1;
    }
    if bytes.is_empty() {
        return Err("that authenticator key is too short to hold a secret".into());
    }
    Ok(bytes)
}

impl Params {
    pub fn parse(stored: &str) -> Result<Params, String> {
        let raw = stored.trim();
        if raw.is_empty() {
            return Err("this item has no authenticator key".into());
        }
        let mut params = Params {
            secret: Vec::new(),
            digits: 6,
            period: 30,
            algorithm: Algorithm::SHA1,
        };
        if !raw.to_ascii_lowercase().starts_with("otpauth://") {
            params.secret = base32(raw)?;
            return Ok(params);
        }
        let url = Url::parse(raw).map_err(|_| "that otpauth link cannot be read".to_string())?;
        if !url
            .host_str()
            .is_some_and(|h| h.eq_ignore_ascii_case("totp"))
        {
            return Err("only otpauth://totp links make codes from the clock".into());
        }
        let query: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let get = |name: &str| {
            query
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };
        params.secret = base32(get("secret").ok_or("that otpauth link carries no secret")?)?;
        // Present but nonsense is refused, not defaulted: a wrong code looks
        // right until a site rejects it.
        let number = |name: &str, fallback: u64, low: u64, high: u64| match get(name) {
            None => Ok(fallback),
            Some(text) => text
                .parse::<u64>()
                .ok()
                .filter(|n| (low..=high).contains(n))
                .ok_or_else(|| {
                    format!("that otpauth link asks for {name}={text}, which latch cannot use")
                }),
        };
        params.digits = number("digits", 6, 6, 8)? as u8;
        params.period = number("period", 30, 1, 300)?;
        params.algorithm = match get("algorithm").map(str::to_ascii_lowercase).as_deref() {
            None | Some("sha1") => Algorithm::SHA1,
            Some("sha256") => Algorithm::SHA256,
            Some("sha512") => Algorithm::SHA512,
            Some(other) => {
                return Err(format!(
                    "that otpauth link asks for {other}, which latch cannot use"
                ));
            }
        };
        Ok(params)
    }

    /// The code for `unix_time`.
    #[allow(dead_code)] // the browser's fills call it
    pub fn code(&self, unix_time: u64) -> String {
        Builder::new()
            .with_algorithm(self.algorithm)
            .with_digits(self.digits)
            .with_step_duration(self.period)
            .with_secret(self.secret.clone())
            .build_noncompliant()
            .generate(unix_time)
            .to_string()
    }

    /// The code now.
    #[allow(dead_code)] // the browser's fills call it
    pub fn current(&self) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.code(now)
    }
}

/// Whether two stored keys are the same key, however spelled (bare
/// base32, or an otpauth link); unreadable ones compare as text.
pub fn same_key(a: &str, b: &str) -> bool {
    match (Params::parse(a), Params::parse(b)) {
        (Ok(a), Ok(b)) => {
            a.secret == b.secret
                && a.digits == b.digits
                && a.period == b.period
                && a.algorithm == b.algorithm
        }
        _ => a.trim() == b.trim(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base32_of(bytes: &[u8]) -> String {
        let mut out = String::new();
        let (mut value, mut bits) = (0u64, 0u32);
        for byte in bytes {
            value = (value << 8) | u64::from(*byte);
            bits += 8;
            while bits >= 5 {
                out.push(ALPHABET[((value >> (bits - 5)) & 31) as usize] as char);
                bits -= 5;
            }
        }
        if bits > 0 {
            out.push(ALPHABET[((value << (5 - bits)) & 31) as usize] as char);
        }
        out
    }

    /// RFC 6238 Appendix B: 8 digits, 30 s steps, per hash.
    #[test]
    fn codes_match_the_rfc_6238_vectors() {
        let seeds = [
            ("sha1", b"12345678901234567890".to_vec()),
            ("sha256", b"12345678901234567890123456789012".to_vec()),
            (
                "sha512",
                b"1234567890123456789012345678901234567890123456789012345678901234".to_vec(),
            ),
        ];
        let vectors: [(u64, [&str; 3]); 6] = [
            (59, ["94287082", "46119246", "90693936"]),
            (1111111109, ["07081804", "68084774", "25091201"]),
            (1111111111, ["14050471", "67062674", "99943326"]),
            (1234567890, ["89005924", "91819424", "93441116"]),
            (2000000000, ["69279037", "90698825", "38618901"]),
            (20000000000, ["65353130", "77737706", "47863826"]),
        ];
        for (i, (hash, seed)) in seeds.iter().enumerate() {
            let link = format!(
                "otpauth://totp/rfc?secret={}&digits=8&algorithm={hash}",
                base32_of(seed)
            );
            let params = Params::parse(&link).unwrap();
            for (time, codes) in vectors {
                assert_eq!(params.code(time), codes[i], "{hash} at {time}");
            }
        }
    }

    #[test]
    fn a_key_is_read_as_sites_print_it() {
        let plain = Params::parse("JBSWY3DPEHPK3PXP").unwrap();
        let printed = Params::parse("jbsw y3dp-ehpk 3pxp==").unwrap();
        assert_eq!(plain.secret, printed.secret);
        assert_eq!((plain.digits, plain.period), (6, 30));
        let link = Params::parse(
            "otpauth://totp/Site:me?secret=JBSWY3DPEHPK3PXP&issuer=Site&period=60&digits=8",
        )
        .unwrap();
        assert_eq!(
            (link.secret.clone(), link.digits, link.period),
            (plain.secret.clone(), 8, 60)
        );
        assert_eq!(plain.code(59).len(), 6);
    }

    #[test]
    fn bad_keys_are_refused_without_quoting_them() {
        for (bad, why) in [
            ("123456", "a code, not a key"),
            ("", "no authenticator key"),
            ("not-a-key-1zzz", "base32"),
            ("otpauth://hotp/x?secret=JBSWY3DPEHPK3PXP", "otpauth://totp"),
            ("otpauth://totp/x?issuer=Me", "no secret"),
            (
                "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&digits=12",
                "digits=12",
            ),
            (
                "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&algorithm=md5",
                "md5",
            ),
        ] {
            let error = Params::parse(bad).err().unwrap();
            assert!(error.contains(why), "{bad}: {error}");
            assert!(!error.contains("zzz") && !error.contains("JBSW"), "{error}");
        }
    }
}
