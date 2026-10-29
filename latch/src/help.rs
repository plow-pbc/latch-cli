//! `latch help` and `latch skill`: help an agent can learn from a layer at a
//! time. `latch help COMMAND` is the command's `--help`; `--examples` shows
//! how to call it; `--json` describes every command for a program.
//!
//! The parser in `cli.rs` is the registry: each command's inputs, and the
//! text `--help` prints, are read from the same clap definitions that
//! validate a command line, so they cannot disagree. What clap does not hold
//! (execution location, examples, output, side effects) is in `ENTRIES`, one
//! per command; the tests below fail when an entry and the parser part ways.

use std::process::ExitCode;

use clap::{ArgAction, ArgMatches, CommandFactory, FromArgMatches};
use serde::Serialize;

use crate::cli::{Cli, GoogleArgs, HandoffAction, HelpArgs};
use crate::failure::{Exit, Failure, code};
use crate::google::policy::{self, Allowed};

/// What latch knows about a command beyond what its parser says.
struct Entry {
    /// The command's words joined by `.` (`browser.tab.new`): its stable action id.
    id: &'static str,
    /// Where this command runs, including modes that only prepare or show a call.
    runs_on: fn(&ArgMatches) -> RunsOn,
    /// What it prints on success.
    output: &'static str,
    effect: Effect,
    /// Refused to an agent: only the owner's own machines may run it.
    owner_only: bool,
    /// The connector it belongs to, which may be turned off on a host.
    connector: Option<&'static str>,
    examples: &'static [Example],
}

#[derive(Clone, Copy, PartialEq)]
enum RunsOn {
    /// The calling machine, without selecting a host.
    Local,
    /// The selected host's receiver, including this machine's own receiver.
    Host,
    /// Local state by default, or the selected host's state.
    Either,
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
        runs_on: |_| RunsOn::Local,
        output: "This machine or agent as `peer` (name, key, kind), and whether its key was created now; with --events, JSON lines (hello, code, waiting, verified, expired, error) instead.",
        effect: Repeatable(
            "creates this machine's or agent's key if missing and registers it with Plow",
        ),
        owner_only: false,
        connector: None,
        examples: &[
            ex("An agent: log in with its own Plow credential", &["login"]),
            ex(
                "An owner's machine, with a token file",
                &["login", "--token-file", "plow.token", "--name", "mac"],
            ),
            ex(
                "Take the name of your machine it replaces, signing that one out",
                &["login", "--events", "--name", "mac", "--replace"],
            ),
        ],
    },
    Entry {
        id: "logout",
        runs_on: |_| RunsOn::Local,
        output: "`removed` (what it deleted, by file name: the key, Plow token and config.json; with --purge every secret, then the folder), `from` (`keychain` or `files`), `stopped` (whether a host was running), `purged`, `device` (an owner's machine: its `name`, null if the state folder was already gone, and whether Plow `removed` it; if not, the `error` and the `next` step; null otherwise) and `state_dir`.",
        effect: Repeatable(
            "stops this state folder's host, takes this machine out of the Plow account and signs out its login, and deletes its key and Plow token; with --purge also its vault and audit keys and the folder itself",
        ),
        owner_only: false,
        connector: None,
        examples: &[
            ex("Log this state folder out", &["logout"]),
            ex(
                "Delete this state folder and every secret it has",
                &["logout", "--purge"],
            ),
        ],
    },
    Entry {
        id: "agents",
        runs_on: |_| RunsOn::Either,
        output: "`host` (with --host: its `name` and whether it is `online`; else null) and `agents`, in Plow's order: each one's `uid`, `name`, `kind` (cloud or own), `cloud_kind`, `line` (`number`, `name`, `card_url`: its persona's contact card, null for an unnamed line), `created_at`, `status` (a cloud agent's phase in Plow; null for own agents), `failure_code`, `last_seen_at` (its credential's last use, not proof it runs) and `standing` on the host (`own_rules`, `exec_blocked`; null without --host).",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[
            ex("List the agents", &["agents"]),
            ex(
                "And their standing on the host studio",
                &["agents", "--host", "studio"],
            ),
        ],
    },
    Entry {
        id: "agents.show",
        runs_on: |_| RunsOn::Local,
        output: "`agent`: the agent, as `agents` lists it (its `standing` null). A failed or teardown agent is still an answer.",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[ex("One agent, by its name", &["agents", "show", "Scout"])],
    },
    Entry {
        id: "agents.catalog",
        runs_on: |_| RunsOn::Local,
        output: "`kinds`: each kind of cloud agent Plow can run, its `id` (for `agents new --cloud`) and `name`.",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[ex("The kinds of cloud agent", &["agents", "catalog"])],
    },
    Entry {
        id: "agents.start-by-text",
        runs_on: |_| RunsOn::Local,
        output: "`number`: Plow's managed number, in E.164; `default`: the kind Plow offers without asking (an `id`, or null); `kinds`: each cloud agent's `id`, `name`, the `phrase` to text the number to start it, and the same as a QR code's contents (`smsto`) and an `sms:` link (`sms_link`). Plow sets the agent up on a line of its own and texts back its contact card.",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[ex(
            "What to text to start a cloud agent",
            &["agents", "start-by-text"],
        )],
    },
    Entry {
        id: "agents.lines",
        runs_on: |_| RunsOn::Local,
        output: "`lines`: each phone line's `uid`, `number`, `name`, whether it is `available` for a new agent and the `agent_uid` that holds it.",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[ex(
            "The phone lines, and which are free",
            &["agents", "lines"],
        )],
    },
    Entry {
        id: "agents.new",
        runs_on: |_| RunsOn::Local,
        output: "`agent` (as `agents show` gives it; a cloud agent's `status` starts as provisioning), `token_file` (an own agent's credential file, by its absolute path; never the credential) and `api_url` (the Plow an own agent logs in to; null for a cloud agent).",
        effect: Once(
            "creates an agent in the account on the phone line given; an own agent's credential is written to a new private file",
        ),
        owner_only: true,
        connector: None,
        examples: &[
            ex(
                "An agent you run yourself",
                &[
                    "agents",
                    "new",
                    "--own",
                    "--token-file",
                    "scout.token",
                    "--name",
                    "Scout",
                    "--line",
                    "+15550000002",
                ],
            ),
            ex(
                "A cloud agent Plow runs",
                &[
                    "agents",
                    "new",
                    "--cloud",
                    "exe:hermes",
                    "--name",
                    "Hermes",
                    "--line",
                    "+15550000003",
                ],
            ),
        ],
    },
    Entry {
        id: "agents.card",
        runs_on: |_| RunsOn::Local,
        output: "`agent` (its uid), `card_url`, the `path` the vCard was saved to and its `size`. The card is the one Plow names for the agent's line (its persona's, photo included), fetched without your credential; an unnamed line has none (`no_card`).",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[ex(
            "Save an agent's contact card",
            &["agents", "card", "Willow", "--output", "willow.vcf"],
        )],
    },
    Entry {
        id: "agents.remove",
        runs_on: |_| RunsOn::Local,
        output: "`removed`: the agent as it was.",
        effect: Once(
            "removes the agent from Plow: its credential stops working, its line is freed and its chats are closed",
        ),
        owner_only: true,
        connector: None,
        examples: &[ex(
            "Remove an agent without asking",
            &["agents", "remove", "Scout", "--yes"],
        )],
    },
    Entry {
        id: "devices",
        runs_on: |_| RunsOn::Local,
        output: "`devices`: the account's machines, online or not.",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[ex("List the machines", &["devices"])],
    },
    Entry {
        id: "devices.remove",
        runs_on: |_| RunsOn::Local,
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
        runs_on: |_| RunsOn::Either,
        output: "`account` (`owner_uid`, `display_name`), `login` (`kind`: device or agent), `this_machine` (its `name` and `key`, and `hosting`: `running`, `ready`, `configuring`, `managed`, `attached`, `state`, `pid`, `blocked`, `waiting` (what a starting host waits for, else null), `deny_exec` (`every_agent`, `agent_uids`) as the host runs or last ran, null if its saved flags are unreadable, and `keep_awake` (`enabled`, `held`, `reason`; see `settings set`), null if its saved settings are unreadable), `hosts` (the account's hosts as Plow lists them, this machine first: `name`, `key`, `self`, Plow's `online` and `last_seen_at`, and `plugins`, the ones it has on as `name` and `state` (enabled or needs_setup), asked of every host Plow lists online within 2 s in all; null when unknown, with `plugins_reason`: host_offline, host_unreachable, caller_denied, protocol_mismatch or the host's own code), `default_host` (where a command without --host goes; null with `default_host_reason`: unknown_host, no_host, host_required or not_logged_in), `plugins`, `update`, `last_error`, `api_url`, `state_dir`, `version` and `protocol`. Before login it asks no one: `login`, `account`, `hosts` and this machine's `name` and `key` are null. With --local, the same without `account`, `hosts` and `default_host*`, asking no one; with --host, that host's own `status --local` (owner devices only; a host on an older latch answers in that latch's shape); with --follow too, JSON lines: `hello`, then a `status` line, again each time it changes.",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[
            ex("Who am I, and which hosts can I reach?", &["status"]),
            ex("This machine's own latch, offline", &["status", "--local"]),
            ex("Another host's own latch", &["status", "--host", "studio"]),
            ex(
                "Watch this machine's latch",
                &["status", "--local", "--follow"],
            ),
        ],
    },
    Entry {
        id: "file.get",
        runs_on: |_| RunsOn::Host,
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
        runs_on: |_| RunsOn::Host,
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
        runs_on: |args| {
            if GoogleArgs::from_arg_matches(args)
                .expect("Google arguments")
                .local_help()
            {
                RunsOn::Local
            } else {
                RunsOn::Host
            }
        },
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
        id: "messages.search",
        runs_on: |_| RunsOn::Host,
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
    Entry {
        id: "messages.thread",
        runs_on: |_| RunsOn::Host,
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
    Entry {
        id: "messages.chats",
        runs_on: |_| RunsOn::Host,
        output: "`chats`: each with `id`, `kind`, `name`, `participants` and `last_message_at`.",
        effect: ReadOnly,
        owner_only: false,
        connector: Some("messages"),
        examples: &[ex(
            "Recent group chats",
            &["messages", "chats", "--host", "mac", "--kind", "group"],
        )],
    },
    Entry {
        id: "messages.unreplied",
        runs_on: |_| RunsOn::Host,
        output: "`messages`: the newest inbound message of each direct chat with no later reply.",
        effect: ReadOnly,
        owner_only: false,
        connector: Some("messages"),
        examples: &[ex(
            "Unanswered in the last day",
            &["messages", "unreplied", "--host", "mac", "--within", "24h"],
        )],
    },
    Entry {
        id: "contacts.search",
        runs_on: |_| RunsOn::Host,
        output: "`people`: each with `name`, `nickname`, `organization`, and `phones` and `emails` as `display` and the `handle` to pass to `latch messages`.",
        effect: ReadOnly,
        owner_only: false,
        connector: Some("contacts"),
        examples: &[ex(
            "Find a person's handles",
            &["contacts", "search", "--host", "mac", "Ada"],
        )],
    },
    Entry {
        id: "contacts.lookup",
        runs_on: |_| RunsOn::Host,
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
        runs_on: |_| RunsOn::Host,
        output: "The program's `exit_code` (or `signal`), `stdout`, `stderr`, declared `writes` and `network` flag. Secret environment values are omitted from judgment and audit; program output is returned as-is.",
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
        runs_on: |_| RunsOn::Host,
        output: "The new session's id (shown only now: keep it), its engine, and the elements on screen.",
        effect: Once(
            "starts a browser session, in a fresh browser of its own, on the host and loads the page",
        ),
        owner_only: false,
        connector: None,
        examples: &[
            ex(
                "Open a page in a new session",
                &["browser", "--host", "mac", "open", "https://shop.example/"],
            ),
            ex(
                "On Camoufox, labelled for listings",
                &[
                    "browser",
                    "--host",
                    "mac",
                    "open",
                    "--engine",
                    "camoufox",
                    "--task",
                    "basket",
                    "https://shop.example/",
                ],
            ),
        ],
    },
    Entry {
        id: "browser.observe",
        runs_on: |_| RunsOn::Host,
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
        runs_on: |_| RunsOn::Host,
        output: "The page's text, at most 20,000 characters (`truncated` when cut).",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[ex("Read the page", &["browser", "--host", "mac", "text"])],
    },
    Entry {
        id: "browser.screenshot",
        runs_on: |_| RunsOn::Host,
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
        runs_on: |args| {
            if args.get_flag("dry_run") {
                RunsOn::Local
            } else {
                RunsOn::Host
            }
        },
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
        runs_on: |_| RunsOn::Host,
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
        runs_on: |args| {
            if matches!(
                args.get_one::<HandoffAction>("action"),
                Some(HandoffAction::Done)
            ) {
                RunsOn::Local
            } else {
                RunsOn::Host
            }
        },
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
        runs_on: |_| RunsOn::Host,
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
        runs_on: |_| RunsOn::Host,
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
        runs_on: |_| RunsOn::Host,
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
        runs_on: |_| RunsOn::Host,
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
        runs_on: |_| RunsOn::Host,
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
        runs_on: |_| RunsOn::Host,
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
        runs_on: |_| RunsOn::Host,
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
        runs_on: |_| RunsOn::Host,
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
        runs_on: |_| RunsOn::Host,
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
        runs_on: |_| RunsOn::Host,
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
        runs_on: |_| RunsOn::Host,
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
        runs_on: |_| RunsOn::Host,
        output: "What was filled (never a value), or a confirmation to carry out with `do --confirm`.",
        effect: Once("fills the page from a vault item, and presses the button named"),
        owner_only: false,
        connector: None,
        examples: &[
            ex(
                "Sign in with the login saved for the page's site",
                &[
                    "browser",
                    "--host",
                    "mac",
                    "fill",
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
        runs_on: |_| RunsOn::Host,
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
        runs_on: |_| RunsOn::Host,
        output: "The session closed.",
        effect: Repeatable("ends the session and deletes its cookies and logins"),
        owner_only: false,
        connector: None,
        examples: &[ex(
            "End this session",
            &["browser", "--host", "mac", "close"],
        )],
    },
    Entry {
        id: "browser.setup",
        runs_on: |_| RunsOn::Local,
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
            ex("Camoufox", &["browser", "setup", "--engine", "camoufox"]),
        ],
    },
    Entry {
        id: "vault.list",
        // An agent's list, the one that names a chat, goes to a host.
        runs_on: |args| match args.try_get_one::<String>("plow_chat") {
            Ok(Some(_)) => RunsOn::Host,
            _ => RunsOn::Either,
        },
        output: "For the owner, every item's metadata: id, kind, title, a login's username, sites, field labels, revision. For an agent, judged by the gatekeeper like its other work: each item's id, title and saved sites, and `next`, how to sign in with one. Never a secret value.",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[ex("List the items", &["vault", "--host", "mac", "list"])],
    },
    Entry {
        id: "vault.show",
        runs_on: |_| RunsOn::Either,
        output: "`item` (its metadata, as `list` shows it: its `fields` by label, each marked secret or not) and `values`: the value of each field that is not secret, by label. A secret field's value is never shown.",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[ex(
            "Show an item",
            &["vault", "--host", "mac", "show", "v3"],
        )],
    },
    Entry {
        id: "vault.add",
        runs_on: |_| RunsOn::Either,
        output: "The item added.",
        effect: Once("adds an item to the host's vault"),
        owner_only: true,
        connector: None,
        examples: &[
            ex(
                "Add a login with its password as JSON on stdin",
                &[
                    "vault",
                    "add",
                    "--kind",
                    "login",
                    "--title",
                    "Shop",
                    "--site",
                    "shop.example",
                    "--field",
                    "username=ada",
                    "--stdin",
                ],
            ),
            ex(
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
            ),
        ],
    },
    Entry {
        id: "vault.edit",
        runs_on: |_| RunsOn::Either,
        output: "The item at its new revision.",
        effect: Once("changes the item"),
        owner_only: true,
        connector: None,
        examples: &[
            ex(
                "Change a login's password, given as JSON on stdin",
                &["vault", "edit", "v1", "--revision", "2", "--stdin"],
            ),
            ex(
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
            ),
        ],
    },
    Entry {
        id: "vault.link",
        runs_on: |_| RunsOn::Either,
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
        runs_on: |_| RunsOn::Either,
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
        runs_on: |_| RunsOn::Either,
        output: "How many rows were `saved`, `updated` (a changed password or authenticator key), left as `duplicates`, and which `failed` and why, with the `source` and the rows `picked`. With --preview, only `preview`: its `source`, its `expect` token for --pick --expect, and `items`, each row's `pick` number, kind, title, username and site, whether it holds a password or authenticator key, whether the vault already holds it or it would update one, and the rows set aside; never a secret.",
        effect: Once(
            "adds the rows picked at the prompt or with --pick; --preview adds nothing and is not audited",
        ),
        owner_only: true,
        connector: None,
        examples: &[
            ex(
                "Import a 1Password export",
                &["vault", "--host", "mac", "import", "export.1pux"],
            ),
            ex(
                "See what an export holds, as JSON",
                &["vault", "import", "export.csv", "--preview"],
            ),
            ex(
                "Then import rows 1 and 3 to 5",
                &["vault", "import", "export.csv", "--pick", "1,3-5"],
            ),
            ex(
                "Import them only if they still mean what the preview showed (its `expect`); otherwise stale_revision, with the token they are at, and nothing is saved",
                &[
                    "vault",
                    "import",
                    "export.csv",
                    "--pick",
                    "1,3-5",
                    "--expect",
                    "4be1…",
                ],
            ),
        ],
    },
    Entry {
        id: "rules.set",
        runs_on: |_| RunsOn::Either,
        output: "The rules saved, with their revision.",
        effect: Repeatable("replaces the rules"),
        owner_only: true,
        connector: None,
        examples: &[
            ex(
                "Let agents read mail, never send it",
                &[
                    "rules",
                    "set",
                    "Agents may search my mail but never send any.",
                ],
            ),
            ex(
                "Save over the rules only if they are still at the revision `rules show` gave (`none`: there were none); otherwise stale_revision, with the revision they are at",
                &["rules", "set", "--revision", "9f2c…", "--file", "rules.txt"],
            ),
        ],
    },
    Entry {
        id: "rules.edit",
        runs_on: |_| RunsOn::Either,
        output: "At a terminal, the person's editor, then a line when the rules are saved; without one, `no_terminal` (use `rules set`).",
        effect: Once("changes the rules"),
        owner_only: true,
        connector: None,
        examples: &[ex(
            "Edit one agent's rules",
            &["rules", "edit", "--agent", "ada"],
        )],
    },
    Entry {
        id: "rules.clear",
        runs_on: |_| RunsOn::Either,
        output: "The rules removed.",
        effect: Repeatable("removes the rules"),
        owner_only: true,
        connector: None,
        examples: &[
            ex("Remove every agent's rules", &["rules", "clear"]),
            ex(
                "Remove one agent's rules only if they are still at the revision `rules show` gave",
                &["rules", "clear", "--agent", "ada", "--revision", "9f2c…"],
            ),
        ],
    },
    Entry {
        id: "rules.show",
        runs_on: |_| RunsOn::Either,
        output: "The rules for every agent and each agent's own, with their revisions, and `deny_exec`: the agents the host refuses exec (`every_agent`, `agent_uids`), as it runs or last ran, null if its saved flags are unreadable.",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[ex("Show the rules", &["rules", "show"])],
    },
    Entry {
        id: "rules.try",
        runs_on: |_| RunsOn::Either,
        output: "`verdict`: allow, not_covered, owner_said_no, or host_refused (a refusal the host makes without asking the judge, named in `refused`); `as` (owner, new_agent, or the agent), `request` (the operation, and the action as the judge was shown it: secret values and an exec's stdin masked) and `gate` (the judgment's details: probabilities, source, rule revisions, model, latency, cost). An agent's preview is its rules layer: the built-in policy (everyday work allowed, anything riskier needing a rule) and the rules, with no chat; a real call also judges its chat, which may add an OK or a no. A refusal is a preview that worked: exit 0. This machine's previews are judged by the calling latch, so they need no running host; another host's go to it. `--draft` judges against draft rules for every agent, never saved; `--new-agent` judges as a new agent, with no rules of its own, whatever connectors and --deny-exec the host has. `--fill-login TITLE --url URL` previews a browser fill of that saved login, as saved for the URL's site, in place of REQUEST (`request` is op browser with the `vault_fill` action the judge sees): the fill's judgment only, not the whole sign-in (opening the site is judged on its own) nor whether the vault has that login; nothing is opened or filled and the vault is not read.",
        effect: Repeatable(
            "asks the gatekeeper, billed to your Plow balance like a real judgment (about $0.0001); the request itself never runs, the draft is never saved, and the preview is not audited",
        ),
        owner_only: true,
        connector: None,
        examples: &[
            ex(
                "Would Penny be allowed to build the site?",
                &[
                    "rules", "try", "--agent", "Penny", "--", "exec", "--write", "~/site", "--",
                    "npm", "run", "build",
                ],
            ),
            ex(
                "Would a new agent be allowed to build the site under draft rules read from stdin?",
                &[
                    "rules",
                    "try",
                    "--new-agent",
                    "--draft",
                    "-",
                    "--",
                    "exec",
                    "--write",
                    "~/site",
                    "--",
                    "npm",
                    "run",
                    "build",
                ],
            ),
            ex(
                "Would a new agent be allowed to fill your saved Instacart login into its sign-in page?",
                &[
                    "rules",
                    "try",
                    "--new-agent",
                    "--fill-login",
                    "Instacart",
                    "--url",
                    "https://www.instacart.com/login",
                ],
            ),
            ex(
                "Preview a mail search on another host, as yourself",
                &[
                    "rules", "try", "--host", "mac", "--", "google", "--", "gmail", "search",
                    "invoice",
                ],
            ),
        ],
    },
    Entry {
        id: "plugins.enable",
        runs_on: |_| RunsOn::Local,
        output: "The connector's `state`, enabled, and what its setup found. A setup that fails leaves it needs_setup and says what to do. When some connected accounts need signing in again, `enable google` still succeeds and names them in `needs_sign_in`. When none works (google_needs_sign_in, with their `accounts`), it offers a sign-in link as below and waits until they all work again. When no Google account is connected in Plow, `enable google` gets a single-use sign-in link from Plow, `sign_in_url` with its `expires_at` (a credential until used: latch never logs or audits it; `connect_url` is Plow's public Connections page). At a terminal it shows the link and a QR code and waits up to 5 minutes for an account Plow did not have, renewing the link if it expires unopened, then enables Google; otherwise it fails with google_not_connected carrying the link: sign in, then run it again. --add-account does the same with Google already connected: it enables Google, then offers a link to add another account; without a terminal or --events the answer is the enable result plus `sign_in_url`, which confirms the enable and the link, not that another account was added. --events (any connector): JSON lines `hello`, a `link` (`url`, `expires_at`) for each link, then one `result` or `error`, with the same wait. sign_in_timeout (exit 1): no new account was seen in time (or, for a sign-in again, the accounts named still don't work), counting one last look at the deadline; the person may still be signing in or chose an account Plow already had. With --add-account, Google was enabled before the wait. Closing stdin (a pipe), Ctrl-C, SIGTERM or a gone stdout reader stops the wait: before the final enable is sent that is google_connect_cancelled (exit 1; with --add-account, Google was enabled before the wait), after it outcome_unknown (exit 5). Only a `result` confirms the enable. The enable after the wait is a fresh one, so a `plugins disable google` made during the wait does not hold.",
        effect: Repeatable(
            "runs the connector's setup and, if it succeeds, turns it on for this host",
        ),
        owner_only: true,
        connector: None,
        examples: &[
            ex("Turn Google on here", &["plugins", "enable", "google"]),
            ex(
                "From an app: get the sign-in link as a line, then the result",
                &["plugins", "enable", "google", "--events"],
            ),
            ex(
                "Add another Google account",
                &["plugins", "enable", "google", "--add-account"],
            ),
        ],
    },
    Entry {
        id: "plugins.disable",
        runs_on: |_| RunsOn::Local,
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
        id: "settings.set",
        runs_on: |_| RunsOn::Local,
        output: "`settings`, every setting as saved (`keep_awake`), and `next`: `latch status --local`, whose `this_machine.hosting.keep_awake` (`enabled`; `held`, null while the running host has not said; `reason` when not held: disabled, not_serving, on_battery, unsupported, refused or unknown) says what the host makes of it. Saved is all this says.",
        effect: Repeatable(
            "saves this machine's host setting, which a running host picks up without a restart",
        ),
        owner_only: false,
        connector: None,
        examples: &[
            ex(
                "Keep this Mac awake while plugged in, for its host",
                &["settings", "set", "--keep-awake", "on"],
            ),
            ex(
                "Let it sleep as usual",
                &["settings", "set", "--keep-awake", "off"],
            ),
        ],
    },
    Entry {
        id: "audit",
        runs_on: |_| RunsOn::Either,
        output: "JSON lines: `hello`; a `snapshot` (`cursor`, the id before the first record shown, and `has_older`); a `record` line each (`cursor`, its id, and `record`: id, time, caller, caller_name, agent, op, command (the command it ran, as `latch KIND …`, as the dashboard shows it, secrets masked; null for a connection), decision, outcome, and `details` with started, args and gate, credential-looking values hidden, best effort); then `caught_up` (the last `cursor`, `has_older`). With --follow, then each new record. The newest 50 unless --last N or --all; --before ID reads the page before. With --host, another host's, the same lines.",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[
            ex("Read the newest records", &["audit"]),
            ex(
                "Read the page before a record",
                &["audit", "--before", "ID"],
            ),
            ex("Watch the audit as it grows", &["audit", "--follow"]),
            ex(
                "Pick up after the last record you saw",
                &["audit", "--follow", "--after", "ID"],
            ),
            ex("Read another host's audit", &["audit", "--host", "studio"]),
        ],
    },
    Entry {
        id: "audit.verify",
        runs_on: |_| RunsOn::Local,
        output: "How many `records` the chain holds and the `last` one's hash, checked without the key.",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[ex("Check the audit's chain", &["audit", "verify"])],
    },
    Entry {
        id: "audit.rotate-key",
        runs_on: |_| RunsOn::Local,
        output: "The `current` key and every key kept; a running `latch serve` seals its next record with the new one.",
        effect: Once("makes a new audit key for new records; old records keep theirs"),
        owner_only: false,
        connector: None,
        examples: &[ex(
            "Seal new records under a new key",
            &["audit", "rotate-key"],
        )],
    },
    Entry {
        id: "audit.judge",
        runs_on: |_| RunsOn::Either,
        output: "One gatekeeper call, by its id (a record's `kept_request`, in `details.gate` or its `layers`): `id`, `time`, `layer` (`owner`, or an agent's `rules` or `chat`), `request` (the body sent to Plow's decision route: model, state, questions; the rules, the chat and the action as the judge saw them) and `answer` (Plow's reply, null when none came). Kept sealed with the host's audit key, exactly as sent; printed with credential-looking values hidden, as `latch audit` hides them, best effort, or with `--raw` exactly as sent, a credential typed into a command included. The judge is never shown a vault or secret value. A host keeps them 7 days (then `no_such_request`), on that host only.",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[
            ex(
                "See what the gatekeeper was asked and answered",
                &["--pretty", "audit", "judge", "REQUEST"],
            ),
            ex(
                "Exactly what the judge saw, credentials included",
                &["--pretty", "audit", "judge", "REQUEST", "--raw"],
            ),
            ex(
                "The same, from another host",
                &["audit", "judge", "REQUEST", "--host", "studio"],
            ),
        ],
    },
    Entry {
        id: "audit.shot",
        runs_on: |_| RunsOn::Either,
        output: "The `record`, the `shot`, the `path` the PNG was saved to, its `size` and `sha256`. Only a screenshot a browser plan's record names (an action's number from its `step_records`, or `final`), and only the bytes that record's sha256 says were kept: a host keeps a request's screenshots 7 days (then `no_such_shot`). A standalone `browser screenshot` was saved on its caller's machine, not kept on the host. latch doesn't redact pixels: it takes no screenshot while a value filled from the vault is on the page, and none of a fill step, but typed text and the rest of the page can show.",
        effect: ReadOnly,
        owner_only: true,
        connector: None,
        examples: &[
            ex(
                "Save the screenshot after a plan's first action",
                &["audit", "shot", "ID", "1", "--output", "step-1.png"],
            ),
            ex(
                "Save the page a plan stopped on, from another host",
                &[
                    "audit", "shot", "ID", "final", "--output", "stop.png", "--host", "studio",
                ],
            ),
        ],
    },
    Entry {
        id: "serve",
        runs_on: |_| RunsOn::Local,
        output: "With -d, the host once it answers: `state_dir`, `pid`, `run`, `version`, `log`. Otherwise the dashboard until Ctrl-C, then how the host stopped; with --foreground, log lines until stopped.",
        effect: Once("runs this machine's host, as a service of this user's unless --foreground"),
        owner_only: false,
        connector: None,
        examples: &[
            ex("Start the host with its dashboard", &["serve"]),
            ex("Start the host detached", &["serve", "-d"]),
            ex("Refuse exec to agents", &["serve", "--deny-exec"]),
        ],
    },
    Entry {
        id: "update",
        runs_on: |_| RunsOn::Local,
        output: "`updated` and the build installed (`to`); `repaired`, the helpers this build runs that were missing (an update cut off part way), when that is why this build installed again; and when a host ran here, `host`: `running` again, or stopped with the `next` command that starts it; with --check, `current`, `latest` and `available`.",
        effect: Repeatable(
            "installs another latch build on this machine and restarts a `serve -d` host on it",
        ),
        owner_only: true,
        connector: None,
        examples: &[
            ex("Install the newest build", &["update"]),
            ex("See whether a newer build is out", &["update", "--check"]),
            ex("Go back to an earlier build", &["update", "v0.1.0-1327"]),
        ],
    },
    Entry {
        id: "stop",
        runs_on: |_| RunsOn::Local,
        output: "`stopped` (whether a host was running), `state_dir`, `state`, `browser_sessions_ended` (the browser sessions open when the host stopped; they end with it) and, if it failed, `error`.",
        effect: Repeatable("stops this state folder's host and removes its service"),
        owner_only: false,
        connector: None,
        examples: &[ex("Stop the host", &["stop"])],
    },
    Entry {
        id: "dashboard",
        runs_on: |_| RunsOn::Local,
        output: "The running host's dashboard, until Ctrl-C; without a terminal, `no_terminal` (use `audit --follow`).",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[ex("Watch the host", &["dashboard"])],
    },
    Entry {
        id: "logs",
        runs_on: |_| RunsOn::Local,
        output: "JSON lines: `hello`, then a `log` line per record of the host's log (serve.log); with --follow, then each one added.",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[ex("Follow the host's log", &["logs", "--follow"])],
    },
    Entry {
        id: "app",
        runs_on: |_| RunsOn::Local,
        output: "`app`, the Plow Latch.app it opened. Only on macOS, from the latch inside the app or a link to it: elsewhere `unsupported_platform`, and a latch outside an app `local_build`. The app picks its state folder itself: a folder --state-dir or LATCH_STATE_DIR names is refused (`bad_arguments`).",
        effect: Repeatable("opens the Mac app's window, or brings it to the front"),
        owner_only: false,
        connector: None,
        examples: &[ex("Open the Mac app", &["app"])],
    },
    Entry {
        id: "help",
        runs_on: |_| RunsOn::Local,
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
        runs_on: |_| RunsOn::Local,
        output: "At a terminal, each step and the latch command it ran; skipped steps say so. Without one, `no_terminal`, whose `next` is the first step not done. With --status, at a terminal or not, `steps`: each step's `step` (login, each connector, host), `state` (done, needed or blocked), `reason`, `args` (the words after `latch` that do it, null once done, or while something else must happen first) and, for a connector, `enabled` and `ready` (enabled and served by a running host). For the owner, the Google step asks Plow (read only) whether Google is connected there: it adds `accounts`, and `connect_url` (where to connect one) while none is; it is blocked when Plow cannot be asked. With an account connected it also asks Plow for their credentials, for up to 5 s, to add `needs_sign_in`: the accounts Google wants the owner to sign in with again (the step is needed when none works); said only when there are some. With --check-access, on a Mac, the Messages and Contacts steps add `access` (granted, not_granted or unknown): whether Plow Latch has Full Disk Access, checked once as the host would and saving nothing.",
        effect: Repeatable(
            "logs in and enables connectors with no host running, then starts the host if the owner says so; done steps are skipped. --status changes nothing",
        ),
        owner_only: false,
        connector: None,
        examples: &[
            ex("Set up this machine, or finish setting it up", &["setup"]),
            ex("See which setup steps are done", &["setup", "--status"]),
            ex(
                "And whether Full Disk Access is granted",
                &["setup", "--status", "--check-access"],
            ),
        ],
    },
    Entry {
        id: "skill",
        runs_on: |_| RunsOn::Local,
        output: "SKILL.md, for this version of latch.",
        effect: ReadOnly,
        owner_only: false,
        connector: None,
        examples: &[ex("Learn how to use latch", &["skill"])],
    },
];

/// The top-level fields of the answers that are plain structs, checked
/// against those structs by the tests.
const OUTPUT_FIELDS: &[(&str, &[&str])] = &[
    ("agents", &["host", "agents"]),
    ("devices", &["devices"]),
    ("devices.remove", &["removed"]),
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

/// `latch help [COMMAND…] [--examples | --json]`, the JSON indented with
/// `--pretty`. Under `google`, the words after it name gog commands, from
/// the grammar that validates them.
pub fn run(args: HelpArgs, pretty: bool) -> ExitCode {
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
        eprintln!("{}", Failure::new(code::unknown_command, message).to_json());
        return Exit::Usage.into();
    };
    if !args.examples && !args.json && gog_words.is_empty() {
        crate::term::print_out(&command.render_long_help().to_string());
        return ExitCode::SUCCESS;
    }
    let mut runnable = Vec::new();
    if gog_words.is_empty() {
        actions(command, path, false, &mut runnable);
    }
    let mut listed: Vec<Action> = runnable.iter().map(|(id, cmd)| describe(id, cmd)).collect();
    let google = find(&mut root, &["google".to_string()]).expect("latch google");
    listed.extend(gog.iter().map(|allowed| describe_gog(allowed, google)));
    let text = if args.json {
        let help = Help {
            version: crate::cli::version(),
            exit_codes: Exit::ALL
                .iter()
                .map(|exit| ExitCodeHelp {
                    code: u8::from(*exit),
                    meaning: exit.meaning(),
                })
                .collect(),
            error_codes: crate::failure::CODES
                .iter()
                .map(|code| ErrorCodeHelp {
                    code: code.name,
                    exit: u8::from(code.exit),
                })
                .collect(),
            actions: listed,
        };
        let help = match pretty {
            true => serde_json::to_string_pretty(&help),
            false => serde_json::to_string(&help),
        };
        format!("{}\n", help.expect("help serializes"))
    } else if args.examples {
        let examples = |action: &Action| -> String {
            (action.examples.iter())
                .map(|example| format!("  # {}\n  {}\n", example.what, example.command))
                .collect()
        };
        (listed.iter())
            .map(|action| format!("{}\n{}\n", action.usage, examples(action)))
            .collect()
    } else {
        (gog.iter().zip(&listed))
            .map(|(allowed, action)| gog_help(allowed, action))
            .collect()
    };
    crate::term::print_out(&text);
    ExitCode::SUCCESS
}

/// A gog command's help: its words, what it changes, its flags, and where
/// gog's own usage is.
fn gog_help(allowed: &Allowed, action: &Action) -> String {
    let words = allowed.words.join(" ");
    let mut text = format!("{}\n", action.usage);
    if !allowed.aliases.is_empty() {
        text += &format!("  Also typed as: {}\n", allowed.aliases.join(", "));
    }
    text += &match action.side_effects {
        None => "  Reads only: changes nothing at Google.\n".to_string(),
        Some(what) => format!("  Side effects: {what}.\n"),
    };
    let flags: Vec<String> = (allowed.flags.iter())
        .map(|(name, value)| match value {
            true => format!("--{name} VALUE"),
            false => format!("--{name}"),
        })
        .collect();
    text += &format!("  Flags: {}\n", flags.join(" "));
    text + &format!("  `latch google --host H -- {words} --help` prints gog's own usage.\n\n")
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
    include_hidden: bool,
    out: &mut Vec<(String, &'a clap::Command)>,
) {
    let runs = !command.has_subcommands() || !command.is_subcommand_required_set();
    if !path.is_empty() && runs {
        out.push((path.join("."), command));
    }
    for sub in command
        .get_subcommands()
        .filter(|sub| include_hidden || !sub.is_hide_set())
    {
        let mut path = path.to_vec();
        path.push(sub.get_name().to_string());
        actions(sub, &path, include_hidden, out);
    }
}

fn entry(id: &str) -> &'static Entry {
    ENTRIES
        .iter()
        .find(|entry| entry.id == id)
        .unwrap_or_else(|| panic!("no help entry for `{id}`"))
}

fn selected(mut args: &ArgMatches) -> (String, &ArgMatches) {
    let mut path = Vec::new();
    while let Some((name, child)) = args.subcommand() {
        path.push(name);
        args = child;
    }
    (path.join("."), args)
}

/// Refuse selectors that this operation would ignore, before touching state.
pub fn check_selectors(matches: &ArgMatches) -> Result<(), Failure> {
    let (id, args) = selected(matches);
    let has = |name| args.try_get_one::<String>(name).ok().flatten().is_some();
    // Unregistered internal or removed commands cannot accept a host either.
    let location = ENTRIES
        .iter()
        .find(|entry| entry.id == id)
        .map_or(RunsOn::Local, |entry| (entry.runs_on)(args));
    if has("host") && location == RunsOn::Local {
        return Err(Failure::new(
            code::host_not_supported,
            format!(
                "`latch {}` runs locally; omit --host and unset LATCH_HOST. Run it on the intended machine",
                id.replace('.', " ")
            ),
        ));
    }
    if has("plow_chat") && location != RunsOn::Host {
        return Err(Failure::new(
            code::plow_chat_not_supported,
            format!(
                "`latch {}` does not use a Plow chat; omit --plow-chat and unset LATCH_PLOW_CHAT",
                id.replace('.', " ")
            ),
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct Help {
    version: &'static str,
    exit_codes: Vec<ExitCodeHelp>,
    /// Every error code, with the exit it comes with.
    error_codes: Vec<ErrorCodeHelp>,
    actions: Vec<Action>,
}

#[derive(Serialize)]
struct ExitCodeHelp {
    code: u8,
    meaning: &'static str,
}

#[derive(Serialize)]
struct ErrorCodeHelp {
    code: &'static str,
    exit: u8,
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
        actions(&root, &[], false, &mut out);
        out.into_iter().map(|(id, _)| id).collect()
    }

    /// The words of the command a parsed line ran, joined like an id.
    fn ran(matches: &clap::ArgMatches) -> String {
        selected(matches).0
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
    fn every_selector_is_routed_or_refused_by_the_registry() {
        let mut root = Cli::command();
        root.build();
        let mut runnable = Vec::new();
        actions(&root, &[], true, &mut runnable);
        for (id, command) in runnable {
            for (selector, flag) in [("host", "--host"), ("plow_chat", "--plow-chat")] {
                let accepts_selector = command.get_arguments().any(|arg| arg.get_id() == selector);
                let entry = ENTRIES.iter().find(|entry| entry.id == id);
                let examples: Vec<Vec<&str>> = entry.map_or_else(
                    || vec![id.split('.').collect()],
                    |entry| {
                        entry
                            .examples
                            .iter()
                            .map(|example| example.args.to_vec())
                            .collect()
                    },
                );
                for example in examples {
                    let mut words = example.iter().copied();
                    let mut argv = Vec::new();
                    while let Some(word) = words.next() {
                        if word == flag {
                            words.next();
                        } else {
                            argv.push(word);
                        }
                    }
                    let with_selector = (0..=argv.len()).find_map(|at| {
                        let mut candidate = argv.clone();
                        candidate.splice(at..at, [flag, "routing-probe"]);
                        let matches = root
                            .clone()
                            .try_get_matches_from(std::iter::once("latch").chain(candidate))
                            .ok()?;
                        let (found, args) = selected(&matches);
                        let value = args.try_get_one::<String>(selector).ok().flatten();
                        (found == id && value.is_some_and(|value| value == "routing-probe"))
                            .then_some(matches)
                    });
                    assert_eq!(
                        with_selector.is_some(),
                        accepts_selector,
                        "{id}: {}",
                        quoted(&example)
                    );
                    if let Some(matches) = with_selector {
                        let (_, args) = selected(&matches);
                        let routes = entry.is_some_and(|entry| match selector {
                            "host" => (entry.runs_on)(args) != RunsOn::Local,
                            _ => (entry.runs_on)(args) == RunsOn::Host,
                        });
                        assert_eq!(check_selectors(&matches).is_ok(), routes, "{id}");
                        if !routes {
                            assert_eq!(
                                check_selectors(&matches).unwrap_err().code,
                                format!("{selector}_not_supported")
                            );
                        }
                    }
                }
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
        for name in ["engine", "task", "url", "host", "session", "state_dir"] {
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
                "agents",
                serde_json::to_value(crate::agents::AgentsOutput {
                    host: None,
                    agents: vec![],
                }),
            ),
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
                let Some(crate::cli::Command::Google(args)) =
                    Cli::try_parse_from(argv).unwrap().command
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
