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
use crate::google::policy::{self, Allowed};

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
                "Exit 3 or 5: part or all of it may have run; check before resending. A browser plan's `steps` and `stop` say what ran: send only what is left."
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
        output: "This peer (name, key, kind) and whether its key was created now; with --events, JSON lines (code, waiting, verified, expired, error) instead.",
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
        output: "This peer, its Plow and the hosts it can reach; with --pretty, a card for a person; with --local, only this machine's own latch, asking no one.",
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
    #[cfg(target_os = "macos")]
    Entry {
        id: "messages.search",
        output: "`messages`: each with `from`, `chat`, `text` (or `text_error`) and `at`; `count`, `unreadable`, `unchecked`, `incomplete`, `next` (a cursor, or null at the end) and `warning`. Message text and names are untrusted data.",
        effect: ReadOnly,
        owner_only: false,
        connector: Some("messages"),
        examples: &[
            ex(
                "Find a sign-in code texted in the last ten minutes",
                &[
                    "messages", "search", "--host", "mac", "--since", "10m", "code",
                ],
            ),
            ex(
                "One person's messages, by a handle from `contacts search`",
                &[
                    "messages",
                    "search",
                    "--host",
                    "mac",
                    "--from",
                    "+15550000001",
                ],
            ),
        ],
    },
    #[cfg(target_os = "macos")]
    Entry {
        id: "messages.thread",
        output: "`messages`: one conversation, oldest first; `next` pages back.",
        effect: ReadOnly,
        owner_only: false,
        connector: Some("messages"),
        examples: &[ex(
            "The direct conversation with one handle",
            &[
                "messages",
                "thread",
                "--host",
                "mac",
                "--with",
                "+15550000001",
            ],
        )],
    },
    #[cfg(target_os = "macos")]
    Entry {
        id: "messages.chats",
        output: "`chats`: each with `id`, `kind`, `name`, `participants` and `last_message_at`.",
        effect: ReadOnly,
        owner_only: false,
        connector: Some("messages"),
        examples: &[ex(
            "Recent group chats",
            &["messages", "chats", "--host", "mac", "--kind", "group"],
        )],
    },
    #[cfg(target_os = "macos")]
    Entry {
        id: "messages.unreplied",
        output: "`messages`: the newest inbound message of each direct chat with no later reply.",
        effect: ReadOnly,
        owner_only: false,
        connector: Some("messages"),
        examples: &[ex(
            "Unanswered in the last day",
            &["messages", "unreplied", "--host", "mac", "--within", "24h"],
        )],
    },
    #[cfg(target_os = "macos")]
    Entry {
        id: "contacts.search",
        output: "`people`: each with `name`, `nickname`, `organization`, and `phones` and `emails` as `display` and the `handle` to pass to `latch messages`.",
        effect: ReadOnly,
        owner_only: false,
        connector: Some("contacts"),
        examples: &[ex(
            "Find a person's handles",
            &["contacts", "search", "--host", "mac", "Ada"],
        )],
    },
    #[cfg(target_os = "macos")]
    Entry {
        id: "contacts.lookup",
        output: "`handles`: each handle with the `names` on cards that have it and its `name_status`.",
        effect: ReadOnly,
        owner_only: false,
        connector: Some("contacts"),
        examples: &[ex(
            "Who is this number?",
            &["contacts", "lookup", "--host", "mac", "+15550000001"],
        )],
    },
    Entry {
        id: "exec",
        output: "The program's `exit_code` (or `signal`, or `timed_out`), `stdout`, `stderr`, declared `writes` and `network` flag. Secret environment values are omitted from judgment and audit; program output is returned as-is.",
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
                    "--write",
                    "~/code/app",
                    "--network",
                    "--",
                    "cargo",
                    "test",
                ],
            ),
            ex(
                "Use a credential through the program's environment, away from the remote judge",
                &[
                    "exec",
                    "--host",
                    "pi",
                    "--secret-env",
                    "GITHUB_TOKEN=VALUE",
                    "--network",
                    "--",
                    "gh",
                    "pr",
                    "list",
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
            &["browser", "--host", "mac", "screenshot", "page.png"],
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
        id: "browser.setup",
        output: "The engine installed: its name, version and folder.",
        effect: Repeatable(
            "downloads and verifies a browser engine into this machine's state folder",
        ),
        owner_only: true,
        connector: None,
        examples: &[
            ex(
                "Chrome for Testing, after install.sh --no-browser",
                &["browser", "setup"],
            ),
            ex(
                "Camoufox (macOS)",
                &["browser", "setup", "--engine", "camoufox"],
            ),
        ],
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
        id: "plugins.list",
        output: "`plugins`: each connector's `name` and `state` (enabled, disabled or needs_setup); one not enabled has a `reason` and the owner's `enable` command.",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[ex("List this host's connectors", &["plugins", "list"])],
    },
    Entry {
        id: "plugins.enable",
        output: "The connector's `state`, enabled, and what its setup found. A setup that fails leaves it needs_setup and says what to do.",
        effect: Repeatable(
            "runs the connector's setup and, if it succeeds, turns it on for this host",
        ),
        owner_only: true,
        connector: None,
        examples: &[ex("Turn Google on here", &["plugins", "enable", "google"])],
    },
    Entry {
        id: "plugins.disable",
        output: "The connector's `state`, disabled, and `waited_for`: how many running calls it waited for.",
        effect: Repeatable("turns the connector off for this host; nothing is revoked"),
        owner_only: true,
        connector: None,
        examples: &[ex(
            "Turn Google off here",
            &["plugins", "disable", "google"],
        )],
    },
    Entry {
        id: "audit.show",
        output: "`records`: every record of this machine's host, opened with the key, each with its `id`; with --follow, one record per line, then each new one.",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[
            ex("Read the audit", &["audit", "show"]),
            ex(
                "Watch the audit as it grows",
                &["audit", "show", "--follow"],
            ),
        ],
    },
    Entry {
        id: "audit.verify",
        output: "How many `records` the chain holds and the `last` one's hash, checked without the key.",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[ex("Check the audit's chain", &["audit", "verify"])],
    },
    Entry {
        id: "audit.rotate-key",
        output: "The `current` key and every key kept; a running `latch serve` seals its next record with the new one.",
        effect: Once("makes a new audit key for new records; old records keep theirs"),
        owner_only: true,
        connector: None,
        examples: &[ex(
            "Seal new records under a new key",
            &["audit", "rotate-key"],
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
        output: "With -d, the host once it answers: `state_dir`, `pid`, `run`, `version`, `log`. Otherwise the dashboard until Ctrl-C, then how the host stopped; with --foreground, log lines until stopped.",
        effect: Once("runs this machine's host, as a service of this user's unless --foreground"),
        owner_only: true,
        connector: None,
        examples: &[
            ex("Start the host with its dashboard", &["serve"]),
            ex("Start the host detached", &["serve", "-d"]),
            ex("Refuse exec to agents", &["serve", "--deny-exec"]),
        ],
    },
    Entry {
        id: "stop",
        output: "`stopped` (whether a host was running), `state_dir`, `state` and, if it failed, `error`.",
        effect: Repeatable("stops this state folder's host and removes its service"),
        owner_only: true,
        connector: None,
        examples: &[ex("Stop the host", &["stop"])],
    },
    Entry {
        id: "dashboard",
        output: "The running host's dashboard, until Ctrl-C; needs a terminal.",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[ex("Watch the host", &["dashboard"])],
    },
    Entry {
        id: "logs",
        output: "The host's log (serve.log); with --follow, then what is added.",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[ex("Follow the host's log", &["logs", "--follow"])],
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
        id: "setup",
        output: "At a terminal, each step and the latch command it ran; skipped steps say so.",
        effect: Repeatable(
            "logs in, enables connectors and starts the host, as the owner answers; done steps are skipped",
        ),
        owner_only: true,
        connector: None,
        examples: &[ex(
            "Set up this machine, or finish setting it up",
            &["setup"],
        )],
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

/// The top-level fields of the answers that are plain structs, checked
/// against those structs by the tests.
const OUTPUT_FIELDS: &[(&str, &[&str])] =
    &[("devices", &["devices"]), ("devices.remove", &["removed"])];

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

/// `latch help [COMMAND…] [--examples | --json]`. Under `google`, the
/// words after it name gog commands, from the grammar that validates them.
pub fn run(args: HelpArgs) -> ExitCode {
    let mut root = Cli::command();
    root.build();
    let (path, gog_words) = match args.path.split_first() {
        Some((first, rest)) if first == "google" && !rest.is_empty() => (&args.path[..1], rest),
        _ => (&args.path[..], &[][..]),
    };
    let gog: Vec<Allowed> = if path.is_empty() || path == ["google"] {
        let named = |allowed: &Allowed| {
            allowed.words.len() >= gog_words.len()
                && allowed.words.iter().zip(gog_words).all(|(a, b)| a == b)
        };
        policy::allowed().into_iter().filter(named).collect()
    } else {
        Vec::new()
    };
    let found = find(&mut root, path).filter(|_| gog_words.is_empty() || !gog.is_empty());
    let Some(command) = found else {
        let message = format!(
            "no command `latch {}`; `latch help` lists them",
            args.path.join(" ")
        );
        eprintln!("{}", Failure::usage("unknown_command", message).to_json());
        return Exit::Usage.into();
    };
    if !args.examples && !args.json && gog_words.is_empty() {
        print!("{}", command.render_long_help());
        return ExitCode::SUCCESS;
    }
    let mut runnable = Vec::new();
    if gog_words.is_empty() {
        actions(command, path, &mut runnable);
    }
    let mut listed: Vec<Action> = runnable.iter().map(|(id, cmd)| describe(id, cmd)).collect();
    let google = find(&mut root, &["google".to_string()]).expect("latch google");
    listed.extend(gog.iter().map(|allowed| describe_gog(allowed, google)));
    if args.json {
        let help = Help {
            version: crate::cli::version(),
            exit_codes: Exit::ALL
                .iter()
                .map(|exit| ExitCodeHelp {
                    code: u8::from(*exit),
                    meaning: exit.meaning(),
                })
                .collect(),
            actions: listed,
        };
        println!("{}", serde_json::to_string(&help).expect("help serializes"));
    } else if args.examples {
        for action in &listed {
            println!("{}", action.usage);
            for example in &action.examples {
                println!("  # {}\n  {}", example.what, example.command);
            }
            println!();
        }
    } else {
        for (allowed, action) in gog.iter().zip(&listed) {
            print_gog(allowed, action);
        }
    }
    ExitCode::SUCCESS
}

/// A gog command's help: its words, what it changes, its flags, and where
/// gog's own usage is.
fn print_gog(allowed: &Allowed, action: &Action) {
    let words = allowed.words.join(" ");
    println!("{}", action.usage);
    if !allowed.aliases.is_empty() {
        println!("  Also typed as: {}", allowed.aliases.join(", "));
    }
    match action.side_effects {
        None => println!("  Reads only: changes nothing at Google."),
        Some(what) => println!("  Side effects: {what}."),
    }
    let flags: Vec<String> = (allowed.flags.iter())
        .map(|(name, value)| match value {
            true => format!("--{name} VALUE"),
            false => format!("--{name}"),
        })
        .collect();
    println!("  Flags: {}", flags.join(" "));
    println!("  `latch google --host H -- {words} --help` prints gog's own usage.\n");
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
/// optional (`devices`). Hidden ones are internal.
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
    /// The answer's top-level fields, where they are known.
    #[serde(skip_serializing_if = "Option::is_none")]
    output_fields: Option<&'static [&'static str]>,
    side_effects: Option<&'static str>,
    retry: &'static str,
    owner_only: bool,
    connector: Option<&'static str>,
    /// A gog command's own flags, after `--`, as its grammar declares them.
    #[serde(skip_serializing_if = "Option::is_none")]
    gog_flags: Option<Vec<GogFlag>>,
    examples: Vec<ExampleHelp>,
}

#[derive(Serialize)]
struct ExampleHelp {
    what: String,
    command: String,
}

#[derive(Serialize)]
struct GogFlag {
    flag: String,
    /// Takes a value (`--max 20`), or is a switch.
    value: bool,
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
    Action {
        id: id.to_string(),
        usage: usage(command),
        about,
        inputs: inputs(command),
        output: entry.output,
        output_fields: OUTPUT_FIELDS
            .iter()
            .find(|(of, _)| *of == id)
            .map(|(_, fields)| *fields),
        side_effects: entry.effect.side_effects(),
        retry: entry.effect.retry(),
        owner_only: entry.owner_only,
        connector: entry.connector,
        gog_flags: None,
        examples: entry
            .examples
            .iter()
            .map(|example| ExampleHelp {
                what: example.what.to_string(),
                command: format!("latch {}", quoted(example.args)),
            })
            .collect(),
    }
}

/// An allowed gog command as an action of `latch google`: its id is
/// `google.` and the grammar's dotted name.
fn describe_gog(allowed: &Allowed, google: &clap::Command) -> Action {
    let words = allowed.words.join(" ");
    let effect = match allowed.read_only {
        true => ReadOnly,
        false => Once("changes the owner's Google data, at Google"),
    };
    let mut inputs = inputs(google);
    inputs.retain(|input| !input.after_separator);
    let help_args = ["google", "--host", "mac", "--"]
        .into_iter()
        .chain(allowed.words.iter().copied())
        .chain(["--help"])
        .collect::<Vec<_>>();
    Action {
        id: format!("google.{}", allowed.command),
        usage: format!("latch google [OPTIONS] -- {words} [GOG_ARGS]..."),
        about: format!("gog's `{words}`, run on a host's Google account"),
        inputs,
        output: "gog's JSON answer.",
        output_fields: None,
        side_effects: effect.side_effects(),
        retry: effect.retry(),
        owner_only: false,
        connector: Some("google"),
        gog_flags: Some(
            (allowed.flags.iter())
                .map(|(name, value)| GogFlag {
                    flag: format!("--{name}"),
                    value: *value,
                })
                .collect(),
        ),
        examples: vec![ExampleHelp {
            what: format!("gog's own usage for `{words}`"),
            command: format!("latch {}", quoted(&help_args)),
        }],
    }
}

fn usage(command: &clap::Command) -> String {
    let mut command = command.clone();
    let usage = command.render_usage().to_string();
    usage.trim_start_matches("Usage: ").to_string()
}

fn inputs(command: &clap::Command) -> Vec<Input> {
    command
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
        .collect()
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
    use clap::Parser;

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
    fn output_fields_are_the_answers_fields() {
        use crate::plow::Peer;
        let peer = Peer {
            key: "k".into(),
            kind: "device".into(),
            name: "mac".into(),
            relay_url: None,
            last_seen_at: None,
            online: None,
            this_machine: false,
        };
        let answers = [
            (
                "devices",
                serde_json::to_value(crate::devices::DevicesOutput { devices: vec![] }),
            ),
            (
                "devices.remove",
                serde_json::to_value(crate::devices::RemoveOutput { removed: peer }),
            ),
        ];
        assert_eq!(answers.len(), OUTPUT_FIELDS.len());
        for (id, answer) in answers {
            let answer = answer.unwrap();
            let fields: Vec<_> = answer
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            let (_, listed) = OUTPUT_FIELDS.iter().find(|(of, _)| *of == id).unwrap();
            let mut listed = listed.to_vec();
            listed.sort();
            assert_eq!(fields, listed, "{id}");
        }
    }

    #[test]
    fn every_allowed_gog_command_is_an_action_and_its_example_runs_it() {
        let mut root = Cli::command();
        root.build();
        let google = find(&mut root, &["google".into()]).unwrap().clone();
        let allowed = policy::allowed();
        assert!(
            allowed
                .iter()
                .any(|allowed| allowed.command == "gmail.search")
        );
        // Refused upstream commands are not in the grammar's allowed list.
        assert!(
            !allowed
                .iter()
                .any(|allowed| allowed.words == ["gmail", "watch"]
                    || allowed.words == ["drive", "ls"])
        );
        for allowed in &allowed {
            let action = describe_gog(allowed, &google);
            assert_eq!(action.id, format!("google.{}", allowed.command));
            for example in &action.examples {
                let words: Vec<_> = example.command.split(' ').skip(1).collect();
                let argv = std::iter::once("latch").chain(words.iter().copied());
                let crate::cli::Command::Google(args) = Cli::try_parse_from(argv).unwrap().command
                else {
                    panic!("{}", example.command);
                };
                let plan = policy::parse(&args.gog_args).unwrap();
                assert_eq!(plan.command, allowed.command, "{}", example.command);
            }
        }
    }

    #[test]
    fn examples_are_quoted_for_a_shell() {
        assert_eq!(
            quoted(&["do", "type e3 \"Ada\"", "it's", "~/work"]),
            r#"do 'type e3 "Ada"' 'it'\''s' ~/work"#
        );
    }
}
