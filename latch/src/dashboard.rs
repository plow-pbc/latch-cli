//! The host's live view: its name, the time of its last heartbeat, calls
//! per minute, and one row per call (time, calling agent, operation and
//! target, result). All of it is data the host already has; nothing here
//! changes what it does.
//!
//! The host keeps the board (`Dashboard`) and sends it over its local
//! socket to whoever watches: an attached `latch serve` or `latch
//! dashboard`, which draw it on their terminal (`show`).

use std::collections::VecDeque;
use std::io::Write;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiff::{Timestamp, tz::TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::watch;

use crate::paint::{self, DIM, FAINT, INK, RED, VOLT};
use crate::term;

/// How many calls the board remembers (more than any screen shows).
const KEEP: usize = 200;

/// How long a closing view waits to give the terminal back.
const GIVE_BACK_WITHIN: Duration = Duration::from_secs(1);

/// Column widths, in characters.
const TIME: usize = 8;
const AGENT: usize = 14;
const OPERATION: usize = 9;
const RESULT: usize = 22;

/// One call, as a row on the screen.
#[derive(Serialize, Deserialize, Clone)]
pub struct Row {
    pub at: Timestamp,
    /// The calling agent's name, or the start of its key if Plow did not
    /// know it.
    pub caller: String,
    pub op: String,
    pub target: String,
    pub result: String,
    pub refused: bool,
}

/// What the screen shows, as the host sends it to its viewers.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Board {
    pub host: String,
    /// A line under the title, such as that the host shares no folders.
    pub note: Option<String>,
    pub last_heartbeat: Option<Timestamp>,
    /// Standing notices under the title: pending browser handoffs.
    pub notices: Vec<String>,
    /// What a starting or stopping host is doing, shown in place of the
    /// heartbeat until the next one.
    pub step: Option<String>,
    pub rows: VecDeque<Row>,
}

/// The host's side: the board, and whoever watches it changing.
#[derive(Clone)]
pub struct Dashboard {
    board: watch::Sender<Board>,
}

impl Dashboard {
    pub fn new(host: &str, note: Option<&str>) -> Dashboard {
        let board = Board {
            host: host.to_string(),
            note: note.map(str::to_string),
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

    pub fn heartbeat(&self) {
        self.board.send_modify(|board| {
            board.last_heartbeat = Some(Timestamp::now());
            board.step = None;
        });
    }

    /// Shows what the host is doing now, while it starts or stops.
    pub fn step(&self, step: &str) {
        self.board
            .send_modify(|board| board.step = Some(step.to_string()));
    }

    /// Replaces the standing notices.
    pub fn notices(&self, lines: Vec<String>) {
        self.board.send_modify(|board| board.notices = lines);
    }

    pub fn call(&self, row: Row) {
        self.board.send_modify(|board| {
            board.rows.push_front(row);
            board.rows.truncate(KEEP);
        });
    }
}

/// Draws the boards `boards` yields on this terminal's second screen,
/// redrawing each second for the clock, until `boards` ends (the host
/// stopped) or `until` does. Gives the terminal back as it was. `footer`
/// says what Ctrl-C does here.
pub async fn show(
    boards: tokio::sync::mpsc::Receiver<Board>,
    until: impl std::future::Future<Output = ()>,
    footer: &str,
) {
    show_on(Box::new(std::io::stdout()), boards, until, footer).await;
}

/// `show` on `out`. Only a thread of its own writes there, so a terminal
/// that takes no output (paused, or a stalled ssh session) never holds up
/// Ctrl-C or what follows it: the terminal is then left as it is, after
/// `GIVE_BACK_WITHIN`.
async fn show_on(
    mut out: Box<dyn Write + Send>,
    mut boards: tokio::sync::mpsc::Receiver<Board>,
    until: impl std::future::Future<Output = ()>,
    footer: &str,
) {
    let board = Arc::new(Mutex::new(Board::default()));
    let (redraw, asked) = mpsc::sync_channel::<()>(1);
    let (given, back) = tokio::sync::oneshot::channel();
    let drawn = board.clone();
    let footer = footer.to_string();
    std::thread::spawn(move || {
        let _ = write!(out, "{}{}", term::ALTERNATE_SCREEN, term::HIDE_CURSOR);
        // Each second, or sooner when asked, until the view is closed.
        loop {
            let (width, height) = term::size();
            let text = drawn
                .lock()
                .expect("board lock")
                .screen(width, height, &footer);
            let _ = write!(out, "{}{text}", term::CLEAR);
            let _ = out.flush();
            if let Err(RecvTimeoutError::Disconnected) = asked.recv_timeout(Duration::from_secs(1))
            {
                break;
            }
        }
        let _ = write!(out, "{}{}", term::SHOW_CURSOR, term::MAIN_SCREEN);
        let _ = out.flush();
        let _ = given.send(());
    });
    tokio::pin!(until);
    loop {
        tokio::select! {
            next = boards.recv() => match next {
                Some(next) => *board.lock().expect("board lock") = next,
                None => break,
            },
            () = &mut until => break,
        }
        let _ = redraw.try_send(());
    }
    drop(redraw);
    let _ = tokio::time::timeout(GIVE_BACK_WITHIN, back).await;
}

impl Board {
    /// The whole screen as text, `width` columns by at most `height` rows.
    fn screen(&self, width: usize, height: usize, footer: &str) -> String {
        let width = width.max(60);
        let now = Timestamp::now();
        let per_minute = self
            .rows
            .iter()
            .take_while(|row| now.as_second() - row.at.as_second() < 60)
            .count();
        let heartbeat = self.last_heartbeat.map_or("not yet".to_string(), clock);

        let mut lines = Vec::new();
        let title = format!(" latch serve · {}", term::one_line(&self.host));
        let status = match &self.step {
            Some(step) => format!("{step} "),
            None => format!("last heartbeat {heartbeat} · {per_minute} calls/min "),
        };
        let gap = width.saturating_sub(title.chars().count() + status.chars().count());
        lines.push(format!(
            "{}{}{}",
            paint::bold(&title, VOLT),
            " ".repeat(gap),
            paint::fg(&status, DIM)
        ));
        lines.push(paint::fg(&"─".repeat(width), FAINT));
        if let Some(note) = &self.note {
            lines.push(paint::fg(&format!(" {}", term::fit(note, width - 1)), DIM));
        }
        for notice in &self.notices {
            lines.push(paint::bold(
                &format!(" {}", term::fit(notice, width - 1)),
                RED,
            ));
        }
        if !self.notices.is_empty() {
            lines.push(paint::fg(&"─".repeat(width), FAINT));
        }

        // Fixed columns for time, agent, operation and result; the target
        // takes what is left.
        let target_width = width.saturating_sub(1 + TIME + AGENT + OPERATION + RESULT + 4 * 2);
        lines.push(paint::fg(
            &format!(
                " {}  {}  {}  {}  {}",
                term::fit("TIME", TIME),
                term::fit("AGENT", AGENT),
                term::fit("OPERATION", OPERATION),
                term::fit("TARGET", target_width),
                "RESULT"
            ),
            DIM,
        ));
        let room = height.saturating_sub(lines.len() + 2);
        for row in self.rows.iter().take(room) {
            let result_color = if row.refused { RED } else { INK };
            lines.push(format!(
                " {}  {}  {}  {}  {}",
                paint::fg(&clock(row.at), DIM),
                paint::fg(&term::fit(&row.caller, AGENT), INK),
                paint::fg(&term::fit(&row.op.replace('_', " "), OPERATION), VOLT),
                term::fit(&row.target, target_width),
                paint::fg(&term::fit(&row.result, RESULT), result_color),
            ));
        }
        if self.rows.is_empty() {
            lines.push(paint::fg(" waiting for calls…", DIM));
        }
        while lines.len() < height.saturating_sub(2) {
            lines.push(String::new());
        }
        lines.push(paint::fg(
            " Commands hide credential-looking values, best effort: some can slip through",
            DIM,
        ));
        lines.push(paint::fg(
            &format!(" {footer} · `latch logs` for the log"),
            DIM,
        ));
        lines.join("\n")
    }
}

/// `14:02:07` in the host's local time.
fn clock(at: Timestamp) -> String {
    at.to_zoned(TimeZone::system())
        .strftime("%H:%M:%S")
        .to_string()
}

/// The target and the result of an operation, from its audit details: what
/// was run or moved, and how it ended.
pub fn describe(op: &str, args: &Value, outcome: &str, took: Duration) -> (String, String) {
    let took = human_duration(took);
    match op {
        "exec" => {
            let shown = crate::redact::shown("exec", args);
            let words: Vec<String> =
                serde_json::from_value(shown["argv"].clone()).unwrap_or_default();
            let ended = if args["timed_out"] == true {
                "timed out".to_string()
            } else if let Some(code) = args["exit_code"].as_i64() {
                format!("exit {code}")
            } else if let Some(signal) = args["signal"].as_str() {
                signal.to_string()
            } else {
                outcome.to_string()
            };
            // A program can print diagnostics and still succeed; say so
            // without calling it a failure.
            let stderr = if args["stderr_bytes"].as_u64().unwrap_or(0) > 0 {
                " · stderr"
            } else {
                ""
            };
            (words.join(" "), format!("{ended}{stderr} · {took}"))
        }
        "file_get" | "file_put" => {
            let path = args["path"].as_str().unwrap_or_default().to_string();
            match args["size"].as_u64() {
                Some(size) if outcome == "ok" => (path, format!("{} · {took}", human_size(size))),
                _ => (path, outcome.to_string()),
            }
        }
        // The action and how many rows came back; never the rows.
        "messages" | "contacts" => {
            let action = args["action"].as_str().unwrap_or(op).to_string();
            match args["result"]["count"].as_u64() {
                Some(count) if outcome == "ok" => (action, format!("{count} rows · {took}")),
                _ => (action, outcome.to_string()),
            }
        }
        "browser" => {
            let command = args["command"].as_str().unwrap_or_default();
            let action = args["action"]
                .as_str()
                .map(|a| format!(" {a}"))
                .unwrap_or_default();
            let page = format!(
                "{}{}",
                args["origin"].as_str().unwrap_or_default(),
                args["path"].as_str().unwrap_or_default()
            );
            // The session's id hash first, so a narrow screen keeps it: an
            // owner device closes a session by it.
            let session = args["session"]
                .as_str()
                .map(|hash| format!("{hash} "))
                .unwrap_or_default();
            (
                format!("{session}{command}{action} {page}")
                    .trim()
                    .to_string(),
                format!("{outcome} · {took}"),
            )
        }
        _ => (String::new(), outcome.to_string()),
    }
}

fn human_size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KB", bytes as f64 / 1024.0),
        1_048_576..1_073_741_824 => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
        _ => format!("{:.1} GB", bytes as f64 / 1_073_741_824.0),
    }
}

fn human_duration(took: Duration) -> String {
    let ms = took.as_millis();
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", took.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A terminal that takes no output: every write waits forever.
    struct Stuck(Mutex<mpsc::Receiver<()>>);

    impl Write for Stuck {
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
        let (_never, wait) = mpsc::channel();
        let (boards, receiver) = tokio::sync::mpsc::channel(4);
        let feed = tokio::spawn(async move {
            for _ in 0..100 {
                if boards.send(Board::default()).await.is_err() {
                    return;
                }
            }
            // Then Ctrl-C, with the drawing thread stuck in its first write.
            std::future::pending::<()>().await;
        });
        let started = std::time::Instant::now();
        let until = tokio::time::sleep(Duration::from_millis(200));
        show_on(Box::new(Stuck(Mutex::new(wait))), receiver, until, "").await;
        assert!(started.elapsed() < GIVE_BACK_WITHIN + Duration::from_secs(2));
        feed.abort();
    }

    #[test]
    fn an_exec_whose_argv_is_not_a_list_of_words_shows_no_command() {
        let (target, result) = describe(
            "exec",
            &json!({"argv": 1, "exit_code": 0}),
            "ok",
            Duration::ZERO,
        );
        assert_eq!(target, "");
        assert!(result.starts_with("exit 0"), "{result}");
    }

    #[test]
    fn a_multiline_argv_stays_on_one_row() {
        // `latch exec -- sh -c $'echo a\necho b'`, plus an escape sequence.
        let argv = json!({"argv": ["sh", "-c", "echo a\necho b\u{1b}[2J"], "exit_code": 0, "timed_out": false});
        let (target, result) = describe("exec", &argv, "ok", Duration::from_millis(5));
        assert_eq!(
            target, "sh -c echo a\necho b\u{1b}[2J",
            "describe keeps the argv as run"
        );
        let mut board = Board {
            host: "host".to_string(),
            note: None,
            last_heartbeat: None,
            notices: Vec::new(),
            step: None,
            rows: VecDeque::from([Row {
                at: Timestamp::now(),
                caller: "agent".to_string(),
                op: "exec".to_string(),
                target,
                result,
                refused: false,
            }]),
        };
        let screen = board.screen(100, 10, "Ctrl-C stops the host");
        assert_eq!(screen.lines().count(), 10, "one line per screen row");
        assert!(
            !screen.contains('\u{1b}'),
            "color is off in tests, and the argv's escape is gone"
        );
        let row = screen.lines().nth(3).unwrap();
        assert!(row.contains("sh -c echo a⏎echo b [2J"), "{row}");

        // A pending handoff stands under the title, above the calls.
        board.notices = vec!["handoff pending: session dkcfhoeamlpb (captcha) for 5f1e…".into()];
        let screen = board.screen(100, 10, "Ctrl-C stops the host");
        assert!(
            screen
                .lines()
                .nth(2)
                .unwrap()
                .contains("handoff pending: session dkcfhoeamlpb")
        );
        assert!(screen.contains("sh -c echo a"));
    }

    #[test]
    fn a_host_sharing_no_folders_says_so_under_its_title() {
        let board = Board {
            host: "host".to_string(),
            note: Some(crate::serve::SHARING_NONE.to_string()),
            last_heartbeat: None,
            notices: Vec::new(),
            step: None,
            rows: VecDeque::new(),
        };
        let screen = board.screen(100, 10, "Ctrl-C stops the host");
        let note = screen.lines().nth(2).unwrap();
        assert!(
            note.contains("Sharing no folders — browser and Google only")
                && note.contains("latch shares set ~/code"),
            "{screen}"
        );
        // On a narrower terminal it is cut to the width rather than wrapped,
        // so the screen still fits.
        let screen = board.screen(80, 10, "Ctrl-C stops the host");
        assert_eq!(screen.lines().count(), 10, "{screen}");
        let note = screen.lines().nth(2).unwrap();
        assert!(note.starts_with(" Sharing no folders"), "{note}");
        assert!(note.chars().count() <= 80, "{note}");
    }

    #[test]
    fn rows_say_what_ran_and_how_it_ended() {
        let exec =
            json!({"argv": ["uname", "-a"], "exit_code": 0, "signal": null, "timed_out": false});
        let took = Duration::from_millis(14);
        assert_eq!(
            describe("exec", &exec, "ok", took),
            ("uname -a".into(), "exit 0 · 14 ms".into())
        );
        let warned = json!({"argv": ["swift", "--version"], "exit_code": 0, "stderr_bytes": 52});
        assert_eq!(
            describe("exec", &warned, "ok", took).1,
            "exit 0 · stderr · 14 ms"
        );
        let file = json!({"path": "big.bin", "size": 209715200, "sha256": "…"});
        let took = Duration::from_millis(5300);
        assert_eq!(
            describe("file_put", &file, "ok", took),
            ("big.bin".into(), "200.0 MB · 5.3 s".into())
        );
        let refused = json!({"path": "/etc/hosts"});
        assert_eq!(
            describe("file_get", &refused, "sandbox_denied", took).1,
            "sandbox_denied"
        );
        let browser = json!({"command": "act", "session": "dkcfhoeamlpb", "origin": "https://shop.example", "path": "/cart"});
        assert_eq!(
            describe("browser", &browser, "ok", took).0,
            "dkcfhoeamlpb act https://shop.example/cart"
        );
    }
}
