//! Which gog commands an agent may run, and what each argument means.
//!
//! `schema.json` is the command grammar the pinned gog version declares
//! (its commands, their aliases and flags), trimmed to the reviewed list.
//! We read every argument against it, the way gog itself would: a word is
//! a subcommand, a flag, a flag's value or a positional value. Anything the
//! grammar does not name is refused before gog starts. Reading the grammar
//! ourselves, instead of matching text, is what stops a search query such
//! as "gmail watch" from being taken for the excluded `gmail watch` command.

use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::problem;
use crate::failure::{Exit, Failure};

/// The grammar, parsed from `schema.json` the first time it is used and
/// shared after that. `LazyLock` runs the closure once per process.
static GRAMMAR: LazyLock<Node> =
    LazyLock::new(|| serde_json::from_str(include_str!("schema.json")).expect("pinned grammar"));

/// One command or command group in the grammar, e.g. `gmail` or `gmail send`.
#[derive(Deserialize)]
struct Node {
    name: String,
    aliases: Vec<String>,
    /// The command's dotted name (`gmail.send`); `None` for a group that
    /// cannot run by itself, such as `gmail`.
    command: Option<String>,
    /// The subcommand that runs when this one is given without one.
    default: Option<String>,
    flags: Vec<Flag>,
    children: Vec<Node>,
    /// Subcommands gog has that we refuse, such as `gmail watch`.
    excluded: Vec<String>,
}

#[derive(Deserialize)]
struct Flag {
    name: String,
    aliases: Vec<String>,
    /// Whether the flag takes a value (`--to ADDR`) or is a switch (`--force`).
    value: bool,
}

impl Node {
    /// The subcommand `word` names, by its name or one of its aliases.
    fn child(&self, word: &str) -> Option<&Node> {
        self.children
            .iter()
            .find(|child| named(word, &child.name, &child.aliases))
    }

    /// The subcommand gog runs when this one is given without one:
    /// `gmail thread ID` runs `gmail thread get ID`.
    fn default_child(&self) -> Option<&Node> {
        let default = self.default.as_ref()?;
        self.children.iter().find(|child| child.name == *default)
    }

    /// The flag `name` names: one of this command's own, else a global one.
    fn flag(&self, name: &str) -> Option<&Flag> {
        self.flags
            .iter()
            .chain(GRAMMAR.flags.iter())
            .find(|flag| named(name, &flag.name, &flag.aliases))
    }
}

/// Whether `word` is `canonical` or one of its aliases.
fn named(word: &str, canonical: &str, aliases: &[String]) -> bool {
    word == canonical || aliases.iter().any(|alias| alias == word)
}

/// Flags whose value is a file gog reads: a path on the client, whose bytes
/// are sent along with the request.
const INPUT_FLAGS: &[&str] = &[
    "attach",
    "body-file",
    "body-html-file",
    "signature-file",
    "note-file",
];

/// Flags whose value is where gog writes a file: a path on the client,
/// where the downloaded bytes land.
const OUTPUT_FLAGS: &[&str] = &["out", "out-dir"];

/// A file argument: which `argv` entry holds its path, and that path as the
/// agent gave it (on the client).
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct LocalFile {
    /// Position in `Plan::argv`. The host's worker swaps in a path inside
    /// its own call folder there. `usize::MAX` means gog picks the file
    /// names itself (`--download` without `--out-dir`).
    pub index: usize,
    pub path: String,
    /// An `--out-dir`: files land below `path` rather than at it.
    pub directory: bool,
}

/// A command that passed the grammar: what gog will run, and what the host
/// needs to know about it.
#[derive(Debug, Default)]
pub struct Plan {
    /// The dotted command name, e.g. `gmail.send`.
    pub command: String,
    /// The arguments for gog: aliases replaced by canonical names, implied
    /// default verbs and safe defaults added.
    pub argv: Vec<String>,
    /// Every flag as (canonical name, value); a switch has "true" or "false".
    pub flags: Vec<(String, String)>,
    /// Files gog reads, sent from the client.
    pub inputs: Vec<LocalFile>,
    /// Files gog writes, sent back to the client.
    pub outputs: Vec<LocalFile>,
    /// `--help` was given: gog prints usage instead of calling Google.
    pub help: bool,
    /// The positional values, in order (a calendar id, a message id, a query).
    pub positionals: Vec<String>,
}

impl Plan {
    /// Reads cannot change Google state; a timed-out write may already have.
    pub fn read_only(&self) -> bool {
        self.help || reads(&self.command)
    }

    /// The value of flag `name`. If the flag was repeated, the last one wins,
    /// as in gog.
    pub fn flag(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The read commands that run on every connected account (or on each one
    /// `--account` names) and come back merged.
    pub fn fanout(&self) -> bool {
        matches!(
            self.command.as_str(),
            "gmail.search"
                | "calendar.calendars"
                | "calendar.events"
                | "calendar.freebusy"
                | "calendar.conflicts"
        )
    }

    /// The agent chose which fields come back (`--select` or `--fields`), so
    /// gog's items pass through as they are.
    pub fn projected(&self) -> bool {
        self.flag("select").is_some() || self.flag("fields").is_some()
    }

    /// A merged calendar event list the agent did not project itself comes
    /// back in the compact shape (`merge::compact_events`).
    pub fn compact(&self) -> bool {
        self.command == "calendar.events" && !self.projected()
    }

    /// For a timed `calendar create`, which is checked for conflicts first:
    /// the calendar it books on. `None` for every other command, and for an
    /// all-day create.
    pub fn conflict_check(&self) -> Option<&str> {
        let timed = self.command == "calendar.create" && self.flag("all-day") != Some("true");
        (timed && !self.help).then(|| self.positionals.first().map_or("primary", String::as_str))
    }

    /// What the audit log may hold about this command: its name, the names
    /// of its flags, and the recipients of mail and invitations. Flag values
    /// such as bodies and search queries are left out.
    pub fn audit(&self) -> serde_json::Value {
        let recipients: Vec<_> = self
            .flags
            .iter()
            .filter(|(key, _)| {
                (self.command.starts_with("gmail.") && matches!(key.as_str(), "to" | "cc" | "bcc"))
                    || matches!(key.as_str(), "attendees" | "add-attendee")
            })
            .collect();
        json!({
            "command": self.command,
            "flags": self.flags.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            "recipients": recipients.iter().map(|(_, value)| value).collect::<Vec<_>>(),
        })
    }

    /// Where a file gog wrote lands on the client. The worker names each
    /// file `<output number>/<path inside that output>`; anything else, or
    /// a path that would climb out with `..`, is a bad response.
    pub fn destination(&self, path: &str) -> Result<PathBuf, Failure> {
        let (index, relative) = path
            .split_once('/')
            .ok_or_else(|| problem(Exit::Failed, "bad_response", json!({})))?;
        let output = index
            .parse::<usize>()
            .ok()
            .and_then(|n| self.outputs.get(n))
            .ok_or_else(|| problem(Exit::Failed, "bad_response", json!({})))?;
        if Path::new(relative)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(problem(Exit::Failed, "bad_response", json!({})));
        }
        Ok(if output.directory {
            Path::new(&output.path).join(relative)
        } else {
            PathBuf::from(&output.path)
        })
    }
}

/// One command the grammar allows, as `latch help` describes it.
pub struct Allowed {
    /// As typed after `--`: `["gmail", "search"]`.
    pub words: Vec<&'static str>,
    /// The dotted name, e.g. `gmail.search`.
    pub command: &'static str,
    pub aliases: &'static [String],
    /// Its flags, by canonical name, and whether each takes a value.
    pub flags: Vec<(&'static str, bool)>,
    pub read_only: bool,
}

/// Every command the grammar allows, in the grammar's order.
pub fn allowed() -> Vec<Allowed> {
    fn walk(node: &'static Node, prefix: &[&'static str], out: &mut Vec<Allowed>) {
        for child in &node.children {
            let mut words = prefix.to_vec();
            words.push(&child.name);
            if let Some(command) = &child.command {
                out.push(Allowed {
                    words: words.clone(),
                    command,
                    aliases: &child.aliases,
                    flags: (child.flags.iter())
                        .filter(|flag| flag.name != "help")
                        .map(|flag| (flag.name.as_str(), flag.value))
                        .collect(),
                    read_only: reads(command),
                });
            }
            walk(child, &words, out);
        }
    }
    let mut out = Vec::new();
    walk(&GRAMMAR, &[], &mut out);
    out
}

/// Every command the grammar allows, as typed after `--` (`gmail search`).
pub fn allowed_commands() -> Vec<String> {
    allowed()
        .iter()
        .map(|allowed| allowed.words.join(" "))
        .collect()
}

/// Whether `command` (dotted) only reads: it cannot change Google state.
fn reads(command: &str) -> bool {
    matches!(
        command,
        "gmail.search"
            | "gmail.messages.search"
            | "gmail.get"
            | "gmail.thread.get"
            | "gmail.labels.list"
            | "gmail.drafts.list"
            | "gmail.drafts.get"
            | "gmail.attachment"
            | "calendar.calendars"
            | "calendar.events"
            | "calendar.event"
            | "calendar.search"
            | "calendar.freebusy"
            | "calendar.conflicts"
    )
}

/// The agent guide for `latch google`, built into latch.
pub const GUIDE: &str = include_str!("../../GOOGLE-AGENT-GUIDE.md");

/// The help text after `latch google --help`'s options: what may run, then
/// the agent guide.
pub fn allowed_help() -> &'static str {
    // clap wants a `&'static str`; the text is built once per run.
    static HELP: LazyLock<String> = LazyLock::new(|| {
        format!(
            "Allowed gog commands (after `--`; `-- <command> --help` shows gog's own help for one, `latch help google <command>` latch's):\n  {}\n\n{GUIDE}",
            allowed_commands().join("\n  ")
        )
    });
    &HELP
}

/// The `--max` a calendar event list gets when the agent names none: gog's
/// own default of 10 is fewer than a busy week holds.
pub const CALENDAR_EVENTS_MAX: &str = "100";

/// latch's own arguments, which may also be written among gog's.
pub struct Lifted {
    /// gog's arguments with latch's taken out.
    pub args: Vec<String>,
    /// The accounts the last `--account` (or `-a`) named, if any.
    pub accounts: Option<Vec<String>>,
    /// `--confirm-conflict` was among the arguments.
    pub confirm_conflict: bool,
}

/// Takes `--account EMAIL[,EMAIL…]` and `--confirm-conflict` out of gog's
/// arguments, so an agent may write them before `--` or anywhere after it.
/// The account flag is spelled as gog spells its own: `--account v`,
/// `--account=v`, `-a v`, `-a=v` or `-av`. Words after a bare `--` are
/// gog's positional values and stay as they are.
pub fn lift(args: &[String]) -> Result<Lifted, Failure> {
    let mut lifted = Lifted {
        args: Vec::new(),
        accounts: None,
        confirm_conflict: false,
    };
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            lifted.args.extend_from_slice(&args[i..]);
            break;
        }
        let value = if arg == "--account" || arg == "-a" {
            i += 1;
            Some(args.get(i).map(String::as_str).unwrap_or(""))
        } else {
            arg.strip_prefix("--account=")
                .or_else(|| arg.strip_prefix("-a="))
                .or_else(|| arg.strip_prefix("-a"))
        };
        if let Some(value) = value {
            let accounts: Vec<String> = value
                .split(',')
                .filter(|account| !account.is_empty())
                .map(str::to_owned)
                .collect();
            if accounts.is_empty() {
                return Err(Failure::usage(
                    "usage",
                    "--account needs a value: the account's email address".into(),
                ));
            }
            lifted.accounts = Some(accounts);
        } else if arg == "--confirm-conflict" {
            lifted.confirm_conflict = true;
        } else {
            lifted.args.push(args[i].clone());
        }
        i += 1;
    }
    Ok(lifted)
}

/// A refusal can name only grammar literals. Stop before any positional
/// value, so even a rejected request cannot put a body or query in the log.
pub fn command_prefix(args: &[String]) -> String {
    let mut node: &Node = &GRAMMAR;
    let mut path = Vec::new();
    for arg in args {
        if let Some(child) = node.child(arg) {
            path.push(child.name.clone());
            node = child;
        } else {
            if node.excluded.contains(arg) {
                path.push(arg.clone());
            }
            break;
        }
    }
    if path.is_empty() {
        "unrecognized".into()
    } else {
        path.join(".")
    }
}

/// Reads gog arguments against the grammar. Returns what gog will run, or
/// `not_allowed` (exit 4) for anything outside the reviewed list.
pub fn parse(args: &[String]) -> Result<Plan, Failure> {
    let mut plan = Plan::default();
    let node = read_arguments(args, &mut plan)?;
    let node = add_default_verb(node, &mut plan);
    plan.help = plan.flag("help") == Some("true");
    // A group's own help (`gmail --help`) lists its commands; it runs
    // nothing, so it is named by the group (`gmail`).
    plan.command = match &node.command {
        Some(command) => command.clone(),
        None if plan.help && !std::ptr::eq(node, &*GRAMMAR) => command_prefix(args),
        None => return Err(denied()),
    };
    add_attachment_output(&mut plan);
    add_download_output(&mut plan);
    add_calendar_notifications(node, &mut plan);
    add_calendar_list_max(&mut plan);
    Ok(plan)
}

/// Walks the arguments from left to right, moving down the grammar one
/// subcommand at a time, and fills in `plan`. Returns the command reached.
fn read_arguments(args: &[String], plan: &mut Plan) -> Result<&'static Node, Failure> {
    let mut node: &'static Node = &GRAMMAR;
    // After the first positional value, words are values, never subcommands.
    let mut positional = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if node.excluded.contains(arg) && !positional {
            return Err(denied());
        }
        // A word this command does not know goes to its default verb.
        if node.child(arg).is_none()
            && let Some(verb) = node.default_child()
        {
            plan.argv.push(verb.name.clone());
            node = verb;
        }
        if arg == "--" {
            // Everything after `--` is positional, and goes to gog as is.
            if node.command.is_none() {
                return Err(denied());
            }
            plan.argv.extend_from_slice(&args[i..]);
            break;
        }
        // gog always runs with `--json`, so the agent's own is dropped. It is
        // still refused before the command path, like every flag there.
        if (arg == "--json" || arg == "-j") && !std::ptr::eq(node, &*GRAMMAR) {
            i += 1;
            continue;
        }
        if arg.starts_with('-') {
            i = read_flag(node, args, i, plan)?;
        } else if !positional && let Some(child) = node.child(arg) {
            plan.argv.push(child.name.clone());
            node = child;
        } else {
            if node.command.is_none() {
                return Err(denied());
            }
            positional = true;
            plan.argv.push(arg.clone());
            plan.positionals.push(arg.clone());
        }
        i += 1;
    }
    Ok(node)
}

/// Reads the flag at `args[i]` into `plan`, taking its value from the same
/// argument (`--to=ADDR`) or the next one (`--to ADDR`). Returns the index
/// of the last argument it used.
fn read_flag(
    node: &Node,
    args: &[String],
    mut i: usize,
    plan: &mut Plan,
) -> Result<usize, Failure> {
    let (name, joined) = split_flag(&args[i]);
    let flag = node.flag(name).ok_or_else(denied)?;
    let value = if flag.value {
        match joined {
            Some(value) => value.to_owned(),
            None => {
                i += 1;
                args.get(i).cloned().ok_or_else(denied)?
            }
        }
    } else {
        joined.unwrap_or("true").to_owned()
    };
    // A switch can only be set on or off; `--force=maybe` is refused.
    if !flag.value && !matches!(value.as_str(), "true" | "false") {
        return Err(denied());
    }
    // Kong splits repeated --attach values at commas. Expand them
    // now so each client file has its own transfer index.
    let values = if flag.name == "attach" {
        value.split(',').map(str::to_owned).collect::<Vec<_>>()
    } else {
        vec![value]
    };
    for value in values {
        add_flag(flag, value, joined.is_some(), plan)?;
    }
    Ok(i)
}

/// `--name=value` as ("name", Some("value")); `-n` and `--name` with no
/// value as ("n", None) and ("name", None).
fn split_flag(arg: &str) -> (&str, Option<&str>) {
    let flag = arg.trim_start_matches('-');
    match flag.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (flag, None),
    }
}

/// Adds one flag to gog's arguments under its canonical name, and notes it
/// as a client file when it names one.
fn add_flag(flag: &Flag, value: String, joined: bool, plan: &mut Plan) -> Result<(), Failure> {
    if flag.value {
        plan.argv.push(format!("--{}", flag.name));
        plan.argv.push(value.clone());
    } else if joined {
        plan.argv.push(format!("--{}={value}", flag.name));
    } else {
        plan.argv.push(format!("--{}", flag.name));
    }
    let index = plan.argv.len() - 1;
    if INPUT_FLAGS.contains(&flag.name.as_str()) {
        // To gog, `-` means standard input, which is not a client file.
        if value == "-" {
            return Err(denied());
        }
        plan.inputs.push(LocalFile {
            index,
            path: value.clone(),
            directory: false,
        });
    }
    if OUTPUT_FLAGS.contains(&flag.name.as_str()) {
        plan.outputs.push(LocalFile {
            index,
            path: value.clone(),
            directory: flag.name == "out-dir",
        });
    }
    plan.flags.push((flag.name.clone(), value));
    Ok(())
}

/// A group that ended without a verb runs its default one, as in gog.
fn add_default_verb(node: &'static Node, plan: &mut Plan) -> &'static Node {
    if node.command.is_none()
        && let Some(verb) = node.default_child()
    {
        plan.argv.push(verb.name.clone());
        return verb;
    }
    node
}

/// `gmail attachment` without `--out` saves to `attachment.bin` in the
/// client's current directory.
fn add_attachment_output(plan: &mut Plan) {
    if plan.command == "gmail.attachment" && plan.outputs.is_empty() {
        plan.argv.extend(["--out".into(), "attachment.bin".into()]);
        plan.outputs.push(LocalFile {
            index: plan.argv.len() - 1,
            path: "attachment.bin".into(),
            directory: false,
        });
    }
}

/// `--download` without `--out-dir` lets gog name the files; they land in
/// the client's current directory.
fn add_download_output(plan: &mut Plan) {
    if plan.flag("download") == Some("true") && plan.outputs.is_empty() {
        plan.outputs.push(LocalFile {
            index: usize::MAX,
            path: ".".into(),
            directory: true,
        });
    }
}

/// Calendar writes tell attendees (`--send-updates all`) unless the agent
/// chose a mode.
fn add_calendar_notifications(node: &Node, plan: &mut Plan) {
    // Some calendar verbs (RSVP) do not expose a notification flag.
    if plan.command.starts_with("calendar.")
        && node.flags.iter().any(|flag| flag.name == "send-updates")
        && plan.flag("send-updates").is_none()
    {
        plan.argv.extend(["--send-updates".into(), "all".into()]);
        plan.flags.push(("send-updates".into(), "all".into()));
    }
}

/// A calendar event list reads `CALENDAR_EVENTS_MAX` events unless the agent
/// chose a number: with gog's default, a cut-off week read as free time.
fn add_calendar_list_max(plan: &mut Plan) {
    if plan.command == "calendar.events" && !plan.help && plan.flag("max").is_none() {
        plan.argv
            .extend(["--max".into(), CALENDAR_EVENTS_MAX.into()]);
        plan.flags.push(("max".into(), CALENDAR_EVENTS_MAX.into()));
    }
}

fn denied() -> Failure {
    Failure::new(
        Exit::Denied,
        "not_allowed",
        "Command or flag is outside the allowed Google interface".into(),
    )
}

/// Every command in the grammar, as dotted names. gog is started with
/// exactly these enabled, as a second fence behind `parse`.
pub fn commands() -> Vec<String> {
    allowed()
        .into_iter()
        .map(|allowed| allowed.command.to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plan(args: &[&str]) -> Result<Plan, Failure> {
        parse(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }
    #[test]
    fn bulk_and_tracking_flags_cannot_expand_a_named_message_action() {
        for args in [
            vec!["gmail", "archive", "message", "--thread"],
            vec!["gmail", "archive", "--query", "marker"],
            vec!["gmail", "mark-read", "--query=marker"],
            vec!["gmail", "unread", "-q", "marker"],
            vec!["gmail", "send", "--track"],
            vec!["gmail", "send", "--track-split"],
        ] {
            assert!(
                matches!(plan(&args), Err(Failure { code, .. }) if code == "not_allowed"),
                "{args:?}"
            );
        }
    }
    #[test]
    fn calendar_attendee_changes_are_audited_as_recipients() {
        let audit = plan(&[
            "calendar",
            "update",
            "primary",
            "event",
            "--add-attendee",
            "guest@example.com",
        ])
        .unwrap()
        .audit();
        assert_eq!(audit["recipients"], json!(["guest@example.com"]));
    }
    #[test]
    fn default_thread_get_keeps_its_native_positional_alias() {
        let plan = plan(&["gmail", "thread", "test-message-thread"]).unwrap();
        assert_eq!(plan.command, "gmail.thread.get");
        assert_eq!(
            plan.argv,
            vec!["gmail", "thread", "get", "test-message-thread"]
        );
    }
    #[test]
    fn comma_separated_attachments_are_individual_client_files() {
        let plan = plan(&["gmail", "send", "--attach", "first.pdf,second.pdf"]).unwrap();
        assert_eq!(
            plan.inputs
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec!["first.pdf", "second.pdf"]
        );
        assert_ne!(plan.inputs[0].index, plan.inputs[1].index);
    }
    #[test]
    fn aliases_and_file_values_are_parsed_as_grammar_not_command_words() {
        let p = plan(&[
            "mail",
            "send",
            "--to",
            "a@example.com",
            "--body",
            "gmail watch",
            "--attach=x.pdf",
        ])
        .unwrap();
        assert_eq!(p.command, "gmail.send");
        assert_eq!(p.inputs[0].path, "x.pdf");
        assert!(!p.audit().to_string().contains("gmail watch"));
        assert_eq!(plan(&["cal", "ls"]).unwrap().command, "calendar.events");
    }
    #[test]
    fn exclusions_and_safety_overrides_are_denied_including_aliases() {
        for args in [
            vec!["gmail", "watch"],
            vec!["gmail", "settings", "filters", "list"],
            vec!["calendar", "acl"],
            vec!["drive", "ls"],
            vec!["gmail", "attachment", "x", "y", "--inline=false"],
            vec!["gmail", "search", "x", "--wrap-untrusted=false"],
            vec!["gmail", "search", "x", "--base-url", "https://example.com"],
            vec!["gmail", "search", "x", "-p"],
            vec!["gmail", "search", "x", "--noninteractive=false"],
            // A raw message carries recipients the audit could not see.
            vec!["gmail", "send", "--raw-file", "message.eml"],
            vec!["gmail", "drafts", "create", "--raw-file=message.eml"],
        ] {
            assert_eq!(plan(&args).unwrap_err().code, "not_allowed");
        }
    }
    #[test]
    fn google_help_prints_the_agent_guide_which_names_the_calendar_shape() {
        assert!(allowed_help().ends_with(GUIDE));
        let guide = GUIDE;
        for needed in [
            "Take every day name you write from `startDayOfWeek`",
            "truncated: {omitted, after}",
            "\"Am I free at 2pm?\" is a busy-time read.",
            "A `gmail search` row is a THREAD, not a message.",
            "--confirm-conflict",
            "calendar_conflict",
        ] {
            assert!(guide.contains(needed), "{needed}");
        }
        // Every command the guide's examples run is one latch allows.
        for line in guide.lines() {
            if let Some(example) = line.trim().strip_prefix("latch google -- ") {
                let args: Vec<String> = example.split(' ').map(str::to_owned).collect();
                if args != ["accounts"] {
                    parse(
                        &args[..args
                            .iter()
                            .position(|a| a.starts_with('-'))
                            .unwrap_or(args.len())],
                    )
                    .unwrap_or_else(|_| panic!("{example}"));
                }
            }
        }
    }
    #[test]
    fn latchs_own_flags_are_lifted_from_anywhere_in_gogs_arguments() {
        let args = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        let lifted = lift(&args(&[
            "calendar",
            "create",
            "primary",
            "--account",
            "a@example.com",
            "--confirm-conflict",
        ]))
        .unwrap();
        assert_eq!(lifted.args, args(&["calendar", "create", "primary"]));
        assert_eq!(lifted.accounts, Some(vec!["a@example.com".to_owned()]));
        assert!(lifted.confirm_conflict);
        // The last one wins, as with gog's flags.
        let lifted = lift(&args(&[
            "gmail",
            "search",
            "q",
            "-a",
            "a@x",
            "--account=b@x,c@x",
        ]))
        .unwrap();
        assert_eq!(
            lifted.accounts,
            Some(vec!["b@x".to_owned(), "c@x".to_owned()])
        );
        // After a bare `--` everything is gog's.
        let lifted = lift(&args(&["gmail", "search", "--", "--account", "x"])).unwrap();
        assert_eq!(
            lifted.args,
            args(&["gmail", "search", "--", "--account", "x"])
        );
        assert_eq!(lifted.accounts, None);
        for empty in [
            &["--account"][..],
            &["--account="],
            &["-a"],
            &["-a="],
            &["--account", ","],
        ] {
            assert_eq!(lift(&args(empty)).err().expect("refused").code, "usage");
        }
    }
    #[test]
    fn a_groups_help_runs_and_json_changes_nothing() {
        let help = plan(&["mail", "--help"]).unwrap();
        assert!(help.help && help.read_only());
        assert_eq!(help.command, "gmail");
        assert_eq!(help.argv, ["gmail", "--help"]);
        // Without --help a group alone still names no command.
        assert_eq!(plan(&["gmail"]).unwrap_err().code, "not_allowed");
        let json = plan(&["gmail", "search", "--json", "q", "-j"]).unwrap();
        assert_eq!(json.argv, ["gmail", "search", "q"]);
        // Before the command path it is refused, as every flag there is.
        assert_eq!(
            plan(&["--json", "gmail", "search", "q"]).unwrap_err().code,
            "not_allowed"
        );
        // After `--` it is a search word, passed on.
        assert_eq!(
            plan(&["gmail", "search", "--", "--json"]).unwrap().argv,
            ["gmail", "search", "--", "--json"]
        );
    }
    #[test]
    fn a_calendar_list_reads_a_hundred_events_unless_told() {
        let max = |words: &[&str]| plan(words).unwrap().flag("max").map(str::to_owned);
        assert_eq!(
            max(&["calendar", "events"]).as_deref(),
            Some(CALENDAR_EVENTS_MAX)
        );
        assert_eq!(max(&["cal", "ls", "--limit", "5"]).as_deref(), Some("5"));
        assert_eq!(max(&["gmail", "search", "q"]), None);
        assert_eq!(max(&["calendar", "events", "--help"]), None);
    }
    #[test]
    fn calendar_writes_notify_by_default_and_keep_explicit_mode() {
        assert_eq!(
            plan(&["calendar", "create", "primary"])
                .unwrap()
                .flag("send-updates"),
            Some("all")
        );
        assert_eq!(
            plan(&["calendar", "delete", "primary", "id", "--send-updates=none"])
                .unwrap()
                .flag("send-updates"),
            Some("none")
        );
    }
}
