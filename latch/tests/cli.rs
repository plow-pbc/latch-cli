//! Runs the real `latch` binary against a tiny fake Plow, to check what an
//! agent actually sees: stdout, stderr, exit codes and the saved state.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;

use serde_json::{Value, json};

/// A fake Plow that answers one HTTP request per entry in `answers`, in
/// order, and hands back the raw requests it received. Like Plow, it answers
/// a registration with the key it was sent: a `null` key in an answer is
/// filled in from the request body, and a key of `REGISTERED` in a later
/// answer becomes the key last registered.
fn fake_plow(answers: Vec<(u16, Value)>) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        let mut registered = Value::Null;
        for (status, mut body) in answers {
            let (mut socket, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            // Headers end at an empty line; the body length is in Content-Length.
            let mut request = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                request.push_str(&line);
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let sent: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            if !sent["key"].is_null() {
                registered = sent["key"].clone();
            }
            if body["key"].is_null() && !sent["key"].is_null() {
                body["key"] = sent["key"].clone();
            }
            if let Some(peers) = body.get_mut("data").and_then(Value::as_array_mut) {
                for peer in peers.iter_mut().filter(|peer| peer["key"] == "REGISTERED") {
                    peer["key"] = registered.clone();
                }
            }
            request.push_str(&String::from_utf8(bytes).unwrap());
            requests.push(request);

            let body = body.to_string();
            let head = format!(
                "HTTP/1.1 {status} Fake\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).unwrap();
            socket.write_all(body.as_bytes()).unwrap();
        }
        requests
    });
    (url, server)
}

/// Test state lives under the repo's gitignored `.dev/`, like every dev run.
fn test_dir() -> tempfile::TempDir {
    let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dev/tests");
    fs::create_dir_all(&parent).unwrap();
    tempfile::tempdir_in(parent).unwrap()
}

/// `latch --state-dir STATE ARGS...`, with none of the caller's latch or
/// Plow environment leaking in.
fn latch(state: &Path, args: &[&str], env: &[(&str, &Path)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_latch"));
    command
        .env_remove("PLOW_API_URL")
        .env_remove("PLOW_TOKEN_FILE")
        .env_remove("PLOW_API_BASE")
        .env_remove("PLOW_AGENT_TOKEN")
        .env_remove("LATCH_HOST")
        .arg("--state-dir")
        .arg(state)
        .args(args);
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().unwrap()
}

fn peer(key: Value, name: &str, kind: &str, relay_url: Value) -> Value {
    json!({"key": key, "kind": kind, "name": name, "relay_url": relay_url,
           "last_seen_at": "2026-10-15T01:30:12Z", "online": true})
}

#[test]
fn failed_login_never_echoes_plow_and_saves_no_credential() {
    // Plow's error text is never shown, even when it contains a secret.
    let (api_url, server) = fake_plow(vec![(403, json!({"detail": "secret-token"}))]);
    let dir = test_dir();
    let token_file = dir.path().join("owner.token");
    fs::write(&token_file, "secret-token\n").unwrap();
    let state = dir.path().join("state");
    let token_arg = token_file.to_str().unwrap();

    let args = [
        "login",
        "--name",
        "lab-a",
        "--api-url",
        &api_url,
        "--token-file",
        token_arg,
    ];
    let output = latch(&state, &args, &[]);

    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    let error: Value = serde_json::from_str(&stderr).unwrap();
    assert_eq!(error["error"]["code"], "plow_denied");
    assert!(!stderr.contains("secret-token"));
    // The key is kept for the next try; nothing else is saved.
    assert!(state.join("key").exists());
    assert!(!state.join("plow-token").exists());
    assert!(!state.join("config.json").exists());

    let request = &server.join().unwrap()[0];
    assert!(request.starts_with("POST /v1/latch/peers "));
    assert!(request.contains("authorization: Bearer secret-token\r\n"));
    let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["key"].as_str().unwrap().len(), 64);
    assert_eq!(body["name"], "lab-a");
    assert!(body.get("relay_url").is_none());
}

#[test]
fn agent_uses_its_own_credential_and_must_name_one_of_several_hosts() {
    let relay = json!("https://relay.example/");
    let (api_url, server) = fake_plow(vec![
        (
            200,
            peer(Value::Null, "research-agent", "agent", Value::Null),
        ),
        (
            200,
            json!({"data": [
                peer(json!("REGISTERED"), "research-agent", "agent", Value::Null),
                peer(json!("a".repeat(64)), "lab-a", "device", relay.clone()),
                peer(json!("b".repeat(64)), "lab-b", "device", relay),
            ]}),
        ),
    ]);
    let dir = test_dir();
    let agent_token = dir.path().join("agent.token");
    fs::write(&agent_token, "agent-credential\n").unwrap();
    let state = dir.path().join("state");
    let env = [("PLOW_TOKEN_FILE", agent_token.as_path())];

    let login = latch(&state, &["login", "--api-url", &api_url], &env);
    assert_eq!(login.status.code(), Some(0), "{login:?}");
    let output: Value = serde_json::from_slice(&login.stdout).unwrap();
    assert_eq!(output["peer"]["kind"], "agent");
    assert_eq!(output["key_created"], true);
    // The agent's credential stays where it lives; latch does not copy it.
    assert!(!state.join("plow-token").exists());

    let exec = latch(&state, &["exec", "--", "true"], &env);
    assert_eq!(exec.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&exec.stderr).unwrap();
    assert_eq!(error["error"]["code"], "host_required");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .ends_with("lab-a, lab-b")
    );

    // Plow lists the agent, so it does not register again.
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].starts_with("GET /v1/latch/peers "));
    assert!(requests[1].contains("authorization: Bearer agent-credential\r\n"));
}

/// An agent logged in with its own credential, against a fake Plow that
/// answers the login and then `answers`. Returns the test folder (state in
/// `state`) and the agent's environment.
fn logged_in_agent(
    answers: Vec<(u16, Value)>,
) -> (tempfile::TempDir, PathBuf, thread::JoinHandle<Vec<String>>) {
    let login = (
        200,
        peer(Value::Null, "research-agent", "agent", Value::Null),
    );
    let (api_url, server) = fake_plow([vec![login], answers].concat());
    let dir = test_dir();
    let token_file = dir.path().join("agent.token");
    fs::write(&token_file, "agent-credential\n").unwrap();
    let state = dir.path().join("state");
    let env = [("PLOW_TOKEN_FILE", token_file.as_path())];
    let login = latch(&state, &["login", "--api-url", &api_url], &env);
    assert_eq!(login.status.code(), Some(0), "{login:?}");
    (dir, token_file, server)
}

#[test]
fn an_agent_plow_no_longer_lists_registers_again_before_calling() {
    // After a rotation Plow forgets the agent's key until it registers again.
    let (dir, token_file, server) = logged_in_agent(vec![
        (200, json!({"data": []})),
        (
            200,
            peer(Value::Null, "registered-again", "agent", Value::Null),
        ),
    ]);
    let env = [("PLOW_TOKEN_FILE", token_file.as_path())];
    let state = dir.path().join("state");
    let exec = latch(&state, &["exec", "--", "true"], &env);
    let error: Value = serde_json::from_slice(&exec.stderr).unwrap();
    assert_eq!(error["error"]["code"], "no_host", "{error}");
    // The new registration is saved, as a login's would be.
    let config: Value =
        serde_json::from_slice(&fs::read(state.join("config.json")).unwrap()).unwrap();
    assert_eq!(config["peer"]["name"], "registered-again");

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);
    let body = |request: &str| -> Value {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    };
    assert!(
        requests[2].starts_with("POST /v1/latch/peers "),
        "{}",
        requests[2]
    );
    let again = body(&requests[2]);
    assert_eq!(again["key"], body(&requests[0])["key"]);
    assert_eq!(again["name"], "research-agent");
    assert!(again.get("take_over").is_none());
}

#[test]
fn a_failed_registration_again_says_why() {
    let (dir, token_file, server) = logged_in_agent(vec![
        (200, json!({"data": []})),
        (409, json!({"detail": "taken"})),
    ]);
    let env = [("PLOW_TOKEN_FILE", token_file.as_path())];
    let exec = latch(&dir.path().join("state"), &["exec", "--", "true"], &env);
    assert_eq!(exec.status.code(), Some(2), "{exec:?}");
    let error: Value = serde_json::from_slice(&exec.stderr).unwrap();
    assert_eq!(error["error"]["code"], "conflict", "{error}");
    assert_eq!(server.join().unwrap().len(), 3);
}

#[test]
fn a_cloud_agent_behind_plows_proxy_sends_no_credential() {
    // On exe.dev the agent's token never reaches the VM: `$PLOW_API_BASE`
    // is a proxy that adds it. It wins over `$PLOW_API_URL`.
    let (proxy, server) = fake_plow(vec![
        (200, peer(Value::Null, "agent", "agent", Value::Null)),
        (
            200,
            json!({"data": [peer(json!("REGISTERED"), "agent", "agent", Value::Null)]}),
        ),
    ]);
    let dir = test_dir();
    let state = dir.path().join("state");
    let env = [
        ("PLOW_API_BASE", Path::new(&proxy)),
        ("PLOW_API_URL", Path::new("http://127.0.0.1:9")),
        ("PLOW_TOKEN_FILE", &dir.path().join("none")),
    ];

    let login = latch(&state, &["login"], &env);
    assert_eq!(login.status.code(), Some(0), "{login:?}");
    let output: Value = serde_json::from_slice(&login.stdout).unwrap();
    assert_eq!(output["peer"]["kind"], "agent");
    assert!(!state.join("plow-token").exists());
    let config: Value =
        serde_json::from_slice(&fs::read(state.join("config.json")).unwrap()).unwrap();
    assert_eq!(config["api_url"], proxy.as_str());

    // Later calls go to the proxy the same way.
    let exec = latch(&state, &["exec", "--", "true"], &env);
    let error: Value = serde_json::from_slice(&exec.stderr).unwrap();
    assert_eq!(error["error"]["code"], "no_host");

    let requests = server.join().unwrap();
    assert!(requests[0].starts_with("POST /v1/latch/peers "));
    assert!(requests[1].starts_with("GET /v1/latch/peers "));
    for request in requests {
        assert!(
            !request.to_ascii_lowercase().contains("authorization"),
            "{request}"
        );
    }
}

#[test]
fn a_cloud_agent_given_its_token_sends_it() {
    let (base, server) = fake_plow(vec![(
        200,
        peer(Value::Null, "agent", "agent", Value::Null),
    )]);
    let dir = test_dir();
    let token_file = dir.path().join("agent.token");
    fs::write(&token_file, "file-credential\n").unwrap();
    let state = dir.path().join("state");
    // `$PLOW_AGENT_TOKEN` comes before the token file.
    let env = [
        ("PLOW_API_BASE", Path::new(&base)),
        ("PLOW_AGENT_TOKEN", Path::new("env-credential")),
        ("PLOW_TOKEN_FILE", token_file.as_path()),
    ];

    let login = latch(&state, &["login"], &env);
    assert_eq!(login.status.code(), Some(0), "{login:?}");
    assert!(!state.join("plow-token").exists());

    let request = &server.join().unwrap()[0];
    assert!(
        request.contains("authorization: Bearer env-credential\r\n"),
        "{request}"
    );
}

#[test]
fn serving_before_any_login_says_to_log_in() {
    let dir = test_dir();
    let area = dir.path().join("shared");
    fs::create_dir(&area).unwrap();
    let state = dir.path().join("never-logged-in");

    let output = latch(
        &state,
        &["serve", "--no-dashboard", area.to_str().unwrap()],
        &[],
    );

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "not_logged_in", "{error}");
    assert!(!state.exists(), "serve created the state directory");
}

#[test]
fn calling_the_host_this_state_dir_serves_names_the_fix() {
    let relay = json!("https://relay.example/");
    let (login_url, login_server) = fake_plow(vec![(
        200,
        peer(Value::Null, "mac", "device", relay.clone()),
    )]);
    let dir = test_dir();
    let owner_token = dir.path().join("owner.token");
    fs::write(&owner_token, "owner-login\n").unwrap();
    let state = dir.path().join("state");
    let args = [
        "login",
        "--api-url",
        &login_url,
        "--token-file",
        owner_token.to_str().unwrap(),
        "--name",
        "mac",
    ];
    let login = latch(&state, &args, &[]);
    assert_eq!(login.status.code(), Some(0), "{login:?}");
    let own_key = serde_json::from_slice::<Value>(&login.stdout).unwrap()["peer"]["key"].clone();
    login_server.join().unwrap();

    // The directory lists this machine as the host, under its own key.
    let (api_url, server) = fake_plow(vec![(
        200,
        json!({"data": [peer(own_key, "mac", "device", relay)]}),
    )]);
    let config_path = state.join("config.json");
    let mut config: Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    config["api_url"] = json!(api_url);
    fs::write(&config_path, config.to_string()).unwrap();

    let exec = latch(&state, &["exec", "--host", "mac", "--", "true"], &[]);
    assert_eq!(exec.status.code(), Some(2), "{exec:?}");
    let error: Value = serde_json::from_slice(&exec.stderr).unwrap();
    assert_eq!(error["error"]["code"], "own_host", "{error}");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("--state-dir"), "{message}");
    server.join().unwrap();
}

#[test]
fn help_says_what_is_allowed_and_how_to_pass_a_command() {
    let dir = test_dir();
    let state = dir.path().join("none");
    // No login and no host needed: the list is in latch itself.
    let help = latch(&state, &["google", "--", "help"], &[]);
    assert_eq!(help.status.code(), Some(0), "{help:?}");
    let allowed: Value = serde_json::from_slice(&help.stdout).unwrap();
    assert!(
        allowed["allowed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "gmail search")
    );

    let missing = latch(&state, &["exec", "--host", "pi", "ls", "-la"], &[]);
    assert_eq!(missing.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&missing.stderr).unwrap();
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("latch exec --host H -- <cmd> <args>"),
        "{message}"
    );

    let exec_help = latch(&state, &["exec", "--help"], &[]);
    let text = String::from_utf8_lossy(&exec_help.stdout);
    assert!(
        text.contains("256 KiB") && text.contains("stdin is empty") && text.contains("exit_code"),
        "{text}"
    );
}

#[test]
fn status_shows_this_machine_once_as_plow_lists_it() {
    let relay = json!("https://relay.example/");
    let (url, server) = fake_plow(vec![(
        200,
        peer(Value::Null, "mac", "device", relay.clone()),
    )]);
    let dir = test_dir();
    let owner_token = dir.path().join("owner.token");
    fs::write(&owner_token, "owner-login\n").unwrap();
    let state = dir.path().join("state");
    let args = [
        "login",
        "--api-url",
        &url,
        "--token-file",
        owner_token.to_str().unwrap(),
        "--name",
        "mac",
    ];
    let login = latch(&state, &args, &[]);
    assert_eq!(login.status.code(), Some(0), "{login:?}");
    let own_key = serde_json::from_slice::<Value>(&login.stdout).unwrap()["peer"]["key"].clone();
    server.join().unwrap();

    // Login saved this machine as online; Plow now lists it offline.
    let mut listed = peer(own_key.clone(), "mac", "device", relay.clone());
    listed["online"] = json!(false);
    let pi = peer(json!("c".repeat(64)), "pi", "device", relay);
    let (url, server) = fake_plow(vec![(200, json!({"data": [listed, pi]}))]);
    let config_path = state.join("config.json");
    let mut config: Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    config["api_url"] = json!(url);
    fs::write(&config_path, config.to_string()).unwrap();

    let status = latch(&state, &["status"], &[]);
    assert_eq!(status.status.code(), Some(0), "{status:?}");
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["peer"]["online"], false, "{status}");
    let hosts = status["hosts"].as_array().unwrap();
    assert_eq!(hosts[0]["key"], own_key);
    assert_eq!(hosts[0]["online"], false);
    assert_eq!(hosts[0]["self"], true);
    assert!(hosts[1].get("self").is_none(), "{status}");
    server.join().unwrap();
}

#[test]
fn exec_stdin_that_is_too_big_or_never_ends_runs_nothing() {
    use std::process::Stdio;
    let dir = test_dir();
    let state = dir.path().join("none");
    let run = |timeout: &str| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_latch"));
        command
            .env_remove("PLOW_TOKEN_FILE")
            .arg("--state-dir")
            .arg(&state)
            .args([
                "exec",
                "--host",
                "pi",
                "--timeout",
                timeout,
                "-i",
                "--",
                "cat",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.spawn().unwrap()
    };
    // More than 256 KiB: refused as soon as the 256 KiB + 1st byte arrives.
    let mut big = run("60");
    let mut pipe = big.stdin.take().unwrap();
    let _ = pipe.write_all(&vec![b'x'; 300 * 1024]);
    let output = big.wait_with_output().unwrap();
    drop(pipe);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "stdin_too_big");

    // A pipe left open: refused once --timeout passes, not run on what came.
    let started = std::time::Instant::now();
    let mut open = run("1");
    let mut pipe = open.stdin.take().unwrap();
    pipe.write_all(b"echo partial").unwrap();
    let output = open.wait_with_output().unwrap();
    drop(pipe);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "stdin_timeout");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

/// A host's own state, logged in as the owner's device `studio`. Returns
/// the test folder, the state and its key.
fn owner_host() -> (tempfile::TempDir, PathBuf, String) {
    let relay = json!("https://relay.example/");
    let (url, server) = fake_plow(vec![(200, peer(Value::Null, "studio", "device", relay))]);
    let dir = test_dir();
    let owner_token = dir.path().join("owner.token");
    fs::write(&owner_token, "owner-login\n").unwrap();
    let state = dir.path().join("state");
    let token = owner_token.to_str().unwrap();
    let args = [
        "login",
        "--api-url",
        &url,
        "--token-file",
        token,
        "--name",
        "studio",
    ];
    let login = latch(&state, &args, &[]);
    assert_eq!(login.status.code(), Some(0), "{login:?}");
    let key = serde_json::from_slice::<Value>(&login.stdout).unwrap()["peer"]["key"]
        .as_str()
        .unwrap()
        .to_string();
    server.join().unwrap();
    (dir, state, key)
}

/// Points `state`'s later Plow calls at `url`.
fn use_plow(state: &Path, url: &str) {
    let config_path = state.join("config.json");
    let mut config: Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    config["api_url"] = json!(url);
    fs::write(&config_path, config.to_string()).unwrap();
}
#[test]
fn rules_on_this_machines_own_host_state_are_set_edited_shown_and_audited() {
    const ADA: &str = "0123456789abcdef0123456789abcdef";
    let ada_key = "a".repeat(64);
    let account = [
        (200, json!([{"uid": ADA, "name": "Ada"}])),
        (
            200,
            json!({"data": [peer(json!(ada_key), "ada-latch", "agent", Value::Null)]}),
        ),
        (
            200,
            json!({"key": ada_key, "kind": "agent", "name": "ada-latch", "agent_uid": ADA,
                   "permissions": {"call": true}}),
        ),
    ];
    let (dir, state, key) = owner_host();
    // `--from`, and the two shows (edit's and the last), each read the account.
    let (url, server) = fake_plow([account.clone(), account.clone(), account].concat());
    use_plow(&state, &url);

    let set = latch(&state, &["rules", "set", "Never send email."], &[]);
    assert_eq!(set.status.code(), Some(0), "{set:?}");
    let set: Value = serde_json::from_slice(&set.stdout).unwrap();
    assert_eq!(set["host"], "studio");
    assert_eq!(set["scope"], "every agent");
    assert_eq!(set["applies"], "applies to the next judgment");

    let rules_file = dir.path().join("bot.txt");
    fs::write(&rules_file, "Only pytest in ~/work.").unwrap();
    let file = rules_file.to_str().unwrap();
    let agent = latch(
        &state,
        &["rules", "set", "--from", "ada-latch", "--file", file],
        &[],
    );
    assert_eq!(agent.status.code(), Some(0), "{agent:?}");
    let agent: Value = serde_json::from_slice(&agent.stdout).unwrap();
    assert_eq!(
        (agent["agent_uid"].as_str(), agent["agent_name"].as_str()),
        (Some(ADA), Some("Ada"))
    );
    assert_eq!(
        fs::read_to_string(state.join(format!("rules/{ADA}.txt"))).unwrap(),
        "Only pytest in ~/work."
    );
    assert_eq!(
        fs::read_to_string(state.join(format!("rules/{ADA}.peer"))).unwrap(),
        ada_key
    );

    // `edit` in $EDITOR: the editor appends a line.
    let editor = dir.path().join("editor.sh");
    fs::write(
        &editor,
        "#!/bin/sh\nprintf '\\nNever buy anything.' >> \"$1\"\n",
    )
    .unwrap();
    fs::set_permissions(&editor, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let edit = latch(&state, &["rules", "edit"], &[("EDITOR", editor.as_path())]);
    assert_eq!(edit.status.code(), Some(0), "{edit:?}");
    assert_eq!(
        fs::read_to_string(state.join("rules/host.txt")).unwrap(),
        "Never send email.\nNever buy anything."
    );

    let show = latch(&state, &["rules", "show"], &[]);
    assert_eq!(show.status.code(), Some(0), "{show:?}");
    let show: Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(
        show["every_agent"]["text"],
        "Never send email.\nNever buy anything."
    );
    assert_eq!(show["agents"][0]["text"], "Only pytest in ~/work.");
    assert_eq!(
        show["warning"],
        "Freeform rules are judged, not guaranteed. --deny-exec is enforced in code."
    );
    server.join().unwrap();

    // Every command is in the host's audit log, by this machine's key, with
    // revisions and never the text.
    let log = fs::read_to_string(state.join("audit.ndjson")).unwrap();
    let records: Vec<Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let commands: Vec<&str> = records
        .iter()
        .map(|r| r["args"]["command"].as_str().unwrap())
        .collect();
    assert_eq!(commands, ["set", "set", "show", "edit", "show"]);
    assert!(
        records
            .iter()
            .all(|r| r["op"] == "rules" && r["caller"] == key.as_str())
    );
    assert_eq!(records[3]["args"]["before"], records[0]["args"]["after"]);
    assert!(!log.contains("email") && !log.contains("pytest"), "{log}");
}

#[test]
fn a_failed_remote_rules_call_writes_no_local_file() {
    let (_dir, state, key) = owner_host();
    let relay = json!("https://relay.example/");
    let (url, server) = fake_plow(vec![(
        200,
        json!({"data": [peer(json!(key), "studio", "device", relay)]}),
    )]);
    use_plow(&state, &url);
    let output = latch(
        &state,
        &["rules", "set", "--host", "pi", "Anything goes."],
        &[],
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "unknown_host");
    assert!(!state.join("rules").exists());
    server.join().unwrap();
}

#[test]
fn an_agent_gets_owner_only_for_rules_on_its_own_state() {
    let (dir, token, server) = logged_in_agent(vec![]);
    server.join().unwrap();
    let state = dir.path().join("state");
    for args in [
        &["rules", "show"][..],
        &["rules", "set", "Anything goes."],
        &["rules", "clear"],
    ] {
        let output = latch(&state, args, &[("PLOW_TOKEN_FILE", token.as_path())]);
        assert_eq!(output.status.code(), Some(4), "{args:?}: {output:?}");
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "owner_only");
    }
    assert!(!state.join("rules").exists());
}

#[test]
fn a_local_rules_command_on_a_broken_audit_log_changes_nothing() {
    let (_dir, state, _key) = owner_host();
    fs::write(state.join("audit.ndjson"), "not a record\n").unwrap();
    let output = latch(&state, &["rules", "set", "Never send email."], &[]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "audit_broken");
    assert!(!state.join("rules/host.txt").exists());
}

#[test]
fn a_local_rules_command_on_an_audit_log_it_cannot_write_changes_nothing() {
    let (_dir, state, _key) = owner_host();
    let show = latch(&state, &["rules", "show"], &[]);
    assert_eq!(show.status.code(), Some(0), "{show:?}");
    let log = state.join("audit.ndjson");
    fs::set_permissions(&log, std::os::unix::fs::PermissionsExt::from_mode(0o400)).unwrap();
    let output = latch(&state, &["rules", "set", "Never send email."], &[]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "audit_io");
    assert!(!state.join("rules/host.txt").exists());
}
