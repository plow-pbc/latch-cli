//! `latch setup`: one pass, at a terminal, through what a new machine
//! needs: a login, Google, on macOS Messages and Contacts, and a running
//! host. It is thin: every step runs an existing latch command (and says
//! which), so a person can do any step by hand later. Each step first looks
//! whether it is done already and then skips it, so setup can be run again
//! at any time. Skipping or failing a step never stops the next one.

use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::state::StateDir;
use crate::term;

/// How often, and how long, the Full Disk Access step asks the host again.
const RECHECK_EVERY: Duration = Duration::from_secs(2);
const RECHECK_FOR: Duration = Duration::from_secs(180);

pub fn run(state_dir: Option<PathBuf>) -> ExitCode {
    if !term::interactive() {
        eprintln!("latch setup asks questions: run it at a terminal");
        return ExitCode::from(2);
    }
    let mut setup = Setup {
        latch: latch_program(),
        state_dir,
        input: Input::start(),
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
    input: Input,
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

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.latch);
        if let Some(dir) = &self.state_dir {
            command.arg("--state-dir").arg(dir);
        }
        command.args(args);
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
        let status = self.command(&["login"]).status();
        if status.is_ok_and(|status| status.success()) {
            return true;
        }
        say(
            "Not logged in, so the other steps wait: run `latch setup` again once `latch login` works.",
        );
        false
    }

    /// Whether this machine's host answers, starting it with `serve -d` if
    /// not. Steps that change a connector need it.
    fn host(&mut self) -> bool {
        if self.local()["serve"]["ready"] == true {
            return true;
        }
        step("Start latch", "latch serve -d");
        let started = self.latch(&["serve", "-d"]);
        if !started.ok {
            failed(&started.json, "latch serve -d");
        }
        started.ok
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
        if !self.input.yes("Connect Google? [Y/n] ", true) {
            say("Skipped; later: `latch plugins enable google`.");
            return;
        }
        if !self.host() {
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

    /// Step 3: Messages and Contacts: Full Disk Access for the app the host runs
    /// in, checked by the host itself again and again while the owner turns
    /// it on, so neither Enter nor a restart is needed.
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
            .input
            .yes("Enable Messages and Contacts on this Mac? [y/N] ", false)
        {
            say("Skipped; later: `latch plugins enable messages` (and contacts).");
            return;
        }
        if !self.host() {
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
                if !shown {
                    let details = &enabled.json["error"]["details"];
                    if details["access"] != "not_granted" {
                        failed(&enabled.json, &command);
                        return false;
                    }
                    shown = true;
                    show_grant(details);
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
            if self.input.skip_within(RECHECK_EVERY) {
                say("Skipped; finish later with `latch plugins enable messages` (and contacts).");
                return;
            }
        }
    }

    /// Step 4: a running host.
    fn start(&mut self) {
        if self.local()["serve"]["ready"] == true {
            done("latch is running.");
            return;
        }
        if !self.input.yes("Start latch now? [Y/n] ", true) {
            say("Not started; later: `latch serve -d`.");
            return;
        }
        if self.host() {
            done("latch is running.");
        }
    }
}

/// Opens the Full Disk Access pane and shows the app to grant it to, and
/// says what to do there.
#[cfg(target_os = "macos")]
fn show_grant(details: &Value) {
    let grant = details["grant"].as_str().unwrap_or("Plow Latch.app");
    let settings = details["settings"]
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

#[cfg(target_os = "macos")]
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
