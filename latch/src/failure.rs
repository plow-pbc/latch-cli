//! How a command fails: an exit code the agent can branch on, plus a short
//! machine-readable code and a human message, printed as JSON on stderr.

use serde_json::Value;
use std::process::ExitCode;
use std::time::Duration;

/// What the agent should do after a failure, with stable numeric exit codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    /// What Plow's answer said besides its status, when it said any. Not
    /// printed. (Boxed, so every `Result<_, Failure>` stays small.)
    pub plow: Option<Box<PlowSaid>>,
    /// The one command or action that fixes it, when there is one. (Boxed,
    /// so every `Result<_, Failure>` stays small.)
    pub next: Option<Box<str>>,
    /// Already said to the person who ran a human command, as a plain line
    /// on their terminal: nothing more is printed for it.
    pub said: bool,
    pub details: Value,
    /// Private, so a failure is only built by `new` or `relayed`.
    _sealed: (),
}

#[derive(Debug, Default)]
pub struct PlowSaid {
    /// How long Plow asked us to wait (its `Retry-After` header), if it did.
    /// Only the host's heartbeat uses it.
    pub retry_after: Option<Duration>,
    /// The code Plow put in a refusal's `detail` (`AGENT_EXISTS`), if it
    /// did: a fixed word, never Plow's text.
    pub code: Option<Box<str>>,
}

impl Failure {
    pub fn new(code: Code, message: String) -> Failure {
        Failure::relayed(code.exit, code.name, message)
    }

    /// A failure another program or peer reported, with its code and exit
    /// as it gave them.
    pub fn relayed(exit: Exit, code: &str, message: String) -> Failure {
        Failure {
            exit,
            code: code.to_string(),
            message,
            plow: None,
            next: None,
            said: false,
            details: Value::Null,
            _sealed: (),
        }
    }

    /// A failure a worker or helper program reported by its code: with that
    /// code's exit when latch knows the code, else exit 1.
    pub fn reported(code: &str, message: String) -> Failure {
        match CODES.iter().find(|known| known.name == code) {
            Some(known) => Failure::new(*known, message),
            None => Failure::relayed(Exit::Failed, code, message),
        }
    }

    /// Reports the same failure as `code`, with its exit.
    pub fn recode(&mut self, code: Code) {
        self.exit = code.exit;
        self.code = code.name.to_string();
    }

    /// How long Plow asked us to wait, if it did.
    pub fn retry_after(&self) -> Option<Duration> {
        self.plow.as_ref().and_then(|plow| plow.retry_after)
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
        serde_json::json!({ "error": self.error() }).to_string()
    }

    /// The error line's object: `{"code", "message", "next", "exit",
    /// ...details}`, also a `login --events` error line's `error`.
    pub fn error(&self) -> Value {
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
        Value::Object(error)
    }
}

/// A registered error code and the exit it always comes with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Code {
    pub name: &'static str,
    pub exit: Exit,
}

/// Defines each code once, as a `code::name` constant with its exit, and
/// `CODES`, the list `latch help --json` shows.
macro_rules! codes {
    ($($name:ident: $exit:ident,)*) => {
        #[allow(non_upper_case_globals)]
        pub mod code {
            use super::{Code, Exit};
            $(pub const $name: Code = Code { name: stringify!($name), exit: Exit::$exit };)*
        }
        pub const CODES: &[Code] = &[$(code::$name),*];
    };
}

// Every error code latch reports. Besides latch's own, these are the ones
// its workers and helper programs (the Camoufox helper, the Messages
// reader) report, which reach the agent unchanged.
codes! {
    account_required: Usage,
    addressbook_missing: Failed,
    addressbook_not_granted: Failed,
    addressbook_unreadable: Failed,
    addressbook_version: Failed,
    already_attached: Failed,
    already_serving: Failed,
    already_true: Failed,
    ambiguous_agent: Usage,
    ambiguous_chat: Usage,
    ambiguous_target: Failed,
    app_broken: Failed,
    audit_broken: Failed,
    audit_failed: Failed,
    audit_io: Failed,
    audit_key: Failed,
    bad_app_signature: Denied,
    bad_arguments: Usage,
    bad_command: Usage,
    bad_directory: Failed,
    bad_download: Failed,
    bad_engine: Usage,
    bad_home: Failed,
    bad_host_answer: Failed,
    bad_host_settings: Failed,
    bad_import: Usage,
    bad_item: Usage,
    bad_key: Usage,
    bad_name: Usage,
    bad_path: Denied,
    bad_pick: Usage,
    bad_relay: Usage,
    bad_release: Failed,
    bad_request: Usage,
    bad_response: Unknown,
    bad_rules: Usage,
    bad_secret_env: Usage,
    bad_serve_args: Usage,
    bad_signature: Denied,
    bad_state: Failed,
    bad_tag: Usage,
    bad_target: Usage,
    bad_trace: Usage,
    bad_values: Usage,
    blocked: Failed,
    browser_gone: Failed,
    calendar_conflict: Denied,
    caller_denied: Denied,
    caller_gone: Unknown,
    check_not_met: Failed,
    checksum_mismatch: Failed,
    code_expired: Failed,
    confirm_needed: Denied,
    confirm_stale: Denied,
    confirmation_required: Usage,
    conflict: Usage,
    connection_lost: Unknown,
    context_changed: Failed,
    covered_target: Failed,
    cursor_expired: Usage,
    deadline: Failed,
    destination_changed: Denied,
    dialog_open: Failed,
    display_failed: Failed,
    download_failed: Failed,
    download_incomplete: Failed,
    duplicate_request: Usage,
    editor_failed: Failed,
    empty_rules: Usage,
    engine_error: Failed,
    engine_failed: Failed,
    engine_not_installed: Failed,
    engine_unverified: Denied,
    eval_failed: Failed,
    exec_denied: Denied,
    expired: Failed,
    failed: Failed,
    file_too_large: Usage,
    fill_refused: Denied,
    gatekeeper_denied: Denied,
    gatekeeper_unavailable: Unreachable,
    get_form: Denied,
    gog_not_configured: Failed,
    gog_not_executable: Failed,
    gog_unverified: Denied,
    gone: Failed,
    google_failed: Failed,
    google_connect_cancelled: Failed,
    google_not_connected: Failed,
    google_timeout: Failed,
    google_worker_failed: Failed,
    google_write_timeout: Unknown,
    handoff_failed: Failed,
    heartbeat: Failed,
    host_gone: Failed,
    host_not_supported: Usage,
    host_offline: Unreachable,
    host_outdated: Usage,
    host_required: Usage,
    host_running: Failed,
    host_stopped: Unreachable,
    host_unreachable: Unreachable,
    host_upgraded: Failed,
    import_expired: Usage,
    install_failed: Failed,
    interrupted: Unknown,
    invalid_request: Usage,
    io_error: Failed,
    iroh: Failed,
    is_a_directory: Failed,
    journal_failed: Failed,
    key_mismatch: Failed,
    keychain_failed: Failed,
    last_tab: Usage,
    launch_busy: Failed,
    line_taken: Usage,
    link_expired: Denied,
    link_used: Denied,
    local_build: Failed,
    local_file: Usage,
    local_socket: Failed,
    log_io: Failed,
    login_collected: Failed,
    login_not_found: Failed,
    messages_missing: Failed,
    messages_not_granted: Failed,
    messages_not_installed: Failed,
    messages_unavailable: Failed,
    messages_unreadable: Failed,
    messages_unverified: Denied,
    missing_field: Usage,
    model_unavailable: Failed,
    name_moved: Failed,
    navigation_failed: Failed,
    needs_fields: Failed,
    needs_login: Failed,
    needs_owner: Failed,
    needs_setup: Failed,
    needs_text: Failed,
    no_answer: Unknown,
    no_binary: Failed,
    no_credential: Usage,
    no_dialog: Failed,
    no_file_name: Usage,
    no_handoff: Failed,
    no_history: Failed,
    no_host: Usage,
    no_host_here: Usage,
    no_progress: Failed,
    no_session: Usage,
    no_state_dir: Usage,
    no_such_agent: Usage,
    no_such_chat: Usage,
    no_such_download: Usage,
    no_such_field: Usage,
    no_such_item: Usage,
    no_such_line: Usage,
    no_such_link: Denied,
    no_such_release: Failed,
    no_such_request: Usage,
    no_such_session: Failed,
    no_such_shot: Usage,
    no_such_tab: Usage,
    no_such_target: Failed,
    no_terminal: Usage,
    not_a_file: Usage,
    not_allowed: Denied,
    not_an_agent: Usage,
    not_attached: Failed,
    not_built_yet: Failed,
    not_configured: Usage,
    not_enabled: Denied,
    not_focused: Failed,
    not_found: Failed,
    not_logged_in: Usage,
    not_on_a_phone_link: Denied,
    not_owner: Denied,
    not_permitted: Denied,
    not_previewable: Usage,
    not_removed: Usage,
    not_this_host: Failed,
    nothing_to_start_again: Usage,
    nothing_typed: Usage,
    origin_mismatch: Denied,
    other_version: Unreachable,
    outcome_unknown: Unknown,
    output_too_large: Failed,
    own_machine: Usage,
    owner_only: Denied,
    plan_budget: Failed,
    plow_chat_not_supported: Usage,
    plow_denied: Denied,
    plow_error: Failed,
    plow_rejected: Failed,
    plow_unreachable: Failed,
    precondition_failed: Failed,
    press_required: Denied,
    profile_failed: Failed,
    program_not_found: Failed,
    protocol_mismatch: Failed,
    rate_limited: Failed,
    read_failed: Failed,
    reader_failed: Failed,
    reader_timeout: Failed,
    record_too_large: Failed,
    relay_unreachable: Failed,
    releases_unreachable: Unreachable,
    removed_command: Usage,
    repeat: Failed,
    request_too_large: Failed,
    rules_io: Failed,
    rules_too_long: Usage,
    sandbox_denied: Denied,
    sandbox_failed: Failed,
    sandbox_unavailable: Failed,
    secret_field: Denied,
    secret_in_argv: Usage,
    secret_on_page: Denied,
    select_failed: Failed,
    serve_failed: Failed,
    serve_required: Unreachable,
    service_failed: Failed,
    session_paused: Failed,
    setup_failed: Failed,
    setup_superseded: Failed,
    sign_in_timeout: Failed,
    signal: Failed,
    split_mismatch: Failed,
    stale_revision: Denied,
    stale_snapshot: Failed,
    state_busy: Failed,
    state_dir: Failed,
    state_dir_too_long: Usage,
    state_format: Failed,
    state_io: Failed,
    stdin_is_terminal: Usage,
    stdin_not_judgeable: Usage,
    stdin_timeout: Usage,
    stdin_too_big: Usage,
    stdin_unreadable: Usage,
    step_budget: Failed,
    stop_failed: Failed,
    store_failed: Failed,
    store_rejected: Failed,
    stuck: Failed,
    timed_out: Failed,
    timeout: Failed,
    tmpdir_failed: Failed,
    token_file_exists: Usage,
    token_file_unwritable: Usage,
    token_not_saved: Failed,
    tool_failed: Failed,
    transfer_failed: Unknown,
    transfer_incomplete: Failed,
    unconfirmed: Failed,
    unknown_account: Usage,
    unknown_cloud_kind: Usage,
    unknown_command: Usage,
    unknown_host: Usage,
    unknown_plugin: Usage,
    unsafe_output: Failed,
    unsupported_capability: Failed,
    unsupported_platform: Failed,
    update_io: Failed,
    upload_failed: Failed,
    url_refused: Denied,
    usage: Usage,
    vague_condition: Usage,
    vault_failed: Failed,
    vault_version: Failed,
    watch_failed: Failed,
    which_chat: Usage,
    which_handoff: Usage,
    workdir_outside_areas: Denied,
    worker_failed: Failed,
    write_path_failed: Failed,
    wrong_target: Usage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_line_carries_its_exit_and_next_step_over_any_detail() {
        let mut failure = Failure::new(code::usage, "Bad flag.".to_string());
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

    #[test]
    fn an_error_lines_exit_is_its_codes_and_the_process_exit() {
        for code in CODES {
            let line: Value =
                serde_json::from_str(&Failure::new(*code, "m".into()).to_json()).unwrap();
            assert_eq!(line["error"]["exit"], u8::from(code.exit), "{code:?}");
            assert_eq!(
                ExitCode::from(code.exit),
                ExitCode::from(u8::from(code.exit))
            );
        }
    }
}
