//! A host's gatekeeper in front of exec, files and Google, and the owner's
//! device rule for it, end to end: a headless host on this machine with
//! real sandboxed workers, callers over iroh, and a synthetic Plow and
//! thread reader standing in for the owner's messages. No real message is
//! ever read.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;
use crate::failure::Exit;
use crate::gate::fake::{self, AGENT, FakePlow, World};
use crate::model;

pub(super) fn always_covered(_: &Value) -> Option<[f64; 3]> {
    Some([0.9, 0.08, 0.02])
}

/// The cited intent never covers the action; no device rule forbids it.
fn never_covered(_: &Value) -> Option<[f64; 3]> {
    Some([0.1, 0.85, 0.05])
}

fn down(_: &Value) -> Option<[f64; 3]> {
    None
}

pub(super) const ASKED: &str = "could you tidy up the work folder at some point?";

/// A synthetic Plow whose decision route answers by `decide`.
fn judged_by(decide: fake::Decide) -> World {
    World {
        decide: Some(decide),
        ..World::default()
    }
}

pub(super) struct TestHost {
    pub(super) host: Arc<Host>,
    pub(super) plow: FakePlow,
    pub(super) area: PathBuf,
    pub(super) state: PathBuf,
    /// The owner's home folder, as `messages` and `contacts` read it (macOS).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    home: PathBuf,
    /// What the host watches as its own binary: a stand-in, so a test can
    /// replace it without touching the workers' real one.
    binary: PathBuf,
    _dir: tempfile::TempDir,
}

/// The latch binary cargo builds beside this test binary: the workers'.
pub(super) fn latch_binary() -> PathBuf {
    let deps = std::env::current_exe().unwrap();
    deps.parent().unwrap().parent().unwrap().join("latch")
}

pub(super) async fn start(world: World) -> TestHost {
    start_denying(world, DenyExec::default()).await
}

/// A host whose owner denies exec as `deny_exec` says.
pub(super) async fn start_denying(world: World, deny_exec: DenyExec) -> TestHost {
    let fake = fake::serve(world);
    let provider = model::Plow::new(&fake.url, "made-up-login".into()).with_model(gate::MODEL);
    start_with(fake, Arc::new(provider), deny_exec).await
}

/// A host on `plow`, its gatekeeper asking `provider`, every citation
/// the owner asking `ASKED`.
pub(super) async fn start_with(
    plow: FakePlow,
    provider: Arc<dyn model::Provider>,
    deny_exec: DenyExec,
) -> TestHost {
    start_reading(plow, provider, deny_exec, fake::Reader::asked(ASKED)).await
}

/// The same, its citations read by `reader`.
pub(super) async fn start_reading(
    plow: FakePlow,
    provider: Arc<dyn model::Provider>,
    deny_exec: DenyExec,
    reader: fake::Reader,
) -> TestHost {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
    std::fs::create_dir_all(&parent).unwrap();
    let dir = tempfile::tempdir_in(parent).unwrap();
    let root = dir.path().canonicalize().unwrap();
    let home = root.join("home");
    let (area, state) = (home.join("area"), root.join("state"));
    std::fs::create_dir_all(&area).unwrap();
    crate::state::StateDir {
        path: state.clone(),
    }
    .create()
    .unwrap();
    std::fs::write(area.join("notes.txt"), "the notes' own words").unwrap();
    let binary = root.join("latch");
    std::fs::write(&binary, "the latch this host started from").unwrap();
    // Its owner has turned its connectors on.
    let enabled = if cfg!(target_os = "macos") {
        r#"{"google": {"state": "enabled"}, "messages": {"state": "enabled"}, "contacts": {"state": "enabled"}}"#
    } else {
        r#"{"google": {"state": "enabled"}}"#
    };
    std::fs::write(state.join("plugins.json"), enabled).unwrap();
    let endpoint = crate::dial::endpoint()
        .relay_mode(RelayMode::Disabled)
        .alpns(vec![wire::ALPN.to_vec(), link::ALPN.to_vec()])
        .bind()
        .await
        .unwrap();
    let vault = crate::vault::host::Vault::new(state.clone());
    crate::gate::rules::Store::new(&state)
        .map_agent(AGENT, fake::AGENT_SWARM)
        .unwrap();
    let audit = Arc::new(Mutex::new(
        AuditLog::open(state.join(crate::audit::FILE)).unwrap(),
    ));
    let host = Arc::new(Host {
        plow: plow.client(),
        audit: audit.clone(),
        dashboard: Dashboard::new("test-host"),
        lifetime: super::Lifetime::new(false, false),
        workers: WorkerSetup {
            home: home.clone(),
            areas: vec![],
            reads: vec![],
            hidden: crate::sandbox::never_readable(&home),
            state_dir: state.clone(),
            program: latch_binary(),
        },
        browsers: crate::browser::host::Browsers::new(state.clone(), vault.clone(), true),
        vault,
        endpoint: endpoint.clone(),
        name: "test-host".into(),
        gatekeeper: Gatekeeper::new(provider, Box::new(reader), &state)
            .keeping_requests(&state)
            .recording(audit),
        deny_exec,
        binary: Binary::at(&binary).unwrap(),
        plugins: crate::plugins::Plugins::load(
            &crate::state::StateDir {
                path: state.clone(),
            },
            &home,
        )
        .unwrap(),
        home: home.clone(),
        stopping: tokio::sync::watch::Sender::new(Stopping::No),
        agent_names: Mutex::default(),
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
        home,
        binary,
        _dir: dir,
    }
}

impl TestHost {
    /// A caller Plow knows as an agent (with its uid) or a device.
    pub(super) async fn caller(&self, kind: &str) -> Endpoint {
        let client = crate::dial::endpoint()
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
    pub(super) async fn call(
        &self,
        client: &Endpoint,
        op: &str,
        args: Value,
    ) -> (Response, Vec<u8>) {
        self.request(
            client,
            Request {
                op: op.into(),
                args,
                intent: None,
            },
        )
        .await
    }

    /// The same, citing the owner's message `fake::CITED`, as an agent's
    /// every call but a read does.
    pub(super) async fn cite(
        &self,
        client: &Endpoint,
        op: &str,
        args: Value,
    ) -> (Response, Vec<u8>) {
        self.request(
            client,
            Request {
                op: op.into(),
                args,
                intent: Some(fake::CITED.into()),
            },
        )
        .await
    }

    async fn request(&self, client: &Endpoint, request: Request) -> (Response, Vec<u8>) {
        let connection = client
            .connect(self.host.endpoint.addr(), wire::ALPN)
            .await
            .unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        wire::write_frame(&mut send, &serde_json::to_vec(&request).unwrap())
            .await
            .unwrap();
        let answer = wire::read_frame(&mut recv).await.unwrap();
        let rest = recv.read_to_end(1 << 20).await.unwrap_or_default();
        connection.close(VarInt::from_u32(0), b"done");
        (serde_json::from_slice(&answer).unwrap(), rest)
    }

    /// Sends one operation over the host's private socket, as the owner at
    /// the host does, and returns its answer.
    pub(super) async fn local_call(&self, op: &str, args: Value) -> Response {
        self.local_request(Request {
            op: op.into(),
            args,
            intent: None,
        })
        .await
    }

    async fn local_request(&self, request: Request) -> Response {
        let (mine, hosts) = tokio::net::UnixStream::pair().unwrap();
        tokio::spawn(serve_local(self.host.clone(), hosts));
        let (mut recv, mut send) = mine.into_split();
        wire::write_frame(&mut send, &serde_json::to_vec(&request).unwrap())
            .await
            .unwrap();
        serde_json::from_slice(&wire::read_frame(&mut recv).await.unwrap()).unwrap()
    }

    /// The audit's operation records, opened with the key: connections
    /// and the gate records written before an allowed intent op left out.
    pub(super) fn audit(&self) -> Vec<Value> {
        self.audit_text()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|record: &Value| record["op"] != "connect" && record["op"] != "gate")
            .collect()
    }

    /// Every audit record, opened with the key, one JSON line each.
    fn audit_text(&self) -> String {
        crate::audit::read_all(&self.state.join(crate::audit::FILE))
            .unwrap()
            .iter()
            .map(|record| serde_json::to_string(record).unwrap() + "\n")
            .collect()
    }

    /// An agent's `file_put` of `sent` to `remote`, promising `size` bytes,
    /// citing `fake::CITED`: the first answer, and the final one if the
    /// host got that far.
    async fn put(
        &self,
        client: &Endpoint,
        remote: &str,
        size: usize,
        sent: &[u8],
    ) -> (Response, Option<Response>) {
        let connection = client
            .connect(self.host.endpoint.addr(), wire::ALPN)
            .await
            .unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let request = Request {
            op: "file_put".into(),
            args: json!({"remote_path": remote, "size": size, "name": "upload.txt"}),
            intent: Some(fake::CITED.into()),
        };
        wire::write_frame(&mut send, &serde_json::to_vec(&request).unwrap())
            .await
            .unwrap();
        let first: Response =
            serde_json::from_slice(&wire::read_frame(&mut recv).await.unwrap()).unwrap();
        if !matches!(first, Response::Ok(_)) {
            return (first, None);
        }
        send.write_all(sent).await.unwrap();
        send.finish().unwrap();
        let last = wire::read_frame(&mut recv)
            .await
            .ok()
            .map(|bytes| serde_json::from_slice(&bytes).unwrap());
        connection.close(VarInt::from_u32(0), b"done");
        (first, last)
    }

    /// How many judgments the decision route was asked for.
    pub(super) fn judged(&self) -> usize {
        self.judgments().len()
    }

    /// The state of each judgment, in order.
    pub(super) fn judgments(&self) -> Vec<Value> {
        self.plow.world.lock().unwrap().asked.clone()
    }
}

fn exec(script: &str) -> Value {
    json!({"argv": ["/bin/sh", "-c", script], "writes": ["area"], "timeout": 10})
}

pub(super) fn refusal(response: &Response) -> (Exit, &str, &Value) {
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
        tokio::join!(t.cite(&agent, "exec", exec("touch made")), during_lookup);
    assert!(requested.is_ok(), "the dashboard never asked for the name");
    assert_eq!(refusal(&answer).1, "exec_denied");
    assert!(!t.area.join("made").exists());
    assert_eq!(records.len(), 1, "the audit waited for the name lookup");
    assert_eq!(records[0]["outcome"], "exec_denied");
}

#[tokio::test]
async fn a_refused_operation_runs_nothing_and_returns_nothing() {
    let t = start(judged_by(never_covered)).await;
    let agent = t.caller("agent").await;

    let (answer, rest) = t
        .cite(&agent, "exec", exec("echo the-output; touch made"))
        .await;
    let (exit, code, details) = refusal(&answer);
    assert_eq!((exit, code), (Exit::Denied, "gatekeeper_denied"));
    assert_eq!(details["reason"], "not_covered");
    assert!(rest.is_empty());
    assert!(!t.area.join("made").exists(), "the program never ran");

    let put = json!({"remote_path": "area/new.txt", "size": 5, "name": "new.txt"});
    let (answer, _) = t.cite(&agent, "file_put", put).await;
    assert_eq!(refusal(&answer).1, "gatekeeper_denied");
    assert!(!t.area.join("new.txt").exists());

    let google = json!({"gog_args": ["gmail", "send", "--to", "a@example.com", "--body", "hi"],
                        "account": [], "confirm_conflict": false, "inputs": []});
    let (answer, rest) = t.cite(&agent, "google", google).await;
    assert_eq!(refusal(&answer).1, "gatekeeper_denied");
    assert!(rest.is_empty());
    assert_eq!(
        t.plow.saw("access-token"),
        0,
        "no Google credential fetched"
    );

    assert_eq!(t.judged(), 3);
    let records = t.audit();
    assert_eq!(records.len(), 3, "and no gate record");
    for record in &records {
        assert_eq!(record["decision"], "deny");
        assert_eq!(record["outcome"], "gatekeeper_denied");
        assert_eq!(record["gate"]["verdict"], "not_covered");
    }
    assert_eq!(records[2]["args"], json!({"command": "gmail.send"}));
}

#[tokio::test]
async fn a_judge_that_cannot_answer_runs_nothing_but_reads() {
    let t = start(judged_by(down)).await;
    let agent = t.caller("agent").await;
    let (answer, _) = t.cite(&agent, "exec", exec("touch made")).await;
    let (exit, code, _) = refusal(&answer);
    assert_eq!((exit, code), (Exit::Unreachable, "gatekeeper_unavailable"));
    assert!(!t.area.join("made").exists());
    let put = json!({"remote_path": "area/new.txt", "size": 5, "name": "new.txt"});
    let (answer, _) = t.cite(&agent, "file_put", put).await;
    assert_eq!(refusal(&answer).1, "gatekeeper_unavailable");
    assert!(!t.area.join("new.txt").exists());
    let records = t.audit();
    assert!(records.iter().all(|r| r["decision"] == "deny"));
    // A read asks no judge, so it runs.
    let (answer, bytes) = t
        .call(&agent, "file_get", json!({"remote_path": "area/notes.txt"}))
        .await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    assert!(bytes.starts_with(b"the notes' own words"));
}

#[tokio::test]
async fn the_answer_does_not_wait_for_the_dashboard_name_lookup() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let released = Arc::new(AtomicBool::new(false));
    let world = World {
        paused: Some((format!("/v1/agents/{AGENT}"), released.clone())),
        ..World::default()
    };
    let plow = fake::serve(world);
    let provider = model::Plow::new(&plow.url, "made-up-login".into()).with_model(gate::MODEL);
    let t = start_with(
        plow,
        Arc::new(provider),
        DenyExec::from_flags(&[crate::cli::EVERY_AGENT.to_string()]),
    )
    .await;
    let agent = t.caller("agent").await;
    let call = t.cite(&agent, "exec", exec("touch made"));
    let answered = tokio::time::timeout(Duration::from_secs(10), call).await;
    released.store(true, Ordering::SeqCst);
    let (answer, _) = answered.expect("the answer waited for the name lookup");
    assert_eq!(refusal(&answer).1, "exec_denied");
}

#[tokio::test]
async fn an_agents_op_runs_only_on_a_cited_intent_and_every_call_is_judged() {
    let t = start(judged_by(always_covered)).await;
    let agent = t.caller("agent").await;
    let tidy = exec("echo tidied >> made");
    // Citing nothing, it is refused before any judge is asked.
    let (answer, _) = t.call(&agent, "exec", tidy.clone()).await;
    let (exit, code, details) = refusal(&answer);
    assert_eq!((exit, code), (Exit::Denied, "gatekeeper_denied"));
    assert_eq!(details["reason"], "intent_required");
    let Response::Error { next, .. } = &answer else {
        unreachable!()
    };
    let next = next.as_deref().unwrap();
    assert!(
        next.contains("update latch") && next.contains("--intent"),
        "{next}"
    );
    assert!(!t.area.join("made").exists());
    assert_eq!(t.judged(), 0);

    // Citing the owner's message, it runs, judged each time.
    for _ in 0..2 {
        let (answer, _) = t.cite(&agent, "exec", tidy.clone()).await;
        let Response::Ok(result) = answer else {
            panic!("refused: {answer:?}");
        };
        assert_eq!(result["exit_code"], 0);
    }
    assert_eq!(
        std::fs::read_to_string(t.area.join("made")).unwrap(),
        "tidied\ntidied\n"
    );
    assert_eq!(t.judged(), 2, "the same exec is judged each time");
    assert_eq!(t.judgments()[0]["cited_message"]["text"], ASKED);

    // Each allowed op's gate record is written before its own, which names it.
    let all: Vec<crate::audit::Record> = crate::audit::read_all(&t.state.join(crate::audit::FILE))
        .unwrap()
        .into_iter()
        .filter(|record| record.op != "connect")
        .collect();
    let ops: Vec<&str> = all.iter().map(|r| r.op.as_str()).collect();
    assert_eq!(ops, ["exec", "gate", "exec", "gate", "exec"]);
    assert_eq!(all[0].gate.as_ref().unwrap()["verdict"], "intent_required");
    for pair in all[1..].chunks(2) {
        let (gate, op) = (&pair[0], &pair[1]);
        assert_eq!(op.gate.as_ref().unwrap()["gate_record"], gate.hash);
        assert_eq!(gate.gate.as_ref().unwrap()["verdict"], "allow");
        assert_eq!(gate.agent.as_deref(), Some(AGENT));
    }
    let all: Vec<Value> = all
        .iter()
        .map(|record| serde_json::to_value(record).unwrap())
        .collect();
    let gate = &all[4]["gate"];
    assert_eq!(gate["verdict"], "allow");
    assert_eq!(gate["class"], "intent");
    assert_eq!(gate["intent"], format!("swarm:{}", fake::CITED));
    assert_eq!(gate["model"], gate::MODEL);
    assert_eq!(gate["model_request"], "req_fake");
    assert!(
        (gate["cost"].as_f64().unwrap() - 0.0001).abs() < 1e-12,
        "{gate}"
    );
    assert!(gate["p"]["covered"].as_f64().unwrap() >= 0.5);
    assert!(gate["latency_ms"].is_u64(), "{gate}");
    // Both caller checks are timed: on connect, and before the operation.
    assert!(all[4]["timings"]["caller_check_ms"].is_u64());
    let connects: Vec<Value> = t
        .audit_text()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["op"] == "connect")
        .collect();
    assert!(!connects.is_empty());
    for connect in &connects {
        assert!(connect["timings"]["caller_check_ms"].is_u64(), "{connect}");
    }
    assert_eq!(all[4]["args"]["argv"][2], "echo tidied >> made");
    assert!(
        !t.audit_text().contains(ASKED),
        "the owner's words in the audit"
    );
}

fn replace_the_judged_write_folder(state: &Value) -> Option<[f64; 3]> {
    let path = Path::new(state["proposed_action"]["writes"][0].as_str().unwrap());
    // Swapped at the judgment, so the folder judged is not the one written.
    if path.is_symlink() {
        return always_covered(state);
    }
    std::fs::rename(path, path.with_file_name("approved-before-swap")).unwrap();
    let home = path.parent().unwrap().parent().unwrap();
    std::os::unix::fs::symlink(home.join("outside-write"), path).unwrap();
    always_covered(state)
}

#[tokio::test]
async fn a_write_path_retargeted_after_judgment_is_refused() {
    let t = start(judged_by(replace_the_judged_write_folder)).await;
    let approved = t.area.join("approved");
    let outside = t.host.workers.home.join("outside-write");
    std::fs::create_dir(&approved).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("victim.txt"), "unchanged").unwrap();
    let agent = t.caller("agent").await;
    let args =
        json!({"argv": ["/bin/sh", "-c", "printf overwritten > victim.txt"], "writes": [approved]});
    let (answer, _) = t.cite(&agent, "exec", args).await;
    assert_eq!(
        std::fs::read(outside.join("victim.txt")).unwrap(),
        b"unchanged"
    );
    assert_eq!(refusal(&answer).1, "bad_path");
}

#[tokio::test]
async fn unusual_and_missing_caches_do_not_fail_exec_or_grant_aliases() {
    let t = start(judged_by(always_covered)).await;
    let home = &t.host.workers.home;
    std::fs::write(home.join(".cache"), "plain file").unwrap();
    let target = home.join("outside-write");
    std::fs::create_dir(&target).unwrap();
    std::os::unix::fs::symlink(&target, home.join(".npm")).unwrap();
    let agent = t.caller("agent").await;
    let (answer, _) = t
        .cite(
            &agent,
            "exec",
            json!({
                "argv": ["/bin/sh", "-c", "echo bad > outside-write/victim"]
            }),
        )
        .await;
    assert!(
        !target.join("victim").exists(),
        "cache granted an undeclared path"
    );
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    assert_eq!(std::fs::read(home.join(".cache")).unwrap(), b"plain file");
    assert!(!home.join(".cargo").exists());
    assert!(!home.join("go").exists());
}

#[tokio::test]
async fn a_repeated_google_change_is_judged_again_with_what_was_done_and_a_read_never() {
    let t = start(judged_by(always_covered)).await;
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
    for args in [&search, &send, &send] {
        t.cite(&agent, "google", args.clone()).await;
    }
    let asked = t.judgments();
    assert_eq!(asked.len(), 2, "each send judged, the search not at all");
    assert_eq!(
        asked[0]["proposed_action"], asked[1]["proposed_action"],
        "the second send is judged as the first"
    );
    assert_eq!(asked[1]["proposed_action"]["command"], send["gog_args"]);
    // The second is judged knowing the first was done on the same intent.
    assert_eq!(asked[0]["already_done"], json!([]));
    assert_eq!(asked[1]["already_done"].as_array().unwrap().len(), 1);
    // A read with a flag the grammar lacks runs nothing, so it is refused as
    // it is, never judged as a change needing the owner's word.
    let (answer, _) = t
        .cite(&agent, "google", google(&["gmail", "get", "m1", "--full"]))
        .await;
    assert_eq!(refusal(&answer).1, "not_allowed");
    assert_eq!(t.judgments().len(), 2);
}

#[tokio::test]
async fn a_put_is_judged_at_its_resolved_path_and_a_short_one_changes_nothing() {
    let t = start(judged_by(always_covered)).await;
    let agent = t.caller("agent").await;
    let notes = t.area.join("notes.txt");
    // A folder destination takes the file's name, and the judge is shown
    // the path it resolves to, not what the agent typed.
    let (_, last) = t.put(&agent, "area", 2, b"hi").await;
    assert!(matches!(last, Some(Response::Ok(_))), "{last:?}");
    assert_eq!(std::fs::read(t.area.join("upload.txt")).unwrap(), b"hi");
    let asked = t.judgments();
    let shown = t.area.join("upload.txt").display().to_string();
    assert!(asked[0].to_string().contains(&shown), "{}", asked[0]);
    // An upload that breaks off leaves the old file and nothing beside it.
    let (_, last) = t.put(&agent, "area/notes.txt", 10, b"new").await;
    assert!(!matches!(last, Some(Response::Ok(_))), "{last:?}");
    assert_eq!(std::fs::read(&notes).unwrap(), b"the notes' own words");
    let names = std::fs::read_dir(&t.area).unwrap().count();
    assert_eq!(names, 2, "only notes.txt and upload.txt");
    // A hidden destination is refused before anything is judged.
    let key = t.state.join("key");
    let (first, _) = t.put(&agent, key.to_str().unwrap(), 1, b"x").await;
    let (exit, code, _) = refusal(&first);
    assert_eq!((exit, code), (Exit::Denied, "bad_path"));
    assert_eq!(t.judged(), 2);
}

#[tokio::test]
async fn a_get_reads_any_readable_path_in_home_and_never_a_hidden_one() {
    let t = start(judged_by(always_covered)).await;
    let agent = t.caller("agent").await;
    // A dotfile outside any shared folder: readable, and a read is not judged.
    let home = &t.host.workers.home;
    std::fs::create_dir(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh/config"), "Host example").unwrap();
    let (answer, bytes) = t
        .call(&agent, "file_get", json!({"remote_path": "~/.ssh/config"}))
        .await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    assert!(bytes.starts_with(b"Host example"), "{bytes:?}");
    // The host's state, a link into it, and the owner's Swarm sign-in are
    // refused.
    std::fs::write(t.state.join("key"), "made-up key").unwrap();
    std::os::unix::fs::symlink(t.state.join("key"), home.join("key-link")).unwrap();
    std::fs::create_dir_all(home.join(".swarm/owner/sam")).unwrap();
    std::fs::write(home.join(".swarm/owner/sam/backup_key"), "made-up key").unwrap();
    let key = t.state.join("key");
    for hidden in [
        key.to_str().unwrap(),
        "~/key-link",
        "~/.swarm/owner/sam/backup_key",
    ] {
        let (answer, _) = t
            .call(&agent, "file_get", json!({"remote_path": hidden}))
            .await;
        let (exit, code, _) = refusal(&answer);
        assert_eq!((exit, code), (Exit::Denied, "bad_path"), "{hidden}");
    }
    assert_eq!(t.judged(), 0);
}

#[tokio::test]
async fn an_exec_the_host_denies_is_refused_without_asking_the_judge() {
    let every_agent = DenyExec::from_flags(&[crate::cli::EVERY_AGENT.to_string()]);
    let t = start_denying(judged_by(always_covered), every_agent).await;
    let agent = t.caller("agent").await;
    let (answer, _) = t.cite(&agent, "exec", exec("touch made")).await;
    let (exit, code, _) = refusal(&answer);
    assert_eq!((exit, code), (Exit::Denied, "exec_denied"));
    assert!(!t.area.join("made").exists());
    assert_eq!(t.judged(), 0);
    assert!(t.audit()[0].get("gate").is_none());
    // Other operations are still judged.
    let (_, last) = t.put(&agent, "area/new.txt", 2, b"hi").await;
    assert!(matches!(last, Some(Response::Ok(_))), "{last:?}");
    assert_eq!(t.judged(), 1);
}

/// An exec of `/bin/sh` reading its script from `stdin`.
fn exec_stdin(stdin: &[u8]) -> Value {
    json!({"argv": ["/bin/sh"], "writes": ["area"], "timeout": 10, "stdin": stdin})
}

#[tokio::test]
async fn exec_paths_are_validated_before_judgment_and_created_only_after_allow() {
    let t = start(judged_by(never_covered)).await;
    let agent = t.caller("agent").await;
    for path in ["~", "~/..", "/", "~/.latch/new", t.state.to_str().unwrap()] {
        let args = json!({"argv": ["true"], "writes": [path]});
        let (answer, _) = t.cite(&agent, "exec", args).await;
        assert_eq!(refusal(&answer).1, "bad_path", "{path}");
        assert_eq!(refusal(&answer).2["writes"], json!([path]));
        assert_eq!(refusal(&answer).2["network"], false);
    }
    assert_eq!(t.judged(), 0);
    let path = t.host.workers.home.join("new-project");
    let args = json!({"argv": ["/bin/sh", "-c", "printf built > result"],
                      "writes": ["~/new-project"], "network": true});
    let (refused, _) = t.cite(&agent, "exec", args.clone()).await;
    assert_eq!(refusal(&refused).1, "gatekeeper_denied");
    assert!(!path.exists());
    t.plow.world.lock().unwrap().decide = Some(always_covered);
    let (answer, _) = t.cite(&agent, "exec", args).await;
    let Response::Ok(result) = answer else {
        panic!("{answer:?}")
    };
    assert_eq!(result["exit_code"], 0, "{result}");
    assert_eq!(std::fs::read(path.join("result")).unwrap(), b"built");
    let asked = t.judgments();
    for state in asked {
        assert_eq!(state["proposed_action"]["writes"], json!([path]));
        assert_eq!(state["proposed_action"]["working_folder"], json!(path));
        assert_eq!(state["proposed_action"]["network"], true);
    }
    for record in t
        .audit()
        .into_iter()
        .filter(|record| record["gate"].is_object())
    {
        assert_eq!(record["args"]["writes"], json!([path]));
        assert_eq!(record["args"]["network"], true);
    }
}

#[tokio::test]
async fn exec_reads_home_but_writes_only_declarations_and_caches() {
    let t = start(judged_by(always_covered)).await;
    let home = &t.host.workers.home;
    std::fs::create_dir(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh/config"), "readable-dotfile").unwrap();
    std::fs::create_dir(home.join(".latch")).unwrap();
    std::fs::write(home.join(".latch/private"), "hidden-private-value").unwrap();
    std::fs::create_dir_all(home.join(".swarm/owner/sam")).unwrap();
    std::fs::write(
        home.join(".swarm/owner/sam/credential.json"),
        "hidden-swarm-value",
    )
    .unwrap();
    std::fs::create_dir(t.area.join("peer")).unwrap();
    std::fs::write(
        t.area.join("peer/latch.json"),
        r#"{"format":1,"written_by":"test"}"#,
    )
    .unwrap();
    std::fs::write(t.area.join("peer/private"), "hidden-peer-value").unwrap();
    for cache in [
        ".cache",
        ".npm",
        ".cargo/registry",
        ".cargo/git",
        "go/pkg/mod",
    ] {
        std::fs::create_dir_all(home.join(cache)).unwrap();
    }
    let agent = t.caller("agent").await;
    let script = r#"
        cat .ssh/config
        cat .latch/private 2>/dev/null
        cat .swarm/owner/sam/credential.json 2>/dev/null
        cat area/peer/private 2>/dev/null
        echo bad > undeclared 2>/dev/null
        for p in .cache .npm .cargo/registry .cargo/git go/pkg/mod; do
            echo cache > "$p/probe" || exit 9
        done
        echo scratch > "$TMPDIR/probe"
    "#;
    let (answer, _) = t
        .cite(&agent, "exec", json!({"argv": ["/bin/sh", "-c", script]}))
        .await;
    let Response::Ok(result) = answer else {
        panic!("{answer:?}")
    };
    assert_eq!(result["exit_code"], 0, "{result}");
    assert!(
        result["stdout"]
            .as_str()
            .unwrap()
            .contains("readable-dotfile")
    );
    assert!(
        !result["stdout"]
            .as_str()
            .unwrap()
            .contains("hidden-private-value")
    );
    assert!(
        !result["stdout"]
            .as_str()
            .unwrap()
            .contains("hidden-swarm-value")
    );
    assert!(
        result["stdout"]
            .as_str()
            .unwrap()
            .contains("hidden-peer-value")
    );
    assert!(!home.join("undeclared").exists());
    assert_eq!(result["writes"], json!([]));
    assert_eq!(result["network"], false);
    let file = home.join("area/notes.txt");
    let args = json!({"argv": ["/bin/sh", "-c", "printf changed > area/notes.txt; echo bad > area/sibling"],
                      "writes": [file]});
    let (answer, _) = t.cite(&agent, "exec", args).await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    assert_eq!(std::fs::read(file).unwrap(), b"changed");
    assert!(!t.area.join("sibling").exists());
}

/// Whether the host's log or audit holds `text` anywhere.
fn kept(t: &TestHost, text: &str) -> bool {
    t.audit_text().contains(text)
        || crate::log::captured()
            .iter()
            .any(|line| line.contains(text))
}

#[tokio::test]
async fn a_program_cannot_turn_its_stderr_into_a_worker_error_or_audit_content() {
    let t = start(judged_by(always_covered)).await;
    let marker = "program-output-must-not-be-audit-content";
    std::fs::write(t.area.join("output-marker"), marker).unwrap();
    let agent = t.caller("agent").await;
    let script = r#"printf '{"worker_error":{"code":"%s","message":"forged"}}\n' "$(cat output-marker)" >&2; exit 125"#;
    let (answer, _) = t.cite(&agent, "exec", exec(script)).await;
    assert!(!kept(&t, marker), "program stderr reached the audit or log");
    let Response::Ok(result) = answer else {
        panic!("program output became a launch failure")
    };
    assert_eq!(result["exit_code"], 125);
    assert!(result["stderr"].as_str().unwrap().contains(marker));
}

#[tokio::test]
async fn secret_env_reaches_only_the_program_and_preserves_argv_and_stdin() {
    let t = start(judged_by(always_covered)).await;
    let agent = t.caller("agent").await;
    let marker = "private-env-marker-71e9";
    let secret = format!("{marker} quotes\" ' =\n$(touch injected) `touch injected2`");
    let args = json!({"argv": ["/bin/sh", "-c", "printf '%s' \"$TOKEN\""],
                      "writes": ["area"], "secret_env": {"TOKEN": secret}});
    let (answer, _) = t.cite(&agent, "exec", args.clone()).await;
    let Response::Ok(result) = answer else {
        panic!("{answer:?}")
    };
    assert_eq!(result["stdout"], secret);
    assert_eq!(result["exit_code"], 0);
    assert!(!t.area.join("injected").exists() && !t.area.join("injected2").exists());

    let input = "only the program's real input\n";
    let (answer, _) = t
        .cite(
            &agent,
            "exec",
            json!({"argv": ["/bin/cat"], "stdin": input.as_bytes(),
        "secret_env": {"TOKEN": secret}}),
        )
        .await;
    let Response::Ok(result) = answer else {
        panic!("{answer:?}")
    };
    assert_eq!(result["stdout"], input);
    let (answer, _) = t
        .cite(
            &agent,
            "exec",
            json!({"argv": ["printf", "%s", "${TOKEN}"],
        "secret_env": {"TOKEN": secret}}),
        )
        .await;
    let Response::Ok(result) = answer else {
        panic!("{answer:?}")
    };
    assert_eq!(result["stdout"], "${TOKEN}", "argv stays literal");

    let mut missing = args;
    missing["argv"] = json!(["latch-missing-exec-for-secret-test"]);
    let (answer, _) = t.cite(&agent, "exec", missing).await;
    assert_eq!(refusal(&answer).1, "program_not_found");
    assert!(!serde_json::to_string(&answer).unwrap().contains(marker));
    let asked = t.judgments();
    assert!(!serde_json::to_string(&asked).unwrap().contains(marker));
    assert_eq!(
        asked[0]["proposed_action"]["secret_env"],
        json!([{"name": "TOKEN", "secret": true, "length": secret.len()}])
    );
    assert!(!kept(&t, marker));
    let board = serde_json::to_string(&*t.host.dashboard.watch().borrow()).unwrap();
    assert!(!board.contains(marker), "{board}");
}

#[tokio::test]
async fn secret_names_that_control_execution_are_allowed_and_explained_to_the_judge() {
    let t = start(judged_by(always_covered)).await;
    let agent = t.caller("agent").await;
    let marker = "hidden-execution-value-742e";
    let names = [
        "GIT_SSH_COMMAND",
        "NODE_OPTIONS",
        "BASH_ENV",
        "PYTHONSTARTUP",
        "GIT_CONFIG_PARAMETERS",
    ];
    let secrets: serde_json::Map<String, Value> = names
        .into_iter()
        .map(|name| (name.to_string(), json!(marker)))
        .collect();
    // true does not interpret these values. This checks judgment, not a payload.
    let (answer, _) = t
        .cite(
            &agent,
            "exec",
            json!({
                "argv": ["true"], "secret_env": secrets
            }),
        )
        .await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    let asked = t.judgments();
    let state = &asked[0];
    assert!(!serde_json::to_string(state).unwrap().contains(marker));
    for name in names {
        assert!(
            state["proposed_action"]["secret_env"]
                .as_array()
                .unwrap()
                .iter()
                .any(|secret| secret["name"] == name && secret["secret"] == true)
        );
    }
    let rules = state["rules_of_judgment"]
        .as_array()
        .unwrap()
        .iter()
        .map(|rule| rule.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    for name in [
        "*_COMMAND",
        "NODE_OPTIONS",
        "BASH_ENV",
        "PYTHONSTARTUP",
        "GIT_CONFIG_*",
    ] {
        assert!(rules.contains(name), "judge was not warned about {name}");
    }
}

#[tokio::test]
async fn invalid_or_reserved_secret_names_never_reach_the_judge() {
    let t = start(judged_by(always_covered)).await;
    let agent = t.caller("agent").await;
    let marker = "invalid-env-private-marker-993f";
    for name in [
        "",
        "3TOKEN",
        "BAD-NAME",
        "HOME",
        "PATH",
        "TMPDIR",
        "LATCH_BWRAP_INNER",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "DYLD_INSERT_LIBRARIES",
    ] {
        let args = json!({"argv": ["true"], "secret_env": {(name): marker}});
        let (answer, _) = t.cite(&agent, "exec", args).await;
        assert_eq!(refusal(&answer).1, "bad_secret_env", "{name}");
        assert!(!serde_json::to_string(&answer).unwrap().contains(marker));
    }
    assert_eq!(t.judged(), 0);
    assert!(!kept(&t, marker));
}

#[tokio::test]
async fn an_agents_exec_stdin_is_judged_with_its_argv_and_never_kept() {
    let t = start(judged_by(always_covered)).await;
    let agent = t.caller("agent").await;
    let script = "touch made # stdin-judged-7f3a";
    let (answer, _) = t.cite(&agent, "exec", exec_stdin(script.as_bytes())).await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    assert!(t.area.join("made").exists());
    let asked = t.judgments();
    assert_eq!(asked[0]["proposed_action"]["argv"], json!(["/bin/sh"]));
    assert_eq!(asked[0]["proposed_action"]["stdin"], script);
    assert_eq!(t.audit()[0]["args"]["stdin"]["size"], script.len());
    assert!(!kept(&t, "stdin-judged-7f3a"));
}

#[tokio::test]
async fn an_agents_stdin_the_judge_cannot_read_runs_nothing() {
    let t = start(judged_by(always_covered)).await;
    let agent = t.caller("agent").await;
    let mut too_long = b"touch made\n".to_vec();
    too_long.resize(crate::gate::JUDGED_STDIN + 1, b'#');
    for stdin in [too_long, b"touch made\n\xff".to_vec()] {
        let (answer, _) = t.cite(&agent, "exec", exec_stdin(&stdin)).await;
        let (exit, code, _) = refusal(&answer);
        assert_eq!((exit, code), (Exit::Usage, "stdin_not_judgeable"));
        assert!(!t.area.join("made").exists());
    }
    assert_eq!(t.judged(), 0);
    let records = t.audit();
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|record| record["decision"] == "deny"));
    // The owner's stdin, judged against a device rule, has the same bounds.
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
    let t = start_denying(judged_by(always_covered), every_agent).await;
    let agent = t.caller("agent").await;
    let secret = b"echo denied-stdin-91c2";
    let (answer, _) = t.cite(&agent, "exec", exec_stdin(secret)).await;
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

#[tokio::test]
async fn owners_cannot_cite_an_intent() {
    let t = start(judged_by(always_covered)).await;
    let device = t.caller("device").await;
    for local in [false, true] {
        let request = Request {
            op: "browser".into(),
            args: json!({"command": "sessions"}),
            intent: Some(fake::CITED.into()),
        };
        let answer = if local {
            t.local_request(request).await
        } else {
            t.request(&device, request).await.0
        };
        let (exit, code, _) = refusal(&answer);
        assert_eq!(exit, Exit::Usage, "{answer:?}");
        assert_eq!(code, "intent_not_supported", "{answer:?}");
    }
    assert_eq!(t.judged(), 0);
}

#[tokio::test]
async fn the_owners_own_devices_are_judged_as_the_owner_against_the_device_rule() {
    let t = start(judged_by(never_covered)).await;
    let device = t.caller("device").await;
    let google = json!({"gog_args": ["gmail", "send", "--to", "a@example.com", "--body", "hi"],
                        "account": [], "confirm_conflict": false, "inputs": []});
    // With no device rule, the owner's calls run unjudged.
    let (answer, _) = t.call(&device, "exec", exec("touch made")).await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    assert!(t.area.join("made").exists());
    let put = json!({"remote_path": "area/new.txt", "size": 0, "name": "new.txt"});
    t.call(&device, "file_put", put).await;
    t.call(&device, "google", google.clone()).await;
    assert_eq!(t.judged(), 0);
    // The browser keeps its gate with its arguments.
    let open = json!({"command": "open", "url": "https://example.com/"});
    t.call(&device, "browser", open).await;
    let browsed = t.audit().pop().unwrap();
    assert_eq!(browsed["op"], "browser");
    assert_eq!(
        browsed["args"]["gate"]["caller"], "owner_device",
        "{browsed}"
    );

    // With one, each is judged against it alone, as the owner's request.
    crate::gate::rules::Store::new(&t.state)
        .set_deny("Never send email.", None)
        .unwrap();
    let (answer, _) = t.call(&device, "exec", exec("true")).await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    t.call(&device, "google", google).await;
    assert_eq!(t.judged(), 2, "exec and google");
    for state in &t.judgments() {
        assert_eq!(state["device_rule"], "Never send email.");
        assert!(state.get("cited_message").is_none(), "{state}");
        assert!(
            state["rules_of_judgment"]
                .to_string()
                .contains("The caller is the owner")
        );
    }
    let record = t
        .audit()
        .into_iter()
        .rfind(|record| record["op"] == "exec")
        .unwrap();
    assert_eq!(record["gate"]["caller"], "owner_device");
    assert_eq!(record["gate"]["verdict"], "allow");
    assert!(record["gate"]["device_rule"].is_string(), "{record}");

    // An agent on the same host is judged as an agent, on its intent and
    // the same rule.
    let agent = t.caller("agent").await;
    let (answer, _) = t.cite(&agent, "exec", exec("touch other")).await;
    assert_eq!(refusal(&answer).2["reason"], "not_covered");
    let record = t.audit().pop().unwrap();
    assert_eq!(record["gate"]["caller"], "agent");
    assert_eq!(record["gate"]["verdict"], "not_covered");
    assert_eq!(record["agent"], AGENT);
    let agents = t.judgments().pop().unwrap();
    assert_eq!(agents["device_rule"], "Never send email.");
    assert_eq!(agents["cited_message"]["text"], ASKED);
}

#[tokio::test]
async fn an_outage_refuses_judged_operations_for_everyone_but_not_reads_or_the_owners_settings() {
    let t = start(judged_by(down)).await;
    crate::gate::rules::Store::new(&t.state)
        .set_deny("Never send email.", None)
        .unwrap();
    let device = t.caller("device").await;
    let agent = t.caller("agent").await;
    for answer in [
        t.call(&device, "exec", exec("touch made")).await.0,
        t.cite(&agent, "exec", exec("touch made")).await.0,
    ] {
        let (exit, code, _) = refusal(&answer);
        assert_eq!((exit, code), (Exit::Unreachable, "gatekeeper_unavailable"));
    }
    assert!(!t.area.join("made").exists());
    let asked = t.judged();

    // The owner's settings never ask the judge, and work while it is down.
    let set = json!({"command": "set", "text": "Never send email or texts."});
    let (answer, _) = t.call(&device, "rules", set).await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    let (answer, _) = t.call(&device, "rules", json!({"command": "show"})).await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    let (answer, _) = t.call(&device, "vault", json!({"command": "list"})).await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    // Nor does an agent's vault list, a read.
    let (answer, _) = t.call(&agent, "vault", json!({"command": "list"})).await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");
    assert_eq!(t.judged(), asked, "no judgment for settings or reads");

    // And the settings stay the owner's: an agent is refused them, unasked.
    for (op, args) in [
        ("rules", json!({"command": "show"})),
        ("vault", json!({"command": "show", "item": "v1"})),
    ] {
        let (answer, _) = t.call(&agent, op, args).await;
        assert_eq!(refusal(&answer).1, "owner_only", "{op}");
    }
    assert_eq!(t.judged(), asked);
}

/// An agent may list the vault, a read no judge is asked about, and sees
/// each item's id, title and saved sites, and how to sign in with one:
/// never a username, a field or a value. The owner's list shows all of its
/// metadata; every other vault command stays theirs.
#[tokio::test]
async fn an_agents_vault_list_is_unjudged_and_shows_ids_titles_and_sites_only() {
    use crate::vault::item::{CARD, Item, LOGIN, Raw};
    use crate::vault::store::Backend;
    let t = start(judged_by(never_covered)).await;
    let device = t.caller("device").await;
    let agent = t.caller("agent").await;
    let login = [
        ("username", "morgan@example.com"),
        ("password", "made-up-pass-81f2"),
    ];
    let card = [
        ("number", "4242424242424242"),
        ("expiry month", "4"),
        ("expiry year", "2031"),
        ("code", "319"),
    ];
    for (kind, title, sites, values) in [
        (
            LOGIN,
            "Parentsquare",
            vec!["parentsquare.com".into()],
            &login[..],
        ),
        (CARD, "Visa", vec![], &card[..]),
    ] {
        let values: Vec<(String, String)> = values
            .iter()
            .map(|(label, value)| (label.to_string(), value.to_string()))
            .collect();
        let raw = Raw::from_labels(kind, title, &values).unwrap();
        t.host
            .vault
            .with(|store| {
                store.add(Item {
                    id: String::new(),
                    revision: 0,
                    sites,
                    other_top_sites: vec![],
                    raw,
                })
            })
            .unwrap();
    }
    let list = || json!({"command": "list"});

    // The owner's list shows the login's username.
    let (answer, _) = t.call(&device, "vault", list()).await;
    let Response::Ok(owners) = answer else {
        panic!("{answer:?}")
    };
    assert_eq!(owners["items"][0]["username"], "morgan@example.com");

    // The agent's lists what is saved where, and no more.
    let (answer, _) = t.call(&agent, "vault", list()).await;
    let Response::Ok(listed) = answer else {
        panic!("{answer:?}")
    };
    assert_eq!(
        listed["items"],
        json!([
            {"item": "v1", "kind": "login", "title": "Parentsquare", "sites": ["parentsquare.com"]},
            {"item": "v2", "kind": "card", "title": "Visa", "sites": []},
        ])
    );
    let next = listed["next"].as_str().unwrap();
    assert!(next.contains("`latch browser fill`"), "{next}");
    let shown = format!("{} {next}", listed["items"]);
    for (label, value) in login
        .iter()
        .chain(&card)
        .filter(|(_, value)| value.len() > 3)
    {
        assert!(!shown.contains(value), "{label} in {shown}");
    }
    assert!(
        !shown.contains("username") && !shown.contains("fields"),
        "{shown}"
    );
    let record = t
        .audit()
        .into_iter()
        .rfind(|record| record["op"] == "vault")
        .unwrap();
    assert_eq!(record["gate"]["class"], "free");
    assert_eq!(record["args"], json!({"command": "list"}));

    // Every other vault command stays the owner's.
    let (answer, _) = t
        .call(&agent, "vault", json!({"command": "show", "item": "v1"}))
        .await;
    assert_eq!(refusal(&answer).1, "owner_only");
    assert_eq!(t.judged(), 0);
}

#[tokio::test]
async fn an_agent_plow_gives_no_uid_runs_nothing() {
    let t = start(judged_by(never_covered)).await;
    let agent = t.caller("agent").await;
    let key = agent.id().to_string();
    t.plow.world.lock().unwrap().callers.get_mut(&key).unwrap()["agent_uid"] = Value::Null;
    let (answer, _) = t.cite(&agent, "exec", exec("touch made")).await;
    let (exit, code, _) = refusal(&answer);
    assert_eq!((exit, code), (Exit::Unreachable, "gatekeeper_unavailable"));
    assert!(!t.area.join("made").exists());
    assert_eq!(t.judged(), 0);
    assert_eq!(t.audit()[0]["decision"], "deny");
}

/// The owner's `latch rules` and `latch agents map` commands over a real
/// connection, as the client sends them: set, edit, show, map and clear
/// work from a device, an agent gets `owner_only` for each, and the
/// agent's next judgment reads what the owner set.
#[tokio::test]
async fn the_owners_rules_commands_work_over_the_connection_and_never_for_an_agent() {
    let t = start(judged_by(always_covered)).await;
    let device = t.caller("device").await;
    let agent = t.caller("agent").await;
    let ok = |answer: Response| match answer {
        Response::Ok(result) => result,
        refused => panic!("refused: {refused:?}"),
    };
    let rule = || crate::gate::rules::Store::new(&t.state).deny().unwrap();

    let set = json!({"command": "set", "text": "Never send email."});
    let done = ok(t.call(&device, "rules", set).await.0);
    assert_eq!(done["host"], "test-host");
    assert_eq!(done["applies"], "applies to the next judgment");

    // `latch rules edit`: a show, then an edit at its revision.
    let shown = ok(t.call(&device, "rules", json!({"command": "show"})).await.0);
    assert_eq!(shown["device_rule"]["text"], "Never send email.");
    assert_eq!(shown["agents"], json!({(AGENT): fake::AGENT_SWARM}));
    assert!(shown["warning"].as_str().unwrap().contains("--deny-exec"));
    let read = shown["device_rule"]["revision"].clone();
    let edit = |text: &str, revision: &Value| json!({"command": "edit", "text": text, "revision": revision});
    let edited = "Never send email. Never pay.";
    ok(t.call(&device, "rules", edit(edited, &read)).await.0);
    let (stale, _) = t
        .call(&device, "rules", edit("Anything goes.", &read))
        .await;
    assert_eq!(refusal(&stale).0, Exit::Denied);
    assert_eq!(refusal(&stale).1, "stale_revision");
    assert_eq!(rule().as_deref(), Some(edited));

    // `latch agents map`, by the agent's name.
    let map = json!({"command": "map_agent", "agent": "Juno", "swarm_id": "@juno:test"});
    let mapped = ok(t.call(&device, "rules", map).await.0);
    assert_eq!(
        (mapped["agent_uid"].as_str(), mapped["replaced"].as_str()),
        (Some(AGENT), Some(fake::AGENT_SWARM))
    );

    for args in [
        json!({"command": "show"}),
        json!({"command": "set", "text": "Anything goes."}),
        json!({"command": "edit", "text": "Anything goes.", "revision": read}),
        json!({"command": "map_agent", "agent": "Juno", "swarm_id": "@other:test"}),
        json!({"command": "clear"}),
    ] {
        let (answer, _) = t.call(&agent, "rules", args.clone()).await;
        assert_eq!(refusal(&answer).0, Exit::Denied, "{args}");
        assert_eq!(refusal(&answer).1, "owner_only", "{args}");
    }
    assert_eq!(rule().as_deref(), Some(edited));

    // The agent's next operation is judged on the rule, with no restart.
    let map = json!({"command": "map_agent", "agent": "Juno", "swarm_id": fake::AGENT_SWARM});
    ok(t.call(&device, "rules", map).await.0);
    ok(t.cite(&agent, "exec", exec("true")).await.0);
    assert_eq!(t.judgments().last().unwrap()["device_rule"], edited);

    let done = ok(t
        .call(&device, "rules", json!({"command": "clear"}))
        .await
        .0);
    assert_eq!(done["cleared"], true);
    assert_eq!(rule(), None);

    // Every command is audited by revision; a saved rule's text is kept
    // with its revision, and no refused one's is.
    let audit = t.audit();
    let rules_records: Vec<&Value> = audit.iter().filter(|r| r["op"] == "rules").collect();
    assert_eq!(rules_records.len(), 12);
    let denied = rules_records
        .iter()
        .filter(|r| r["decision"] == "deny")
        .count();
    assert_eq!(denied, 6, "five from the agent, one stale edit");
    let saved: Vec<(&Value, &Value)> = rules_records
        .iter()
        .filter(|r| !r["args"]["text"].is_null())
        .map(|r| (&r["args"]["after"], &r["args"]["text"]))
        .collect();
    assert_eq!(
        saved,
        [
            (
                &json!(crate::gate::rules::revision("Never send email.")),
                &json!("Never send email.")
            ),
            (&json!(crate::gate::rules::revision(edited)), &json!(edited)),
        ]
    );
    let text = serde_json::to_string(&audit).unwrap();
    assert!(!text.contains("Anything goes"), "{text}");
    // serve.log names each command, never a saved rule's text.
    let log = crate::log::captured().join("\n");
    assert!(log.contains("rules:"), "{log}");
    assert!(!log.contains("Never send email"), "{log}");
}

/// One request over a phone link's connection, as the phone page sends it.
async fn phone_call(host: &TestHost, op: &str, args: Value) -> Response {
    let phone = crate::dial::endpoint()
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
        intent: None,
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
    let host = start(judged_by(never_covered)).await;
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
async fn a_host_whose_binary_was_replaced_says_which_build_is_there_on_its_dashboard() {
    use std::os::unix::fs::PermissionsExt;
    let host = start(judged_by(never_covered)).await;
    let mut board = host.host.dashboard.watch();
    watch_binary(&host.host, Duration::from_millis(50));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(board.borrow().update, None, "the same binary");

    // An upgrade renames the new latch over the old one.
    let new = host.binary.with_extension("tmp");
    let says = r#"#!/bin/sh
echo '{"version":"0.1.0 (133): commit 1a2b3c"}'
"#;
    std::fs::write(&new, says).unwrap();
    std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&new, &host.binary).unwrap();
    let said = tokio::time::timeout(
        Duration::from_secs(10),
        board.wait_for(|board| board.update.is_some()),
    )
    .await
    .unwrap()
    .unwrap()
    .update
    .clone();
    assert_eq!(
        said.as_deref(),
        Some("latch was updated; restart to use build 133")
    );
}

#[tokio::test]
async fn a_stalled_audit_write_holds_up_no_other_caller() {
    let t = start(judged_by(down)).await;
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

/// The Messages reader, built from `messages-reader/` once per test run.
#[cfg(target_os = "macos")]
fn messages_reader() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("messages-reader");
            let swift = |extra: &[&str]| {
                let output = std::process::Command::new("swift")
                    .args([
                        "build",
                        "--package-path",
                        package.to_str().unwrap(),
                        "--product",
                        "latch-messages",
                    ])
                    .args(extra)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                String::from_utf8(output.stdout).unwrap()
            };
            swift(&[]);
            PathBuf::from(swift(&["--show-bin-path"]).trim()).join("latch-messages")
        })
        .clone()
}

impl TestHost {
    /// The owner's Messages and address book, from the synthetic fixtures,
    /// and the reader installed where a host looks for it.
    #[cfg(target_os = "macos")]
    pub(super) fn with_messages(&self) {
        let sqlite = |sql: &str, db: &Path| {
            std::fs::create_dir_all(db.parent().unwrap()).unwrap();
            let mut child = std::process::Command::new("/usr/bin/sqlite3")
                .arg(db)
                .stdin(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            std::io::Write::write_all(child.stdin.as_mut().unwrap(), sql.as_bytes()).unwrap();
            drop(child.stdin.take());
            assert!(child.wait().unwrap().success());
        };
        let [messages, book] = crate::messages::host::folders(&self.home);
        sqlite(
            include_str!("../../tests/messages/chat.sql"),
            &messages.join("chat.db"),
        );
        sqlite(
            include_str!("../../tests/messages/addressbook.sql"),
            &book.join("AddressBook-v22.abcddb"),
        );
        let reader = crate::messages::host::reader(&self.state);
        std::fs::create_dir_all(reader.parent().unwrap()).unwrap();
        std::fs::copy(messages_reader(), &reader).unwrap();
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn messages_and_contacts_reads_are_unjudged_and_audited_without_a_row() {
    let t = start(judged_by(always_covered)).await;
    t.with_messages();
    let agent = t.caller("agent").await;

    let search = json!({"action": "search", "args": ["--", "code"]});
    let (answer, _) = t.call(&agent, "messages", search).await;
    let Response::Ok(found) = answer else {
        panic!("{answer:?}")
    };
    assert_eq!(found["count"], 1);
    assert_eq!(found["messages"][0]["text"], "Your code is 482913");
    assert_eq!(
        found["messages"][0]["from"]["name"], "Ann Example",
        "{found}"
    );
    assert!(
        found["warning"]
            .as_str()
            .unwrap()
            .contains("untrusted historical data")
    );

    let (answer, _) = t
        .call(
            &agent,
            "contacts",
            json!({"action": "search", "args": ["--", "sample"]}),
        )
        .await;
    let Response::Ok(people) = answer else {
        panic!("{answer:?}")
    };
    assert_eq!(people["people"][0]["name"], "Robert Sample");
    assert_eq!(people["people"][0]["nickname"], "Bo");
    assert_eq!(people["people"][0]["emails"][0]["handle"], "bo@example.com");

    let (answer, _) = t
        .call(
            &agent,
            "messages",
            json!({"action": "thread", "args": ["--with", "(555) 000-0001"]}),
        )
        .await;
    let Response::Ok(thread) = answer else {
        panic!("{answer:?}")
    };
    assert_eq!(thread["count"], 3);

    // No call was judged; each ran, and is audited, as its effective request.
    assert_eq!(t.judged(), 0);
    let records = t.audit();
    assert_eq!(
        records[0]["args"]["plan"],
        json!({"command": "search", "limit": 50, "oldest_first": false, "phrase": "code"})
    );
    assert_eq!(records[1]["args"]["action"], "contacts.search");
    assert_eq!(records[1]["args"]["plan"]["text"], "sample");
    assert_eq!(records[2]["args"]["plan"]["with"], "+15550000001");

    // The audit keeps the action, its words and the counts; no row.
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["op"], "messages");
    assert_eq!(records[0]["decision"], "allow");
    assert_eq!(records[0]["args"]["action"], "messages.search");
    assert_eq!(records[0]["args"]["result"]["count"], 1);
    assert_eq!(records[2]["args"]["result"]["count"], 3);
    // The reader's phases are timed in the audit, not in the answer.
    let timings = &records[0]["args"]["result"]["timings"];
    for phase in ["store_open_ms", "address_book_ms", "query_ms", "reader_ms"] {
        assert!(timings[phase].is_u64(), "{timings}");
    }
    assert!(found.get("timings").is_none(), "{found}");

    // A later page of contacts: its cursor holds the last name the page
    // before gave, which neither the audit nor the log keeps.
    let first = json!({"action": "search", "args": ["--limit", "1", "--", "a"]});
    let (answer, _) = t.call(&agent, "contacts", first).await;
    let Response::Ok(first) = answer else {
        panic!("{answer:?}")
    };
    assert_eq!(first["people"][0]["name"], "Ann Example");
    let next = first["next"].as_str().unwrap().to_string();
    let page = json!({"action": "search", "args": ["--cursor", next.clone()]});
    let (answer, _) = t.call(&agent, "contacts", page).await;
    let Response::Ok(second) = answer else {
        panic!("{answer:?}")
    };
    assert_eq!(second["people"][0]["name"], "Robert Sample");
    let paged = &t.audit()[4]["args"];
    assert_eq!(paged["args"], json!(["--cursor", "[cursor]"]));
    assert_eq!(paged["plan"]["after"]["key"], "[name]");

    let records = t.audit();
    // The audit as it reads, opened (on disk it is sealed).
    let audit = serde_json::to_string(&records).unwrap();
    // What the host logged: every line this test process logged.
    let log = crate::log::captured().join("\n");
    assert!(log.contains("messages:"), "{log}");
    // The board says how many matched, and shows nothing that was read.
    let board = serde_json::to_string(&*t.host.dashboard.watch().borrow()).unwrap();
    assert!(board.contains(r#""text":"1 match""#), "{board}");
    // Nothing the reader returned: no body, no code, no name, no handle it found.
    for returned in [
        "482913",
        "Your code",
        "Super Duper",
        "lunch?",
        "Ann Example",
        "Robert Sample",
        "Sample & Co",
        "Dinner",
        next.as_str(),
    ] {
        assert!(!audit.contains(returned), "{returned} in the audit");
        assert!(!log.contains(returned), "{returned} in serve.log");
        assert!(!board.contains(returned), "{returned} on the dashboard");
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn an_exec_cannot_read_the_messages_or_address_book_its_host_reads() {
    let t = start(judged_by(always_covered)).await;
    t.with_messages();
    let agent = t.caller("agent").await;
    let [messages, book] = crate::messages::host::folders(&t.home);
    for path in [
        messages.join("chat.db"),
        book.join("AddressBook-v22.abcddb"),
    ] {
        let (answer, _) = t
            .cite(&agent, "exec", exec(&format!("cat '{}'", path.display())))
            .await;
        let Response::Ok(result) = answer else {
            panic!("{answer:?}")
        };
        assert_ne!(result["exit_code"], 0, "{}", path.display());
        assert!(
            result["stderr"]
                .as_str()
                .unwrap()
                .contains("Operation not permitted"),
            "{result}"
        );
    }
    // The messages worker, beside it, reads them.
    let (answer, _) = t
        .call(&agent, "messages", json!({"action": "chats", "args": []}))
        .await;
    let Response::Ok(chats) = answer else {
        panic!("{answer:?}")
    };
    assert_eq!(chats["count"], 3);
}

/// A cursor as the reader writes one (base64url of its JSON), made up: a
/// thread page naming no conversation.
#[cfg(target_os = "macos")]
fn forged_cursor() -> String {
    let cursor = json!({"command": "thread", "key": "1", "id": 1,
                        "filters": {"from": [], "oldestFirst": false, "handles": []}})
    .to_string();
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let bytes = cursor.as_bytes();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |n, b| n << 8 | *b as u32) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_later_page_runs_on_the_filters_its_cursor_carries() {
    let t = start(judged_by(always_covered)).await;
    t.with_messages();
    let agent = t.caller("agent").await;
    let first = json!({"action": "search", "args": ["--from", "(555) 000-0001", "--limit", "1"]});
    let (answer, _) = t.call(&agent, "messages", first).await;
    let Response::Ok(first) = answer else {
        panic!("{answer:?}")
    };
    let next = first["next"].as_str().unwrap().to_string();
    let page = json!({"action": "search", "args": ["--cursor", next]});
    let (answer, _) = t.call(&agent, "messages", page).await;
    let Response::Ok(second) = answer else {
        panic!("{answer:?}")
    };
    assert_eq!(second["messages"][0]["id"], 301);
    // The page ran, and is audited, as its whole request, not an opaque token.
    let page = &t.audit()[1]["args"]["plan"];
    assert_eq!(page["from"], json!(["+15550000001"]), "{page}");
    assert_eq!(page["after"]["id"], 103, "{page}");
    assert_eq!(page["limit"], 50);

    // A cursor that names no conversation is refused.
    let forged = json!({"action": "thread", "args": ["--cursor", forged_cursor()]});
    let (answer, _) = t.call(&agent, "messages", forged).await;
    let (exit, code, _) = refusal(&answer);
    assert_eq!((exit, code), (Exit::Usage, "usage"));
    assert_eq!(t.judged(), 0);
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn messages_and_contacts_run_unjudged_for_the_owner_and_agents_even_in_an_outage() {
    let t = start(judged_by(down)).await;
    t.with_messages();
    let (device, agent) = (t.caller("device").await, t.caller("agent").await);
    let calls = [
        (
            "messages",
            json!({"action": "search", "args": ["--", "code"]}),
        ),
        (
            "contacts",
            json!({"action": "lookup", "args": ["--", "+15550000001"]}),
        ),
    ];
    for (op, args) in &calls {
        for caller in [&device, &agent] {
            let (answer, _) = t.call(caller, op, args.clone()).await;
            assert!(matches!(answer, Response::Ok(_)), "{op}: {answer:?}");
        }
    }
    assert_eq!(t.judged(), 0);
    let records = t.audit();
    for (index, record) in records.iter().enumerate() {
        let caller = if index % 2 == 0 {
            "owner_device"
        } else {
            "agent"
        };
        assert_eq!(record["gate"]["caller"], caller, "{record}");
        assert_eq!(record["decision"], "allow", "{record}");
        assert!(record["args"].get("result").is_some(), "{record}");
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_contacts_worker_cannot_read_messages() {
    let t = start(judged_by(always_covered)).await;
    t.with_messages();
    let [messages, book] = crate::messages::host::folders(&t.home);
    let argv: Vec<String> = vec![
        "--store".into(),
        messages.join("chat.db").display().to_string(),
        "--addressbook".into(),
        book.display().to_string(),
        "chats".into(),
    ];
    let workers = &t.host.workers;
    let contacts = crate::messages::host::reads("contacts", &t.home);
    let refused = crate::messages::host::read(workers, &contacts, &argv)
        .await
        .unwrap_err();
    assert_eq!(refused.code, "messages_not_granted");
    let messages_reads = crate::messages::host::reads("messages", &t.home);
    let read = crate::messages::host::read(workers, &messages_reads, &argv)
        .await
        .unwrap();
    assert_eq!(read["count"], 3);
    // The contacts call itself reads the address book.
    let agent = t.caller("agent").await;
    let search = json!({"action": "search", "args": ["--", "ann"]});
    let (answer, _) = t.call(&agent, "contacts", search).await;
    let Response::Ok(people) = answer else {
        panic!("{answer:?}")
    };
    assert_eq!(people["count"], 1);
}

#[tokio::test]
async fn local_vault_edits_reach_a_running_hosts_release_path() {
    use crate::vault::store::Backend;
    use tokio::io::AsyncWriteExt;
    let t = start(judged_by(always_covered)).await;
    let state = crate::state::StateDir {
        path: t.state.clone(),
    };
    state.create().unwrap();
    let key = state.load_or_create_key().unwrap().0.to_string();
    state
        .save_config(&crate::state::Config {
            api_url: "http://127.0.0.1:9".into(),
            peer: serde_json::from_value(
                json!({"key": key, "name": "host", "kind": "device", "relay_url": null}),
            )
            .unwrap(),
        })
        .unwrap();
    assert!(
        t.host
            .vault
            .with(|store| Ok(store.list()))
            .unwrap()
            .is_empty()
    );
    let mut child = tokio::process::Command::new(latch_binary())
        .arg("--state-dir")
        .arg(&t.state)
        .args([
            "vault",
            "add",
            "--kind",
            "login",
            "--title",
            "the owner",
            "--site",
            "example.com",
            "--stdin",
        ])
        .env_remove("LATCH_HOST")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"password": "synthetic-fill-value"}"#)
        .await
        .unwrap();
    let output = child.wait_with_output().await.unwrap();
    assert!(output.status.success(), "local add failed");
    assert_eq!(
        t.host
            .vault
            .with(|store| store.release("v1", "password"))
            .unwrap(),
        "synthetic-fill-value"
    );
}

#[tokio::test]
async fn an_exec_is_audited_whole_and_shown_with_its_credentials_hidden() {
    let t = start(judged_by(always_covered)).await;
    let agent = t.caller("agent").await;
    let secret = "synthetic-exec-credential-7f3a";
    // Longer than the 4 KiB the audit once kept: it is kept whole now.
    let long = "x".repeat(6000);
    let argv = json!([
        "/bin/sh",
        "-c",
        "printf %s \"$2\"",
        "sh",
        "--token",
        secret,
        long
    ]);
    let (answer, _) = t
        .cite(&agent, "exec", json!({"argv": argv, "timeout": 10}))
        .await;
    assert!(matches!(answer, Response::Ok(_)), "{answer:?}");

    // The audit keeps the command as it ran, sealed on disk.
    let records = t.audit();
    assert_eq!(records[0]["args"]["argv"], argv);
    let on_disk = std::fs::read_to_string(t.state.join(crate::audit::FILE)).unwrap();
    assert!(!on_disk.contains(secret) && !on_disk.contains("--token"));

    // serve.log and the dashboard show it with the credential hidden.
    let logged: Vec<String> = crate::log::captured()
        .into_iter()
        .filter(|line| line.contains("exec:") && line.contains("--token"))
        .collect();
    assert!(!logged.is_empty());
    assert!(
        logged.iter().all(|line| !line.contains(secret)),
        "{logged:?}"
    );
    assert!(
        logged.iter().all(|line| line.contains("[redacted]")),
        "{logged:?}"
    );
    let board = serde_json::to_string(&*t.host.dashboard.watch().borrow()).unwrap();
    assert!(board.contains("--token [redacted]"), "{board}");
    assert!(!board.contains(secret), "{board}");
}
