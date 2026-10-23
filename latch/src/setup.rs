//! `latch setup`: one pass, at a terminal, through what a new machine
//! needs: a login, Google, on macOS Messages and Contacts, and a running
//! host. None of the steps before the last needs a host running; the last
//! offers to start one. It is thin: every step runs an existing latch command (and says
//! which), so a person can do any step by hand later. Each step first looks
//! whether it is done already and then skips it, so setup can be run again
//! at any time. Skipping or failing a step never stops the next one.

use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
#[cfg(target_os = "macos")]
use std::sync::mpsc::RecvTimeoutError;
use std::sync::mpsc::{self, Receiver};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use crate::failure::{Failure, code};
use crate::state::StateDir;

/// How often, and how long, the Full Disk Access step checks again.
#[cfg(target_os = "macos")]
const RECHECK_EVERY: Duration = Duration::from_secs(2);
#[cfg(target_os = "macos")]
const RECHECK_FOR: Duration = Duration::from_secs(180);

pub fn run(state_dir: Option<PathBuf>) -> ExitCode {
    let mut setup = Setup {
        latch: latch_program(),
        state_dir,
        input: None,
    };
    say("Setting up latch. Each step runs a latch command; you can run it yourself later.");
    if setup.log_in() {
        setup.google();
        #[cfg(target_os = "macos")]
        setup.messages_and_contacts();
        setup.start();
    }
    say("Done. Run `latch setup` again any time.");
    ExitCode::SUCCESS
}

/// The latch this setup runs its commands with: itself.
/// `LATCH_SETUP_LATCH` names another, for tests.
fn latch_program() -> PathBuf {
    std::env::var_os("LATCH_SETUP_LATCH")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("latch"))
}

struct Setup {
    latch: PathBuf,
    state_dir: Option<PathBuf>,
    /// Started at the first question, after `latch login`: login asks its
    /// own questions at this terminal, and a reader started earlier would
    /// take its answers.
    input: Option<Input>,
}

/// What a latch command printed, read as JSON: on stdout if it worked, its
/// error on stderr if it did not.
struct Ran {
    ok: bool,
    json: Value,
}

impl Setup {
    /// `latch ARGS…` on this state folder, for its JSON answer.
    fn latch(&self, args: &[&str]) -> Ran {
        let output = self.command(args).stdin(Stdio::null()).output();
        match output {
            Ok(output) => {
                let printed = if output.status.success() {
                    &output.stdout
                } else {
                    &output.stderr
                };
                // The last line: anything before it is a person's text.
                let json = String::from_utf8_lossy(printed)
                    .lines()
                    .rev()
                    .find_map(|line| serde_json::from_str(line).ok())
                    .unwrap_or(Value::Null);
                Ran {
                    ok: output.status.success(),
                    json,
                }
            }
            Err(err) => Ran {
                ok: false,
                json: serde_json::json!({"error": {"message": err.to_string()}}),
            },
        }
    }

    fn input(&mut self) -> &Input {
        self.input.get_or_insert_with(Input::start)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.latch);
        if let Some(dir) = &self.state_dir {
            command.arg("--state-dir").arg(dir);
        }
        // Setup is about this machine: `plugins list` must not ask another.
        command.env_remove("LATCH_HOST").args(args);
        command
    }

    /// `latch status --local`.
    fn local(&self) -> Value {
        self.latch(&["status", "--local"]).json
    }

    /// Step 1: a login, by text at this terminal.
    fn log_in(&mut self) -> bool {
        if let Some(name) = self.local()["peer"]["name"].as_str() {
            done(&format!("Logged in as {name}."));
            return true;
        }
        step("Log in", "latch login");
        // Its questions, a login by text's screen and what went wrong are
        // the person's, at this terminal; setup goes by its exit status. A
        // login that asked no one answers JSON, read for the name.
        let login = self
            .command(&["login"])
            .stdin(Stdio::inherit())
            .stderr(Stdio::inherit())
            .output();
        if let Ok(output) = login
            && output.status.success()
        {
            let answer: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
            if let Some(name) = answer["peer"]["name"].as_str() {
                done(&format!("Logged in as {name}."));
            }
            return true;
        }
        say(
            "Not logged in, so the other steps wait: run `latch setup` again once `latch login` works.",
        );
        false
    }

    /// A connector's state: as the host lists it when it runs, else as it
    /// last saved it in the state folder's plugins.json.
    fn plugin_state(&self, name: &str) -> Option<String> {
        if self.local()["serve"]["ready"] == true {
            let listed = self.latch(&["plugins", "list"]);
            return listed.json["plugins"]
                .as_array()?
                .iter()
                .find(|plugin| plugin["name"] == name)
                .and_then(|plugin| plugin["state"].as_str())
                .map(String::from);
        }
        let state = StateDir::new(self.state_dir.clone()).ok()?;
        let saved = std::fs::read_to_string(state.path.join("plugins.json")).ok()?;
        let saved: Value = serde_json::from_str(&saved).ok()?;
        saved[name]["state"].as_str().map(String::from)
    }

    /// Step 2: Google, through Plow.
    fn google(&mut self) {
        if self.plugin_state("google").as_deref() == Some("enabled") {
            done("Google is connected.");
            return;
        }
        if !self.input().yes("Connect Google? [Y/n] ", true) {
            say("Skipped; later: `latch plugins enable google`.");
            return;
        }
        step("Connect Google", "latch plugins enable google");
        // At this terminal, so a Google sign-in shows its link and waits for
        // the account; a failure says why there itself. Its exit decides.
        let enabled = self
            .command(&["plugins", "enable", "google"])
            .stdin(Stdio::inherit())
            .stderr(Stdio::inherit())
            .stdout(Stdio::piped())
            .output();
        // Google is on only once the enable printed its result.
        if enabled.is_ok_and(|output| output.status.success() && !output.stdout.is_empty()) {
            done("Google is connected.");
        } else {
            eprintln!("✗ Google is not on yet.\n  Try again later: latch plugins enable google");
        }
    }

    /// Step 3: Messages and Contacts: Full Disk Access for the app the host
    /// runs in, checked as that app again and again while the owner turns it
    /// on, so neither Enter nor a restart is needed.
    #[cfg(target_os = "macos")]
    fn messages_and_contacts(&mut self) {
        let names = ["messages", "contacts"];
        let mut pending: Vec<&str> = names
            .into_iter()
            .filter(|name| self.plugin_state(name).as_deref() != Some("enabled"))
            .collect();
        if pending.is_empty() {
            done("Messages and Contacts are on.");
            return;
        }
        if !self
            .input()
            .yes("Enable Messages and Contacts on this Mac? [y/N] ", false)
        {
            say("Skipped; later: `latch plugins enable messages` (and contacts).");
            return;
        }
        let mut shown = false;
        let started = Instant::now();
        loop {
            pending.retain(|name| {
                let command = format!("latch plugins enable {name}");
                let enabled = self.latch(&["plugins", "enable", name]);
                if enabled.ok {
                    done(&format!("{} is on ({command}).", title(name)));
                    return false;
                }
                // Only a missing grant is worth waiting for; any other
                // failure is this connector's to retry later.
                let error = &enabled.json["error"];
                if error["access"] != "not_granted" {
                    failed(&enabled.json, &command);
                    return false;
                }
                if !shown {
                    shown = true;
                    show_grant(error);
                }
                true
            });
            if pending.is_empty() {
                return;
            }
            if started.elapsed() >= RECHECK_FOR {
                say(
                    "Not granted yet; finish later with `latch plugins enable messages` (and contacts).",
                );
                return;
            }
            if self.input().skip_within(RECHECK_EVERY) {
                say("Skipped; finish later with `latch plugins enable messages` (and contacts).");
                return;
            }
        }
    }

    /// Step 4, last: a running host, if the owner wants one now. `--again`
    /// once a host ran here, so it keeps the flags its owner gave it
    /// (`--deny-exec`).
    fn start(&mut self) {
        if self.local()["serve"]["ready"] == true {
            done("latch is running.");
            return;
        }
        if !self.input().yes("Start latch now? [Y/n] ", true) {
            say(
                "Not started; later: `latch serve`, or `latch serve -d` to run it in the background.",
            );
            return;
        }
        let args = match StateDir::new(self.state_dir.clone()) {
            Ok(state) => start_args(&state),
            Err(_) => vec!["serve", "-d"],
        };
        let command = format!("latch {}", args.join(" "));
        step("Start latch", &command);
        let started = self.latch(&args);
        if started.ok {
            done("latch is running.");
        } else {
            failed(&started.json, &command);
        }
    }
}

/// What starts this state folder's host in the background: with `--again`
/// once a host ran here, so it keeps the flags its owner gave it.
fn start_args(state: &StateDir) -> Vec<&'static str> {
    if state.path.join(crate::managed::ARGS_FILE).exists() {
        vec!["serve", "-d", "--again"]
    } else {
        vec!["serve", "-d"]
    }
}

/// Without a terminal nobody answers setup's questions: `no_terminal`,
/// whose `next` is the first step `setup --status` says is not done (its
/// command, or where to connect an account first), or `latch status` once
/// every step is.
pub fn no_terminal(status: &Status) -> Failure {
    let next = status
        .steps
        .iter()
        .filter(|step| step.state != StepState::Done)
        .find_map(|step| step.args.as_ref())
        .map(|args| format!("latch {}", args.join(" ")))
        .unwrap_or_else(|| "latch status".to_string());
    let message = "latch setup asks questions, so it needs a terminal".to_string();
    let mut failure = Failure::new(code::no_terminal, message);
    failure.next = Some(next.into());
    failure
}

/// `latch setup --status`: each step of setup, read from the state folder
/// and, for Google, from Plow (read only), so it changes nothing and needs no
/// host or terminal.
#[derive(Serialize)]
pub struct Status {
    pub steps: Vec<Step>,
}

#[derive(Serialize)]
pub struct Step {
    /// `login`, each connector this build has, then `host`.
    pub step: &'static str,
    pub state: StepState,
    pub reason: String,
    /// The words after `latch` that do this step, while it is not done.
    pub args: Option<Vec<&'static str>>,
    /// A connector's: its owner turned it on, and its setup passed then.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// A connector's: enabled, and a host is serving it now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready: Option<bool>,
    /// Google's: the accounts connected in Plow, when Plow said.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accounts: Option<Vec<String>>,
    /// Google's, when none is connected: where the owner connects one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect_url: Option<String>,
    /// Google's: the connected accounts Google wants the owner to sign in
    /// with again, when there are any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_sign_in: Option<Vec<String>>,
    /// Messages' and Contacts', with `--check-access`: whether Plow Latch
    /// has Full Disk Access (`granted`, `not_granted` or `unknown`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<&'static str>,
}

#[derive(Serialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Done,
    /// Running `args` does it.
    Needed,
    /// Something else comes first: an earlier step, or what `reason` says.
    Blocked,
}

pub async fn status(state: &StateDir, check_access: bool) -> Result<Status, Failure> {
    use crate::plugins::{Connector, Setting};
    use StepState::{Blocked, Done, Needed};
    let local = crate::status::local(state)?;
    let access = if check_access {
        full_disk_access(state).await
    } else {
        None
    };
    let saved = crate::plugins::saved(state)?;
    let serving = local.serve.ready;
    // Whether Google is connected in Plow, asked only for the owner's login.
    let owner = local.peer.as_ref().is_some_and(|me| me.kind == "device");
    let (plow, sign_in_again) = if owner {
        match crate::plow::Plow::from_state(state) {
            Ok(plow) => {
                let connectors = plow.connectors().await;
                let connected = connectors.as_ref().is_ok_and(|c| c.gmail.connected);
                let again = if connected {
                    sign_in_again(&plow).await
                } else {
                    None
                };
                (Some(connectors), again)
            }
            Err(failure) => (Some(Err(failure)), None),
        }
    } else {
        (None, None)
    };
    let mut steps = vec![match &local.peer {
        Some(me) => Step::new("login", Done, format!("Logged in as {}.", me.name), None),
        None => Step::new(
            "login",
            Needed,
            "This machine is not logged in to Plow.".into(),
            Some(vec!["login", "--events"]),
        ),
    }];
    for &connector in Connector::ALL {
        let name = connector.name();
        let what = format!("The {} connector", title(name));
        let enabled = saved.get(&connector) == Some(&Setting::Enabled);
        let (state, reason, args) = match (&local.peer, saved.get(&connector)) {
            (None, _) => (Blocked, format!("{what} needs a login first."), None),
            (Some(me), _) if me.kind != "device" => (
                Blocked,
                format!("{what} is turned on by the owner's own login, not an agent's."),
                None,
            ),
            (Some(_), Some(Setting::Enabled)) if serving => (Done, format!("{what} is on."), None),
            (Some(_), Some(Setting::Enabled)) => (
                Done,
                format!("{what} is on; agents can use it once latch is running."),
                None,
            ),
            (Some(_), Some(Setting::NeedsSetup { reason })) => (
                Blocked,
                format!("{what} needs setup: {reason}"),
                Some(vec!["plugins", "enable", name]),
            ),
            (Some(_), None) => (
                Needed,
                format!("{what} is off."),
                Some(vec!["plugins", "enable", name]),
            ),
        };
        let mut step = Step {
            enabled: Some(enabled),
            ready: Some(enabled && serving),
            ..Step::new(name, state, reason, args)
        };
        if connector == Connector::Google
            && let Some(plow) = &plow
        {
            google_in_plow(&mut step, plow, sign_in_again.as_deref());
        }
        if connector != Connector::Google {
            step.access = access;
        }
        steps.push(step);
    }
    steps.push(match (&local.peer, &local.serve) {
        (None, _) => Step::new("host", Blocked, "latch needs a login first.".into(), None),
        (_, serve) if serve.ready => Step::new("host", Done, "latch is running.".into(), None),
        (
            _,
            crate::status::Serve {
                blocked: Some(problem),
                ..
            },
        ) => Step::new("host", Blocked, problem.message.clone(), None),
        (_, serve) if serve.running => {
            Step::new("host", Blocked, "latch is starting.".into(), None)
        }
        _ => Step::new(
            "host",
            Needed,
            "latch is not running.".into(),
            Some(start_args(state)),
        ),
    });
    Ok(Status { steps })
}

/// Whether Plow Latch has Full Disk Access, as the host would see it: one
/// check, on the Messages folder, for Messages and Contacts alike. Nothing
/// is turned on or saved, and nothing is written in the state folder.
#[cfg(target_os = "macos")]
async fn full_disk_access(state: &StateDir) -> Option<&'static str> {
    use crate::messages::access::{self, Status};
    let dir = state.path.canonicalize().unwrap_or(state.path.clone());
    Some(
        match access::check_as_app(&dir, crate::plugins::Connector::Messages)
            .await
            .status
        {
            Status::Granted => "granted",
            Status::NotGranted => "not_granted",
            Status::Unavailable => "unknown",
        },
    )
}

/// No Full Disk Access to check off a Mac, and no step that needs it.
#[cfg(not(target_os = "macos"))]
async fn full_disk_access(_state: &StateDir) -> Option<&'static str> {
    None
}

/// The Google step for the owner, by what Plow says: none connected
/// (connect one first), connected (then the connector), or Plow could not
/// say, which is never taken for either.
/// The connected Google accounts Google wants the owner to sign in with
/// again. Only getting their credentials tells, so this does, within
/// `SIGN_IN_CHECK_WITHIN`; none when Plow could not say in time.
async fn sign_in_again(plow: &crate::plow::Plow) -> Option<Vec<String>> {
    match tokio::time::timeout(SIGN_IN_CHECK_WITHIN, plow.google_tokens()).await {
        Ok(Ok(tokens)) => Some(tokens.needs_sign_in()),
        Ok(Err(failure)) if failure.code == "google_needs_sign_in" => Some(
            failure.details["accounts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|account| account.as_str().map(String::from))
                .collect(),
        ),
        _ => None,
    }
}

/// How long `setup --status` waits to learn whether Google wants a sign-in
/// again: it stays quick, and says nothing of it past this.
const SIGN_IN_CHECK_WITHIN: std::time::Duration = std::time::Duration::from_secs(5);

fn google_in_plow(
    step: &mut Step,
    plow: &Result<crate::plow::Connectors, Failure>,
    sign_in_again: Option<&[String]>,
) {
    use StepState::{Blocked, Needed};
    let connectors = match plow {
        Ok(connectors) => connectors,
        Err(failure) => {
            step.state = Blocked;
            step.reason = format!(
                "Plow could not say whether Google is connected: {}",
                failure.message
            );
            step.args = None;
            return;
        }
    };
    let accounts: Vec<String> = connectors
        .gmail
        .accounts
        .iter()
        .map(|account| account.account.clone())
        .collect();
    if !connectors.gmail.connected {
        let page = connectors.connections_page();
        step.state = Needed;
        step.reason = format!(
            "Google is not connected in Plow: `latch plugins enable google` signs you in, \
             or connect it at {page} first."
        );
        // Signs in and waits, reporting as JSON lines; asking mints nothing.
        step.args = Some(vec!["plugins", "enable", "google", "--events"]);
        step.connect_url = Some(page);
    } else {
        // The connector's own state stands; Plow adds who is connected, and
        // a setup that failed is worth trying again now.
        let mut reason = step.reason.chars();
        let first = reason.next().map(|c| c.to_lowercase().collect::<String>());
        step.reason = format!(
            "Google is connected in Plow ({}); {}{}",
            accounts.join(", "),
            first.unwrap_or_default(),
            reason.as_str()
        );
        if step.state == Blocked {
            step.state = Needed;
        }
        if let Some(again) = sign_in_again.filter(|again| !again.is_empty()) {
            // None of them works: signing in again is the step.
            if accounts.iter().all(|account| again.contains(account)) {
                step.state = Needed;
                step.reason = format!(
                    "Google needs you to sign in again with {}: `latch plugins enable google` \
                     signs you in.",
                    again.join(", ")
                );
                step.args = Some(vec!["plugins", "enable", "google", "--events"]);
            }
            step.needs_sign_in = Some(again.to_vec());
        }
    }
    step.accounts = Some(accounts);
}

impl Step {
    fn new(
        step: &'static str,
        state: StepState,
        reason: String,
        args: Option<Vec<&'static str>>,
    ) -> Step {
        Step {
            step,
            state,
            reason,
            args,
            enabled: None,
            ready: None,
            accounts: None,
            connect_url: None,
            needs_sign_in: None,
            access: None,
        }
    }
}

/// Opens the Full Disk Access pane and shows the app to grant it to, and
/// says what to do there.
#[cfg(target_os = "macos")]
fn show_grant(error: &Value) {
    let grant = error["grant"].as_str().unwrap_or("Plow Latch.app");
    let settings = error["settings"]
        .as_str()
        .unwrap_or(crate::messages::access::SETTINGS);
    say(&format!(
        "Turn on Full Disk Access for {grant}: drag it from the Finder window into the list in \
         System Settings, then switch it on. latch checks again every few seconds; type s and \
         Enter to skip."
    ));
    let _ = Command::new("open").arg(settings).status();
    let _ = Command::new("open").arg("-R").arg(grant).status();
}

fn title(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Lines typed at the terminal, read by one thread for the whole setup, so a
/// wait that also listens for `s` never swallows the next answer.
struct Input(Receiver<String>);

impl Input {
    fn start() -> Input {
        let (lines, input) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if lines.send(line.trim().to_lowercase()).is_err() {
                    break;
                }
            }
        });
        Input(input)
    }

    /// Asks `question`; an empty answer (or a closed stdin) is `default`.
    fn yes(&self, question: &str, default: bool) -> bool {
        eprint!("{question}");
        match self.0.recv().unwrap_or_default().as_str() {
            "" => default,
            answer => answer.starts_with('y'),
        }
    }

    /// Waits `wait`, or less if `s` is typed: then true.
    #[cfg(target_os = "macos")]
    fn skip_within(&self, wait: Duration) -> bool {
        let until = Instant::now() + wait;
        loop {
            match self
                .0
                .recv_timeout(until.saturating_duration_since(Instant::now()))
            {
                Ok(line) if line == "s" => return true,
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => return false,
                // No one types any more: just wait.
                Err(RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(until.saturating_duration_since(Instant::now()));
                    return false;
                }
            }
        }
    }
}

fn say(text: &str) {
    eprintln!("{text}");
}

fn step(what: &str, command: &str) {
    eprintln!("→ {what}: {command}");
}

fn done(text: &str) {
    eprintln!("✓ {text}");
}

/// A step's command failed: its own message, and how to try it again.
fn failed(json: &Value, command: &str) {
    let message = json["error"]["message"].as_str().unwrap_or("it failed");
    eprintln!("✗ {message}\n  Try again later: {command}");
}
