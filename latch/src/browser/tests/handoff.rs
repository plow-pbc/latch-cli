//! `handoff` through the host's own path: the session paused for the
//! owner while the runtime's other sessions go on, the pending handoff
//! where the dashboard reads it, `handoff done` handing it back with every
//! old ref refused, the deadline, and `no_screen` at once on a host with no
//! screen (forced here; the real screenless check is the Pi's).

use std::time::{Duration, Instant};

use serde_json::json;

use std::sync::Arc;

use super::{Calm, Host, act, latch, local_page, test_state};
use crate::browser::handoff;
use crate::browser::host::Browsers;

const OWNER_AGENT: &str = "handoff-caller";

conformance!(
    a_handoff_pauses_the_session_until_the_owner_hands_it_back,
    a_plan_running_when_the_handoff_starts_sends_nothing_more,
    refs_are_forgotten_even_while_another_session_holds_the_browser,
    ;
    mac:
    a_host_with_no_screen_answers_needs_owner_at_once,
);

/// The host's browsers, with its screen check forced to `screen`.
fn browsers(state: &std::path::Path, screen: bool) -> Arc<Browsers> {
    let mut browsers = Browsers::new(
        state.to_path_buf(),
        crate::vault::host::Vault::new(state.to_path_buf()),
        super::HEADLESS,
    );
    browsers.test = Some((latch(), Vec::new()));
    browsers.provider = Some(Arc::new(Calm));
    browsers.screen = Some(screen);
    Arc::new(browsers)
}

async fn a_handoff_pauses_the_session_until_the_owner_hands_it_back(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["default"]);
    let page = local_page();
    let browsers = browsers(&state, true);
    let open = || async {
        let (opened, _) = super::run_as(
            &browsers,
            OWNER_AGENT.into(),
            "open",
            "default",
            json!({"url": page}),
        )
        .await;
        let opened = opened.unwrap();
        (opened["session"].as_str().unwrap().to_string(), opened)
    };
    let (id, opened) = open().await;
    let old = opened["snapshot"].as_str().unwrap().to_string();
    let (other, _) = open().await;

    // The handoff waits; meanwhile the pending handoff is where the
    // dashboard and `handoff done` read it.
    let waiting = {
        let (browsers, id) = (browsers.clone(), id.clone());
        tokio::spawn(async move {
            super::run_as(
                &browsers,
                OWNER_AGENT.into(),
                "handoff",
                &id,
                json!({"reason": "captcha", "within": 60}),
            )
            .await
        })
    };
    let started = Instant::now();
    let pending = loop {
        let pending = handoff::pending(&state);
        if !pending.is_empty() || started.elapsed() > Duration::from_secs(20) {
            break pending;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    println!("pending: {}", handoff::line(&pending[0]));
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["reason"], "captcha");
    let line = handoff::line(&pending[0]);
    assert!(
        line.contains(pending[0]["session"].as_str().unwrap()) && line.contains("handoff-call"),
        "{line}"
    );
    assert_ne!(
        pending[0]["session"],
        json!(id),
        "the file holds the id's hash, never the id"
    );

    // The session is paused; the runtime's other session goes on.
    let (paused, _) = super::run_as(&browsers, OWNER_AGENT.into(), "observe", &id, json!({})).await;
    assert_eq!(paused.unwrap_err().code, "session_paused");
    let (going, _) =
        super::run_as(&browsers, OWNER_AGENT.into(), "observe", &other, json!({})).await;
    assert!(going.is_ok(), "the other session works during the handoff");

    // The owner hands it back on the host.
    let done = handoff::done(&state, None).unwrap();
    assert_eq!(done["handed_back"], pending[0]["session"]);
    let (handed, audit) = waiting.await.unwrap();
    let handed = handed.unwrap();
    println!("handed back: {handed}");
    assert_eq!(handed["handed_back"], true);
    assert!(handed["page"]["snapshot"].is_string());
    assert_eq!(audit["reason"], "captcha");
    assert!(handoff::pending(&state).is_empty());

    // Every ref from before the handoff is refused; fresh ones work.
    let mut stale = act("scroll e1 down");
    stale["plan"]["steps"][0]["literal"]["snapshot"] = json!(old);
    let (refused, _) = super::run_as(&browsers, OWNER_AGENT.into(), "do", &id, stale).await;
    assert_eq!(refused.unwrap_err().code, "stale_snapshot");
    let (fresh, _) = super::run_as(&browsers, OWNER_AGENT.into(), "observe", &id, json!({})).await;
    assert!(fresh.is_ok());

    // Not handed back in time: the agent gets the session back all the same.
    let started = Instant::now();
    let (late, _) = super::run_as(
        &browsers,
        OWNER_AGENT.into(),
        "handoff",
        &id,
        json!({"reason": "2fa", "within": 1}),
    )
    .await;
    let late = late.unwrap_err();
    assert_eq!(late.code, "timeout");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(handoff::pending(&state).is_empty());
    let (after, _) = super::run_as(&browsers, OWNER_AGENT.into(), "observe", &id, json!({})).await;
    assert!(after.is_ok());
    browsers.close_all().await;
}

async fn a_host_with_no_screen_answers_needs_owner_at_once(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["default"]);
    let mut host = Host::new(&state);
    host.browsers.screen = Some(false);
    let (id, _) = host
        .open(OWNER_AGENT, "default", json!({"url": local_page()}))
        .await;
    let started = Instant::now();
    let failure = host
        .call(OWNER_AGENT, "handoff", &id, json!({"reason": "captcha"}))
        .await
        .unwrap_err();
    println!(
        "no screen: {} {} {}",
        failure.code, failure.message, failure.details
    );
    assert_eq!(
        (failure.code.as_str(), &failure.details["reason"]),
        ("needs_owner", &json!("no_screen"))
    );
    assert!(started.elapsed() < Duration::from_secs(2), "at once");
    assert!(handoff::pending(&state).is_empty());
    // The session was never paused.
    assert!(
        host.call(OWNER_AGENT, "observe", &id, json!({}))
            .await
            .is_ok()
    );
    let bad = host
        .call(OWNER_AGENT, "handoff", &id, json!({"reason": "lunch"}))
        .await
        .unwrap_err();
    assert_eq!(bad.code, "bad_arguments");
    host.browsers.close_all().await;
}

/// A page whose text says "ready" a little after it loads, beside a field.
fn slow_page() -> String {
    let page = b"<!doctype html><title>latch slow page</title>\
        <p id=t>loading</p><p><label>Note <input id=note></label></p>\
        <script>setTimeout(() => { t.textContent = 'ready'; }, 2500)</script>";
    format!(
        "http://127.0.0.1:{}/",
        super::serve(vec![("/".into(), page.to_vec())])
    )
}

async fn a_plan_running_when_the_handoff_starts_sends_nothing_more(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["default"]);
    let browsers = browsers(&state, true);
    let (opened, _) = super::run_as(
        &browsers,
        OWNER_AGENT.into(),
        "open",
        "default",
        json!({"url": slow_page()}),
    )
    .await;
    let opened = opened.unwrap();
    let id = opened["session"].as_str().unwrap().to_string();
    let note = super::find(&opened, "textbox", "Note");
    // Step 1 waits for "ready"; step 2 types, after the handoff has begun.
    let plan = json!({"request": "r-handoff-plan", "via": "do", "plan": {"steps": [
        {"literal": {"op": "scroll", "text": "down"}, "check": {"kind": "text", "value": "ready"}},
        {"literal": {"op": "type", "ref": note, "text": "after the handoff"}},
    ]}});
    let running = {
        let (browsers, id) = (browsers.clone(), id.clone());
        tokio::spawn(
            async move { super::run_as(&browsers, OWNER_AGENT.into(), "do", &id, plan).await },
        )
    };
    tokio::time::sleep(Duration::from_millis(700)).await;
    let waiting = {
        let (browsers, id) = (browsers.clone(), id.clone());
        tokio::spawn(async move {
            super::run_as(
                &browsers,
                OWNER_AGENT.into(),
                "handoff",
                &id,
                json!({"reason": "captcha", "within": 60}),
            )
            .await
        })
    };
    let (planned, _) = running.await.unwrap();
    let stopped = planned.unwrap_err();
    println!("plan: {} {}", stopped.code, stopped.message);
    assert_eq!(stopped.code, "session_paused");
    let started = Instant::now();
    while handoff::pending(&state).is_empty() && started.elapsed() < Duration::from_secs(20) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    handoff::done(&state, None).unwrap();
    let (handed, _) = waiting.await.unwrap();
    let field = super::find(&handed.unwrap()["page"], "textbox", "Note");
    let (seen, _) = super::run_as(&browsers, OWNER_AGENT.into(), "observe", &id, json!({})).await;
    let seen = seen.unwrap();
    let note = seen["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["ref"] == json!(field))
        .unwrap()
        .clone();
    assert!(
        note["value"].is_null(),
        "nothing was typed during the handoff: {note}"
    );
    browsers.close_all().await;
}

async fn refs_are_forgotten_even_while_another_session_holds_the_browser(engine: &str) {
    let (_dir, state, _area) = test_state(engine, &["default"]);
    let browsers = browsers(&state, true);
    let page = slow_page();
    let open = || async {
        let (opened, _) = super::run_as(
            &browsers,
            OWNER_AGENT.into(),
            "open",
            "default",
            json!({"url": page}),
        )
        .await;
        let opened = opened.unwrap();
        (opened["session"].as_str().unwrap().to_string(), opened)
    };
    let (id, opened) = open().await;
    let old = opened["snapshot"].as_str().unwrap().to_string();
    let (other, _) = open().await;
    let waiting = {
        let (browsers, id) = (browsers.clone(), id.clone());
        tokio::spawn(async move {
            super::run_as(
                &browsers,
                OWNER_AGENT.into(),
                "handoff",
                &id,
                json!({"reason": "2fa", "within": 60}),
            )
            .await
        })
    };
    let started = Instant::now();
    while handoff::pending(&state).is_empty() && started.elapsed() < Duration::from_secs(20) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Another session's fill now holds the browser (it stays prepared).
    let held = browsers.session_for_test(&other).await;
    let mut audit = serde_json::Map::new();
    let _ = crate::browser::steps::call(
        &held,
        json!({"op": "prepare_fill", "mode": "login"}),
        &mut audit,
    )
    .await;
    handoff::done(&state, None).unwrap();
    let (handed, _) = waiting.await.unwrap();
    println!(
        "handed back while held: {:?}",
        handed
            .as_ref()
            .map(|_| "ok")
            .map_err(|f| (&f.code, &f.message))
    );
    // Old refs never work again, whatever the answer was.
    let mut stale = act("scroll e1 down");
    stale["plan"]["steps"][0]["literal"]["snapshot"] = json!(old);
    let _ = crate::browser::steps::call(&held, json!({"op": "fill_resume"}), &mut audit).await;
    let (refused, _) = super::run_as(&browsers, OWNER_AGENT.into(), "do", &id, stale).await;
    let refused = refused.unwrap_err();
    assert!(
        handed.is_ok(),
        "retiring refs is let by another session's pause"
    );
    assert_eq!(
        refused.code, "stale_snapshot",
        "an old ref after the handoff: {}",
        refused.message
    );
    browsers.close_all().await;
}
