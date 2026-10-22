//! A host's gatekeeper in front of exec, files and Google, and the owner's
//! rules for it, end to end: a headless host on this machine with real
//! sandboxed workers, callers over iroh, and a synthetic Plow holding the
//! owner's chat. No real chat is ever read.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;
use crate::failure::Exit;
use crate::gate::fake::{self, AGENT, FakePlow, World, agent_self, message, owner};
use crate::model;

/// The owner's yes covers an operation; anything else does not.
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

fn down(_: &Value) -> Option<[f64; 3]> {
    None
}

const ASKED: &str = "could you tidy up the work folder at some point?";

fn a_chat(decide: fake::Decide) -> World {
    let mut world = World::with_chat(
        "chat_a",
        vec![
            message("m1", owner(), ASKED, 30),
            message("m2", agent_self(), "Sure, I'll look later.", 29),
        ],
    );
    world.decide = Some(decide);
    world
}

struct TestHost {
    host: Arc<Host>,
    plow: FakePlow,
    area: PathBuf,
    state: PathBuf,
    /// What the host watches as its own binary: a stand-in, so a test can
    /// replace it without touching the workers' real one.
    binary: PathBuf,
    _dir: tempfile::TempDir,
}

/// The latch binary cargo builds beside this test binary: the workers'.
fn latch_binary() -> PathBuf {
    let deps = std::env::current_exe().unwrap();
    deps.parent().unwrap().parent().unwrap().join("latch")
}

async fn start(world: World) -> TestHost {
    start_denying(world, DenyExec::default()).await
}

/// A host whose owner denies exec as `deny_exec` says.
async fn start_denying(world: World, deny_exec: DenyExec) -> TestHost {
    let fake = fake::serve(world);
    let provider = model::Plow::new(&fake.url, "made-up-login".into()).with_model(gate::MODEL);
    start_with(fake, Arc::new(provider), deny_exec, None).await
}

/// A host on `plow`, its gatekeeper asking `provider`.
async fn start_with(
    plow: FakePlow,
    provider: Arc<dyn model::Provider>,
    deny_exec: DenyExec,
    dashboard: Option<Dashboard>,
) -> TestHost {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
    std::fs::create_dir_all(&parent).unwrap();
    let dir = tempfile::tempdir_in(parent).unwrap();
    let root = dir.path().canonicalize().unwrap();
    let (area, state) = (root.join("area"), root.join("state"));
    std::fs::create_dir(&area).unwrap();
    std::fs::create_dir(&state).unwrap();
    std::fs::write(area.join("notes.txt"), "the notes' own words").unwrap();
    let binary = root.join("latch");
    std::fs::write(&binary, "the latch this host started from").unwrap();
    let endpoint = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .alpns(vec![wire::ALPN.to_vec(), link::ALPN.to_vec()])
        .bind()
        .await
        .unwrap();
    let vault = crate::vault::host::Vault::new(state.clone());
    let rules = RuleFiles::new(&state, std::slice::from_ref(&area)).unwrap();
    let host = Arc::new(Host {
        plow: plow.client(),
        audit: Arc::new(Mutex::new(
            AuditLog::open(state.join("audit.ndjson")).unwrap(),
        )),
        dashboard,
        workers: WorkerSetup {
            areas: vec![area.clone()],
            reads: vec![],
            hidden: vec![],
            state_folders: crate::state_folders::StateFolders::none(),
            state_dir: state.clone(),
            program: latch_binary(),
        },
        browsers: crate::browser::host::Browsers::new(state.clone(), vault.clone(), true),
        vault,
        endpoint: endpoint.clone(),
        name: "test-host".into(),
        gatekeeper: Gatekeeper::new(plow.client(), provider, Box::new(rules)),
        deny_exec,
        binary: Binary::at(&binary).unwrap(),
        stopping: tokio::sync::watch::Sender::new(Stopping::No),
    });
    let serving = host.clone();
    tokio::spawn(async move {
        while let Some(incoming) = serving.endpoint.accept().await {
            tokio::spawn(serve_connection(serving.clone(), incoming));
        }
    });
    TestHost {
        host,
        plow,
        area,
        state,
        binary,
        _dir: dir,
    }
}

impl TestHost {
    /// A caller Plow knows as an agent (with its uid) or a device.
    async fn caller(&self, kind: &str) -> Endpoint {
        let client = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let key = client.id().to_string();
        let agent_uid = (kind == "agent").then_some(AGENT);
        let identity = json!({"key": key, "kind": kind, "name": format!("a {kind}"),
                              "agent_uid": agent_uid, "permissions": {"call": true}});
        self.plow
            .world
            .lock()
            .unwrap()
            .callers
            .insert(key, identity);
        client
    }

    /// Sends one operation and returns the host's first answer, and
    /// whatever else came back on the stream.
    async fn call(&self, client: &Endpoint, op: &str, args: Value) -> (Response, Vec<u8>) {
        let connection = client
            .connect(self.host.endpoint.addr(), wire::ALPN)
            .await
            .unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let request = Request {
            op: op.into(),
            args,
            chat: None,
        };
        wire::write_frame(&mut send, &serde_json::to_vec(&request).unwrap())
            .await
            .unwrap();
        let answer = wire::read_frame(&mut recv).await.unwrap();
        let rest = recv.read_to_end(1 << 20).await.unwrap_or_default();
        connection.close(VarInt::from_u32(0), b"done");
        (serde_json::from_slice(&answer).unwrap(), rest)
    }

    fn audit(&self) -> Vec<Value> {
        std::fs::read_to_string(self.state.join("audit.ndjson"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|record: &Value| record["op"] != "connect")
            .collect()
    }

    fn judged(&self) -> usize {
        self.plow.saw("POST /v1/decisions")
    }
}

fn exec(script: &str) -> Value {
    json!({"argv": ["/bin/sh", "-c", script], "workdir": null, "timeout": 10})
}

fn refusal(response: &Response) -> (Exit, &str, &Value) {
    match response {
        Response::Error {
            exit,
            code,
            details,
            ..
        } => (*exit, code.as_str(), details),
        Response::Ok(result) => panic!("allowed: {result}"),
    }
}

#[tokio::test]
async fn an_operation_is_audited_before_dashboard_name_lookup() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let released = Arc::new(AtomicBool::new(false));
    let names = format!("/v1/agents/{AGENT}");
    let world = World {
        paused: Some((names.clone(), released.clone())),
        ..World::default()
    };
    let plow = fake::serve(world);
    let provider = model::Plow::new(&plow.url, "made-up-login".into()).with_model(gate::MODEL);
    let t = start_with(
        plow,
        Arc::new(provider),
        DenyExec::from_flags(&[crate::cli::EVERY_AGENT.to_string()]),
        Some(Dashboard::start("test-host")),
    )
    .await;
    let agent = t.caller("agent").await;
    let during_lookup = async {
        let requested = tokio::time::timeout(Duration::from_secs(30), async {
            while t.plow.saw(&format!("GET {names}")) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        let records = t.audit();
        released.store(true, Ordering::SeqCst);
        (requested, records)
    };
    let ((answer, _), (requested, records)) =
        tokio::join!(t.call(&agent, "exec", exec("touch made")), during_lookup);
    t.host.dashboard.as_ref().unwrap().stop();
    assert!(requested.is_ok(), "the dashboard never asked for the name");
    assert_eq!(refusal(&answer).1, "exec_denied");
    assert!(!t.area.join("made").exists());
    assert_eq!(records.len(), 1, "the audit waited for the name lookup");
    assert_eq!(records[0]["outcome"], "exec_denied");
}

#[tokio::test]
async fn a_refused_operation_runs_nothing_and_returns_nothing() {
    let t = start(a_chat(covered_after_a_yes)).await;
    let agent = t.caller("agent").await;

    let (answer, rest) = t
        .call(&agent, "exec", exec("echo the-output; touch made"))
        .await;
    let (exit, code, details) = refusal(&answer);
    assert_eq!((exit, code), (Exit::Denied, "gatekeeper_denied"));
    assert_eq!(details["reason"], "not_covered");
    assert!(rest.is_empty());
    assert!(!t.area.join("made").exists(), "the program never ran");

    let (answer, rest) = t
        .call(&agent, "file_get", json!({"remote_path": "notes.txt"}))
        .await;
    assert_eq!(refusal(&answer).1, "gatekeeper_denied");
    assert!(rest.is_empty(), "no bytes of the file");
    assert!(!format!("{answer:?}").contains("own words"));

    let put = json!({"remote_path": "new.txt", "size": 5, "name": "new.txt"});
    let (answer, _) = t.call(&agent, "file_put", put).await;
    assert_eq!(refusal(&answer).1, "gatekeeper_denied");
    assert!(!t.area.join("new.txt").exists());

    let google = json!({"gog_args": ["gmail", "search", "invoice"], "account": [],
                        "confirm_conflict": false, "inputs": []});
    let (answer, rest) = t.call(&agent, "google", google).await;
    assert_eq!(refusal(&answer).1, "gatekeeper_denied");
    assert!(rest.is_empty());
    assert_eq!(
        t.plow.saw("access-token"),
        0,
        "no Google credential fetched"
    );

    assert_eq!(t.judged(), 4);
    let records = t.audit();
    assert_eq!(records.len(), 4);
    for record in &records {
        assert_eq!(record["decision"], "deny");
        assert_eq!(record["outcome"], "gatekeeper_denied");
        assert_eq!(record["gate"]["verdict"], "not_covered");
    }
    assert_eq!(records[3]["args"], json!({"command": "gmail.search"}));
}

#[tokio::test]
async fn a_judge_that_cannot_answer_runs_nothing() {
    let t = start(a_chat(down)).await;
    let agent = t.caller("agent").await;
    let (answer, _) = t.call(&agent, "exec", exec("touch made")).await;
    let (exit, code, _) = refusal(&answer);
    assert_eq!((exit, code), (Exit::Unreachable, "gatekeeper_unavailable"));
    assert!(!t.area.join("made").exists());
    let (answer, rest) = t
        .call(&agent, "file_get", json!({"remote_path": "notes.txt"}))
        .await;
    assert_eq!(refusal(&answer).1, "gatekeeper_unavailable");
    assert!(rest.is_empty());
    let records = t.audit();
    assert!(records.iter().all(|r| r["decision"] == "deny"));
}

#[tokio::test]
async fn the_owners_reply_lets_the_retry_through_and_every_call_is_judged() {
    let t = start(a_chat(covered_after_a_yes)).await;
    let agent = t.caller("agent").await;
    let tidy = exec("echo tidied >> made");
    let (answer, _) = t.call(&agent, "exec", tidy.clone()).await;
    assert_eq!(refusal(&answer).2["reason"], "not_covered");

    // The agent asks; the owner answers in the chat.
    {
        let mut world = t.plow.world.lock().unwrap();
        let chat = world.messages.get_mut("chat_a").unwrap();
        chat.push(message(
            "m3",
            agent_self(),
            "May I run `echo tidied >> made`?",
            2,
        ));
        chat.push(message("m4", owner(), "yes, go ahead", 1));
    }

    for _ in 0..2 {
        let (answer, _) = t.call(&agent, "exec", tidy.clone()).await;
        let Response::Ok(result) = answer else {
            panic!("refused: {answer:?}");
        };
        assert_eq!(result["exit_code"], 0);
    }
    assert_eq!(
        std::fs::read_to_string(t.area.join("made")).unwrap(),
        "tidied\ntidied\n"
    );
    assert_eq!(t.judged(), 3, "the same exec is judged each time");
    let asked = t.plow.world.lock().unwrap().asked.clone();
    assert_eq!(
        asked[2]["already_done_by_agent"], "(nothing since this host started)",
        "an exec run again is not a one-time action"
    );

    let records = t.audit();
    let gate = &records[2]["gate"];
    assert_eq!(gate["verdict"], "allow");
    assert_eq!(gate["source"], "chat");
    assert_eq!(gate["chat"], "chat_a");
    assert_eq!(gate["owner_messages"], json!(["m1", "m4"]));
    assert_eq!(gate["window"]["count"], 4);
    assert_eq!(gate["model"], gate::MODEL);
    assert_eq!(gate["model_request"], "req_fake");
    assert_eq!(gate["cost"], 0.0001);
    assert!(gate["p"]["COVERED"].as_f64().unwrap() >= 0.5);
    assert_eq!(records[2]["args"]["argv"][2], "echo tidied >> made");

    let log = std::fs::read_to_string(t.state.join("audit.ndjson")).unwrap();
    for said in [ASKED, "yes, go ahead", "May I run", "Sure, I'll look later"] {
        assert!(!log.contains(said), "chat text in the audit: {said}");
    }
}

fn always_covered(_: &Value) -> Option<[f64; 3]> {
    Some([0.9, 0.08, 0.02])
}

#[tokio::test]
async fn a_google_change_is_already_done_the_next_time_and_a_read_is_not() {
    let t = start(a_chat(always_covered)).await;
    let agent = t.caller("agent").await;
    let google = |gog_args: &[&str]| json!({"gog_args": gog_args, "account": [], "confirm_conflict": false, "inputs": []});
    let send = google(&[
        "gmail",
        "send",
        "--to",
        "a@example.com",
        "--body",
        "the body",
    ]);
    let search = google(&["gmail", "search", "invoice"]);
    for args in [&search, &search, &send, &send] {
        t.call(&agent, "google", args.clone()).await;
    }
    let asked = t.plow.world.lock().unwrap().asked.clone();
    assert_eq!(asked.len(), 4);
    for state in &asked[..3] {
        assert_eq!(
            state["already_done_by_agent"],
            "(nothing since this host started)"
        );
    }
    let done = &asked[3]["already_done_by_agent"]["since this host started"];
    assert_eq!(done.as_array().unwrap().len(), 1, "{done}");
    assert_eq!(done[0]["what"]["command"], send["gog_args"]);
}

/// Covered until the agent has already done something since the host started.
fn covered_once(state: &Value) -> Option<[f64; 3]> {
    Some(if state["already_done_by_agent"].is_string() {
        [0.9, 0.08, 0.02]
    } else {
        [0.1, 0.85, 0.05]
    })
}

#[tokio::test]
async fn the_same_google_change_sent_twice_at_once_runs_once() {
    let t = start(a_chat(covered_once)).await;
    let agent = t.caller("agent").await;
    let send = json!({"gog_args": ["gmail", "send", "--to", "a@example.com", "--body", "the body"],
                      "account": [], "confirm_conflict": false, "inputs": []});
    let (first, second) = tokio::join!(
        t.call(&agent, "google", send.clone()),
        t.call(&agent, "google", send.clone()),
    );
    // The second is judged after the first ran, and sees it already done.
    // (The one that runs stops at the missing gog: it ran all the same.)
    let refused = [&first.0, &second.0]
        .into_iter()
        .filter(
            |answer| matches!(answer, Response::Error { code, .. } if code == "gatekeeper_denied"),
        )
        .count();
    assert_eq!(refused, 1, "{first:?} {second:?}");
    assert_eq!(t.judged(), 2);
}

#[tokio::test]
async fn an_exec_the_host_denies_is_refused_without_asking_the_judge() {
    let every_agent = DenyExec::from_flags(&[crate::cli::EVERY_AGENT.to_string()]);
    let t = start_denying(a_chat(always_covered), every_agent).await;
    let agent = t.caller("agent").await;
    let (answer, _) = t.call(&agent, "exec", exec("touch made")).await;
    let (exit, code, _) = refusal(&answer);
    assert_eq!((exit, code), (Exit::Denied, "exec_denied"));
    assert!(!t.area.join("made").exists());
    assert_eq!(t.judged(), 0);
    assert!(t.audit()[0].get("gate").is_none());
    // Other operations are still judged.
    let (answer, _) = t
        .call(&agent, "file_get", json!({"remote_path": "notes.txt"}))
        .await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    assert_eq!(t.judged(), 1);
}

/// An exec of `/bin/sh` reading its script from `stdin`.
fn exec_stdin(stdin: &[u8]) -> Value {
    json!({"argv": ["/bin/sh"], "workdir": null, "timeout": 10, "stdin": stdin})
}

/// Whether the host's log or audit holds `text` anywhere.
fn kept(t: &TestHost, text: &str) -> bool {
    let audit = std::fs::read_to_string(t.state.join("audit.ndjson")).unwrap();
    audit.contains(text)
        || crate::log::captured()
            .iter()
            .any(|line| line.contains(text))
}

#[tokio::test]
async fn an_agents_exec_stdin_is_judged_with_its_argv_and_never_kept() {
    let t = start(a_chat(always_covered)).await;
    let agent = t.caller("agent").await;
    let script = "touch made # stdin-judged-7f3a";
    let (answer, _) = t.call(&agent, "exec", exec_stdin(script.as_bytes())).await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    assert!(t.area.join("made").exists());
    let asked = t.plow.world.lock().unwrap().asked.clone();
    assert_eq!(asked[0]["proposed_action"]["argv"], json!(["/bin/sh"]));
    assert_eq!(asked[0]["proposed_action"]["stdin"], script);
    assert_eq!(t.audit()[0]["args"]["stdin"]["size"], script.len());
    assert!(!kept(&t, "stdin-judged-7f3a"));
}

#[tokio::test]
async fn an_agents_stdin_the_judge_cannot_read_runs_nothing() {
    let t = start(a_chat(always_covered)).await;
    let agent = t.caller("agent").await;
    let mut too_long = b"touch made\n".to_vec();
    too_long.resize(crate::gate::JUDGED_STDIN + 1, b'#');
    for stdin in [too_long, b"touch made\n\xff".to_vec()] {
        let (answer, _) = t.call(&agent, "exec", exec_stdin(&stdin)).await;
        let (exit, code, _) = refusal(&answer);
        assert_eq!((exit, code), (Exit::Usage, "stdin_not_judgeable"));
        assert!(!t.area.join("made").exists());
    }
    assert_eq!(t.judged(), 0);
    let records = t.audit();
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|record| record["decision"] == "deny"));
    // The owner's stdin is judged too, so the same bounds apply.
    let device = t.caller("device").await;
    let mut long = b"touch made\n".to_vec();
    long.resize(crate::gate::JUDGED_STDIN + 1, b'#');
    let (answer, _) = t.call(&device, "exec", exec_stdin(&long)).await;
    assert_eq!(refusal(&answer).1, "stdin_not_judgeable");
    assert!(!t.area.join("made").exists());
}

#[tokio::test]
async fn an_exec_the_host_denies_keeps_no_trace_of_its_stdin() {
    let every_agent = DenyExec::from_flags(&[crate::cli::EVERY_AGENT.to_string()]);
    let t = start_denying(a_chat(always_covered), every_agent).await;
    let agent = t.caller("agent").await;
    let secret = b"echo denied-stdin-91c2";
    let (answer, _) = t.call(&agent, "exec", exec_stdin(secret)).await;
    assert_eq!(refusal(&answer).1, "exec_denied");
    let record = &t.audit()[0];
    assert_eq!(record["args"]["argv"], json!(["/bin/sh"]));
    assert_eq!(record["args"]["stdin"]["size"], secret.len());
    // The bytes as the request carried them, and as text.
    let as_numbers = serde_json::to_string(&secret.to_vec()).unwrap();
    let as_numbers = &as_numbers[1..as_numbers.len() - 1];
    assert!(!kept(&t, "denied-stdin-91c2"));
    assert!(!kept(&t, as_numbers));
}

/// The owner's own request is covered; anything else is judged as before.
fn the_owners_request(state: &Value) -> Option<[f64; 3]> {
    let owner = state["rules_of_judgment"]
        .as_array()
        .unwrap()
        .iter()
        .any(|rule| {
            rule.as_str()
                .unwrap()
                .starts_with("The caller is the owner")
        });
    covered_after_a_yes(state).map(|p| if owner { [0.9, 0.08, 0.02] } else { p })
}

#[tokio::test]
async fn the_owners_own_devices_are_judged_as_the_owner() {
    let t = start(a_chat(the_owners_request)).await;
    let device = t.caller("device").await;
    let (answer, _) = t.call(&device, "exec", exec("touch made")).await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    assert!(t.area.join("made").exists());
    let (answer, _) = t
        .call(&device, "file_get", json!({"remote_path": "notes.txt"}))
        .await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    let put = json!({"remote_path": "new.txt", "size": 0, "name": "new.txt"});
    t.call(&device, "file_put", put).await;
    let google = json!({"gog_args": ["gmail", "search", "invoice"], "account": [],
                        "confirm_conflict": false, "inputs": []});
    t.call(&device, "google", google).await;
    assert_eq!(t.judged(), 4, "exec, file get, file put and google");
    // A browser open is judged too, before any engine starts.
    let open = json!({"command": "open", "profile": "default", "url": "https://example.com/"});
    t.call(&device, "browser", open).await;
    assert_eq!(t.judged(), 5, "and the browser's open");
    let browsed = t.audit().pop().unwrap();
    assert_eq!(browsed["op"], "browser");
    // The browser keeps its judgment with its arguments.
    assert_eq!(
        browsed["args"]["gate"]["caller"], "owner_device",
        "{browsed}"
    );

    // Judged as the owner's own request, sent just now: no agent's chat.
    let asked = t.plow.world.lock().unwrap().asked.clone();
    for state in &asked {
        let said = state["conversation"].as_array().unwrap();
        assert_eq!(said.len(), 1, "{said:?}");
        assert_eq!(said[0]["from"], "OWNER");
        let text = said[0]["text"].as_str().unwrap();
        let request: Value =
            serde_json::from_str(text.strip_prefix("Do this now: ").unwrap()).unwrap();
        assert_eq!(request, state["proposed_action"]);
        assert!(
            state["rules_of_judgment"]
                .to_string()
                .contains("The caller is the owner")
        );
    }
    let records = t.audit();
    assert_eq!(records[0]["gate"]["caller"], "owner_device");
    assert_eq!(records[0]["gate"]["source"], "owner_request");
    assert_eq!(records[0]["gate"]["verdict"], "allow");

    // An agent on the same host is judged as an agent, on its chat.
    let agent = t.caller("agent").await;
    let (answer, _) = t.call(&agent, "exec", exec("touch other")).await;
    assert_eq!(refusal(&answer).2["reason"], "not_covered");
    let record = t.audit().pop().unwrap();
    assert_eq!(record["gate"]["caller"], "agent");
    assert_eq!(record["gate"]["verdict"], "not_covered");
    assert_eq!(record["agent"], AGENT);

    // The host's rules hold for the owner too, read as the host's, and
    // for an agent as every agent's.
    let rules = t.state.join(crate::gate::rules::RULES_DIR);
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(rules.join("host.txt"), "Never send email.").unwrap();
    t.call(&device, "exec", exec("true")).await;
    t.call(&agent, "exec", exec("true")).await;
    let asked = t.plow.world.lock().unwrap().asked.clone();
    let [.., owners, agents] = &asked[..] else {
        panic!("{asked:?}");
    };
    assert_eq!(
        owners["owner_rules"],
        json!({"rules on this host": "Never send email."})
    );
    assert_eq!(
        agents["owner_rules"],
        json!({"rules for every agent on this host": "Never send email."})
    );
}

#[tokio::test]
async fn an_outage_refuses_gated_operations_for_everyone_but_not_the_owners_settings() {
    let t = start(a_chat(down)).await;
    let device = t.caller("device").await;
    let agent = t.caller("agent").await;
    for caller in [&device, &agent] {
        let (answer, _) = t.call(caller, "exec", exec("touch made")).await;
        let (exit, code, _) = refusal(&answer);
        assert_eq!((exit, code), (Exit::Unreachable, "gatekeeper_unavailable"));
        let Response::Error { message, .. } = answer else {
            unreachable!()
        };
        assert!(message.contains("rules, vault, shares"), "{message}");
    }
    assert!(!t.area.join("made").exists());
    let asked = t.judged();

    // The owner's settings never ask the judge, and work while it is down.
    let set = json!({"command": "set", "text": "Never send email."});
    let (answer, _) = t.call(&device, "rules", set).await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    let (answer, _) = t.call(&device, "rules", json!({"command": "show"})).await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    let (answer, _) = t.call(&device, "vault", json!({"command": "list"})).await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    assert_eq!(t.judged(), asked, "no judgment for the owner's settings");

    // And they stay the owner's: an agent is refused them.
    for (op, args) in [
        ("rules", json!({"command": "show"})),
        ("vault", json!({"command": "list"})),
    ] {
        let (answer, _) = t.call(&agent, op, args).await;
        assert_eq!(refusal(&answer).1, "owner_only", "{op}");
    }
    assert_eq!(t.judged(), asked);
}

#[tokio::test]
async fn an_agent_plow_gives_no_uid_runs_nothing() {
    let t = start(a_chat(covered_after_a_yes)).await;
    let agent = t.caller("agent").await;
    let key = agent.id().to_string();
    t.plow.world.lock().unwrap().callers.get_mut(&key).unwrap()["agent_uid"] = Value::Null;
    let (answer, _) = t.call(&agent, "exec", exec("touch made")).await;
    let (exit, code, _) = refusal(&answer);
    assert_eq!((exit, code), (Exit::Unreachable, "gatekeeper_unavailable"));
    assert!(!t.area.join("made").exists());
    assert_eq!(t.judged(), 0);
    assert_eq!(t.audit()[0]["decision"], "deny");
}

#[tokio::test]
async fn an_agent_with_no_chat_and_no_rules_has_no_intent() {
    let world = World {
        decide: Some(covered_after_a_yes),
        ..World::default()
    };
    let t = start(world).await;
    let agent = t.caller("agent").await;
    let (answer, _) = t.call(&agent, "exec", exec("touch made")).await;
    assert_eq!(refusal(&answer).0, Exit::Denied);
    assert_eq!(refusal(&answer).1, "no_intent");
    assert!(!t.area.join("made").exists());
    assert_eq!(t.judged(), 0, "nothing to judge, so no model call");
    assert_eq!(t.audit()[0]["gate"]["verdict"], "no_intent");
}

/// The owner's `latch rules` commands over a real connection, as the
/// client sends them: set, edit, show and clear work from a device, an
/// agent gets `owner_only` for each, and the agent's next judgment reads
/// what the owner set.
#[tokio::test]
async fn the_owners_rules_commands_work_over_the_connection_and_never_for_an_agent() {
    let world = World {
        decide: Some(always_covered),
        ..World::default()
    };
    let t = start(world).await;
    let device = t.caller("device").await;
    let agent = t.caller("agent").await;
    let ok = |answer: Response| match answer {
        Response::Ok(result) => result,
        refused => panic!("refused: {refused:?}"),
    };
    let rules = |name: &str| std::fs::read_to_string(t.state.join("rules").join(name)).ok();

    let set = json!({"command": "set", "text": "Never send email."});
    let done = ok(t.call(&device, "rules", set).await.0);
    assert_eq!(done["host"], "test-host");
    assert_eq!(done["scope"], "every agent");
    assert_eq!(done["applies"], "applies to the next judgment");
    // `--from` by the peer name of the agent's latch.
    let set = json!({"command": "set", "from": "a agent", "text": "Run tests in the work folder."});
    let done = ok(t.call(&device, "rules", set).await.0);
    assert_eq!(
        (done["agent_uid"].as_str(), done["agent_name"].as_str()),
        (Some(AGENT), Some("Juno"))
    );

    // `latch rules edit --from Juno`: a show, then an edit at its revision.
    let shown = ok(t
        .call(&device, "rules", json!({"command": "show", "from": "Juno"}))
        .await
        .0);
    let read = shown["agents"][0]["revision"].clone();
    let edit = |text: &str, revision: &Value| json!({"command": "edit", "from": AGENT, "text": text, "revision": revision});
    let edited = "Run tests in the work folder. Nothing else.";
    ok(t.call(&device, "rules", edit(edited, &read)).await.0);
    let (stale, _) = t
        .call(&device, "rules", edit("Anything goes.", &read))
        .await;
    assert_eq!(refusal(&stale).0, Exit::Denied);
    assert_eq!(refusal(&stale).1, "stale_revision");
    assert_eq!(rules(&format!("{AGENT}.txt")).as_deref(), Some(edited));

    let shown = ok(t.call(&device, "rules", json!({"command": "show"})).await.0);
    assert_eq!(shown["every_agent"]["text"], "Never send email.");
    assert_eq!(shown["agents"][0]["text"], edited);
    assert_eq!(
        shown["warning"],
        "Freeform rules are judged, not guaranteed. --deny-exec is enforced in code."
    );

    for args in [
        json!({"command": "show"}),
        json!({"command": "set", "text": "Anything goes."}),
        json!({"command": "edit", "text": "Anything goes.", "revision": read}),
        json!({"command": "clear", "from": "Juno"}),
    ] {
        let (answer, _) = t.call(&agent, "rules", args.clone()).await;
        assert_eq!(refusal(&answer).0, Exit::Denied, "{args}");
        assert_eq!(refusal(&answer).1, "owner_only", "{args}");
    }
    assert_eq!(rules("host.txt").as_deref(), Some("Never send email."));
    assert_eq!(rules(&format!("{AGENT}.txt")).as_deref(), Some(edited));

    // The agent's next operation is judged on both texts, with no restart.
    let (answer, _) = t.call(&agent, "exec", exec("true")).await;
    ok(answer);
    let asked = t.plow.world.lock().unwrap().asked.last().unwrap().clone();
    assert_eq!(
        asked["owner_rules"],
        json!({"rules for every agent on this host": "Never send email.",
               "rules for this agent": edited})
    );

    let done = ok(t
        .call(
            &device,
            "rules",
            json!({"command": "clear", "from": "Juno"}),
        )
        .await
        .0);
    assert_eq!(done["cleared"], true);
    assert_eq!(rules(&format!("{AGENT}.txt")), None);
    assert_eq!(rules("host.txt").as_deref(), Some("Never send email."));
    ok(t.call(&device, "rules", json!({"command": "clear"}))
        .await
        .0);
    assert_eq!(rules("host.txt"), None);

    // Every command is audited by revision, never by text.
    let audit = t.audit();
    let rules_records: Vec<&Value> = audit.iter().filter(|r| r["op"] == "rules").collect();
    assert_eq!(rules_records.len(), 12);
    let denied = rules_records
        .iter()
        .filter(|r| r["decision"] == "deny")
        .count();
    assert_eq!(denied, 5, "four from the agent, one stale edit");
    let text = serde_json::to_string(&audit).unwrap();
    assert!(
        !text.contains("email") && !text.contains("work folder"),
        "{text}"
    );
}

/// Rules alone, judged by jev itself: an agent with neither chat nor rules
/// has no intent; with rules allowing only tests, `pytest` is allowed every
/// time, repeats included, and `curl` never. `just gatekeeper-live KEY`
/// runs it.
#[cfg(feature = "live-model")]
#[tokio::test]
async fn live_rules_alone_allow_tests_and_nothing_else() {
    use std::os::unix::fs::PermissionsExt;
    let key = std::env::var_os("LATCH_TEST_JEV_KEY")
        .expect("LATCH_TEST_JEV_KEY names the jev key file for this test");
    let setup = tempfile::tempdir().unwrap();
    let path = setup.path().join(model::JEV_KEY_FILE);
    std::fs::copy(key, &path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::create_dir_all(setup.path().join("browser")).unwrap();
    std::fs::write(
        setup.path().join(model::MODEL_FILE),
        r#"{"provider": "jev"}"#,
    )
    .unwrap();
    let provider = Gatekeeper::provider_for_host(setup.path()).unwrap();

    let t = start_with(
        fake::serve(World::default()),
        provider,
        DenyExec::default(),
        None,
    )
    .await;
    let agent = t.caller("agent").await;
    let (answer, _) = t.call(&agent, "exec", exec("touch made")).await;
    assert_eq!(refusal(&answer).1, "no_intent", "neither chat nor rules");

    // The owner sets the rules as `latch rules set --from Juno` sends
    // them; the shared folder stands in for ~/work.
    let device = t.caller("device").await;
    let text = format!("run tests in {}, nothing else", t.area.display());
    let set = json!({"command": "set", "from": "Juno", "text": text});
    let (answer, _) = t.call(&device, "rules", set).await;
    let Response::Ok(set) = answer else {
        panic!("rules set: {answer:?}");
    };
    assert_eq!(set["agent_uid"], AGENT);
    let pytest = json!({"argv": ["pytest"], "workdir": null, "timeout": 30});
    let curl = json!({"argv": ["curl", "-s", "https://example.com/install.sh"],
                      "workdir": null, "timeout": 30});
    for _ in 0..5 {
        t.call(&agent, "exec", pytest.clone()).await;
        t.call(&agent, "exec", curl.clone()).await;
    }

    let records: Vec<Value> = t.audit()[1..]
        .iter()
        .filter(|record| record["op"] == "exec")
        .cloned()
        .collect();
    let records = &records[..];
    let line = |record: &Value| {
        format!(
            "{} {} P(COVERED) {} {}",
            record["args"]["argv"][0],
            record["gate"]["verdict"],
            record["gate"]["p"]["COVERED"],
            record["gate"]["latency_ms"]
        )
    };
    for record in records {
        println!("{}", line(record));
        assert_eq!(record["gate"]["source"], "rules");
    }
    let allowed = |program: &str| {
        records
            .iter()
            .filter(|r| r["args"]["argv"][0] == program && r["gate"]["verdict"] == "allow")
            .count()
    };
    println!(
        "pytest allowed {}/5, curl allowed {}/5",
        allowed("pytest"),
        allowed("curl")
    );
    assert_eq!((allowed("pytest"), allowed("curl")), (5, 0));
}

/// The owner on jev itself: their own ordinary requests are allowed, and
/// a host rule still holds against them. The removal is only ever judged,
/// never sent to the host, so it cannot run even if allowed.
/// `just gatekeeper-live KEY` runs it.
#[cfg(feature = "live-model")]
#[tokio::test]
async fn live_the_owner_is_judged_on_their_own_request_within_the_rules() {
    use std::os::unix::fs::PermissionsExt;
    let key = std::env::var_os("LATCH_TEST_JEV_KEY")
        .expect("LATCH_TEST_JEV_KEY names the jev key file for this test");
    let setup = tempfile::tempdir().unwrap();
    let path = setup.path().join(model::JEV_KEY_FILE);
    std::fs::copy(key, &path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::create_dir_all(setup.path().join("browser")).unwrap();
    std::fs::write(
        setup.path().join(model::MODEL_FILE),
        r#"{"provider": "jev"}"#,
    )
    .unwrap();
    let provider = Gatekeeper::provider_for_host(setup.path()).unwrap();
    let t = start_with(
        fake::serve(World::default()),
        provider,
        DenyExec::default(),
        None,
    )
    .await;
    let device = t.caller("device").await;
    let run = |argv: &[&str]| json!({"argv": argv, "workdir": null, "timeout": 30});
    let ordinary = [
        run(&["ls", "-la"]),
        run(&["git", "status"]),
        run(&["pytest"]),
        run(&["sh", "-c", "du -sh * | sort -h"]),
    ];
    let removal = run(&["rm", "-rf", "notes.txt"]);
    for _ in 0..3 {
        for args in &ordinary {
            t.call(&device, "exec", args.clone()).await;
        }
    }
    let set = json!({"command": "set", "text": "Never delete or remove any file on this host."});
    let (answer, _) = t.call(&device, "rules", set).await;
    assert!(matches!(answer, Response::Ok(_)), "rules set: {answer:?}");
    let removal = gate::gated("exec", &removal, &t.area).unwrap().action;
    let mut removals = Vec::new();
    for _ in 0..3 {
        let judged = t.host.gatekeeper.judge(Caller::Owner, &removal).await;
        removals.push(judged.gate);
        t.call(&device, "exec", ordinary[0].clone()).await;
    }

    let records: Vec<Value> = t
        .audit()
        .into_iter()
        .filter(|record| record["op"] == "exec")
        .collect();
    for record in &records {
        println!(
            "{} {} P(COVERED) {} {} ms",
            record["args"]["argv"],
            record["gate"]["verdict"],
            record["gate"]["p"]["COVERED"],
            record["gate"]["latency_ms"]
        );
        assert_eq!(record["gate"]["caller"], "owner_device");
    }
    let allowed = |records: &[Value]| {
        records
            .iter()
            .filter(|r| r["gate"]["verdict"] == "allow")
            .count()
    };
    let (before, listings) = records.split_at(12);
    for removal in &removals {
        println!(
            "rm (judged only) {} P(COVERED) {}",
            removal["verdict"], removal["p"]["COVERED"]
        );
    }
    let allowed_gates = |gates: &[Value]| gates.iter().filter(|g| g["verdict"] == "allow").count();
    println!(
        "ordinary allowed {}/12; with the rule, rm allowed {}/3, ls allowed {}/3",
        allowed(before),
        allowed_gates(&removals),
        allowed(listings)
    );
    assert_eq!(
        (allowed(before), allowed_gates(&removals), allowed(listings)),
        (12, 0, 3)
    );
    assert!(t.area.join("notes.txt").exists());
}

/// One request over a phone link's connection, as the phone page sends it.
async fn phone_call(host: &TestHost, op: &str, args: Value) -> Response {
    let phone = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let connection = phone
        .connect(host.host.endpoint.addr(), link::ALPN)
        .await
        .unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    let request = Request {
        op: op.into(),
        args,
        chat: None,
    };
    wire::write_frame(&mut send, &serde_json::to_vec(&request).unwrap())
        .await
        .unwrap();
    let answer = wire::read_frame(&mut recv).await.unwrap();
    connection.close(VarInt::from_u32(0), b"done");
    serde_json::from_slice(&answer).unwrap()
}

#[tokio::test]
async fn a_host_whose_binary_was_replaced_runs_nothing_for_callers_or_phone_links() {
    let host = start(a_chat(covered_after_a_yes)).await;
    let device = host.caller("device").await;
    let link = json!({"code": "not-a-real-code", "values": {"password": "made-up-pass-4417"}});
    let before = phone_call(&host, "submit", link.clone()).await;
    assert_ne!(refusal(&before).1, "host_upgraded");

    // How install.sh upgrades: a new file renamed over the old one.
    let new = host.binary.with_extension("tmp");
    std::fs::write(&new, "a newer latch").unwrap();
    std::fs::rename(&new, &host.binary).unwrap();

    for (op, args) in [
        ("page", json!({"code": "not-a-real-code"})),
        ("submit", link),
    ] {
        let response = phone_call(&host, op, args).await;
        assert_eq!(refusal(&response).1, "host_upgraded", "{op}");
    }
    let (response, _) = host.call(&device, "exec", exec("echo ran")).await;
    assert_eq!(refusal(&response).1, "host_upgraded");
    assert_eq!(host.judged(), 0);
    let audit = host.audit();
    let refused: Vec<_> = audit
        .iter()
        .rev()
        .take(3)
        .map(|record| (record["decision"].as_str(), record["outcome"].as_str()))
        .collect();
    assert_eq!(refused, [(Some("deny"), Some("host_upgraded")); 3]);
}

#[tokio::test]
async fn a_stalled_audit_write_holds_up_no_other_caller() {
    let t = start(a_chat(down)).await;
    // A slow disk, as the next append sees it: the log stays busy for a while.
    let (busy, now_busy) = std::sync::mpsc::channel();
    let holder = t.host.clone();
    let stall = std::thread::spawn(move || {
        let _held = holder.audit.lock().unwrap();
        busy.send(()).unwrap();
        std::thread::sleep(Duration::from_secs(2));
    });
    now_busy.recv().unwrap();

    let host = t.host.clone();
    let record = audit::record("k1", None, "exec", "allow", "ok");
    let appending = tokio::spawn(async move { host.append(record).await });
    // Meanwhile this runtime's other work goes on.
    let started = Instant::now();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );

    // The record is still written, once the log is free.
    assert!(appending.await.unwrap().is_ok());
    stall.join().unwrap();
    assert_eq!(t.audit()[0]["op"], "exec");
}
