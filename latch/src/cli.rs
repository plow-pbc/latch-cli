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

/// The Plow a build from `channel` logs in to when nothing else names one:
/// real Plow for a release (`standalone`), none for a local build, which
/// points at a dev stack.
pub fn default_api_url(channel: Option<&str>) -> Option<&'static str> {
    let first = channel.and_then(|channel| channel.split_whitespace().next());
    (first == Some("standalone")).then_some("https://api.plow.co")
}

/// What `--version` prints, e.g. `0.1.0 (commit 1a2b3c4d5, built 2026-10-21
/// 03:45 UTC, local build, protocol plow/latch/1)`: where this build came from
/// (see `build.rs`), the protocol it speaks to peers and, for a build with
/// baked-in defaults, those, so it is obvious which build this is.
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
        format!("{} ({about})", env!("CARGO_PKG_VERSION"))
    })
}

/// What `latch --help` adds after the commands: how an agent uses latch.
const AGENT_HELP: &str = "\
Agents:
  Log in with your own Plow agent credential (`latch login` finds it). Never
  use a device login copied from one of the owner's machines: that is the
  owner acting, and hosts never check it against what the owner asked for.

  Exit 4 means denied. A host's gatekeeper refusal, gatekeeper_denied with
  reason not_covered, is the one to retry: ask the owner in your Plow chat,
  naming exactly what you will do, then retry once they agree. owner_said_no
  and exec_denied (the owner turned exec off on that host) mean no.
  Exit 3, gatekeeper_unavailable: the host could not judge. Its message says
  what fixes it: often a retry later, but a host out of Plow credits fails
  until the host's account owner adds them.";

#[derive(Parser, Debug)]
#[command(
    name = "latch",
    version = version(),
    about = "Reach your other machines over iroh, with Plow as the directory.",
    after_help = AGENT_HELP
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
    /// The host's vault: add, list, edit, remove and import items (owner devices only)
    Vault(VaultArgs),
    /// The owner's standing rules for agents on a host, which the host's
    /// gatekeeper reads at every judgment (owner devices only)
    Rules(RulesArgs),
    /// The folders `latch serve` shares when it is named none, and its --allow-read paths: show them, or set them
    Shares(SharesArgs),
    /// This machine's host as JSON lines, for an app: a snapshot of its state and recent activity, then with --follow each change
    Events(EventsArgs),
    /// Run this machine as a host, in the foreground, until stopped
    #[command(after_help = SERVE_HELP)]
    Serve(ServeArgs),
    /// Internal: the sandboxed process a host starts for each file read or write, exec, Google call and startup probe
    #[command(hide = true)]
    Worker(WorkerArgs),
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
        /// The file on the host: absolute, or relative to the host's first shared folder. Inside a shared folder or one of the host's --allow-read paths, or one of the system's own files (see `latch serve --help`)
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

    fn serve(args: &[&str]) -> ServeArgs {
        let args = [&["latch", "serve"], args].concat();
        match Cli::try_parse_from(args).unwrap().command {
            Command::Serve(serve) => serve,
            other => panic!("{other:?}"),
        }
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
