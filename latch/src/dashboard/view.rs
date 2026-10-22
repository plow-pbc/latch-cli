//! What a viewer of the board sees, and what its keys do: the task list
//! with steps that fold open, and a full-screen inspector for one call.
//! The view holds only where the person is (selection, what is open, a
//! filter, a pause); the board is the host's.
//!
//! Text is laid out in fixed columns and wrapped onto continuation lines,
//! never cut where it matters (a refusal's reason is always whole). Every
//! state is in words, so nothing depends on color.

use std::collections::HashSet;

use jiff::{Timestamp, tz::TimeZone};
use serde_json::Value;

use super::{Board, Call, Caller, Outcome, State, Task};
use crate::paint::{self, DIM, FAINT, RED, VOLT};
use crate::term;

/// A key a viewer pressed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Tab,
    Esc,
    End,
    Backspace,
    Char(char),
}

/// What a key asks of the viewer itself.
#[derive(Debug, PartialEq)]
pub enum Asked {
    Nothing,
    /// `q`: close the view, leaving the host running.
    Detach,
}

/// One thing on the list: a task, a call in a session, or a plan's step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sel {
    Task(u64),
    Call(u64, u64),
    Step(u64, u64, usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tab {
    Steps,
    Decision,
    Request,
}

struct Inspect {
    sel: Sel,
    tab: Tab,
    scroll: usize,
}

pub struct View {
    /// The host's board as last sent.
    pub board: Board,
    /// A board kept still while the viewer reads (Space; End resumes).
    frozen: Option<Board>,
    /// When the host went away, if it did.
    pub gone: Option<Timestamp>,
    /// An attached `latch serve`, whose Ctrl-C stops the host.
    pub attached: bool,
    /// The viewer's own line under the header (a version mismatch).
    notice: Option<String>,
    selected: Option<Sel>,
    open: HashSet<Sel>,
    filter: String,
    /// The filter being typed after `/`.
    typing: Option<String>,
    /// Only what was refused, stopped or failed.
    problems: bool,
    inspect: Option<Inspect>,
}

/// One line of the list, before layout.
struct Item {
    sel: Option<Sel>,
    depth: usize,
    at: Option<Timestamp>,
    caller: Option<Caller>,
    title: String,
    outcome: Option<Outcome>,
    /// Has children: open (true) or folded (false).
    fold: Option<bool>,
}

impl View {
    pub fn new(attached: bool, notice: Option<String>) -> View {
        View {
            board: Board::default(),
            frozen: None,
            gone: None,
            attached,
            notice,
            selected: None,
            open: HashSet::new(),
            filter: String::new(),
            typing: None,
            problems: false,
            inspect: None,
        }
    }

    /// The board on screen: the frozen one while paused.
    fn shown(&self) -> &Board {
        self.frozen.as_ref().unwrap_or(&self.board)
    }

    pub fn key(&mut self, key: Key) -> Asked {
        if let Some(typed) = &mut self.typing {
            match key {
                Key::Enter => {
                    self.filter = self.typing.take().unwrap_or_default();
                }
                Key::Esc => self.typing = None,
                Key::Backspace => {
                    typed.pop();
                }
                Key::Char(ch) => typed.push(ch),
                _ => {}
            }
            return Asked::Nothing;
        }
        if let Some(inspect) = &mut self.inspect {
            match key {
                Key::Tab => {
                    inspect.tab = match inspect.tab {
                        Tab::Steps => Tab::Decision,
                        Tab::Decision => Tab::Request,
                        Tab::Request => Tab::Steps,
                    };
                    inspect.scroll = 0;
                }
                Key::Up => inspect.scroll = inspect.scroll.saturating_sub(1),
                Key::Down => inspect.scroll += 1,
                Key::Esc | Key::Left => self.inspect = None,
                Key::Char('q') => return Asked::Detach,
                _ => {}
            }
            return Asked::Nothing;
        }
        let items = self.items();
        let selectable: Vec<Sel> = items.iter().filter_map(|item| item.sel).collect();
        let at = self
            .selected
            .and_then(|sel| selectable.iter().position(|s| *s == sel));
        match key {
            Key::Up => {
                self.selected = match at {
                    Some(at) => selectable.get(at.saturating_sub(1)).copied(),
                    None => selectable.first().copied(),
                }
            }
            Key::Down => {
                self.selected = match at {
                    Some(at) => selectable.get(at + 1).or(selectable.get(at)).copied(),
                    None => selectable.first().copied(),
                }
            }
            Key::Right => {
                if let Some(sel) = self.selected
                    && items
                        .iter()
                        .any(|item| item.sel == Some(sel) && item.fold.is_some())
                {
                    self.open.insert(sel);
                }
            }
            Key::Left => {
                if let Some(sel) = self.selected {
                    if self.open.remove(&sel) {
                    } else {
                        self.selected = parent(sel);
                    }
                }
            }
            Key::Enter => {
                if let Some(sel) = self.selected {
                    self.inspect = Some(Inspect {
                        sel,
                        tab: Tab::Steps,
                        scroll: 0,
                    });
                }
            }
            Key::Char(' ') => {
                if self.frozen.is_none() {
                    self.frozen = Some(self.board.clone());
                }
            }
            Key::End => self.frozen = None,
            Key::Char('/') => self.typing = Some(self.filter.clone()),
            Key::Char('r') => self.problems = !self.problems,
            Key::Char('q') => return Asked::Detach,
            _ => {}
        }
        Asked::Nothing
    }

    /// The list as it stands: tasks with work running first, then the
    /// latest; within each, what is open.
    fn items(&self) -> Vec<Item> {
        let board = self.shown();
        let filter = self.filter.to_lowercase();
        let mut tasks: Vec<&Task> = board
            .tasks
            .iter()
            .filter(|task| !self.problems || task.problem())
            .filter(|task| filter.is_empty() || matches(task, &filter))
            .collect();
        tasks.sort_by_key(|task| {
            (
                task.outcome().state != State::Running,
                std::cmp::Reverse(task.at()),
            )
        });
        let mut items = Vec::new();
        for task in tasks {
            let sel = Sel::Task(task.id);
            let open = self.open.contains(&sel);
            if let Some(call) = task.lone() {
                items.push(Item {
                    sel: Some(sel),
                    depth: 0,
                    at: Some(call.at),
                    caller: Some(task.caller.clone()),
                    title: call.title.clone(),
                    outcome: Some(call.outcome.clone()),
                    fold: (!call.steps.is_empty()).then_some(open),
                });
                if open {
                    self.steps(&mut items, task.id, call, 1);
                }
                continue;
            }
            items.push(Item {
                sel: Some(sel),
                depth: 0,
                at: Some(task.at()),
                caller: Some(task.caller.clone()),
                title: task.title.clone(),
                outcome: Some(task.outcome()),
                fold: Some(open),
            });
            if !open {
                continue;
            }
            if task.dropped > 0 {
                items.push(note(1, "Earlier steps unavailable"));
            }
            for call in &task.calls {
                let sel = Sel::Call(task.id, call.id);
                let open = self.open.contains(&sel);
                items.push(Item {
                    sel: Some(sel),
                    depth: 1,
                    at: Some(call.at),
                    caller: None,
                    title: call.title.clone(),
                    outcome: Some(call.outcome.clone()),
                    fold: (!call.steps.is_empty()).then_some(open),
                });
                if open {
                    self.steps(&mut items, task.id, call, 2);
                }
            }
        }
        if board.dropped && !board.tasks.is_empty() {
            items.push(note(0, "Earlier tasks unavailable"));
        }
        items
    }

    fn steps(&self, items: &mut Vec<Item>, task: u64, call: &Call, depth: usize) {
        for (n, step) in call.steps.iter().enumerate() {
            items.push(Item {
                sel: Some(Sel::Step(task, call.id, n)),
                depth,
                at: None,
                caller: None,
                title: format!("{} {}", n + 1, step.title),
                outcome: step.outcome.clone(),
                fold: None,
            });
        }
    }

    /// The whole screen, `width` columns by `height` rows.
    pub fn screen(&self, width: usize, height: usize) -> String {
        let width = width.max(40);
        let mut lines = match &self.inspect {
            Some(inspect) => self.inspector(inspect, width, height),
            None => self.list(width, height),
        };
        lines.truncate(height);
        lines.join("\n")
    }

    fn header(&self, width: usize) -> Vec<String> {
        let board = self.shown();
        let wide = width >= 100;
        let host = format!("HOST {}", term::one_line(&board.host));
        let serving = match (&board.step, board.since) {
            (Some(step), _) => step.clone(),
            (None, Some(since)) => format!(
                "Serving since {} ({})",
                clock_short(since),
                ago(since, Timestamp::now())
            ),
            (None, None) => "Starting".to_string(),
        };
        let directory = format!(
            "Directory: {}",
            match board.directory {
                Some(true) => "connected",
                Some(false) => "reconnecting",
                None => "unknown",
            }
        );
        let socket = format!(
            "Local socket: {}",
            match board.socket {
                Some(true) => "listening",
                Some(false) => "unavailable",
                None => "unknown",
            }
        );
        let connectors: Vec<String> = board
            .connectors
            .iter()
            .map(|(name, state)| format!("{}: {state}", capitalized(name)))
            .collect();
        let mut lines = Vec::new();
        if wide {
            lines.push(paint::bold(
                &format!("{host} | {serving} | {directory}"),
                VOLT,
            ));
            let mut second = connectors.clone();
            second.push(socket);
            lines.push(second.join(" | "));
        } else {
            lines.push(paint::bold(&format!("{host} | {serving}"), VOLT));
            lines.push(format!("{directory} | {socket}"));
            if !connectors.is_empty() {
                lines.push(connectors.join(" | "));
            }
        }
        let mut more = Vec::new();
        if let Some(gone) = self.gone {
            let last = board.updated.unwrap_or(gone);
            more.push(paint::bold(
                &format!("Host disconnected; last update {}", clock(last)),
                RED,
            ));
        }
        if let Some(notice) = &self.notice {
            more.push(paint::bold(notice, RED));
        }
        if let Some(note) = &board.note {
            more.push(paint::fg(note, DIM));
        }
        for handoff in &board.handoffs {
            more.push(paint::bold(handoff, RED));
        }
        if self.frozen.is_some() {
            more.push("Paused: new work is not shown; End resumes".to_string());
        }
        match (&self.typing, self.filter.is_empty(), self.problems) {
            (Some(typed), _, _) => {
                more.push(format!("Filter: {typed}_  (Enter applies, Esc cancels)"))
            }
            (None, false, problems) => more.push(format!(
                "Filter: {}{}",
                self.filter,
                if problems { " | problems only" } else { "" }
            )),
            (None, true, true) => more.push("Showing problems only (r shows all)".to_string()),
            _ => {}
        }
        for line in more {
            lines.extend(wrap_painted(&line, width));
        }
        lines
    }

    fn list(&self, width: usize, height: usize) -> Vec<String> {
        let mut lines = self.header(width);
        let layout = Layout::for_width(width);
        lines.push(paint::fg(&layout.heading(), DIM));
        let footer = self.footer(width);
        let room = height.saturating_sub(lines.len() + footer.len());
        let items = self.items();
        let mut rows: Vec<(bool, Vec<String>)> = Vec::new();
        for item in &items {
            let selected = item.sel.is_some() && item.sel == self.selected;
            rows.push((selected, layout.row(item, selected)));
        }
        if items.is_empty() {
            rows.push((false, vec![paint::fg(" waiting for calls…", DIM)]));
        }
        // Keep the selected row on screen: start far enough down for it.
        let selected_at = rows.iter().position(|(selected, _)| *selected);
        let mut start = 0;
        if let Some(at) = selected_at {
            let mut used: usize = rows[..=at].iter().map(|(_, row)| row.len()).sum();
            while used > room && start < at {
                used -= rows[start].1.len();
                start += 1;
            }
        }
        let mut body = Vec::new();
        for (_, row) in &rows[start..] {
            if body.len() + row.len() > room {
                break;
            }
            body.extend(row.iter().cloned());
        }
        lines.extend(body);
        while lines.len() + footer.len() < height {
            lines.push(String::new());
        }
        lines.extend(footer);
        lines
    }

    fn footer(&self, width: usize) -> Vec<String> {
        let live = if self.frozen.is_some() {
            "Paused; End resumes"
        } else {
            "Live; Space pauses"
        };
        let stop = if self.attached {
            "Ctrl-C stop host"
        } else {
            "Ctrl-C close"
        };
        let mut lines = vec![paint::fg(
            "Commands hide credential-looking values, best effort: some can slip through",
            FAINT,
        )];
        if width >= 100 {
            lines.push(paint::fg(
                &format!(
                    "Up/Down select  Left/Right fold  Enter details  / filter  r problems  Space pause  q detach  {stop}"
                ),
                DIM,
            ));
        } else {
            lines.push(paint::fg(
                "Enter details  Left/Right fold  / filter  r problems  q detach",
                DIM,
            ));
            lines.push(paint::fg(&format!("{stop} | {live}"), DIM));
        }
        lines
    }

    fn inspector(&self, inspect: &Inspect, width: usize, height: usize) -> Vec<String> {
        let board = self.shown();
        let footer = vec![paint::fg(
            "Tab switch  Up/Down scroll  Esc back  q detach",
            DIM,
        )];
        let Some((task, call, step)) = find(board, inspect.sel) else {
            let mut lines = vec!["This is no longer on the board.".to_string()];
            lines.extend(footer);
            return lines;
        };
        let mut lines = Vec::new();
        let title = match (call, step) {
            (_, Some(step)) => step.title.clone(),
            (Some(call), None) => call.title.clone(),
            (None, None) => task.title.clone(),
        };
        let at = call.map_or(task.at(), |call| call.at);
        lines.extend(wrap_painted(
            &paint::bold(
                &format!(
                    "{} | {} | {}",
                    term::one_line(&title),
                    task.caller.label,
                    clock(at)
                ),
                VOLT,
            ),
            width,
        ));
        let tab = |name: &str, this: Tab| {
            if inspect.tab == this {
                format!("[{name}]")
            } else {
                name.to_string()
            }
        };
        lines.push(format!(
            "{}   {}   {}",
            tab("Steps", Tab::Steps),
            tab("Decision", Tab::Decision),
            tab("Request", Tab::Request)
        ));
        let body = match (inspect.tab, call) {
            (Tab::Steps, Some(call)) => call_steps(task, call, step.map(|s| &s.outcome)),
            (Tab::Steps, None) => session_calls(task),
            (Tab::Decision, Some(call)) => decision(task, call),
            (Tab::Request, Some(call)) => request(call),
            (_, None) => vec!["Select one of the session's calls for its details.".to_string()],
        };
        let mut wrapped = Vec::new();
        for line in body {
            wrapped.extend(wrap(&line, width));
        }
        let room = height.saturating_sub(lines.len() + footer.len());
        let scroll = inspect.scroll.min(wrapped.len().saturating_sub(room));
        lines.extend(wrapped.into_iter().skip(scroll).take(room));
        while lines.len() + footer.len() < height {
            lines.push(String::new());
        }
        lines.extend(footer);
        lines
    }
}

/// A task, call and step by their selection.
fn find(board: &Board, sel: Sel) -> Option<(&Task, Option<&Call>, Option<&super::Step>)> {
    let (task_id, call_id, step) = match sel {
        Sel::Task(task) => (task, None, None),
        Sel::Call(task, call) => (task, Some(call), None),
        Sel::Step(task, call, step) => (task, Some(call), Some(step)),
    };
    let task = board.tasks.iter().find(|task| task.id == task_id)?;
    let call = match call_id {
        Some(id) => Some(task.calls.iter().find(|call| call.id == id)?),
        None => task.lone(),
    };
    let step = match (call, step) {
        (Some(call), Some(n)) => Some(call.steps.get(n)?),
        _ => None,
    };
    Some((task, call, step))
}

fn parent(sel: Sel) -> Option<Sel> {
    match sel {
        Sel::Task(_) => Some(sel),
        Sel::Call(task, _) => Some(Sel::Task(task)),
        Sel::Step(task, call, _) => Some(Sel::Call(task, call)),
    }
}

fn matches(task: &Task, filter: &str) -> bool {
    let has = |text: &str| text.to_lowercase().contains(filter);
    has(&task.title)
        || has(&task.caller.label)
        || task
            .calls
            .iter()
            .any(|call| has(&call.title) || call.steps.iter().any(|step| has(&step.title)))
}

fn note(depth: usize, text: &str) -> Item {
    Item {
        sel: None,
        depth,
        at: None,
        caller: None,
        title: text.to_string(),
        outcome: None,
        fold: None,
    }
}

/// The Steps tab of a call: how it ended and why, what it was, its steps.
fn call_steps(task: &Task, call: &Call, step: Option<&Option<Outcome>>) -> Vec<String> {
    let mut lines = Vec::new();
    let outcome = match step {
        Some(Some(outcome)) => outcome.clone(),
        Some(None) => Outcome::new(State::Done, Some("not reached".into())),
        None => call.outcome.clone(),
    };
    let ran = match outcome.state {
        State::Refused => " This action did not run.",
        State::Running => " It is still running.",
        _ => "",
    };
    lines.push(format!("{}.{ran}", outcome.line().to_uppercase_first()));
    let detail = &call.detail;
    if let Some(message) = &detail.message {
        lines.push(message.clone());
    }
    lines.push(String::new());
    lines.push(format!("Action: {}", term::one_line(&detail.action)));
    if let Some(page) = &detail.page {
        lines.push(format!("Page: {page}"));
    }
    let took = detail
        .took_ms
        .map(|ms| format!(" | took {}", human_ms(ms)))
        .unwrap_or_default();
    lines.push(format!("Started: {}{took}", full_time(call.at)));
    if let Some(code) = &detail.code {
        lines.push(format!("Code: {code}"));
    }
    lines.push(format!(
        "Caller: {} ({})",
        task.caller.label, task.caller.detail
    ));
    if !call.steps.is_empty() {
        lines.push(String::new());
        lines.push("Steps, in order:".to_string());
        for (n, step) in call.steps.iter().enumerate() {
            let how = step
                .outcome
                .as_ref()
                .map_or("not reached".to_string(), Outcome::line);
            lines.push(format!("  {} {} — {how}", n + 1, step.title));
        }
    }
    lines.push(String::new());
    lines.push(format!(
        "Audit: {}",
        detail.audit_id.as_deref().unwrap_or("not recorded")
    ));
    lines
}

/// A session's calls, in order.
fn session_calls(task: &Task) -> Vec<String> {
    let mut lines = vec![format!("{}: {}", task.title, task.outcome().line())];
    if let Some(session) = &task.session {
        lines.push(format!("Browser session {session}"));
    }
    lines.push(format!(
        "Caller: {} ({})",
        task.caller.label, task.caller.detail
    ));
    lines.push(String::new());
    if task.dropped > 0 {
        lines.push("Earlier steps unavailable".to_string());
    }
    for call in &task.calls {
        lines.push(format!(
            "{}  {} — {}",
            clock(call.at),
            term::one_line(&call.title),
            call.outcome.line()
        ));
    }
    lines
}

/// The Decision tab: the gatekeeper's record of its judgment, as recorded.
/// Scores are what the model gave, not reasons; no rule is quoted that the
/// judge did not cite.
fn decision(task: &Task, call: &Call) -> Vec<String> {
    let detail = &call.detail;
    let mut lines = Vec::new();
    let outcome = &call.outcome;
    let ran = if outcome.state == State::Refused {
        " This action did not run."
    } else {
        ""
    };
    lines.push(format!("{}.{ran}", outcome.line().to_uppercase_first()));
    if let Some(message) = &detail.message {
        lines.push(message.clone());
    }
    lines.push(String::new());
    let Some(gate) = &detail.gate else {
        lines.push("The gatekeeper did not judge this call.".to_string());
        lines.push(format!(
            "Audit: {}",
            detail.audit_id.as_deref().unwrap_or("not recorded")
        ));
        return lines;
    };
    let p = &gate["p"];
    if p.is_object() {
        lines.push(format!(
            "Judge P: within instructions {} | not covered {} | forbidden {}",
            score(&p["COVERED"]),
            score(&p["NOT_COVERED"]),
            score(&p["FORBIDDEN"])
        ));
    } else {
        lines.push("Judge P: no answer from the judge".to_string());
    }
    lines.push(match gate["threshold"].as_f64() {
        Some(threshold) => format!("Allow threshold: within instructions >= {threshold:.2}"),
        None => "Allow threshold: not recorded".to_string(),
    });
    if let Some(model) = gate["model"].as_str() {
        lines.push(format!("Model: {model}"));
    }
    if let Some(reason) = gate["model_failed"].as_str() {
        lines.push(format!("Judge unavailable: {reason}"));
    }
    if let Some(source) = gate["source"].as_str() {
        lines.push(format!("Judged on: {}", source.replace('_', " ")));
    }
    let rules = &gate["rules"];
    if rules.is_object() {
        let scope: Vec<&str> = [("host", "this host"), ("agent", "this agent")]
            .iter()
            .filter(|(key, _)| rules[*key].is_string())
            .map(|(_, name)| *name)
            .collect();
        let revisions: Vec<String> = ["host", "agent"]
            .iter()
            .filter_map(|key| rules[*key].as_str())
            .map(|hash| hash.chars().take(12).collect())
            .collect();
        lines.push(format!(
            "Rules used: {} | revisions: {}",
            scope.join(" + "),
            revisions.join(", ")
        ));
        lines.push("Matched rule: not provided by the judge".to_string());
        lines.push(
            "Rule pointer: `latch rules show` has the current rules; they may differ from these revisions"
                .to_string(),
        );
    } else {
        lines.push("Rules used: none".to_string());
    }
    if let Some(chat) = gate["chat"].as_str() {
        let window = &gate["window"];
        lines.push(format!(
            "Chat: {chat}; messages {} to {} ({}); message text not kept here",
            window["first"].as_str().unwrap_or("?"),
            window["last"].as_str().unwrap_or("?"),
            window["count"].as_u64().unwrap_or(0)
        ));
    }
    lines.push("Scores are the model's outputs, not explanations.".to_string());
    lines.push(String::new());
    lines.push(format!(
        "Caller: {} ({})",
        task.caller.label, task.caller.detail
    ));
    lines.push(format!(
        "Audit: {}",
        detail.audit_id.as_deref().unwrap_or("not recorded")
    ));
    lines
}

/// The Request tab: the action and the details the audit keeps of it.
fn request(call: &Call) -> Vec<String> {
    let mut lines = vec![format!("Action: {}", term::one_line(&call.detail.action))];
    if !call.detail.request.is_null() {
        let text = serde_json::to_string_pretty(&call.detail.request).unwrap_or_default();
        lines.extend(text.lines().map(term::one_line));
    }
    lines
}

fn score(p: &Value) -> String {
    p.as_f64().map_or("?".to_string(), |p| format!("{p:.2}"))
}

/// The list's columns at one width: at 100 and more, the caller has its
/// own column; narrower, it goes under the task's title.
struct Layout {
    caller: Option<usize>,
    task: usize,
    result: usize,
}

const TIME: usize = 8;

impl Layout {
    fn for_width(width: usize) -> Layout {
        if width >= 100 {
            // A margin, the selection mark, TIME, CALLER, the fold mark,
            // TASK and RESULT, with two-space gaps.
            let (caller, result) = (18, 32);
            let task = width.saturating_sub(1 + 2 + TIME + 2 + caller + 2 + 2 + 2 + result + 1);
            Layout {
                caller: Some(caller),
                task,
                result,
            }
        } else {
            let result = 25.min(width / 3);
            let task = width.saturating_sub(1 + TIME + 2 + 2 + 2 + result + 1);
            Layout {
                caller: None,
                task,
                result,
            }
        }
    }

    /// One screen line in the list's columns.
    fn line(
        &self,
        mark: &str,
        time: &str,
        caller: &str,
        fold: &str,
        title: &str,
        result: &str,
    ) -> String {
        match self.caller {
            Some(width) => format!(
                "{mark}  {}  {}  {fold} {}  {result}",
                pad(time, TIME),
                pad(caller, width),
                pad(title, self.task)
            ),
            None => format!(
                "{mark}{}  {fold} {}  {result}",
                pad(time, TIME),
                pad(title, self.task)
            ),
        }
    }

    fn heading(&self) -> String {
        self.line(" ", "TIME", "CALLER", " ", "TASK / STEP", "RESULT")
    }

    /// One item as its lines: the first with every column, then the
    /// continuations of whatever wrapped.
    fn row(&self, item: &Item, selected: bool) -> Vec<String> {
        let fold = match item.fold {
            Some(true) => "v",
            Some(false) => ">",
            None => " ",
        };
        let time = match item.sel {
            Some(Sel::Step(..)) | None => String::new(),
            _ => item.at.map(clock).unwrap_or_default(),
        };
        let indent = "  ".repeat(item.depth);
        let task_width = self.task.saturating_sub(indent.len()).max(10);
        let mut titles: Vec<String> = wrap(&term::one_line(&item.title), task_width)
            .into_iter()
            .map(|line| format!("{indent}{line}"))
            .collect();
        let mut callers = Vec::new();
        if let Some(caller) = &item.caller {
            match self.caller {
                Some(width) => callers = wrap(&caller.label, width),
                // Narrow: under the title.
                None => titles.extend(
                    wrap(&caller.label, task_width)
                        .into_iter()
                        .map(|line| format!("{indent}{line}")),
                ),
            }
        }
        let results = match &item.outcome {
            Some(outcome) => wrap(&outcome.line(), self.result),
            None => Vec::new(),
        };
        let height = titles.len().max(results.len()).max(callers.len()).max(1);
        let problem = item.outcome.as_ref().is_some_and(Outcome::problem);
        let at = |lines: &[String], n: usize| lines.get(n).cloned().unwrap_or_default();
        (0..height)
            .map(|n| {
                let first = n == 0;
                let text = self.line(
                    if first && selected { ">" } else { " " },
                    if first { &time } else { "" },
                    &at(&callers, n),
                    if first { fold } else { " " },
                    &at(&titles, n),
                    "",
                );
                let text = if first && selected {
                    paint::bold(&text, VOLT)
                } else {
                    text
                };
                let result = at(&results, n);
                if problem {
                    format!("{text}{}", paint::fg(&result, RED))
                } else {
                    format!("{text}{result}")
                }
            })
            .collect()
    }
}

/// `text` padded to `width` with spaces (never cut: a caller wraps first).
fn pad(text: &str, width: usize) -> String {
    let count = text.chars().count();
    format!("{text}{}", " ".repeat(width.saturating_sub(count)))
}

/// `text` on lines of at most `width` characters, broken at spaces where
/// it can be, mid-word where a word is longer than a line. Nothing is cut.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    // An indented line (JSON, a list) keeps its indent on every line.
    let indent = text.len() - text.trim_start_matches(' ').len();
    if indent > 0 && indent < width {
        let pad = " ".repeat(indent);
        return wrap(&text[indent..], width - indent)
            .into_iter()
            .map(|line| format!("{pad}{line}"))
            .collect();
    }
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ').filter(|word| !word.is_empty()) {
        let mut word = word.to_string();
        loop {
            let used = line.chars().count();
            let room = if used == 0 {
                width
            } else {
                width.saturating_sub(used + 1)
            };
            let length = word.chars().count();
            if length <= room {
                if used > 0 {
                    line.push(' ');
                }
                line.push_str(&word);
                break;
            }
            if used > 0 {
                lines.push(std::mem::take(&mut line));
                continue;
            }
            // A word longer than a line: as much as fits, then the rest.
            let head: String = word.chars().take(width).collect();
            lines.push(head);
            word = word.chars().skip(width).collect();
            if word.is_empty() {
                break;
            }
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// A line that may hold color, wrapped as plain text when it is too wide.
fn wrap_painted(line: &str, width: usize) -> Vec<String> {
    let plain = strip(line);
    if plain.chars().count() <= width {
        return vec![line.to_string()];
    }
    wrap(&plain, width)
}

/// `text` without its color escapes.
fn strip(text: &str) -> String {
    let mut plain = String::new();
    let mut escape = false;
    for ch in text.chars() {
        match (escape, ch) {
            (false, '\u{1b}') => escape = true,
            (true, 'm') => escape = false,
            (true, _) => {}
            (false, ch) => plain.push(ch),
        }
    }
    plain
}

trait UpperFirst {
    fn to_uppercase_first(&self) -> String;
}

impl UpperFirst for String {
    fn to_uppercase_first(&self) -> String {
        let mut words = self.splitn(2, " - ");
        let state = words.next().unwrap_or_default().to_uppercase();
        match words.next() {
            Some(rest) => format!("{state} - {rest}"),
            None => state,
        }
    }
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `14:02:07` in this machine's local time.
fn clock(at: Timestamp) -> String {
    at.to_zoned(TimeZone::system())
        .strftime("%H:%M:%S")
        .to_string()
}

fn clock_short(at: Timestamp) -> String {
    at.to_zoned(TimeZone::system())
        .strftime("%H:%M")
        .to_string()
}

fn full_time(at: Timestamp) -> String {
    at.to_zoned(TimeZone::system())
        .strftime("%Y-%m-%d %H:%M:%S %Z")
        .to_string()
}

/// `18m`, `2h 5m`, `40s`.
fn ago(since: Timestamp, now: Timestamp) -> String {
    let seconds = (now.as_second() - since.as_second()).max(0);
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        _ => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
    }
}

fn human_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

#[cfg(test)]
pub(super) mod testing {
    use super::*;

    impl View {
        pub fn selected(&self) -> Option<Sel> {
            self.selected
        }
    }
}
