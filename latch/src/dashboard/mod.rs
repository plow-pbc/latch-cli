//! The host's live view: what callers asked for and what happened, as a
//! list of tasks whose steps unfold, with every detail one key away. All of
//! it is data the host already has; nothing here changes what it does.
//!
//! The host keeps the board (`Dashboard`) and sends it over its local
//! socket to whoever watches: an attached `latch serve` or `latch
//! dashboard`, which draw it on their terminal (`show`).
//!
//! A task is what a caller did: one call (an exec, a file, a search), or a
//! browser session with its calls in order, a plan's steps under its call.
//! Calls are grouped only by what links them for real (a browser
//! session), never by when they came.

mod describe;
mod show;
mod view;

use std::collections::VecDeque;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::watch;

pub use describe::{Finished, begun, finished, refused, session};
pub use show::{Ending, show};

/// How many tasks the board keeps, and how many calls in each: older ones
/// are dropped, and the board says so.
const KEEP_TASKS: usize = 200;
const KEEP_CALLS: usize = 50;

/// What the screen shows, as the host sends it to its viewers.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Board {
    pub host: String,
    /// When the host began serving.
    pub since: Option<Timestamp>,
    /// What a starting or stopping host is doing, until it serves.
    pub step: Option<String>,
    /// Whether the last registration with Plow (the directory) worked.
    pub directory: Option<bool>,
    /// Whether the host's local socket is listening.
    pub socket: Option<bool>,
    /// Each connector this build has, and whether its owner enabled it.
    pub connectors: Vec<(String, String)>,
    /// Pending browser handoffs, each a line of its own.
    pub handoffs: Vec<String>,
    /// Newest first.
    pub tasks: VecDeque<Task>,
    /// Older tasks were dropped.
    pub dropped: bool,
    /// When the host last changed the board.
    pub updated: Option<Timestamp>,
    /// The last id given to a task or call.
    #[serde(skip)]
    next: u64,
}

/// What one caller did: one call, or a browser session's calls.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Task {
    pub id: u64,
    /// The browser session (its id's hash) whose calls this is.
    pub session: Option<String>,
    /// The session's task label or site; for a lone call, its own title.
    pub title: String,
    pub caller: Caller,
    /// In the order they came.
    pub calls: Vec<Call>,
    /// Earlier calls were dropped.
    pub dropped: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Caller {
    /// `the owner (local)`, `the owner (device)` or the agent's name.
    pub label: String,
    /// The device's name and key, or the agent's key.
    pub detail: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Call {
    pub id: u64,
    pub at: Timestamp,
    pub title: String,
    pub outcome: Outcome,
    /// A browser plan's steps, in order.
    pub steps: Vec<Step>,
    pub detail: Detail,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum State {
    Running,
    Done,
    /// The gatekeeper, or the host's owner, did not let it run.
    Refused,
    /// A browser safety check stopped it.
    Stopped,
    /// It ran and failed, or the service did.
    Failed,
}

/// How a call or step ended: its state, and what to say after it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Outcome {
    pub state: State,
    pub text: Option<String>,
}

impl Outcome {
    pub fn new(state: State, text: Option<String>) -> Outcome {
        Outcome { state, text }
    }

    /// `Done`, `Done - exit 0`, `Refused - owner said no`.
    pub fn line(&self) -> String {
        let state = match self.state {
            State::Running => "Running",
            State::Done => "Done",
            State::Refused => "Refused",
            State::Stopped => "Stopped",
            State::Failed => "Failed",
        };
        match &self.text {
            Some(text) => format!("{state} - {text}"),
            None => state.to_string(),
        }
    }

    pub fn problem(&self) -> bool {
        matches!(self.state, State::Refused | State::Stopped | State::Failed)
    }
}

/// One step of a plan: what was asked, and how it went (none: not reached).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Step {
    pub title: String,
    pub outcome: Option<Outcome>,
}

/// What the inspector shows of a call.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Detail {
    /// The complete action, as the host recorded it.
    pub action: String,
    /// The page's origin and path.
    pub page: Option<String>,
    pub took_ms: Option<u64>,
    pub code: Option<String>,
    /// The full failure message.
    pub message: Option<String>,
    pub audit_id: Option<String>,
    /// The gatekeeper's record of its judgment: probabilities, model,
    /// threshold, rules revisions, chat window. No chat or rules text.
    pub gate: Option<Value>,
    /// The audit's details of the request, credential-looking values hidden.
    pub request: Value,
}

/// The host's side: the board, and whoever watches it changing.
#[derive(Clone)]
pub struct Dashboard {
    board: watch::Sender<Board>,
}

/// A call `begin` put on the board, for `end` to finish.
#[derive(Clone, Copy, Debug)]
pub struct CallId(u64);

impl Dashboard {
    pub fn new(host: &str) -> Dashboard {
        let board = Board {
            host: host.to_string(),
            ..Board::default()
        };
        Dashboard {
            board: watch::Sender::new(board),
        }
    }

    /// The board now, and each change after.
    pub fn watch(&self) -> watch::Receiver<Board> {
        self.board.subscribe()
    }

    fn change(&self, change: impl FnOnce(&mut Board)) {
        self.board.send_modify(|board| {
            change(board);
            board.updated = Some(Timestamp::now());
        });
    }

    /// The host serves from now on.
    pub fn serving(&self) {
        self.change(|board| {
            board.since = Some(Timestamp::now());
            board.step = None;
        });
    }

    /// Whether the host's last registration with Plow worked.
    pub fn directory(&self, connected: bool) {
        self.change(|board| board.directory = Some(connected));
    }

    pub fn socket(&self, listening: bool) {
        self.change(|board| board.socket = Some(listening));
    }

    pub fn connectors(&self, connectors: Vec<(String, String)>) {
        self.change(|board| board.connectors = connectors);
    }

    /// Shows what the host is doing now, while it starts or stops.
    pub fn step(&self, step: &str) {
        self.change(|board| board.step = Some(step.to_string()));
    }

    /// Replaces the pending handoffs.
    pub fn notices(&self, lines: Vec<String>) {
        self.change(|board| board.handoffs = lines);
    }

    /// A call starting, by `caller`: shown running, in its session's task
    /// if it names one, or as a task of its own.
    pub fn begin(
        &self,
        caller: Caller,
        session: Option<String>,
        (title, steps): (String, Vec<Step>),
    ) -> CallId {
        let mut id = 0;
        self.change(|board| {
            board.next += 1;
            id = board.next;
            let outcome = Outcome::running(&steps);
            board.add(
                caller,
                session,
                Call {
                    id,
                    at: Timestamp::now(),
                    title,
                    outcome,
                    steps,
                    detail: Detail::default(),
                },
            );
        });
        CallId(id)
    }

    /// One step of the plan running in browser session `session` finished,
    /// as its answer shows it (`step`, `outcome`, `code`): at most once per
    /// step, so a plan's updates are as many as its steps.
    pub fn progress(&self, session: &str, shown: &Value) {
        let Some(n) = shown["step"]
            .as_u64()
            .and_then(|n| (n as usize).checked_sub(1))
        else {
            return;
        };
        self.change(|board| {
            let running = board
                .tasks
                .iter_mut()
                .filter(|task| task.session.as_deref() == Some(session))
                .flat_map(|task| task.calls.iter_mut())
                .find(|call| call.outcome.state == State::Running && n < call.steps.len());
            if let Some(call) = running {
                call.steps[n].outcome = describe::step_outcome(shown);
                call.outcome = Outcome::running(&call.steps);
            }
        });
    }

    /// The call `id` ended as `finished`, by `caller` as now known. An
    /// `open` names its session only now: its task becomes that session's.
    pub fn end(&self, id: CallId, caller: Caller, finished: Finished) {
        self.change(|board| board.end(id.0, caller, finished));
    }

    /// A call that ended before it could run: a caller refused at the
    /// door, a phone link's.
    pub fn once(&self, caller: Caller, finished: Finished) {
        let id = self.begin(caller.clone(), None, (finished.title.clone(), Vec::new()));
        self.end(id, caller, finished);
    }
}

impl Board {
    fn add(&mut self, caller: Caller, session: Option<String>, call: Call) {
        let existing = session.as_ref().and_then(|session| {
            self.tasks
                .iter()
                .position(|task| task.session.as_ref() == Some(session))
        });
        let Some(at) = existing else {
            self.next += 1;
            self.tasks.push_front(Task {
                id: self.next,
                title: match &session {
                    Some(session) => format!("Browser session {session}"),
                    None => call.title.clone(),
                },
                session,
                caller,
                calls: vec![call],
                dropped: 0,
            });
            if self.tasks.len() > KEEP_TASKS {
                self.tasks.pop_back();
                self.dropped = true;
            }
            return;
        };
        // A task with new work moves to the front.
        let mut task = self.tasks.remove(at).expect("the task is there");
        task.calls.push(call);
        if task.calls.len() > KEEP_CALLS {
            task.calls.remove(0);
            task.dropped += 1;
        }
        self.tasks.push_front(task);
    }

    fn end(&mut self, id: u64, caller: Caller, finished: Finished) {
        let Some(task) = self
            .tasks
            .iter_mut()
            .find(|task| task.calls.iter().any(|call| call.id == id))
        else {
            return;
        };
        task.caller = caller;
        if task.session.is_none() {
            task.session = finished.session.clone();
        }
        if task.session.is_none() {
            task.title = finished.title.clone();
        } else if let Some(label) = &finished.label {
            // A session's task is named by what the caller labelled it,
            // or the site it is on.
            task.title = label.clone();
        } else if task.title.starts_with("Browser session ")
            && let Some(site) = &finished.site
        {
            task.title = format!("Browser session: {site}");
        }
        let call = task
            .calls
            .iter_mut()
            .find(|call| call.id == id)
            .expect("the call is in its task");
        call.title = finished.title;
        call.outcome = finished.outcome;
        call.steps = finished.steps;
        call.detail = finished.detail;
    }
}

impl Outcome {
    /// A call that runs: with steps, which one it is on.
    fn running(steps: &[Step]) -> Outcome {
        let on = steps.iter().position(|step| step.outcome.is_none());
        Outcome::new(State::Running, on.map(|n| format!("step {}", n + 1)))
    }
}

impl Task {
    /// A task's result. A session's is what matters in it: a call still
    /// running, else its latest plan or problem, so a later observation
    /// that went fine never makes a stopped plan look done.
    pub fn outcome(&self) -> Outcome {
        self.key_call()
            .map_or(Outcome::new(State::Done, None), |call| call.outcome.clone())
    }

    /// The call that says how the task is going (see `outcome`).
    pub fn key_call(&self) -> Option<&Call> {
        let latest = |pick: &dyn Fn(&Call) -> bool| self.calls.iter().rev().find(|call| pick(call));
        latest(&|call| call.outcome.state == State::Running)
            .or_else(|| latest(&|call| !call.steps.is_empty() || call.outcome.problem()))
            .or(self.calls.last())
    }

    pub fn at(&self) -> Timestamp {
        self.calls
            .last()
            .map_or(Timestamp::UNIX_EPOCH, |call| call.at)
    }

    /// A task of one call and no session: the call is the task.
    pub fn lone(&self) -> Option<&Call> {
        match (&self.session, self.calls.as_slice()) {
            (None, [call]) => Some(call),
            _ => None,
        }
    }

    pub fn problem(&self) -> bool {
        self.calls.iter().any(|call| call.outcome.problem())
    }
}

#[cfg(test)]
mod tests;
