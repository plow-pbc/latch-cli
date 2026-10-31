//! `latch`: an agent's command-line way to reach its owner's other machines.
//!
//! `main` parses the command line, runs one command, and applies the output
//! contract every command shares: on success one JSON object on stdout and
//! exit 0; on failure one `{"error": {"code", "message", "next", "exit"}}`
//! line on stderr and that exit code, from the table in `failure.rs`.

#![warn(clippy::undocumented_unsafe_blocks)]

mod agents;
mod app;
mod audit;
mod browser;
mod cli;
mod credential;
mod dashboard;
mod devices;
mod dial;
// Windows runs no workers until it has a sandbox: what only they use is idle there.
#[cfg_attr(windows, allow(dead_code, unused_imports))]
mod exec;
mod failure;
mod files;
mod gate;
mod google;
#[cfg(target_os = "macos")]
mod gui;
mod heartbeat;
mod help;
mod hosts;
mod input;
mod keep_awake;
mod keyart;
mod lifecycle;
mod log;
mod login;
#[cfg(target_os = "macos")]
mod login_item;
mod logo;
mod logout;
mod logs;
mod managed;
mod messages;
mod model;
mod overview;
mod paint;
mod plow;
mod plugins;
mod proxy;
mod redact;
mod relay_link;
mod remote;
mod sandbox;
mod seal;
mod secrets;
mod serve;
mod settings;
mod setup;
mod state;
mod status;
mod stream;
mod system_dns;
mod term;
mod trace;
mod transfer;
mod transport;
mod update;
mod vault;
mod watch;
mod wire;
// Windows runs no workers until it has a sandbox: what only they use is idle there.
#[cfg_attr(windows, allow(dead_code, unused_imports))]
mod worker;

use crate::failure::code;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::error::ErrorKind;
use clap::{CommandFactory, FromArgMatches};
use serde::Serialize;
use serde_json::{Value, json};

use cli::{
    BrowserCommand, Cli, Command, DevicesAction, FileCommand, HandoffAction, HostArg, HostsCommand,
};
use failure::{Exit, Failure};
use state::StateDir;
use wire::Request;

fn main() -> ExitCode {
    #[cfg(feature = "fake-gog")]
    if let Some(exit) = google::fake::run_if_gog() {
        return exit;
    }
    // Opened as the Mac app, latch runs its window instead (gui.rs).
    #[cfg(target_os = "macos")]
    if let Some(exit) = gui::launch() {
        return exit;
    }
    // A Windows console draws latch's escape sequences only when asked to.
    #[cfg(windows)]
    term::Console::draw_escapes();
    // A relative LATCH_STATE_DIR names a folder from here: made absolute
    // for every latch this one starts, a host's service in another
    // working folder included.
    if let Some(dir) = std::env::var_os(cli::STATE_DIR_ENV).map(PathBuf::from)
        && dir.is_relative()
        && let Ok(dir) = std::path::absolute(&dir)
    {
        // SAFETY: no other thread exists yet to read or write the environment.
        unsafe { std::env::set_var(cli::STATE_DIR_ENV, dir) };
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
    // A bare `latch`: the overview, for a person at a terminal.
    let Some(command) = cli.command else {
        let shown = if term::stdout_is_terminal() {
            StateDir::new(cli.state_dir).and_then(|state| overview::run(&state))
        } else {
            Err(overview::no_terminal())
        };
        return match shown {
            Ok(()) => ExitCode::SUCCESS,
            Err(failure) => {
                report(&failure);
                failure.exit.into()
            }
        };
    };
    let command = match command {
        Command::Worker(args) => return worker::run(args),
        // At a terminal, running other latch commands; no runtime of its own.
        Command::Setup(args) if !args.status && term::interactive() => {
            return setup::run(cli.state_dir);
        }
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
            term::print_out(&help::skill());
            return ExitCode::SUCCESS;
        }
        // Only opens the app, which picks its state folder itself: a
        // folder named here (not just this build's default) is refused.
        Command::App => {
            let named = cli.state_dir.filter(|dir| {
                Some(dir.as_path()) != cli::BUILD_STATE_DIR.map(std::path::Path::new)
            });
            return match app::open(named.as_deref()).and_then(|opened| json(cli.pretty, opened)) {
                Ok(Printed(text)) => {
                    term::print_out(&format!("{text}\n"));
                    ExitCode::SUCCESS
                }
                Err(failure) => {
                    report(&failure);
                    failure.exit.into()
                }
            };
        }
        // Run by install.sh before anything is installed: no state folder.
        Command::VerifySignature { key, file } => {
            return match update::verify_file(&key, &file).and_then(|v| json(cli.pretty, v)) {
                Ok(Printed(text)) => {
                    term::print_out(&format!("{text}\n"));
                    ExitCode::SUCCESS
                }
                Err(failure) => {
                    report(&failure);
                    failure.exit.into()
                }
            };
        }
        // Run as administrator by `latch setup`, maybe as another account:
        // no state folder needed, and no runtime.
        #[cfg(windows)]
        Command::GrantListing => {
            return match setup::grant_listing().and_then(|done| json(cli.pretty, done)) {
                Ok(Printed(text)) => {
                    term::print_out(&format!("{text}\n"));
                    ExitCode::SUCCESS
                }
                Err(failure) => {
                    report(&failure);
                    failure.exit.into()
                }
            };
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
                term::print_out(&format!("{text}\n"));
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
    // An agent holding its own Plow credential logs in on its first command
    // that needs Plow or a host; what answers here without one does not.
    let needs_login = match &command {
        Command::Status(args) => !args.local && (args.host.is_some() || !args.follow),
        Command::Google(args) => !args.local_help(),
        Command::Browser(args) => !matches!(
            args.command,
            BrowserCommand::Do { dry_run: true, .. }
                | BrowserCommand::Setup { .. }
                | BrowserCommand::Handoff {
                    action: Some(HandoffAction::Done),
                    ..
                }
        ),
        Command::Exec(_)
        | Command::File(_)
        | Command::Messages(_)
        | Command::Contacts(_)
        | Command::Vault(_)
        | Command::Rules(_)
        | Command::Agents(_)
        | Command::Devices(_)
        | Command::Hosts(HostsCommand::Default { name: Some(_), .. }) => true,
        _ => false,
    };
    if needs_login {
        login::agent_on_first_use(&state).await?;
    }
    match command {
        Command::Login(args) if args.events => {
            login::event(stream::hello());
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
                    login::event(json!({"type": "error", "error": failure.error()}));
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
        Command::Logout(args) => json(pretty, logout::run(&state, &args).await?),
        Command::Agents(args) => json(pretty, agents::run(&state, args).await?),
        Command::Hosts(HostsCommand::Default { name, clear }) => {
            json(pretty, hosts::default(&state, name, clear).await?)
        }
        Command::Devices(args) => match args.action {
            None => json(pretty, devices::list(&state).await?),
            Some(DevicesAction::Remove { name, yes }) => {
                json(pretty, devices::remove(&state, &name, yes).await?)
            }
        },
        Command::Status(args) if args.host.is_some() || args.follow => {
            let follow = args.follow;
            match remote::other_host(&state, args.host).await? {
                Some(host) if follow => {
                    let request = json!({"follow": true});
                    streamed(remote::stream(&state, &host, "status", request, true).await)
                }
                Some(host) => json(pretty, remote::status(&state, &host).await?),
                None if follow => {
                    // Before the first login, too.
                    state.create()?;
                    let dir = state.path.clone();
                    print_local(true, move |send| status::follow(&dir, None, send))
                }
                None => json(pretty, status::local(&state)?),
            }
        }
        Command::Status(args) if args.local => json(pretty, status::local(&state)?),
        Command::Status(_) => json(pretty, status::run(&state).await?),
        Command::Setup(args) if !args.status => {
            Err(setup::no_terminal(&setup::status(&state, false).await?))
        }
        Command::Setup(args) => json(pretty, setup::status(&state, args.check_access).await?),
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
        Command::Plugins(args)
            if matches!(
                args.command,
                cli::PluginsCommand::Enable { events: true, .. }
            ) =>
        {
            // As `login --events`: `hello`, any `link` lines, then one
            // `result` or `error`.
            login::event(stream::hello());
            match plugins::client::run(&state, args).await {
                Ok(result) => login::event(json!({"type": "result", "result": result})),
                Err(failure) => {
                    login::event(json!({"type": "error", "error": failure.error()}));
                    return Err(failure);
                }
            }
            Ok(Printed(String::new()))
        }
        Command::Plugins(args) => json(pretty, plugins::client::run(&state, args).await?),
        Command::Settings(cli::SettingsArgs {
            command: cli::SettingsCommand::Set { keep_awake },
        }) => json(pretty, settings::set(&state, keep_awake == cli::OnOff::On)?),
        Command::Update(args) => json(pretty, update::run(&state, args).await?),
        Command::Audit(cli::AuditArgs {
            action: Some(action),
            ..
        }) => json(pretty, audit::run(&state, action).await?),
        Command::Audit(args) => {
            let range = audit::Range {
                last: args.last,
                all: args.all,
                before: args.before,
                after: args.after,
                follow: args.follow,
            };
            range.check()?;
            match remote::other_host(&state, args.host).await? {
                Some(host) => {
                    let request = serde_json::to_value(&range).expect("a range serializes");
                    let follow = range.follow;
                    streamed(remote::stream(&state, &host, "audit", request, follow).await)
                }
                None => {
                    // A host's log begins when it first serves, or keeps an
                    // owner command here.
                    if !state.path.join(audit::FILE).exists() {
                        let message = "this machine has never served, so it keeps no audit";
                        let mut failure = Failure::new(code::no_host_here, message.into());
                        failure.next = Some("latch audit --host NAME".into());
                        return Err(failure);
                    }
                    let whose = match state.load_config()? {
                        Some(config) => format!("{}'s", config.peer.name),
                        None => "this machine's".to_string(),
                    };
                    let dir = state.path.clone();
                    print_local(range.follow, move |send| {
                        audit::read(&dir, &whose, &range, None, send)
                    })
                }
            }
        }
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
            logs::run(&state, args.follow)?;
            Ok(Printed(String::new()))
        }
        Command::PruneHelpers => json(pretty, plugins::prune_alone(&state.path)?),
        Command::InstallGog => json(pretty, google::install::if_missing(&state)?),
        #[cfg(target_os = "macos")]
        Command::LoginItem { again } => json(
            pretty,
            if again {
                login_item::again()?
            } else {
                login_item::status()?
            },
        ),
        #[cfg(target_os = "macos")]
        Command::AccessCheck { name } => {
            let connector = plugins::Connector::named(&name)?;
            json(
                pretty,
                messages::access::check_here(&state, connector).await?,
            )
        }
        Command::Worker(_)
        | Command::BrowserWorker(_)
        | Command::Help(_)
        | Command::Skill
        | Command::App
        | Command::VerifySignature { .. } => {
            unreachable!("handled in main")
        }
        #[cfg(windows)]
        Command::GrantListing => unreachable!("handled in main"),
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

/// Prints a stream of this machine's lines on stdout as they come, until
/// they end or nobody reads them. They are its whole output. A followed
/// stream (`follow`) also ends once stdin, when it is a pipe, closes, even
/// while a write to a reader that stopped reading is stuck: the lines come
/// from a thread of their own. A stream that ends by itself never looks
/// at stdin.
fn print_local(
    follow: bool,
    lines: impl FnOnce(&mut dyn FnMut(&Value) -> bool) -> Result<(), Failure> + Send + 'static,
) -> Result<Printed, Failure> {
    let print = |line: &Value| {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{line}").and_then(|()| out.flush()).is_ok()
    };
    if !follow {
        return streamed(lines(&mut |line| print(line)));
    }
    let (ended, end) = std::sync::mpsc::channel();
    let stdin_closed = ended.clone();
    std::thread::spawn(move || {
        let _ = ended.send(lines(&mut |line| print(line)));
    });
    if watch::stdin_is_a_pipe() {
        std::thread::spawn(move || {
            watch::wait_for_stdin_to_close();
            let _ = stdin_closed.send(Ok(()));
        });
    }
    streamed(end.recv().expect("the stream says how it ended"))
}

/// A stream's lines, printed as they came, are its whole output.
fn streamed(result: Result<(), Failure>) -> Result<Printed, Failure> {
    result.map(|()| Printed(String::new()))
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
        let _ = json(pretty, version).map(|Printed(text)| term::print_out(&format!("{text}\n")));
        return ExitCode::SUCCESS;
    }
    let message = err.to_string().trim_end().to_string();
    let mut failure = Failure::new(code::usage, message);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_is_one_line_or_indented_with_the_same_content() {
        let result =
            json!({"host": "mac", "sessions": [{"id": "s1", "tabs": 2}], "note": "café\nnext"});
        let Ok(Printed(compact)) = json(false, &result) else {
            unreachable!()
        };
        let Ok(Printed(pretty)) = json(true, &result) else {
            unreachable!()
        };
        assert!(!compact.contains('\n'), "{compact}");
        assert!(pretty.contains("\n  \"host\""), "{pretty}");
        for text in [&compact, &pretty] {
            assert_eq!(serde_json::from_str::<Value>(text).unwrap(), result);
        }
    }
}
