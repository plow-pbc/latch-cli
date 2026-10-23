//! A bare `latch` at a terminal: who this machine is signed in as, whether
//! it hosts, the account's hosts, where commands go, its connectors and its
//! update, with the Plow logo animating beside it (`logo.rs`) and one next
//! step under it. What this machine knows draws at once; the account and
//! its hosts fill in as Plow answers, which it has two seconds of wall time
//! to do. Read-only: it registers, starts and saves nothing.

use std::io::Write;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use jiff::Zoned;

use crate::failure::{Failure, code};
use crate::lifecycle::Phase;
use crate::logo;
use crate::paint::{self, AMBER, DIM, Rgb, VOLT};
use crate::state::StateDir;
use crate::status::{self, Directory, LocalStatus};
use crate::term;

/// How long Plow has to answer, from when the overview starts asking (the
/// credential's read included).
const PLOW_WITHIN: Duration = Duration::from_secs(2);
/// The width of a row's label: "Connectors" and a space.
const LABEL: usize = 11;
/// The facts' width beside the mark: 80 columns in all.
const BESIDE: usize = 46;
/// Between the mark and the facts.
const GAP: usize = 2;

/// Piped or redirected, there is no one to show it to.
pub fn no_terminal() -> Failure {
    let message = "a bare `latch` shows an overview for a person at a terminal".to_string();
    let mut failure = Failure::new(code::no_terminal, message);
    failure.next = Some("latch status".into());
    failure
}

pub fn run(state: &StateDir) -> Result<(), Failure> {
    let color = !term::no_color();
    paint::set_color(color);
    // Plow is asked first, on a thread of its own, so nothing here waits
    // for it; it is left behind if it never answers.
    let deadline = Instant::now() + PLOW_WITHIN;
    let config = state.load_config()?;
    let (answered, answers) = mpsc::channel();
    if config.is_some() {
        let state = StateDir {
            path: state.path.clone(),
        };
        std::thread::spawn(move || {
            let asked = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|err| Failure::new(code::plow_error, err.to_string()))
                .and_then(|runtime| runtime.block_on(status::directory(&state)));
            let _ = answered.send(asked);
        });
    }
    let asked = std::env::var("LATCH_HOST").ok();
    let mut facts = Facts {
        here_default: config
            .as_ref()
            .filter(|config| crate::hosts::this_machine_chosen(state, config, asked.as_deref()))
            .map(|config| config.peer.name.clone()),
        local: status::local(state)?,
        suggested: crate::login::default_name(),
        api: config
            .as_ref()
            .map(|config| config.api_url.clone())
            .or_else(|| {
                crate::cli::BUILD_API_URL
                    .or(crate::cli::default_api_url(crate::cli::BUILD_CHANNEL))
                    .map(str::to_string)
            }),
        serve: crate::setup::start_args(state),
        plow: if config.is_some() {
            Asked::Waiting
        } else {
            Asked::SignedOut
        },
        home: std::env::var_os("HOME").map(Into::into),
        now: Zoned::now(),
    };

    let (columns, rows) = term::size();
    let mut screen = Screen::new(columns, rows, color, &facts);
    let mut frames = screen
        .animate
        .then(logo::frames)
        .unwrap_or_default()
        .into_iter();
    let mut mark = frames.next();
    let mut next_frame = Instant::now();
    let mut out = std::io::stdout();
    loop {
        let pixels = mark
            .as_ref()
            .map_or_else(logo::finished, |(pixels, _)| *pixels);
        screen.draw(&mut out, &pixels, &facts);
        if let Some((_, ms)) = &mark {
            next_frame += Duration::from_millis(*ms);
        }
        // Until the next frame, or Plow's answer, or its deadline.
        let waiting = matches!(facts.plow, Asked::Waiting);
        let until = match (mark.is_some() && frames.len() > 0, waiting) {
            (true, true) => next_frame.min(deadline),
            (true, false) => next_frame,
            (false, true) => deadline,
            (false, false) => break,
        };
        if waiting {
            match answers.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(answer) => facts.plow = Asked::from(answer),
                Err(RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                    facts.plow = Asked::NoAnswer
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => facts.plow = Asked::NoAnswer,
            }
        } else {
            std::thread::sleep(until.saturating_duration_since(Instant::now()));
        }
        if Instant::now() >= next_frame && frames.len() > 0 {
            mark = frames.next();
        }
    }
    screen.finish(&mut out, &facts);
    Ok(())
}

/// What the overview knows, as it learns it.
struct Facts {
    local: LocalStatus,
    /// The name `latch login` would give this machine.
    suggested: Option<String>,
    /// The Plow this machine is signed in to, or would sign in to.
    api: Option<String>,
    /// Where a command without `--host` goes when that needs no Plow:
    /// this machine, named or having served.
    here_default: Option<String>,
    /// How `latch serve` starts the host again here.
    serve: Vec<&'static str>,
    plow: Asked,
    home: Option<std::path::PathBuf>,
    now: Zoned,
}

/// Plow's answer, as far as it got.
enum Asked {
    /// Not logged in: nothing to ask.
    SignedOut,
    Waiting,
    Answered(Directory),
    /// Plow, or reading the login, said no: what went wrong.
    Failed(String),
    NoAnswer,
}

impl From<Result<Directory, Failure>> for Asked {
    fn from(answer: Result<Directory, Failure>) -> Asked {
        match answer {
            Ok(directory) => Asked::Answered(directory),
            Err(failure) => Asked::Failed(failure.message),
        }
    }
}

/// A piece of a row and how it looks.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Tone {
    Plain,
    Bold,
    Dim,
    Good,
    Warn,
}

type Line = Vec<(String, Tone)>;

fn plain(text: impl Into<String>) -> (String, Tone) {
    (text.into(), Tone::Plain)
}

fn toned(text: impl Into<String>, tone: Tone) -> (String, Tone) {
    (text.into(), tone)
}

/// "this Mac" on a Mac.
fn here() -> &'static str {
    if cfg!(target_os = "macos") {
        "Mac"
    } else {
        "machine"
    }
}

impl Facts {
    /// The facts, a row each; `width` is what fits beside the mark.
    fn rows(&self, width: usize) -> Vec<Line> {
        let mut rows = vec![self.header(), vec![]];
        let logged_in = self.local.login.as_ref();
        let account = match (&self.plow, logged_in) {
            (_, None) => plain("not signed in"),
            (Asked::Answered(directory), _) => plain(
                directory
                    .account
                    .display_name
                    .clone()
                    .unwrap_or_else(|| directory.account.owner_uid.clone()),
            ),
            (asked, _) => self.not_answered(asked, "Plow didn't answer"),
        };
        rows.push(labeled("Account", vec![account]));
        if let Some(login) = logged_in {
            rows.push(labeled(
                "Login",
                match login.kind.as_str() {
                    "device" => vec![plain("owner "), toned("(device)", Tone::Dim)],
                    kind => vec![plain(kind)],
                },
            ));
        }
        rows.push(vec![]);
        rows.extend(self.hosts());
        if logged_in.is_some() {
            rows.push(labeled("Commands", vec![self.commands()]));
        }
        if self
            .local
            .this_machine
            .hosting
            .deny_exec
            .as_ref()
            .is_some_and(|deny| deny.every_agent())
        {
            rows.push(labeled(
                "Exec",
                vec![plain("agents can't run programs here")],
            ));
        }
        rows.extend(self.connectors(width));
        rows.push(labeled("Update", self.update()));
        rows.push(labeled("Folder", vec![plain(self.folder())]));
        rows
    }

    fn header(&self) -> Line {
        let build = crate::cli::BUILD_NUMBER.filter(|number| !number.is_empty());
        let mut line = vec![
            toned("latch", Tone::Bold),
            plain(format!(
                " {} ({})",
                env!("CARGO_PKG_VERSION"),
                build.unwrap_or("local build")
            )),
        ];
        if let Some(api) = &self.api {
            let host = api.split("://").last().unwrap_or(api).trim_end_matches('/');
            line.extend([toned(" · ", Tone::Dim), plain(host)]);
        }
        line
    }

    /// A Plow row that has no answer (yet).
    fn not_answered(&self, asked: &Asked, no_answer: &str) -> (String, Tone) {
        match asked {
            Asked::Waiting => toned("…", Tone::Warn),
            Asked::Failed(message) => toned(term::one_line(message), Tone::Warn),
            _ => toned(no_answer, Tone::Warn),
        }
    }

    /// This machine first, as it knows itself and then as Plow lists it,
    /// then the account's other hosts.
    fn hosts(&self) -> Vec<Line> {
        let machine = &self.local.this_machine;
        let mut lines = vec![];
        match &machine.name {
            Some(name) => lines.push(vec![
                toned(format!("this {}: ", here()), Tone::Dim),
                plain(term::one_line(name)),
            ]),
            None => {
                lines.push(vec![
                    toned(format!("this {},", here()), Tone::Dim),
                    plain(" not signed in"),
                ]);
                if let Some(name) = &self.suggested {
                    lines.push(vec![
                        toned("suggested name: ", Tone::Dim),
                        plain(term::one_line(name)),
                    ]);
                }
            }
        }
        lines.extend(self.hosting());
        match &self.plow {
            Asked::SignedOut => lines.push(vec![toned("sign in to see the others", Tone::Dim)]),
            Asked::Answered(directory) => {
                let own = directory.hosts.iter().find(|host| host.this_machine);
                lines.push(match own {
                    Some(host) => {
                        let mut line = vec![toned("Plow: ", Tone::Dim)];
                        line.extend(self.seen(host));
                        line
                    }
                    None => vec![toned("Plow: ", Tone::Dim), plain("not listed as a host")],
                });
                let others: Vec<_> = directory
                    .hosts
                    .iter()
                    .filter(|host| !host.this_machine)
                    .collect();
                for host in others.iter().take(3) {
                    let mut line = vec![plain(term::one_line(&host.name)), toned(": ", Tone::Dim)];
                    line.extend(self.seen(host));
                    lines.push(line);
                }
                if others.len() > 3 {
                    lines.push(vec![toned(
                        format!("+{} more: latch status", others.len() - 3),
                        Tone::Dim,
                    )]);
                }
            }
            asked => lines.push(vec![
                toned("Plow: ", Tone::Dim),
                self.not_answered(asked, "didn't answer"),
            ]),
        }
        let mut lines = lines.into_iter();
        let first = labeled("Hosts", lines.next().unwrap_or_default());
        std::iter::once(first).chain(lines.map(indented)).collect()
    }

    /// What latch knows of its own host here.
    fn hosting(&self) -> Vec<Line> {
        let hosting = &self.local.this_machine.hosting;
        // Only ever another latch version's host.
        if hosting.blocked.is_some() {
            let pid = hosting
                .pid
                .map(|pid| format!(" (pid {pid})"))
                .unwrap_or_default();
            return vec![vec![toned(
                format!("blocked by another latch{pid}"),
                Tone::Warn,
            )]];
        }
        match hosting.state {
            Phase::Serving => {
                let how = match (hosting.managed, hosting.attached) {
                    (_, true) => "in a terminal",
                    (true, false) => "background",
                    (false, false) => "foreground",
                };
                vec![vec![
                    toned("serving", Tone::Good),
                    toned(format!(" ({how})"), Tone::Dim),
                ]]
            }
            Phase::Starting => vec![vec![plain("starting")]],
            Phase::Stopped => vec![vec![plain("stopped")]],
            Phase::Error => {
                let mut lines = vec![vec![toned("stopped after an error", Tone::Warn)]];
                if let Some(error) = &self.local.last_error {
                    lines.push(vec![toned(term::one_line(&error.message), Tone::Dim)]);
                }
                lines
            }
        }
    }

    /// Plow's words for a host: online, or not online and when it last
    /// heard from it.
    fn seen(&self, host: &status::Host) -> Line {
        match host.online {
            Some(true) => vec![toned("online", Tone::Good)],
            _ => {
                let last = host
                    .last_seen_at
                    .as_deref()
                    .and_then(|at| at.parse::<jiff::Timestamp>().ok())
                    .map(|at| format!(", last seen {}", self.when(at)))
                    .unwrap_or_default();
                vec![toned(format!("not online{last}"), Tone::Dim)]
            }
        }
    }

    /// A time of day today, else a date.
    fn when(&self, at: jiff::Timestamp) -> String {
        let at = at.to_zoned(self.now.time_zone().clone());
        if at.date() == self.now.date() {
            at.strftime("%-I:%M %p").to_string()
        } else {
            at.strftime("%b %-d").to_string()
        }
    }

    fn commands(&self) -> (String, Tone) {
        if let Some(name) = &self.here_default {
            return plain(format!("go to {}", term::one_line(name)));
        }
        let Asked::Answered(directory) = &self.plow else {
            return self.not_answered(&self.plow, "Plow didn't answer");
        };
        if let Some(name) = &directory.default_host {
            return plain(format!("go to {}", term::one_line(name)));
        }
        let names: Vec<String> = directory
            .hosts
            .iter()
            .map(|host| term::one_line(&host.name))
            .collect();
        match directory.default_host_reason.as_deref() {
            Some("host_required") => plain(format!("need --host: {}", names.join(", "))),
            Some("unknown_host") => toned(
                format!(
                    "LATCH_HOST names no host: {}",
                    term::one_line(&std::env::var("LATCH_HOST").unwrap_or_default())
                ),
                Tone::Warn,
            ),
            _ => plain("no hosts yet"),
        }
    }

    /// Each connector and its state, as many to a row as fit.
    fn connectors(&self, width: usize) -> Vec<Line> {
        let items: Vec<Line> = self
            .local
            .plugins
            .as_array()
            .into_iter()
            .flatten()
            .map(|plugin| {
                let name = crate::setup::title(plugin["name"].as_str().unwrap_or_default());
                match plugin["state"].as_str() {
                    Some("enabled") => vec![plain(format!("{name} on"))],
                    Some("needs_setup") => {
                        vec![plain(format!("{name} ")), toned("needs setup", Tone::Warn)]
                    }
                    _ => vec![plain(format!("{name} off"))],
                }
            })
            .collect();
        let room = width.saturating_sub(LABEL);
        let mut lines: Vec<Line> = vec![];
        for item in items {
            let fits = lines
                .last()
                .is_some_and(|line| visible(line) + 3 + visible(&item) <= room);
            match lines.last_mut() {
                Some(line) if fits => {
                    line.push(toned(" · ", Tone::Dim));
                    line.extend(item);
                }
                _ => lines.push(item),
            }
        }
        let mut lines = lines.into_iter();
        let first = labeled("Connectors", lines.next().unwrap_or_default());
        std::iter::once(first).chain(lines.map(indented)).collect()
    }

    fn update(&self) -> Line {
        let update = &self.local.update;
        match (update.available, update.build, update.checked_at) {
            (true, Some(build), _) => {
                vec![toned(format!("build {build} is available"), Tone::Warn)]
            }
            (_, _, Some(checked)) => vec![
                plain("up to date"),
                toned(format!(" (checked {})", self.when(checked)), Tone::Dim),
            ],
            _ => vec![plain("not checked")],
        }
    }

    /// The state folder, `~/…` under the home folder.
    fn folder(&self) -> String {
        let dir = &self.local.state_dir;
        let short = self
            .home
            .as_deref()
            .and_then(|home| dir.strip_prefix(home).ok())
            .map(|rest| Path::new("~").join(rest));
        term::one_line(&short.as_deref().unwrap_or(dir).display().to_string())
    }

    /// The one thing to do next, if there is one: a host that can't be
    /// used, no login, a connector needing setup, no host running, an update.
    fn next_step(&self) -> Option<Line> {
        let hosting = &self.local.this_machine.hosting;
        let run = |what: String, command: String| {
            vec![plain(format!("{what} Run: ")), toned(command, Tone::Good)]
        };
        if let Some(blocked) = &hosting.blocked {
            return Some(vec![plain(term::one_line(&blocked.message))]);
        }
        let Some(login) = &self.local.login else {
            let what = format!("This {} isn't signed in to Plow.", here());
            return Some(run(what, "latch setup".into()));
        };
        let owner = login.kind == "device";
        let needs_setup = self
            .local
            .plugins
            .as_array()
            .into_iter()
            .flatten()
            .find(|plugin| plugin["state"] == "needs_setup");
        if let (true, Some(plugin)) = (owner, needs_setup) {
            let name = crate::setup::title(plugin["name"].as_str().unwrap_or_default());
            let command = plugin["enable"].as_str().unwrap_or_default().to_string();
            return Some(run(format!("The {name} connector needs setup."), command));
        }
        if owner && matches!(hosting.state, Phase::Stopped | Phase::Error) && !hosting.running {
            let what = format!("Latch isn't running on this {}.", here());
            return Some(run(what, format!("latch {}", self.serve.join(" "))));
        }
        let update = &self.local.update;
        if let (true, Some(build)) = (update.available, update.build) {
            return Some(run(
                format!("Build {build} is available."),
                "latch update".into(),
            ));
        }
        None
    }

    /// What goes under the facts: the next step, and where to look further.
    fn closing(&self) -> Vec<Line> {
        let mut lines = vec![vec![]];
        lines.extend(self.next_step());
        lines.push(vec![toned(
            "As JSON: latch status. Live activity: latch dashboard.",
            Tone::Dim,
        )]);
        lines
    }
}

fn labeled(label: &str, rest: Line) -> Line {
    let mut line = vec![toned(format!("{label:<LABEL$}"), Tone::Dim)];
    line.extend(rest);
    line
}

fn indented(rest: Line) -> Line {
    let mut line = vec![plain(" ".repeat(LABEL))];
    line.extend(rest);
    line
}

fn visible(line: &Line) -> usize {
    line.iter().map(|(text, _)| text.chars().count()).sum()
}

/// `line` cut to `width` columns, never wrapped, in its colors.
fn painted(line: &Line, width: usize) -> String {
    let mut left = width;
    let mut out = String::new();
    for (text, tone) in line {
        if left == 0 {
            break;
        }
        let cut: String = text.chars().take(left).collect();
        left -= cut.chars().count();
        out.push_str(&match tone {
            Tone::Plain => cut,
            Tone::Bold => paint::bold(&cut, WHITE),
            Tone::Dim => paint::fg(&cut, DIM),
            Tone::Good => paint::fg(&cut, VOLT),
            Tone::Warn => paint::fg(&cut, AMBER),
        });
    }
    out
}

const WHITE: Rgb = Rgb(240, 240, 232);

/// Where the mark goes: beside the facts when 80 columns fit, else above.
#[derive(PartialEq, Debug)]
enum Layout {
    Beside,
    Above,
}

/// The terminal, and what has been drawn on it: each draw goes back up
/// over the last and draws again, so it must fit on the screen.
struct Screen {
    columns: usize,
    layout: Layout,
    /// The mark moves; without color, or without room to redraw it, it is
    /// drawn finished, once.
    animate: bool,
    /// Lines drawn once, above what is drawn again (the still mark, when it
    /// sits above the facts).
    fixed: bool,
    /// Draws nothing until the end: not even the facts fit to redraw.
    at_end_only: bool,
    drawn: usize,
}

/// The most rows Plow's answer can add to the facts: other hosts and the
/// line saying how many more.
const PLOW_ROWS: usize = 4;

impl Screen {
    fn new(columns: usize, rows: usize, color: bool, facts: &Facts) -> Screen {
        let tall = |layout: &Layout| {
            let width = facts_width(columns, layout);
            let facts = facts.rows(width).len() + PLOW_ROWS;
            let block = match layout {
                Layout::Beside => facts.max(logo::ROWS),
                Layout::Above => logo::ROWS + 1 + facts,
            };
            (block + facts_closing(), facts + facts_closing())
        };
        let beside = columns >= logo::COLUMNS + GAP + BESIDE && tall(&Layout::Beside).0 < rows;
        let layout = if beside {
            Layout::Beside
        } else {
            Layout::Above
        };
        let (whole, below) = tall(&layout);
        let animate = color && whole < rows;
        Screen {
            columns,
            fixed: layout == Layout::Above && !animate,
            at_end_only: whole >= rows && below >= rows,
            layout,
            animate,
            drawn: 0,
        }
    }

    /// The lines of one frame: the mark as `pixels` and the facts so far.
    fn lines(&self, pixels: &logo::Pixels, facts: &Facts) -> Vec<String> {
        let width = facts_width(self.columns, &self.layout);
        let rows: Vec<String> = facts
            .rows(width)
            .iter()
            .map(|row| painted(row, width))
            .collect();
        let closing = facts
            .closing()
            .into_iter()
            .map(|row| painted(&row, self.columns));
        let mark = logo::lines(pixels);
        let mut lines = vec![];
        match self.layout {
            Layout::Beside => {
                let blank = " ".repeat(logo::COLUMNS);
                for i in 0..rows.len().max(mark.len()) {
                    let left = mark.get(i).unwrap_or(&blank);
                    let right = rows.get(i).map(String::as_str).unwrap_or_default();
                    lines.push(
                        format!("{left}{}{right}", " ".repeat(GAP))
                            .trim_end()
                            .to_string(),
                    );
                }
            }
            Layout::Above => {
                if !self.fixed {
                    lines.extend(mark);
                    lines.push(String::new());
                }
                lines.extend(rows);
            }
        }
        lines.extend(closing);
        lines
    }

    fn draw(&mut self, out: &mut impl Write, pixels: &logo::Pixels, facts: &Facts) {
        if self.at_end_only {
            return;
        }
        if self.fixed && self.drawn == 0 {
            let mut text = logo::lines(&logo::finished()).join("\n");
            text.push_str("\n\n");
            let _ = out.write_all(text.as_bytes());
        }
        let mut text = String::from(term::SYNC_START);
        if self.drawn > 0 {
            text.push_str(&term::up(self.drawn));
        }
        let lines = self.lines(pixels, facts);
        for line in &lines {
            text.push_str(line);
            text.push_str(term::CLEAR_LINE_END);
            text.push('\n');
        }
        text.push_str(term::CLEAR_BELOW);
        text.push_str(term::SYNC_END);
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
        self.drawn = lines.len();
    }

    /// The last frame: the mark at rest and what Plow said, or didn't.
    fn finish(&mut self, out: &mut impl Write, facts: &Facts) {
        if self.at_end_only {
            self.at_end_only = false;
            self.fixed = self.layout == Layout::Above;
        }
        self.draw(out, &logo::finished(), facts);
    }
}

fn facts_width(columns: usize, layout: &Layout) -> usize {
    match layout {
        Layout::Beside => columns - logo::COLUMNS - GAP,
        Layout::Above => columns,
    }
}

/// A blank line, the next step and the pointer to `latch status`.
fn facts_closing() -> usize {
    3
}

#[cfg(test)]
mod tests;
