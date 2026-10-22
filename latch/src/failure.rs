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
            details: Value::Null,
        }
    }

    pub fn failed(code: &str, message: String) -> Failure {
        Failure::new(Exit::Failed, code, message)
    }

    pub fn usage(code: &str, message: String) -> Failure {
        Failure::new(Exit::Usage, code, message)
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
    ("back_unknown", Exit::Failed),
    ("bad_arguments", Exit::Usage),
    ("bad_command", Exit::Usage),
    ("bad_directory", Exit::Failed),
    ("bad_home", Exit::Failed),
    ("bad_host_settings", Exit::Failed),
    ("bad_import", Exit::Usage),
    ("bad_item", Exit::Failed),
    ("bad_item", Exit::Usage),
    ("bad_key", Exit::Usage),
    ("bad_path", Exit::Denied),
    ("bad_path", Exit::Failed),
    ("bad_pick", Exit::Usage),
    ("bad_profile", Exit::Usage),
    ("bad_relay", Exit::Usage),
    ("bad_request", Exit::Failed),
    ("bad_request", Exit::Usage),
    ("bad_response", Exit::Failed),
    ("bad_response", Exit::Unknown),
    ("bad_rules", Exit::Usage),
    ("bad_secret_env", Exit::Failed),
    ("bad_serve_args", Exit::Usage),
    ("bad_state", Exit::Failed),
    ("bad_target", Exit::Denied),
    ("bad_target", Exit::Usage),
    ("bad_test_access", Exit::Usage),
    ("bad_trace", Exit::Usage),
    ("browser_gone", Exit::Failed),
    ("calendar_conflict", Exit::Denied),
    ("caller_denied", Exit::Denied),
    ("checksum_mismatch", Exit::Failed),
    ("code_expired", Exit::Failed),
    ("confirm_needed", Exit::Denied),
    ("confirm_stale", Exit::Denied),
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
    ("host_offline", Exit::Unreachable),
    ("host_required", Exit::Usage),
    ("host_running", Exit::Failed),
    ("host_unreachable", Exit::Unreachable),
    ("host_upgraded", Exit::Failed),
    ("import_expired", Exit::Usage),
    ("invalid_request", Exit::Usage),
    ("io_error", Exit::Failed),
    ("iroh", Exit::Failed),
    ("is_a_directory", Exit::Failed),
    ("journal_failed", Exit::Failed),
    ("key_mismatch", Exit::Failed),
    ("keychain_failed", Exit::Failed),
    ("last_tab", Exit::Usage),
    ("launch_busy", Exit::Failed),
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
    ("no_session", Exit::Usage),
    ("no_state_dir", Exit::Usage),
    ("no_such_agent", Exit::Usage),
    ("no_such_chat", Exit::Usage),
    ("no_such_download", Exit::Usage),
    ("no_such_field", Exit::Usage),
    ("no_such_item", Exit::Usage),
    ("no_such_request", Exit::Failed),
    ("no_such_session", Exit::Failed),
    ("no_such_shot", Exit::Failed),
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
    ("profile_busy", Exit::Failed),
    ("profile_engine_mismatch", Exit::Usage),
    ("profile_failed", Exit::Failed),
    ("protocol_mismatch", Exit::Failed),
    ("rate_limited", Exit::Failed),
    ("reader_failed", Exit::Failed),
    ("reader_timeout", Exit::Failed),
    ("relay_unreachable", Exit::Failed),
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
    ("site_busy", Exit::Failed),
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
    ("timeout", Exit::Failed),
    ("tmpdir_failed", Exit::Failed),
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

    /// The codes this crate's source gives a failure, with their exits:
    /// `Failure::usage("code"`, `Failure::failed("code"`, `Done::failed("code"`
    /// and `Exit::Name, "code"`. Test code is left out.
    fn named_in_source() -> Vec<(String, String)> {
        let mut named = Vec::new();
        let mut folders = vec![std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src"
        ))];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).unwrap() {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                if path.is_dir() {
                    if name != "tests" {
                        folders.push(path);
                    }
                    continue;
                }
                if !name.ends_with(".rs") || name.ends_with("tests.rs") || name == "failure.rs" {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                let text = text.split("#[cfg(test)]\nmod tests").next().unwrap();
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
        }
        named
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
