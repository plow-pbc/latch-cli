//! `latch`: an agent's command-line way to reach its owner's other machines.
//!
//! `main` parses the command line, runs one command, and applies the output
//! contract every command shares: on success one JSON object on stdout and
//! exit 0; on failure one `{"error": {"code", "message", "next", "exit"}}`
//! line on stderr and that exit code, from the table in `failure.rs`.

#![warn(clippy::undocumented_unsafe_blocks)]

mod audit;
mod browser;
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

use clap::error::ErrorKind;
use clap::{CommandFactory, FromArgMatches};
use serde::Serialize;
use serde_json::{Value, json};

use cli::{Cli, Command, DevicesAction, FileCommand, HostArg};
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
    let matches = match Cli::command().try_get_matches() {
        Ok(matches) => matches,
        Err(err) => return usage_error(err),
    };
    if let Err(failure) = help::check_selectors(&matches) {
        report(&failure);
        return failure.exit.into();
    }
    let cli = Cli::from_arg_matches(&matches).expect("the command line was validated");
    // Workers confine the initial thread before any runtime threads exist.
    let command = match cli.command {
        Command::Worker(args) => return worker::run(args),
        // At a terminal, running other latch commands; no runtime of its own.
        Command::Setup(args) if !args.status => return setup::run(cli.state_dir),
        // A browser worker drives its engine on plain threads, no runtime.
        Command::BrowserWorker(args) => {
            return match StateDir::new(cli.state_dir) {
                Ok(state) => browser::worker::run(
                    &state.path,
                    &args.engine,
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
        Command::Help(args) => return help::run(args, cli.pretty),
        Command::Skill => {
            print!("{}", help::skill());
            return ExitCode::SUCCESS;
        }
        command => command,
    };
    // Only the human commands still say a newer build is out: a regular
    // command's stderr is its error line alone, and `status --local` carries
    // the same fact.
    let notice = match &command {
        Command::Dashboard => true,
        Command::Serve(args) => !args.detach && !args.foreground,
        _ => false,
    };
    let state_dir = cli.state_dir.clone();
    let exit = run_async(cli.state_dir, command, cli.pretty);
    if notice && let Ok(state) = StateDir::new(state_dir) {
        update::notice(&state.path);
    }
    exit
}

#[tokio::main]
async fn run_async(state_dir: Option<PathBuf>, command: Command, pretty: bool) -> ExitCode {
    let exit = match run(state_dir, command, pretty).await {
        Ok(Printed(text)) => {
            if !text.is_empty() {
                println!("{text}");
            }
            ExitCode::SUCCESS
        }
        Err(failure) => {
            report(&failure);
            failure.exit.into()
        }
    };
    dial::closed().await;
    exit
}

/// What a command that worked prints on stdout: its JSON; nothing, for a
/// stream that printed its lines as they came; or, for a person leaving
/// attached `serve`'s dashboard, one line.
struct Printed(String);

/// Runs one command and returns its JSON output.
async fn run(
    state_dir: Option<PathBuf>,
    command: Command,
    pretty: bool,
) -> Result<Printed, Failure> {
    let state = StateDir::new(state_dir)?;
    state.check_format()?;
    let foreground = matches!(&command, Command::Serve(args) if args.foreground);
    trace::init(&state, foreground)?;
    match command {
        Command::Login(args) if args.events => {
            login::event(events::hello());
            // Every ending is a line too; the failure still goes to stderr.
            match login::run(&state, args).await {
                Ok(output) => {
                    let peer = &output.peer;
                    login::event(json!({"type": "verified", "peer":
                        {"name": peer.name, "key": peer.key, "kind": peer.kind}}));
                    Ok(Printed(String::new()))
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
            // A person who logged in by text reads one line where they
            // texted; every other login is a regular command's JSON.
            if output.by_text {
                eprintln!("Logged in as {}.", output.peer.name);
                return Ok(Printed(String::new()));
            }
            json(pretty, output)
        }
        Command::Logout(args) => json(pretty, logout::run(&state, args.purge).await?),
        Command::Devices(args) => match args.action {
            None => json(pretty, devices::list(&state).await?),
            Some(DevicesAction::Remove { name, yes }) => {
                json(pretty, devices::remove(&state, &name, yes).await?)
            }
        },
        Command::Status(args) if args.local => json(pretty, status::local(&state)?),
        Command::Status(_) => json(pretty, status::run(&state).await?),
        Command::Setup(_) => json(pretty, setup::status(&state)?),
        Command::File(FileCommand::Get {
            remote_path,
            local_dest,
            host,
        }) => {
            let peer = hosts::resolve(&state, host.host).await?;
            json(
                pretty,
                transfer::get(&state, &peer, host.plow_chat, &remote_path, local_dest).await?,
            )
        }
        Command::File(FileCommand::Put {
            local_path,
            remote_path,
            host,
        }) => {
            let peer = hosts::resolve(&state, host.host).await?;
            json(
                pretty,
                transfer::put(&state, &peer, host.plow_chat, &local_path, &remote_path).await?,
            )
        }
        Command::Google(args) => json(pretty, google::client::run(&state, args).await?),
        Command::Browser(args) => json(pretty, browser::client::run(&state, args).await?),
        Command::Vault(args) => json(pretty, vault::client::run(&state, args).await?),
        // A person edited the rules in their editor: a line says it is saved.
        Command::Rules(args) if matches!(args.command, cli::RulesCommand::Edit { .. }) => {
            let saved = gate::client::run(&state, args).await?;
            let whose = saved["agent_name"].as_str().unwrap_or("every agent");
            let host = saved["host"].as_str().unwrap_or("this host");
            eprintln!("Saved the rules for {whose} on {host}.");
            Ok(Printed(String::new()))
        }
        Command::Rules(args) => json(pretty, gate::client::run(&state, args).await?),
        Command::Plugins(args) => json(pretty, plugins::client::run(&state, args).await?),
        Command::Update(args) => json(pretty, update::run(&state, args).await?),
        // Its lines, printed as they come, are its whole output.
        Command::Audit(cli::AuditArgs {
            action: cli::AuditAction::Show { follow: true },
        }) => {
            audit::follow(&state)?;
            Ok(Printed(String::new()))
        }
        Command::Audit(args) => json(pretty, audit::run(&state, args.action)?),
        // Its lines, printed as they come, are its whole output.
        Command::Events(args) => {
            events::run(&state, args)?;
            Ok(Printed(String::new()))
        }
        #[cfg(target_os = "macos")]
        Command::Messages(command) => {
            let request = messages::messages_request(&command);
            json(
                pretty,
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
            json(
                pretty,
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
            let request = exec::request(&args)?;
            // The host may let the program run for its whole timeout.
            let answer_within = Duration::from_secs(args.timeout) + dial::ANSWER_MARGIN;
            let result = call_host(&state, args.host, "exec", request, answer_within).await?;
            if result["timed_out"] == true {
                return Err(exec::timed_out(args.timeout, result));
            }
            json(pretty, result)
        }
        Command::Serve(args) if args.foreground => {
            serve::run(&state, &args).await?;
            // A clean stop prints nothing: the host's output was its log.
            Ok(Printed(String::new()))
        }
        Command::Serve(args) if args.detach => json(pretty, managed::start(&state, &args).await?),
        Command::Serve(args) => {
            paint::set_color(!term::no_color());
            // A person at the terminal reads one line, however it ends.
            match managed::attach(&state, &args).await? {
                managed::Attached::Stopped(stopped) => {
                    let line = match stopped["error"]["message"].as_str() {
                        Some(why) => format!("The host stopped: {why}"),
                        None => "The host stopped.".to_string(),
                    };
                    Ok(Printed(line))
                }
                managed::Attached::Detached(line) => Ok(Printed(line)),
            }
        }
        Command::Stop => json(pretty, managed::stop(&state).await?),
        Command::Dashboard => {
            paint::set_color(!term::no_color());
            managed::dashboard(&state).await?;
            Ok(Printed(String::new()))
        }
        // Its lines, printed as they come, are its whole output.
        Command::Logs(args) => {
            events::logs(&state, args.follow)?;
            Ok(Printed(String::new()))
        }
        #[cfg(target_os = "macos")]
        Command::AccessCheck { name } => {
            let connector = plugins::Connector::named(&name)?;
            json(
                pretty,
                messages::access::check_here(&state, connector).await?,
            )
        }
        Command::Worker(_) | Command::BrowserWorker(_) | Command::Help(_) | Command::Skill => {
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
        chat: host.plow_chat,
    };
    dial::call(state, &peer, &request, answer_within).await
}

/// A command's result as its JSON: one line, or indented with `--pretty`.
fn json(pretty: bool, output: impl Serialize) -> Result<Printed, Failure> {
    // Our outputs are plain structs of strings and lists, so this cannot fail.
    let text = match pretty {
        true => serde_json::to_string_pretty(&output),
        false => serde_json::to_string(&output),
    };
    Ok(Printed(text.expect("output serializes")))
}

/// clap prints `--help` itself; `--version` is JSON, and every real usage
/// error becomes a JSON error with exit 2, like latch's own errors.
fn usage_error(err: clap::Error) -> ExitCode {
    if err.kind() == ErrorKind::DisplayHelp {
        let _ = err.print();
        return ExitCode::SUCCESS;
    }
    if err.kind() == ErrorKind::DisplayVersion {
        // clap stops at `--version`, so `--pretty` is looked for by hand.
        let pretty = std::env::args().any(|word| word == "--pretty");
        let version = json!({ "version": cli::version() });
        let _ = json(pretty, version).map(|Printed(text)| println!("{text}"));
        return ExitCode::SUCCESS;
    }
    let message = err.to_string().trim_end().to_string();
    let mut failure = Failure::usage("usage", message);
    // `latch exec --host h ls -la` (no `--`): say how to pass a command.
    let exec = std::env::args().skip(1).any(|word| word == "exec");
    if exec && err.kind() == ErrorKind::UnknownArgument {
        failure.next = Some("latch exec --host H -- <cmd> <args>".into());
    }
    report(&failure);
    Exit::Usage.into()
}

/// Puts a failure on stderr as its one JSON error line, whether a person
/// or a program reads it; unless a human command already said it to the
/// person in a plain line.
fn report(failure: &Failure) {
    if !failure.said {
        eprintln!("{}", failure.to_json());
    }
}
