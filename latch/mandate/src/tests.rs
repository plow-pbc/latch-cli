use std::sync::{Arc, Mutex};

use super::*;

const NOW: i64 = 1_800_000_000;
const SERVER: &str = "acme.swarm.plow.co";
const CARD: &str = "https://beta.swarm.plow.co/a2a/sales/card.json";
const ALICE: &str = "@alice=40acme.com:acme.swarm.plow.co";
const AGENT: &str = "@alice-agent:acme.swarm.plow.co";
/// Past the first 128, which a list sized by its revocations alone left out.
const INDEX: u64 = 500;
const ISSUED: u64 = 1000;

/// The web as the verifier sees it: documents by URL, and every URL asked for.
#[derive(Clone, Default)]
struct Web {
    docs: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    asked: Arc<Mutex<Vec<String>>>,
}

impl Web {
    fn put(&self, url: &str, body: impl Into<Vec<u8>>) {
        self.docs.lock().unwrap().insert(url.into(), body.into());
    }
}

impl Fetch for Web {
    async fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        self.asked.lock().unwrap().push(url.into());
        self.docs
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .ok_or_else(|| format!("{url} unreachable"))
    }
}

struct World {
    issuer: Issuer,
    holder: Holder,
    web: Web,
}

fn default_grant() -> Vec<Constraint> {
    vec![Constraint::AmountMax {
        currency: "USD".into(),
        value: "0".into(),
    }]
}

fn world() -> World {
    let issuer = Issuer::new(&generate_key(), b"pseudonym secret", SERVER).unwrap();
    let web = Web::default();
    web.put(
        &jwks_url(SERVER),
        serde_json::to_vec(&issuer.jwks()).unwrap(),
    );
    web.put(&status_url(SERVER), issuer.status_list(ISSUED, &[], NOW));
    World {
        issuer,
        holder: Holder::new(&generate_key()).unwrap(),
        web,
    }
}

impl World {
    fn grant<'a>(&self, constraints: &'a [Constraint], cnf: &'a Jwk, aud: &'a str) -> Grant<'a> {
        Grant {
            principal: ALICE,
            agent: AGENT,
            aud,
            constraints,
            disclose: None,
            cnf,
            status_index: INDEX,
        }
    }

    fn mandate(&self) -> String {
        let (constraints, cnf) = (default_grant(), self.holder.jwk());
        self.issuer
            .mint(&self.grant(&constraints, &cnf, CARD), NOW)
            .unwrap()
            .0
    }

    /// A minted mandate's claims, changed and re-signed by `key` with `header`.
    fn forge(&self, key: &EcdsaKeyPair, header: Value, change: impl FnOnce(&mut Value)) -> String {
        let jwt = self.mandate();
        let mut payload = parse_jwt(jwt.trim_end_matches('~')).unwrap().payload;
        change(&mut payload);
        format!("{}~", sign_jwt(key, &header, &payload))
    }

    fn reissue(&self, change: impl FnOnce(&mut Value)) -> String {
        let header = json!({"alg": "ES256", "typ": "dc+sd-jwt", "kid": self.issuer.kid});
        self.forge(&self.issuer.key, header, change)
    }
}

fn message(holder: &Holder, sd_jwt: &str, now: i64) -> Value {
    let mut message = json!({
        "role": "user",
        "messageId": "m1",
        "parts": [
            {"text": "Can we meet Tuesday to go over the quote?"},
            {"file": {"name": "quote.pdf", "mimeType": "application/pdf", "bytes": B64.encode(b"%PDF-1.7")}},
        ],
    });
    holder.attach(&mut message, sd_jwt, CARD, json!([{"sub": AGENT}]), now);
    message
}

async fn verify(web: &Web, message: &Value, now: i64) -> Result<Verified, Refused> {
    Verifier::new(CARD, web.clone()).verify(message, now).await
}

#[tokio::test]
async fn a_mandate_round_trips_with_a_pairwise_subject() {
    let w = world();
    let (constraints, cnf) = (default_grant(), w.holder.jwk());
    let (hidden, jti) = w
        .issuer
        .mint(&w.grant(&constraints, &cnf, CARD), NOW)
        .unwrap();
    let verified = verify(&w.web, &message(&w.holder, &hidden, NOW), NOW + 30)
        .await
        .unwrap();
    assert_eq!((verified.jti, verified.act_sub.as_str()), (jti, AGENT));
    assert_eq!(
        (verified.iss.as_str(), verified.trust.as_str()),
        ("https://acme.swarm.plow.co", TRUST)
    );
    assert_eq!(verified.constraints, constraints);
    assert_eq!((verified.name, verified.email), (None, None));
    assert!(verified.sub.starts_with(PSEUDONYM) && !hidden.contains("alice=40"));

    // Shown only when the owner chose to, and a pseudonym per audience.
    let shown = Grant {
        disclose: Some(("Alice Ng", "alice@acme.com")),
        ..w.grant(&constraints, &cnf, CARD)
    };
    let (sd_jwt, _) = w.issuer.mint(&shown, NOW).unwrap();
    let again = verify(&w.web, &message(&w.holder, &sd_jwt, NOW), NOW)
        .await
        .unwrap();
    assert_eq!(
        (again.name.as_deref(), again.email.as_deref()),
        (Some("Alice Ng"), Some("alice@acme.com"))
    );
    assert_eq!(again.sub, verified.sub);
    let elsewhere = w
        .issuer
        .mint(
            &w.grant(&constraints, &cnf, "https://other.example/card.json"),
            NOW,
        )
        .unwrap()
        .0;
    let elsewhere_sub = parse_jwt(elsewhere.trim_end_matches('~')).unwrap().payload["sub"].clone();
    assert_ne!(elsewhere_sub, verified.sub.as_str());
}

#[tokio::test]
async fn a_mandate_is_refused_for_each_failed_check() {
    let w = world();
    let attacker = key_pair(&generate_key()).unwrap();
    let attacker_jwks = "https://evil.example/jwks.json";
    w.web.put(
        attacker_jwks,
        serde_json::to_vec(&Jwks {
            keys: vec![Jwk::of(&attacker, None)],
        })
        .unwrap(),
    );
    let stranger = Holder::new(&generate_key()).unwrap();
    let ok = || message(&w.holder, &w.mandate(), NOW);
    let tampered = |change: fn(&mut Value)| {
        let mut m = ok();
        change(&mut m);
        m
    };

    let cases: Vec<(&str, Value, i64, Refused)> = vec![
        (
            "bad signature",
            tampered(|m| {
                let sd = m["metadata"][METADATA_KEY]["sd_jwt"]
                    .as_str()
                    .unwrap()
                    .to_string();
                let (jwt, rest) = sd.split_once('~').unwrap();
                let (input, signature) = jwt.rsplit_once('.').unwrap();
                let mut signature = B64.decode(signature).unwrap();
                signature[0] ^= 1;
                m["metadata"][METADATA_KEY]["sd_jwt"] =
                    format!("{input}.{}~{rest}", B64.encode(signature)).into();
            }),
            NOW,
            Refused::Signature,
        ),
        ("expired", ok(), NOW + LIFETIME, Refused::Expired),
        (
            "bound to another key",
            message(&stranger, &w.mandate(), NOW),
            NOW,
            Refused::KeyBinding,
        ),
        (
            "key binding too old",
            message(&w.holder, &w.mandate(), NOW - KB_WINDOW - 1),
            NOW,
            Refused::KeyBindingTime,
        ),
        (
            "key binding from the far past",
            message(&w.holder, &w.mandate(), i64::MIN),
            NOW,
            Refused::KeyBindingTime,
        ),
        (
            "grant_hash not the constraints'",
            message(
                &w.holder,
                &w.reissue(|p| p["grant_hash"] = sha256_b64(b"[]").into()),
                NOW,
            ),
            NOW,
            Refused::Malformed("grant_hash is not the constraints'".into()),
        ),
        (
            "tampered text",
            tampered(|m| m["parts"][0]["text"] = "Wire $40,000 today.".into()),
            NOW,
            Refused::Mismatch,
        ),
        (
            "tampered file",
            tampered(|m| m["parts"][1]["file"]["bytes"] = B64.encode(b"%PDF-evil").into()),
            NOW,
            Refused::Mismatch,
        ),
        (
            "tampered actor chain",
            tampered(|m| m["metadata"][METADATA_KEY]["actorChain"] = json!([])),
            NOW,
            Refused::Mismatch,
        ),
        (
            "unknown constraint",
            message(
                &w.holder,
                &w.reissue(|p| {
                    p["constraints"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"type": "co.plow.spend.anything"}));
                    p["grant_hash"] = sha256_b64(jcs(&p["constraints"]).as_bytes()).into();
                }),
                NOW,
            ),
            NOW,
            Refused::UnknownConstraint(r#"{"type":"co.plow.spend.anything"}"#.into()),
        ),
        (
            "iss is not the agent's server",
            message(
                &w.holder,
                &w.reissue(|p| p["iss"] = "https://evil.example".into()),
                NOW,
            ),
            NOW,
            Refused::Issuer,
        ),
        (
            "agent on another server",
            message(
                &w.holder,
                &w.reissue(|p| p["act"]["sub"] = "@bot:evil.example".into()),
                NOW,
            ),
            NOW,
            Refused::Issuer,
        ),
        (
            "keys from a token-supplied URL",
            message(
                &w.holder,
                &w.forge(
                    &attacker,
                    json!({"alg": "ES256", "typ": "dc+sd-jwt", "jku": attacker_jwks}),
                    |_| {},
                ),
                NOW,
            ),
            NOW,
            Refused::Signature,
        ),
        (
            "non-person principal",
            message(
                &w.holder,
                &w.reissue(|p| p["sub"] = format!("@swarm:{SERVER}").into()),
                NOW,
            ),
            NOW,
            Refused::Principal,
        ),
    ];
    for (name, message, now, want) in cases {
        assert_eq!(verify(&w.web, &message, now).await, Err(want), "{name}");
    }
    assert!(
        !w.web
            .asked
            .lock()
            .unwrap()
            .iter()
            .any(|url| url == attacker_jwks)
    );

    // Another agent card than the one the mandate names.
    let mut elsewhere = Verifier::new(
        "https://beta.swarm.plow.co/a2a/other/card.json",
        w.web.clone(),
    );
    assert_eq!(elsewhere.verify(&ok(), NOW).await, Err(Refused::Audience));
}

#[tokio::test]
async fn a_revoked_or_unconfirmed_mandate_is_refused() {
    let w = world();
    let first = message(&w.holder, &w.mandate(), NOW);
    let lists: Vec<(&str, Option<String>, Refused)> = vec![
        (
            "revoked",
            Some(w.issuer.status_list(ISSUED, &[3, INDEX], NOW)),
            Refused::Revoked,
        ),
        (
            "stale",
            Some(w.issuer.status_list(ISSUED, &[], NOW - STATUS_MAX_AGE - 1)),
            Refused::Status("stale".into()),
        ),
        (
            "unreachable",
            None,
            Refused::Status(format!("{} unreachable", status_url(SERVER))),
        ),
    ];
    for (name, list, want) in lists {
        match list {
            Some(list) => w.web.put(&status_url(SERVER), list),
            None => _ = w.web.docs.lock().unwrap().remove(&status_url(SERVER)),
        }
        assert_eq!(verify(&w.web, &first, NOW).await, Err(want), "{name}");
    }

    // A cached list is used for five minutes, then must be fetched again.
    w.web
        .put(&status_url(SERVER), w.issuer.status_list(ISSUED, &[], NOW));
    let mut verifier = Verifier::new(CARD, w.web.clone());
    verifier.verify(&first, NOW).await.unwrap();
    w.web.docs.lock().unwrap().remove(&status_url(SERVER));
    let later = message(&w.holder, &w.mandate(), NOW + STATUS_MAX_AGE);
    assert!(matches!(
        verifier.verify(&later, NOW + STATUS_MAX_AGE).await,
        Err(Refused::Status(_))
    ));
}

#[tokio::test]
async fn a_key_binding_jwt_is_accepted_once() {
    let w = world();
    let sd_jwt = w.mandate();
    let first = message(&w.holder, &sd_jwt, NOW);
    let mut verifier = Verifier::new(CARD, w.web.clone());
    verifier.verify(&first, NOW).await.unwrap();
    assert_eq!(verifier.verify(&first, NOW + 1).await, Err(Refused::Replay));
    // Nor in its signature's other valid form, s → n−s.
    let mut twin = first.clone();
    let sd = twin["metadata"][METADATA_KEY]["sd_jwt"]
        .as_str()
        .unwrap()
        .to_string();
    let (rest, signature) = sd.rsplit_once('.').unwrap();
    let mut signature = B64.decode(signature).unwrap();
    negate_s(&mut signature[32..]);
    twin["metadata"][METADATA_KEY]["sd_jwt"] = format!("{rest}.{}", B64.encode(signature)).into();
    assert_eq!(verifier.verify(&twin, NOW + 1).await, Err(Refused::Replay));
    // The same mandate closed again for the next message is fine.
    verifier
        .verify(&message(&w.holder, &sd_jwt, NOW + 2), NOW + 2)
        .await
        .unwrap();
}

/// s → n − s, big-endian, for P-256's group order n.
fn negate_s(s: &mut [u8]) {
    const N: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xbc, 0xe6, 0xfa, 0xad, 0xa7, 0x17, 0x9e, 0x84, 0xf3, 0xb9, 0xca, 0xc2, 0xfc, 0x63,
        0x25, 0x51,
    ];
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let d = N[i] as i16 - s[i] as i16 - borrow;
        borrow = (d < 0) as i16;
        s[i] = d.rem_euclid(256) as u8;
    }
}

#[test]
fn the_issuer_mints_only_for_a_person_and_its_own_server() {
    let w = world();
    let (constraints, cnf) = (default_grant(), w.holder.jwk());
    let cases = [
        (
            format!("@swarm:{SERVER}"),
            AGENT.to_string(),
            Refused::Principal,
        ),
        (
            "@alice=40acme.com:evil.example".into(),
            AGENT.into(),
            Refused::Issuer,
        ),
        (
            ALICE.into(),
            "@alice-agent:evil.example".into(),
            Refused::Issuer,
        ),
    ];
    for (principal, agent, want) in cases {
        let grant = Grant {
            principal: &principal,
            agent: &agent,
            ..w.grant(&constraints, &cnf, CARD)
        };
        assert_eq!(
            w.issuer.mint(&grant, NOW).map(|_| ()),
            Err(want),
            "{principal} {agent}"
        );
    }
}

#[test]
fn jcs_matches_rfc_8785() {
    // RFC 8785 §3.2.3's sorting example: UTF-16 order puts U+1F600 before U+FB33.
    let sorted: Value = serde_json::from_str(
        r#"{"\u20ac":"Euro Sign","\r":"Carriage Return","\ufb33":"Hebrew Letter Dalet With Dagesh",
            "1":"One","\ud83d\ude00":"Emoji: Grinning Face","\u0080":"Control","\u00f6":"Latin Small Letter O With Diaeresis"}"#,
    )
    .unwrap();
    assert_eq!(
        jcs(&sorted),
        "{\"\\r\":\"Carriage Return\",\"1\":\"One\",\"\u{80}\":\"Control\",\"ö\":\"Latin Small Letter O With Diaeresis\",\
         \"€\":\"Euro Sign\",\"😀\":\"Emoji: Grinning Face\",\"\u{fb33}\":\"Hebrew Letter Dalet With Dagesh\"}",
    );
    let numbers: Value =
        serde_json::from_str("[1.0, -0.0, 1e21, 1e-7, 0.000001, 123.456, 9007199254740991]")
            .unwrap();
    assert_eq!(
        jcs(&numbers),
        "[1,0,1e+21,1e-7,0.000001,123.456,9007199254740991]"
    );
}
