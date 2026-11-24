//! A real host-side browser call and worker, with a stand-in terminal-browser
//! CLI and a DevTools endpoint that serves one session tab and one owner tab.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Map, Value, json, value::RawValue};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_websockets::{Message, ServerBuilder, WebSocketStream};

use super::ENGINE;
use crate::browser::host::{self, Browsers, Caller};
use crate::gate::{Gate, Gatekeeper, fake};
use crate::model::{Answer, Asked, CallMeta, Provider, Question};
use crate::vault::{
    host::Vault,
    item::{Item, LOGIN, Raw},
    store::Backend,
};

const ORIGIN: &str = "https://shop.example";
const PAGE: &str = "https://shop.example/login";
const USERNAME: &str = "fake-browser-user";
const PASSWORD: &str = "fake-browser-password";

#[derive(Default)]
struct Judge(Mutex<Vec<String>>);

impl Provider for Judge {
    fn ask<'a>(&'a self, _: &'a Value, questions: &'a [Question]) -> Asked<'a> {
        answers(questions, false)
    }

    fn ask_as_written<'a>(&'a self, state: &'a RawValue, questions: &'a [Question]) -> Asked<'a> {
        let state: Value = serde_json::from_str(state.get()).unwrap();
        let Some(kind) = state["proposed_action"]["kind"].as_str() else {
            return answers(questions, false);
        };
        let mut seen = self.0.lock().unwrap();
        seen.push(kind.into());
        answers(questions, kind == "vault_fill" && seen.len() == 1)
    }
}

fn answers(questions: &[Question], denied: bool) -> Asked<'_> {
    Box::pin(async move {
        Ok((
            questions
                .iter()
                .map(|q| {
                    let probability = if denied && q.id == "unconnected" {
                        0.98
                    } else {
                        0.02
                    };
                    (q.id.clone(), Answer::Yes(probability))
                })
                .collect(),
            CallMeta {
                provider: "stand-in",
                model: crate::gate::MODEL.into(),
                request_id: None,
                latency_ms: 0,
                cost: None,
                exchange: None,
            },
        ))
    })
}

async fn send(ws: &mut WebSocketStream<TcpStream>, value: Value) {
    ws.send(Message::text(value.to_string())).await.unwrap();
}

async fn devtools(listener: TcpListener) -> (usize, Vec<String>, usize) {
    let (http, _) = listener.accept().await.unwrap();
    let mut http = BufReader::new(http);
    let mut line = String::new();
    http.read_line(&mut line).await.unwrap();
    assert!(line.starts_with("GET /json/version "));
    while line != "\r\n" {
        line.clear();
        assert_ne!(http.read_line(&mut line).await.unwrap(), 0);
    }
    let body = r#"{"webSocketDebuggerUrl":"ws://elsewhere.invalid/devtools/browser/fake"}"#;
    http.get_mut()
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    drop(http);

    let (client, _) = listener.accept().await.unwrap();
    let (request, mut ws) = ServerBuilder::new().accept(client).await.unwrap();
    assert_eq!(request.uri().path(), "/devtools/browser/fake");
    let mut url = "about:blank".to_string();
    let mut clicks = 0;
    let mut refused = Vec::new();
    let mut filled = 0;
    while let Some(Ok(message)) = ws.next().await {
        let payload = message.as_text().unwrap();
        assert!(
            !payload.contains(PASSWORD),
            "a vault secret reached the relay"
        );
        let call: Value = serde_json::from_str(payload).unwrap();
        let method = call["method"].as_str().unwrap();
        let params = &call["params"];
        // These are the only commands the driver can send without a tab
        // session. The owner's target is never looked up or attached.
        if matches!(method, "Target.attachToTarget" | "Target.getTargetInfo") {
            assert_eq!(params["targetId"], "latch-target", "{method}");
        } else {
            assert_eq!(call["sessionId"], "latch-session", "{method}");
        }
        let result = match method {
            "Target.attachToTarget" => json!({"sessionId": "latch-session"}),
            "Target.getTargetInfo" => json!({"targetInfo": {"url": url, "title": "Fake login"}}),
            "Target.setAutoAttach"
            | "Page.enable"
            | "Network.enable"
            | "Network.setBypassServiceWorker"
            | "Fetch.enable" => json!({}),
            "Page.getFrameTree" => {
                json!({"frameTree":{"frame":{"id":"latch-frame","loaderId":"login"}}})
            }
            "Page.createIsolatedWorld" => json!({"executionContextId": 1}),
            "Page.getLayoutMetrics" => {
                json!({"cssVisualViewport": {"clientWidth":1,"clientHeight":1,"pageX":0,"pageY":0}})
            }
            "Page.captureScreenshot" => {
                json!({"data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+j410AAAAASUVORK5CYII="})
            }
            "Page.navigate" => {
                url = params["url"].as_str().unwrap().into();
                assert_eq!(url, PAGE);
                for (id, url) in [
                    ("loopback", "http://127.0.0.1/"),
                    ("private", "https://192.168.0.1/"),
                    ("link-local", "http://169.254.1.2/"),
                    ("file", "file:///hidden"),
                    ("public", "https://1.1.1.1/image.png"),
                ] {
                    send(
                        &mut ws,
                        json!({
                            "method":"Fetch.requestPaused", "sessionId":"latch-session",
                            "params":{"requestId":id,"request":{"url":url,"method":"GET"}}
                        }),
                    )
                    .await;
                }
                send(
                    &mut ws,
                    json!({"method":"Page.loadEventFired","sessionId":"latch-session","params":{}}),
                )
                .await;
                json!({"frameId":"latch-frame"})
            }
            "Fetch.failRequest" => {
                assert_eq!(params["errorReason"], "BlockedByClient");
                refused.push(params["requestId"].as_str().unwrap().into());
                json!({})
            }
            "Fetch.continueRequest" => {
                assert_eq!(params["requestId"], "public");
                json!({})
            }
            "Runtime.evaluate" => {
                let expression = params["expression"]
                    .as_str()
                    .unwrap()
                    .lines()
                    .last()
                    .unwrap();
                json!({"result":{"value":read(expression)}})
            }
            "Runtime.callFunctionOn" => {
                let code = params["functionDeclaration"].as_str().unwrap();
                let value = if code.contains("__latch.typeStart(") {
                    json!({"assign":true})
                } else {
                    assert!(code.contains("__latch.fill("));
                    filled += usize::from(params["arguments"][2]["value"] == USERNAME);
                    json!({"held":true})
                };
                json!({"result":{"value":value}})
            }
            "Input.dispatchMouseEvent" => {
                clicks += usize::from(params["type"] == "mouseReleased");
                json!({})
            }
            other => panic!("unexpected DevTools command {other}"),
        };
        let mut reply = json!({"id":call["id"],"result":result});
        if let Some(session) = call.get("sessionId") {
            reply["sessionId"] = session.clone();
        }
        if ws.send(Message::text(reply.to_string())).await.is_err() {
            break;
        }
    }
    (clicks, refused, filled)
}

fn read(expression: &str) -> Value {
    let button = json!({"role":"button","name":"Continue","on_screen":true});
    let op = expression.split_once('(').map_or(expression, |(op, _)| op);
    match op {
        "true" => json!(true),
        "self.origin === 'null' ? 'opaque' : self.origin" => json!(ORIGIN),
        "__latch.stageMark" => json!({"mark":"login","asks":true}),
        "__latch.holding" => json!(0),
        "__latch.scroll" => json!({"x":0,"y":0}),
        "__latch.read" => json!({"origin":ORIGIN,"url":PAGE,"ids":[1,2,3],"elements":[
            {"role":"textbox","name":"Username","on_screen":true},
            {"role":"textbox","name":"Password","on_screen":true},button]}),
        "__latch.text" => json!({"text":"Sign in to the fake shop"}),
        "__latch.describeAt" => button,
        "__latch.review" => json!({"uid":2,"element":button,"lines":[]}),
        "__latch.locate" => json!({"kind":"button","x":24,"y":36}),
        "__latch.fillTarget" => {
            let password = expression.ends_with(", 1)");
            json!({
                "kind":"text","tag":"input","form":0,"name":if password {"Password"} else {"Username"},
                "type":if password {"password"} else {"text"}, "page_secret":password,
                "origin":ORIGIN,"token":"login-document"
            })
        }
        "__latch.controls" => json!({"origin":ORIGIN,"controls":[]}),
        "__latch.fields" => json!([]),
        "__latch.screenView" | "__latch.viewOf" => json!({"lines":[]}),
        _ => Value::Null,
    }
}

#[tokio::test]
async fn terminal_browser_drives_only_its_tab_through_the_gate() {
    let test_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
    fs::create_dir_all(&test_root).unwrap();
    let dir = tempfile::tempdir_in(test_root).unwrap();
    let path = dir.path();
    let program = path.join("fake-terminal-browser");
    let latch = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("latch");
    let mut browsers = Browsers::new(path.into(), Vault::new(path.into()), false);
    browsers.test = Some((latch, vec![]));
    let judge = Arc::new(Judge::default());
    browsers.provider = Some(judge.clone());
    let keeper = Gatekeeper::new(
        judge.clone(),
        Box::new(fake::Reader::asked("Sign into my test shop")),
        path,
    );
    crate::gate::rules::Store::new(path)
        .map_agent(fake::AGENT, fake::AGENT_SWARM)
        .unwrap();
    let caller = || Caller {
        key: "test-caller",
        device: false,
        gate: Some(Gate {
            keeper: &keeper,
            caller: crate::gate::Caller::Agent {
                uid: fake::AGENT,
                intent: Some(fake::CITED),
            },
            peer: "test-caller",
        }),
    };
    let call = async |command: &str, name: &str, args: &Value| {
        let mut audit = Map::new();
        host::call(&browsers, caller(), command, name, args, None, &mut audit)
            .await
            .map(|(value, _)| value)
    };

    // SAFETY: nextest isolates this test in its own process, and it uses
    // a current-thread runtime. Set the path before any worker starts.
    unsafe { std::env::set_var("LATCH_TERMINAL_BROWSER", &program) };
    let missing = call("open", ENGINE, &json!({"url": PAGE}))
        .await
        .unwrap_err();
    assert_eq!(missing.code, "engine_not_installed");
    assert!(missing.message.contains(super::INSTALL));

    fs::write(
        path.join("host.json"),
        json!({"browser_engine": ENGINE, "allow_local_browsing": true}).to_string(),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(devtools(listener));
    let script = r#"#!/bin/sh
cd -- "$(dirname -- "$0")" || exit 1
case "$*" in
    "ls --all --json")
        if [ -e stopped ]; then
            printf '%s\n' '{"browsers":[]}'
        elif [ -e opened ]; then
            printf '%s\n' '{"browsers":[{"key":"pane","cdpPort":PORT,"tabs":[{"id":1,"targetId":"owner-target"},{"id":2,"targetId":"latch-target"}]}]}'
        else
            printf '%s\n' '{"browsers":[{"key":"pane","cdpPort":PORT,"tabs":[{"id":1,"targetId":"owner-target"}]}]}'
        fi ;;
    "new-tab --browser pane about:blank")
        touch opened
        echo 'opened tab 2 in pane (terminal-browser ls for details)' ;;
    "action --browser pane --target latch-target -- tab close")
        mv opened closed ;;
    *) exit 2 ;;
esac
"#;
    fs::write(&program, script.replace("PORT", &port.to_string())).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();

    fs::write(path.join("stopped"), "").unwrap();
    let stopped = call("open", "", &json!({"url": PAGE})).await.unwrap_err();
    assert_eq!(stopped.code, "engine_not_running");
    assert!(stopped.message.contains("terminal-browser open"));
    assert!(!path.join("opened").exists());
    fs::remove_file(path.join("stopped")).unwrap();

    let opened = call("open", "", &json!({"url": PAGE})).await.unwrap();
    assert_eq!(opened["engine"], ENGINE);
    let id = opened["session"].as_str().unwrap();
    let observed = call("observe", id, &json!({})).await.unwrap();
    assert_eq!(observed["title"], "Fake login");
    assert_eq!(observed["tabs"].as_array().unwrap().len(), 1);
    assert_eq!(observed["elements"].as_array().unwrap().len(), 3);
    assert_eq!(
        call("goto", id, &json!({"url":"http://127.0.0.1/"}))
            .await
            .unwrap_err()
            .code,
        "url_refused"
    );

    browsers
        .vault
        .with(|store| {
            store.add(Item {
                id: String::new(),
                revision: 0,
                sites: vec!["shop.example".into()],
                other_top_sites: vec![],
                raw: Raw::from_labels(
                    LOGIN,
                    "Test login",
                    &[
                        ("username".into(), USERNAME.into()),
                        ("password".into(), PASSWORD.into()),
                    ],
                )
                .unwrap(),
            })
        })
        .unwrap();
    let fill = json!({"snapshot":observed["snapshot"],"fields":[["username",["e1"]]]});
    assert_eq!(
        call("fill", id, &fill).await.unwrap_err().code,
        "gatekeeper_denied"
    );
    call("fill", id, &fill).await.unwrap();
    let secret =
        json!({"snapshot":observed["snapshot"],"fields":[["password",["e2"]]],"press":"e3"});
    let failure = call("fill", id, &secret).await.unwrap_err();
    assert_eq!(failure.code, "fill_refused");
    assert!(failure.message.contains("cannot protect vault secrets"));
    let step = crate::browser::plan::short_step("click e3").unwrap();
    let plan = crate::browser::plan::Plan {
        steps: vec![step],
        ..Default::default()
    };
    call(
        "do",
        id,
        &json!({"request":"r123456789abc", "plan":plan, "act":true}),
    )
    .await
    .unwrap();
    call("close", id, &json!({})).await.unwrap();
    assert!(path.join("closed").is_file());
    let (clicks, refused, filled) = server.await.unwrap();
    assert_eq!(clicks, 1);
    assert_eq!(filled, 1);
    assert_eq!(refused, ["loopback", "private", "link-local", "file"]);
    let judged = judge.0.lock().unwrap();
    assert_eq!(
        *judged,
        ["vault_fill", "vault_fill", "vault_fill", "browser_plan"]
    );
}
