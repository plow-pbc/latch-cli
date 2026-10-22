//! `rules_try` on a headless host: the owner previews a verdict, as an
//! agent or as themselves, and nothing the request asks for runs.

use serde_json::{Value, json};

use super::gated_tests::{always_covered, refusal, start, start_denying};
use super::*;
use crate::failure::Exit;
use crate::gate::fake::{AGENT, LINE, World, agent_self, message, owner};

/// The judge allows only what the owner said yes to in the chat.
fn covered_after_a_yes(state: &Value) -> Option<[f64; 3]> {
    let said_yes = state["conversation"].as_array().is_some_and(|messages| {
        messages
            .iter()
            .any(|m| m["from"] == "OWNER" && m["text"].as_str().unwrap().contains("yes"))
    });
    Some(if said_yes {
        [0.9, 0.08, 0.02]
    } else {
        [0.1, 0.85, 0.05]
    })
}

fn chat_with(decide: crate::gate::fake::Decide) -> World {
    let mut world = World::with_chat(
        "chat_a",
        vec![
            message("m1", owner(), "please build the site", 30),
            message("m2", agent_self(), "On it.", 29),
        ],
    );
    world.decide = Some(decide);
    world
}

/// A preview of `op` with `args`, as `from` if given.
fn try_args(op: &str, args: Value, from: Option<&str>, chat: Option<&str>) -> Value {
    let mut tried = json!({"request": {"op": op, "args": args, "chat": chat}});
    if let Some(from) = from {
        tried["from"] = json!(from);
    }
    tried
}

fn answer(response: Response) -> Value {
    match response {
        Response::Ok(answer) => answer,
        refused => panic!("not previewed: {refused:?}"),
    }
}

#[tokio::test]
async fn an_exec_preview_is_judged_and_runs_nothing() {
    let t = start(chat_with(always_covered)).await;
    let device = t.caller("device").await;
    let fresh = t.host.workers.home.join("fresh");
    let exec = json!({"argv": ["/bin/sh", "-c", "touch ran"], "writes": ["~/fresh"],
                      "network": true, "secret_env": {"API_TOKEN": "s3cret-value"}});
    let (response, rest) = t
        .call(&device, "rules_try", try_args("exec", exec, None, None))
        .await;
    let tried = answer(response);
    assert_eq!(tried["verdict"], "allow");
    assert_eq!(tried["as"], "owner");
    assert_eq!(tried["host"], "test-host");
    assert_eq!(tried["gate"]["caller"], "owner_device");
    assert_eq!(tried["gate"]["verdict"], "allow");
    assert!(tried["gate"]["p"].is_object(), "{tried}");
    let action = &tried["request"]["action"];
    assert_eq!(action["writes"], json!([fresh]));
    assert_eq!(action["network"], true);
    assert_eq!(action["secret_env"][0]["name"], "API_TOKEN");
    assert!(!tried.to_string().contains("s3cret-value"), "{tried}");
    assert!(rest.is_empty());
    assert_eq!(t.judged(), 1);
    // Nothing ran: no write folder made, no file written anywhere.
    assert!(!fresh.exists());
    assert!(!t.host.workers.home.join("ran").exists());

    // Its record is a preview's, and no exec record exists.
    let records = t.audit();
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert_eq!(record["op"], "rules_try");
    assert_eq!(record["decision"], "allow");
    assert_eq!(record["outcome"], "allow");
    assert_eq!(record["args"]["op"], "exec");
    assert_eq!(record["args"]["action"]["network"], true);
    assert_eq!(record["gate"]["verdict"], "allow");
    assert!(
        !serde_json::to_string(&records)
            .unwrap()
            .contains("s3cret-value")
    );
}

#[tokio::test]
async fn an_agent_cannot_preview() {
    let t = start(chat_with(always_covered)).await;
    let agent = t.caller("agent").await;
    let (response, _) = t
        .call(
            &agent,
            "rules_try",
            try_args("exec", json!({"argv": ["true"]}), None, None),
        )
        .await;
    let (exit, code, _) = refusal(&response);
    assert_eq!((exit, code), (Exit::Denied, "owner_only"));
    assert_eq!(t.judged(), 0);
    assert_eq!(t.audit()[0]["decision"], "deny");
}

#[tokio::test]
async fn from_judges_as_that_agent_with_its_rules_and_chat() {
    let t = start(chat_with(covered_after_a_yes)).await;
    let device = t.caller("device").await;
    for set in [
        json!({"command": "set", "text": "Never send email."}),
        json!({"command": "set", "from": "Juno", "text": "Builds are fine."}),
    ] {
        answer(t.call(&device, "rules", set).await.0);
    }
    let exec = json!({"argv": ["npm", "run", "build"]});
    let (response, _) = t
        .call(
            &device,
            "rules_try",
            try_args("exec", exec, Some("Juno"), None),
        )
        .await;
    let tried = answer(response);
    // A refusal is the preview's verdict, not an error.
    assert_eq!(tried["verdict"], "not_covered");
    assert_eq!(
        tried["as"],
        json!({"agent_uid": AGENT, "agent_name": "Juno"})
    );
    assert_eq!(tried["gate"]["caller"], "agent");
    assert_eq!(tried["gate"]["chat"], "chat_a");
    assert_eq!(tried["gate"]["source"], "chat_and_rules");
    let asked = t.plow.world.lock().unwrap().asked.last().unwrap().clone();
    assert_eq!(
        asked["owner_rules"],
        json!({"rules for every agent on this host": "Never send email.",
               "rules for this agent": "Builds are fine."})
    );
    assert_eq!(asked["conversation"][0]["text"], "please build the site");
    let record = t.audit().into_iter().last().unwrap();
    assert_eq!(
        (&record["op"], &record["outcome"], &record["args"]["from"]),
        (&json!("rules_try"), &json!("not_covered"), &json!(AGENT))
    );

    // The same request from the owner: shared rules only, no chat.
    let (response, _) = t
        .call(
            &device,
            "rules_try",
            try_args("exec", json!({"argv": ["npm", "run", "build"]}), None, None),
        )
        .await;
    assert_eq!(answer(response)["gate"]["source"], "owner_request");
    let asked = t.plow.world.lock().unwrap().asked.last().unwrap().clone();
    assert_eq!(
        asked["owner_rules"],
        json!({"rules on this host": "Never send email."})
    );

    let (response, _) = t
        .call(
            &device,
            "rules_try",
            try_args("exec", json!({"argv": ["true"]}), Some("Nobody"), None),
        )
        .await;
    assert_eq!(refusal(&response).1, "no_such_agent");
}

#[tokio::test]
async fn browser_steps_and_settings_are_not_previewed() {
    let t = start(chat_with(always_covered)).await;
    let device = t.caller("device").await;
    for (op, args) in [
        (
            "browser",
            json!({"command": "open", "url": "https://example.com"}),
        ),
        ("vault", json!({"command": "list"})),
    ] {
        let (response, _) = t
            .call(&device, "rules_try", try_args(op, args, None, None))
            .await;
        let (exit, code, _) = refusal(&response);
        assert_eq!((exit, code), (Exit::Usage, "not_previewable"), "{op}");
    }
    assert_eq!(t.judged(), 0);
}

#[tokio::test]
async fn the_hosts_own_refusals_are_previewed_without_the_judge() {
    let exec = || json!({"argv": ["/bin/sh", "-c", "touch ran"]});
    for flags in [crate::cli::EVERY_AGENT.to_string(), AGENT.to_string()] {
        let deny = DenyExec::from_flags(std::slice::from_ref(&flags));
        let t = start_denying(chat_with(always_covered), deny).await;
        let device = t.caller("device").await;
        let (response, _) = t
            .call(
                &device,
                "rules_try",
                try_args("exec", exec(), Some("Juno"), None),
            )
            .await;
        let tried = answer(response);
        assert_eq!(tried["verdict"], "host_refused", "{flags}");
        assert_eq!(tried["refused"]["code"], "exec_denied");
        assert!(tried.get("gate").is_none());
        assert_eq!(t.judged(), 0, "{flags}");
        assert!(!t.host.workers.home.join("ran").exists());
        assert_eq!(t.audit()[0]["outcome"], "host_refused");
        // The owner is never refused exec.
        let (response, _) = t
            .call(&device, "rules_try", try_args("exec", exec(), None, None))
            .await;
        assert_eq!(answer(response)["verdict"], "allow");
    }

    let t = start(chat_with(always_covered)).await;
    let device = t.caller("device").await;
    let off = t
        .local_call("plugins", json!({"command": "disable", "name": "google"}))
        .await;
    assert!(matches!(off, Response::Ok(_)), "{off:?}");
    let google = json!({"gog_args": ["gmail", "search", "invoice"], "account": [],
                        "confirm_conflict": false, "inputs": []});
    let (response, _) = t
        .call(
            &device,
            "rules_try",
            try_args("google", google, Some("Juno"), None),
        )
        .await;
    let tried = answer(response);
    assert_eq!(tried["verdict"], "host_refused");
    assert_eq!(tried["refused"]["code"], "not_enabled");
    assert_eq!(t.judged(), 0);
    assert_eq!(
        t.plow.saw("access-token"),
        0,
        "no Google credential fetched"
    );
}

#[tokio::test]
async fn an_agent_with_several_chats_needs_one_named() {
    let mut world = chat_with(always_covered);
    world.add(
        "chat_b",
        LINE,
        vec![message("b1", owner(), "yes, build it", 5)],
    );
    let t = start(world).await;
    let device = t.caller("device").await;
    let exec = || json!({"argv": ["npm", "run", "build"]});
    let (response, _) = t
        .call(
            &device,
            "rules_try",
            try_args("exec", exec(), Some("Juno"), None),
        )
        .await;
    let (exit, code, _) = refusal(&response);
    assert_eq!((exit, code), (Exit::Usage, "which_chat"));
    assert_eq!(t.judged(), 0);

    let (response, _) = t
        .call(
            &device,
            "rules_try",
            try_args("exec", exec(), Some("Juno"), Some("chat_b")),
        )
        .await;
    let tried = answer(response);
    assert_eq!(tried["verdict"], "allow");
    assert_eq!(tried["gate"]["chat"], "chat_b");
    let asked = t.plow.world.lock().unwrap().asked.last().unwrap().clone();
    assert_eq!(asked["conversation"][0]["text"], "yes, build it");
}

#[tokio::test]
async fn the_owner_on_the_host_previews_over_the_socket() {
    let t = start(chat_with(always_covered)).await;
    let tried = answer(
        t.local_call(
            "rules_try",
            try_args(
                "file_get",
                json!({"remote_path": "area/notes.txt"}),
                Some("Juno"),
                None,
            ),
        )
        .await,
    );
    assert_eq!(tried["verdict"], "allow");
    assert_eq!(tried["gate"]["caller"], "agent");
    assert!(!tried.to_string().contains("own words"), "no file content");
    assert_eq!(t.audit()[0]["op"], "rules_try");
}
