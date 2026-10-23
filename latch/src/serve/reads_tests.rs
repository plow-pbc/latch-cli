//! The owner reading a host's status, audit and events from another
//! machine, end to end: the same headless host as `gated_tests`, callers
//! over iroh, and the same output the host's own commands give.

use std::time::Duration;

use serde_json::{Value, json};

use super::gated_tests::{TestHost, refusal, start};
use super::*;
use crate::cli::{AuditAction, EventsArgs};
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

    async fn next(&mut self) -> Response {
        let frame = tokio::time::timeout(Duration::from_secs(10), wire::read_frame(&mut self.recv))
            .await
            .expect("a frame within 10s")
            .unwrap();
        serde_json::from_slice(&frame).unwrap()
    }

    async fn line(&mut self) -> Value {
        let mut answer = ok(self.next().await);
        assert!(answer.get("line").is_some(), "not a line: {answer}");
        answer["line"].take()
    }
}

/// What the host's own `latch events` prints for its state folder.
fn events_here(t: &TestHost) -> Vec<Value> {
    let args = EventsArgs {
        follow: false,
        after: None,
        lifecycle_only: false,
        host: None,
    };
    let mut lines = Vec::new();
    crate::events::stream(&t.state, &args, None, |line| {
        lines.push(line.clone());
        true
    })
    .unwrap();
    lines
}

fn audit_here(t: &TestHost) -> Value {
    let show = AuditAction::Show {
        follow: false,
        host: None,
    };
    crate::audit::run(
        &StateDir {
            path: t.state.clone(),
        },
        show,
    )
    .unwrap()
}

#[tokio::test]
async fn an_agent_is_refused_each_read_and_nothing_is_streamed() {
    let t = start(World::default()).await;
    let agent = t.caller("agent").await;
    for (op, args) in [
        ("status", json!({})),
        ("audit", json!({"follow": false})),
        ("audit", json!({"follow": true})),
        ("events", json!({"follow": true})),
    ] {
        let mut stream = Stream::open(&t, &agent, op, args).await;
        let answer = stream.next().await;
        let (exit, code, _) = refusal(&answer);
        assert_eq!((exit, code), (Exit::Denied, "owner_only"), "{op}");
    }
    assert_eq!(t.judged(), 0, "reads are never judged");
}

#[tokio::test]
async fn the_owner_reads_the_same_status_audit_and_events_as_on_the_host() {
    let t = start(World::default()).await;
    let device = t.caller("device").await;

    let (status, _) = t.call(&device, "status", json!({})).await;
    let mut status = ok(status);
    status.as_object_mut().unwrap().remove("audit_id");
    let here = crate::status::local(&StateDir {
        path: t.state.clone(),
    })
    .unwrap();
    assert_eq!(status, serde_json::to_value(here).unwrap());

    // The records as the host shows them, up to the one this read added.
    let (audit, _) = t.call(&device, "audit", json!({"follow": false})).await;
    let mut audit = ok(audit);
    let here = audit_here(&t);
    let mut records = here["records"].as_array().unwrap().clone();
    let own = records.pop().unwrap();
    assert_eq!(
        (own["op"].as_str(), own["id"] == audit["audit_id"]),
        (Some("audit"), true)
    );
    audit.as_object_mut().unwrap().remove("audit_id");
    assert_eq!(audit, json!({"records": records}));
    assert!(records.iter().any(|record| record["op"] == "status"));

    // Without --follow: the snapshot, then the end.
    let mut stream = Stream::open(&t, &device, "events", json!({"follow": false})).await;
    let snapshot = stream.line().await;
    let end = ok(stream.next().await);
    assert!(
        end.get("line").is_none() && end["audit_id"].is_string(),
        "{end}"
    );
    let here = events_here(&t);
    // Here, the snapshot also holds the events read's own record.
    let mut activity = here[0]["activity"].as_array().unwrap().clone();
    assert_eq!(activity.pop().unwrap()["op"], "events");
    assert_eq!(snapshot["activity"], json!(activity));
    assert_eq!(snapshot["host"], here[0]["host"]);
}

#[tokio::test]
async fn a_followed_stream_sends_each_new_record_and_ends_once_the_caller_leaves() {
    let t = start(World::default()).await;
    let device = t.caller("device").await;
    let mut events = Stream::open(&t, &device, "events", json!({"follow": true})).await;
    assert_eq!(events.line().await["type"], "snapshot");
    let mut audit = Stream::open(&t, &device, "audit", json!({"follow": true})).await;
    assert!(audit.line().await["id"].is_string());

    ok(t.local_call("rules", json!({"command": "show"})).await);
    let line = loop {
        let line = events.line().await;
        if line["type"] == "activity" && line["entry"]["op"] == "rules" {
            break line;
        }
    };
    assert!(line["cursor"].is_string());
    let record = loop {
        let record = audit.line().await;
        if record["op"] == "rules" {
            break record;
        }
    };
    assert_eq!(record["id"], line["entry"]["id"]);

    // The host hears the callers leave, ends their streams and records them.
    events.connection.close(VarInt::from_u32(0), b"done");
    audit.connection.close(VarInt::from_u32(0), b"done");
    let ended = |op: &str| t.audit().iter().any(|record| record["op"] == op);
    let started = Instant::now();
    while !(ended("events") && ended("audit")) {
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
    let mut events = Stream::open(&t, &device, "events", json!({"follow": true})).await;
    assert_eq!(events.line().await["type"], "snapshot");
    t.host.stopping.send_replace(Stopping::Draining);
    let answer = loop {
        match events.next().await {
            Response::Ok(_) => continue,
            refused => break refused,
        }
    };
    let (exit, code, _) = refusal(&answer);
    assert_eq!((exit, code), (Exit::Unreachable, "host_stopped"));
}
