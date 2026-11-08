//! Plow Mandate: an outside agent checks whom our agent acts for and what it
//! may commit that person to.
//!
//! A mandate is an SD-JWT VC (`dc+sd-jwt`) the company's issuer signs, bound
//! to the agent's P-256 key (`cnf`). The agent closes it per message with a
//! key-binding JWT over that message, its audience and its actor chain, and
//! carries it in the A2A message's `metadata["co.plow.mandate"]`. Strip that
//! field and the message is plain A2A.
//!
//! [`Issuer`] mints, [`Holder`] presents, [`Verifier`] checks. Verification
//! is deterministic and fails closed: the issuer's keys and status list come
//! only from the well-known URLs on the agent's own server, and a stale or
//! unreachable status list refuses the mandate.

use std::collections::HashMap;
use std::future::Future;

use aws_lc_rs::digest::{SHA256, digest};
use aws_lc_rs::hmac;
use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use aws_lc_rs::signature::{
    ECDSA_P256_SHA256_FIXED, ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair,
    UnparsedPublicKey,
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The A2A extension's URI, for the Agent Card and `A2A-Extensions`.
pub const EXTENSION: &str = "https://plow.co/ext/mandate/v1";
/// The message metadata key the mandate rides in.
pub const METADATA_KEY: &str = "co.plow.mandate";
pub const VCT: &str = "https://plow.co/vct/mandate/v1";
/// What "verified" means in v1: Plow's issuer attests the person granted it.
pub const TRUST: &str = "issuer-attested";
/// A mandate lives 15 minutes; the agent asks for a fresh one after that.
pub const LIFETIME: i64 = 15 * 60;
/// How far a key-binding JWT's `iat` may be from the verifier's clock.
pub const KB_WINDOW: i64 = 60;
/// The oldest status list a verifier accepts.
pub const STATUS_MAX_AGE: i64 = 5 * 60;
const PSEUDONYM: &str = "urn:plow:pp:";

/// Why a mandate was refused (or, from [`Issuer::mint`], not minted).
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    Malformed(String),
    /// `iss` is not `https://` + the agent's server.
    Issuer,
    /// The issuer's keys could not be fetched or read.
    Keys(String),
    Signature,
    Expired,
    Audience,
    /// The key-binding JWT is not signed by the mandate's `cnf` key.
    KeyBinding,
    /// The key-binding JWT's `iat` is outside the window.
    KeyBindingTime,
    /// The message, actor chain or SD-JWT is not the one the key-binding JWT covers.
    Mismatch,
    Replay,
    /// The status list could not be fetched, read, or is stale.
    Status(String),
    Revoked,
    UnknownConstraint(String),
    /// The principal is not a person.
    Principal,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "mandate refused: {self:?}")
    }
}

impl std::error::Error for Refused {}

/// What the owner may let the agent commit them to. Typed only: a type the
/// verifier doesn't know fails the whole mandate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Constraint {
    /// `value` is a decimal string, as in AP2 and the Payment Request API.
    #[serde(rename = "co.plow.amount.max")]
    AmountMax { currency: String, value: String },
    #[serde(rename = "co.plow.calendar.max_minutes")]
    CalendarMaxMinutes { minutes: u32 },
    #[serde(rename = "co.plow.share.paths")]
    SharePaths { paths: Vec<String> },
}

/// A P-256 public key as a JWK.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Jwk {
    pub kty: String,
    pub crv: String,
    pub x: String,
    pub y: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kid: Option<String>,
}

impl Jwk {
    fn of(key: &EcdsaKeyPair, kid: Option<String>) -> Jwk {
        // The uncompressed point: 0x04, then x and y.
        let point = key.public_key().as_ref();
        Jwk {
            kty: "EC".into(),
            crv: "P-256".into(),
            x: B64.encode(&point[1..33]),
            y: B64.encode(&point[33..]),
            kid,
        }
    }

    fn verifies(&self, message: &[u8], signature: &[u8]) -> bool {
        let (Ok(x), Ok(y)) = (B64.decode(&self.x), B64.decode(&self.y)) else {
            return false;
        };
        if self.kty != "EC" || self.crv != "P-256" {
            return false;
        }
        let point = [&[4u8][..], &x, &y].concat();
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, point)
            .verify(message, signature)
            .is_ok()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Jwks {
    pub keys: Vec<Jwk>,
}

/// A fresh P-256 key, as the PKCS#8 bytes [`Issuer::new`] and [`Holder::new`] take.
pub fn generate_key() -> Vec<u8> {
    EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
        .expect("generating a P-256 key")
        .as_ref()
        .to_vec()
}

fn key_pair(pkcs8: &[u8]) -> Result<EcdsaKeyPair, Refused> {
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8)
        .map_err(|_| Refused::Malformed("not a P-256 PKCS#8 key".into()))
}

/// The issuer's keys and status list, at fixed paths on the company server.
pub fn jwks_url(server: &str) -> String {
    format!("https://{server}/.well-known/plow-mandate/jwks.json")
}

pub fn status_url(server: &str) -> String {
    format!("https://{server}/.well-known/plow-mandate/status")
}

/// A Matrix user id's server name: `@local:server` → `server`.
fn server_of(user: &str) -> Option<&str> {
    user.strip_prefix('@')?
        .split_once(':')
        .map(|(_, server)| server)
        .filter(|s| is_server_name(s))
}

/// Matrix `server_name` grammar: a DNS name, an IPv4 address or a bracketed
/// IPv6 one, then an optional `:port`. Nothing else may reach a URL's host.
fn is_server_name(name: &str) -> bool {
    let (host, port) = match name.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((ip6, port))
                if !ip6.is_empty()
                    && ip6
                        .chars()
                        .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.') =>
            {
                (ip6, port)
            }
            _ => return false,
        },
        None => {
            let (host, port) = name.split_at(name.find(':').unwrap_or(name.len()));
            let dns = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '.';
            if host.is_empty() || host.len() > 255 || !host.chars().all(dns) {
                return false;
            }
            (host, port)
        }
    };
    !host.is_empty()
        && (port.is_empty()
            || port.strip_prefix(':').is_some_and(|p| {
                (1..=5).contains(&p.len()) && p.chars().all(|c| c.is_ascii_digit())
            }))
}

/// A person's id is their email, `@` escaped as `=40`.
fn is_person(user: &str) -> bool {
    user.strip_prefix('@')
        .and_then(|u| u.split_once(':'))
        .is_some_and(|(local, _)| local.contains("=40"))
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    digest(&SHA256, bytes).as_ref().to_vec()
}

fn sha256_b64(bytes: &[u8]) -> String {
    B64.encode(sha256(bytes))
}

fn random_b64(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    SystemRandom::new()
        .fill(&mut bytes)
        .expect("system randomness");
    B64.encode(bytes)
}

fn sign_jwt(key: &EcdsaKeyPair, header: &Value, payload: &Value) -> String {
    let input = format!(
        "{}.{}",
        B64.encode(header.to_string()),
        B64.encode(payload.to_string())
    );
    let signature = key
        .sign(&SystemRandom::new(), input.as_bytes())
        .expect("ES256 signing");
    format!("{input}.{}", B64.encode(signature.as_ref()))
}

/// A compact JWT's header and payload, unchecked, and its signing input and signature.
struct Jwt<'a> {
    header: Value,
    payload: Value,
    input: &'a str,
    signature: Vec<u8>,
}

fn parse_jwt(token: &str) -> Result<Jwt<'_>, Refused> {
    let bad = || Refused::Malformed("not a compact JWT".into());
    let (input, signature) = token.rsplit_once('.').ok_or_else(bad)?;
    let (header, payload) = input.split_once('.').ok_or_else(bad)?;
    let part = |p: &str| -> Result<Value, Refused> {
        serde_json::from_slice(&B64.decode(p).map_err(|_| bad())?).map_err(|_| bad())
    };
    Ok(Jwt {
        header: part(header)?,
        payload: part(payload)?,
        input,
        signature: B64.decode(signature).map_err(|_| bad())?,
    })
}

/// RFC 8785 (JCS): keys sorted by UTF-16 code units, no whitespace, numbers
/// as ECMAScript prints them.
pub fn jcs(value: &Value) -> String {
    let mut out = String::new();
    write_jcs(value, &mut out);
    out
}

fn write_jcs(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::from(key.as_str()).to_string());
                out.push(':');
                write_jcs(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_jcs(item, out);
            }
            out.push(']');
        }
        Value::Number(n) if n.is_f64() => out.push_str(&ecmascript_number(n.as_f64().unwrap())),
        other => out.push_str(&other.to_string()),
    }
}

fn ecmascript_number(f: f64) -> String {
    if f == 0.0 {
        return "0".into();
    }
    if (1e-6..1e21).contains(&f.abs()) {
        // Rust's shortest round-trip digits, never in exponent form here.
        return f.to_string();
    }
    let s = format!("{f:e}");
    if s.contains("e-") {
        s
    } else {
        s.replace('e', "e+")
    }
}

/// The message as the key-binding JWT covers it: without this extension's
/// field, and without `metadata` if that leaves it empty. File parts carry
/// their bytes inline, so their bytes are covered too.
fn message_hash(message: &Value) -> String {
    let mut message = message.clone();
    if let Some(metadata) = message.get_mut("metadata").and_then(Value::as_object_mut) {
        metadata.remove(METADATA_KEY);
        if metadata.is_empty() {
            message.as_object_mut().unwrap().remove("metadata");
        }
    }
    sha256_b64(jcs(&message).as_bytes())
}

/// The company server's mandate issuer: it alone signs mandates and status lists.
pub struct Issuer {
    key: EcdsaKeyPair,
    kid: String,
    server: String,
    pseudonym_secret: hmac::Key,
}

/// What the owner granted, as the issuer stored it.
pub struct Grant<'a> {
    /// The person, `@alice=40acme.com:<server>`.
    pub principal: &'a str,
    /// Their agent, `act.sub`.
    pub agent: &'a str,
    /// The target agent's card URL.
    pub aud: &'a str,
    pub constraints: &'a [Constraint],
    /// `(name, email)`, disclosable only when the owner chose "show who I am".
    pub disclose: Option<(&'a str, &'a str)>,
    /// The agent's key the mandate is bound to.
    pub cnf: &'a Jwk,
    /// This mandate's index in the status list.
    pub status_index: u64,
}

impl Issuer {
    /// `key` is the issuer's P-256 PKCS#8 key; `pseudonym_secret` keys the
    /// pairwise `sub`. `server` is the Matrix server name, which is also
    /// `iss`'s host.
    pub fn new(key: &[u8], pseudonym_secret: &[u8], server: &str) -> Result<Issuer, Refused> {
        if !is_server_name(server) {
            return Err(Refused::Issuer);
        }
        let key = key_pair(key)?;
        let kid = sha256_b64(key.public_key().as_ref())[..16].to_string();
        Ok(Issuer {
            key,
            kid,
            server: server.into(),
            pseudonym_secret: hmac::Key::new(hmac::HMAC_SHA256, pseudonym_secret),
        })
    }

    pub fn jwks(&self) -> Jwks {
        Jwks {
            keys: vec![Jwk::of(&self.key, Some(self.kid.clone()))],
        }
    }

    fn iss(&self) -> String {
        format!("https://{}", self.server)
    }

    /// The SD-JWT (`issuer-jwt~disclosures~`) for a stored grant, and its `jti`.
    /// Refuses a principal that is not a person and accounts on another server.
    pub fn mint(&self, grant: &Grant, now: i64) -> Result<(String, String), Refused> {
        if server_of(grant.principal) != Some(&self.server)
            || server_of(grant.agent) != Some(&self.server)
        {
            return Err(Refused::Issuer);
        }
        if !is_person(grant.principal) {
            return Err(Refused::Principal);
        }
        let pairwise = hmac::sign(
            &self.pseudonym_secret,
            format!("{}\0{}", grant.principal, grant.aud).as_bytes(),
        );
        let constraints = serde_json::to_value(grant.constraints).unwrap();
        let disclosures: Vec<String> = grant
            .disclose
            .map(|(name, email)| {
                [("name", name), ("email", email)]
                    .map(|(claim, value)| {
                        B64.encode(json!([random_b64(16), claim, value]).to_string())
                    })
                    .to_vec()
            })
            .unwrap_or_default();
        let jti = random_b64(16);
        let mut payload = json!({
            "iss": self.iss(),
            "sub": format!("{PSEUDONYM}{}", B64.encode(pairwise.as_ref())),
            "act": {"sub": grant.agent},
            "cnf": {"jwk": grant.cnf},
            "aud": grant.aud,
            "vct": VCT,
            "constraints": constraints,
            "grant_hash": sha256_b64(jcs(&constraints).as_bytes()),
            "iat": now,
            "exp": now + LIFETIME,
            "jti": jti,
            "status": {"status_list": {"idx": grant.status_index, "uri": status_url(&self.server)}},
            "trust": TRUST,
            "_sd_alg": "sha-256",
        });
        if !disclosures.is_empty() {
            payload["_sd"] = disclosures
                .iter()
                .map(|d| sha256_b64(d.as_bytes()))
                .collect();
        }
        let header = json!({"alg": "ES256", "typ": "dc+sd-jwt", "kid": self.kid});
        let mut sd_jwt = sign_jwt(&self.key, &header, &payload);
        for disclosure in &disclosures {
            sd_jwt.push('~');
            sd_jwt.push_str(disclosure);
        }
        sd_jwt.push('~');
        Ok((sd_jwt, jti))
    }

    /// The status list (an IETF Token Status List JWT, one bit per mandate)
    /// for the first `issued` indexes, signed fresh: verifiers refuse one
    /// older than [`STATUS_MAX_AGE`].
    pub fn status_list(&self, issued: u64, revoked: &[u64], now: i64) -> String {
        let mut bits = vec![0u8; issued.div_ceil(8) as usize];
        for &i in revoked {
            bits[(i / 8) as usize] |= 1 << (i % 8);
        }
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut zlib, &bits).unwrap();
        let payload = json!({
            "sub": status_url(&self.server),
            "iat": now,
            "ttl": STATUS_MAX_AGE,
            "status_list": {"bits": 1, "lst": B64.encode(zlib.finish().unwrap())},
        });
        sign_jwt(
            &self.key,
            &json!({"alg": "ES256", "typ": "statuslist+jwt", "kid": self.kid}),
            &payload,
        )
    }
}

/// The agent's side: its `cnf` key, which closes a mandate per message.
pub struct Holder {
    key: EcdsaKeyPair,
}

impl Holder {
    pub fn new(key: &[u8]) -> Result<Holder, Refused> {
        Ok(Holder {
            key: key_pair(key)?,
        })
    }

    pub fn jwk(&self) -> Jwk {
        Jwk::of(&self.key, None)
    }

    /// Puts the mandate on an outbound A2A message for `aud`: a key-binding
    /// JWT over the message as it stands, `aud` and `actor_chain` (A2A
    /// #2028's shape, attribution only) goes in `metadata[METADATA_KEY]`.
    pub fn attach(
        &self,
        message: &mut Value,
        sd_jwt: &str,
        aud: &str,
        actor_chain: Value,
        now: i64,
    ) {
        let payload = json!({
            "iat": now,
            "aud": aud,
            "nonce": random_b64(16),
            "sd_hash": sha256_b64(sd_jwt.as_bytes()),
            "msg_hash": message_hash(message),
            "actor_chain_hash": sha256_b64(jcs(&actor_chain).as_bytes()),
        });
        let kb = sign_jwt(
            &self.key,
            &json!({"alg": "ES256", "typ": "kb+jwt"}),
            &payload,
        );
        let message = message
            .as_object_mut()
            .expect("an A2A message is an object");
        let metadata = message
            .entry("metadata")
            .or_insert_with(|| Value::Object(Map::new()));
        metadata[METADATA_KEY] =
            json!({"sd_jwt": format!("{sd_jwt}{kb}"), "actorChain": actor_chain});
    }
}

/// An HTTPS GET of the issuer's keys or status list. Callers pin it to
/// https and public addresses; the verifier picks the URL, never the token.
pub trait Fetch {
    fn get(&self, url: &str) -> impl Future<Output = Result<Vec<u8>, String>> + Send;
}

/// A mandate that passed every check.
#[derive(Debug, PartialEq, Eq)]
pub struct Verified {
    /// The pairwise pseudonym of the person the agent acts for.
    pub sub: String,
    pub iss: String,
    pub jti: String,
    /// The agent.
    pub act_sub: String,
    pub constraints: Vec<Constraint>,
    pub grant_hash: String,
    pub trust: String,
    pub exp: i64,
    /// Present only when the owner chose to show who they are.
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    act: Act,
    cnf: Cnf,
    aud: String,
    vct: String,
    constraints: Vec<Value>,
    grant_hash: String,
    exp: i64,
    jti: String,
    status: Status,
    trust: String,
    #[serde(default, rename = "_sd")]
    sd: Vec<String>,
}

#[derive(Deserialize)]
struct Act {
    sub: String,
}

#[derive(Deserialize)]
struct Cnf {
    jwk: Jwk,
}

#[derive(Deserialize)]
struct Status {
    status_list: StatusRef,
}

#[derive(Deserialize)]
struct StatusRef {
    idx: u64,
    uri: String,
}

#[derive(Deserialize)]
struct KbClaims {
    iat: i64,
    aud: String,
    sd_hash: String,
    msg_hash: String,
    actor_chain_hash: String,
}

/// Checks inbound mandates addressed to one agent card.
pub struct Verifier<F> {
    card_url: String,
    fetch: F,
    /// Fetched documents by URL, with when they were fetched.
    cache: HashMap<String, (i64, Vec<u8>)>,
    /// `(jti, key-binding JWT hash)` → the mandate's `exp`.
    seen: HashMap<(String, String), i64>,
}

impl<F: Fetch> Verifier<F> {
    pub fn new(card_url: &str, fetch: F) -> Verifier<F> {
        Verifier {
            card_url: card_url.into(),
            fetch,
            cache: HashMap::new(),
            seen: HashMap::new(),
        }
    }

    /// Verifies the mandate on an inbound A2A message, against the raw
    /// message as received.
    pub async fn verify(&mut self, message: &Value, now: i64) -> Result<Verified, Refused> {
        let extension = &message["metadata"][METADATA_KEY];
        let presentation = extension["sd_jwt"]
            .as_str()
            .ok_or(Refused::Malformed("no sd_jwt".into()))?;
        let (sd_jwt, kb) = presentation
            .rsplit_once('~')
            .ok_or(Refused::Malformed("no key binding".into()))?;
        let sd_jwt = format!("{sd_jwt}~");
        let mut parts = sd_jwt.trim_end_matches('~').split('~');
        let jwt = parse_jwt(parts.next().unwrap())?;
        let disclosures: Vec<&str> = parts.collect();
        let claims: Claims = serde_json::from_value(jwt.payload.clone())
            .map_err(|e| Refused::Malformed(e.to_string()))?;

        // 1. The issuer is the agent's own server; its keys come from there.
        let server = server_of(&claims.act.sub)
            .ok_or(Refused::Malformed("act.sub is not a user id".into()))?;
        if claims.iss != format!("https://{server}") {
            return Err(Refused::Issuer);
        }
        let keys = self.keys(server, now).await?;

        // 2. Signature, disclosures, expiry, audience.
        if jwt.header["alg"] != "ES256" || jwt.header["typ"] != "dc+sd-jwt" {
            return Err(Refused::Malformed("not an ES256 dc+sd-jwt".into()));
        }
        if !keys
            .iter()
            .any(|k| k.verifies(jwt.input.as_bytes(), &jwt.signature))
        {
            return Err(Refused::Signature);
        }
        let (name, email) = disclosed(&disclosures, &claims.sd)?;
        if now >= claims.exp {
            return Err(Refused::Expired);
        }
        if claims.aud != self.card_url {
            return Err(Refused::Audience);
        }
        if claims.vct != VCT || claims.trust != TRUST {
            return Err(Refused::Malformed("unknown vct or trust".into()));
        }
        if claims.grant_hash
            != sha256_b64(jcs(&Value::Array(claims.constraints.clone())).as_bytes())
        {
            return Err(Refused::Malformed(
                "grant_hash is not the constraints'".into(),
            ));
        }

        // 3. The key-binding JWT: the agent's key, now, us, this message.
        let kb_jwt = parse_jwt(kb)?;
        if kb_jwt.header["alg"] != "ES256" || kb_jwt.header["typ"] != "kb+jwt" {
            return Err(Refused::Malformed("not an ES256 kb+jwt".into()));
        }
        if !claims
            .cnf
            .jwk
            .verifies(kb_jwt.input.as_bytes(), &kb_jwt.signature)
        {
            return Err(Refused::KeyBinding);
        }
        let kb_claims: KbClaims = serde_json::from_value(kb_jwt.payload)
            .map_err(|e| Refused::Malformed(e.to_string()))?;
        if kb_claims.iat.abs_diff(now) > KB_WINDOW as u64 {
            return Err(Refused::KeyBindingTime);
        }
        if kb_claims.aud != self.card_url {
            return Err(Refused::Audience);
        }
        if kb_claims.sd_hash != sha256_b64(sd_jwt.as_bytes())
            || kb_claims.msg_hash != message_hash(message)
            || kb_claims.actor_chain_hash != sha256_b64(jcs(&extension["actorChain"]).as_bytes())
        {
            return Err(Refused::Mismatch);
        }

        // 4. Each key-binding JWT is accepted once, until the mandate expires.
        // Recorded only once every check has passed, so a refusal for a
        // passing reason (a status fetch that failed) doesn't spend it.
        self.seen.retain(|_, exp| *exp > now);
        // Keyed by the signed input, not the token: an ECDSA signature has a
        // second valid form (s → n−s), so the token's bytes aren't unique.
        let once = (claims.jti.clone(), sha256_b64(kb_jwt.input.as_bytes()));
        if self.seen.contains_key(&once) {
            return Err(Refused::Replay);
        }

        // 5. Not revoked, by a status list at most five minutes old.
        if claims.status.status_list.uri != status_url(server) {
            return Err(Refused::Status(
                "the status list is not the issuer's".into(),
            ));
        }
        if self
            .revoked(server, &keys, claims.status.status_list.idx, now)
            .await?
        {
            return Err(Refused::Revoked);
        }

        // 6. Only known constraint types; the principal is a person.
        let constraints = claims
            .constraints
            .into_iter()
            .map(|c| {
                serde_json::from_value(c.clone())
                    .map_err(|_| Refused::UnknownConstraint(c.to_string()))
            })
            .collect::<Result<Vec<Constraint>, _>>()?;
        if !claims
            .sub
            .strip_prefix(PSEUDONYM)
            .is_some_and(|p| B64.decode(p).is_ok_and(|b| b.len() == 32))
        {
            return Err(Refused::Principal);
        }

        self.seen.insert(once, claims.exp);
        Ok(Verified {
            sub: claims.sub,
            iss: claims.iss,
            jti: claims.jti,
            act_sub: claims.act.sub,
            constraints,
            grant_hash: claims.grant_hash,
            trust: claims.trust,
            exp: claims.exp,
            name,
            email,
        })
    }

    /// The document at `url`, fetched again once the cached copy is older
    /// than [`STATUS_MAX_AGE`]. A failed fetch is an error, never the stale copy.
    async fn fetched(&mut self, url: &str, now: i64) -> Result<Vec<u8>, String> {
        if let Some((at, body)) = self.cache.get(url)
            && now - at < STATUS_MAX_AGE
        {
            return Ok(body.clone());
        }
        self.cache.remove(url);
        let body = self.fetch.get(url).await?;
        self.cache.insert(url.into(), (now, body.clone()));
        Ok(body)
    }

    async fn keys(&mut self, server: &str, now: i64) -> Result<Vec<Jwk>, Refused> {
        let body = self
            .fetched(&jwks_url(server), now)
            .await
            .map_err(Refused::Keys)?;
        let jwks: Jwks = serde_json::from_slice(&body).map_err(|e| Refused::Keys(e.to_string()))?;
        Ok(jwks.keys)
    }

    async fn revoked(
        &mut self,
        server: &str,
        keys: &[Jwk],
        index: u64,
        now: i64,
    ) -> Result<bool, Refused> {
        let status = |why: &str| Refused::Status(why.into());
        let body = self
            .fetched(&status_url(server), now)
            .await
            .map_err(Refused::Status)?;
        let token = std::str::from_utf8(&body).map_err(|_| status("not text"))?;
        let jwt = parse_jwt(token.trim()).map_err(|_| status("not a JWT"))?;
        if jwt.header["typ"] != "statuslist+jwt"
            || !keys
                .iter()
                .any(|k| k.verifies(jwt.input.as_bytes(), &jwt.signature))
        {
            return Err(status("not signed by the issuer"));
        }
        if jwt.payload["sub"] != status_url(server) {
            return Err(status("for another list"));
        }
        let iat = jwt.payload["iat"]
            .as_i64()
            .ok_or_else(|| status("no iat"))?;
        if now.saturating_sub(iat) > STATUS_MAX_AGE || iat.saturating_sub(now) > KB_WINDOW {
            return Err(status("stale"));
        }
        let list = &jwt.payload["status_list"];
        if list["bits"] != 1 {
            return Err(status("not one bit per mandate"));
        }
        let compressed = B64
            .decode(list["lst"].as_str().unwrap_or_default())
            .map_err(|_| status("bad lst"))?;
        let mut bits = Vec::new();
        std::io::Read::read_to_end(
            &mut flate2::read::ZlibDecoder::new(&compressed[..]),
            &mut bits,
        )
        .map_err(|_| status("bad lst"))?;
        let byte = bits
            .get((index / 8) as usize)
            .ok_or_else(|| status("index past the list"))?;
        Ok(byte >> (index % 8) & 1 == 1)
    }
}

/// The disclosed `name` and `email`. Each disclosure must be one the issuer
/// signed, at most once; nothing else is selectively disclosable.
fn disclosed(
    disclosures: &[&str],
    digests: &[String],
) -> Result<(Option<String>, Option<String>), Refused> {
    let (mut name, mut email) = (None, None);
    for disclosure in disclosures {
        if !digests.contains(&sha256_b64(disclosure.as_bytes())) {
            return Err(Refused::Signature);
        }
        let bad = || Refused::Malformed("bad disclosure".into());
        let decoded: (String, String, String) =
            serde_json::from_slice(&B64.decode(disclosure).map_err(|_| bad())?)
                .map_err(|_| bad())?;
        let slot = match decoded.1.as_str() {
            "name" => &mut name,
            "email" => &mut email,
            _ => return Err(bad()),
        };
        if slot.replace(decoded.2).is_some() {
            return Err(bad());
        }
    }
    Ok((name, email))
}

#[cfg(test)]
mod tests;
