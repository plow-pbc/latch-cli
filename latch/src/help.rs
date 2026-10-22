//! `latch help` and `latch skill`: help an agent can learn from a layer at a
//! time. `latch help COMMAND` is the command's `--help`; `--examples` shows
//! how to call it; `--json` describes every command for a program.
//!
//! The parser in `cli.rs` is the registry: each command's inputs, and the
//! text `--help` prints, are read from the same clap definitions that
//! validate a command line, so they cannot disagree. What clap does not hold
//! (examples, output, side effects) is in `ENTRIES`, one per command, and
//! the tests below fail when an entry and the parser part ways.

use std::process::ExitCode;

use clap::{ArgAction, CommandFactory};
use serde::Serialize;

use crate::cli::{Cli, HelpArgs};
use crate::failure::{Exit, Failure};

/// What latch knows about a command beyond what its parser says.
struct Entry {
    /// The command's words joined by `.` (`browser.tab.new`): its stable action id.
    id: &'static str,
    /// What it prints on success.
    output: &'static str,
    effect: Effect,
    /// Refused to an agent: only the owner's own machines may run it.
    owner_only: bool,
    /// The connector it belongs to, which may be turned off on a host.
    connector: Option<&'static str>,
    examples: &'static [Example],
}

/// What running a command changes, which decides whether to resend it.
enum Effect {
    /// Changes nothing.
    ReadOnly,
    /// Changes this, and running it again gives the same result.
    Repeatable(&'static str),
    /// Changes this, and running it again does it again (or is refused).
    Once(&'static str),
}

impl Effect {
    fn side_effects(&self) -> Option<&'static str> {
        match self {
            Effect::ReadOnly => None,
            Effect::Repeatable(what) | Effect::Once(what) => Some(what),
        }
    }

    fn retry(&self) -> &'static str {
        match self {
            Effect::ReadOnly => "Changes nothing: resend it after exit 3 or 5.",
            Effect::Repeatable(_) => {
                "Resending gives the same result: resend it after exit 3 or 5."
            }
            Effect::Once(_) => {
                "Exit 3: nothing was sent, resend it. Exit 5: it may have happened; check before resending."
            }
        }
    }
}

struct Example {
    what: &'static str,
    /// The words after `latch`.
    args: &'static [&'static str],
}

const fn ex(what: &'static str, args: &'static [&'static str]) -> Example {
    Example { what, args }
}

use Effect::{Once, ReadOnly, Repeatable};

const ENTRIES: &[Entry] = &[
    Entry {
        id: "login",
        output: "This peer (name, key, kind) and whether its key was created now.",
        effect: Repeatable("creates this peer's key if missing and registers it with Plow"),
        owner_only: false,
        connector: None,
        examples: &[
            ex("An agent: log in with its own Plow credential", &["login"]),
            ex(
                "An owner's machine, with a token file",
                &["login", "--token-file", "plow.token", "--name", "mac"],
            ),
        ],
    },
    Entry {
        id: "devices",
        output: "`devices`: the account's machines, online or not.",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[ex("List the machines", &["devices"])],
    },
    Entry {
        id: "devices.remove",
        output: "`removed`: the machine removed.",
        effect: Once("removes the machine from Plow and signs out the login it registered with"),
        owner_only: true,
        connector: None,
        examples: &[ex(
            "Remove the old Pi without a prompt",
            &["devices", "remove", "old-pi", "--yes"],
        )],
    },
    Entry {
        id: "status",
        output: "This peer, its Plow and the hosts it can reach; a card with --pretty.",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[
            ex("Which hosts can I reach?", &["status"]),
            ex("This machine's own latch, offline", &["status", "--local"]),
        ],
    },
    Entry {
        id: "file.get",
        output: "Where the file was written, its size and sha256.",
        effect: Repeatable("writes the file on this machine"),
        owner_only: false,
        connector: None,
        examples: &[ex(
            "Fetch a build log from the Pi",
            &["file", "get", "--host", "pi", "build/log.txt", "./log.txt"],
        )],
    },
    Entry {
        id: "file.put",
        output: "Where the file was written on the host, its size and sha256.",
        effect: Repeatable("writes the file on the host, replacing one of the same name"),
        owner_only: false,
        connector: None,
        examples: &[ex(
            "Send a script into the Pi's shared folder",
            &["file", "put", "--host", "pi", "./check.sh", "scripts/"],
        )],
    },
    Entry {
        id: "google",
        output: "gog's JSON answer.",
        effect: Once(
            "whatever the gog command does: a search changes nothing; a send, an invite or an edit happens at Google",
        ),
        owner_only: false,
        connector: Some("google"),
        examples: &[
            ex(
                "Search one account's mail",
                &[
                    "google",
                    "--host",
                    "mac",
                    "--account",
                    "ada@example.com",
                    "--",
                    "gmail",
                    "search",
                    "from:grace newer_than:7d",
                ],
            ),
            ex("What gog commands are allowed", &["google", "--", "help"]),
        ],
    },
    Entry {
        id: "exec",
        output: "The program's `exit_code` (or `signal`, or `timed_out`), `stdout` and `stderr`.",
        effect: Once(
            "runs the program on the host, inside its sandbox; its effects are the program's",
        ),
        owner_only: false,
        connector: None,
        examples: &[
            ex(
                "Run a shell line on the Pi",
                &["exec", "--host", "pi", "--", "sh", "-c", "uname -a; ls"],
            ),
            ex(
                "Run the tests, for up to ten minutes",
                &[
                    "exec",
                    "--host",
                    "pi",
                    "--timeout",
                    "600",
                    "--",
                    "cargo",
                    "test",
                ],
            ),
        ],
    },
    Entry {
        id: "browser.open",
        output: "The new session's id (shown only now: keep it) and the elements on screen.",
        effect: Once("starts a browser session on the host and loads the page"),
        owner_only: false,
        connector: None,
        examples: &[
            ex(
                "Open a page in a new session",
                &["browser", "--host", "mac", "open", "https://shop.example/"],
            ),
            ex(
                "On the shop profile, holding the site for this session",
                &[
                    "browser",
                    "--host",
                    "mac",
                    "open",
                    "--profile",
                    "shop",
                    "--task",
                    "basket",
                    "--reserve-site",
                    "https://shop.example/",
                ],
            ),
        ],
    },
    Entry {
        id: "browser.observe",
        output: "A snapshot: refs (e1, e2, …) for the elements, the page's first text and the tabs.",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[
            ex("See the page", &["browser", "--host", "mac", "observe"]),
            ex(
                "Every element, off-screen ones too",
                &["browser", "--host", "mac", "observe", "--all"],
            ),
        ],
    },
    Entry {
        id: "browser.text",
        output: "The page's text, at most 20,000 characters (`truncated` when cut); with --raw, the text alone.",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[ex("Read the page", &["browser", "--host", "mac", "text"])],
    },
    Entry {
        id: "browser.screenshot",
        output: "Where the PNG was written.",
        effect: Repeatable("writes the PNG on this machine"),
        owner_only: false,
        connector: None,
        examples: &[ex(
            "Capture the page",
            &[
                "browser",
                "--host",
                "mac",
                "screenshot",
                "--output",
                "page.png",
            ],
        )],
    },
    Entry {
        id: "browser.do",
        output: "Each step's outcome (`steps`) and, if the plan stopped, its `stop`: step, code and message.",
        effect: Once("acts on the page: clicks, typing, choices, fills"),
        owner_only: false,
        connector: None,
        examples: &[
            ex(
                "Type, then click and check where it led",
                &[
                    "browser",
                    "--host",
                    "mac",
                    "do",
                    "type e3 \"Ada Lovelace\"",
                    "click e7 -> path: /cart",
                ],
            ),
            ex(
                "A goal in words",
                &["browser", "--host", "mac", "do", "search for \"blue mug\""],
            ),
            ex(
                "What a request that exited 5 did",
                &[
                    "browser",
                    "--host",
                    "mac",
                    "do",
                    "--history",
                    "r3f09a1c2b7e4",
                ],
            ),
        ],
    },
    Entry {
        id: "browser.act",
        output: "The step's outcome, as a plan of one step.",
        effect: Once("one click, keypress, choice, scroll or Back on the page"),
        owner_only: false,
        connector: None,
        examples: &[
            ex(
                "Click and check the result",
                &[
                    "browser",
                    "--host",
                    "mac",
                    "act",
                    "click",
                    "e7",
                    "--check",
                    "text: Added to cart",
                ],
            ),
            ex(
                "Scroll down",
                &["browser", "--host", "mac", "act", "scroll", "down"],
            ),
        ],
    },
    Entry {
        id: "browser.handoff",
        output: "How the owner handed the session back.",
        effect: Once("hands the session's window to the owner until they hand it back"),
        owner_only: false,
        connector: None,
        examples: &[
            ex(
                "Ask the owner to solve a CAPTCHA",
                &["browser", "--host", "mac", "handoff", "--reason", "captcha"],
            ),
            ex(
                "On the host: hand the session back",
                &["browser", "handoff", "done"],
            ),
        ],
    },
    Entry {
        id: "browser.find",
        output: "Candidate refs; nothing is done to them.",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[ex(
            "Find the download button",
            &[
                "browser", "--host", "mac", "find", "--role", "button", "--name", "Download",
            ],
        )],
    },
    Entry {
        id: "browser.wait",
        output: "Whether the condition held before the deadline.",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[
            ex(
                "Wait for a confirmation",
                &["browser", "--host", "mac", "wait", "--text", "Saved"],
            ),
            ex(
                "Wait for a spinner to go",
                &[
                    "browser", "--host", "mac", "wait", "--ref", "e4", "--state", "hidden",
                ],
            ),
        ],
    },
    Entry {
        id: "browser.goto",
        output: "The page's elements.",
        effect: Repeatable("loads the page in the active tab"),
        owner_only: false,
        connector: None,
        examples: &[ex(
            "Load the next page",
            &[
                "browser",
                "--host",
                "mac",
                "goto",
                "https://shop.example/cart",
            ],
        )],
    },
    Entry {
        id: "browser.tab.new",
        output: "The new tab and its elements.",
        effect: Once("opens a tab"),
        owner_only: false,
        connector: None,
        examples: &[ex(
            "Open a page in a new tab",
            &[
                "browser",
                "--host",
                "mac",
                "tab",
                "new",
                "https://docs.example/",
            ],
        )],
    },
    Entry {
        id: "browser.tab.switch",
        output: "The tab, now active.",
        effect: Repeatable("makes the tab the active one"),
        owner_only: false,
        connector: None,
        examples: &[ex(
            "Go back to the first tab",
            &["browser", "--host", "mac", "tab", "switch", "t1"],
        )],
    },
    Entry {
        id: "browser.tab.close",
        output: "The tabs left.",
        effect: Once("closes a tab"),
        owner_only: false,
        connector: None,
        examples: &[ex(
            "Close the second tab",
            &["browser", "--host", "mac", "tab", "close", "t2"],
        )],
    },
    Entry {
        id: "browser.dialog",
        output: "The page after the dialog.",
        effect: Once("answers the page's open dialog"),
        owner_only: false,
        connector: None,
        examples: &[ex(
            "Accept a confirm dialog",
            &["browser", "--host", "mac", "dialog", "accept"],
        )],
    },
    Entry {
        id: "browser.eval",
        output: "The script's JSON value.",
        effect: Once("runs the script in the page"),
        owner_only: false,
        connector: None,
        examples: &[ex(
            "Run a check script",
            &[
                "browser",
                "--host",
                "mac",
                "eval",
                "--script-file",
                "check.js",
            ],
        )],
    },
    Entry {
        id: "browser.upload",
        output: "The file's name, size and sha256.",
        effect: Once("puts the file into the page's file input"),
        owner_only: false,
        connector: None,
        examples: &[ex(
            "Attach an invoice",
            &["browser", "--host", "mac", "upload", "e4", "./invoice.pdf"],
        )],
    },
    Entry {
        id: "browser.downloads",
        output: "The session's downloads, as handles (d1, d2, …).",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[ex(
            "List the downloads",
            &["browser", "--host", "mac", "downloads"],
        )],
    },
    Entry {
        id: "browser.download.get",
        output: "Where the file was written, its size and sha256.",
        effect: Repeatable("writes the file on this machine"),
        owner_only: false,
        connector: None,
        examples: &[ex(
            "Bring a report here",
            &[
                "browser",
                "--host",
                "mac",
                "download",
                "get",
                "d1",
                "--output",
                "report.csv",
            ],
        )],
    },
    Entry {
        id: "browser.fill",
        output: "What was filled (never a value), or a confirmation to carry out with `do --confirm`.",
        effect: Once("fills the page from a vault item, and presses the button named"),
        owner_only: false,
        connector: None,
        examples: &[
            ex(
                "Sign in with a vault login",
                &[
                    "browser",
                    "--host",
                    "mac",
                    "fill",
                    "--item",
                    "v1",
                    "--field",
                    "username=e4",
                    "--field",
                    "password=e5",
                    "--press",
                    "e6",
                ],
            ),
            ex(
                "Fill as observe suggested",
                &[
                    "browser",
                    "--host",
                    "mac",
                    "fill",
                    "--item",
                    "v1",
                    "--suggested",
                    "s3f2a1c09bd",
                ],
            ),
        ],
    },
    Entry {
        id: "browser.sessions",
        output: "Your open sessions, without their ids.",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[ex(
            "List your sessions",
            &["browser", "--host", "mac", "sessions"],
        )],
    },
    Entry {
        id: "browser.close",
        output: "The session closed.",
        effect: Repeatable("ends the session; its profile stays"),
        owner_only: false,
        connector: None,
        examples: &[ex(
            "End this session",
            &["browser", "--host", "mac", "close"],
        )],
    },
    Entry {
        id: "browser.profile.create",
        output: "The profile made, and its engine.",
        effect: Once("makes a browser profile in this machine's state folder"),
        owner_only: true,
        connector: None,
        examples: &[ex(
            "A profile on Camoufox",
            &[
                "browser", "profile", "create", "shop", "--engine", "camoufox",
            ],
        )],
    },
    Entry {
        id: "vault.list",
        output: "Every item's metadata: id, kind, title, sites, revision; never a value.",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[ex("List the items", &["vault", "--host", "mac", "list"])],
    },
    Entry {
        id: "vault.add",
        output: "The item added.",
        effect: Once("adds an item to the host's vault"),
        owner_only: true,
        connector: None,
        examples: &[ex(
            "Add a login; its password is asked for",
            &[
                "vault",
                "--host",
                "mac",
                "add",
                "--kind",
                "login",
                "--title",
                "Shop",
                "--site",
                "shop.example",
                "--field",
                "username=ada",
            ],
        )],
    },
    Entry {
        id: "vault.edit",
        output: "The item at its new revision.",
        effect: Once("changes the item"),
        owner_only: true,
        connector: None,
        examples: &[ex(
            "Rename an item",
            &[
                "vault",
                "--host",
                "mac",
                "edit",
                "v1",
                "--revision",
                "2",
                "--title",
                "Shop (old)",
            ],
        )],
    },
    Entry {
        id: "vault.link",
        output: "A one-time link (and a QR code on a terminal) for 15 minutes.",
        effect: Once("makes a one-time link that adds an item from a phone"),
        owner_only: true,
        connector: None,
        examples: &[ex(
            "Add a card from the phone",
            &[
                "vault",
                "--host",
                "mac",
                "link",
                "--kind",
                "card",
                "--title",
                "Visa",
                "--page",
                "https://phone.example/latch",
            ],
        )],
    },
    Entry {
        id: "vault.remove",
        output: "The item removed.",
        effect: Once("removes the item"),
        owner_only: true,
        connector: None,
        examples: &[ex(
            "Remove an item",
            &["vault", "--host", "mac", "remove", "v3", "--revision", "1"],
        )],
    },
    Entry {
        id: "vault.import",
        output: "The items imported.",
        effect: Once("adds the rows picked at the prompt"),
        owner_only: true,
        connector: None,
        examples: &[ex(
            "Import a 1Password export",
            &["vault", "--host", "mac", "import", "export.1pux"],
        )],
    },
    Entry {
        id: "rules.set",
        output: "The rules saved, with their revision.",
        effect: Repeatable("replaces the rules"),
        owner_only: true,
        connector: None,
        examples: &[ex(
            "Let agents read mail, never send it",
            &[
                "rules",
                "set",
                "Agents may search my mail but never send any.",
            ],
        )],
    },
    Entry {
        id: "rules.edit",
        output: "The rules saved, with their revision.",
        effect: Once("changes the rules"),
        owner_only: true,
        connector: None,
        examples: &[ex(
            "Edit one agent's rules",
            &["rules", "--from", "ada", "edit"],
        )],
    },
    Entry {
        id: "rules.clear",
        output: "The rules removed.",
        effect: Repeatable("removes the rules"),
        owner_only: true,
        connector: None,
        examples: &[ex("Remove every agent's rules", &["rules", "clear"])],
    },
    Entry {
        id: "rules.show",
        output: "The rules for every agent and each agent's own, with their revisions.",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[ex("Show the rules", &["rules", "show"])],
    },
    Entry {
        id: "shares",
        output: "`shares` and `allow_read`: what `latch serve` shares when named none.",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[ex("Show the saved shares", &["shares"])],
    },
    Entry {
        id: "shares.set",
        output: "`shares` and `allow_read`, as saved.",
        effect: Repeatable("replaces the saved folders and --allow-read paths"),
        owner_only: true,
        connector: None,
        examples: &[ex(
            "Share ~/work, and let callers read the Rust toolchain",
            &["shares", "set", "~/work", "--allow-read", "~/.rustup"],
        )],
    },
    Entry {
        id: "events",
        output: "JSON lines: a snapshot of the host, then with --follow each change.",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[ex("Watch this machine's host", &["events", "--follow"])],
    },
    Entry {
        id: "serve",
        output: "Nothing until stopped; the dashboard or log lines meanwhile.",
        effect: Once("runs this machine as a host until stopped"),
        owner_only: true,
        connector: None,
        examples: &[
            ex("Share ~/work", &["serve", "~/work"]),
            ex(
                "Share ~/work, with the Rust toolchain readable and exec refused to agents",
                &[
                    "serve",
                    "~/work",
                    "--allow-read",
                    "~/.rustup",
                    "--allow-read",
                    "~/.cargo",
                    "--deny-exec",
                ],
            ),
        ],
    },
    Entry {
        id: "help",
        output: "Help text; with --json, every command described as JSON.",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[
            ex("Help for one command", &["help", "browser", "open"]),
            ex(
                "How to call the browser commands",
                &["help", "browser", "--examples"],
            ),
            ex("Every command, for a program", &["help", "--json"]),
        ],
    },
    Entry {
        id: "skill",
        output: "SKILL.md, for this version of latch.",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[ex("Learn how to use latch", &["skill"])],
    },
];

/// The guide for agents, versioned with the binary.
const SKILL: &str = include_str!("../SKILL.md");

/// `latch skill`: SKILL.md with this build's version at the end of its
/// front matter.
pub fn skill() -> String {
    let version = serde_json::to_string(crate::cli::version()).expect("a string serializes");
    let (front, body) = SKILL
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .expect("SKILL.md starts with its front matter");
    format!("---\n{front}\nmetadata:\n  version: {version}\n---\n{body}")
}

/// `latch help [COMMAND…] [--examples | --json]`.
pub fn run(args: HelpArgs) -> ExitCode {
    let mut root = Cli::command();
    root.build();
    let Some(command) = find(&mut root, &args.path) else {
        let message = format!(
            "no command `latch {}`; `latch help` lists them",
            args.path.join(" ")
        );
        eprintln!("{}", Failure::usage("unknown_command", message).to_json());
        return Exit::Usage.into();
    };
    if !args.examples && !args.json {
        print!("{}", command.render_long_help());
        return ExitCode::SUCCESS;
    }
    let mut runnable = Vec::new();
    actions(command, &args.path, &mut runnable);
    if args.json {
        let actions = runnable.iter().map(|(id, cmd)| describe(id, cmd)).collect();
        let help = Help {
            version: crate::cli::version(),
            exit_codes: Exit::ALL
                .iter()
                .map(|exit| ExitCodeHelp {
                    code: u8::from(*exit),
                    meaning: exit.meaning(),
                })
                .collect(),
            actions,
        };
        println!("{}", serde_json::to_string(&help).expect("help serializes"));
    } else {
        for (id, _) in &runnable {
            println!("latch {}", id.replace('.', " "));
            for example in entry(id).examples {
                println!("  # {}\n  latch {}", example.what, quoted(example.args));
            }
            println!();
        }
    }
    ExitCode::SUCCESS
}

/// The subcommand `path` names, from the root.
fn find<'a>(root: &'a mut clap::Command, path: &[String]) -> Option<&'a mut clap::Command> {
    path.iter().try_fold(root, |command, word| {
        command
            .find_subcommand_mut(word)
            .filter(|found| !found.is_hide_set())
    })
}

/// Every command under `command` (itself included) that runs on its own,
/// with its id: a command with no subcommands, or whose subcommand is
/// optional (`devices`, `shares`). Hidden ones are internal.
fn actions<'a>(
    command: &'a clap::Command,
    path: &[String],
    out: &mut Vec<(String, &'a clap::Command)>,
) {
    let runs = !command.has_subcommands() || !command.is_subcommand_required_set();
    if !path.is_empty() && runs {
        out.push((path.join("."), command));
    }
    for sub in command.get_subcommands().filter(|sub| !sub.is_hide_set()) {
        let mut path = path.to_vec();
        path.push(sub.get_name().to_string());
        actions(sub, &path, out);
    }
}

fn entry(id: &str) -> &'static Entry {
    ENTRIES
        .iter()
        .find(|entry| entry.id == id)
        .unwrap_or_else(|| panic!("no help entry for `{id}`"))
}

#[derive(Serialize)]
struct Help {
    version: &'static str,
    exit_codes: Vec<ExitCodeHelp>,
    actions: Vec<Action>,
}

#[derive(Serialize)]
struct ExitCodeHelp {
    code: u8,
    meaning: &'static str,
}

#[derive(Serialize)]
struct Action {
    id: String,
    usage: String,
    about: String,
    inputs: Vec<Input>,
    output: &'static str,
    side_effects: Option<&'static str>,
    retry: &'static str,
    owner_only: bool,
    connector: Option<&'static str>,
    examples: Vec<String>,
}

#[derive(Serialize)]
struct Input {
    name: String,
    /// `-s, --long`, `--long` or `-s`; none for a positional input.
    flag: Option<String>,
    required: bool,
    repeatable: bool,
    /// Taken only after `--`.
    after_separator: bool,
    /// What it takes (`FILE`), none for a switch.
    value: Option<String>,
    choices: Vec<String>,
    default: Option<String>,
    env: Option<String>,
    about: String,
}

fn describe(id: &str, command: &clap::Command) -> Action {
    let entry = entry(id);
    let about = command
        .get_long_about()
        .or(command.get_about())
        .map(ToString::to_string)
        .unwrap_or_default();
    let inputs = command
        .get_arguments()
        .filter(|arg| {
            !arg.is_hide_set() && !matches!(arg.get_action(), ArgAction::Help | ArgAction::Version)
        })
        .map(|arg| {
            let takes_value = arg.get_action().takes_values();
            let short = arg.get_short().map(|short| format!("-{short}"));
            let long = arg.get_long().map(|long| format!("--{long}"));
            let flag = match (short, long) {
                (Some(short), Some(long)) => Some(format!("{short}, {long}")),
                (short, long) => long.or(short),
            };
            let value = takes_value.then(|| match arg.get_value_names() {
                Some(names) => names
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" "),
                None => arg.get_id().as_str().to_uppercase(),
            });
            let default = arg.get_default_values();
            Input {
                name: arg.get_id().to_string(),
                flag,
                required: arg.is_required_set(),
                repeatable: matches!(arg.get_action(), ArgAction::Append | ArgAction::Count)
                    || arg
                        .get_num_args()
                        .is_some_and(|range| range.max_values() > 1),
                after_separator: arg.is_last_set(),
                value,
                choices: arg
                    .get_possible_values()
                    .iter()
                    .filter(|choice| !choice.is_hide_set())
                    .map(|choice| choice.get_name().to_string())
                    .collect(),
                default: (takes_value && !default.is_empty()).then(|| {
                    default
                        .iter()
                        .map(|value| value.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(",")
                }),
                env: arg.get_env().map(|env| env.to_string_lossy().into_owned()),
                about: arg
                    .get_long_help()
                    .or(arg.get_help())
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            }
        })
        .collect();
    let mut usage = command.clone();
    Action {
        id: id.to_string(),
        usage: usage
            .render_usage()
            .to_string()
            .trim_start_matches("Usage: ")
            .to_string(),
        about,
        inputs,
        output: entry.output,
        side_effects: entry.effect.side_effects(),
        retry: entry.effect.retry(),
        owner_only: entry.owner_only,
        connector: entry.connector,
        examples: entry
            .examples
            .iter()
            .map(|example| format!("latch {}", quoted(example.args)))
            .collect(),
    }
}

/// `args` as a shell would need them typed: a word with anything but plain
/// characters goes in single quotes.
fn quoted(args: &[&str]) -> String {
    args.iter()
        .map(|arg| {
            let plain = !arg.is_empty()
                && arg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:,@+~%".contains(c));
            if plain {
                arg.to_string()
            } else {
                format!("'{}'", arg.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runnable() -> Vec<String> {
        let mut root = Cli::command();
        root.build();
        let mut out = Vec::new();
        actions(&root, &[], &mut out);
        out.into_iter().map(|(id, _)| id).collect()
    }

    /// The words of the command a parsed line ran, joined like an id.
    fn ran(matches: &clap::ArgMatches) -> String {
        let mut words = Vec::new();
        let mut matches = matches;
        while let Some((name, sub)) = matches.subcommand() {
            words.push(name.to_string());
            matches = sub;
        }
        words.join(".")
    }

    #[test]
    fn the_parser_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_command_has_exactly_one_entry_and_every_entry_a_command() {
        let runnable = runnable();
        let missing: Vec<_> = runnable
            .iter()
            .filter(|id| !ENTRIES.iter().any(|entry| entry.id == id.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "commands with no help entry: {missing:?}"
        );
        for entry in ENTRIES {
            let count = runnable.iter().filter(|id| *id == entry.id).count();
            assert_eq!(count, 1, "help entry `{}` names no command", entry.id);
            assert_eq!(
                ENTRIES.iter().filter(|other| other.id == entry.id).count(),
                1,
                "two help entries for `{}`",
                entry.id
            );
        }
    }

    #[test]
    fn every_example_parses_as_the_command_it_is_listed_under() {
        for entry in ENTRIES {
            assert!(!entry.examples.is_empty(), "`{}` has no example", entry.id);
            for example in entry.examples {
                let argv = std::iter::once("latch").chain(example.args.iter().copied());
                let matches = Cli::command()
                    .try_get_matches_from(argv)
                    .unwrap_or_else(|err| panic!("`{}`: {err}", quoted(example.args)));
                assert_eq!(ran(&matches), entry.id, "{}", quoted(example.args));
            }
        }
    }

    #[test]
    fn json_inputs_are_the_parsers_arguments() {
        let mut root = Cli::command();
        root.build();
        let open = find(&mut root, &["browser".into(), "open".into()]).unwrap();
        let action = describe("browser.open", open);
        let names: Vec<_> = action
            .inputs
            .iter()
            .map(|input| input.name.as_str())
            .collect();
        for name in [
            "profile",
            "task",
            "reserve_site",
            "url",
            "host",
            "session",
            "state_dir",
        ] {
            assert!(names.contains(&name), "{name} missing from {names:?}");
        }
        let url = action
            .inputs
            .iter()
            .find(|input| input.name == "url")
            .unwrap();
        assert!(url.flag.is_none() && !url.required);
        let host = action
            .inputs
            .iter()
            .find(|input| input.name == "host")
            .unwrap();
        assert_eq!(host.env.as_deref(), Some("LATCH_HOST"));
    }

    #[test]
    fn examples_are_quoted_for_a_shell() {
        assert_eq!(
            quoted(&["do", "type e3 \"Ada\"", "it's", "~/work"]),
            r#"do 'type e3 "Ada"' 'it'\''s' ~/work"#
        );
    }
}
