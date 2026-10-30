//! A bare `latch` at a terminal: who this machine is signed in as, whether
//! it hosts, the account's hosts and the plugins each has on, where
//! commands go, its connectors and its update, with the Plow logo animating
//! beside it (`logo.rs`) and one next step under it. What this machine
//! knows from its files draws at once; what can wait (Plow's answer, and
//! the host's state, which asks the service manager) is asked on threads of
//! its own and fills in as it arrives, within two seconds of wall time.
//! Once Plow has answered, each other host is asked for its plugins, and
//! has two seconds more. Read-only: it registers, starts and saves nothing.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use jiff::Zoned;
use serde_json::Value;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::failure::{Failure, code};
use crate::lifecycle::Phase;
use crate::logo;
use crate::paint::{self, AMBER, DIM, Rgb, VOLT};
use crate::state::StateDir;
use crate::status::{self, Directory, Hosted, Login, Plugin};
use crate::term;

/// How long what can wait has to arrive, from when the overview starts
/// asking (Plow's credential's read included).
const WITHIN: Duration = Duration::from_secs(2);
/// Past the hosts' own time to ask, for their last answers to arrive here.
const SLACK: Duration = Duration::from_millis(250);
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

/// What arrives from the threads that may wait.
enum Arrived {
    Plow(Result<Directory, Failure>),
    /// `None`: the service manager didn't answer in time.
    Host(Option<Hosted>),
    /// A host's plugins, by its place in Plow's answer, or why it has none.
    Plugins(usize, Result<Vec<Plugin>, String>),
}

pub fn run(state: &StateDir) -> Result<(), Failure> {
    let color = !term::no_color();
    paint::set_color(color);
    // Each is asked on a thread of its own, so nothing here waits for it.
    // Plow's is left behind if it never answers; the host's ends by the
    // deadline, its service manager's command killed and reaped.
    state.check_format()?;
    let mut deadline = Instant::now() + WITHIN;
    let config = state.load_config()?;
    let (arrived, arrivals) = mpsc::channel();
    let path = state.path.clone();
    let host = arrived.clone();
    let hosting = std::thread::spawn(move || {
        let _ = host.send(Arrived::Host(status::hosted(
            &StateDir { path },
            Some(deadline),
        )));
    });
    if config.is_some() {
        let path = state.path.clone();
        std::thread::spawn(move || {
            let state = StateDir { path };
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    let failure = Failure::new(code::plow_error, err.to_string());
                    let _ = arrived.send(Arrived::Plow(Err(failure)));
                    return;
                }
            };
            let directory = runtime.block_on(status::directory(&state));
            let peers = directory
                .as_ref()
                .ok()
                .map(|directory| directory.peers.clone());
            let _ = arrived.send(Arrived::Plow(directory));
            if let Some(peers) = peers {
                runtime.block_on(status::ask_plugins(&state, &peers, |index, answer| {
                    let _ = arrived.send(Arrived::Plugins(index, answer));
                }));
            }
        });
    }
    let named = crate::hosts::named(state, std::env::var("LATCH_HOST").ok().as_deref())?;
    let mut facts = Facts {
        login: config.as_ref().map(|config| Login {
            kind: config.peer.kind.clone(),
        }),
        name: config.as_ref().map(|config| config.peer.name.clone()),
        here_default: config
            .as_ref()
            .filter(|_| !matches!(named, crate::hosts::Named::NoDefault))
            .filter(|config| crate::hosts::this_machine_chosen(state, config, named.host()))
            .map(|config| config.peer.name.clone()),
        plugins: crate::plugins::list_saved(state)?["plugins"].take(),
        update: crate::update::status(&state.path),
        state_dir: state.path.canonicalize().unwrap_or(state.path.clone()),
        suggested: crate::login::default_name(),
        api: match &config {
            Some(config) => Some(config.api_url.clone()),
            // The Plow a login would use.
            None => crate::login::api_url(
                None,
                crate::credential::env,
                state.load_plow_url()?,
                crate::cli::BUILD_CHANNEL,
            ),
        },
        state_flag: (state.path != StateDir::new(crate::cli::default_state_dir())?.path).then(
            || {
                format!(
                    " --state-dir {}",
                    crate::login::shell_quoted(&state.path.display().to_string())
                )
            },
        ),
        serve: crate::setup::start_args(state),
        host: Pending::Waiting,
        plow: config.is_some().then_some(Pending::Waiting),
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
        // Until the next frame, or the next arrival, or the deadline.
        let waiting = facts.waiting();
        let until = match (mark.is_some() && frames.len() > 0, waiting) {
            (true, true) => next_frame.min(deadline),
            (true, false) => next_frame,
            (false, true) => deadline,
            (false, false) => break,
        };
        if waiting {
            match arrivals.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(Arrived::Plow(answer)) => {
                    // The hosts' plugins have their own time from here.
                    if answer.is_ok() {
                        deadline = deadline.max(Instant::now() + status::PLUGINS_WITHIN + SLACK);
                    }
                    facts.plow = Some(Pending::from(answer));
                }
                Ok(Arrived::Plugins(index, answer)) => facts.plugins_of(index, answer),
                Ok(Arrived::Host(Some(hosted))) => facts.host = Pending::Got(hosted),
                Ok(Arrived::Host(None)) => facts.host = Pending::NoAnswer,
                Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(_) => facts.give_up(),
            }
        } else {
            std::thread::sleep(until.saturating_duration_since(Instant::now()));
        }
        if Instant::now() >= next_frame && frames.len() > 0 {
            mark = frames.next();
        }
    }
    screen.finish(&mut out, &facts);
    // Nothing it started outlives it.
    let _ = hosting.join();
    Ok(())
}

/// What the overview knows, as it learns it.
struct Facts {
    /// `None` before login.
    login: Option<Login>,
    /// The name `serve` registers this machine with, once logged in.
    name: Option<String>,
    /// As `latch status --local` says.
    plugins: Value,
    update: crate::update::Status,
    state_dir: PathBuf,
    /// The name `latch login` would give this machine.
    suggested: Option<String>,
    /// The Plow this machine is signed in to, or would sign in to.
    api: Option<String>,
    /// Where a command without `--host` goes when that needs no Plow:
    /// this machine, named or having served.
    here_default: Option<String>,
    /// How `latch serve` starts the host again here.
    serve: Vec<&'static str>,
    /// ` --state-dir DIR` when this is not the folder latch uses without
    /// one, for the commands it suggests.
    state_flag: Option<String>,
    host: Pending<Hosted>,
    /// `None` before login: nothing to ask.
    plow: Option<Pending<Directory>>,
    home: Option<PathBuf>,
    now: Zoned,
}

/// An answer, as far as it got.
enum Pending<T> {
    Waiting,
    Got(T),
    /// What went wrong, as the answer said.
    Failed(String),
    NoAnswer,
}

impl From<Result<Directory, Failure>> for Pending<Directory> {
    fn from(answer: Result<Directory, Failure>) -> Pending<Directory> {
        match answer {
            Ok(directory) => Pending::Got(directory),
            Err(failure) => Pending::Failed(failure.message),
        }
    }
}

impl Facts {
    fn waiting(&self) -> bool {
        matches!(self.host, Pending::Waiting)
            || matches!(self.plow, Some(Pending::Waiting))
            || self.listed().iter().any(plugins_waiting)
    }

    /// The deadline: whatever has not arrived never will.
    fn give_up(&mut self) {
        if let Pending::Waiting = self.host {
            self.host = Pending::NoAnswer;
        }
        if let Some(Pending::Waiting) = self.plow {
            self.plow = Some(Pending::NoAnswer);
        }
        if let Some(Pending::Got(directory)) = &mut self.plow {
            for host in directory
                .hosts
                .iter_mut()
                .filter(|host| plugins_waiting(host))
            {
                host.plugins_reason = Some(code::host_unreachable.name.to_string());
            }
        }
    }

    /// The hosts Plow listed, once it has.
    fn listed(&self) -> &[status::Host] {
        match &self.plow {
            Some(Pending::Got(directory)) => &directory.hosts,
            _ => &[],
        }
    }

    fn plugins_of(&mut self, index: usize, answer: Result<Vec<Plugin>, String>) {
        let Some(Pending::Got(directory)) = &mut self.plow else {
            return;
        };
        let Some(host) = directory.hosts.get_mut(index) else {
            return;
        };
        match answer {
            Ok(plugins) => host.plugins = Some(plugins),
            Err(reason) => host.plugins_reason = Some(reason),
        }
    }

    fn agent(&self) -> bool {
        self.login
            .as_ref()
            .is_some_and(|login| login.kind == "agent")
    }

    /// The host, once it is known.
    fn hosting(&self) -> Option<&Hosted> {
        match &self.host {
            Pending::Got(hosted) => Some(hosted),
            _ => None,
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
        let logged_in = self.login.as_ref();
        let account = match (&self.plow, logged_in) {
            (_, None) | (None, _) => plain("not logged in"),
            (Some(Pending::Got(directory)), _) => plain(
                directory
                    .account
                    .display_name
                    .as_deref()
                    .map(term::one_line)
                    .unwrap_or_else(|| {
                        let id: String = directory.account.owner_uid.chars().take(8).collect();
                        format!("no name set in Plow (ID {}…)", term::one_line(&id))
                    }),
            ),
            (Some(pending), _) => not_answered(pending, "Plow didn't answer", None),
        };
        rows.push(labeled("Account", vec![account]));
        if let Some(login) = logged_in {
            rows.push(labeled(
                "Login",
                match login.kind.as_str() {
                    "device" => vec![plain("account owner")],
                    kind => vec![plain(kind)],
                },
            ));
        }
        rows.push(vec![]);
        rows.extend(self.hosts());
        if let Some(plow) = &self.plow {
            rows.push(labeled("Commands", vec![self.commands(plow)]));
        }
        if self
            .hosting()
            .and_then(|hosted| hosted.hosting.deny_exec.as_ref())
            .is_some_and(|deny| deny.every_agent())
        {
            rows.push(labeled(
                "Exec",
                vec![plain("agents can't run programs here")],
            ));
        }
        // Only the owner's own login turns connectors on.
        if !self.agent() {
            rows.extend(self.connectors(width));
        }
        rows.push(labeled("Update", self.update()));
        let mut folder = folded(&self.folder(), width.saturating_sub(LABEL))
            .into_iter()
            .map(|piece| vec![plain(piece)]);
        rows.push(labeled("Folder", folder.next().unwrap_or_default()));
        rows.extend(folder.map(indented));
        rows
    }

    fn header(&self) -> Line {
        let build = crate::cli::BUILD_NUMBER.filter(|number| !number.is_empty());
        let mut line = vec![
            toned("latch", Tone::Bold),
            plain(format!(
                " {} ({})",
                env!("CARGO_PKG_VERSION"),
                build.map_or("local build".to_string(), |build| format!("build {build}"))
            )),
        ];
        if let Some(api) = &self.api {
            let host = api.split("://").last().unwrap_or(api).trim_end_matches('/');
            line.extend([toned(" · Plow: ", Tone::Dim), plain(host)]);
        }
        line
    }

    /// This machine first, as it knows itself and then as Plow lists it,
    /// then the account's other hosts.
    fn hosts(&self) -> Vec<Line> {
        let mut lines = vec![];
        match &self.name {
            Some(name) => lines.push(vec![
                toned(format!("this {}: ", here()), Tone::Dim),
                plain(term::one_line(name)),
            ]),
            None => {
                lines.push(vec![
                    toned(format!("this {},", here()), Tone::Dim),
                    plain(" not logged in"),
                ]);
                if let Some(name) = &self.suggested {
                    lines.push(vec![
                        toned("name at login: ", Tone::Dim),
                        plain(term::one_line(name)),
                    ]);
                }
            }
        }
        lines.extend(self.host_state());
        match &self.plow {
            None => lines.push(vec![toned("log in to see other hosts", Tone::Dim)]),
            Some(Pending::Got(directory)) => {
                let own = directory.hosts.iter().find(|host| host.this_machine);
                lines.push(match own {
                    Some(host) => {
                        let mut line = vec![toned("Plow says: ", Tone::Dim)];
                        line.extend(self.seen(host));
                        line
                    }
                    None => vec![
                        toned("Plow says: ", Tone::Dim),
                        plain("not listed as a host"),
                    ],
                });
                let others: Vec<_> = directory
                    .hosts
                    .iter()
                    .filter(|host| !host.this_machine)
                    .collect();
                for host in others.iter().take(3) {
                    let mut line = vec![plain(term::one_line(&host.name)), toned(": ", Tone::Dim)];
                    line.extend(self.plugins(host));
                    lines.push(line);
                }
                if others.len() > 3 {
                    lines.push(vec![toned(
                        format!("+{} more: latch status", others.len() - 3),
                        Tone::Dim,
                    )]);
                }
            }
            Some(Pending::Waiting) => lines.push(vec![
                toned("Plow says: ", Tone::Dim),
                toned("…", Tone::Warn),
            ]),
            Some(pending) => lines.push(vec![not_answered(
                pending,
                "Plow didn't answer",
                Some(PLOW_ERROR),
            )]),
        }
        let mut lines = lines.into_iter();
        let first = labeled("Hosts", lines.next().unwrap_or_default());
        std::iter::once(first).chain(lines.map(indented)).collect()
    }

    /// What latch knows of its own host here, once it knows.
    fn host_state(&self) -> Vec<Line> {
        let Pending::Got(Hosted {
            hosting,
            last_error,
        }) = &self.host
        else {
            return vec![vec![not_answered(
                &self.host,
                "state unknown: the check timed out",
                None,
            )]];
        };
        // Only ever another latch version's host.
        if hosting.blocked.is_some() {
            let pid = hosting
                .pid
                .map(|pid| format!(" (pid {pid})"))
                .unwrap_or_default();
            return vec![vec![toned(
                format!("blocked: other latch{pid}"),
                Tone::Warn,
            )]];
        }
        match hosting.state {
            Phase::Serving => {
                let how = match (hosting.managed, hosting.attached) {
                    (_, true) => "in a terminal",
                    (true, false) => "background",
                    (false, false) => "--foreground",
                };
                vec![vec![
                    toned("serving", Tone::Good),
                    toned(format!(" ({how})"), Tone::Dim),
                ]]
            }
            Phase::Starting => vec![vec![plain("starting")]],
            // An agent's login calls hosts: not hosting is no news.
            Phase::Stopped if self.agent() => vec![],
            Phase::Stopped => vec![vec![plain("stopped")]],
            Phase::Error => {
                let mut lines = vec![vec![toned("stopped after an error", Tone::Warn)]];
                if let Some(error) = last_error {
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

    /// Another host: the plugins it has on, once it says; else Plow's words
    /// for it, and why it didn't say, or that it is still being asked.
    fn plugins(&self, host: &status::Host) -> Line {
        let mut line = match &host.plugins {
            Some(plugins) if plugins.is_empty() => return vec![toned("no plugins on", Tone::Dim)],
            Some(plugins) => {
                let mut line = vec![];
                for plugin in plugins {
                    if !line.is_empty() {
                        line.push(toned(", ", Tone::Dim));
                    }
                    line.push(plain(crate::setup::title(&plugin.name)));
                    if plugin.state == "needs_setup" {
                        line.push(toned(" (needs setup)", Tone::Warn));
                    }
                }
                return line;
            }
            None => self.seen(host),
        };
        line.push(match host.plugins_reason.as_deref() {
            Some("host_offline") => return line,
            None if host.online == Some(false) => return line,
            Some(reason) => toned(format!(" · {}", why(reason)), Tone::Warn),
            None => toned(" · …", Tone::Warn),
        });
        line
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

    fn commands(&self, plow: &Pending<Directory>) -> (String, Tone) {
        if let Some(name) = &self.here_default {
            return plain(format!("default host: {}", term::one_line(name)));
        }
        let Pending::Got(directory) = plow else {
            return not_answered(plow, "Plow didn't answer", Some(PLOW_ERROR));
        };
        if let Some(name) = &directory.default_host {
            return plain(format!("default host: {}", term::one_line(name)));
        }
        let names: Vec<String> = directory
            .hosts
            .iter()
            .map(|host| term::one_line(&host.name))
            .collect();
        match directory.default_host_reason.as_deref() {
            Some("host_required" | "no_default_host") => {
                plain(format!("choose with --host: {}", names.join(", ")))
            }
            Some("unknown_host") => toned(
                format!(
                    "no matching host for LATCH_HOST={}",
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
        let update = &self.update;
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
        let dir = &self.state_dir;
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
        let hosting = self.hosting().map(|hosted| &hosted.hosting);
        if let Some(hosting) = hosting.filter(|hosting| hosting.blocked.is_some()) {
            return Some(blocked_step(hosting.pid));
        }
        let Some(login) = &self.login else {
            let what = format!("This {} isn't logged in to Plow.", here());
            return Some(run_step(what, self.latch("setup")));
        };
        let owner = login.kind == "device";
        let needs_setup = self
            .plugins
            .as_array()
            .into_iter()
            .flatten()
            .find(|plugin| plugin["state"] == "needs_setup");
        if let (true, Some(plugin)) = (owner, needs_setup) {
            let name = crate::setup::title(plugin["name"].as_str().unwrap_or_default());
            let enable = plugin["enable"].as_str().unwrap_or_default();
            let command = self.latch(enable.strip_prefix("latch ").unwrap_or(enable));
            return Some(run_step(
                format!("The {name} connector needs setup."),
                command,
            ));
        }
        let stopped = hosting.is_some_and(|hosting| {
            matches!(hosting.state, Phase::Stopped | Phase::Error) && !hosting.running
        });
        if owner && stopped {
            return Some(self.stopped_step());
        }
        let update = &self.update;
        if let (true, Some(build)) = (update.available, update.build) {
            return Some(run_step(
                format!("Build {build} is available."),
                self.latch("update"),
            ));
        }
        None
    }

    fn stopped_step(&self) -> Line {
        let what = format!("Latch isn't running on this {}.", here());
        run_step(what, self.latch(&self.serve.join(" ")))
    }

    /// `latch WORDS` for this state folder.
    fn latch(&self, words: &str) -> String {
        format!(
            "latch{} {words}",
            self.state_flag.as_deref().unwrap_or_default()
        )
    }

    /// What goes under the facts, `columns` wide: the next step, and where
    /// to look further.
    fn closing(&self, columns: usize) -> Vec<Line> {
        let mut lines = vec![vec![]];
        lines.extend(
            self.next_step()
                .map(|step| wrapped(step, columns))
                .into_iter()
                .flatten(),
        );
        lines.extend(wrapped(footer(), columns));
        lines
    }

    /// The most screen rows the closing can take at `columns`: with the step
    /// it has now, or with one the host's state can bring when it arrives.
    fn closing_rows(&self, columns: usize) -> usize {
        let steps = [
            self.next_step(),
            Some(blocked_step(Some(u32::MAX))),
            Some(self.stopped_step()),
        ];
        let step = steps
            .into_iter()
            .flatten()
            .map(|step| screen_rows(&wrapped(step, columns), columns))
            .max()
            .unwrap_or(0);
        1 + step + screen_rows(&wrapped(footer(), columns), columns)
    }
}

/// `what`, and the command that does it.
fn run_step(what: String, command: String) -> Line {
    vec![plain(format!("{what} Run: ")), toned(command, Tone::Good)]
}

fn blocked_step(pid: Option<u32>) -> Line {
    let mut step = vec![
        plain("Another latch version is running."),
        plain(" Ctrl-C in its terminal"),
    ];
    if let Some(pid) = pid {
        step.extend([plain(", or "), toned(format!("kill {pid}"), Tone::Good)]);
    }
    step.push(plain("."));
    step
}

fn footer() -> Line {
    vec![
        toned("For agents (JSON): latch status.", Tone::Dim),
        toned(" Live activity: latch dashboard.", Tone::Dim),
    ]
}

/// Why a host didn't say which plugins it has on, in words.
fn why(reason: &str) -> String {
    match reason {
        "host_unreachable" => "no answer".into(),
        "caller_denied" => "refused this login".into(),
        "protocol_mismatch" => "incompatible version".into(),
        // The host's own code.
        code => code.replace('_', " "),
    }
}

/// Not answered about its plugins yet, nor known not to answer.
fn plugins_waiting(host: &status::Host) -> bool {
    host.plugins.is_none() && host.plugins_reason.is_none()
}

/// What a row that needed Plow's answer says when asking failed.
const PLOW_ERROR: &str = "Plow error (see Account)";

/// A row that has no answer (yet). What went wrong is said once, on the Account row; the other rows that
/// needed the answer say `elsewhere`.
fn not_answered<T>(
    pending: &Pending<T>,
    no_answer: &str,
    elsewhere: Option<&str>,
) -> (String, Tone) {
    match (pending, elsewhere) {
        (Pending::Waiting, _) => toned("…", Tone::Warn),
        (Pending::Failed(message), None) => toned(term::one_line(message), Tone::Warn),
        (Pending::Failed(_), Some(elsewhere)) => toned(elsewhere, Tone::Warn),
        _ => toned(no_answer, Tone::Warn),
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

/// The columns `line` takes on a terminal.
fn visible(line: &Line) -> usize {
    line.iter().map(|(text, _)| text.width()).sum()
}

/// The screen rows `lines` take at `columns`, a line wider than that
/// wrapped by the terminal.
fn screen_rows(lines: &[Line], columns: usize) -> usize {
    lines
        .iter()
        .map(|line| visible(line).div_ceil(columns.max(1)).max(1))
        .sum()
}

/// `line` on as many lines as it needs at `width` columns, broken only
/// before a span that starts with a space or is a command. A span is never
/// broken, so a command copies whole: one wider than the screen is left to
/// the terminal to wrap.
fn wrapped(line: Line, width: usize) -> Vec<Line> {
    let mut lines: Vec<Line> = vec![vec![]];
    for (text, tone) in line {
        let last = lines.last_mut().expect("there is always a line");
        let can_break = !last.is_empty() && (text.starts_with(' ') || tone == Tone::Good);
        if can_break && visible(last) + text.width() > width {
            lines.push(vec![(text.trim_start().to_string(), tone)]);
        } else {
            last.push((text, tone));
        }
    }
    lines
}

/// `text` in pieces of at most `width` columns, each ending after a `/`
/// when one falls in it.
fn folded(text: &str, width: usize) -> Vec<String> {
    let mut pieces = vec![];
    let mut rest = text;
    while rest.width() > width {
        let mut end = 0;
        let mut taken = 0;
        for (at, ch) in rest.char_indices() {
            taken += ch.width().unwrap_or(0);
            if taken > width && end > 0 {
                break;
            }
            end = at + ch.len_utf8();
        }
        let end = rest[..end]
            .rfind('/')
            .filter(|&slash| slash > 0)
            .map_or(end, |slash| slash + 1);
        pieces.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    pieces.push(rest.to_string());
    pieces
}

/// `line` cut to `width` columns, never wrapped, in its colors; a cut
/// line ends with `…`.
fn painted(line: &Line, width: usize) -> String {
    let long = visible(line) > width;
    let mut left = if long { width.saturating_sub(1) } else { width };
    let mut kept: Line = vec![];
    for (text, tone) in line {
        if left == 0 {
            break;
        }
        let mut cut = String::new();
        for ch in text.chars() {
            let columns = ch.width().unwrap_or(0);
            if columns > left {
                left = 0;
                break;
            }
            left -= columns;
            cut.push(ch);
        }
        kept.push((cut, *tone));
    }
    let mut out = colored(&kept);
    if long && width > 0 {
        out.push('…');
    }
    out
}

/// `line` whole, in its colors.
fn colored(line: &Line) -> String {
    line.iter()
        .map(|(text, tone)| match tone {
            Tone::Plain => text.clone(),
            Tone::Bold => paint::bold(text, WHITE),
            Tone::Dim => paint::fg(text, DIM),
            Tone::Good => paint::fg(text, VOLT),
            Tone::Warn => paint::fg(text, AMBER),
        })
        .collect()
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

/// The most rows what arrives later can add to the facts: Plow's other
/// hosts and the line saying how many more (4), and the host's state, an
/// error's message and the Exec row (2).
const ARRIVING_ROWS: usize = 6;

impl Screen {
    fn new(columns: usize, rows: usize, color: bool, facts: &Facts) -> Screen {
        let closing = facts.closing_rows(columns);
        let tall = |layout: &Layout| {
            let width = facts_width(columns, layout);
            let facts = facts.rows(width).len() + ARRIVING_ROWS;
            let block = match layout {
                Layout::Beside => facts.max(logo::ROWS),
                Layout::Above => logo::ROWS + 1 + facts,
            };
            (block + closing, facts + closing)
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

    /// The lines of one frame: the mark as `pixels` and the facts so far,
    /// each with the columns it takes.
    fn lines(&self, pixels: &logo::Pixels, facts: &Facts) -> Vec<(String, usize)> {
        let width = facts_width(self.columns, &self.layout);
        let rows: Vec<(String, usize)> = facts
            .rows(width)
            .iter()
            .map(|row| (painted(row, width), visible(row).min(width)))
            .collect();
        // Never cut: wider than the screen, the terminal wraps it.
        let closing = facts
            .closing(self.columns)
            .into_iter()
            .map(|row| (colored(&row), visible(&row)));
        let mark = logo::lines(pixels);
        let mut lines = vec![];
        match self.layout {
            Layout::Beside => {
                let blank = " ".repeat(logo::COLUMNS);
                for i in 0..rows.len().max(mark.len()) {
                    let left = mark.get(i).unwrap_or(&blank);
                    let (right, columns) = rows
                        .get(i)
                        .map_or(("", 0), |(row, columns)| (row, *columns));
                    lines.push((
                        format!("{left}{}{right}", " ".repeat(GAP))
                            .trim_end()
                            .to_string(),
                        logo::COLUMNS + GAP + columns,
                    ));
                }
            }
            Layout::Above => {
                if !self.fixed {
                    lines.extend(mark.into_iter().map(|line| (line, logo::COLUMNS)));
                    lines.push((String::new(), 0));
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
        let columns = self.columns.max(1);
        for (line, width) in &lines {
            // Cleared first: after a full-width row the cursor still sits
            // on its last character, which clearing after would erase.
            text.push_str(term::CLEAR_LINE_END);
            text.push_str(line);
            // A line the terminal wrapped is cleared after too, on its
            // last row, unless it ends at the edge.
            if *width > columns && width % columns != 0 {
                text.push_str(term::CLEAR_LINE_END);
            }
            text.push('\n');
        }
        text.push_str(term::CLEAR_BELOW);
        text.push_str(term::SYNC_END);
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
        self.drawn = lines
            .iter()
            .map(|(_, width)| width.div_ceil(columns).max(1))
            .sum();
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

#[cfg(test)]
mod tests;
