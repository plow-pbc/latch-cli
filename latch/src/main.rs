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
mod hosts;
mod input;
mod keyart;
mod lifecycle;
mod log;
mod login;
mod logo;
mod model;
mod paint;
mod plow;
mod sandbox;
mod serve;
mod shares;
mod state;
mod state_folders;
mod status;
mod term;
mod trace;
mod transfer;
mod transport;
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
                ),
                Err(failure) => {
                    eprintln!("{}", failure.to_json());
                    failure.exit.into()
                }
            };
        }
        command => command,
    };
    run_async(cli.state_dir, command)
}

#[tokio::main]
async fn run_async(state_dir: Option<PathBuf>, command: Command) -> ExitCode {
    match run(state_dir, command).await {
        Ok(output) => {
            if !output.text.is_empty() {
                println!("{}", output.text);
            }
            output.exit
        }
        Err(failure) => {
            eprintln!("{}", failure.to_json());
            failure.exit.into()
        }
    }
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
            let output = match login::run(&state, args).await {
                Ok(output) => output,
                // A person at a terminal reads the sentence; a pipe gets the JSON.
                Err(failure) if term::stderr_is_terminal() => {
                    eprintln!("{}", failure.message);
                    return Ok(Printed {
                        text: String::new(),
                        exit: failure.exit.into(),
                    });
                }
                Err(failure) => return Err(failure),
            };
            // A person at a terminal reads one line; a pipe gets the JSON.
            if !term::stdout_is_terminal() {
                return to_json(output);
            }
            Ok(Printed {
                text: format!("Logged in as {}.", output.peer.name),
                exit: ExitCode::SUCCESS,
            })
        }
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
        Command::Shares(args) => to_json(shares::run(&state, args.action)?),
        // Its lines, printed as they come, are its whole output.
        Command::Events(args) => {
            events::run(&state, args)?;
            Ok(Printed {
                text: String::new(),
                exit: ExitCode::SUCCESS,
            })
        }
        Command::Exec(args) => {
            let mut request =
                json!({ "argv": args.argv, "workdir": args.workdir, "timeout": args.timeout });
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
        Command::Serve(args) => {
            // The dashboard is for a person at a terminal who has not asked
            // for plain output (`--no-dashboard`, `NO_COLOR`).
            let dashboard = !args.no_dashboard && term::stdout_is_terminal() && !term::no_color();
            paint::set_color(dashboard);
            let deny_exec = serve::DenyExec::from_flags(&args.deny_exec);
            serve::run(
                &state,
                &args.folders,
                &args.allow_read,
                dashboard,
                args.headless_browsers,
                deny_exec,
            )
            .await?;
            to_json(json!({}))
        }
        Command::Worker(_) | Command::BrowserWorker(_) => unreachable!("handled in main"),
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
    eprintln!("{}", failure.to_json());
    Exit::Usage.into()
}
