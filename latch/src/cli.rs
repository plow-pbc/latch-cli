//! The command line, declared as plain structs. clap's `derive` reads these
//! definitions (and their `///` comments, which become the `--help` text) and
//! generates the parser for us.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Defaults baked in when latch is built with these variables set, for a
/// local build that keeps its own state and talks to one Plow. Flags still
/// override them. Unset, latch uses `~/.latch` and has no default Plow.
pub const BUILD_STATE_DIR: Option<&str> = option_env!("LATCH_BUILD_STATE_DIR");
pub const BUILD_API_URL: Option<&str> = option_env!("LATCH_BUILD_API_URL");

/// What `--version` prints. A build with baked-in defaults names them, so it
/// is obvious which build this is. clap wants a `&'static str`; leaking the
/// one string, once per run, gives it that.
fn version() -> &'static str {
    let version = env!("CARGO_PKG_VERSION");
    match (BUILD_STATE_DIR, BUILD_API_URL) {
        (None, None) => version,
        (dir, url) => {
            let dir = dir.unwrap_or("~/.latch");
            let url = url.unwrap_or("none");
            Box::leak(format!("{version} (local build: state {dir}, Plow {url})").into_boxed_str())
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "latch",
    version = version(),
    about = "Reach your other machines over iroh, with Plow as the directory."
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
    /// Run this machine as a host, in the foreground, until stopped
    Serve(ServeArgs),
    /// Internal: the sandboxed process a host starts for each file read or write, exec, Google call and startup probe
    #[command(hide = true)]
    Worker(WorkerArgs),
    /// Internal: the process a host starts for each browser session
    #[command(hide = true)]
    BrowserWorker(BrowserWorkerArgs),
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
    #[command(subcommand)]
    pub command: BrowserCommand,
}

#[derive(Subcommand, Debug)]
pub enum BrowserCommand {
    /// Start a new session on a profile, load URL, and list the page's elements;
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
    },
    /// The page's text
    Text,
    /// A PNG of the page, written to a file on this machine
    Screenshot {
        #[arg(long, short)]
        output: PathBuf,
    },
    /// The plan command: up to 10 steps in order, each `click|type|select|press
    /// eN ["text"]`, `fill --item vN --field F=eN… [--press eN]`, or a goal in
    /// words with its texts in double quotes, with an optional check after
    /// ` -> `: `do 'click e7 -> path: /cart' 'add the note "Ring twice"'`
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
    /// Linux: the X display for the engine, or `private` to start one
    #[arg(long)]
    pub display: Option<String>,
}

/// Arguments the host gives a worker (see `worker.rs`).
#[derive(Args, Debug)]
pub struct WorkerArgs {
    /// A directory the worker may read and write (repeated)
    #[arg(long = "area")]
    pub areas: Vec<PathBuf>,
    /// The host's state directory, which the worker may never touch
    #[arg(long)]
    pub denied: PathBuf,
    /// An exec worker's own temp folder (its `TMPDIR`), which it may read and write
    #[arg(long)]
    pub tmpdir: Option<PathBuf>,
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
    /// Write exactly SIZE bytes from stdin to a file, atomically
    Write {
        #[arg(long)]
        size: u64,
        /// The file's name, used if PATH is a folder
        #[arg(long)]
        name: Option<String>,
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
pub struct StatusArgs {
    /// Show a card for a person, with key art to compare between machines, instead of JSON
    #[arg(long)]
    pub pretty: bool,
}

#[derive(Args, Debug)]
pub struct ServeArgs {
    /// Folders to share: callers may read and write only inside these (the first is where `exec` starts)
    #[arg(value_name = "FOLDER")]
    pub folders: Vec<PathBuf>,

    /// Log plainly even on a terminal, instead of showing the live dashboard
    #[arg(long)]
    pub no_dashboard: bool,

    /// Run every browser session with no window, in Chrome's headless mode:
    /// for a test host nobody watches. Sites block headless browsers far
    /// more often, so a host people use runs them headed (the default)
    #[arg(long)]
    pub headless_browsers: bool,
}

#[derive(Args, Debug)]
pub struct LoginArgs {
    /// File holding the owner's Plow login token, saved for this machine [without it: the token saved here, else an agent's own credential ($PLOW_TOKEN_FILE, else /config/secrets/plow-api-token), else at a terminal a login by text]
    #[arg(long, value_name = "FILE")]
    pub token_file: Option<PathBuf>,

    /// Plow API base URL, e.g. https://api.plow.co
    #[arg(long, env = "PLOW_API_URL", value_name = "URL", default_value = BUILD_API_URL)]
    pub api_url: Option<String>,

    /// Name of this peer, unique in the account (a machine's is asked for at a terminal; an agent defaults to its own name)
    #[arg(long)]
    pub name: Option<String>,
}

/// Which host a command goes to. Without `--host` or `$LATCH_HOST`, latch
/// uses the only host in the directory, if there is exactly one.
#[derive(Args, Debug)]
pub struct HostArg {
    /// Name of the host to use
    #[arg(long, env = "LATCH_HOST")]
    pub host: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum FileCommand {
    /// Copy a file from the host to this machine
    Get {
        /// The file on the host: absolute, or relative to the host's first shared folder. Anything the host's user can read except latch's state folder
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
        /// Where it goes on the host, inside one of its shared folders: absolute, or relative to the first. An existing folder gets the file inside it, under its own name, as with `cp`
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

    /// Directory to run in, inside one of the host's areas [default: the first area]
    #[arg(long, value_name = "DIR")]
    pub workdir: Option<String>,

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
