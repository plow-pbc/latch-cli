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
    latch_command(state, args, env).output().unwrap()
}

fn latch_command(state: &Path, args: &[&str], env: &[(&str, &Path)]) -> Command {
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
    command
}

/// Runs latch with its stdin and stderr on a terminal (a pty) with `typed`
/// already typed, as a person would. Returns the exit code and everything
/// the terminal showed.
fn latch_at_a_terminal(state: &Path, args: &[&str], typed: &str) -> (Option<i32>, String) {
    use std::os::fd::FromRawFd;
    let (mut main, mut side) = (0, 0);
    let (name, settings, size) = (
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    );
    // SAFETY: openpty fills in two new descriptors, which we then own.
    let opened = unsafe { libc::openpty(&mut main, &mut side, name, settings, size) };
    assert_eq!(opened, 0);
    let (mut main, side) = unsafe { (fs::File::from_raw_fd(main), fs::File::from_raw_fd(side)) };
    let mut command = latch_command(state, args, &[]);
    command
        .stdin(side.try_clone().unwrap())
        .stderr(side.try_clone().unwrap())
        .stdout(std::process::Stdio::null());
    let mut child = command.spawn().unwrap();
    drop(command);
    drop(side);
    main.write_all(typed.as_bytes()).unwrap();
    let mut reading = main.try_clone().unwrap();
    let shown = thread::spawn(move || {
        let mut shown = Vec::new();
        // Ends with an error once the last holder of the other side exits.
        let _ = reading.read_to_end(&mut shown);
        String::from_utf8_lossy(&shown).to_string()
    });
    let code = child.wait().unwrap().code();
    drop(main);
    (code, shown.join().unwrap())
}

/// Plow's answer to `GET /v1/agents/me`.
fn me(name: &str) -> (u16, Value) {
    let agent = json!({"uid": "0123456789abcdef0123456789abcdef", "name": name});
    (200, json!({"agent": agent}))
}

fn body(request: &str) -> Value {
    serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
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
        me("research-agent"),
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
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("GET /v1/agents/me "));
    assert_eq!(body(&requests[1])["name"], "research-agent");
    assert!(requests[2].starts_with("GET /v1/latch/peers "));
    assert!(requests[2].contains("authorization: Bearer agent-credential\r\n"));
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
    let (api_url, server) = fake_plow([vec![me("research-agent"), login], answers].concat());
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
    assert_eq!(requests.len(), 4);
    assert!(
        requests[3].starts_with("POST /v1/latch/peers "),
        "{}",
        requests[3]
    );
    let again = body(&requests[3]);
    assert_eq!(again["key"], body(&requests[1])["key"]);
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
    assert_eq!(server.join().unwrap().len(), 4);
}

#[test]
fn a_cloud_agent_behind_plows_proxy_sends_no_credential() {
    // On exe.dev the agent's token never reaches the VM: `$PLOW_API_BASE`
    // is a proxy that adds it. It wins over `$PLOW_API_URL`.
    let (proxy, server) = fake_plow(vec![
        me("agent"),
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
    assert!(requests[0].starts_with("GET /v1/agents/me "));
    assert!(requests[1].starts_with("POST /v1/latch/peers "));
    assert!(requests[2].starts_with("GET /v1/latch/peers "));
    for request in requests {
        assert!(
            !request.to_ascii_lowercase().contains("authorization"),
            "{request}"
        );
    }
}

#[test]
fn a_cloud_agent_given_its_token_sends_it() {
    let (base, server) = fake_plow(vec![
        me("agent"),
        (200, peer(Value::Null, "agent", "agent", Value::Null)),
    ]);
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

    for request in server.join().unwrap() {
        assert!(
            request.contains("authorization: Bearer env-credential\r\n"),
            "{request}"
        );
    }
}

#[test]
fn an_agent_with_neither_a_token_a_file_nor_a_proxy_has_no_credential() {
    let dir = test_dir();
    let env = [("PLOW_TOKEN_FILE", dir.path().join("none"))];
    let env = [(env[0].0, env[0].1.as_path())];
    let args = ["login", "--api-url", "http://127.0.0.1:9"];
    let login = latch(&dir.path().join("state"), &args, &env);
    assert_eq!(login.status.code(), Some(2), "{login:?}");
    let error: Value = serde_json::from_slice(&login.stderr).unwrap();
    assert_eq!(error["error"]["code"], "no_credential");
}

#[test]
fn an_agent_registers_under_its_plow_name_or_with_its_uid_on_a_clash() {
    let (api_url, server) = fake_plow(vec![
        me("Research bot"),
        (409, json!({"detail": "taken"})),
        (
            200,
            peer(Value::Null, "Research bot-012345", "agent", Value::Null),
        ),
    ]);
    let dir = test_dir();
    let token_file = dir.path().join("agent.token");
    fs::write(&token_file, "agent-credential\n").unwrap();
    let env = [("PLOW_TOKEN_FILE", token_file.as_path())];
    let login = latch(
        &dir.path().join("state"),
        &["login", "--api-url", &api_url],
        &env,
    );
    assert_eq!(login.status.code(), Some(0), "{login:?}");

    let requests = server.join().unwrap();
    assert_eq!(body(&requests[1])["name"], "Research bot");
    assert_eq!(body(&requests[2])["name"], "Research bot-012345");
    assert_eq!(body(&requests[1])["key"], body(&requests[2])["key"]);
}

#[test]
fn an_agent_given_a_name_stops_on_a_clash() {
    let (api_url, server) = fake_plow(vec![me("Research bot"), (409, json!({}))]);
    let dir = test_dir();
    let token_file = dir.path().join("agent.token");
    fs::write(&token_file, "agent-credential\n").unwrap();
    let env = [("PLOW_TOKEN_FILE", token_file.as_path())];
    let args = ["login", "--api-url", &api_url, "--name", "bot"];
    let login = latch(&dir.path().join("state"), &args, &env);
    assert_eq!(login.status.code(), Some(2), "{login:?}");
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(body(&requests[1])["name"], "bot");
}

#[test]
fn a_machine_is_named_after_its_hostname_and_stops_on_a_clash() {
    let uname = Command::new("uname").arg("-n").output().unwrap();
    let hostname = String::from_utf8(uname.stdout).unwrap();
    let hostname = hostname.trim().split('.').next().unwrap().to_string();
    let holder = peer(json!("c".repeat(64)), &hostname, "device", Value::Null);
    // Taken, and Plow lists who holds it.
    let (api_url, server) = fake_plow(vec![(409, json!({})), (200, json!({"data": [holder]}))]);
    let dir = test_dir();
    let owner_token = dir.path().join("owner.token");
    fs::write(&owner_token, "owner-login\n").unwrap();
    let state = dir.path().join("state");
    let args = [
        "login",
        "--api-url",
        &api_url,
        "--token-file",
        owner_token.to_str().unwrap(),
    ];
    let login = latch(&state, &args, &[]);
    assert_eq!(login.status.code(), Some(2), "{login:?}");
    let error: Value = serde_json::from_slice(&login.stderr).unwrap();
    assert_eq!(error["error"]["code"], "conflict", "{error}");
    assert!(!state.join("config.json").exists());

    // One registration, never retried to take the name over.
    let requests = server.join().unwrap();
    assert_eq!(body(&requests[0])["name"], hostname.as_str());
    assert!(body(&requests[0]).get("take_over").is_none());
    assert_eq!(requests.len(), 2);
    assert!(requests[1].starts_with("GET /v1/latch/peers "));
}

#[test]
fn a_name_clash_stops_a_login_at_a_terminal_too() {
    let holder = peer(json!("c".repeat(64)), "mac", "device", Value::Null);
    let (api_url, server) = fake_plow(vec![(409, json!({})), (200, json!({"data": [holder]}))]);
    let dir = test_dir();
    let owner_token = dir.path().join("owner.token");
    fs::write(&owner_token, "owner-login\n").unwrap();
    let state = dir.path().join("state");
    let owner = owner_token.to_str().unwrap();
    let args = [
        "login",
        "--api-url",
        &api_url,
        "--token-file",
        owner,
        "--name",
        "mac",
    ];

    // Even a person ready to say yes is not asked to replace the machine.
    let (code, shown) = latch_at_a_terminal(&state, &args, "y\n");
    assert_eq!(code, Some(2), "{shown}");
    assert!(shown.contains("\"conflict\""), "{shown}");
    assert!(shown.contains("latch devices remove mac"), "{shown}");
    assert!(!shown.contains("Replace it?"), "{shown}");
    assert!(!state.join("config.json").exists());
    assert_eq!(server.join().unwrap().len(), 2);
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

/// One HTTP request as a fake reads it: the head (request line and
/// headers, lowercased names) and the body.
struct Http {
    head: String,
    body: Vec<u8>,
}

impl Http {
    fn read(reader: &mut impl BufRead) -> Option<Http> {
        let mut head = String::new();
        let mut length = 0;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).ok()? == 0 {
                return None;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().ok()?;
            }
            head.push_str(&line);
            if line == "\r\n" {
                break;
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).ok()?;
        Some(Http { head, body })
    }

    fn line(&self) -> &str {
        self.head.lines().next().unwrap_or_default()
    }

    fn bearer(&self) -> Option<String> {
        self.head.lines().find_map(|line| {
            let lower = line.to_ascii_lowercase();
            lower
                .strip_prefix("authorization: bearer ")
                .map(|_| line["authorization: bearer ".len()..].trim().to_string())
        })
    }
}

fn respond(socket: &mut impl Write, status: u16, body: &Value) {
    let body = body.to_string();
    let head = format!(
        "HTTP/1.1 {status} Fake\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = socket.write_all(head.as_bytes());
    let _ = socket.write_all(body.as_bytes());
}

const AGENT_UID: &str = "0123456789abcdef0123456789abcdef";

/// A fake Plow with state, for as many requests as come: one owner login
/// and one agent whose credential can be rotated. Like Plow, it lists and
/// looks up only peers whose credential is still live, and a registration
/// with a live credential replaces a dead row with the same key.
#[derive(Clone)]
struct LivePlow {
    url: String,
    state: std::sync::Arc<std::sync::Mutex<LiveState>>,
}

struct LiveState {
    owner: String,
    agent: String,
    /// Each peer with the credential that registered it.
    peers: Vec<(Value, String)>,
    /// Request lines and their bearers, in order.
    seen: Vec<(String, Option<String>)>,
}

impl LivePlow {
    fn start() -> LivePlow {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let plow = LivePlow {
            url: format!("http://{}", listener.local_addr().unwrap()),
            state: std::sync::Arc::new(std::sync::Mutex::new(LiveState {
                owner: "owner-login".to_string(),
                agent: "agent-credential".to_string(),
                peers: Vec::new(),
                seen: Vec::new(),
            })),
        };
        let serving = plow.clone();
        thread::spawn(move || {
            for socket in listener.incoming() {
                let plow = serving.clone();
                thread::spawn(move || {
                    let mut socket = socket.unwrap();
                    let mut reader = BufReader::new(socket.try_clone().unwrap());
                    if let Some(request) = Http::read(&mut reader) {
                        let (status, body) = plow.answer(&request);
                        respond(&mut socket, status, &body);
                    }
                });
            }
        });
        plow
    }

    /// The agent's credential is replaced: the old one stops working, and
    /// its peer is no longer live.
    fn rotate(&self, new: &str) {
        self.state.lock().unwrap().agent = new.to_string();
    }

    fn seen(&self) -> Vec<(String, Option<String>)> {
        self.state.lock().unwrap().seen.clone()
    }

    fn answer(&self, request: &Http) -> (u16, Value) {
        let mut state = self.state.lock().unwrap();
        let bearer = request.bearer();
        state
            .seen
            .push((request.line().to_string(), bearer.clone()));
        let token = bearer.unwrap_or_default();
        let kind = if token == state.owner {
            "device"
        } else if token == state.agent {
            "agent"
        } else {
            return (401, json!({}));
        };
        let live = |state: &LiveState, credential: &String| {
            *credential == state.owner || *credential == state.agent
        };
        let mut words = request.line().split(' ');
        let (method, path) = (words.next().unwrap(), words.next().unwrap());
        match (method, path) {
            ("POST", "/v1/latch/peers") => {
                let sent: Value = serde_json::from_slice(&request.body).unwrap();
                let name = sent["name"].as_str().unwrap_or("agent");
                let peer = peer(sent["key"].clone(), name, kind, sent["relay_url"].clone());
                let taken = state.peers.iter().any(|(other, credential)| {
                    other["key"] == sent["key"] && live(&state, credential) && *credential != token
                });
                if taken {
                    return (409, json!({}));
                }
                state.peers.retain(|(other, _)| other["key"] != sent["key"]);
                state.peers.push((peer.clone(), token));
                (200, peer)
            }
            ("GET", "/v1/latch/peers") => {
                let listed: Vec<Value> = state
                    .peers
                    .iter()
                    .filter(|(_, credential)| live(&state, credential))
                    .map(|(peer, _)| peer.clone())
                    .collect();
                (200, json!({"data": listed}))
            }
            ("GET", "/v1/agents") if kind == "device" => {
                (200, json!([{"uid": AGENT_UID, "name": "Research bot"}]))
            }
            ("GET", "/v1/agents/me") if kind == "agent" => (
                200,
                json!({"agent": {"uid": AGENT_UID, "name": "Research bot"}}),
            ),
            ("GET", path) if path.starts_with("/v1/latch/peers/") => {
                let key = &path["/v1/latch/peers/".len()..];
                let found = state
                    .peers
                    .iter()
                    .find(|(peer, credential)| peer["key"] == key && live(&state, credential));
                match found {
                    Some((peer, _)) => {
                        let uid = (peer["kind"] == "agent").then_some(AGENT_UID);
                        (
                            200,
                            json!({"key": key, "kind": peer["kind"], "name": peer["name"],
                                   "agent_uid": uid, "permissions": {"call": true}}),
                        )
                    }
                    None => (404, json!({})),
                }
            }
            ("POST", path) if path.ends_with("/offline") => (200, json!({})),
            // What the host's gatekeeper reads, on the owner's login: the
            // agent's one chat, where the owner asked for it, and a judge
            // that agrees.
            ("GET", path) if kind == "device" && path == format!("/v1/agents/{AGENT_UID}") => (
                200,
                json!({"uid": AGENT_UID, "name": "Research bot", "line": {"uid": "line_agent"}}),
            ),
            ("GET", "/v1/chats") if kind == "device" => (
                200,
                json!({"data": [{"uid": "chat_a", "participants": [
                    {"type": "agent", "relationship": "self", "line": {"uid": "line_agent"}}]}],
                       "has_more": false}),
            ),
            ("GET", path) if kind == "device" && path.starts_with("/v1/chats/chat_a/messages") => (
                200,
                json!({"data": [{"uid": "m1", "body": "echo whatever you like on the host",
                                 "created_at": "2026-10-20T12:00:00Z",
                                 "sender": {"type": "member", "display_name": "Owner", "role": "owner"}}],
                       "has_more": false}),
            ),
            ("POST", "/v1/decisions") if kind == "device" => (
                200,
                json!({"id": "req_test", "model": "typesafe/jev-1.14", "answers": {"decision": {
                    "type": "choice", "choice": "COVERED",
                    "probabilities": {"COVERED": 0.9, "NOT_COVERED": 0.08, "FORBIDDEN": 0.02}}},
                    "usage": {"input_tokens": 1, "output_tokens": 0}}),
            ),
            _ => (404, json!({})),
        }
    }
}

/// A stand-in for exe.dev's proxy: every request it forwards to `upstream`
/// gains the agent's credential as a bearer, as the VM never holds it.
fn adding_proxy(upstream: &str, credential: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = upstream.trim_start_matches("http://").to_string();
    let header = format!("Authorization: Bearer {credential}\r\n");
    thread::spawn(move || {
        for socket in listener.incoming() {
            let mut socket = socket.unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let Some(request) = Http::read(&mut reader) else {
                continue;
            };
            let (line, rest) = request.head.split_once("\r\n").unwrap();
            let mut forwarded = std::net::TcpStream::connect(&upstream).unwrap();
            let head = format!("{line}\r\n{header}{rest}");
            forwarded.write_all(head.as_bytes()).unwrap();
            forwarded.write_all(&request.body).unwrap();
            let mut answer = Vec::new();
            forwarded.read_to_end(&mut answer).unwrap();
            socket.write_all(&answer).unwrap();
        }
    });
    url
}

#[test]
fn a_cloud_agent_logs_in_through_a_proxy_that_adds_its_credential() {
    let plow = LivePlow::start();
    let proxy = adding_proxy(&plow.url, "agent-credential");
    let dir = test_dir();
    let state = dir.path().join("state");
    let env = [
        ("PLOW_API_BASE", Path::new(&proxy)),
        ("PLOW_TOKEN_FILE", &dir.path().join("none")),
    ];

    let login = latch(&state, &["login"], &env);
    assert_eq!(login.status.code(), Some(0), "{login:?}");
    let output: Value = serde_json::from_slice(&login.stdout).unwrap();
    assert_eq!(output["peer"]["kind"], "agent");
    assert_eq!(output["peer"]["name"], "Research bot");
    let status = latch(&state, &["status"], &env);
    assert_eq!(status.status.code(), Some(0), "{status:?}");

    // Plow saw the proxy's credential on every call; latch sent none.
    let seen = plow.seen();
    assert!(seen.len() >= 3, "{seen:?}");
    for (line, bearer) in seen {
        assert_eq!(bearer.as_deref(), Some("agent-credential"), "{line}");
    }
}

/// An iroh relay on this machine, over plain HTTP, kept running by the
/// returned runtime. Returns its URL.
fn local_relay() -> (tokio::runtime::Runtime, String) {
    use iroh_relay::server::{RelayConfig, Server, ServerConfig};
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut config = ServerConfig::default();
    config.relay = Some(RelayConfig::new(([127, 0, 0, 1], 0)));
    let server = runtime.block_on(Server::spawn(config)).unwrap();
    let url = format!("http://{}", server.http_addr().unwrap());
    // The server stops when dropped; it lives as long as the runtime.
    runtime.spawn(async move {
        let _server = server;
        std::future::pending::<()>().await
    });
    (runtime, url)
}

/// A running `latch serve`, stopped when dropped: when the test ends,
/// passing or not.
struct Serving(std::process::Child);

impl Drop for Serving {
    fn drop(&mut self) {
        // SAFETY: signals our own child.
        unsafe { libc::kill(self.0.id() as i32, libc::SIGTERM) };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while matches!(self.0.try_wait(), Ok(None)) {
            if std::time::Instant::now() > deadline {
                let _ = self.0.kill();
                break;
            }
            thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = self.0.wait();
    }
}

/// `latch serve` on `host`'s state, sharing `share`, through the local
/// relay, once it has registered that relay with Plow.
fn serving(host: &Path, share: &Path, relay: &str) -> Serving {
    let mut serve = Serving(
        Command::new(env!("CARGO_BIN_EXE_latch"))
            .args([
                "--state-dir",
                host.to_str().unwrap(),
                "serve",
                "--no-dashboard",
            ])
            .arg(share)
            .env("LATCH_TEST_RELAY_URL", relay)
            .env_remove("LATCH_HOST")
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut log = BufReader::new(serve.0.stderr.take().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(log.read_line(&mut line).unwrap(), 0, "serve stopped");
        if line.contains("heartbeat: registered") {
            break;
        }
    }
    thread::spawn(move || std::io::copy(&mut log, &mut std::io::sink()));
    serve
}

#[test]
fn a_rotated_agent_registers_again_and_its_next_call_runs() {
    let plow = LivePlow::start();
    let (_relay, relay) = local_relay();
    let dir = test_dir();
    let share = dir.path().join("share");
    fs::create_dir(&share).unwrap();
    let owner_token = dir.path().join("owner.token");
    fs::write(&owner_token, "owner-login\n").unwrap();
    let agent_token = dir.path().join("agent.token");
    fs::write(&agent_token, "agent-credential\n").unwrap();
    let (host, agent) = (dir.path().join("host"), dir.path().join("agent"));
    let agent_env = [("PLOW_TOKEN_FILE", agent_token.as_path())];
    let owner = owner_token.to_str().unwrap();
    let args = [
        "login",
        "--api-url",
        &plow.url,
        "--token-file",
        owner,
        "--name",
        "host",
    ];
    assert_eq!(latch(&host, &args, &[]).status.code(), Some(0));
    let login = latch(&agent, &["login", "--api-url", &plow.url], &agent_env);
    assert_eq!(login.status.code(), Some(0), "{login:?}");

    let serve = serving(&host, &share, &relay);
    let exec = |word: &str| {
        latch(
            &agent,
            &["exec", "--host", "host", "--", "echo", word],
            &agent_env,
        )
    };

    let before = exec("before");
    assert_eq!(before.status.code(), Some(0), "{before:?}");

    // Rotated: Plow forgets the agent's peer until it registers again.
    plow.rotate("agent-rotated");
    fs::write(&agent_token, "agent-rotated\n").unwrap();
    let after = exec("after");
    drop(serve);
    assert_eq!(after.status.code(), Some(0), "{after:?}");
    let result: Value = serde_json::from_slice(&after.stdout).unwrap();
    assert_eq!(result["stdout"], "after\n");
    let seen = plow.seen();
    let again = seen.iter().position(|(line, bearer)| {
        line.starts_with("POST /v1/latch/peers ") && bearer.as_deref() == Some("agent-rotated")
    });
    assert!(
        again.is_some(),
        "no registration with the new credential: {seen:?}"
    );
}

#[test]
fn the_owners_rules_commands_reach_a_host_and_an_agents_never_do() {
    let plow = LivePlow::start();
    let (_relay, relay) = local_relay();
    let dir = test_dir();
    let share = dir.path().join("share");
    fs::create_dir(&share).unwrap();
    let owner_token = dir.path().join("owner.token");
    fs::write(&owner_token, "owner-login\n").unwrap();
    let agent_token = dir.path().join("agent.token");
    fs::write(&agent_token, "agent-credential\n").unwrap();
    let (host, owner, agent) = (
        dir.path().join("host"),
        dir.path().join("owner"),
        dir.path().join("agent"),
    );
    let agent_env = [("PLOW_TOKEN_FILE", agent_token.as_path())];
    for (state, name) in [(&host, "host"), (&owner, "owner-mac")] {
        let token = owner_token.to_str().unwrap();
        let args = [
            "login",
            "--api-url",
            &plow.url,
            "--token-file",
            token,
            "--name",
            name,
        ];
        assert_eq!(latch(state, &args, &[]).status.code(), Some(0), "{name}");
    }
    let login = latch(&agent, &["login", "--api-url", &plow.url], &agent_env);
    assert_eq!(login.status.code(), Some(0), "{login:?}");
    let serve = serving(&host, &share, &relay);

    let run = |state: &Path, args: &[&str], env: &[(&str, &Path)]| {
        let output = latch(state, args, env);
        let printed = if output.status.success() {
            &output.stdout
        } else {
            &output.stderr
        };
        let json = serde_json::from_slice::<Value>(printed).unwrap_or(Value::Null);
        (output.status.code(), json)
    };
    let rules_file = dir.path().join("bot.txt");
    fs::write(&rules_file, "Run tests in the shared folder.").unwrap();
    let editor = dir.path().join("editor.sh");
    fs::write(&editor, "#!/bin/sh\nprintf ' Nothing else.' >> \"$1\"\n").unwrap();
    fs::set_permissions(&editor, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let file = rules_file.to_str().unwrap();
    let owner_runs = [
        run(
            &owner,
            &["rules", "set", "--host", "host", "Never send email."],
            &[],
        ),
        run(
            &owner,
            &[
                "rules",
                "set",
                "--host",
                "host",
                "--from",
                "Research bot",
                "--file",
                file,
            ],
            &[],
        ),
        run(
            &owner,
            &["rules", "edit", "--host", "host", "--from", "Research bot"],
            &[("EDITOR", editor.as_path())],
        ),
        run(&owner, &["rules", "show", "--host", "host"], &[]),
    ];
    let agent_runs: Vec<_> = [
        &["rules", "show", "--host", "host"][..],
        &["rules", "set", "--host", "host", "Anything goes."],
        &["rules", "edit", "--host", "host"],
        &["rules", "clear", "--host", "host", "--from", "Research bot"],
    ]
    .iter()
    .map(|args| run(&agent, args, &[("EDITOR", editor.as_path()), agent_env[0]]))
    .collect();
    let rules_after_agent = fs::read_to_string(host.join(format!("rules/{AGENT_UID}.txt")));
    let cleared = run(
        &owner,
        &["rules", "clear", "--host", "host", "--from", "Research bot"],
        &[],
    );

    drop(serve);

    for (code, json) in &owner_runs {
        assert_eq!(*code, Some(0), "{json}");
    }
    let [set, set_agent, edited, shown] = owner_runs.map(|(_, json)| json);
    assert_eq!(
        (set["host"].as_str(), set["scope"].as_str()),
        (Some("host"), Some("every agent"))
    );
    assert_eq!(set_agent["agent_uid"], AGENT_UID);
    assert_eq!(set_agent["agent_name"], "Research bot");
    assert_eq!(edited["agent_uid"], AGENT_UID);
    let edited_text = "Run tests in the shared folder. Nothing else.";
    assert_eq!(shown["every_agent"]["text"], "Never send email.");
    assert_eq!(shown["agents"][0]["text"], edited_text);
    assert_eq!(shown["agents"][0]["agent_name"], "Research bot");
    for (code, json) in &agent_runs {
        assert_eq!(*code, Some(4), "{json}");
        assert_eq!(json["error"]["code"], "owner_only", "{json}");
    }
    assert_eq!(rules_after_agent.unwrap(), edited_text);
    assert_eq!(cleared.0, Some(0), "{}", cleared.1);
    assert_eq!(cleared.1["cleared"], true);
    assert!(!host.join(format!("rules/{AGENT_UID}.txt")).exists());
    assert_eq!(
        fs::read_to_string(host.join("rules/host.txt")).unwrap(),
        "Never send email."
    );
    // Nothing was written on the machines the commands came from.
    assert!(!owner.join("rules").exists() && !agent.join("rules").exists());

    // The host audited every command by revision, never by text.
    let log = fs::read_to_string(host.join("audit.ndjson")).unwrap();
    let records: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|record: &Value| record["op"] == "rules")
        .collect();
    let decisions: Vec<(&str, &str)> = records
        .iter()
        .map(|r| {
            (
                r["args"]["command"].as_str().unwrap(),
                r["decision"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        decisions,
        [
            ("set", "allow"),
            ("set", "allow"),
            ("show", "allow"),
            ("edit", "allow"),
            ("show", "allow"),
            ("show", "deny"),
            ("set", "deny"),
            ("show", "deny"),
            ("clear", "deny"),
            ("clear", "allow"),
        ]
    );
    assert!(
        !log.contains("email") && !log.contains("shared folder"),
        "{log}"
    );
}
