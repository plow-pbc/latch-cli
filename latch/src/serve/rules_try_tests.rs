//! `rules_try` on a headless host: the owner previews a verdict, as an
//! agent or as themselves, and nothing the request asks for runs.

use serde_json::{Value, json};

use super::gated_tests::{always_covered, covered_after_a_yes, refusal, start, start_denying};
use super::*;
use crate::failure::Exit;
use crate::gate::fake::{AGENT, LINE, World, agent_self, message, owner};

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

/// The audit's operations and the dashboard's call kinds, in order: a
/// preview adds to neither.
fn traces(t: &super::gated_tests::TestHost) -> (Vec<String>, Vec<String>) {
    let ops = t
        .audit()
        .iter()
        .map(|record| record["op"].as_str().unwrap().to_string())
        .collect();
    let watching = t.host.dashboard.watch();
    let board = watching.borrow();
    let kinds = board
        .tasks
        .iter()
        .flat_map(|task| &task.calls)
        .map(|call| call.kind.clone())
        .collect();
    (ops, kinds)
}

fn untouched() -> (Vec<String>, Vec<String>) {
    (Vec::new(), Vec::new())
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

    // Not audited, not on the dashboard.
    assert_eq!(traces(&t), untouched());
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
    assert_eq!(traces(&t), untouched());
}

#[tokio::test]
async fn from_judges_as_that_agent_by_its_rules_alone() {
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
    assert_eq!(tried["gate"]["chat"], Value::Null);
    assert_eq!(tried["gate"]["source"], "rules");
    assert_eq!(t.judged(), 1);
    let asked = t.plow.world.lock().unwrap().asked.last().unwrap().clone();
    assert_eq!(
        asked["owner_rules"],
        json!({"rules for every agent on this host": "Never send email.",
               "rules for this agent": "Builds are fine."})
    );
    assert_eq!(asked["conversation"], "(no chat)", "its chat is not read");
    // Only the two rules commands were audited and shown.
    let rules = vec!["rules".to_string(), "rules".to_string()];
    assert_eq!(traces(&t), (rules.clone(), rules));

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
        assert_eq!(tried["gate"], Value::Null);
        let whom = match flags == crate::cli::EVERY_AGENT {
            true => "You turned exec off for every agent",
            false => "You turned exec off for this agent",
        };
        let message = tried["refused"]["message"].as_str().unwrap();
        assert!(message.starts_with(whom), "{message}");
        assert_eq!(t.judged(), 0, "{flags}");
        assert!(!t.host.workers.home.join("ran").exists());
        assert_eq!(traces(&t), untouched());
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
async fn an_agent_is_previewed_without_any_of_its_chats() {
    let mut world = chat_with(always_covered);
    world.add(
        "chat_b",
        LINE,
        vec![message("b1", owner(), "yes, build it", 5)],
    );
    let t = start(world).await;
    let device = t.caller("device").await;
    let exec = || json!({"argv": ["npm", "run", "build"]});
    // Several chats, none named: no chat is needed, so none is chosen.
    let (response, _) = t
        .call(
            &device,
            "rules_try",
            try_args("exec", exec(), Some("Juno"), None),
        )
        .await;
    let tried = answer(response);
    assert_eq!(tried["verdict"], "allow");
    assert_eq!(tried["gate"]["chat"], Value::Null);
    let asked = t.plow.world.lock().unwrap().asked.last().unwrap().clone();
    assert_eq!(asked["conversation"], "(no chat)");
    assert_eq!(t.judged(), 1);

    // Naming one is refused: a preview judges the rules alone.
    let (response, _) = t
        .call(
            &device,
            "rules_try",
            try_args("exec", exec(), Some("Juno"), Some("chat_b")),
        )
        .await;
    let (exit, code, _) = refusal(&response);
    assert_eq!((exit, code), (Exit::Usage, "plow_chat_not_supported"));
    assert_eq!(t.judged(), 1);
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
    assert_eq!(traces(&t), untouched());
}

#[tokio::test]
async fn a_preview_leaves_nothing_in_the_audit_log_or_dashboard() {
    let t = start(chat_with(always_covered)).await;
    let device = t.caller("device").await;
    let secret = "synthetic-preview-credential-4c1e";
    let argv = json!(["curl", "--token", secret, "https://example.com"]);
    let (response, _) = t
        .call(
            &device,
            "rules_try",
            try_args("exec", json!({"argv": argv}), Some("Juno"), None),
        )
        .await;
    assert_eq!(answer(response)["verdict"], "allow");
    assert_eq!(traces(&t), untouched());
    let on_disk = std::fs::read_to_string(t.state.join(crate::audit::FILE)).unwrap_or_default();
    assert!(!on_disk.contains("--token"));
    // serve.log has one line for it: who asked, and what it found.
    let logged: Vec<String> = crate::log::captured()
        .into_iter()
        .filter(|line| line.contains("rules_try:"))
        .collect();
    assert!(!logged.is_empty());
    assert!(
        logged.iter().all(|line| !line.contains("--token")),
        "{logged:?}"
    );
    let board = serde_json::to_string(&*t.host.dashboard.watch().borrow()).unwrap();
    assert!(!board.contains(secret), "{board}");
}

#[tokio::test]
async fn the_answer_shows_the_action_as_the_judge_saw_it() {
    let t = start(chat_with(always_covered)).await;
    let device = t.caller("device").await;
    let preview = async |op: &str, args: Value| {
        let tried = try_args(op, args, Some("Juno"), None);
        answer(t.call(&device, "rules_try", tried).await.0)
    };

    let send = json!([
        "gmail",
        "send",
        "--to",
        "ada@example.com",
        "--body",
        "See you at 6."
    ]);
    let google = json!({"gog_args": send, "account": ["owner@example.com"],
                        "confirm_conflict": false, "inputs": []});
    let tried = preview("google", google).await;
    let judged = t.plow.world.lock().unwrap().asked.last().unwrap()["proposed_action"].clone();
    assert_eq!(tried["request"]["action"], judged);
    assert_eq!(tried["request"]["action"]["command"], send);
    assert_eq!(
        tried["request"]["action"]["accounts"],
        json!(["owner@example.com"])
    );

    let put = json!({"remote_path": "area/new.txt", "size": 5, "name": "new.txt"});
    let tried = preview("file_put", put).await;
    let action = &tried["request"]["action"];
    assert_eq!(action["operation"], "file_put");
    assert_eq!(action["file_name"], "new.txt");
    assert_eq!(action["size"], 5);
    assert!(
        action["path"].as_str().unwrap().ends_with("area/new.txt"),
        "{action}"
    );
    assert!(!t.area.join("new.txt").exists());

    // An exec's stdin is shown as the audit keeps it.
    let script = b"echo hi\n".to_vec();
    let exec = json!({"argv": ["/bin/sh"], "stdin": script});
    let tried = preview("exec", exec).await;
    assert_eq!(
        tried["request"]["action"]["stdin"],
        crate::exec::stdin_audit(&script)
    );

    assert_eq!(traces(&t), untouched());
}

/// A contacts page's cursor holds the name the page before ended on: the
/// preview shows its plan as the audit keeps it, without that name.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_previewed_contacts_page_does_not_show_where_the_last_page_ended() {
    let t = start(chat_with(always_covered)).await;
    t.with_messages();
    let device = t.caller("device").await;
    let first = json!({"action": "search", "args": ["--limit", "1", "--", "a"]});
    let (listed, _) = t.call(&device, "contacts", first).await;
    let Response::Ok(first) = listed else {
        panic!("{listed:?}")
    };
    assert_eq!(first["people"][0]["name"], "Ann Example");
    let next = first["next"].as_str().unwrap().to_string();

    let page = json!({"action": "search", "args": ["--cursor", next.clone()]});
    let (response, _) = t
        .call(
            &device,
            "rules_try",
            try_args("contacts", page, Some("Juno"), None),
        )
        .await;
    let tried = answer(response);
    // The judge saw the page's whole plan; the answer masks its position.
    let judged = t.plow.world.lock().unwrap().asked.last().unwrap()["proposed_action"].clone();
    assert_eq!(judged["arguments"]["after"]["key"], "Ann Example");
    let shown = &tried["request"]["action"];
    assert_eq!(shown["operation"], "contacts.search");
    assert_eq!(shown["arguments"]["after"]["key"], "[name]");
    assert_eq!(
        shown["arguments"]["after"]["id"],
        judged["arguments"]["after"]["id"]
    );
    let text = tried.to_string();
    assert!(
        !text.contains("Ann Example") && !text.contains(&next),
        "{text}"
    );
    // Only the real first page was audited and shown.
    let contacts = vec!["contacts".to_string()];
    assert_eq!(traces(&t), (contacts.clone(), contacts));
}

#[tokio::test]
async fn every_verdict_has_one_answer_shape() {
    let deny = DenyExec::from_flags(&[AGENT.to_string()]);
    let t = start_denying(chat_with(covered_after_a_yes), deny).await;
    let device = t.caller("device").await;
    let build = json!({"argv": ["npm", "run", "build"], "writes": ["~/site"], "timeout": 30});

    let (response, _) = t
        .call(
            &device,
            "rules_try",
            try_args("exec", build.clone(), None, None),
        )
        .await;
    let refused = answer(response);
    // The owner's own preview: shared rules only, none set, so allowed.
    assert_eq!(refused["verdict"], "allow");
    assert_eq!(refused["gate"]["rules"], Value::Null, "{refused}");
    let google = json!({"gog_args": ["gmail", "search", "invoice"], "account": ["owner@example.com"],
                        "confirm_conflict": false, "inputs": []});
    let (response, _) = t
        .call(
            &device,
            "rules_try",
            try_args("google", google, Some("Juno"), None),
        )
        .await;
    let not_covered = answer(response);
    assert_eq!(not_covered["verdict"], "not_covered");

    // Refused by the host: the same shape, its paths resolved, no gate.
    let (response, _) = t
        .call(
            &device,
            "rules_try",
            try_args("exec", build, Some("Juno"), None),
        )
        .await;
    let host_refused = answer(response);
    assert_eq!(host_refused["verdict"], "host_refused");
    assert_eq!(host_refused["gate"], Value::Null);
    let site = t.host.workers.home.join("site");
    assert_eq!(host_refused["request"]["action"]["writes"], json!([site]));
    assert_eq!(
        host_refused["request"]["action"]["working_folder"],
        json!(site)
    );
    let message = host_refused["refused"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("You turned exec off for this agent")
            && message.contains(&format!("--deny-exec={AGENT}")),
        "{message}"
    );
    assert!(!site.exists());

    assert_eq!(traces(&t), untouched());
}

/// Allowed by rules that say builds are fine, forbidden by rules that
/// forbid them, and not covered by anything else.
fn by_the_rules(state: &Value) -> Option<[f64; 3]> {
    let rules = state["owner_rules"].to_string();
    Some(if rules.contains("Never run builds") {
        [0.05, 0.05, 0.9]
    } else if rules.contains("Builds are fine") {
        [0.9, 0.05, 0.05]
    } else {
        [0.1, 0.8, 0.1]
    })
}

/// A preview as a new agent, against `draft` if given.
fn as_new_agent(op: &str, args: Value, draft: Option<&str>) -> Value {
    let mut tried = try_args(op, args, None, None);
    tried["new_agent"] = json!(true);
    tried["draft"] = json!(draft);
    tried
}

#[tokio::test]
async fn a_draft_is_judged_in_place_of_the_saved_rules_and_never_saved() {
    let t = start(chat_with(by_the_rules)).await;
    let device = t.caller("device").await;
    let saved = json!({"command": "set", "text": "Builds are fine."});
    answer(t.call(&device, "rules", saved).await.0);
    let build = || json!({"argv": ["npm", "run", "build"]});

    // The saved rules: allowed, judged by the rules alone, with no chat.
    let tried = answer(
        t.call(&device, "rules_try", as_new_agent("exec", build(), None))
            .await
            .0,
    );
    assert_eq!(tried["verdict"], "allow");
    assert_eq!(tried["as"], "new_agent");
    assert_eq!(tried["gate"]["caller"], "new_agent");
    assert_eq!(tried["gate"]["source"], "rules");
    assert_eq!(tried["gate"]["chat"], Value::Null);
    let asked = t.plow.world.lock().unwrap().asked.last().unwrap().clone();
    assert!(asked["conversation"].is_string(), "{asked}");

    // A draft that forbids it flips the verdict.
    let draft = "Never run builds.";
    let tried = answer(
        t.call(
            &device,
            "rules_try",
            as_new_agent("exec", build(), Some(draft)),
        )
        .await
        .0,
    );
    assert_eq!(tried["verdict"], "owner_said_no");
    let asked = t.plow.world.lock().unwrap().asked.last().unwrap().clone();
    assert!(asked["owner_rules"].to_string().contains(draft), "{asked}");
    assert!(!asked["owner_rules"].to_string().contains("Builds are fine"));

    // A blank draft is no rules: the built-in policy alone judges.
    let tried = answer(
        t.call(
            &device,
            "rules_try",
            as_new_agent("exec", build(), Some(" \n")),
        )
        .await
        .0,
    );
    assert_eq!(tried["verdict"], "not_covered");
    assert_eq!(tried["gate"]["source"], "defaults");

    // The owner previews a draft too, as themselves.
    let mut mine = try_args("exec", build(), None, None);
    mine["draft"] = json!(draft);
    let tried = answer(t.call(&device, "rules_try", mine).await.0);
    assert_eq!(tried["verdict"], "owner_said_no");
    assert_eq!(tried["as"], "owner");

    // Nothing was saved, and only the one rules command left a trace.
    let shown = answer(t.call(&device, "rules", json!({"command": "show"})).await.0);
    assert_eq!(shown["every_agent"]["text"], "Builds are fine.");
    let rules = vec!["rules".to_string(); 2];
    assert_eq!(traces(&t), (rules.clone(), rules));
}

#[tokio::test]
async fn a_new_agent_is_judged_whatever_connectors_and_deny_exec_say() {
    let deny = DenyExec::from_flags(&[crate::cli::EVERY_AGENT.to_string()]);
    let t = start_denying(chat_with(by_the_rules), deny).await;
    let device = t.caller("device").await;
    let off = t
        .local_call("plugins", json!({"command": "disable", "name": "google"}))
        .await;
    assert!(matches!(off, Response::Ok(_)), "{off:?}");
    let draft = Some("Builds are fine.");
    let exec = json!({"argv": ["/bin/sh", "-c", "touch ran"]});
    let google = json!({"gog_args": ["gmail", "search", "invoice"], "account": [],
                        "confirm_conflict": false, "inputs": []});
    for (op, args) in [("exec", exec), ("google", google)] {
        let tried = answer(
            t.call(&device, "rules_try", as_new_agent(op, args, draft))
                .await
                .0,
        );
        // Judged by the draft, not refused by the host.
        assert_eq!(tried["verdict"], "allow", "{op}: {tried}");
        assert_eq!(tried["refused"], Value::Null);
    }
    assert_eq!(t.judged(), 2);
    assert!(!t.host.workers.home.join("ran").exists());

    // A new agent has no chat, and is not also some agent.
    let mut chat = as_new_agent("exec", json!({"argv": ["true"]}), draft);
    chat["request"]["chat"] = json!("chat_a");
    let (response, _) = t.call(&device, "rules_try", chat).await;
    let (exit, code, _) = refusal(&response);
    assert_eq!((exit, code), (Exit::Usage, "plow_chat_not_supported"));
    let mut both = as_new_agent("exec", json!({"argv": ["true"]}), draft);
    both["from"] = json!("Juno");
    let (response, _) = t.call(&device, "rules_try", both).await;
    let (exit, code, _) = refusal(&response);
    assert_eq!((exit, code), (Exit::Usage, "bad_request"));
    assert_eq!(t.judged(), 2);
}

#[tokio::test]
async fn a_path_the_host_never_allows_is_the_hosts_refusal() {
    let t = start(chat_with(always_covered)).await;
    let device = t.caller("device").await;
    let hidden = t.host.workers.state_dir.join("config.json");
    for (op, args) in [
        ("exec", json!({"argv": ["true"], "writes": ["/"]})),
        ("exec", json!({"argv": ["true"], "writes": ["~"]})),
        ("file_get", json!({"remote_path": hidden})),
    ] {
        let tried = answer(
            t.call(
                &device,
                "rules_try",
                try_args(op, args.clone(), Some("Juno"), None),
            )
            .await
            .0,
        );
        assert_eq!(tried["verdict"], "host_refused", "{args}: {tried}");
        assert_eq!(tried["refused"]["code"], "bad_path", "{args}");
        assert_eq!(tried["gate"], Value::Null);
    }
    assert_eq!(t.judged(), 0);
    // A malformed request is still an error, not a verdict.
    let (response, _) = t
        .call(
            &device,
            "rules_try",
            try_args("exec", json!({"argv": []}), Some("Juno"), None),
        )
        .await;
    assert_eq!(refusal(&response).1, "bad_request");
    assert_eq!(traces(&t), untouched());
}

/// A preview of a login fill of `title` on `url`, into `fields`.
fn fill_args(url: &str, kind: &str, title: &str, fields: Value) -> Value {
    json!({"fill": {"url": url, "item": {"kind": kind, "title": title}, "fields": fields}})
}

/// The names in the host's state folder.
fn state_files(t: &super::gated_tests::TestHost) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(&t.state)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_login_fill_is_judged_as_the_browser_judges_it_and_nothing_is_opened() {
    let t = start(chat_with(always_covered)).await;
    let device = t.caller("device").await;
    let before = state_files(&t);
    let fill = fill_args(
        "https://www.instacart.com/login",
        "login",
        "Instacart",
        json!(["username", "password"]),
    );
    let tried = answer(t.call(&device, "rules_try", fill).await.0);
    assert_eq!(tried["verdict"], "allow");
    assert_eq!(tried["as"], "owner");
    assert_eq!(tried["gate"]["caller"], "owner_device");
    // The action a real fill shows the judge, and only that.
    let shown = json!({"kind": "vault_fill", "site": "instacart.com", "page_host": "www.instacart.com",
                       "item": {"kind": "login", "title": "Instacart", "sites": ["instacart.com"]},
                       "fields": ["username", "password"]});
    let judged = t.plow.world.lock().unwrap().asked.last().unwrap()["proposed_action"].clone();
    assert_eq!(judged, shown);
    assert_eq!(tried["request"], json!({"op": "browser", "action": shown}));
    assert_eq!(t.judged(), 1);
    // No vault opened or made, no key read or written, no browser
    // session, and nothing audited: only what the judge was sent is kept,
    // as for every judgment.
    let kept = tried["gate"]["kept_request"].as_str().unwrap();
    let kept = crate::gate::requests::read(&t.state, kept, true).unwrap();
    assert_eq!(kept["request"]["state"]["proposed_action"], shown);
    let mut after = state_files(&t);
    after.retain(|name| name != "gatekeeper");
    assert_eq!(after, before);
    assert!(
        !before.iter().any(|name| name.starts_with("vault")),
        "{before:?}"
    );
    assert_eq!(traces(&t), untouched());
}

#[tokio::test]
async fn a_fill_preview_judges_as_the_agent_by_its_rules_alone() {
    let t = start(chat_with(by_the_rules)).await;
    let device = t.caller("device").await;
    let set = json!({"command": "set", "from": "Juno", "text": "Builds are fine."});
    answer(t.call(&device, "rules", set).await.0);
    let mut fill = fill_args(
        "https://shop.example/login",
        "login",
        "Shop",
        json!(["password"]),
    );
    fill["from"] = json!("Juno");
    let tried = answer(t.call(&device, "rules_try", fill.clone()).await.0);
    assert_eq!(tried["verdict"], "allow");
    assert_eq!(
        tried["as"],
        json!({"agent_uid": AGENT, "agent_name": "Juno"})
    );
    assert_eq!(tried["gate"]["caller"], "agent");
    assert_eq!(tried["gate"]["source"], "rules");
    assert_eq!(tried["request"]["action"]["fields"], json!(["password"]));

    // A draft for every agent stands in for the saved rules.
    fill["draft"] = json!("Never run builds.");
    let tried = answer(t.call(&device, "rules_try", fill).await.0);
    assert_eq!(tried["verdict"], "owner_said_no");
    assert_eq!(t.judged(), 2);
}

#[tokio::test]
async fn a_fill_that_is_not_a_login_on_a_site_is_refused_unjudged() {
    let t = start(chat_with(always_covered)).await;
    let device = t.caller("device").await;
    let login = json!(["username", "password"]);
    for fill in [
        fill_args("instacart", "login", "Instacart", login.clone()),
        fill_args("https://www.instacart.com/", "card", "Visa", login.clone()),
        fill_args("https://www.instacart.com/", "login", " ", login.clone()),
        fill_args(
            "https://www.instacart.com/",
            "login",
            "Instacart",
            json!([]),
        ),
        fill_args(
            "https://www.instacart.com/",
            "login",
            "Instacart",
            json!([""]),
        ),
    ] {
        let (response, _) = t.call(&device, "rules_try", fill.clone()).await;
        let (exit, code, _) = refusal(&response);
        assert_eq!((exit, code), (Exit::Usage, "bad_request"), "{fill}");
    }
    assert_eq!(t.judged(), 0);
}
