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
        label: "you (local)".into(),
        detail: "workstation, on this host's own socket".into(),
    }
}

fn denied(code: &str, message: &str, details: Value) -> Response {
    Response::Error {
        exit: Exit::Denied,
        code: code.into(),
        message: message.into(),
        details,
        next: None,
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
    let dashboard = Dashboard::new("workstation");
    dashboard.serving();
    dashboard.directory(true);
    dashboard.socket(true);
    dashboard.connectors(vec![
        ("messages".into(), "enabled".into()),
        ("google".into(), "disabled".into()),
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
            next: None,
        },
        None,
    );
    let gate = json!({"caller": "agent", "rule": "allowed when within instructions >= 0.50",
        "verdict": "not_covered",
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
    assert_eq!(exec.calls[0].outcome.line(), "Done - exit 0");
    let upload = board.tasks.front().unwrap();
    assert_eq!(upload.title, "Write: report.pdf");
    assert_eq!(upload.outcome().line(), "Refused - needed the owner's OK");
    let detail = &upload.calls[0].detail;
    assert!(detail.audit_id.as_deref().unwrap().starts_with("27ff057f"));
}

#[test]
fn results_say_what_happened_in_words() {
    let dashboard = Dashboard::new("host");
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
        next: None,
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
    let dashboard = Dashboard::new("host");
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
    let dashboard = Dashboard::new("host");
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

    let lines = screen(&view_of(&dashboard, false), 120, 400);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("earlier calls of this session unavailable"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("earlier calls unavailable"))
    );
}

#[test]
fn each_column_is_its_own_at_120_and_the_caller_goes_under_the_command_at_80() {
    // Width, height, the status lines under the host's, and how far under
    // the title the caller is.
    let layouts: [(usize, usize, &[&str], usize); 2] = [
        (
            120,
            30,
            &["Messages: enabled | Google: disabled | Local socket: listening"],
            0,
        ),
        (
            80,
            24,
            &[
                "Directory: connected | Local socket: listening",
                "Messages: enabled | Google: disabled",
            ],
            1,
        ),
    ];
    for (width, height, status, caller_row) in layouts {
        let lines = screen(&view_of(&busy(), true), width, height);
        assert!(
            lines.iter().all(|line| line.chars().count() <= width),
            "{lines:#?}"
        );
        assert!(
            lines[0].starts_with("HOST workstation | Serving since "),
            "{}",
            lines[0]
        );
        assert_eq!(lines[0].ends_with("| Directory: connected"), width == 120);
        assert_eq!(&lines[1..=status.len()], status);
        let heading = lines.iter().find(|line| line.contains("COMMAND")).unwrap();
        assert_eq!(heading.contains("CALLER"), caller_row == 0, "{heading}");
        assert!(heading.contains("KIND"), "{heading}");
        let at = lines
            .iter()
            .position(|line| line.contains("file      put report.pdf"))
            .unwrap();
        assert!(
            lines[at + caller_row].contains("report-agent"),
            "{:?}",
            &lines[at..at + 2]
        );
        // The result column lines up with its heading, and its reason is
        // never cut: it wraps whole onto the next line.
        let result = heading.find("RESULT").unwrap();
        assert_eq!(lines[at].find("refused"), Some(result), "{heading}");
        let words: Vec<&str> = lines[at..at + 2]
            .iter()
            .map(|line| line.get(result..).unwrap_or_default().trim())
            .collect();
        assert_eq!(words.join(" ").trim(), "refused · needed the owner's OK");
        assert!(!lines.iter().any(|line| line.contains('…')));
        let footer = lines[lines.len() - 2..].join("\n");
        assert!(
            footer.contains("q detach") && footer.contains("Ctrl-C stop host"),
            "{footer}"
        );
    }

    // A detached viewer is not offered to stop the host.
    let lines = screen(&view_of(&busy(), false), 80, 24);
    assert!(
        !lines.iter().any(|line| line.contains("stop host")),
        "{lines:#?}"
    );

    // The session: its calls as typed, under the open that started it.
    let lines = screen(&view_of(&busy(), true), 120, 30);
    let at = |text: &str| lines.iter().position(|line| line.contains(text)).unwrap();
    let open = &lines[at("open https://example.com/ --task 'Check order")];
    assert!(
        open.contains("you (local)  ") && open.contains("browser"),
        "{open}"
    );
    let plan = &lines[at("do 'Open example.com'")];
    assert!(at("--task 'Check order") < at("do 'Open example.com'"));
    assert!(
        plan.contains("browser     do 'Open example.com'"),
        "indented, with no time or caller: {plan}"
    );
    assert!(
        plan.trim_end().ends_with("stopped · step 3; 2 ran"),
        "{plan}"
    );
    // Each call's kind and its command as typed.
    assert!(
        lines
            .iter()
            .any(|line| line.contains("exec      cargo test"))
    );
}

#[test]
fn the_decision_tab_shows_the_judgment_as_recorded_and_no_more() {
    let mut view = view_of(&busy(), false);
    view.key(Key::Enter);
    let steps = screen(&view, 80, 40).join("\n");
    assert!(
        steps.contains("Write: report.pdf | report-agent |"),
        "{steps}"
    );
    assert!(steps.contains("[Steps]"));
    assert!(
        steps.contains("REFUSED - needed the owner's OK. This action did not run."),
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
    assert!(decision.contains("Decision rule: allowed when within instructions >= 0.50"));
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
            .any(|line| line.contains("COMMAND"))
    );
}

#[test]
fn new_work_never_moves_the_selection_and_a_pause_holds_the_screen() {
    let dashboard = busy();
    let mut view = view_of(&dashboard, false);
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
    assert!(!marked[0].contains("exec      ls"), "{marked:?}");

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
        paused.contains("Paused") && !paused.contains("exec      pwd"),
        "{paused}"
    );
    view.key(Key::End);
    assert!(screen(&view, 120, 30).join("\n").contains("exec      pwd"));

    // `r`: only what went wrong; `/`: a filter.
    view.key(Key::Char('r'));
    let problems = screen(&view, 120, 30).join("\n");
    assert!(problems.contains("put report.pdf") && !problems.contains("exec      pwd"));
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
        filtered.contains("exec      cargo test") && !filtered.contains("exec      pwd"),
        "{filtered}"
    );
}

#[test]
fn a_host_that_went_away_is_never_shown_as_live() {
    let mut view = view_of(&busy(), false);
    view.gone = Some(jiff::Timestamp::now());
    let lines = screen(&view, 80, 24);
    assert!(
        lines[0].starts_with("HOST workstation | Host disconnected; last update "),
        "{lines:#?}"
    );
    // Nothing the host said of itself while it ran is shown as still so.
    let all = lines.join("\n");
    for live in [
        "Serving since",
        "Directory: connected",
        "Local socket: listening",
    ] {
        assert!(!all.contains(live), "{all}");
    }
}

#[test]
fn an_update_stands_under_the_host_line() {
    let dashboard = busy();
    dashboard.updated("latch was updated; restart to use build 133".into());
    for width in [120, 80] {
        let lines = screen(&view_of(&dashboard, true), width, 24);
        let at = lines
            .iter()
            .position(|line| line == "latch was updated; restart to use build 133")
            .unwrap_or_else(|| panic!("{lines:#?}"));
        assert!(at <= 3, "{lines:#?}");
    }
}

#[test]
fn a_newer_build_stands_under_the_host_line() {
    let dashboard = busy();
    dashboard.available(Some("Build 134 is available. Run `latch update`.".into()));
    for width in [120, 80] {
        let lines = screen(&view_of(&dashboard, true), width, 24);
        let at = lines
            .iter()
            .position(|line| line == "Build 134 is available. Run `latch update`.")
            .unwrap_or_else(|| panic!("{lines:#?}"));
        assert!(at <= 3, "{lines:#?}");
    }
    dashboard.available(None);
    let lines = screen(&view_of(&dashboard, true), 120, 24);
    assert!(
        !lines.iter().any(|line| line.contains("available")),
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

/// The plan from `busy`, begun again in its session: running, its steps
/// still to come.
fn plan_in_session(dashboard: &Dashboard, request: &str) -> CallId {
    let plan = json!({"command": "do", "session": "the-session-id", "request": request,
        "plan": {"steps": [{"goal": "Open example.com"}, {"goal": "Find recent orders"},
                           {"goal": "Open order details"}]}});
    dashboard.begin(owner(), session("browser", &plan), begun("browser", &plan))
}

#[test]
fn a_running_plan_shows_its_step_as_each_one_ends() {
    let dashboard = busy();
    plan_in_session(&dashboard, "r1");
    let session = crate::browser::host::hash_of("the-session-id");
    let task = |board: &Board| board.tasks.front().unwrap().clone();
    assert_eq!(
        task(&board(&dashboard)).outcome().line(),
        "Running - step 1"
    );
    dashboard.progress(&session, "r1", &json!({"step": 1, "outcome": "checked"}));
    dashboard.progress(&session, "r1", &json!({"step": 2, "outcome": "checked"}));
    let running = task(&board(&dashboard));
    assert_eq!(running.outcome().line(), "Running - step 3");
    let plan = running.calls.last().unwrap();
    let steps: Vec<String> = plan
        .steps
        .iter()
        .map(|step| step.outcome.as_ref().map_or("-".into(), Outcome::line))
        .collect();
    assert_eq!(steps, ["Done", "Done", "-"]);

    // On the list, running, as asked; in its details, two done, the third
    // still to come.
    let mut view = view_of(&dashboard, false);
    let lines = screen(&view, 120, 30);
    let row = lines
        .iter()
        .rfind(|line| line.contains("do 'Open example.com'"))
        .unwrap();
    assert!(row.trim_end().ends_with("running · step 3"), "{row}");
    for _ in 0..2 {
        view.key(Key::Down);
    }
    view.key(Key::Enter);
    let details = screen(&view, 100, 40).join("\n");
    assert!(
        details.contains("RUNNING - step 3. It is still running."),
        "{details}"
    );
    assert!(
        details.contains("3 Open order details — pending"),
        "{details}"
    );
}

#[test]
fn overlapping_plans_in_a_session_each_show_their_own_steps() {
    let dashboard = Dashboard::new("host");
    plan_in_session(&dashboard, "r1");
    plan_in_session(&dashboard, "r2");
    let session = crate::browser::host::hash_of("the-session-id");
    dashboard.progress(&session, "r2", &json!({"step": 1, "outcome": "checked"}));
    dashboard.progress(&session, "r2", &json!({"step": 2, "outcome": "checked"}));
    let lines: Vec<String> = board(&dashboard).tasks[0]
        .calls
        .iter()
        .map(|call| call.outcome.line())
        .collect();
    assert_eq!(lines, ["Running - step 1", "Running - step 3"]);
}

#[test]
fn a_session_shows_its_latest_plan_or_problem_not_just_its_last_call() {
    let dashboard = busy();
    let session_task = |dashboard: &Dashboard| {
        board(dashboard)
            .tasks
            .into_iter()
            .find(|task| task.session.is_some())
            .unwrap()
    };
    // An observation that went fine after the stopped plan.
    let observe = json!({"command": "observe", "session": "the-session-id"});
    let hash = crate::browser::host::hash_of("the-session-id");
    let observed = json!({"command": "observe", "session": hash});
    run(
        &dashboard,
        owner(),
        "browser",
        observe,
        observed,
        Response::Ok(json!({})),
        None,
    );
    assert_eq!(
        session_task(&dashboard).outcome().line(),
        "Stopped - step 3; 2 ran"
    );

    // A later plan that went through is what the session shows then.
    let id = plan_in_session(&dashboard, "r1");
    let plan = json!({"command": "do", "session": "the-session-id",
        "plan": {"steps": [{"goal": "Open example.com"}, {"goal": "Find recent orders"},
                           {"goal": "Open order details"}]}});
    let done = json!({"command": "do", "session": hash, "step_outcomes": [
        {"step": 1, "outcome": "checked"}, {"step": 2, "outcome": "checked"},
        {"step": 3, "outcome": "checked"}]});
    let ended = finished(
        "browser",
        &plan,
        &done,
        &Response::Ok(json!({})),
        None,
        None,
        Duration::ZERO,
    );
    dashboard.end(id, owner(), ended);
    assert_eq!(session_task(&dashboard).outcome().line(), "Done - 3 steps");
}

#[test]
fn a_row_taller_than_the_screen_shows_what_fits_and_where_the_rest_is() {
    let dashboard = Dashboard::new("host");
    let long = "The owner said not to do this. ".repeat(20);
    let refused = denied(
        "gatekeeper_denied",
        &long,
        json!({"reason": "owner_said_no"}),
    );
    let words: Vec<String> = (0..60).map(|n| format!("word{n}")).collect();
    let request = json!({"argv": words});
    run(
        &dashboard,
        agent(),
        "exec",
        request.clone(),
        request,
        refused,
        None,
    );
    let mut view = view_of(&dashboard, false);
    // Its command wraps onto more lines than the 80 by 12 screen has room.
    let lines = screen(&view, 80, 12);
    assert_eq!(lines.len(), 12);
    assert!(
        lines.iter().any(|line| line.starts_with('>')),
        "the selected row shows: {lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("… more; Enter shows it all")),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("refused · owner said")),
        "{lines:#?}"
    );
    // And all of it is in its details.
    view.key(Key::Enter);
    let details = screen(&view, 80, 60).join("\n");
    assert!(details.contains("word59"), "{details}");
}

#[test]
fn the_first_row_is_selected_and_keys_do_what_they_say() {
    let mut view = view_of(&busy(), false);
    let first = screen(&view, 120, 30);
    let marked: Vec<&String> = first.iter().filter(|line| line.starts_with('>')).collect();
    assert_eq!(marked.len(), 1, "{first:#?}");
    assert!(marked[0].contains("put report.pdf"));
    // Space pauses and resumes; Esc clears a filter and keeps a row selected.
    view.key(Key::Char(' '));
    assert!(screen(&view, 120, 30).join("\n").contains("Paused"));
    view.key(Key::Char(' '));
    assert!(!screen(&view, 120, 30).join("\n").contains("Paused"));
    for key in [
        Key::Char('/'),
        Key::Char('c'),
        Key::Char('a'),
        Key::Char('r'),
        Key::Enter,
    ] {
        view.key(key);
    }
    let filtered = screen(&view, 120, 30);
    assert!(
        filtered
            .iter()
            .any(|line| line.starts_with('>') && line.contains("cargo test"))
    );
    view.key(Key::Esc);
    let cleared = screen(&view, 120, 30).join("\n");
    assert!(
        cleared.contains("put report.pdf") && !cleared.contains("Filter:"),
        "{cleared}"
    );
}

#[test]
fn rows_hidden_above_are_said_and_home_goes_back_to_them() {
    let dashboard = Dashboard::new("host");
    for n in 0..12 {
        let request = json!({"argv": ["echo", format!("call {n}")]});
        let audit = json!({"argv": ["echo", format!("call {n}")], "exit_code": 0});
        run(
            &dashboard,
            owner(),
            "exec",
            request,
            audit,
            Response::Ok(json!({})),
            None,
        );
    }
    let mut view = view_of(&dashboard, false);
    for _ in 0..11 {
        view.key(Key::Down);
    }
    let bottom = screen(&view, 80, 16);
    assert!(
        bottom
            .iter()
            .any(|line| line.contains("… more above; Home goes to the top")),
        "{bottom:#?}"
    );
    assert!(
        bottom
            .iter()
            .any(|line| line.starts_with('>') && line.contains("call 0")),
        "{bottom:#?}"
    );
    view.key(Key::Home);
    let top = screen(&view, 80, 16);
    assert!(
        top.iter()
            .any(|line| line.starts_with('>') && line.contains("call 11")),
        "{top:#?}"
    );
    assert!(!top.iter().any(|line| line.contains("more above")));
}

#[test]
fn a_browser_call_shows_the_judgment_its_own_record_holds() {
    let dashboard = Dashboard::new("host");
    let plan = json!({"command": "do", "session": "s",
        "plan": {"steps": [{"literal": {"op": "click", "ref": "e7"},
                            "check": {"kind": "path", "value": "/nowhere.html"}}]}});
    let audit = json!({"command": "do", "session": "x", "origin": "http://127.0.0.1:5", "path": "/search.html",
        "gate": {"caller": "owner_device", "rule": "allowed when within instructions >= 0.50",
                 "p": {"COVERED": 0.9, "NOT_COVERED": 0.05, "FORBIDDEN": 0.05}},
        "step_outcomes": [{"step": 1, "outcome": "stopped", "code": "check_not_met"}],
        "stop": {"step": 1, "code": "check_not_met"}});
    let stopped = Response::Error {
        exit: Exit::Failed,
        code: "check_not_met".into(),
        message: "step 1's check did not hold within 10s".into(),
        details: json!({"page": {"url": "http://127.0.0.1:5/cart.html?id=7"}}),
        next: None,
    };
    run(&dashboard, owner(), "browser", plan, audit, stopped, None);
    let call = board(&dashboard).tasks[0].calls[0].clone();
    assert_eq!(call.title, "click e7 -> path: /nowhere.html");
    assert_eq!(
        call.detail.page.as_deref(),
        Some("http://127.0.0.1:5/cart.html"),
        "where it stopped, no query"
    );
    let mut view = view_of(&dashboard, false);
    view.key(Key::Enter);
    view.key(Key::Tab);
    let decision = screen(&view, 100, 30).join("\n");
    assert!(
        decision.contains("Judge P: within instructions 0.90"),
        "{decision}"
    );
}

#[test]
fn a_fill_refused_part_way_through_a_plan_says_what_ran_first() {
    let dashboard = Dashboard::new("host");
    let plan = json!({"command": "do", "session": "s",
        "plan": {"steps": [{"goal": "Open the sign-in page"}, {"goal": "Find the form"},
                           {"literal": {"op": "fill", "ref": "e4", "item": "bank"}}]}});
    let audit = json!({"command": "do", "session": "x",
        "step_outcomes": [{"step": 1, "outcome": "checked"}, {"step": 2, "outcome": "checked"},
                          {"step": 3, "outcome": "stopped", "code": "gatekeeper_denied"}],
        "stop": {"step": 3, "code": "gatekeeper_denied"}});
    let no = denied(
        "gatekeeper_denied",
        "The owner's messages don't cover this.",
        json!({}),
    );
    run(
        &dashboard,
        agent(),
        "browser",
        plan,
        audit.clone(),
        no,
        None,
    );
    let call = board(&dashboard).tasks[0].calls[0].clone();
    assert_eq!(call.outcome.line(), "Stopped - step 3; 2 ran");
    assert_eq!(
        call.steps[2].outcome.as_ref().map(Outcome::line).as_deref(),
        Some("Refused - needed the owner's OK")
    );
    // So it reads after a restart too, from its audit record alone.
    let mut record = crate::audit::record(
        "agent-key",
        Some("agent-1".into()),
        "browser",
        "deny",
        "gatekeeper_denied",
    );
    record.time = "2026-10-22T14:00:00Z".into();
    record.args = audit;
    let restored = Dashboard::new("host");
    restored.restore(recorded(&record, "host-key").into_iter().collect());
    for dashboard in [&dashboard, &restored] {
        let mut view = view_of(dashboard, false);
        view.key(Key::Enter);
        let inspector = screen(&view, 100, 30).join("\n");
        assert!(
            inspector.contains("STOPPED - step 3; 2 ran."),
            "{inspector}"
        );
        assert!(!inspector.contains("did not run"), "{inspector}");
    }
}

#[test]
fn each_call_shows_its_kind_and_its_command_as_typed() {
    let dashboard = Dashboard::new("host");
    let ok = || Response::Ok(json!({}));
    let failing = json!({"argv": ["sh", "-c", "echo went wrong >&2; exit 1"], "exit_code": 1});
    run(
        &dashboard,
        owner(),
        "exec",
        failing.clone(),
        failing,
        ok(),
        None,
    );
    let network = json!({"argv": ["curl", "-sSI", "https://example.com"], "network": true,
                         "writes": ["out dir"], "exit_code": 0});
    run(
        &dashboard,
        owner(),
        "exec",
        network.clone(),
        network,
        ok(),
        None,
    );
    // As sent: the secret's value; as audited: its length. Only its name shows.
    let secret = json!({"argv": ["sh", "-c", "echo $DEMO"], "workdir": "/Users/o",
                        "secret_env": {"DEMO": "hunter2"}, "timeout": 30, "stdin": [104, 105]});
    let audited = json!({"argv": ["sh", "-c", "echo $DEMO"], "workdir": "/Users/o",
                         "secret_env": [{"name": "DEMO", "secret": true, "length": 7}],
                         "timeout": 30, "stdin": {"size": 2}, "exit_code": 0});
    run(
        &dashboard,
        owner(),
        "exec",
        secret,
        audited.clone(),
        ok(),
        None,
    );
    // Where it runs unless told: home, or its first write folder.
    let home = std::env::home_dir().unwrap();
    let at_home = json!({"argv": ["ls"], "workdir": home, "exit_code": 0});
    run(
        &dashboard,
        owner(),
        "exec",
        at_home.clone(),
        at_home,
        ok(),
        None,
    );
    let in_project = json!({"argv": ["make"], "workdir": "/p", "writes": ["/p"], "exit_code": 0});
    run(
        &dashboard,
        owner(),
        "exec",
        in_project.clone(),
        in_project,
        ok(),
        None,
    );
    // As the client sends it: `--account` lifted out of gog's arguments.
    let labels = json!({"gog_args": ["gmail", "labels", "list"],
                        "account": ["owner@example.com"]});
    let unconnected = Response::Error {
        exit: Exit::Failed,
        code: "account_required".into(),
        message: "connect an account first".into(),
        details: Value::Null,
        next: None,
    };
    run(
        &dashboard,
        owner(),
        "google",
        labels,
        json!({}),
        unconnected,
        None,
    );
    let search = json!({"action": "search", "args": ["--", "the code"]});
    run(
        &dashboard,
        owner(),
        "messages",
        search,
        json!({"action": "messages.search"}),
        ok(),
        None,
    );
    run(
        &dashboard,
        owner(),
        "file_get",
        json!({"remote_path": "notes.txt"}),
        json!({"path": "notes.txt", "size": 10}),
        ok(),
        None,
    );
    let observe = json!({"command": "observe", "session": "s1"});
    let observed = json!({"command": "observe", "session": "s1hash"});
    run(
        &dashboard,
        owner(),
        "browser",
        observe,
        observed,
        ok(),
        None,
    );
    let rows: Vec<(String, String, String)> = board(&dashboard)
        .tasks
        .iter()
        .rev()
        .map(|task| {
            let call = &task.calls[0];
            (
                call.kind.clone(),
                call.command.clone(),
                call.outcome.short(),
            )
        })
        .collect();
    let row = |kind: &str, command: &str, result: &str| {
        (kind.to_string(), command.to_string(), result.to_string())
    };
    assert_eq!(
        rows,
        [
            row("exec", "sh -c 'echo went wrong >&2; exit 1'", "exit 1"),
            row(
                "exec",
                "--write 'out dir' --network curl -sSI https://example.com",
                "ok · exit 0"
            ),
            row(
                "exec",
                "--workdir /Users/o --secret-env DEMO=••• --timeout 30 -i sh -c 'echo $DEMO'",
                "ok · exit 0"
            ),
            row("exec", "ls", "ok · exit 0"),
            row("exec", "--write /p make", "ok · exit 0"),
            row(
                "google",
                "--account owner@example.com gmail labels list",
                "failed · account required"
            ),
            // Never the search.
            row("messages", "search", "ok"),
            row("file", "get notes.txt", "ok · 10 B"),
            row("browser", "observe", "ok"),
        ]
    );
    assert!(
        !serde_json::to_string(&board(&dashboard))
            .unwrap()
            .contains("hunter2")
    );
    // A call restored from its audit record alone reads the same.
    let mut record = crate::audit::record("host-key", None, "exec", "allow", "ok");
    record.time = "2026-10-22T14:00:00Z".into();
    record.args = audited;
    let (_, _, restored) = recorded(&record, "host-key").unwrap();
    assert_eq!(
        restored.command,
        "--workdir /Users/o --secret-env DEMO=••• --timeout 30 -i sh -c 'echo $DEMO'"
    );
    // And so it does in the inspector.
    let mut view = view_of(&dashboard, false);
    view.key(Key::Char('/'));
    for ch in "DEMO".chars() {
        view.key(Key::Char(ch));
    }
    view.key(Key::Enter);
    view.key(Key::Enter);
    let details = screen(&view, 120, 30).join("\n");
    assert!(
        details.contains(
            "Command: latch exec --workdir /Users/o --secret-env DEMO=••• --timeout 30 -i sh -c"
        ),
        "{details}"
    );
}

/// A host that starts again shows what its log holds of the calls before.
#[test]
fn a_board_starts_with_the_calls_the_audit_log_holds() {
    let record = |caller: &str, agent: Option<&str>, op: &str, args: Value, outcome: &str| {
        let mut record =
            crate::audit::record(caller, agent.map(str::to_string), op, "allow", outcome);
        record.time = "2026-10-22T14:00:00Z".into();
        record.args = args;
        record.hash = "ab".repeat(32);
        record
    };
    let hash = crate::browser::host::hash_of("s");
    let records = [
        record(
            "host-key",
            None,
            "exec",
            json!({"argv": ["ls"], "exit_code": 0}),
            "ok",
        ),
        record(
            "host-key",
            None,
            "browser",
            json!({"command": "open", "session": hash, "origin": "https://example.com", "path": "/"}),
            "ok",
        ),
        record(
            "device-key",
            None,
            "browser",
            json!({"command": "close", "session": hash}),
            "ok",
        ),
        record(
            "agent-key-0123456789",
            Some("agent-1"),
            "exec",
            json!({"argv": ["cargo", "test"]}),
            "gatekeeper_denied",
        ),
    ];
    let dashboard = Dashboard::new("host");
    dashboard.restore(
        records
            .iter()
            .filter_map(|record| recorded(record, "host-key"))
            .collect(),
    );
    let lines = screen(&view_of(&dashboard, false), 120, 20);
    let at = |text: &str| {
        lines
            .iter()
            .position(|line| line.contains(text))
            .unwrap_or_else(|| panic!("{text}: {lines:#?}"))
    };
    let local = jiff::Timestamp::from_second(1_792_677_600)
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::system())
        .strftime("%H:%M:%S")
        .to_string();
    assert!(lines[at("exec      ls")].contains(&local), "{lines:#?}");
    assert!(lines[at("exec      ls")].contains("you (local)"));
    assert!(lines[at("open https://example.com/")].contains("ok"));
    // The session's close, under its open.
    assert_eq!(at("browser     close"), at("open https://example.com/") + 1);
    let refused = &lines[at("cargo test")];
    assert!(
        refused.contains("agent agent-ke") && refused.contains("refused · needed the owner"),
        "{refused}"
    );
}

#[test]
fn nothing_typed_queried_or_sent_as_a_terminal_command_reaches_the_screen() {
    let dashboard = Dashboard::new("host");
    let typing = json!({"command": "do", "session": "s",
        "plan": {"steps": [{"literal": {"op": "type", "ref": "e3", "text": "482913"}}]}});
    run(
        &dashboard,
        owner(),
        "browser",
        typing.clone(),
        json!({"command": "do"}),
        Response::Ok(json!({})),
        None,
    );
    let search = json!({"gog_args": ["gmail", "search", "private-query"]});
    run(
        &dashboard,
        owner(),
        "google",
        search.clone(),
        json!({}),
        Response::Ok(json!({})),
        None,
    );
    let driving = Response::Error {
        exit: Exit::Failed,
        code: "not_built_yet".into(),
        message: "this host does not run `\u{1b}[2J\u{1b}]0;owned\u{7}` yet".into(),
        details: Value::Null,
        next: None,
    };
    run(
        &dashboard,
        owner(),
        "\u{1b}[2J",
        json!({}),
        json!({}),
        driving,
        None,
    );
    let shown = serde_json::to_string(&board(&dashboard)).unwrap();
    assert!(
        !shown.contains("482913") && !shown.contains("private-query"),
        "{shown}"
    );
    let titles: Vec<String> = board(&dashboard)
        .tasks
        .iter()
        .flat_map(|task| task.calls.iter().map(|call| call.title.clone()))
        .collect();
    assert!(titles.contains(&"type e3".to_string()), "{titles:?}");
    assert!(
        titles.contains(&"Google: gmail.search".to_string()),
        "{titles:?}"
    );
    // The failure's details hold the caller's escape, shown and never sent.
    let mut view = view_of(&dashboard, false);
    view.key(Key::Enter);
    screen(&view, 80, 30);
}

#[test]
fn the_decision_tab_shows_the_owners_rule_for_an_owners_call() {
    let dashboard = Dashboard::new("host");
    let gate = json!({"caller": "owner_on_host",
        "rule": "refused when forbidden >= within instructions",
        "p": {"COVERED": 0.3, "NOT_COVERED": 0.3, "FORBIDDEN": 0.4}});
    let request = json!({"argv": ["rm", "-rf", "build"]});
    let refused = denied(
        "gatekeeper_denied",
        "The gatekeeper judged that this host's rules forbid this.",
        json!({"reason": "owner_said_no"}),
    );
    run(
        &dashboard,
        owner(),
        "exec",
        request.clone(),
        request,
        refused,
        Some(gate),
    );
    let mut view = view_of(&dashboard, false);
    view.key(Key::Enter);
    view.key(Key::Tab);
    let decision = screen(&view, 100, 30).join("\n");
    assert!(
        decision.contains("Refused - owner said no")
            || decision.contains("REFUSED - owner said no"),
        "{decision}"
    );
    assert!(
        decision.contains("Decision rule: refused when forbidden >= within instructions"),
        "{decision}"
    );
}

#[test]
fn a_refilled_call_is_at_the_time_it_began() {
    let mut record = crate::audit::record("host-key", None, "exec", "allow", "ok");
    record.args = json!({"argv": ["ls"], "exit_code": 0});
    record.time = "2026-10-22T14:00:02Z".into();
    record.started = Some("2026-10-22T14:00:00Z".into());
    let (_, at, _) = recorded(&record, "host-key").unwrap();
    assert_eq!(at.to_string(), "2026-10-22T14:00:00Z");
    // A record made before latch kept the start: the time it was made.
    record.started = None;
    let (_, at, _) = recorded(&record, "host-key").unwrap();
    assert_eq!(at.to_string(), "2026-10-22T14:00:02Z");
}

#[test]
fn a_frame_redraws_in_place_as_one_update_never_clearing_the_screen() {
    let drawn = super::show::frame("one\ntwo", 4);
    assert_eq!(
        drawn,
        "\x1b[?2026h\x1b[H\x1b[Kone\n\x1b[Ktwo\n\x1b[J\x1b[?2026l"
    );
    assert!(!drawn.contains("\x1b[2J"));
    // A frame as tall as the screen: nothing below it to clear, and no
    // line feed that would scroll it.
    assert_eq!(
        super::show::frame("one\ntwo", 2),
        "\x1b[?2026h\x1b[H\x1b[Kone\n\x1b[Ktwo\x1b[?2026l"
    );
}
