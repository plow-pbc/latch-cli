//! Where an item may be filled. Sites are compared by **registrable
//! domain** (the public-suffix list, private suffixes included): a login for
//! `example.com` fills on `www.example.com` and `login.example.com`, as the
//! Latch app's did, never on `example.com.evil.net` or `example.co`.
//!
//! Every fill needs both the field's frame and the top-level page on HTTPS
//! with a real host: plain `http`, an IP address, a local host, a missing
//! page address and an opaque origin (a sandboxed frame) are all refused.

use std::net::IpAddr;

use url::Url;

use super::item::Meta;

/// The registrable domain of a site as the owner wrote it: a bare host
/// (`example.com`) or a URL (`https://www.example.com/login`).
pub fn registrable(site: &str) -> Result<String, String> {
    let site = site.trim();
    let with_scheme = if site.contains("://") {
        site.to_string()
    } else {
        format!("https://{site}")
    };
    let url =
        Url::parse(&with_scheme).map_err(|_| format!("could not read a site from {site:?}"))?;
    let host = url
        .host_str()
        .ok_or_else(|| format!("could not read a site from {site:?}"))?;
    domain_of(host).map_err(|why| format!("{site:?}: {why}"))
}

/// The site a page is on, for telling apart what sessions on one profile
/// do: its registrable domain, or for a host without one (an IP address, a
/// local host) the host itself. None for a page with no host.
pub fn site_of(url: &str) -> Option<String> {
    let url = Url::parse(url).ok()?;
    let host = url.host_str()?;
    Some(domain_of(host).unwrap_or_else(|_| {
        host.trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase()
    }))
}

/// The registrable domain of `host`, or why it has none latch fills on.
fn domain_of(host: &str) -> Result<String, String> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare.parse::<IpAddr>().is_ok() {
        return Err("an IP address is never a site latch fills on".into());
    }
    if host == "localhost" || host.ends_with(".localhost") || !host.contains('.') {
        return Err("a local host is never a site latch fills on".into());
    }
    match psl::domain_str(&host) {
        Some(domain) => Ok(domain.to_string()),
        None => Err("that is a public suffix, shared by many sites; name the site under it".into()),
    }
}

/// An origin a fill was asked into, checked: its host and registrable
/// domain, or why no value goes there.
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub host: String,
    pub domain: String,
}

pub fn check(origin: &str) -> Result<Checked, String> {
    if origin.is_empty() {
        return Err("the page has no address".into());
    }
    if origin == "opaque" || origin == "null" {
        return Err("the field is in an opaque (sandboxed) frame".into());
    }
    let url = Url::parse(origin).map_err(|_| "the page's origin could not be read".to_string())?;
    if url.scheme() != "https" {
        return Err(format!("{} is not HTTPS", url.scheme()));
    }
    let host = url
        .host_str()
        .ok_or_else(|| "the page's origin has no host".to_string())?;
    let domain = domain_of(host)?;
    Ok(Checked {
        host: host.to_string(),
        domain,
    })
}

/// Whether `item` may be filled into a field whose frame has origin
/// `frame` on a page whose top-level origin is `top`:
/// - a login: both share one of its sites' registrable domains (the top
///   may instead be one of its other top-level sites);
/// - a card or identity: any top-level site, or one of its "only on these
///   sites"; the frame (a payment processor's, say) may be another site;
/// - a note: any site.
///
/// In every case both origins must pass `check`. The error is the reason,
/// for the audit and the agent; it never holds a value.
pub fn allows(item: &Meta, frame: &str, top: &str) -> Result<(Checked, Checked), String> {
    let frame = check(frame).map_err(|why| format!("field frame: {why}"))?;
    let top = check(top).map_err(|why| format!("top-level page: {why}"))?;
    let ours = |domain: &str, list: &[String]| list.iter().any(|site| site == domain);
    match item.kind {
        "login" => {
            if !ours(&frame.domain, &item.sites) {
                return Err(format!(
                    "the field is on {}, not on this login's site",
                    frame.domain
                ));
            }
            if !ours(&top.domain, &item.sites) && !ours(&top.domain, &item.other_top_sites) {
                return Err(format!(
                    "the page is on {}, not on this login's site",
                    top.domain
                ));
            }
        }
        "card" | "identity" => {
            if !item.sites.is_empty() && !ours(&top.domain, &item.sites) {
                return Err(format!(
                    "the page is on {}, not on this item's sites",
                    top.domain
                ));
            }
        }
        "note" => {}
        _ => return Err("this item does not fill".into()),
    }
    Ok((frame, top))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::item::{CARD, IDENTITY, Item, LOGIN, NOTE, Raw};

    fn item(kind: u8, sites: &[&str], tops: &[&str]) -> Meta {
        Item {
            id: "v1".into(),
            revision: 1,
            sites: sites.iter().map(|s| s.to_string()).collect(),
            other_top_sites: tops.iter().map(|s| s.to_string()).collect(),
            raw: Raw {
                kind,
                name: "t".into(),
                ..Raw::default()
            },
        }
        .meta()
    }

    #[test]
    fn sites_are_registrable_domains_private_suffixes_included() {
        assert_eq!(
            registrable("https://www.Example.com/login").unwrap(),
            "example.com"
        );
        assert_eq!(registrable("login.example.co.uk").unwrap(), "example.co.uk");
        assert_eq!(registrable("toscrape.com").unwrap(), "toscrape.com");
        // herokuapp.com is a private suffix: each app is its own site.
        assert_eq!(
            registrable("the-internet.herokuapp.com").unwrap(),
            "the-internet.herokuapp.com"
        );
        for refused in [
            "herokuapp.com",
            "co.uk",
            "localhost",
            "127.0.0.1",
            "[::1]",
            "intranet",
            "a.localhost",
        ] {
            assert!(registrable(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn a_login_fills_on_its_site_and_subdomains_only() {
        let login = item(LOGIN, &["toscrape.com"], &[]);
        assert!(
            allows(
                &login,
                "https://quotes.toscrape.com",
                "https://quotes.toscrape.com"
            )
            .is_ok()
        );
        assert!(allows(&login, "https://toscrape.com", "https://toscrape.com").is_ok());
        for (frame, top, why) in [
            (
                "https://toscrape.com.evil.example",
                "https://toscrape.com.evil.example",
                "not on this login's site",
            ),
            (
                "https://toscrape.co",
                "https://toscrape.co",
                "not on this login's site",
            ),
            (
                "https://toscrape-com.net",
                "https://toscrape-com.net",
                "not on this login's site",
            ),
            (
                "https://xn--tscrape-0za.com",
                "https://xn--tscrape-0za.com",
                "not on this login's site",
            ),
            (
                "http://quotes.toscrape.com",
                "http://quotes.toscrape.com",
                "not HTTPS",
            ),
            ("opaque", "https://quotes.toscrape.com", "opaque"),
            ("https://quotes.toscrape.com", "", "no address"),
            (
                "https://127.0.0.1:8443",
                "https://127.0.0.1:8443",
                "IP address",
            ),
            (
                "https://quotes.toscrape.com",
                "https://evil.example",
                "the page is on evil.example",
            ),
            (
                "https://evil.example",
                "https://quotes.toscrape.com",
                "the field is on evil.example",
            ),
        ] {
            let refused = allows(&login, frame, top).unwrap_err();
            assert!(refused.contains(why), "{frame} {top}: {refused}");
        }
        // Another top-level site the login allows embeds its sign-in frame.
        let embedded = item(LOGIN, &["toscrape.com"], &["portal.example"]);
        assert!(
            allows(
                &embedded,
                "https://login.toscrape.com",
                "https://www.portal.example"
            )
            .is_ok()
        );
    }

    #[test]
    fn cards_fill_on_any_site_unless_listed_and_notes_anywhere() {
        let card = item(CARD, &[], &[]);
        assert!(allows(&card, "https://js.stripe.com", "https://shop.example").is_ok());
        assert!(allows(&card, "http://shop.example", "https://shop.example").is_err());
        assert!(allows(&card, "opaque", "https://shop.example").is_err());
        let listed = item(IDENTITY, &["shop.example"], &[]);
        assert!(allows(&listed, "https://pay.other", "https://www.shop.example").is_ok());
        assert!(allows(&listed, "https://shop.example", "https://elsewhere.example").is_err());
        let note = item(NOTE, &[], &[]);
        assert!(allows(&note, "https://a.example", "https://b.example").is_ok());
        assert!(allows(&note, "http://a.example", "http://a.example").is_err());
    }
}
