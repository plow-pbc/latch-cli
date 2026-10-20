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
    /// The host's vault: add, list, remove and import items (owner devices only)
    Vault(VaultArgs),
    /// Run this machine as a host, in the foreground, until stopped
    Serve(ServeArgs),
    /// Internal: the sandboxed process a host starts for each file read or write
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
    /// One action, or a list of them: `act click e7`, `act click "New expense"`,
    /// `act type "amount" 12.50`, `act select e5 Blue`, `act press Enter`,
    /// `act scroll down`, `act back`; `act --steps FILE` for several, one per
    /// line in the same form; `act --history R` for what request R did
    Act {
        #[command(flatten)]
        step: StepArgs,
        /// Several steps, one per line in act's own form, from FILE (or - for stdin)
        #[arg(long, value_name = "FILE", conflicts_with = "history")]
        steps: Option<PathBuf>,
        /// The per-step trace of request R (an `act` that exited 5 names it)
        #[arg(long, value_name = "R")]
        history: Option<String>,
    },
    /// Candidate refs for a description, or for a role and exact name; never acts
    Find {
        /// What the element is, in words: "the September invoice's download button"
        description: Option<String>,
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        name: Option<String>,
        /// The action the element is for (click, type, select …; default click)
        #[arg(long)]
        action: Option<ActAction>,
    },
    /// Wait for an element's state, text, a URL, or a sentence, with a deadline
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
        /// A URL the page's starts with
        #[arg(long)]
        url: Option<String>,
        /// A sentence about what the page shows, judged by the decision model
        #[arg(long = "for", value_name = "SENTENCE")]
        sentence: Option<String>,
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
    /// Sign in with a vault login: latch picks the form's fields (or name them)
    /// and fills them itself; the agent never sees a value
    Login {
        /// The snapshot the refs are from (default: the latest)
        #[arg(long)]
        snapshot: Option<String>,
        /// The vault item (such as v3)
        #[arg(long)]
        item: String,
        #[arg(long)]
        username: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        submit: Option<String>,
    },
    /// Fill a card, identity or note's fields: --field number=e4 --field
    /// expiry=e5 …; a secret field needs --press eN, the button that submits it
    Fill {
        #[arg(long)]
        snapshot: Option<String>,
        #[arg(long)]
        item: String,
        /// LABEL=eN, or LABEL=eN,eN,… to split it across boxes
        #[arg(long = "field", value_name = "LABEL=REFS")]
        fields: Vec<String>,
        #[arg(long)]
        press: Option<String>,
    },
    /// Fill the current authenticator code of a vault login, computed on the host
    FillCode {
        #[arg(long)]
        snapshot: Option<String>,
        #[arg(long)]
        item: String,
        /// The code field (default: the one latch finds)
        #[arg(long)]
        field: Option<String>,
        #[arg(long)]
        submit: Option<String>,
    },
    /// Your open sessions, without their ids
    Sessions,
    /// End the session (its profile, and other sessions on it, stay); an
    /// owner device may name any session by the hash the host's dashboard shows
    Close,
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

impl ActAction {
    pub fn name(self) -> &'static str {
        match self {
            ActAction::Click => "click",
            ActAction::Type => "type",
            ActAction::Select => "select",
            ActAction::Press => "press",
            ActAction::Scroll => "scroll",
            ActAction::Back => "back",
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

/// One step of `act`: on the command line, or one line of `--steps`.
#[derive(Args, Debug, Clone, Default)]
pub struct StepArgs {
    /// The snapshot a ref is from (default: the latest)
    #[arg(long)]
    pub snapshot: Option<String>,
    pub action: Option<ActAction>,
    /// A ref such as e7, an element's exact name, or a description of it
    /// (or a key for press, a direction for scroll)
    pub target: Option<String>,
    /// The text to type, option to choose, key to press or direction
    pub text: Option<String>,
    /// For click: a position in the page's CSS pixels instead of a target
    #[arg(long, value_parser = parse_at, value_name = "X,Y")]
    pub at: Option<(f64, f64)>,
    /// What the page will show after the step, in a sentence the decision
    /// model judges: "a form for a new expense is shown"
    #[arg(long, value_name = "SENTENCE")]
    pub expect: Option<String>,
    /// The page's URL will start with this
    #[arg(long, value_name = "URL")]
    pub expect_url: Option<String>,
    /// The page will show this text
    #[arg(long, value_name = "TEXT")]
    pub expect_text: Option<String>,
    /// A field will hold a value, or be checked or unchecked
    #[arg(long, value_name = "eN=VALUE")]
    pub expect_field: Option<String>,
    /// A download the step starts will complete
    #[arg(long, value_parser = ["complete"])]
    pub expect_download: Option<String>,
    /// Seconds the expectations may take (default 10)
    #[arg(long, value_name = "S")]
    pub within: Option<f64>,
}

/// One line of `act --steps`, parsed as `act`'s own arguments.
#[derive(clap::Parser, Debug)]
#[command(no_binary_name = true, name = "step")]
pub struct StepLine {
    #[command(flatten)]
    pub step: StepArgs,
}

fn parse_at(text: &str) -> Result<(f64, f64), String> {
    let (x, y) = text.split_once(',').ok_or("write the position as X,Y")?;
    let number = |n: &str| {
        n.trim()
            .parse::<f64>()
            .map_err(|_| format!("{n:?} is not a number"))
    };
    Ok((number(x)?, number(y)?))
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
}
