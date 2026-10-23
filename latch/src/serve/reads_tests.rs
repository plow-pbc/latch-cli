//! The owner reading a host's status and audit from another machine, end
//! to end: the same headless host as `gated_tests`, callers over iroh, and
//! the same lines the host's own commands give.

use std::time::Duration;

use serde_json::{Value, json};

use super::gated_tests::{TestHost, refusal, start};
use super::*;
use crate::audit::Range;
use crate::failure::Exit;
use crate::gate::fake::World;

fn ok(response: Response) -> Value {
    match response {
        Response::Ok(value) => value,
        Response::Error { code, message, .. } => panic!("{code}: {message}"),
    }
}

/// An open stream of `op`, read frame by frame.
struct Stream {
    connection: Connection,
    recv: iroh::endpoint::RecvStream,
}

impl Stream {
    async fn open(t: &TestHost, client: &Endpoint, op: &str, args: Value) -> Stream {
        let connection = client
            .connect(t.host.endpoint.addr(), wire::ALPN)
            .await
            .unwrap();
        let (mut send, recv) = connection.open_bi().await.unwrap();
        let request = Request {
            op: op.into(),
            args,
            chat: None,
        };
        wire::write_frame(&mut send, &serde_json::to_vec(&request).unwrap())
            .await
            .unwrap();
        send.finish().unwrap();
        Stream { connection, recv }
    }

    /// The next frame. The time limit only guards against a hang: before
    /// its first line, a debug-built host reads, opens and masks the whole
    /// log, which for a 16 MiB record takes most of a minute on a slow CI
    /// runner.
    async fn next(&mut self) -> Response {
        let frame =
            tokio::time::timeout(Duration::from_secs(120), wire::read_frame(&mut self.recv))
                .await
                .expect("a frame within 120s")
                .unwrap();
        serde_json::from_slice(&frame).unwrap()
    }

    async fn line(&mut self) -> Value {
        let mut answer = ok(self.next().await);
        assert!(answer.get("line").is_some(), "not a line: {answer}");
        answer["line"].take()
    }

    /// Every line up to the end of the stream, and its final answer.
    async fn all_lines(&mut self) -> (Vec<Value>, Value) {
        let mut lines = Vec::new();
        loop {
            let mut answer = ok(self.next().await);
            match answer.get_mut("line") {
                Some(line) => lines.push(line.take()),
                None => return (lines, answer),
            }
        }
    }

    /// The error that ends the stream, after any lines.
    async fn refusal(&mut self) -> Response {
        loop {
            match self.next().await {
                Response::Ok(_) => continue,
                refused => return refused,
            }
        }
    }
}

/// What the host's own `latch audit` prints for `args`.
fn audit_here(t: &TestHost, args: Value) -> Vec<Value> {
    let range: Range = serde_json::from_value(args).unwrap();
    let mut lines = Vec::new();
    crate::audit::read(&t.state, "test-host's", &range, None, |line| {
        lines.push(line.clone());
        true
    })
    .unwrap();
    lines
}

fn records(lines: &[Value]) -> Vec<Value> {
    lines
        .iter()
        .filter(|line| line["type"] == "record")
        .map(|line| line["record"].clone())
        .collect()
}

#[tokio::test]
async fn an_agent_is_refused_each_read_and_nothing_is_streamed() {
    let t = start(World::default()).await;
    let agent = t.caller("agent").await;
    for (op, args) in [
        ("status", json!({})),
        ("status", json!({"follow": true})),
        ("audit", json!({})),
        ("audit", json!({"follow": true})),
        (
            "audit_shot",
            json!({"record": "f".repeat(64), "shot": "final"}),
        ),
    ] {
        let mut stream = Stream::open(&t, &agent, op, args).await;
        let answer = stream.next().await;
        let (exit, code, _) = refusal(&answer);
        assert_eq!((exit, code), (Exit::Denied, "owner_only"), "{op}");
    }
    assert_eq!(t.judged(), 0, "reads are never judged");
}

#[tokio::test]
async fn the_owner_reads_the_same_status_and_audit_as_on_the_host_masked_as_there() {
    let t = start(World::default()).await;
    let secret = "synthetic-remote-credential-7c41";
    let mut exec = crate::audit::record("k1", None, "exec", "allow", "ok");
    exec.args = json!({"argv": ["curl", "-H", format!("Authorization: Bearer {secret}")]});
    t.host.append(exec).await.unwrap();
    let device = t.caller("device").await;

    let (status, _) = t.call(&device, "status", json!({})).await;
    let mut status = ok(status);
    status.as_object_mut().unwrap().remove("audit_id");
    let here = crate::status::local(&StateDir {
        path: t.state.clone(),
    })
    .unwrap();
    assert_eq!(status, serde_json::to_value(here).unwrap());

    let mut stream = Stream::open(&t, &device, "audit", json!({"all": true})).await;
    let (lines, end) = stream.all_lines().await;
    assert!(end["audit_id"].is_string(), "{end}");
    assert!(!lines.iter().any(|line| line.to_string().contains(secret)));
    // Here, the log also holds the read's own record, after the rest.
    let here = audit_here(&t, json!({"all": true}));
    let mut held = records(&here);
    assert_eq!(held.pop().unwrap()["id"], end["audit_id"]);
    assert_eq!(records(&lines), held);
    let ops: Vec<&str> = held.iter().map(|r| r["op"].as_str().unwrap()).collect();
    assert_eq!(ops, ["exec", "connect", "status", "connect"]);
    assert_eq!(lines[0], here[0]);
    assert_eq!(lines.last().unwrap()["type"], "caught_up");

    // An earlier page, and an anchor that host does not hold.
    let second = held[1]["id"].as_str().unwrap();
    let mut page = Stream::open(&t, &device, "audit", json!({"before": second})).await;
    let (page, _) = page.all_lines().await;
    assert_eq!(records(&page), held[..1]);
    let unknown = json!({"follow": true, "after": "f".repeat(64)});
    let mut expired = Stream::open(&t, &device, "audit", unknown).await;
    let answer = expired.refusal().await;
    let (exit, code, _) = refusal(&answer);
    assert_eq!((exit, code), (Exit::Usage, "cursor_expired"));
}

#[tokio::test]
async fn the_owner_fetches_the_screenshot_a_plans_record_names() {
    let t = start(World::default()).await;
    let (session, png) = (
        "lkjhgfdsapoiuytrewqmnbvcxzlkjhgf",
        b"\x89PNG a synthetic page",
    );
    crate::browser::journal::save_shot(&t.state, session, "r1", "1", png).unwrap();
    let sha256 = crate::state::to_hex(&<sha2::Sha256 as sha2::Digest>::digest(png));
    let mut plan = crate::audit::record(
        "agent-key",
        Some("agent-1".into()),
        "browser",
        "allow",
        "ok",
    );
    plan.args = json!({"command": "do", "session": crate::browser::host::hash_of(session), "request": "r1",
                       "step_records": [{"step": 1, "action": 1,
                                         "screenshot": {"shot": "1", "size": png.len(), "sha256": sha256}}]});
    let id = t.host.append(plan).await.unwrap();
    let device = t.caller("device").await;

    let out = tempfile::tempdir().unwrap();
    let path = out.path().join("step-1.png");
    let connection = device
        .connect(t.host.endpoint.addr(), wire::ALPN)
        .await
        .unwrap();
    let request = Request {
        op: "audit_shot".into(),
        args: json!({"record": id, "shot": "1"}),
        chat: None,
    };
    let (start, done, got) = crate::transfer::download(&connection.into(), &request, &path)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), png);
    assert_eq!(
        (start["size"].as_u64(), got.as_str()),
        (Some(png.len() as u64), sha256.as_str())
    );
    let read = t
        .audit()
        .into_iter()
        .find(|record| record["op"] == "audit_shot")
        .unwrap();
    assert_eq!(
        read["args"],
        json!({"record": id, "shot": "1", "size": png.len(), "sha256": sha256})
    );
    assert_eq!(done["audit_id"].as_str().map(str::len), Some(64));

    let mut missing = Stream::open(
        &t,
        &device,
        "audit_shot",
        json!({"record": id, "shot": "2"}),
    )
    .await;
    let answer = missing.refusal().await;
    let (exit, code, _) = refusal(&answer);
    assert_eq!((exit, code), (Exit::Usage, "no_such_shot"));
}

#[tokio::test]
async fn a_followed_stream_sends_each_new_record_and_ends_once_the_caller_leaves() {
    let t = start(World::default()).await;
    let device = t.caller("device").await;
    let mut status = Stream::open(&t, &device, "status", json!({"follow": true})).await;
    assert_eq!(status.line().await, json!({"type": "hello", "schema": 1}));
    assert_eq!(status.line().await["type"], "status");
    let mut audit = Stream::open(&t, &device, "audit", json!({"follow": true})).await;
    while audit.line().await["type"] != "caught_up" {}

    ok(t.local_call("rules", json!({"command": "show"})).await);
    let line = loop {
        let line = audit.line().await;
        if line["record"]["op"] == "rules" {
            break line;
        }
    };
    assert_eq!(line["type"], "record");
    assert_eq!(line["cursor"], line["record"]["id"]);

    // The host hears the callers leave, ends their streams and records them.
    status.connection.close(VarInt::from_u32(0), b"done");
    audit.connection.close(VarInt::from_u32(0), b"done");
    let ended = |op: &str| t.audit().iter().any(|record| record["op"] == op);
    let started = Instant::now();
    while !(ended("status") && ended("audit")) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "streams still open"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_stopping_host_ends_a_followed_stream_and_says_so() {
    let t = start(World::default()).await;
    let device = t.caller("device").await;
    let mut audit = Stream::open(&t, &device, "audit", json!({"follow": true})).await;
    assert_eq!(audit.line().await["type"], "hello");
    t.host.stopping.send_replace(Stopping::Draining);
    let answer = audit.refusal().await;
    let (exit, code, _) = refusal(&answer);
    assert_eq!((exit, code), (Exit::Unreachable, "host_stopped"));
}

#[tokio::test]
async fn a_stopping_host_ends_a_stream_whose_caller_stopped_reading() {
    let t = start(World::default()).await;
    // Far more than the connection lets through to a caller not reading.
    // A path is shown as it is; an exec's long word would be hidden.
    for _ in 0..15 {
        let mut record = crate::audit::record("k1", None, "file_get", "allow", "ok");
        record.args = json!({"path": "x".repeat(200_000)});
        t.host.append(record).await.unwrap();
    }
    let device = t.caller("device").await;
    let mut stalled = Stream::open(&t, &device, "audit", json!({"follow": true})).await;
    // Its first line comes once the host has read the log, so the stop
    // below lands while it sends the records, not while it reads them.
    assert_eq!(stalled.line().await["type"], "hello");
    tokio::time::sleep(Duration::from_millis(500)).await;

    t.host.stopping.send_replace(Stopping::Draining);
    // Generous, for a busy CI runner: a stuck send never ends at all, and
    // each look reads, opens and checks every record of the log.
    let started = Instant::now();
    while !t.audit().iter().any(|record| record["op"] == "audit") {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the stream is stuck in a send"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let ended = t.audit().into_iter().find(|record| record["op"] == "audit");
    assert_eq!(ended.unwrap()["outcome"], "host_stopped");
    // What the caller reads now is whole frames, then the stream's end:
    // never a frame the host cut off, with its answer inside it.
    let mut answered = false;
    loop {
        let frame =
            tokio::time::timeout(Duration::from_secs(60), wire::read_frame(&mut stalled.recv))
                .await
                .expect("the stream ends");
        match frame {
            Ok(frame) => {
                let read = serde_json::from_slice::<Response>(&frame);
                assert!(read.is_ok(), "a broken frame: {} bytes", frame.len());
                answered |= matches!(read, Ok(Response::Error { .. }));
            }
            // Ended by its answer, or reset: never cut off within a frame.
            Err(err) => {
                assert!(
                    answered || err.kind() != std::io::ErrorKind::UnexpectedEof,
                    "{err}"
                );
                break;
            }
        }
    }
}

#[tokio::test]
async fn a_record_too_large_for_one_frame_ends_the_read_and_is_never_left_out() {
    let t = start(World::default()).await;
    // A path is shown as it is; an exec's long word would be hidden.
    let mut record = crate::audit::record("k1", None, "file_get", "allow", "ok");
    record.args = json!({"path": "x".repeat(wire::MAX_FRAME)});
    let id = t.host.append(record).await.unwrap();
    let device = t.caller("device").await;
    let mut stream = Stream::open(&t, &device, "audit", json!({"all": true})).await;
    let answer = loop {
        match stream.next().await {
            Response::Ok(line) => assert_ne!(line["line"]["type"], "caught_up"),
            refused => break refused,
        }
    };
    let (exit, code, details) = refusal(&answer);
    assert_eq!((exit, code), (Exit::Failed, "record_too_large"));
    assert_eq!(details["id"], id);
    assert_eq!(details["limit"], wire::MAX_FRAME);
}
