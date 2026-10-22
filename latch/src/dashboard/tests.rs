use std::time::Duration;

use serde_json::{Value, json};

use super::show::{parse, show_on};
use super::view::{Key, View};
use super::*;
use crate::failure::Exit;
use crate::wire::Response;

fn agent() -> Caller {
    Caller {
        label: "report-agent".into(),
        detail: "agent 0123, peer report-latch, key abcd".into(),
    }
}

fn owner() -> Caller {
    Caller {
        label: "the owner (local)".into(),
        detail: "workstation, on this host's own socket".into(),
    }
}

fn denied(code: &str, message: &str, details: Value) -> Response {
    Response::Error {
        exit: Exit::Denied,
        code: code.into(),
        message: message.into(),
        details,
    }
}

/// One call, begun and ended as the host does it.
fn run(
    dashboard: &Dashboard,
    caller: Caller,
    op: &str,
    request: Value,
    audit: Value,
    response: Response,
    gate: Option<Value>,
) {
    let id = dashboard.begin(caller.clone(), session(op, &request), begun(op, &request));
    let ended = finished(
        op,
        &request,
        &audit,
        &response,
        gate.as_ref(),
        Some("27ff057f18f54d04a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718"),
        Duration::from_millis(1200),
    );
    dashboard.end(id, caller, ended);
}

/// A host's board with an exec, a refused upload, and a browser session
/// whose plan stopped at its third step.
fn busy() -> Dashboard {
    let dashboard = Dashboard::new("workstation", None);
    dashboard.serving();
    dashboard.directory(true);
    dashboard.socket(true);
    dashboard.connectors(vec![
        ("messages".into(), "ready".into()),
        ("google".into(), "connected".into()),
    ]);
    run(
        &dashboard,
        Caller {
            label: "build-agent".into(),
            detail: "agent 99".into(),
        },
        "exec",
        json!({"argv": ["cargo", "test"]}),
        json!({"argv": ["cargo", "test"], "exit_code": 0}),
        Response::Ok(json!({})),
        None,
    );
    // The session: opened with a task label, then a plan of three steps.
    let open =
        json!({"command": "open", "url": "https://example.com/", "task": "Check order status"});
    let hash = crate::browser::host::hash_of("the-session-id");
    let opened = json!({"command": "open", "session": hash,
                        "task": "Check order status at example.com",
                        "origin": "https://example.com", "path": "/"});
    run(
        &dashboard,
        owner(),
        "browser",
        open,
        opened,
        Response::Ok(json!({})),
        None,
    );
    let plan = json!({"command": "do", "session": "the-session-id",
        "plan": {"steps": [{"goal": "Open example.com"}, {"goal": "Find recent orders"},
                           {"goal": "Open order details"}]}});
    let stopped = json!({"command": "do", "session": hash,
        "origin": "https://example.com", "path": "/orders",
        "step_outcomes": [{"step": 1, "outcome": "checked"}, {"step": 2, "outcome": "checked"},
                          {"step": 3, "outcome": "stopped", "code": "ambiguous_target"}],
        "stop": {"step": 3, "code": "ambiguous_target"}});
    run(
        &dashboard,
        owner(),
        "browser",
        plan,
        stopped,
        Response::Error {
            exit: Exit::Failed,
            code: "ambiguous_target".into(),
            message: "step 3: more than one element matches".into(),
            details: Value::Null,
        },
        None,
    );
    let gate = json!({"caller": "agent", "threshold": 0.5, "verdict": "not_covered",
        "p": {"COVERED": 0.21, "NOT_COVERED": 0.76, "FORBIDDEN": 0.03},
        "model": "jev-1.14", "source": "chat_and_rules",
        "rules": {"host": "aa".repeat(32), "agent": "bb".repeat(32)},
        "chat": "chat-7", "window": {"first": "m1", "last": "m9", "count": 9}});
    run(
        &dashboard,
        agent(),
        "file_put",
        json!({"remote_path": "report.pdf", "size": 10}),
        json!({"path": "report.pdf"}),
        denied(
            "gatekeeper_denied",
            "The owner's messages don't cover this. Ask the owner in chat, saying exactly what you will do, then retry.",
            json!({"reason": "not_covered"}),
        ),
        Some(gate),
    );
    dashboard
}

fn board(dashboard: &Dashboard) -> Board {
    dashboard.watch().borrow().clone()
}

fn screen(view: &View, width: usize, height: usize) -> Vec<String> {
    let screen = view.screen(width, height);
    assert!(!screen.contains('\u{1b}'), "color is off in tests");
    screen.lines().map(str::to_string).collect()
}

fn view_of(dashboard: &Dashboard, attached: bool) -> View {
    let mut view = View::new(attached, None);
    view.board = board(dashboard);
    view
}

#[test]
fn a_browser_session_groups_its_calls_and_a_plan_its_steps() {
    let board = board(&busy());
    assert_eq!(
        board.tasks.len(),
        3,
        "exec, session, upload: {:?}",
        board.tasks
    );
    let session = board
        .tasks
        .iter()
        .find(|task| task.session.is_some())
        .unwrap();
    let hash = crate::browser::host::hash_of("the-session-id");
    assert_eq!(session.session.as_deref(), Some(hash.as_str()));
    assert_eq!(session.title, "Check order status at example.com");
    assert_eq!(session.calls.len(), 2, "open, then the plan");
    let plan = &session.calls[1];
    assert_eq!(
        plan.title,
        "Plan: Open example.com; Find recent orders; Open order details"
    );
    assert_eq!(plan.outcome.line(), "Stopped - step 3; 2 ran");
    let steps: Vec<String> = plan
        .steps
        .iter()
        .map(|step| step.outcome.as_ref().map_or("-".into(), Outcome::line))
        .collect();
    assert_eq!(steps, ["Done", "Done", "Stopped - target unclear"]);
    assert_eq!(session.outcome().line(), "Stopped - step 3; 2 ran");

    // The exec stands alone, as does the refused upload.
    let exec = board
        .tasks
        .iter()
        .find(|task| task.title == "Run: cargo test")
        .unwrap();
    assert_eq!(exec.lone().unwrap().outcome.line(), "Done - exit 0");
    let upload = board.tasks.front().unwrap();
    assert_eq!(upload.title, "Write: report.pdf");
    assert_eq!(
        upload.outcome().line(),
        "Refused - outside your instructions"
    );
    let detail = &upload.calls[0].detail;
    assert!(detail.audit_id.as_deref().unwrap().starts_with("27ff057f"));
    assert_eq!(detail.gate.as_ref().unwrap()["threshold"], 0.5);
}

#[test]
fn results_say_what_happened_in_words() {
    let dashboard = Dashboard::new("host", None);
    let exec = |code: i64| json!({"argv": ["false"], "exit_code": code});
    run(
        &dashboard,
        owner(),
        "exec",
        exec(1),
        exec(1),
        Response::Ok(json!({})),
        None,
    );
    let unavailable = Response::Error {
        exit: Exit::Unreachable,
        code: "gatekeeper_unavailable".into(),
        message: "the decision model did not answer".into(),
        details: Value::Null,
    };
    run(
        &dashboard,
        agent(),
        "exec",
        exec(0),
        exec(0),
        unavailable,
        None,
    );
    let no = denied(
        "gatekeeper_denied",
        "The owner said not to do this.",
        json!({"reason": "owner_said_no"}),
    );
    run(&dashboard, agent(), "exec", exec(0), exec(0), no, None);
    let search = json!({"action": "search", "args": ["--", "code"]});
    let found = json!({"action": "messages.search", "result": {"count": 4}});
    run(
        &dashboard,
        owner(),
        "messages",
        search,
        found,
        Response::Ok(json!({})),
        None,
    );
    let lines: Vec<String> = board(&dashboard)
        .tasks
        .iter()
        .rev()
        .map(|task| format!("{}: {}", task.title, task.outcome().line()))
        .collect();
    assert_eq!(
        lines,
        [
            "Run: false: Failed - exit 1",
            "Run: false: Refused - judge unavailable",
            "Run: false: Refused - owner said no",
            "Search Messages: Done - 4 matches",
        ]
    );
}

#[test]
fn a_running_call_shows_running_until_it_ends() {
    let dashboard = Dashboard::new("host", None);
    let request = json!({"argv": ["sleep", "5"]});
    let id = dashboard.begin(owner(), None, begun("exec", &request));
    let running = board(&dashboard);
    assert_eq!(running.tasks[0].outcome().line(), "Running");
    assert_eq!(running.tasks[0].title, "Run: sleep 5");
    let ended = finished(
        "exec",
        &request,
        &json!({"argv": ["sleep", "5"], "exit_code": 0}),
        &Response::Ok(json!({})),
        None,
        None,
        Duration::ZERO,
    );
    dashboard.end(id, owner(), ended);
    assert_eq!(board(&dashboard).tasks[0].outcome().line(), "Done - exit 0");
}

#[test]
fn what_was_dropped_is_said_never_implied_complete() {
    let dashboard = Dashboard::new("host", None);
    let request = json!({"argv": ["true"]});
    let audit = json!({"argv": ["true"], "exit_code": 0});
    for _ in 0..=KEEP_TASKS {
        run(
            &dashboard,
            owner(),
            "exec",
            request.clone(),
            audit.clone(),
            Response::Ok(json!({})),
            None,
        );
    }
    let call = |n: usize| json!({"command": "observe", "session": format!("s{n}")});
    for _ in 0..=KEEP_CALLS {
        let observed = json!({"command": "observe", "session": "lots"});
        run(
            &dashboard,
            owner(),
            "browser",
            call(0),
            observed,
            Response::Ok(json!({})),
            None,
        );
    }
    let shown = board(&dashboard);
    assert!(shown.dropped);
    assert_eq!(shown.tasks.len(), KEEP_TASKS);
    assert_eq!(shown.tasks[0].dropped, 1);

    let mut view = view_of(&dashboard, false);
    view.key(Key::Down);
    view.key(Key::Right);
    let lines = screen(&view, 120, 400);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Earlier steps unavailable"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Earlier tasks unavailable"))
    );
}

#[test]
fn at_120_columns_each_column_is_its_own_and_a_plan_unfolds() {
    let dashboard = busy();
    let mut view = view_of(&dashboard, true);
    let lines = screen(&view, 120, 30);
    assert!(
        lines.iter().all(|line| line.chars().count() <= 120),
        "{lines:#?}"
    );
    assert!(
        lines[0].starts_with("HOST workstation | Serving since "),
        "{}",
        lines[0]
    );
    assert!(lines[0].ends_with("| Directory: connected"), "{}", lines[0]);
    assert_eq!(
        lines[1],
        "Messages: ready | Google: connected | Local socket: listening"
    );
    let heading = lines
        .iter()
        .find(|line| line.contains("TASK / STEP"))
        .unwrap();
    for column in ["TIME", "CALLER", "RESULT"] {
        assert!(heading.contains(column), "{heading}");
    }
    let at = lines
        .iter()
        .position(|line| line.contains("Write: report.pdf"))
        .unwrap();
    let upload = &lines[at];
    assert!(
        upload.contains("report-agent"),
        "the caller has its column: {upload}"
    );
    // The result column lines up with its heading, and its reason wraps
    // whole onto the next line.
    let result = heading.find("RESULT").unwrap();
    assert_eq!(upload.find("Refused"), Some(result), "{upload}\n{heading}");
    assert_eq!(
        format!(
            "{} {}",
            upload[result..].trim(),
            lines[at + 1][result..].trim()
        ),
        "Refused - outside your instructions"
    );
    let footer = lines.last().unwrap();
    assert!(
        footer.contains("q detach") && footer.contains("Ctrl-C stop host"),
        "{footer}"
    );

    // Selecting the session and unfolding it, then its plan: the steps in
    // order, the one that stopped saying why.
    view.key(Key::Down);
    view.key(Key::Down);
    view.key(Key::Right);
    view.key(Key::Down);
    view.key(Key::Down);
    view.key(Key::Right);
    let lines = screen(&view, 120, 30);
    let at = |text: &str| lines.iter().position(|line| line.contains(text)).unwrap();
    assert!(at("Check order status at example.com") < at("1 Open example.com"));
    assert!(at("1 Open example.com") < at("3 Open order details"));
    let stopped = &lines[at("3 Open order details")];
    assert!(stopped.ends_with("Stopped - target unclear"), "{stopped}");
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with(">") && line.contains("Plan: Open"))
    );
}

#[test]
fn at_80_columns_the_caller_goes_under_the_title_and_a_refusal_wraps_whole() {
    let mut view = view_of(&busy(), true);
    let lines = screen(&view, 80, 24);
    assert!(
        lines.iter().all(|line| line.chars().count() <= 80),
        "{lines:#?}"
    );
    assert!(
        lines[0].starts_with("HOST workstation | Serving since "),
        "{}",
        lines[0]
    );
    assert_eq!(lines[1], "Directory: connected | Local socket: listening");
    assert_eq!(lines[2], "Messages: ready | Google: connected");
    let heading = lines
        .iter()
        .find(|line| line.contains("TASK / STEP"))
        .unwrap();
    assert!(!heading.contains("CALLER"), "{heading}");
    let at = lines
        .iter()
        .position(|line| line.contains("Write: report.pdf"))
        .unwrap();
    assert!(
        lines[at + 1].contains("report-agent"),
        "under the title: {:?}",
        &lines[at..at + 2]
    );
    // The reason is never cut: it wraps onto the next line.
    let result: String = lines[at..at + 2]
        .iter()
        .map(|line| line[heading.find("RESULT").unwrap()..].trim().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(result, "Refused - outside your instructions");
    assert!(!lines.iter().any(|line| line.contains('…')));
    let footer = &lines[lines.len() - 2..];
    assert!(footer[0].contains("q detach"), "{footer:?}");
    assert!(
        footer[1].contains("Ctrl-C stop host") && footer[1].contains("Live"),
        "{footer:?}"
    );

    // A detached viewer is not offered to stop the host.
    view = view_of(&busy(), false);
    let lines = screen(&view, 80, 24);
    assert!(
        !lines.iter().any(|line| line.contains("stop host")),
        "{lines:#?}"
    );
}

#[test]
fn the_decision_tab_shows_the_judgment_as_recorded_and_no_more() {
    let mut view = view_of(&busy(), false);
    view.key(Key::Down);
    view.key(Key::Enter);
    let steps = screen(&view, 80, 40).join("\n");
    assert!(
        steps.contains("Write: report.pdf | report-agent |"),
        "{steps}"
    );
    assert!(steps.contains("[Steps]"));
    assert!(
        steps.contains("REFUSED - outside your instructions. This action did not run."),
        "{steps}"
    );
    assert!(steps.contains("The owner's messages don't cover this."));
    view.key(Key::Tab);
    let decision = screen(&view, 80, 40).join("\n");
    assert!(decision.contains("[Decision]"));
    assert!(
        decision.contains("Judge P: within instructions 0.21 | not covered 0.76 | forbidden 0.03"),
        "{decision}"
    );
    assert!(decision.contains("Allow threshold: within instructions >= 0.50"));
    assert!(
        decision
            .contains("Rules used: this host + this agent | revisions: aaaaaaaaaaaa, bbbbbbbbbbbb")
    );
    assert!(decision.contains("Matched rule: not provided by the judge"));
    assert!(decision.contains("Chat: chat-7; messages m1 to m9 (9)"));
    // The whole audit id, wrapped where it must be, never cut.
    let id = "27ff057f18f54d04a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718";
    assert!(decision.replace('\n', "").contains(id), "{decision}");
    view.key(Key::Esc);
    assert!(
        screen(&view, 80, 40)
            .iter()
            .any(|line| line.contains("TASK / STEP"))
    );
}

#[test]
fn new_work_never_moves_the_selection_and_a_pause_holds_the_screen() {
    let dashboard = busy();
    let mut view = view_of(&dashboard, false);
    view.key(Key::Down);
    view.key(Key::Down);
    let selected = view.selected().unwrap();
    let request = json!({"argv": ["ls"]});
    run(
        &dashboard,
        owner(),
        "exec",
        request.clone(),
        json!({"argv": ["ls"], "exit_code": 0}),
        Response::Ok(json!({})),
        None,
    );
    view.board = board(&dashboard);
    assert_eq!(view.selected(), Some(selected));
    let lines = screen(&view, 120, 30);
    let marked: Vec<&String> = lines.iter().filter(|line| line.starts_with('>')).collect();
    assert_eq!(marked.len(), 1);
    assert!(!marked[0].contains("Run: ls"), "{marked:?}");

    view.key(Key::Char(' '));
    run(
        &dashboard,
        owner(),
        "exec",
        json!({"argv": ["pwd"]}),
        json!({"argv": ["pwd"], "exit_code": 0}),
        Response::Ok(json!({})),
        None,
    );
    view.board = board(&dashboard);
    let paused = screen(&view, 120, 30).join("\n");
    assert!(
        paused.contains("Paused") && !paused.contains("Run: pwd"),
        "{paused}"
    );
    view.key(Key::End);
    assert!(screen(&view, 120, 30).join("\n").contains("Run: pwd"));

    // `r`: only what went wrong; `/`: a filter.
    view.key(Key::Char('r'));
    let problems = screen(&view, 120, 30).join("\n");
    assert!(problems.contains("Write: report.pdf") && !problems.contains("Run: pwd"));
    view.key(Key::Char('r'));
    for key in [
        Key::Char('/'),
        Key::Char('c'),
        Key::Char('a'),
        Key::Char('r'),
        Key::Enter,
    ] {
        view.key(key);
    }
    let filtered = screen(&view, 120, 30).join("\n");
    assert!(
        filtered.contains("Run: cargo test") && !filtered.contains("Run: pwd"),
        "{filtered}"
    );
}

#[test]
fn a_host_that_went_away_is_never_shown_as_live() {
    let mut view = view_of(&busy(), false);
    view.gone = Some(jiff::Timestamp::now());
    let lines = screen(&view, 80, 24);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("Host disconnected; last update ")),
        "{lines:#?}"
    );
}

#[test]
fn keys_arrive_as_the_terminal_sends_them() {
    assert_eq!(
        parse(b"\x1b[A\x1b[B\x1b[C\x1b[D\x1b[F\r\tq/\x7f"),
        [
            Key::Up,
            Key::Down,
            Key::Right,
            Key::Left,
            Key::End,
            Key::Enter,
            Key::Tab,
            Key::Char('q'),
            Key::Char('/'),
            Key::Backspace
        ]
    );
    assert_eq!(parse(b"\x1b"), [Key::Esc]);
    assert_eq!(parse(b"\x1bOA"), [Key::Up]);
}

/// A terminal that takes no output: every write waits forever.
struct Stuck(std::sync::Mutex<std::sync::mpsc::Receiver<()>>);

impl std::io::Write for Stuck {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.lock().unwrap().recv();
        Ok(0)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn a_terminal_that_takes_no_output_never_holds_up_closing_the_view() {
    let (_never, wait) = std::sync::mpsc::channel();
    let (boards, receiver) = tokio::sync::mpsc::channel(4);
    let (_keys, keys) = tokio::sync::mpsc::channel(4);
    let feed = tokio::spawn(async move {
        for _ in 0..100 {
            if boards.send(Board::default()).await.is_err() {
                return;
            }
        }
        std::future::pending::<()>().await;
    });
    let started = std::time::Instant::now();
    let until = tokio::time::sleep(Duration::from_millis(200));
    let stuck = Box::new(Stuck(std::sync::Mutex::new(wait)));
    let ending = show_on(stuck, View::new(true, None), receiver, keys, until).await;
    assert_eq!(ending, Ending::Closed);
    assert!(started.elapsed() < Duration::from_secs(3));
    feed.abort();
}

#[tokio::test]
async fn q_detaches_and_an_attached_view_ends_with_its_host() {
    let (boards, receiver) = tokio::sync::mpsc::channel(4);
    let (keys, key_receiver) = tokio::sync::mpsc::channel(4);
    keys.send(Key::Char('q')).await.unwrap();
    let ending = show_on(
        Box::new(std::io::sink()),
        View::new(true, None),
        receiver,
        key_receiver,
        std::future::pending(),
    )
    .await;
    assert_eq!(ending, Ending::Detached);
    drop(boards);

    let (boards, receiver) = tokio::sync::mpsc::channel::<Board>(4);
    let (_keys, key_receiver) = tokio::sync::mpsc::channel(4);
    drop(boards);
    let ending = show_on(
        Box::new(std::io::sink()),
        View::new(true, None),
        receiver,
        key_receiver,
        std::future::pending(),
    )
    .await;
    assert_eq!(ending, Ending::Closed);
}
