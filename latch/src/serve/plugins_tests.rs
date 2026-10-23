//! A host's connectors, turned on and off by its owner, end to end: the
//! same headless host as `gated_tests`, with real workers, callers over
//! iroh and the owner over the host's private socket. Every test host
//! starts with Google enabled, as its owner would have left it.

use std::time::Duration;

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
    // Messages too, before its reader is asked to plan the call.
    #[cfg(target_os = "macos")]
    {
        ok(plugins(&t, "disable", "messages").await);
        let read = json!({"action": "search", "args": ["--since", "1h"]});
        let (answer, _) = t.call(&agent, "messages", read).await;
        assert_eq!(refusal(&answer).1, "not_enabled");
    }

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
    for record in &google {
        assert_eq!(record["decision"], "deny");
        assert_eq!(record["outcome"], "not_enabled");
    }
    // Each keeps its command's grammar words, never the query.
    let commands: Vec<String> = google
        .into_iter()
        .map(|record| {
            let record: crate::audit::Record = serde_json::from_value(record).unwrap();
            dashboard::recorded_command(&record).unwrap()
        })
        .collect();
    assert_eq!(
        commands,
        ["latch google gmail search", "latch google accounts"]
    );
    let shown = state_of(&t, "google").await;
    assert_eq!(shown["state"], "disabled");
    assert_eq!(shown["enable"], "latch plugins enable google");
}

/// Where this host's pinned gog goes.
fn gog_folder(t: &TestHost) -> PathBuf {
    t.state
        .join("plugins/gog")
        .join(&crate::google::PIN.version)
}

#[tokio::test]
async fn google_setup_refuses_a_missing_or_unverified_gog_and_stays_off() {
    let t = start(covered()).await;
    ok(plugins(&t, "disable", "google").await);

    let answer = plugins(&t, "enable", "google").await;
    assert_eq!(refusal(&answer).1, "gog_not_configured");
    let shown = state_of(&t, "google").await;
    assert_eq!(shown["state"], "needs_setup");
    assert!(shown["reason"].as_str().unwrap().contains("not installed"));

    // Not the pinned release: the check every Google worker makes refuses it.
    std::fs::create_dir_all(gog_folder(&t)).unwrap();
    std::fs::write(gog_folder(&t).join("gog"), "a stand-in gog").unwrap();
    let answer = plugins(&t, "enable", "google").await;
    assert_eq!(refusal(&answer).1, "gog_unverified");
    assert_eq!(state_of(&t, "google").await["state"], "needs_setup");
    assert_eq!(
        refusal(&t.local_call("google", accounts()).await).1,
        "not_enabled"
    );
    assert_eq!(t.plow.saw("access-token"), 0, "no credential asked for");
}

/// Sends one operation over the host's socket in the background.
fn pending(t: &Arc<TestHost>, op: &'static str, args: Value) -> tokio::task::JoinHandle<Response> {
    let t = t.clone();
    tokio::spawn(async move { t.local_call(op, args).await })
}

/// Until Plow has been asked for a Google credential `times` times.
async fn asked_for_google(t: &TestHost, times: usize) {
    while t.plow.saw("access-token") < times {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn disable_refuses_new_calls_at_once_and_returns_after_the_running_one() {
    let t = Arc::new(start(covered()).await);
    t.plow.world.lock().unwrap().google_paused = true;
    let running = pending(&t, "google", accounts());
    asked_for_google(&t, 1).await;
    let disabling = pending(
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

    t.plow.world.lock().unwrap().google_paused = false;
    assert_eq!(
        ok(running.await.unwrap())["accounts"],
        json!(["me@example.com"])
    );
    assert_eq!(ok(disabling.await.unwrap())["waited_for"], 1);
    let records = t.audit();
    let at = |op: &str, outcome: &str| {
        records
            .iter()
            .position(|r| r["op"] == op && r["outcome"] == outcome)
            .unwrap()
    };
    assert!(
        at("google", "ok") < at("plugins", "ok"),
        "disable ended after the call"
    );
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
async fn messages_and_contacts_are_enabled_only_once_the_host_can_open_their_stores() {
    use std::os::unix::fs::PermissionsExt;
    let t = start(covered()).await;
    let home = t.state.parent().unwrap().join("home");
    let mode = |path: &Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
    };
    let enable = |name: &'static str| plugins(&t, "enable", name);
    let access = |answer: &Response| {
        let (_, code, details) = refusal(answer);
        assert_eq!(code, "needs_setup");
        details["access"].as_str().unwrap().to_string()
    };

    // No folder, then a folder with no database: nothing to read.
    assert_eq!(access(&enable("messages").await), "unavailable");
    let messages = home.join("Library/Messages");
    std::fs::create_dir_all(&messages).unwrap();
    assert_eq!(access(&enable("messages").await), "unavailable");

    // A folder the host may not list, as macOS refuses a host without access.
    std::fs::write(messages.join("chat.db"), "a synthetic store").unwrap();
    mode(&messages, 0o000);
    let answer = enable("messages").await;
    mode(&messages, 0o700);
    assert_eq!(access(&answer), "not_granted");
    let details = refusal(&answer).2;
    assert_eq!(details["grant"], json!(t.host.workers.program));
    assert_eq!(details["settings"], crate::messages::access::SETTINGS);

    // A listable folder whose database the host may not open.
    mode(&messages.join("chat.db"), 0o000);
    let answer = enable("messages").await;
    mode(&messages.join("chat.db"), 0o600);
    assert_eq!(access(&answer), "not_granted");
    assert_eq!(state_of(&t, "messages").await["state"], "needs_setup");
    assert_eq!(
        refusal(&t.local_call("messages", json!({})).await).1,
        "not_enabled"
    );

    let enabled = ok(enable("messages").await);
    assert_eq!(enabled["access"], "granted");
    assert_eq!(state_of(&t, "messages").await["state"], "enabled");

    // Contacts, checked on its own: every store in Sources must open.
    let book = home.join("Library/Application Support/AddressBook");
    let source = book.join("Sources/A1");
    std::fs::create_dir_all(&source).unwrap();
    assert_eq!(access(&enable("contacts").await), "unavailable");
    std::fs::write(source.join("AddressBook-v22.abcddb"), "a synthetic store").unwrap();
    std::fs::write(book.join("AddressBook-v22.abcddb"), "a synthetic store").unwrap();
    // A file in Sources holds no store, as the reader sees it.
    std::fs::write(book.join("Sources/.DS_Store"), "not a source").unwrap();
    mode(&source.join("AddressBook-v22.abcddb"), 0o000);
    let answer = enable("contacts").await;
    mode(&source.join("AddressBook-v22.abcddb"), 0o600);
    assert_eq!(access(&answer), "not_granted");
    assert_eq!(ok(enable("contacts").await)["access"], "granted");
}

/// Names come from the address book, which is Contacts': a Messages call
/// shows them only while Contacts is enabled too.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn messages_show_contact_names_only_while_contacts_is_enabled() {
    let t = start(covered()).await;
    t.with_messages();
    let search = json!({"action": "search", "args": ["--", "code"]});
    let named = ok(t.local_call("messages", search.clone()).await);
    assert_eq!(
        named["messages"][0]["from"]["name"], "Ann Example",
        "{named}"
    );

    ok(plugins(&t, "disable", "contacts").await);
    let unnamed = ok(t.local_call("messages", search).await);
    assert_eq!(unnamed["count"], 1, "{unnamed}");
    assert_eq!(unnamed["names_unavailable"], "contacts_not_enabled");
    assert!(!unnamed.to_string().contains("Ann Example"), "{unnamed}");
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

/// Google with the pinned gog `install-binary.sh` downloads (`just test
/// browser` puts it in `.dev/cft-state`).
#[cfg(target_os = "macos")]
mod pinned {
    use super::*;

    /// Installs the pinned gog `just test browser` puts in `.dev/cft-state`,
    /// and returns where.
    fn install_pinned_gog(t: &TestHost) -> PathBuf {
        let pinned = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../.dev/cft-state/plugins/gog")
            .join(&crate::google::PIN.version)
            .join("gog");
        let gog = gog_folder(t).join("gog");
        std::fs::create_dir_all(gog_folder(t)).unwrap();
        std::fs::copy(&pinned, &gog).expect("the pinned gog; run `just test browser`");
        gog
    }

    #[tokio::test]
    async fn google_is_enabled_only_once_its_setup_succeeds_and_stays_so_across_a_restart() {
        use std::os::unix::fs::PermissionsExt;
        let t = start(World {
            decide: Some(always_covered),
            ..World::default()
        })
        .await;
        ok(plugins(&t, "disable", "google").await);
        let gog = install_pinned_gog(&t);

        std::fs::set_permissions(&gog, std::fs::Permissions::from_mode(0o644)).unwrap();
        let answer = plugins(&t, "enable", "google").await;
        assert_eq!(refusal(&answer).1, "gog_not_executable");
        std::fs::set_permissions(&gog, std::fs::Permissions::from_mode(0o755)).unwrap();

        // gog, but no Google account connected in Plow: where to connect one.
        let answer = plugins(&t, "enable", "google").await;
        let (_, code, details) = refusal(&answer);
        assert_eq!(code, "google_not_connected");
        assert_eq!(
            details["connect_url"],
            "https://app.example.com/connections"
        );
        let Response::Error { message, .. } = &answer else {
            unreachable!()
        };
        assert!(
            message.contains("https://app.example.com/connections"),
            "{message}"
        );
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

    /// A Google setup still waiting on Plow when the owner disables Google
    /// leaves it disabled.
    #[tokio::test]
    async fn a_setup_overtaken_by_disable_changes_nothing() {
        let t = Arc::new(start(covered()).await);
        ok(plugins(&t, "disable", "google").await);
        install_pinned_gog(&t);
        t.plow.world.lock().unwrap().google_paused = true;
        let enabling = pending(
            &t,
            "plugins",
            json!({"command": "enable", "name": "google"}),
        );
        asked_for_google(&t, 1).await;
        ok(plugins(&t, "disable", "google").await);

        t.plow.world.lock().unwrap().google_paused = false;
        assert_eq!(refusal(&enabling.await.unwrap()).1, "setup_superseded");
        assert_eq!(state_of(&t, "google").await["state"], "disabled");
        assert_eq!(
            refusal(&t.local_call("google", accounts()).await).1,
            "not_enabled"
        );
    }

    /// Google wanting the owner to sign in again is said as
    /// `google_needs_sign_in`, naming the accounts: for an enable when no
    /// account works, and for an agent's call either way. When some still
    /// work, the enable succeeds and names the rest in `needs_sign_in`.
    #[tokio::test]
    async fn an_account_google_wants_signed_in_again_is_named() {
        let t = start(covered()).await;
        ok(plugins(&t, "disable", "google").await);
        install_pinned_gog(&t);
        {
            let mut world = t.plow.world.lock().unwrap();
            world.google_accounts = vec!["me@example.com".into()];
            world.google_reauth = vec!["me@example.com".into()];
        }
        let answer = plugins(&t, "enable", "google").await;
        let (exit, code, details) = refusal(&answer);
        assert_eq!((exit, code), (Exit::Failed, "google_needs_sign_in"));
        assert_eq!(details["accounts"], json!(["me@example.com"]));
        assert_eq!(state_of(&t, "google").await["state"], "needs_setup");

        {
            let mut world = t.plow.world.lock().unwrap();
            world.google_accounts = vec!["me@example.com".into(), "work@example.com".into()];
            world.google_reauth = vec!["work@example.com".into()];
        }
        let enabled = ok(plugins(&t, "enable", "google").await);
        assert_eq!(enabled["accounts"], json!(["me@example.com"]));
        assert_eq!(enabled["needs_sign_in"], json!(["work@example.com"]));
        let on_work = json!({"gog_args": ["gmail", "search", "invoice"],
                             "account": ["work@example.com"], "confirm_conflict": false,
                             "inputs": []});
        // An agent's call on that account: ready, then the refusal.
        let agent = t.caller("agent").await;
        let (ready, rest) = t.call(&agent, "google", on_work).await;
        assert_eq!(ok(ready), json!({"ready": true}));
        let size = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
        let refused: Response = serde_json::from_slice(&rest[4..4 + size]).unwrap();
        let (_, code, details) = refusal(&refused);
        assert_eq!(code, "google_needs_sign_in", "{details}");
        assert_eq!(details["accounts"], json!(["work@example.com"]));

        t.plow.world.lock().unwrap().google_reauth =
            vec!["me@example.com".into(), "work@example.com".into()];
        let (refused, _) = t.call(&agent, "google", accounts()).await;
        assert_eq!(refusal(&refused).1, "google_needs_sign_in");
    }
}
