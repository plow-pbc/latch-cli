//! `latch`: an agent's command-line way to reach its owner's other machines.
//!
//! `main` parses the command line, runs one command, and applies the output
//! contract every command shares: on success one JSON object on stdout and
//! exit 0; on failure `{"error": {"code", "message"}}` on stderr and an exit
//! code from the table in `failure.rs`.

#![warn(clippy::undocumented_unsafe_blocks)]

mod audit;
mod browser;
mod card;
mod cli;
mod credential;
mod dashboard;
mod devices;
mod dial;
mod events;
mod exec;
mod failure;
mod files;
mod gate;
mod google;
mod heartbeat;
mod help;
mod hosts;
mod input;
mod keyart;
mod lifecycle;
mod log;
mod login;
mod logo;
mod logout;
mod managed;
mod messages;
mod model;
mod paint;
mod plow;
mod plugins;
mod redact;
mod sandbox;
mod seal;
mod secrets;
mod serve;
mod setup;
mod state;
mod status;
mod term;
mod trace;
mod transfer;
mod transport;
mod update;
mod vault;
mod watch;
mod wire;
mod worker;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use clap::error::ErrorKind;
use serde::Serialize;
use serde_json::{Value, json};

use cli::{BrowserCommand, Cli, Command, DevicesAction, FileCommand, HostArg};
use failure::{Exit, Failure};
use state::StateDir;
use wire::Request;

fn main() -> ExitCode {
    #[cfg(feature = "fake-gog")]
    if let Some(exit) = google::fake::run_if_gog() {
        return exit;
    }
    // A bare `latch` at a terminal shows the Plow logo. Anywhere else (a
    // pipe, an agent) it stays a usage error, as before.
    if std::env::args_os().len() == 1 && term::stdout_is_terminal() {
        let color = !term::no_color();
        paint::set_color(color);
        logo::show(color);
        return ExitCode::SUCCESS;
    }
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => return usage_error(err),
    };
    // Workers confine the initial thread before any runtime threads exist.
    let command = match cli.command {
        Command::Worker(args) => return worker::run(args),
        // At a terminal, running other latch commands; no runtime of its own.
        Command::Setup => return setup::run(cli.state_dir),
        // A browser worker drives its engine on plain threads, no runtime.
        Command::BrowserWorker(args) => {
            return match StateDir::new(cli.state_dir) {
                Ok(state) => browser::worker::run(
                    &state.path,
                    &args.profile,
                    &args.scratch,
                    Duration::from_secs(args.idle_timeout),
                    Duration::from_secs(args.call_limit),
                    args.headless,
                    args.private_display,
                    args.allow_local_browsing,
                    args.page_isolation_off,
                ),
                Err(failure) => {
                    eprintln!("{}", failure.to_json());
                    failure.exit.into()
                }
            };
        }
        // Help is in latch itself: no state folder, no host.
        Command::Help(args) => return help::run(args),
        Command::Skill => {
            print!("{}", help::skill());
            return ExitCode::SUCCESS;
        }
        command => command,
    };
    let notice = !matches!(command, Command::Update(_));
    let state_dir = cli.state_dir.clone();
    let exit = run_async(cli.state_dir, command);
    if notice && let Ok(state) = StateDir::new(state_dir) {
        update::notice(&state.path);
    }
    exit
}

#[tokio::main]
async fn run_async(state_dir: Option<PathBuf>, command: Command) -> ExitCode {
    let exit = match run(state_dir, command).await {
        Ok(output) => {
            if !output.text.is_empty() {
                println!("{}", output.text);
            }
            output.exit
        }
        Err(failure) => {
            report(&failure);
            failure.exit.into()
        }
    };
    dial::closed().await;
    exit
}

/// What a command prints on stdout (JSON, `status --pretty`'s card, or
/// `login`'s one line at a terminal), and the exit code that goes with it.
/// That code is 0, except for an `exec` whose time ran out: its result is
/// still printed, but latch exits 1.
struct Printed {
    text: String,
    exit: ExitCode,
}

/// Runs one command and returns its JSON output.
async fn run(state_dir: Option<PathBuf>, command: Command) -> Result<Printed, Failure> {
    trace::init()?;
    let state = StateDir::new(state_dir)?;
    state.check_format()?;
    match command {
        Command::Login(args) if args.events => {
            // Every ending is a line too; the failure still goes to stderr.
            match login::run(&state, args).await {
                Ok(output) => {
                    let peer = &output.peer;
                    login::event(json!({"type": "verified", "peer":
                        {"name": peer.name, "key": peer.key, "kind": peer.kind}}));
                    Ok(Printed {
                        text: String::new(),
                        exit: ExitCode::SUCCESS,
                    })
                }
                Err(failure) if failure.code == "code_expired" => {
                    login::event(json!({"type": "expired"}));
                    Err(failure)
                }
                Err(failure) => {
                    login::event(json!({"type": "error", "error":
                        {"code": failure.code, "message": failure.message}}));
                    Err(failure)
                }
            }
        }
        Command::Login(args) => {
            let output = login::run(&state, args).await?;
            // A person at a terminal reads one line; a pipe gets the JSON.
            if !term::stdout_is_terminal() {
                return to_json(output);
            }
            Ok(Printed {
                text: format!("Logged in as {}.", output.peer.name),
                exit: ExitCode::SUCCESS,
            })
        }
        Command::Logout(args) => to_json(logout::run(&state, args.purge).await?),
        Command::Devices(args) => match args.action {
            None => to_json(devices::list(&state).await?),
            Some(DevicesAction::Remove { name, yes }) => {
                to_json(devices::remove(&state, &name, yes).await?)
            }
        },
        Command::Status(args) if args.local => to_json(status::local(&state)?),
        Command::Status(args) => {
            let output = status::run(&state).await?;
            if !args.pretty {
                return to_json(output);
            }
            // The card is for a person: colored only at a terminal.
            paint::set_color(term::stdout_is_terminal() && !term::no_color());
            Ok(Printed {
                text: card::render(&output, &state.path.display().to_string()),
                exit: ExitCode::SUCCESS,
            })
        }
        Command::File(FileCommand::Get {
            remote_path,
            local_dest,
            host,
        }) => {
            let peer = hosts::resolve(&state, host.host).await?;
            to_json(transfer::get(&state, &peer, host.chat, &remote_path, local_dest).await?)
        }
        Command::File(FileCommand::Put {
            local_path,
            remote_path,
            host,
        }) => {
            let peer = hosts::resolve(&state, host.host).await?;
            to_json(transfer::put(&state, &peer, host.chat, &local_path, &remote_path).await?)
        }
        Command::Google(args) => to_json(google::client::run(&state, args).await?),
        Command::Browser(args) => {
            let raw = matches!(args.command, BrowserCommand::Text { raw: true });
            let result = browser::client::run(&state, args).await?;
            if !raw {
                return to_json(result);
            }
            if result["truncated"] == true {
                eprintln!("latch: the page's text was cut short; this is its beginning");
            }
            Ok(Printed {
                text: result["text"].as_str().unwrap_or_default().to_string(),
                exit: ExitCode::SUCCESS,
            })
        }
        Command::Vault(args) => to_json(vault::client::run(&state, args).await?),
        Command::Rules(args) => to_json(gate::client::run(&state, args).await?),
        Command::Plugins(args) => to_json(plugins::client::run(&state, args).await?),
        Command::Update(args) => to_json(update::run(&state, args).await?),
        // Its lines, printed as they come, are its whole output.
        Command::Audit(cli::AuditArgs {
            action: cli::AuditAction::Show { follow: true },
        }) => {
            audit::follow(&state)?;
            Ok(Printed {
                text: String::new(),
                exit: ExitCode::SUCCESS,
            })
        }
        Command::Audit(args) => to_json(audit::run(&state, args.action)?),
        // Its lines, printed as they come, are its whole output.
        Command::Events(args) => {
            events::run(&state, args)?;
            Ok(Printed {
                text: String::new(),
                exit: ExitCode::SUCCESS,
            })
        }
        #[cfg(target_os = "macos")]
        Command::Messages(command) => {
            let request = messages::messages_request(&command);
            to_json(
                call_host(
                    &state,
                    command.host(),
                    "messages",
                    request,
                    dial::ANSWER_WITHIN,
                )
                .await?,
            )
        }
        #[cfg(target_os = "macos")]
        Command::Contacts(command) => {
            let request = messages::contacts_request(&command);
            to_json(
                call_host(
                    &state,
                    command.host(),
                    "contacts",
                    request,
                    dial::ANSWER_WITHIN,
                )
                .await?,
            )
        }
        Command::Exec(args) => {
            let secret_env = exec::parse_secret_env(&args.secret_env)?;
            let mut request = json!({ "argv": args.argv, "workdir": args.workdir, "timeout": args.timeout,
                        "writes": args.writes, "network": args.network, "secret_env": secret_env });
            if args.stdin {
                let bytes = input::read_all(Duration::from_secs(args.timeout))?;
                request["stdin"] = json!(bytes);
            }
            // The host may let the program run for its whole timeout.
            let answer_within = Duration::from_secs(args.timeout) + dial::ANSWER_MARGIN;
            let result = call_host(&state, args.host, "exec", request, answer_within).await?;
            let mut printed = to_json(&result)?;
            if result["timed_out"] == true {
                printed.exit = Exit::Failed.into();
            }
            Ok(printed)
        }
        Command::Serve(args) if args.foreground => {
            let deny_exec = serve::DenyExec::from_flags(&args.deny_exec);
            serve::run(&state, args.headless_browsers, deny_exec, args.attached).await?;
            // A clean stop prints nothing: the host's output was its log.
            Ok(Printed {
                text: String::new(),
                exit: ExitCode::SUCCESS,
            })
        }
        Command::Serve(args) if args.detach => to_json(managed::start(&state, &args).await?),
        Command::Serve(args) => {
            paint::set_color(!term::no_color());
            match managed::attach(&state, &args).await? {
                managed::Attached::Stopped(stopped) => to_json(stopped),
                // A person at the terminal reads one line.
                managed::Attached::Detached(line) => Ok(Printed {
                    text: line,
                    exit: ExitCode::SUCCESS,
                }),
            }
        }
        Command::Stop => to_json(managed::stop(&state).await?),
        Command::Dashboard => {
            paint::set_color(!term::no_color());
            managed::dashboard(&state).await?;
            Ok(Printed {
                text: String::new(),
                exit: ExitCode::SUCCESS,
            })
        }
        // Its lines, printed as they come, are its whole output.
        Command::Logs(args) => {
            events::logs(&state, args.follow)?;
            Ok(Printed {
                text: String::new(),
                exit: ExitCode::SUCCESS,
            })
        }
        #[cfg(target_os = "macos")]
        Command::AccessCheck { name } => {
            let connector = plugins::Connector::named(&name)?;
            to_json(messages::access::check_here(&state, connector).await?)
        }
        Command::Worker(_)
        | Command::BrowserWorker(_)
        | Command::Help(_)
        | Command::Skill
        | Command::Setup => {
            unreachable!("handled in main")
        }
    }
}

/// Picks the host, sends it one operation and returns what it answers.
async fn call_host(
    state: &StateDir,
    host: HostArg,
    op: &str,
    args: Value,
    answer_within: Duration,
) -> Result<Value, Failure> {
    let peer = hosts::resolve(state, host.host).await?;
    let request = Request {
        op: op.to_string(),
        args,
        chat: host.chat,
    };
    dial::call(state, &peer, &request, answer_within).await
}

fn to_json(output: impl Serialize) -> Result<Printed, Failure> {
    // Our outputs are plain structs of strings and lists, so this cannot fail.
    let text = serde_json::to_string(&output).expect("output serializes");
    Ok(Printed {
        text,
        exit: ExitCode::SUCCESS,
    })
}

/// clap prints `--help` and `--version` itself; every real usage error
/// becomes a JSON error with exit 2, like latch's own errors.
fn usage_error(err: clap::Error) -> ExitCode {
    if matches!(
        err.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        let _ = err.print();
        return ExitCode::SUCCESS;
    }
    let mut message = err.to_string().trim_end().to_string();
    // `latch exec --host h ls -la` (no `--`): say how to pass a command.
    let exec = std::env::args().skip(1).any(|word| word == "exec");
    if exec && err.kind() == ErrorKind::UnknownArgument {
        message.push_str("\n\nPut the command after `--`: latch exec --host H -- <cmd> <args>");
    }
    let failure = Failure::usage("usage", message);
    report(&failure);
    Exit::Usage.into()
}

/// Puts a failure on stderr: for a person at a terminal its message, a
/// sentence, then any details it carries (a stopped plan's steps, a request
/// id) as JSON; for a pipe the JSON error, so agents and scripts read its
/// code.
fn report(failure: &Failure) {
    if !term::stderr_is_terminal() {
        eprintln!("{}", failure.to_json());
        return;
    }
    eprintln!("{}", failure.message);
    let has_details = match &failure.details {
        Value::Null => false,
        Value::Object(details) => !details.is_empty(),
        _ => true,
    };
    if has_details {
        let details = serde_json::to_string_pretty(&failure.details).expect("details serialize");
        eprintln!("{details}");
    }
}
