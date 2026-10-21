//! What an agent is shown of a page: the elements on screen, at most
//! `SHOWN` of them, and the frames they are in, each frame's address cut
//! short. The answer says how many were left out and how to list them all.
//!
//! Only the display is cut. The snapshot keeps every element it read, so
//! the ref of one left out works as soon as `observe --all` (or a scroll
//! and an `observe`) shows it; the audit, the gatekeeper and reservations
//! read full addresses from the page, never from this.

use serde_json::{Map, Value, json};

/// The most elements one answer shows. A heavy page (redfin.com's home)
/// reads the 200 a snapshot holds, about 18 KB of JSON, of which 24 are on
/// screen; 60 leaves room for a denser screen at about 6 KB.
pub const SHOWN: usize = 60;
/// The longest frame address shown, in characters.
const URL_CHARS: usize = 80;

/// Bounds every page in `value`, an answer or an error's details: each
/// object holding a snapshot's `elements`.
pub fn bound(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if object.get("snapshot").is_some_and(Value::is_string)
                && object.get("elements").is_some_and(Value::is_array)
            {
                page(object);
            }
            object.values_mut().for_each(bound);
        }
        Value::Array(items) => items.iter_mut().for_each(bound),
        _ => {}
    }
}

fn page(page: &mut Map<String, Value>) {
    let Some(Value::Array(elements)) = page.remove("elements") else {
        return;
    };
    let total = elements.len();
    let shown: Vec<Value> = elements
        .into_iter()
        .filter(|element| element["offscreen"] != true)
        .take(SHOWN)
        .collect();
    let left_out = total - shown.len();
    let mut frames_left_out = 0;
    if let Some(Value::Array(frames)) = page.remove("frames") {
        let used = |frame: &Value| {
            frame["frame"] == "f0"
                || shown
                    .iter()
                    .any(|element| element["frame"] == frame["frame"])
        };
        let (kept, dropped): (Vec<Value>, Vec<Value>) = frames.into_iter().partition(used);
        frames_left_out = dropped.len();
        let kept: Vec<Value> = kept
            .into_iter()
            .map(|mut frame| {
                if let Some(url) = frame["url"].as_str() {
                    frame["url"] = json!(short(url));
                }
                frame
            })
            .collect();
        if kept.len() > 1 {
            page.insert("frames".into(), json!(kept));
        }
    }
    page.insert("elements".into(), json!(shown));
    let mut omitted = Vec::new();
    if left_out > 0 {
        omitted.push(format!(
            "{} (off screen, or past the first {SHOWN})",
            count(left_out, "element")
        ));
    }
    if frames_left_out > 0 {
        omitted.push(format!(
            "{} with none of them shown",
            count(frames_left_out, "frame")
        ));
    }
    if !omitted.is_empty() {
        page.insert(
            "omitted".into(),
            json!(format!(
                "{} left out; `latch browser observe --all` lists every one",
                omitted.join(" and ")
            )),
        );
    }
}

fn count(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

/// An address for display: without its query, fragment or `;` parameters,
/// and at most `URL_CHARS` long, `…` where it was cut.
fn short(url: &str) -> String {
    let bare = url.split(['?', '#', ';']).next().unwrap_or(url);
    let mut shown: String = bare.chars().take(URL_CHARS - 1).collect();
    if shown.len() < url.len() {
        shown.push('…');
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_shows_its_on_screen_elements_and_their_frames_and_says_what_it_left_out() {
        let mut elements: Vec<Value> = (1..=3)
            .map(|n| json!({"ref": format!("e{n}"), "role": "link", "name": "On"}))
            .collect();
        elements.push(json!({"ref": "e4", "role": "button", "name": "Pay", "frame": "f2"}));
        elements.push(json!({"ref": "e5", "role": "link", "name": "Footer", "offscreen": true}));
        elements.push(
            json!({"ref": "e6", "role": "link", "name": "Ad", "frame": "f1", "offscreen": true}),
        );
        let ad = format!(
            "https://ads.example/activityi;src=1;ord={}?x=1",
            "9".repeat(300)
        );
        let mut answer = json!({
            "snapshot": "o2",
            "url": "https://shop.example/cart?step=2",
            "elements": elements,
            "frames": [
                {"frame": "f0", "origin": "https://shop.example", "url": "https://shop.example/cart?step=2"},
                {"frame": "f1", "origin": "https://ads.example", "parent": "f0", "url": ad},
                {"frame": "f2", "origin": "https://pay.example", "parent": "f0", "url": "https://pay.example/card"},
            ],
        });
        bound(&mut answer);
        let refs: Vec<&str> = answer["elements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["ref"].as_str().unwrap())
            .collect();
        assert_eq!(refs, ["e1", "e2", "e3", "e4"]);
        // The payment frame stays, named as before; the ad frame, with no
        // element shown, is counted, not listed.
        assert_eq!(
            answer["frames"],
            json!([
                {"frame": "f0", "origin": "https://shop.example", "url": "https://shop.example/cart…"},
                {"frame": "f2", "origin": "https://pay.example", "parent": "f0", "url": "https://pay.example/card"},
            ])
        );
        // The page's own address is not a display copy: it stays whole.
        assert_eq!(answer["url"], "https://shop.example/cart?step=2");
        let omitted = answer["omitted"].as_str().unwrap();
        assert!(omitted.starts_with("2 elements"), "{omitted}");
        assert!(omitted.contains("and 1 frame with none"), "{omitted}");
        assert!(omitted.contains("observe --all"), "{omitted}");
    }

    #[test]
    fn at_most_shown_elements_and_nothing_said_when_nothing_is_left_out() {
        let many: Vec<Value> = (1..=SHOWN + 5)
            .map(|n| json!({"ref": format!("e{n}"), "role": "link", "name": "x"}))
            .collect();
        let mut answer = json!({"page": {"snapshot": "o1", "elements": many}});
        bound(&mut answer);
        assert_eq!(answer["page"]["elements"].as_array().unwrap().len(), SHOWN);
        assert!(
            answer["page"]["omitted"]
                .as_str()
                .unwrap()
                .starts_with("5 elements")
        );

        let mut small = json!({"snapshot": "o1", "elements": [{"ref": "e1"}], "frames": [{"frame": "f0", "url": "about:blank"}, {"frame": "f1", "url": "about:blank"}]});
        bound(&mut small);
        assert!(small.get("omitted").is_some(), "the empty frame is counted");
        let mut whole = json!({"snapshot": "o1", "elements": [{"ref": "e1"}]});
        bound(&mut whole);
        assert_eq!(
            whole,
            json!({"snapshot": "o1", "elements": [{"ref": "e1"}]})
        );
    }

    #[test]
    fn a_short_address_is_cut_at_its_query_or_length() {
        assert_eq!(short("about:blank"), "about:blank");
        assert_eq!(short("https://a.example/x?y=1"), "https://a.example/x…");
        assert_eq!(short("https://a.example/p;q=1"), "https://a.example/p…");
        let long = format!("https://a.example/{}", "z".repeat(200));
        assert_eq!(short(&long).chars().count(), URL_CHARS);
        assert!(short(&long).ends_with('…'));
    }
}
