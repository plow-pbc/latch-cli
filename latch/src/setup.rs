//! `latch setup`: one pass, at a terminal, through what a new machine
//! needs: a login, Google, on macOS Messages and Contacts, and a running
//! host. None of the steps before the last needs a host running; the last
//! offers to start one. It is thin: every step runs an existing latch command (and says
//! which), so a person can do any step by hand later. Each step first looks
//! whether it is done already and then skips it, so setup can be run again
//! at any time. Skipping or failing a step never stops the next one.

use std::fs::File;
use std::io::{BufRead, Read, Write};
use std::os::fd::FromRawFd;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
#[cfg(target_os = "macos")]
use std::sync::mpsc::RecvTimeoutError;
use std::sync::mpsc::{self, Receiver};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use crate::failure::Failure;
use crate::state::StateDir;
use crate::term;

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
    if !term::interactive() {
        let failure = setup.no_terminal();
        eprintln!("{}", failure.to_json());
        return failure.exit.into();
    }
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

    /// Without a terminal nobody answers setup's questions: it names, as
    /// `next`, the first of its steps not done yet.
    fn no_terminal(&self) -> Failure {
        let local = self.local();
        let off = |name: &str| self.plugin_state(name).as_deref() != Some("enabled");
        let next = if local["peer"].is_null() {
            "latch login --events"
        } else if off("google") {
            "latch plugins enable google"
        } else if cfg!(target_os = "macos") && off("messages") {
            "latch plugins enable messages"
        } else if cfg!(target_os = "macos") && off("contacts") {
            "latch plugins enable contacts"
        } else if local["serve"]["ready"] != true {
            "latch serve -d"
        } else {
            "latch status"
        };
        let message = "latch setup asks questions, so it needs a terminal".to_string();
        let mut failure = Failure::usage("no_terminal", message);
        failure.next = Some(next.into());
        failure
    }

    /// Step 1: a login, by text at this terminal.
    fn log_in(&mut self) -> bool {
        if let Some(name) = self.local()["peer"]["name"].as_str() {
            done(&format!("Logged in as {name}."));
            return true;
        }
        step("Log in", "latch login");
        match self.login() {
            Ok(answer) => {
                if let Some(name) = answer["peer"]["name"].as_str() {
                    done(&format!("Logged in as {name}."));
                }
                return true;
            }
            Err(error) => {
                let message = error["error"]["message"].as_str().unwrap_or("it failed");
                eprintln!("✗ {message}");
            }
        }
        say(
            "Not logged in, so the other steps wait: run `latch setup` again once `latch login` works.",
        );
        false
    }

    /// `latch login`, at this terminal: its questions, and a login by
    /// text's screen, are the person's. Its stderr is a terminal of setup's
    /// own, passed on here as it comes, all but its error line, which setup
    /// reads and says in its own words. Its JSON (a login that asked no one)
    /// is read too, never shown.
    fn login(&self) -> Result<Value, Value> {
        let unread = |why: String| serde_json::json!({"error": {"message": why}});
        let (main, side) = pty().map_err(|err| unread(format!("no terminal for login: {err}")))?;
        let mut command = self.command(&["login"]);
        command
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(side));
        let child = command.spawn();
        // Every copy of the terminal's other side closes with login, so
        // passing it on ends when login does.
        drop(command);
        let child = child.map_err(|err| unread(err.to_string()))?;
        let passing = std::thread::spawn(move || pass_on(main));
        let output = child
            .wait_with_output()
            .map_err(|err| unread(err.to_string()))?;
        let error = passing.join().unwrap_or(Value::Null);
        if output.status.success() {
            return Ok(serde_json::from_slice(&output.stdout).unwrap_or(Value::Null));
        }
        Err(error)
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
        let enabled = self.latch(&["plugins", "enable", "google"]);
        if enabled.ok {
            done("Google is connected.");
        } else {
            failed(&enabled.json, "latch plugins enable google");
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

/// `latch setup --status`: each step of setup, read from the state folder
/// alone, so it changes nothing and needs no host, terminal or network.
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

pub fn status(state: &StateDir) -> Result<Status, Failure> {
    use crate::plugins::{Connector, Setting};
    use StepState::{Blocked, Done, Needed};
    let local = crate::status::local(state)?;
    let saved = crate::plugins::saved(state)?;
    let serving = local.serve.ready;
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
        steps.push(Step {
            enabled: Some(enabled),
            ready: Some(enabled && serving),
            ..Step::new(name, state, reason, args)
        });
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

/// A new terminal (a pty) the size of this one: its two sides.
fn pty() -> std::io::Result<(File, File)> {
    // SAFETY: an all-zero winsize is a valid value of the C struct.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: TIOCGWINSZ only writes this terminal's size into `size`; on
    // failure it stays zero, which a terminal takes as unknown.
    unsafe { libc::ioctl(2, libc::TIOCGWINSZ, &mut size) };
    let (mut main, mut side) = (0, 0);
    let none = std::ptr::null_mut();
    // SAFETY: openpty writes two new descriptors, which are ours from here.
    if unsafe { libc::openpty(&mut main, &mut side, none, std::ptr::null_mut(), &mut size) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both are open descriptors no one else owns.
    Ok(unsafe { (File::from_raw_fd(main), File::from_raw_fd(side)) })
}

/// Passes what login shows on its terminal on to this one as it comes,
/// prompts with no line end included, all but its one JSON error line,
/// which it returns.
fn pass_on(mut from: File) -> Value {
    let mut out = std::io::stderr();
    let mut error = Value::Null;
    // A line that begins with `{`, held until its end: the error line, or
    // else shown after all.
    let mut held: Option<Vec<u8>> = None;
    let mut line_start = true;
    let mut chunk = [0u8; 4096];
    // Ends with an error (EIO) once login, the terminal's last user, exits.
    while let Ok(read) = from.read(&mut chunk) {
        if read == 0 {
            break;
        }
        let mut shown = Vec::new();
        for &byte in &chunk[..read] {
            if let Some(line) = &mut held {
                line.push(byte);
                if byte == b'\n' {
                    let line = held.take().expect("held");
                    match serde_json::from_slice::<Value>(&line) {
                        Ok(value) if value.get("error").is_some() => error = value,
                        _ => shown.extend(line),
                    }
                    line_start = true;
                }
                continue;
            }
            if line_start && byte == b'{' {
                held = Some(vec![byte]);
                continue;
            }
            shown.push(byte);
            line_start = byte == b'\n';
        }
        let _ = out.write_all(&shown).and_then(|()| out.flush());
    }
    if let Some(line) = held {
        let _ = out.write_all(&line);
    }
    error
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
