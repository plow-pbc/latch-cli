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
    /// The code Plow put in a refusal's `detail` (`AGENT_EXISTS`), if it
    /// did: a fixed word, never Plow's text. Not printed.
    pub plow_code: Option<Box<str>>,
    pub details: Value,
}

impl Failure {
    pub fn new(exit: Exit, code: &str, message: String) -> Failure {
        Failure {
            exit,
            code: code.to_string(),
            message,
            retry_after: None,
            plow_code: None,
            details: Value::Null,
        }
    }

    pub fn failed(code: &str, message: String) -> Failure {
        Failure::new(Exit::Failed, code, message)
    }

    pub fn usage(code: &str, message: String) -> Failure {
        Failure::new(Exit::Usage, code, message)
    }

    /// `{"error": {"code": "...", "message": "..."}}`
    pub fn to_json(&self) -> String {
        serde_json::json!({"error": self.error()}).to_string()
    }

    /// `{"code": "...", "message": "...", ...details}`
    pub fn error(&self) -> Value {
        let mut error = serde_json::json!({ "code": self.code, "message": self.message });
        if let Some(details) = self.details.as_object() {
            error.as_object_mut().unwrap().extend(details.clone());
        }
        error
    }
}
