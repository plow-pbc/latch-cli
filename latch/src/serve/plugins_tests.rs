//! A host's connectors, turned on and off by its owner, end to end: the
//! same headless host as `gated_tests`, with real workers, callers over
//! iroh and the owner over the host's private socket. Every test host
//! starts with Google enabled, as its owner would have left it.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::gated_tests::{TestHost, always_covered, refusal, start, start_with};
use super::*;
use crate::failure::Exit;
use crate::gate::fake::{FakePlow, World};
use crate::model;

fn covered() -> World {
    World {
        decide: Some(always_covered),
        google_accounts: vec!["me@example.com".into()],
        ..World::default()
    }
}

/// Another host on the same synthetic Plow.
async fn another_on(plow: &FakePlow) -> TestHost {
    let provider = model::Plow::new(&plow.url, "made-up-login".into()).with_model(gate::MODEL);
    start_with(plow.clone(), Arc::new(provider), DenyExec::default()).await
}

fn accounts() -> Value {
    json!({"gog_args": ["accounts"], "account": [], "confirm_conflict": false, "inputs": []})
}

fn search() -> Value {
    json!({"gog_args": ["gmail", "search", "invoice"], "account": [],
           "confirm_conflict": false, "inputs": []})
}

fn ok(response: Response) -> Value {
    match response {
        Response::Ok(value) => value,
        Response::Error { code, message, .. } => panic!("{code}: {message}"),
    }
}

async fn plugins(t: &TestHost, command: &str, name: &str) -> Response {
    t.local_call("plugins", json!({"command": command, "name": name}))
        .await
}

async fn list(t: &TestHost) -> Vec<Value> {
    let listed = ok(t.local_call("plugins", json!({"command": "list"})).await);
    listed["plugins"].as_array().unwrap().clone()
}

async fn state_of(t: &TestHost, name: &str) -> Value {
    list(t)
        .await
        .into_iter()
        .find(|plugin| plugin["name"] == name)
        .unwrap()
}

#[tokio::test]
async fn a_disabled_connector_is_refused_before_it_is_judged_or_given_a_credential() {
    let t = start(covered()).await;
    let names: Vec<Value> = list(&t).await.iter().map(|p| p["name"].clone()).collect();
    #[cfg(target_os = "macos")]
    assert_eq!(
        names,
        json!(["google", "messages", "contacts"])
            .as_array()
            .unwrap()
            .clone()
    );
    #[cfg(not(target_os = "macos"))]
    assert_eq!(names, vec![json!("google")]);

    let disabled = ok(plugins(&t, "disable", "google").await);
    assert_eq!(disabled["waited_for"], 0);
    let agent = t.caller("agent").await;
    let (answer, rest) = t.call(&agent, "google", search()).await;
    let (exit, code, details) = refusal(&answer);
    assert_eq!((exit, code), (Exit::Denied, "not_enabled"));
    assert_eq!(details["enable"], "latch plugins enable google");
    assert!(rest.is_empty());
    let owners = t.local_call("google", accounts()).await;
    assert_eq!(refusal(&owners).1, "not_enabled");

    assert_eq!(t.judged(), 0, "nothing was judged");
    assert_eq!(
        t.plow.saw("access-token"),
        0,
        "no Google credential fetched"
    );
    let google: Vec<Value> = t
        .audit()
        .into_iter()
        .filter(|r| r["op"] == "google")
        .collect();
    assert_eq!(google.len(), 2);
    for record in google {
        assert_eq!(record["decision"], "deny");
        assert_eq!(record["outcome"], "not_enabled");
    }
    let shown = state_of(&t, "google").await;
    assert_eq!(shown["state"], "disabled");
    assert_eq!(shown["enable"], "latch plugins enable google");
}

#[tokio::test]
async fn google_is_enabled_only_once_its_setup_succeeds_and_stays_so_across_a_restart() {
    let t = start(World {
        decide: Some(always_covered),
        ..World::default()
    })
    .await;
    ok(plugins(&t, "disable", "google").await);

    // No gog: needs setup, and still refused.
    let answer = plugins(&t, "enable", "google").await;
    assert_eq!(refusal(&answer).1, "gog_not_configured");
    let shown = state_of(&t, "google").await;
    assert_eq!(shown["state"], "needs_setup");
    assert!(shown["reason"].as_str().unwrap().contains("not installed"));
    assert_eq!(
        refusal(&t.local_call("google", accounts()).await).1,
        "not_enabled"
    );

    // gog, but no Google account connected in Plow.
    let gog = t
        .state
        .join("plugins/gog")
        .join(&crate::google::PIN.version);
    std::fs::create_dir_all(&gog).unwrap();
    std::fs::write(gog.join("gog"), "a stand-in gog").unwrap();
    let answer = plugins(&t, "enable", "google").await;
    assert_eq!(refusal(&answer).1, "google_not_connected");
    assert_eq!(state_of(&t, "google").await["state"], "needs_setup");

    t.plow.world.lock().unwrap().google_accounts = vec!["me@example.com".into()];
    let enabled = ok(plugins(&t, "enable", "google").await);
    assert_eq!(enabled["state"], "enabled");
    assert_eq!(enabled["accounts"], json!(["me@example.com"]));
    let listed = ok(t.local_call("google", accounts()).await);
    assert_eq!(listed["accounts"], json!(["me@example.com"]));

    // A host started again on this state folder keeps it enabled.
    let state = crate::state::StateDir {
        path: t.state.clone(),
    };
    let again = crate::plugins::Plugins::load(&state, Path::new("/nonexistent")).unwrap();
    assert!(again.start(crate::plugins::Connector::Google).is_ok());
}

#[tokio::test]
async fn disable_refuses_new_calls_at_once_and_returns_after_the_running_one() {
    let t = start(World {
        google_delay: Duration::from_secs(2),
        ..covered()
    })
    .await;
    let t = Arc::new(t);
    let finished = |t: &Arc<TestHost>, op: &'static str, args: Value| {
        let t = t.clone();
        tokio::spawn(async move {
            let response = t.local_call(op, args).await;
            (response, Instant::now())
        })
    };
    let running = finished(&t, "google", accounts());
    // Plow is now handing that call its credential.
    while t.plow.saw("access-token") == 0 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let disabling = finished(
        &t,
        "plugins",
        json!({"command": "disable", "name": "google"}),
    );
    while std::fs::read_to_string(t.state.join("plugins.json"))
        .unwrap()
        .contains("google")
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let refused = t.local_call("google", accounts()).await;
    assert_eq!(refusal(&refused).1, "not_enabled");
    assert!(
        !disabling.is_finished(),
        "disable waits for the running call"
    );

    let (answer, ran_until) = running.await.unwrap();
    assert_eq!(ok(answer)["accounts"], json!(["me@example.com"]));
    let (disabled, disabled_at) = disabling.await.unwrap();
    assert_eq!(ok(disabled)["waited_for"], 1);
    assert!(disabled_at >= ran_until);
    assert_eq!(
        t.plow.saw("access-token"),
        1,
        "the refused call got no credential"
    );
}

#[tokio::test]
async fn disabling_google_on_one_host_leaves_another_working_and_asks_plow_nothing() {
    let this = start(covered()).await;
    let other = another_on(&this.plow).await;
    let before = this.plow.seen.lock().unwrap().len();
    ok(plugins(&this, "disable", "google").await);
    assert_eq!(
        this.plow.seen.lock().unwrap().len(),
        before,
        "disabling asks Plow nothing, so it revokes nothing"
    );
    assert_eq!(
        refusal(&this.local_call("google", accounts()).await).1,
        "not_enabled"
    );
    let listed = ok(other.local_call("google", accounts()).await);
    assert_eq!(listed["accounts"], json!(["me@example.com"]));
}

#[tokio::test]
async fn only_the_owner_at_the_host_turns_a_plugin_on_or_off() {
    let t = start(covered()).await;
    let agent = t.caller("agent").await;
    let device = t.caller("device").await;
    for caller in [&agent, &device] {
        for command in ["enable", "disable"] {
            let (answer, _) = t
                .call(
                    caller,
                    "plugins",
                    json!({"command": command, "name": "google"}),
                )
                .await;
            assert_eq!(refusal(&answer).0, Exit::Denied);
            assert_eq!(refusal(&answer).1, "owner_only");
        }
    }
    let (listed, _) = t.call(&agent, "plugins", json!({"command": "list"})).await;
    assert_eq!(ok(listed)["plugins"][0]["state"], "enabled");
    assert_eq!(t.judged(), 0, "plugins commands are never judged");
    assert_eq!(state_of(&t, "google").await["state"], "enabled");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn messages_and_contacts_are_enabled_only_with_the_hosts_full_disk_access() {
    use std::os::unix::fs::PermissionsExt;
    let t = start(covered()).await;
    let home = t.state.parent().unwrap().join("home");

    let answer = plugins(&t, "enable", "messages").await;
    let (_, code, details) = refusal(&answer);
    assert_eq!(code, "needs_setup");
    assert_eq!(details["access"], "unavailable");

    // Unreadable, as a folder macOS protects is to a host without access.
    let messages = home.join("Library/Messages");
    std::fs::create_dir_all(&messages).unwrap();
    std::fs::set_permissions(&messages, std::fs::Permissions::from_mode(0o000)).unwrap();
    let answer = plugins(&t, "enable", "messages").await;
    std::fs::set_permissions(&messages, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_, code, details) = refusal(&answer);
    assert_eq!(code, "needs_setup");
    assert_eq!(details["access"], "not_granted");
    assert_eq!(details["grant"], json!(t.host.workers.program));
    assert_eq!(details["settings"], crate::messages::access::SETTINGS);
    let shown = state_of(&t, "messages").await;
    assert_eq!(shown["state"], "needs_setup");
    assert_eq!(
        refusal(&t.local_call("messages", json!({})).await).1,
        "not_enabled"
    );

    let enabled = ok(plugins(&t, "enable", "messages").await);
    assert_eq!(enabled["access"], "granted");
    assert_eq!(state_of(&t, "messages").await["state"], "enabled");
    // Each connector is checked on its own.
    assert_eq!(state_of(&t, "contacts").await["state"], "disabled");
    let answer = plugins(&t, "enable", "contacts").await;
    assert_eq!(refusal(&answer).2["access"], "unavailable");
}

#[cfg(target_os = "macos")]
#[test]
fn full_disk_access_is_granted_to_the_app_the_latch_is_inside() {
    let inside = Path::new("/Applications/Plow Latch.app/Contents/MacOS/latch");
    assert_eq!(
        crate::plugins::host::grantee(inside),
        Path::new("/Applications/Plow Latch.app")
    );
    let bare = Path::new("/usr/local/bin/latch");
    assert_eq!(crate::plugins::host::grantee(bare), bare);
}
