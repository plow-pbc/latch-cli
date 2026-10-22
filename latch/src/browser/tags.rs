//! Workaround tags for an agent's `eval` script: what it does that latch's
//! ordinary commands would do instead, or that only raw browser access
//! could. They are recorded in the audit and never block anything; with
//! `unsupported_capability` hits, they show what raw commands would have
//! been needed for.
//!
//! The tags come from the script's text, so they can miss what a script
//! hides and flag what it only mentions. They are evidence, not a gate.

/// The tags `script` earns, in a fixed order.
pub fn workaround_tags(script: &str) -> Vec<&'static str> {
    let code = without_comments(script);
    let mut tags = Vec::new();
    let has = |words: &[&str]| words.iter().any(|word| code.contains(word));
    if has(&[
        "contentWindow",
        "contentDocument",
        "frameElement",
        "window.parent",
        "window.top",
        "frames[",
    ]) {
        tags.push("other_frames");
    }
    if has(&["shadowRoot", "attachShadow"]) {
        tags.push("shadow_roots");
    }
    if synthetic(&code) {
        tags.push("synthetic_events");
    }
    if [
        "value",
        "checked",
        "selectedIndex",
        "textContent",
        "innerText",
    ]
    .iter()
    .any(|property| assigns(&code, &format!(".{property}")))
    {
        tags.push("sets_field_values");
    }
    if assigns(&code, "fetch") || has(&["XMLHttpRequest.prototype", "XMLHttpRequest ="]) {
        tags.push("patches_network");
    }
    if has(&[
        "document.cookie",
        "localStorage",
        "sessionStorage",
        "indexedDB",
        "cookieStore",
    ]) {
        tags.push("reads_cookies_or_storage");
    }
    if has(&[
        "setInterval",
        "setTimeout",
        "requestAnimationFrame",
        "MutationObserver",
    ]) {
        tags.push("polls_instead_of_wait");
    }
    tags
}

/// An event made by script: `dispatchEvent`, `createEvent`, `.click()`, or
/// any `new …Event(`.
fn synthetic(code: &str) -> bool {
    code.contains("dispatchEvent")
        || code.contains("createEvent")
        || code.contains(".click()")
        || code.match_indices("Event(").any(|(at, _)| {
            code[..at]
                .trim_end_matches(char::is_alphanumeric)
                .ends_with("new ")
        })
}

/// Whether `code` assigns to something ending in `name` (`x.value = 1`,
/// `window.fetch = …`, `fetch=…`), not just compares it (`==`, `===`).
fn assigns(code: &str, name: &str) -> bool {
    code.match_indices(name).any(|(at, _)| {
        let before = code[..at].chars().next_back();
        // `fetch` alone must be a whole word; `.value` starts with its dot.
        let whole =
            name.starts_with('.') || !before.is_some_and(|c| c.is_alphanumeric() || c == '_');
        let after = code[at + name.len()..].trim_start();
        let next = after.chars().nth(1);
        let assignment = after.starts_with('=') && next != Some('=') && next != Some('>');
        let ends_word = !code[at + name.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        whole && ends_word && assignment
    })
}

/// `script` with `//` and `/* */` comments blanked, so a note about
/// `fetch` earns no tag. Strings are kept: `"fetch"` in a string is rare
/// enough, and a tag is only evidence.
fn without_comments(script: &str) -> String {
    let mut out = String::with_capacity(script.len());
    let mut rest = script;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("//") {
            rest = after.find('\n').map_or("", |end| &after[end..]);
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.find("*/").map_or("", |end| &after[end + 2..]);
            out.push(' ');
        } else {
            let ch = rest.chars().next().expect("not empty");
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    out
}

/// At most `max` bytes of `text`, cut on a character boundary, and whether
/// anything was cut.
pub fn capped(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_that_patches_fetch_is_tagged() {
        let script = "const real = window.fetch;\nwindow.fetch = (...a) => { seen.push(a[0]); return real(...a); };\nlocalStorage.getItem('x')";
        assert_eq!(
            workaround_tags(script),
            ["patches_network", "reads_cookies_or_storage"]
        );
        assert_eq!(workaround_tags("fetch = null"), ["patches_network"]);
        assert_eq!(
            workaround_tags("XMLHttpRequest.prototype.open = f"),
            ["patches_network"]
        );
    }

    #[test]
    fn calling_or_comparing_is_not_assigning() {
        assert!(workaround_tags("await fetch('/api').then(r => r.json())").is_empty());
        assert!(workaround_tags("input.value === 'x' || input.value == 'y'").is_empty());
        assert!(workaround_tags("const prefetch = 1; refetch = 2").is_empty());
        assert!(workaround_tags("// window.fetch = spy\n/* el.value = 1 */ 1 + 1").is_empty());
        assert!(workaround_tags("const f = (x) => x").is_empty());
    }

    #[test]
    fn each_workaround_has_its_tag() {
        for (script, tag) in [
            (
                "document.querySelector('iframe').contentDocument.body",
                "other_frames",
            ),
            ("host.shadowRoot.querySelector('b')", "shadow_roots"),
            (
                "el.dispatchEvent(new MouseEvent('click'))",
                "synthetic_events",
            ),
            ("button.click()", "synthetic_events"),
            ("field.value = 'Ada'", "sets_field_values"),
            ("box.checked=true", "sets_field_values"),
            ("document.cookie", "reads_cookies_or_storage"),
            (
                "await new Promise(r => setTimeout(r, 500))",
                "polls_instead_of_wait",
            ),
        ] {
            assert!(
                workaround_tags(script).contains(&tag),
                "{script}: {:?}",
                workaround_tags(script)
            );
        }
    }

    #[test]
    fn a_long_script_is_capped_on_a_character_boundary() {
        assert_eq!(capped("abc", 4), ("abc", false));
        let text = "é".repeat(3000);
        let (kept, cut) = capped(&text, 4096);
        assert!(cut && kept.len() == 4096 && kept.chars().all(|c| c == 'é'));
        let (kept, _) = capped("aé", 2);
        assert_eq!(kept, "a");
    }
}
