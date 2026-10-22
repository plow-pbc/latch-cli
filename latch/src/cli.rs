//! The command line, declared as plain structs. clap's `derive` reads these
//! definitions (and their `///` comments, which become the `--help` text) and
//! generates the parser for us.

use std::path::PathBuf;
use std::sync::OnceLock;

use clap::{Args, Parser, Subcommand};

/// Defaults baked in when latch is built with these variables set, for a
/// local build that keeps its own state and talks to one Plow. Flags still
/// override them. Unset, latch uses `~/.latch`, and only a release build has
/// a default Plow (`default_api_url`).
pub const BUILD_STATE_DIR: Option<&str> = option_env!("LATCH_BUILD_STATE_DIR");
pub const BUILD_API_URL: Option<&str> = option_env!("LATCH_BUILD_API_URL");
/// How this build is distributed (e.g. `standalone`), set by a release
/// build. Unset, it is a local build: made by hand from a checkout.
pub const BUILD_CHANNEL: Option<&str> = option_env!("LATCH_BUILD_CHANNEL");
/// The release workflow's run number, set by a release build: the build
/// number `--version` shows after the version, as `0.1.0 (1327)`.
pub const BUILD_NUMBER: Option<&str> = option_env!("LATCH_BUILD_NUMBER");

/// The Plow a build from `channel` logs in to when nothing else names one:
/// real Plow for a release (`standalone`), none for a local build, which
/// points at a dev stack.
pub fn default_api_url(channel: Option<&str>) -> Option<&'static str> {
    let first = channel.and_then(|channel| channel.split_whitespace().next());
    (first == Some("standalone")).then_some("https://api.plow.co")
}

/// What `--version` prints, e.g. `0.1.0 (commit 1a2b3c4d5, built 2026-10-21
/// 03:45 UTC, local build, protocol plow/latch/1)`, or for a release build
/// `0.1.0 (1327): commit …, standalone tag v0.1.0-1327, …`: where this build
/// came from (see `build.rs`), the protocol it speaks to peers and, for a
/// build with baked-in defaults, those, so it is obvious which build this is.
pub fn version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        let mut about = format!(
            "commit {}, built {}, {}, protocol {}",
            env!("LATCH_COMMIT"),
            env!("LATCH_BUILT"),
            BUILD_CHANNEL.unwrap_or("local build"),
            String::from_utf8_lossy(crate::wire::ALPN),
        );
        if BUILD_STATE_DIR.is_some() || BUILD_API_URL.is_some() {
            let dir = BUILD_STATE_DIR.unwrap_or("~/.latch");
            let url = BUILD_API_URL.unwrap_or("none");
            about.push_str(&format!("; state {dir}, Plow {url}"));
        }
        version_line(env!("CARGO_PKG_VERSION"), BUILD_NUMBER, &about)
    })
}

/// `package (number): about` for a numbered build, else `package (about)`.
fn version_line(package: &str, number: Option<&str>, about: &str) -> String {
    match number.filter(|number| !number.is_empty()) {
        Some(number) => format!("{package} ({number}): {about}"),
        None => format!("{package} ({about})"),
    }
}

/// What `latch --help` adds after the commands: how an agent uses latch.
const AGENT_HELP: &str = "\
Agents:
  Log in with your own Plow agent credential (`latch login` finds it). Never
  use a device login copied from one of the owner's machines: that is the
  owner acting, and hosts judge it as the owner's own request, not against
  what the owner asked you for.

  Exit 4 means denied. A host's gatekeeper refusal, gatekeeper_denied with
  reason not_covered, is the one to retry: ask the owner in your Plow chat,
  naming exactly what you will do, then retry once they agree. owner_said_no
  and exec_denied (the owner turned exec off on that host) mean no.
  Exit 3, gatekeeper_unavailable: the host could not judge. Its message says
  what fixes it: often a retry later, but a host out of Plow credits fails
  until the host's account owner adds them. A browser step's
  model_unavailable (exit 1) is the same decision model, for a click's risk,
  a goal or a sentence check; nothing was sent, and nothing gets around it.

  `latch skill` prints the guide for agents; `latch help --json` describes
  every command; `latch help COMMAND --examples` shows how to call one.";

/// What `latch browser do --help` adds after its options: the short form
/// by example. BROWSER.md shows the same examples.
pub(crate) const DO_HELP: &str = "\
Examples (one step per argument, each in single quotes so the shell keeps its double quotes):
  Literal steps, on refs from the last page latch returned:
    latch browser do 'type e3 \"Ada Lovelace\"' 'select e5 \"Large\"' 'press e3 Enter' 'click e7'
  A goal in words; the texts in its double quotes are all it may type:
    latch browser do 'search for \"blue mug\"' 'add the note \"Ring twice\" for the driver'
  A check after ` -> `: typed (path:, text:, new text:, field: eN=..., query:, origin:,
  download: complete), or a sentence the decision model judges:
    latch browser do 'click e7 -> path: /cart' 'type e3 \"Acme\" -> field: e3=Acme'
    latch browser do 'click e9 -> the cart shows 2 items'
  A fact, which a goal types where it names it as {NAME}:
    latch browser do --fact street='12 Elm Road' 'enter the delivery street {street}'
  The plan as it would be sent, sending nothing (a ` -> ` inside double quotes is text):
    latch browser do --dry-run 'add the note \"A -> B\" for the driver'

At most 10 steps. A ref is from the last page latch returned (or --snapshot);
once the page has changed, observe again for fresh refs.
A plan stops at the first step that does not pass. Before going on, read
`steps` (each step's outcome; those before the stop ran) and `stop` (its
step, code and message), then send only what is left: resending the whole
plan repeats what already ran. An answer that exited 5 names its request:
read `do --history R` first, and never resend it.";

#[derive(Parser, Debug)]
#[command(
    name = "latch",
    version = version(),
    about = "Reach your other machines over iroh, with Plow as the directory.",
    after_help = AGENT_HELP,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Directory of this latch instance: its key, Plow credential and config (~/.latch unless this build has another default)
    #[arg(long, global = true, value_name = "DIR", default_value = BUILD_STATE_DIR)]
    pub state_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create this peer's key if missing and register it with Plow
    Login(LoginArgs),
    /// List this account's machines, online or not, or remove one
    Devices(DevicesArgs),
    /// Print this peer's identity and the hosts it can reach
    Status(StatusArgs),
    /// Copy a file between this machine and a host
    #[command(subcommand)]
    File(FileCommand),
    /// Run one allowed Google command (gog) on a host
    #[command(after_help = crate::google::policy::allowed_help())]
    Google(GoogleArgs),
    /// Run one program on a host, inside its sandbox
    Exec(ExecArgs),
    /// Drive a real browser on a host: open, observe, act by ref
    Browser(BrowserArgs),
    /// The host's vault: add, list, edit, remove and import items (owner devices only)
    Vault(VaultArgs),
    /// The owner's standing rules for agents on a host, which the host's
    /// gatekeeper reads at every judgment (owner devices only)
    Rules(RulesArgs),
    /// The folders `latch serve` shares when it is named none, and its --allow-read paths: show them, or set them
    Shares(SharesArgs),
    /// This machine's host audit log: its records in full, its chain checked without the key, or a new key
    Audit(AuditArgs),
    /// This machine's host as JSON lines, for an app: a snapshot of its state and recent activity, then with --follow each change
    Events(EventsArgs),
    /// Run this machine as a host, in the foreground, until stopped
    #[command(after_help = SERVE_HELP)]
    Serve(ServeArgs),
    /// Help for a command, its examples, or every command as JSON
    Help(HelpArgs),
    /// Print SKILL.md, which teaches an agent how to use latch
    Skill,
    /// Internal: the sandboxed process a host starts for each file read or write, exec, Google call and startup probe
    #[command(hide = true)]
    Worker(WorkerArgs),
    /// Internal: the process a host starts for each browser session
    #[command(hide = true)]
    BrowserWorker(BrowserWorkerArgs),
}

#[derive(Args, Debug)]
pub struct HelpArgs {
    /// The command, e.g. `browser` or `browser open` [default: every command]
    #[arg(value_name = "COMMAND")]
    pub path: Vec<String>,
    /// Examples of each command, ready to adapt
    #[arg(long, conflicts_with = "json")]
    pub examples: bool,
    /// Each command's inputs, output, action id, side effects and retry
    /// rule, and latch's exit codes, as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct BrowserArgs {
    /// Name of the host to use
    #[arg(long, env = "LATCH_HOST", global = true)]
    pub host: Option<String>,
    /// The session to use: the id `open` answered
    #[arg(long, env = "LATCH_BROWSER_SESSION", global = true)]
    pub session: Option<String>,
    /// The agent turn this call belongs to, for the host's metrics
    #[arg(long, env = "LATCH_TURN", global = true)]
    pub turn: Option<String>,
    /// The owner's Plow chat this agent acts for, when its line has more
    /// than one: the host's gatekeeper reads what the owner asked there
    #[arg(long, env = "LATCH_CHAT", global = true)]
    pub chat: Option<String>,
    #[command(subcommand)]
    pub command: BrowserCommand,
}

#[derive(Subcommand, Debug)]
pub enum BrowserCommand {
    /// Start a new session on a profile, load URL, and list the elements on screen;
    /// answers the session's id, which every later command needs
    Open {
        /// The profile, whose logins the session shares (default: default)
        #[arg(long)]
        profile: Option<String>,
        /// A label for the session, shown in listings and the audit
        #[arg(long)]
        task: Option<String>,
        /// Hold URL's site until the session closes: another session's action
        /// there gets site_busy
        #[arg(long)]
        reserve_site: bool,
        url: Option<String>,
    },
    /// A fresh element list (snapshot), the page's first text, and the tabs
    Observe {
        /// Observe this tab, making it the active one
        #[arg(long)]
        tab: Option<String>,
        /// Only the element with this ref and what is inside it
        #[arg(long)]
        scope: Option<String>,
        /// Every element the snapshot read, off-screen ones too, and every
        /// frame: answers show the elements on screen, at most 60
        #[arg(long)]
        all: bool,
    },
    /// The page's text
    Text {
        /// Print only the text, as it is in the answer (the same cut and
        /// the same fields left out), not JSON
        #[arg(long)]
        raw: bool,
    },
    /// A PNG of the page, written to a file on this machine
    Screenshot {
        /// Where the PNG goes on this machine
        #[arg(value_name = "PATH")]
        output: PathBuf,
    },
    /// The plan command: up to 10 steps in order, each `click|type|select|press
    /// eN ["text"]`, `fill --item vN --field F=eN… [--press eN]`, or a goal in
    /// words with its texts in double quotes, with an optional check after
    /// ` -> `: `do 'click e7 -> path: /cart' 'add the note "Ring twice"'`
    #[command(after_help = DO_HELP)]
    Do {
        /// The steps, one argument each
        steps: Vec<String>,
        /// The canonical plan object, from FILE (or - for stdin)
        #[arg(long, value_name = "FILE", conflicts_with_all = ["steps", "history"])]
        plan_file: Option<PathBuf>,
        /// Carry out confirmation C from a `confirm_needed` stop, then the steps
        #[arg(long, value_name = "C", conflicts_with = "history")]
        confirm: Option<String>,
        /// Request R's steps, actions and outcomes (an answer that exited 5 names R)
        #[arg(long, value_name = "R", conflicts_with = "steps")]
        history: Option<String>,
        /// With --history: one of R's screenshots, by action number or `final`, written to --output
        #[arg(long, value_name = "N", requires_all = ["history", "output"])]
        shot: Option<String>,
        /// Where a PNG goes on this machine: the page as the plan ended, or --shot's
        #[arg(long, short)]
        output: Option<PathBuf>,
        /// A text every step may type, as NAME=TEXT (repeatable)
        #[arg(long, value_name = "NAME=TEXT", conflicts_with = "history")]
        fact: Vec<String>,
        /// A vault item the plan may fill from (repeatable)
        #[arg(long, value_name = "vN", conflicts_with = "history")]
        item: Vec<String>,
        /// Print the plan as it would be sent, and send nothing
        #[arg(long, conflicts_with = "history")]
        dry_run: bool,
        /// Seconds the whole plan may take (default 120, at most 300)
        #[arg(long, value_name = "S")]
        within: Option<f64>,
    },
    /// One literal step on a ref from the last page latch returned:
    /// `act click e7`, `act type e3 "Acme"`, `act select e5 Large`,
    /// `act press e4 Enter`, `act scroll [eN] down`, `act back`; run as a
    /// plan of one step
    Act {
        /// The snapshot the ref is from (default: the latest)
        #[arg(long)]
        snapshot: Option<String>,
        action: Option<ActAction>,
        /// A ref such as e7 (or a direction for scroll)
        target: Option<String>,
        /// The text to type, option to choose, key to press or direction
        text: Option<String>,
        /// What the step must lead to: `path: /cart`, `text: Saved`,
        /// `field: e3=Large`, `download: complete`, … or a sentence
        #[arg(long, value_name = "CHECK")]
        check: Option<String>,
    },
    /// Hand this session to the owner, who acts in its window (a CAPTCHA, a
    /// texted code, a passkey, a sign-in): waits until they hand it back.
    /// On the host, `handoff done [HASH]` hands a session back.
    Handoff {
        /// `done`: on the host, hand a pending session back to its agent
        action: Option<HandoffAction>,
        /// With `done`: the session's hash, when several are pending
        hash: Option<String>,
        #[arg(long)]
        reason: Option<HandoffReason>,
        /// Seconds to wait for the owner (default 600)
        #[arg(long, value_name = "S")]
        within: Option<u64>,
    },
    /// Candidate refs by role and exact name; never acts
    Find {
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        name: Option<String>,
    },
    /// Wait for an element's state, text or a path, with a deadline
    Wait {
        /// An element by ref, with --state
        #[arg(long = "ref", value_name = "eN")]
        target: Option<String>,
        /// The snapshot the ref is from (default: the latest)
        #[arg(long)]
        snapshot: Option<String>,
        /// visible, hidden, enabled, disabled, checked, unchecked or gone
        #[arg(long, requires = "target")]
        state: Option<String>,
        /// Text the page shows
        #[arg(long)]
        text: Option<String>,
        /// Text that is part of the page's path
        #[arg(long, value_name = "P")]
        url_path: Option<String>,
        /// Seconds to wait (default 10)
        #[arg(long, value_name = "S")]
        within: Option<f64>,
    },
    /// Load URL in the session's active tab, and list its elements; a site
    /// the session has not been judged for is judged first, as `tab new URL`
    Goto { url: String },
    /// Open, switch to or close a tab
    Tab {
        #[command(subcommand)]
        command: TabCommand,
    },
    /// Answer the open dialog
    Dialog {
        action: DialogAction,
        /// The text for a prompt
        #[arg(long)]
        text: Option<String>,
    },
    /// Run JavaScript from a file (or - for stdin) in the page's main frame;
    /// prints its JSON value. The host audits the script.
    Eval {
        #[arg(long, value_name = "FILE")]
        script_file: PathBuf,
    },
    /// Put a file from this machine into a file input, by its ref
    Upload {
        /// The snapshot the ref is from (default: the latest)
        #[arg(long)]
        snapshot: Option<String>,
        target: String,
        file: PathBuf,
    },
    /// The session's downloads, as handles (d1, d2, …)
    Downloads,
    /// Bring a completed download to this machine
    Download {
        #[command(subcommand)]
        command: DownloadCommand,
    },
    /// Fill from a vault item, for every kind (a login, its authenticator
    /// code as `otp`, a card, an identity, a note): `--field username=e4
    /// --field password=e5 --press e6`, or `--suggested S`, a mapping
    /// `observe` suggested. A secret needs --press eN, the button that
    /// submits it; a card or identity secret stops for a confirmation (`do
    /// --confirm C`) before any value is released
    Fill {
        #[arg(long)]
        snapshot: Option<String>,
        #[arg(long)]
        item: String,
        /// FIELD=eN, or FIELD=eN,eN,… to split it across boxes
        #[arg(
            long = "field",
            value_name = "FIELD=REFS",
            conflicts_with = "suggested"
        )]
        fields: Vec<String>,
        /// A suggestion's id, from observe
        #[arg(long, value_name = "S")]
        suggested: Option<String>,
        #[arg(long)]
        press: Option<String>,
    },
    /// Removed: `fill` signs in now
    #[command(hide = true)]
    Login {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Removed: `fill --field otp=eN` fills a code now
    #[command(hide = true)]
    FillCode {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Your open sessions, without their ids
    Sessions,
    /// End the session (its profile, and other sessions on it, stay); an
    /// owner device may name any session by the hash the host's dashboard shows
    Close,
    /// Owner only, on the host itself: the host's browser profiles
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
}

#[derive(Subcommand, Debug)]
pub enum ProfileCommand {
    /// Make profile NAME, bound to ENGINE for life, in this machine's state
    /// directory (the host's, when run as the host)
    Create {
        name: String,
        #[arg(long, value_enum, default_value_t = EngineArg::Cft)]
        engine: EngineArg,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum EngineArg {
    Cft,
    Camoufox,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq)]
pub enum ActAction {
    Click,
    Type,
    Select,
    Press,
    Scroll,
    Back,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum HandoffAction {
    Done,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum HandoffReason {
    Captcha,
    #[value(name = "2fa")]
    TwoFactor,
    Passkey,
    Signin,
}

impl HandoffReason {
    pub fn name(self) -> &'static str {
        match self {
            HandoffReason::Captcha => "captcha",
            HandoffReason::TwoFactor => "2fa",
            HandoffReason::Passkey => "passkey",
            HandoffReason::Signin => "signin",
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum TabCommand {
    /// A new tab, on URL or blank; it becomes the active one
    New { url: Option<String> },
    /// Make tab T (such as t2) the active one
    Switch { tab: String },
    /// Close tab T, or the active one
    Close { tab: Option<String> },
}

#[derive(Subcommand, Debug)]
pub enum DownloadCommand {
    /// Write download D (such as d1) to a file here
    Get {
        download: String,
        #[arg(long, short)]
        output: PathBuf,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum DialogAction {
    Accept,
    Dismiss,
}

#[derive(Args, Debug)]
pub struct VaultArgs {
    /// Name of the host to use
    #[arg(long, env = "LATCH_HOST", global = true)]
    pub host: Option<String>,
    #[command(subcommand)]
    pub command: VaultCommand,
}

#[derive(Subcommand, Debug)]
pub enum VaultCommand {
    /// Every item's metadata (never a value)
    List,
    /// Add an item; its secrets are asked for at a prompt, never taken from
    /// the command line or the environment
    Add {
        /// login, card, identity or note
        #[arg(long)]
        kind: String,
        #[arg(long)]
        title: String,
        /// A login's site (required, repeatable); for a card or identity, the
        /// only sites it fills on
        #[arg(long)]
        site: Vec<String>,
        /// Another top-level site a login may be filled under
        #[arg(long)]
        other_top_site: Vec<String>,
        /// A non-secret field: LABEL=VALUE (username=…, cardholder name=…, city=…)
        #[arg(long)]
        field: Vec<String>,
        /// Also ask for this field at a prompt (an identity's ssn, say)
        #[arg(long)]
        ask: Vec<String>,
        /// Also ask for a login's authenticator key
        #[arg(long)]
        totp: bool,
    },
    /// Change an item at its current revision: its title, sites and fields.
    /// Its secrets are asked for at a prompt, never taken from the command
    /// line or the environment; its kind never changes
    Edit {
        item: String,
        /// The revision `vault list` shows; an older one is refused
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        title: Option<String>,
        /// Replaces the item's sites (repeatable)
        #[arg(long)]
        site: Vec<String>,
        /// Clears the item's sites: a card or identity then fills on any
        /// site (a login keeps at least one)
        #[arg(long, conflicts_with = "site")]
        no_sites: bool,
        /// Replaces a login's other top-level sites (repeatable)
        #[arg(long)]
        other_top_site: Vec<String>,
        /// Clears a login's other top-level sites
        #[arg(long, conflicts_with = "other_top_site")]
        no_other_top_sites: bool,
        /// A non-secret field: LABEL=VALUE; an empty VALUE clears it
        #[arg(long)]
        field: Vec<String>,
        /// Ask for this field at a prompt: password, totp (a login's
        /// authenticator key), a card's number or code, …
        #[arg(long)]
        ask: Vec<String>,
    },
    /// A one-time link that adds an item from your phone: the phone page
    /// connects to the host itself, and the value goes straight there.
    /// Prints the link, and a QR code on a terminal; it works once, for 15
    /// minutes
    Link {
        /// login, card, identity or note
        #[arg(long)]
        kind: String,
        #[arg(long)]
        title: String,
        /// A login's site (required, repeatable); for a card or identity, the
        /// only sites it fills on
        #[arg(long)]
        site: Vec<String>,
        /// A non-secret field, filled in on the phone: LABEL=VALUE
        #[arg(long)]
        field: Vec<String>,
        /// Also ask the phone for this field (an identity's ssn, say); a
        /// login's username and password, a card's number and code and a
        /// note's text are always asked for
        #[arg(long)]
        ask: Vec<String>,
        /// The phone page's address (see `phone/`); the link adds the rest
        /// after `#`
        #[arg(long, env = "LATCH_PHONE_PAGE")]
        page: String,
    },
    /// Remove an item at its current revision
    Remove {
        item: String,
        #[arg(long)]
        revision: u64,
    },
    /// Import another password manager's export (1PUX, CSV, JSON): shows
    /// what it holds and imports only the rows you pick
    Import { file: std::path::PathBuf },
}

/// Arguments the host gives a browser worker (see `browser/worker.rs`).
#[derive(Args, Debug)]
pub struct RulesArgs {
    /// The serving machine the rules are for [default: this machine's own host state]
    #[arg(long, global = true)]
    pub host: Option<String>,
    /// Whose rules: an agent's name, the peer name of the latch it calls
    /// from, or its agent uid, resolved once to the uid [default: every agent's]
    #[arg(long, global = true, value_name = "NAME")]
    pub from: Option<String>,
    #[command(subcommand)]
    pub command: RulesCommand,
}

#[derive(Subcommand, Debug)]
pub enum RulesCommand {
    /// Replace the rules with TEXT, or with the text of --file F (- reads stdin)
    Set {
        #[arg(conflicts_with = "file")]
        text: Option<String>,
        #[arg(long, value_name = "F")]
        file: Option<PathBuf>,
    },
    /// Change the rules in $EDITOR; saved only if they did not change meanwhile
    Edit,
    /// Remove the rules
    Clear,
    /// The rules for every agent and each agent's own, with their revisions
    Show,
}

#[derive(Args, Debug)]
pub struct BrowserWorkerArgs {
    /// The profile the session uses
    #[arg(long)]
    pub profile: String,
    /// The session's own folder: the engine's home, temp and crash folders
    #[arg(long)]
    pub scratch: PathBuf,
    /// Seconds without a call before the session closes
    #[arg(long, default_value_t = 1800)]
    pub idle_timeout: u64,
    /// Seconds a call may take, before time for typing or waiting; shorter
    /// only in tests
    #[arg(long, default_value_t = crate::browser::core::CALL.as_secs())]
    pub call_limit: u64,
    /// Run the engine with no window (the host's `--headless-browsers`)
    #[arg(long)]
    pub headless: bool,
    /// Linux: start a private Xvfb display for the engine
    #[arg(long)]
    pub private_display: bool,
    /// Let the engine reach this machine's local addresses (the owner's
    /// `allow_local_browsing` in host.json)
    #[arg(long)]
    pub allow_local_browsing: bool,
}

/// Arguments the host gives a worker (see `worker.rs`).
#[derive(Args, Debug)]
pub struct WorkerArgs {
    /// A directory the worker may read and write (repeated)
    #[arg(long = "area")]
    pub areas: Vec<PathBuf>,
    /// A file or folder a file or exec worker may also read, never write (repeated)
    #[arg(long = "read")]
    pub reads: Vec<PathBuf>,
    /// A path no worker may read or write, even inside an area (repeated)
    #[arg(long = "hidden")]
    pub hidden: Vec<PathBuf>,
    /// The host's state directory, which the worker may never touch
    #[arg(long)]
    pub denied: PathBuf,
    /// The home folder readable by an exec worker
    #[arg(long)]
    pub home: Option<PathBuf>,
    /// Permit outbound IP networking
    #[arg(long)]
    pub network: bool,
    /// An exec worker's own temp folder (its `TMPDIR`), which it may read and write
    #[arg(long)]
    pub tmpdir: Option<PathBuf>,
    /// An exec worker's own time limit in seconds: past it, the worker
    /// kills its program, whether or not the host is still there to
    #[arg(long)]
    pub kill_after: Option<u64>,
    /// Test only: make the sandbox fail to apply
    #[arg(long)]
    pub break_sandbox_for_test: bool,
    #[command(subcommand)]
    pub action: WorkerAction,
}

#[derive(Subcommand, Debug)]
pub enum WorkerAction {
    /// Send a file's size and bytes to stdout
    Read { path: PathBuf },
    /// Write exactly SIZE bytes from stdin to a new file in its temp folder
    Write {
        #[arg(long)]
        size: u64,
        path: PathBuf,
    },
    /// Run the pinned Google CLI
    Google { program: PathBuf },
    /// Confine itself, as every worker does, then exit: the host's startup
    /// check that the sandbox can really be installed
    Probe,
    /// Become ARGV, run from WORKDIR (inside an area)
    Exec {
        #[arg(long)]
        workdir: Option<PathBuf>,
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
}

#[derive(Args, Debug)]
pub struct DevicesArgs {
    #[command(subcommand)]
    pub action: Option<DevicesAction>,
}

#[derive(Subcommand, Debug)]
pub enum DevicesAction {
    /// Remove a machine and sign out the Plow login it registered with
    Remove {
        name: String,
        /// Remove without asking (required when no one is at a terminal)
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Args, Debug)]
pub struct EventsArgs {
    /// Keep printing changes as they happen, until stopped (or, when stdin is a pipe, until it closes)
    #[arg(long)]
    pub follow: bool,
    /// Pick up right after this cursor, from an earlier line; one that cannot be honoured gets `reset` and a fresh snapshot
    #[arg(long, value_name = "CURSOR")]
    pub after: Option<String>,
    /// Only the host's state changes, no activity
    #[arg(long)]
    pub lifecycle_only: bool,
}

#[derive(Args, Debug)]
pub struct AuditArgs {
    #[command(subcommand)]
    pub action: AuditAction,
}

#[derive(Subcommand, Debug)]
pub enum AuditAction {
    /// Every record, opened with the key: each command in full, as it ran
    Show,
    /// Check every record's place in the chain, without the key
    Verify,
    /// Seal new records under a new key and keep every old one; a running `latch serve` uses it once restarted
    RotateKey,
}

#[derive(Args, Debug)]
pub struct SharesArgs {
    #[command(subcommand)]
    pub action: Option<SharesAction>,
}

#[derive(Subcommand, Debug)]
pub enum SharesAction {
    /// Replace the saved folders and --allow-read paths; a running `latch serve` keeps its own until restarted
    Set {
        #[arg(value_name = "FOLDER", required = true)]
        folders: Vec<PathBuf>,
        /// A file or folder callers may also read, never write (repeatable; see `latch serve --help`)
        #[arg(long, value_name = "PATH")]
        allow_read: Vec<PathBuf>,
    },
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Show a card for a person, with key art to compare between machines, instead of JSON
    #[arg(long)]
    pub pretty: bool,
    /// Only this machine's own latch, asking no one: state folder, login,
    /// whether a host serves it, version, saved shares and --allow-read paths. Works offline and before login
    #[arg(long, conflicts_with = "pretty")]
    pub local: bool,
}

#[derive(Args, Debug)]
pub struct ServeArgs {
    /// Folders to share: callers may read and write inside these (the first is where `exec` starts) [default: the ones `latch shares set` saved, with its --allow-read paths]
    #[arg(value_name = "FOLDER")]
    pub folders: Vec<PathBuf>,

    /// A file or folder outside the shares that callers may also read, never write, e.g. ~/.rustup (repeatable; needs FOLDERs). Refused inside the never-readable places below
    #[arg(long, value_name = "PATH", requires = "folders")]
    pub allow_read: Vec<PathBuf>,

    /// Log plainly even on a terminal, instead of showing the live dashboard
    #[arg(long)]
    pub no_dashboard: bool,

    /// Run every browser session with no window, in Chrome's headless mode:
    /// for a test host nobody watches. Sites block headless browsers far
    /// more often, so a host people use runs them headed (the default)
    #[arg(long)]
    pub headless_browsers: bool,

    /// Refuse `exec` to every agent, or with `=AGENT_UID` to that agent only (repeatable). Keyed by the agent's Plow uid, never its name; the owner's own machines are unaffected
    #[arg(long, value_name = "AGENT_UID", num_args = 0..=1, require_equals = true,
          default_missing_value = EVERY_AGENT)]
    pub deny_exec: Vec<String>,
}

/// What `latch serve --help` adds after the options.
const SERVE_HELP: &str = "\
What callers can reach (`exec`'s programs and `file get`/`file put` alike):
  Read and write: the shared folders, and an exec call's own temp folder.
  Read only: each --allow-read path, and the system's own files: programs,
  libraries, developer tools, Homebrew, certificates. Nothing else: not the
  rest of your home folder, not other projects.
  Never, even inside a shared or --allow-read folder: ~/.ssh, ~/.gnupg,
  keychains, browser profiles, Mail, Messages, Calendars, Contacts, macOS's
  privacy settings (TCC), latch's state (this host's, ~/.latch, and any
  other state folder in the shares, even one made while the host runs),
  ~/.aws, ~/.config/gcloud, ~/.azure, ~/.kube, ~/.docker/config.json,
  ~/.netrc, ~/.npmrc, ~/.git-credentials and ~/.config/gh. An --allow-read
  inside one of these is refused.
  Your home folder, its Library, and the folders above them (/Users, /)
  can be neither shared nor named with --allow-read: name the folders in it
  you need.
  exec runs with HOME set to the first shared folder. Tools kept in your home
  folder (~/.rustup and ~/.cargo, nvm, pyenv) need --allow-read.";

/// What a bare `--deny-exec` stands for. An agent uid is hex, so it is
/// never this.
pub const EVERY_AGENT: &str = "*";

#[derive(Args, Debug)]
pub struct LoginArgs {
    /// File holding the owner's Plow login token, saved for this machine [without it: the token saved here, else an agent's own credential ($PLOW_AGENT_TOKEN, else the file $PLOW_TOKEN_FILE or /config/secrets/plow-api-token, else none with $PLOW_API_BASE, whose proxy adds it), else at a terminal a login by text]
    #[arg(long, value_name = "FILE")]
    pub token_file: Option<PathBuf>,

    /// Plow API base URL, e.g. https://api.plow.co [else $PLOW_API_BASE, a cloud agent's; else $PLOW_API_URL]
    #[arg(long, value_name = "URL")]
    pub api_url: Option<String>,

    /// Name of this peer, unique in the account [default: a machine's hostname; an agent's own name in Plow]
    #[arg(long)]
    pub name: Option<String>,

    /// For an app: report the login as JSON lines on stdout (code, waiting, verified, expired, error), logging in by text with no terminal needed. Never prints the token
    #[arg(long)]
    pub events: bool,
}

/// Which host a command goes to. Without `--host` or `$LATCH_HOST`, latch
/// uses the only host in the directory, if there is exactly one.
#[derive(Args, Debug)]
pub struct HostArg {
    /// Name of the host to use
    #[arg(long, env = "LATCH_HOST")]
    pub host: Option<String>,
    /// The owner's Plow chat this agent acts for, when its line has more
    /// than one: the host's gatekeeper reads what the owner asked there
    #[arg(long, env = "LATCH_CHAT")]
    pub chat: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum FileCommand {
    /// Copy a file from the host to this machine
    Get {
        /// The file on the host, inside its home folder: absolute, `~/…` (quote it so this shell leaves the `~` alone), or relative to home. Never latch's state or, on macOS, ~/Library (its Caches excepted)
        remote_path: String,
        /// Where to write it on this machine [default: the remote file's name, in the current directory]
        local_dest: Option<PathBuf>,
        #[command(flatten)]
        host: HostArg,
    },
    /// Copy a file from this machine to the host
    Put {
        /// The file on this machine to send
        local_path: PathBuf,
        /// Where it goes on the host, inside its home folder: absolute, `~/…` (quoted), or relative to home. An existing folder gets the file inside it, under its own name, as with `cp`. The judge sees the resolved path; the old file stays until the new one is complete
        remote_path: String,
        #[command(flatten)]
        host: HostArg,
    },
}

#[derive(Args, Debug)]
pub struct GoogleArgs {
    #[command(flatten)]
    pub host: HostArg,

    /// Google account(s) to use, comma-separated (may also be given after `--`)
    #[arg(long, value_delimiter = ',', value_name = "EMAIL")]
    pub account: Vec<String>,

    /// Book a calendar event even though it overlaps busy time, after the owner agreed (may also be given after `--`)
    #[arg(long)]
    pub confirm_conflict: bool,

    /// The gog command and its arguments, after `--`
    #[arg(last = true, required = true, value_name = "GOG_ARGS")]
    pub gog_args: Vec<String>,
}

/// What `latch exec --help` adds after the options.
const EXEC_HELP: &str = "\
Exit code and output:
  latch's own exit code says whether latch worked (0 ok, 1 failed, 2 usage,
  3 host unreachable, 4 denied, 5 connection lost). The command's own result
  is `exit_code` (or `signal`, or `timed_out`) in the JSON printed on stdout.
  stdout and stderr are each kept up to 256 KiB (then `truncated` is true).
  For big or binary output, write it to a file and fetch it with `latch file get`.

Input:
  Without -i the command's stdin is empty (so `echo hi | latch exec -- cat`
  prints nothing). With -i (--stdin) latch reads its own stdin to the end
  first, at most 256 KiB, within --timeout, and sends it; a terminal, more
  than 256 KiB or a pipe that never ends is a usage error and nothing runs:
    latch exec --host pi -i -- sh < script.sh
    cat <<'EOF' | latch exec --host pi -i -- sh
    uname -a
    EOF
  For bigger input, `latch file put` a file and read that.

Example:
  latch exec --host pi -- sh -c 'uname -a; ls'";

#[derive(Args, Debug)]
#[command(after_help = EXEC_HELP)]
pub struct ExecArgs {
    #[command(flatten)]
    pub host: HostArg,

    /// Directory inside the host's home [default: first write folder, otherwise home]
    #[arg(long, value_name = "DIR")]
    pub workdir: Option<String>,

    /// File or folder the command may write (repeatable); otherwise only temp and caches are writable
    #[arg(long = "write", value_name = "PATH")]
    pub writes: Vec<String>,

    /// Allow outbound IP networking; host Unix sockets stay blocked
    #[arg(long)]
    pub network: bool,

    /// Seconds before the program and everything it started are killed (1 to 600)
    #[arg(long, default_value_t = 60, value_name = "SECS",
          value_parser = clap::value_parser!(u64).range(1..=600))]
    pub timeout: u64,

    /// Send this machine's stdin to the program (read to the end first, at most 256 KiB); without it the program's stdin is empty
    #[arg(short = 'i', long)]
    pub stdin: bool,

    /// The program and its arguments, after `--` (no shell is involved)
    #[arg(last = true, required = true, value_name = "ARGV")]
    pub argv: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_numbered_build_shows_its_number_after_the_version() {
        assert_eq!(
            version_line("0.1.0", Some("1327"), "commit abc, protocol p"),
            "0.1.0 (1327): commit abc, protocol p"
        );
        assert_eq!(
            version_line("0.1.0", None, "commit abc, local build"),
            "0.1.0 (commit abc, local build)"
        );
        assert_eq!(version_line("0.1.0", Some(""), "x"), "0.1.0 (x)");
    }

    fn serve(args: &[&str]) -> ServeArgs {
        let args = [&["latch", "serve"], args].concat();
        match Cli::try_parse_from(args).unwrap().command {
            Command::Serve(serve) => serve,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_screenshot_takes_its_path_as_an_argument() {
        let parsed = Cli::try_parse_from(["latch", "browser", "screenshot", "page.png"]).unwrap();
        let Command::Browser(args) = parsed.command else {
            panic!("not a browser command");
        };
        let BrowserCommand::Screenshot { output } = args.command else {
            panic!("not a screenshot");
        };
        assert_eq!(output, PathBuf::from("page.png"));
        assert!(Cli::try_parse_from(["latch", "browser", "screenshot"]).is_err());
    }

    #[test]
    fn deny_exec_takes_its_agent_only_after_an_equals_sign() {
        let bare = serve(&["/a", "--deny-exec", "/b"]);
        assert_eq!(bare.folders, [PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(bare.deny_exec, [EVERY_AGENT]);

        let some = serve(&["/a", "--deny-exec=ab12", "--deny-exec=cd34"]);
        assert_eq!(some.folders, [PathBuf::from("/a")]);
        assert_eq!(some.deny_exec, ["ab12", "cd34"]);

        assert!(serve(&["/a"]).deny_exec.is_empty());
    }
    #[test]
    fn google_requires_the_separator_and_leaves_gog_flags_alone() {
        assert!(Cli::try_parse_from(["latch", "google", "gmail", "labels", "list"]).is_err());
        let args = Cli::try_parse_from([
            "latch",
            "google",
            "--account",
            "one@example.com",
            "--",
            "gmail",
            "labels",
            "list",
            "--account",
            "two@example.com",
        ])
        .unwrap();
        let Command::Google(args) = args.command else {
            panic!("google");
        };
        assert_eq!(args.account, ["one@example.com"]);
        assert_eq!(
            args.gog_args,
            ["gmail", "labels", "list", "--account", "two@example.com"]
        );
    }

    #[test]
    fn an_edit_either_replaces_or_clears_a_site_list() {
        let edit = |extra: &[&str]| {
            let mut argv = vec!["latch", "vault", "edit", "v1", "--revision", "2"];
            argv.extend(extra);
            Cli::try_parse_from(argv)
        };
        let parsed = edit(&["--no-sites", "--no-other-top-sites"]).unwrap();
        let Command::Vault(VaultArgs {
            command:
                VaultCommand::Edit {
                    no_sites,
                    no_other_top_sites,
                    ..
                },
            ..
        }) = parsed.command
        else {
            panic!("vault edit");
        };
        assert!(no_sites && no_other_top_sites);
        assert!(edit(&["--no-sites", "--site", "shop.example"]).is_err());
        assert!(edit(&["--no-other-top-sites", "--other-top-site", "pay.example"]).is_err());
    }
}
