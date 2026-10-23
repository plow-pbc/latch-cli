//! How a command fails: an exit code the agent can branch on, plus a short
//! machine-readable code and a human message, printed as JSON on stderr.

use serde_json::Value;
use std::process::ExitCode;
use std::time::Duration;

/// What the agent should do after a failure, with stable numeric exit codes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Exit {
    Failed,
    Usage,
    Unreachable,
    Denied,
    Unknown,
}

impl Exit {
    pub const ALL: [Exit; 5] = [
        Exit::Failed,
        Exit::Usage,
        Exit::Unreachable,
        Exit::Denied,
        Exit::Unknown,
    ];

    /// What the agent should do, as `latch help --json` tells it.
    pub fn meaning(self) -> &'static str {
        match self {
            Exit::Failed => "The operation failed; read the error.",
            Exit::Usage => {
                "The command line is wrong, or the host is unknown or ambiguous; fix the command."
            }
            Exit::Unreachable => {
                "The host could not be reached, or could not judge the call. Its message says what fixes it: often a retry later. A browser plan may have run steps before it stopped: read its `steps` and `stop` and send only what is left."
            }
            Exit::Denied => {
                "Refused; don't retry, except: gatekeeper_denied with reason not_covered (ask the owner in your Plow chat, naming exactly what you will do, and retry once they agree); confirm_needed (if the task needs it, carry it out with `latch browser do --confirm C`); calendar_conflict (ask the owner; once they agree, resend with --confirm-conflict)."
            }
            Exit::Unknown => {
                "The connection was lost after the request was sent; check before resending."
            }
        }
    }
}

/// Numeric codes shared by the process exit and the wire protocol.
impl From<Exit> for u8 {
    fn from(exit: Exit) -> u8 {
        match exit {
            Exit::Failed => 1,
            Exit::Usage => 2,
            Exit::Unreachable => 3,
            Exit::Denied => 4,
            Exit::Unknown => 5,
        }
    }
}
impl TryFrom<u8> for Exit {
    type Error = &'static str;
    fn try_from(code: u8) -> Result<Self, Self::Error> {
        match code {
            1 => Ok(Self::Failed),
            2 => Ok(Self::Usage),
            3 => Ok(Self::Unreachable),
            4 => Ok(Self::Denied),
            5 => Ok(Self::Unknown),
            _ => Err("invalid exit code"),
        }
    }
}
impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> ExitCode {
        ExitCode::from(u8::from(exit))
    }
}

/// Everything a failed command reports. Commands return `Result<_, Failure>`
/// and `main` turns the failure into stderr output and the exit code.
#[derive(Debug)]
pub struct Failure {
    pub exit: Exit,
    pub code: String,
    pub message: String,
    /// How long Plow asked us to wait (its `Retry-After` header), if it did.
    /// Only the host's heartbeat uses it; it is not printed.
    pub retry_after: Option<Duration>,
    /// The one command or action that fixes it, when there is one. (Boxed,
    /// so every `Result<_, Failure>` stays small.)
    pub next: Option<Box<str>>,
    /// Already said to the person who ran a human command, as a plain line
    /// on their terminal: nothing more is printed for it.
    pub said: bool,
    pub details: Value,
}

impl Failure {
    pub fn new(exit: Exit, code: &str, message: String) -> Failure {
        Failure {
            exit,
            code: code.to_string(),
            message,
            retry_after: None,
            next: None,
            said: false,
            details: Value::Null,
        }
    }

    pub fn failed(code: &str, message: String) -> Failure {
        Failure::new(Exit::Failed, code, message)
    }

    pub fn usage(code: &str, message: String) -> Failure {
        Failure::new(Exit::Usage, code, message)
    }

    /// For a human command that fails once it has talked to the person:
    /// says the message to them in a plain line, and nothing more is printed.
    pub fn told(mut self) -> Failure {
        eprintln!("{}", self.message);
        self.said = true;
        self
    }

    /// `{"error": {"code", "message", "next", "exit", ...details}}`, one
    /// line. The details go in first, so none of them can stand in for the
    /// code, message, next step or exit.
    pub fn to_json(&self) -> String {
        let mut error = match &self.details {
            Value::Object(details) => details.clone(),
            _ => serde_json::Map::new(),
        };
        error.insert("code".into(), self.code.clone().into());
        error.insert("message".into(), self.message.clone().into());
        if let Some(next) = &self.next {
            error.insert("next".into(), next.to_string().into());
        }
        error.insert("exit".into(), u8::from(self.exit).into());
        serde_json::json!({ "error": error }).to_string()
    }
}

/// Every error code latch reports, with the exit it comes with, as
/// `latch help --json` lists them. A code reported with two exits is listed
/// once for each. Besides the codes latch's own source names, this holds
/// the ones its workers and helper programs (the Camoufox helper, the
/// Messages reader) report, which reach the agent unchanged.
pub const CODES: &[(&str, Exit)] = &[
    ("account_required", Exit::Usage),
    ("addressbook_missing", Exit::Failed),
    ("addressbook_not_granted", Exit::Failed),
    ("addressbook_unreadable", Exit::Failed),
    ("addressbook_version", Exit::Failed),
    ("already_serving", Exit::Failed),
    ("ambiguous_agent", Exit::Usage),
    ("ambiguous_chat", Exit::Usage),
    ("audit_broken", Exit::Failed),
    ("audit_failed", Exit::Failed),
    ("audit_io", Exit::Failed),
    ("audit_key", Exit::Failed),
    ("bad_app_signature", Exit::Denied),
    ("bad_arguments", Exit::Usage),
    ("bad_command", Exit::Usage),
    ("bad_directory", Exit::Failed),
    ("bad_download", Exit::Failed),
    ("bad_engine", Exit::Usage),
    ("bad_home", Exit::Failed),
    ("bad_host_settings", Exit::Failed),
    ("bad_import", Exit::Usage),
    ("bad_item", Exit::Failed),
    ("bad_item", Exit::Usage),
    ("bad_key", Exit::Usage),
    ("bad_path", Exit::Denied),
    ("bad_path", Exit::Failed),
    ("bad_pick", Exit::Usage),
    ("bad_relay", Exit::Usage),
    ("bad_release", Exit::Failed),
    ("bad_request", Exit::Failed),
    ("bad_request", Exit::Usage),
    ("bad_response", Exit::Failed),
    ("bad_response", Exit::Unknown),
    ("bad_rules", Exit::Usage),
    ("bad_secret_env", Exit::Usage),
    ("bad_serve_args", Exit::Usage),
    ("bad_signature", Exit::Denied),
    ("bad_state", Exit::Failed),
    ("bad_tag", Exit::Usage),
    ("bad_target", Exit::Denied),
    ("bad_target", Exit::Usage),
    ("bad_test_access", Exit::Usage),
    ("bad_trace", Exit::Usage),
    ("browser_gone", Exit::Failed),
    ("calendar_conflict", Exit::Denied),
    ("caller_denied", Exit::Denied),
    ("check_not_met", Exit::Failed),
    ("checksum_mismatch", Exit::Failed),
    ("code_expired", Exit::Failed),
    ("confirm_needed", Exit::Denied),
    ("confirm_needed", Exit::Usage),
    ("confirm_stale", Exit::Denied),
    ("confirm_stale", Exit::Usage),
    ("confirmation_required", Exit::Usage),
    ("conflict", Exit::Usage),
    ("connection_lost", Exit::Failed),
    ("connection_lost", Exit::Unknown),
    ("context_changed", Exit::Failed),
    ("covered_target", Exit::Failed),
    ("destination_changed", Exit::Denied),
    ("dialog_open", Exit::Failed),
    ("display_failed", Exit::Failed),
    ("download_failed", Exit::Failed),
    ("download_incomplete", Exit::Failed),
    ("duplicate_request", Exit::Usage),
    ("editor_failed", Exit::Failed),
    ("empty_rules", Exit::Usage),
    ("engine_error", Exit::Failed),
    ("engine_failed", Exit::Failed),
    ("engine_not_installed", Exit::Failed),
    ("engine_unverified", Exit::Denied),
    ("engine_unverified", Exit::Failed),
    ("eval_failed", Exit::Failed),
    ("exec_denied", Exit::Denied),
    ("expired", Exit::Failed),
    ("failed", Exit::Failed),
    ("file_too_large", Exit::Usage),
    ("fill_refused", Exit::Denied),
    ("gatekeeper_denied", Exit::Denied),
    ("gatekeeper_unavailable", Exit::Unreachable),
    ("get_form", Exit::Denied),
    ("gog_not_configured", Exit::Failed),
    ("gog_not_executable", Exit::Failed),
    ("gog_unverified", Exit::Denied),
    ("gone", Exit::Failed),
    ("google_failed", Exit::Failed),
    ("google_not_connected", Exit::Failed),
    ("google_timeout", Exit::Failed),
    ("google_worker_failed", Exit::Failed),
    ("handoff_failed", Exit::Failed),
    ("heartbeat", Exit::Failed),
    ("host_gone", Exit::Failed),
    ("host_not_supported", Exit::Usage),
    ("host_offline", Exit::Unreachable),
    ("host_required", Exit::Usage),
    ("host_running", Exit::Failed),
    ("host_unreachable", Exit::Unreachable),
    ("host_upgraded", Exit::Failed),
    ("import_expired", Exit::Usage),
    ("install_failed", Exit::Failed),
    ("invalid_request", Exit::Usage),
    ("io_error", Exit::Failed),
    ("iroh", Exit::Failed),
    ("is_a_directory", Exit::Failed),
    ("journal_failed", Exit::Failed),
    ("key_mismatch", Exit::Failed),
    ("keychain_failed", Exit::Failed),
    ("last_tab", Exit::Usage),
    ("launch_busy", Exit::Failed),
    ("local_build", Exit::Failed),
    ("local_file", Exit::Failed),
    ("local_file", Exit::Usage),
    ("local_socket", Exit::Failed),
    ("log_io", Exit::Failed),
    ("login_collected", Exit::Failed),
    ("login_not_found", Exit::Failed),
    ("messages_missing", Exit::Failed),
    ("messages_not_installed", Exit::Failed),
    ("messages_unavailable", Exit::Failed),
    ("messages_unverified", Exit::Denied),
    ("missing_field", Exit::Usage),
    ("model_unavailable", Exit::Failed),
    ("name_moved", Exit::Failed),
    ("navigation_failed", Exit::Failed),
    ("needs_fields", Exit::Failed),
    ("needs_owner", Exit::Failed),
    ("needs_setup", Exit::Failed),
    ("no_answer", Exit::Failed),
    ("no_answer", Exit::Unknown),
    ("no_binary", Exit::Failed),
    ("no_credential", Exit::Usage),
    ("no_dialog", Exit::Failed),
    ("no_file_name", Exit::Usage),
    ("no_handoff", Exit::Failed),
    ("no_history", Exit::Failed),
    ("no_host", Exit::Usage),
    ("no_intent", Exit::Denied),
    ("no_session", Exit::Usage),
    ("no_state_dir", Exit::Usage),
    ("no_such_agent", Exit::Usage),
    ("no_such_chat", Exit::Usage),
    ("no_such_download", Exit::Usage),
    ("no_such_field", Exit::Usage),
    ("no_such_item", Exit::Usage),
    ("no_such_release", Exit::Failed),
    ("no_such_request", Exit::Usage),
    ("no_such_session", Exit::Failed),
    ("no_such_shot", Exit::Usage),
    ("no_such_tab", Exit::Usage),
    ("no_such_target", Exit::Failed),
    ("no_terminal", Exit::Usage),
    ("not_a_file", Exit::Usage),
    ("not_allowed", Exit::Denied),
    ("not_an_agent", Exit::Usage),
    ("not_built_yet", Exit::Failed),
    ("not_configured", Exit::Usage),
    ("not_enabled", Exit::Denied),
    ("not_focused", Exit::Failed),
    ("not_found", Exit::Failed),
    ("not_logged_in", Exit::Failed),
    ("not_logged_in", Exit::Usage),
    ("not_on_a_phone_link", Exit::Denied),
    ("not_owner", Exit::Denied),
    ("not_permitted", Exit::Denied),
    ("not_removed", Exit::Usage),
    ("nothing_to_start_again", Exit::Usage),
    ("nothing_typed", Exit::Usage),
    ("origin_mismatch", Exit::Denied),
    ("other_version", Exit::Unreachable),
    ("outcome_unknown", Exit::Unknown),
    ("output_too_large", Exit::Failed),
    ("own_machine", Exit::Usage),
    ("owner_only", Exit::Denied),
    ("plow_denied", Exit::Denied),
    ("plow_error", Exit::Failed),
    ("plow_rejected", Exit::Failed),
    ("plow_unreachable", Exit::Failed),
    ("press_required", Exit::Denied),
    ("profile_failed", Exit::Failed),
    ("protocol_mismatch", Exit::Failed),
    ("rate_limited", Exit::Failed),
    ("reader_failed", Exit::Failed),
    ("reader_timeout", Exit::Failed),
    ("relay_unreachable", Exit::Failed),
    ("releases_unreachable", Exit::Unreachable),
    ("removed_command", Exit::Usage),
    ("rules_io", Exit::Failed),
    ("rules_too_long", Exit::Usage),
    ("sandbox_denied", Exit::Denied),
    ("sandbox_failed", Exit::Failed),
    ("sandbox_unavailable", Exit::Failed),
    ("secret_field", Exit::Denied),
    ("secret_in_argv", Exit::Usage),
    ("secret_on_page", Exit::Denied),
    ("select_failed", Exit::Failed),
    ("serve_failed", Exit::Failed),
    ("serve_required", Exit::Unreachable),
    ("service_failed", Exit::Failed),
    ("session_paused", Exit::Failed),
    ("setup_failed", Exit::Failed),
    ("setup_superseded", Exit::Failed),
    ("signal", Exit::Failed),
    ("split_mismatch", Exit::Failed),
    ("stale_revision", Exit::Denied),
    ("stale_revision", Exit::Failed),
    ("stale_snapshot", Exit::Failed),
    ("state_busy", Exit::Failed),
    ("state_dir_too_long", Exit::Usage),
    ("state_format", Exit::Failed),
    ("state_io", Exit::Failed),
    ("stdin_is_terminal", Exit::Usage),
    ("stdin_not_judgeable", Exit::Usage),
    ("stdin_timeout", Exit::Usage),
    ("stdin_too_big", Exit::Usage),
    ("stdin_unreadable", Exit::Usage),
    ("stop_failed", Exit::Failed),
    ("store_failed", Exit::Failed),
    ("store_rejected", Exit::Failed),
    ("timed_out", Exit::Failed),
    ("timeout", Exit::Failed),
    ("tmpdir_failed", Exit::Failed),
    ("tool_failed", Exit::Failed),
    ("transfer_failed", Exit::Failed),
    ("transfer_failed", Exit::Unknown),
    ("transfer_incomplete", Exit::Failed),
    ("unknown_account", Exit::Usage),
    ("unknown_command", Exit::Usage),
    ("unknown_host", Exit::Usage),
    ("unknown_plugin", Exit::Usage),
    ("unsafe_output", Exit::Failed),
    ("unsupported_capability", Exit::Failed),
    ("unsupported_platform", Exit::Failed),
    ("update_io", Exit::Failed),
    ("upload_failed", Exit::Failed),
    ("url_refused", Exit::Denied),
    ("usage", Exit::Usage),
    ("vague_condition", Exit::Usage),
    ("vault_failed", Exit::Failed),
    ("vault_version", Exit::Failed),
    ("watch_failed", Exit::Failed),
    ("which_chat", Exit::Usage),
    ("which_handoff", Exit::Usage),
    ("workdir_outside_areas", Exit::Denied),
    ("worker_failed", Exit::Failed),
    ("write_path_failed", Exit::Failed),
    ("wrong_target", Exit::Usage),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The codes this crate's production source gives a failure, with their
    /// exits: `Failure::usage("code"`, `Failure::failed("code"`,
    /// `Done::failed("code"` and `Exit::Name, "code"`. Everything under
    /// `#[cfg(test)]` is left out: the items it marks, and the files of the
    /// modules it declares.
    fn named_in_source() -> Vec<(String, String)> {
        let src = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
        let mut files = Vec::new();
        let mut folders = vec![src.to_path_buf()];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    folders.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    files.push(path);
                }
            }
        }
        let mut production = Vec::new();
        let mut test_only = Vec::new();
        for path in &files {
            let (text, declared) = without_tests(&std::fs::read_to_string(path).unwrap());
            // `mod name;` in a.rs is a/name.rs; in main.rs or mod.rs, name.rs beside it.
            let stem = path.file_stem().unwrap();
            let folder = match stem.to_str() {
                Some("main" | "mod") => path.parent().unwrap().to_path_buf(),
                _ => path.with_extension(""),
            };
            test_only.extend(declared.iter().map(|name| folder.join(name)));
            production.push((path, text));
        }
        let mut named = Vec::new();
        for (path, text) in production {
            let tested = test_only
                .iter()
                .any(|module| path.with_extension("") == *module || path.starts_with(module));
            if tested || path.ends_with("failure.rs") {
                continue;
            }
            for (call, exit) in [
                ("Failure::usage(", "Usage"),
                ("Failure::failed(", "Failed"),
                ("Done::failed(", "Failed"),
            ] {
                for (at, _) in text.match_indices(call) {
                    if let Some(code) = code_literal(&text[at + call.len()..]) {
                        named.push((code, exit.to_string()));
                    }
                }
            }
            for (at, _) in text.match_indices("Exit::") {
                let rest = &text[at + "Exit::".len()..];
                let exit: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphabetic())
                    .collect();
                let Some(rest) = rest[exit.len()..].trim_start().strip_prefix(',') else {
                    continue;
                };
                if let Some(code) = code_literal(rest) {
                    named.push((code, exit));
                }
            }
        }
        named
    }

    /// `text` without the items `#[cfg(test)]` marks, and the names of the
    /// modules among them that live in files of their own (`mod name;`).
    fn without_tests(text: &str) -> (String, Vec<String>) {
        const MARK: &str = "#[cfg(test)]";
        let (mut kept, mut declared) = (String::new(), Vec::new());
        let mut rest = text;
        while let Some(at) = rest.find(MARK) {
            kept.push_str(&rest[..at]);
            let item = &rest[at + MARK.len()..];
            let end = item_end(item);
            let words: Vec<&str> = item[..end].split_whitespace().collect();
            if let [.., "mod", name] = words.as_slice() {
                declared.push(name.trim_end_matches(';').to_string());
            }
            rest = &item[end..];
        }
        kept.push_str(rest);
        (kept, declared)
    }

    /// Where the item `text` starts with ends: after its first `;` outside
    /// any braces, or after the brace that closes its first block. String
    /// and character literals and comments are stepped over, so the braces
    /// in them don't count.
    fn item_end(text: &str) -> usize {
        let bytes = text.as_bytes();
        let (mut at, mut depth) = (0, 0);
        while at < bytes.len() {
            match bytes[at] {
                b'/' if bytes.get(at + 1) == Some(&b'/') => {
                    at += text[at..].find('\n').unwrap_or(text.len() - at);
                }
                b'/' if bytes.get(at + 1) == Some(&b'*') => {
                    at += text[at..].find("*/").map_or(text.len() - at, |end| end + 2);
                    continue;
                }
                b'r' if matches!(bytes.get(at + 1), Some(b'"' | b'#'))
                    && !bytes[at.saturating_sub(1)].is_ascii_alphanumeric() =>
                {
                    let hashes = text[at + 1..].bytes().take_while(|&b| b == b'#').count();
                    let close = format!("\"{}", "#".repeat(hashes));
                    let body = at + 2 + hashes;
                    at = body + text[body..].find(&close).unwrap() + close.len();
                    continue;
                }
                b'"' => {
                    at += 1;
                    while bytes[at] != b'"' {
                        at += if bytes[at] == b'\\' { 2 } else { 1 };
                    }
                }
                // A character literal ('x', '\n', '\u{7b}'), not a lifetime ('a).
                b'\'' if bytes.get(at + 1) == Some(&b'\\') => {
                    at += 2 + text[at + 2..].find('\'').unwrap();
                }
                b'\'' if bytes.get(at + 2) == Some(&b'\'') => at += 2,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return at + 1;
                    }
                }
                b';' if depth == 0 => return at + 1,
                _ => {}
            }
            at += 1;
        }
        text.len()
    }

    /// The string literal `text` starts with (after whitespace), if it reads
    /// as a code: lower-case letters, digits and underscores.
    fn code_literal(text: &str) -> Option<String> {
        let rest = text.trim_start().strip_prefix('"')?;
        let code = &rest[..rest.find('"')?];
        let is_code = !code.is_empty()
            && code
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        is_code.then(|| code.to_string())
    }

    #[test]
    fn every_code_the_source_names_is_listed_with_its_exit() {
        let listed: Vec<(String, String)> = CODES
            .iter()
            .map(|(code, exit)| (code.to_string(), format!("{exit:?}")))
            .collect();
        let named = named_in_source();
        assert!(named.len() > 200, "the scan found only {}", named.len());
        let missing: std::collections::BTreeSet<_> =
            named.iter().filter(|pair| !listed.contains(pair)).collect();
        assert!(missing.is_empty(), "not in CODES: {missing:?}");
        let mut sorted = listed.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, listed, "CODES is sorted, each pair once");
    }

    #[test]
    fn an_error_line_carries_its_exit_and_next_step_over_any_detail() {
        let mut failure = Failure::usage("usage", "Bad flag.".to_string());
        failure.next = Some("latch help status".into());
        failure.details = serde_json::json!({"exit": 0, "code": "other", "request_id": "r1"});
        let line: Value = serde_json::from_str(&failure.to_json()).unwrap();
        assert_eq!(
            line,
            serde_json::json!({"error": {"code": "usage", "message": "Bad flag.",
                "next": "latch help status", "exit": 2, "request_id": "r1"}})
        );
        assert!(!failure.to_json().contains('\n'));
    }
}
