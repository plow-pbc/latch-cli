//! An identity's secret (a Social Security number) filled as a card is:
//! no confirmation and no press, blank in pictures and out of scripts
//! while it is in the page's fields, emptied after a while, and sent by
//! the agent's own press of the page's button.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::cards::reds_and_blues;
use super::{Harness, SSN, find, item, latch, site, test_state};
use crate::browser::host::Browsers;
use crate::vault::host::Vault;
use crate::vault::item::IDENTITY;
use crate::vault::store::Backend;

conformance!(an_identity_secret_fills_like_a_card);

async fn an_identity_secret_fills_like_a_card(engine: &str) {
    let (_dir, state, _area) = test_state(engine);
    let vault = Vault::new(state.clone());
    let identity = vault
        .with(|s| {
            s.add(item(
                IDENTITY,
                "Ada",
                &[],
                &[("first name", "Ada"), ("ssn", SSN)],
            ))
        })
        .unwrap();
    let local = format!("http://127.0.0.1:{}", site());
    let mut browsers = Browsers::new(state, vault, super::super::HEADLESS);
    browsers.test = Some((
        latch(),
        vec![(local.clone(), "https://forms.example".into())],
    ));
    // The agent presses Submit itself, a click the decision model judges.
    browsers.provider = Some(Arc::new(super::super::Calm));
    browsers.filled_for = Some(Duration::from_secs(3));
    let mut h = Harness::start(browsers, engine).await;
    let page = h
        .call("open", json!({"url": format!("{local}/id-form.html")}))
        .await
        .unwrap();
    // The name the agent types itself shows, blue.
    let step = format!("type {} \"Ada\"", find(&page, "textbox", "First name"));
    h.call(
        "do",
        json!({"request": "r-name", "plan": {"steps": [crate::browser::plan::short_step(&step).unwrap()]}}),
    )
    .await
    .unwrap();
    let ssn = json!([["ssn", [find(&page, "textbox", "Social Security number")]]]);
    let fill = async |h: &mut Harness| {
        let (filled, _) = h
            .gated_fill(json!({"item": identity.item, "fields": ssn}))
            .await;
        filled.unwrap()
    };

    // A fill that would press the form's button is refused: submitting is
    // the agent's own click.
    let submit = find(&page, "button", "Submit application");
    let (pressed, _) = h
        .gated_fill(json!({"item": identity.item, "fields": ssn, "press": submit}))
        .await;
    let pressed = pressed.unwrap_err();
    assert_eq!(pressed.code, "bad_arguments");
    assert!(
        pressed.message.contains("an identity fill presses nothing"),
        "{}",
        pressed.message
    );

    // Filled with no press and no confirmation: the number waits in its
    // field for the agent's own press.
    let filled = fill(&mut h).await;
    println!("filled: {filled}");
    assert_eq!(filled["submitted"], false);
    let next = filled["next"].as_str().unwrap();
    assert!(
        next.contains("screenshots show them blank") && next.contains("3 seconds after this fill"),
        "{next}"
    );
    let refused = h.call("eval", json!({"script": "1"})).await.unwrap_err();
    assert_eq!(refused.code, "secret_on_page");
    let (red, blue) = reds_and_blues(&h.shot().await.unwrap());
    assert_eq!(red, 0, "a screenshot shows the number's field");
    assert!(blue > 50, "a screenshot lost the typed name: {blue} pixels");

    // Left there, the number's field is emptied once the time is up; the
    // agent's own text stays.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let left = h
        .call(
            "eval",
            json!({"script": "[...document.querySelectorAll('input')].map(i => i.value)"}),
        )
        .await
        .unwrap();
    assert_eq!(left["value"], json!(["Ada", ""]), "{left}");

    // Filled again, the agent's own press sends it.
    fill(&mut h).await;
    h.click(&submit).await.unwrap();
    h.call("wait", json!({"text": "application:", "within": 5}))
        .await
        .unwrap();
    let page = h.text().await;
    assert!(page.contains("application: ssn ok, name Ada"), "{page}");
    let everything = h.shown.join("\n");
    assert!(
        !everything.contains(SSN) && !everything.contains(&SSN.replace('-', "")),
        "the number reached a result or the audit"
    );
    h.call("close", json!({})).await.ok();
}
