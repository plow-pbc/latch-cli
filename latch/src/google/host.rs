//! Authenticated host operations. The host sees file metadata and pipes;
//! only sandboxed children open the transferred files or contact Google.
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::transport::{RecvStream, SendStream};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::{Account, MAX_OUTPUT, Output, PIN, Request, Tokens, policy, problem, process};
use crate::failure::{Failure, code};
use crate::plow::Plow;
use crate::serve::Done;
use crate::wire::{self, Response};
use crate::worker::{Running, WorkerSetup};

/// The host's `google` operation. Every outcome, refusals included, ends as
/// one answer for the client and an audit record of the command, flag names,
/// accounts and recipients (never bodies, queries or tokens).
pub async fn run(
    workers: &WorkerSetup,
    plow: &Plow,
    args: Value,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Done {
    let mut audit = json!({"command":"unrecognized","flags":[],"recipients":[],"accounts":[]});
    // Leave time for the refusal to reach the client before its answer window.
    let limit = crate::dial::ANSWER_WITHIN - crate::dial::ANSWER_MARGIN;
    let result = tokio::time::timeout(
        limit,
        operation(workers, plow, args.clone(), send, recv, &mut audit),
    )
    .await
    .unwrap_or_else(|_| Err(timeout_failure(&args, limit)));
    match result {
        Ok(value) => Done {
            response: Response::Ok(value),
            audit_args: audit,
            outcome: "ok".into(),
        },
        Err(failure) => {
            audit["refusal"] = json!(failure.code);
            if audit.get("outcomes").is_none() {
                let outcomes: Vec<_> = audit["accounts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|account| {
                        json!({
                            "account": account,
                            "status": "failed",
                            "reason": reason(&failure),
                        })
                    })
                    .collect();
                audit["outcomes"] = json!(outcomes);
            }
            Done {
                response: Response::error(&failure),
                audit_args: audit,
                outcome: failure.code,
            }
        }
    }
}
fn timeout_failure(args: &Value, limit: Duration) -> Failure {
    let write = serde_json::from_value::<Request>(args.clone())
        .ok()
        .and_then(|request| policy::parse(&request.gog_args).ok())
        .is_some_and(|plan| !plan.read_only());
    // A write that timed out may have happened: its own code, exit 5.
    let code = match write {
        true => code::google_write_timeout,
        false => code::google_timeout,
    };
    let mut failure = problem(code, json!({"limit_seconds": limit.as_secs()}));
    if write {
        failure.message = "Google write timed out; outcome unknown; check before resending".into();
    }
    failure
}

/// Checks the request, gets the Google tokens, and runs gog: once per account
/// for a fan-out read (results merged), else once. Fills in `audit` as it
/// learns more, so a refusal at any step is still recorded.
async fn operation(
    workers: &WorkerSetup,
    plow: &Plow,
    args: Value,
    send: &mut SendStream,
    recv: &mut RecvStream,
    audit: &mut Value,
) -> Result<Value, Failure> {
    let mut request: Request =
        serde_json::from_value(args).map_err(|_| problem(code::not_allowed, json!({})))?;
    audit["command"] = json!(policy::command_prefix(&request.gog_args));
    let plan = plan(&mut request, &owner_time_zone())?;
    let accounts_only = plan.is_none();
    if let Some(plan) = &plan {
        *audit = plan.audit();
    } else {
        audit["command"] = json!("accounts");
    }
    // A host without gog says so before anything about accounts.
    let program = match &plan {
        Some(_) => {
            let program = gog_program(&workers.state_dir, &PIN.version)?;
            Some(
                program
                    .canonicalize()
                    .map_err(|_| problem(code::gog_unverified, json!({})))?,
            )
        }
        None => None,
    };
    let tokens = plow.google_tokens().await?;
    let connected: Vec<_> = tokens.accounts.iter().map(|a| a.account.clone()).collect();
    audit["accounts"] = json!(connected);
    if accounts_only {
        return Ok(json!({"accounts":connected}));
    }
    let plan = plan.expect("command parsed");
    let selected = select(&tokens, &request.account, plan.fanout() || plan.help)?;
    audit["accounts"] = json!(selected.iter().map(|a| &a.account).collect::<Vec<_>>());
    if request.inputs.len() != plan.inputs.len()
        || request
            .inputs
            .iter()
            .zip(&plan.inputs)
            .any(|(a, b)| a.index != b.index)
    {
        return Err(problem(code::not_allowed, json!({})));
    }
    let program = program.expect("found above for every command but accounts");
    process::verify(&program)?;
    // Calendars the conflict check could not read and never will (no
    // access); the booked event's answer names them.
    let mut unreadable: Vec<String> = Vec::new();
    if let Some(booked) = plan.conflict_check() {
        let checked = conflict(workers, &program, &tokens, &plan, selected[0], booked).await;
        unreadable = match checked {
            Ok(unreadable) => unreadable,
            Err(failure) if request.confirm_conflict && failure.code == "calendar_conflict" => {
                serde_json::from_value(failure.details["could_not_check"].clone())
                    .unwrap_or_default()
            }
            Err(failure) => return Err(failure),
        };
    }
    frame(send, json!({"ready":true})).await?;
    if plan.fanout() && !plan.help {
        let mut items = Vec::new();
        let mut outcomes = Vec::new();
        let mut answered = 0;
        // An account the owner granted no read of this kind is left out,
        // and said so, rather than failing on Google's refusal.
        let (readable, unreadable): (Vec<&Account>, Vec<&Account>) = selected
            .iter()
            .partition(|account| can_read(account, &plan.command));
        for account in unreadable {
            outcomes.push(json!({
                "account": account.account,
                "status": "skipped",
                "reason": "no_access",
            }));
        }
        let selected = readable;
        let runs = at_most(
            FANOUT_AT_ONCE,
            selected
                .iter()
                .map(|account| async {
                    let (worker, output, _folder) =
                        match invoke(workers, &program, account, &request, None).await {
                            Ok(run) => run,
                            Err(failure) => return Ok(Err(failure)),
                        };
                    worker.finish().await.map_err(worker_error)?;
                    Ok(Ok(output))
                })
                .collect(),
        )
        .await;
        for (account, run) in selected.iter().zip(runs) {
            match run? {
                Ok(output) => {
                    let values = super::merge::items(
                        &plan.command,
                        output.result,
                        plan.projected(),
                        &account.account,
                    );
                    let status = if values.is_empty() { "empty" } else { "ok" };
                    outcomes.push(json!({ "account": account.account, "status": status }));
                    items.extend(values);
                    answered += 1;
                }
                Err(failure) => {
                    // A sandbox that cannot be applied fails every account
                    // alike, so the whole call fails with that code.
                    if failure.code == "output_too_large" || failure.code == "sandbox_failed" {
                        return Err(failure);
                    }
                    outcomes.push(json!({
                        "account": account.account,
                        "status": "failed",
                        "reason": reason(&failure),
                    }));
                }
            }
        }
        audit["outcomes"] = json!(outcomes);
        super::merge::sort(&plan.command, &mut items);
        if answered == 0 {
            // Every account wants the owner to sign in again: one code for it.
            let again: Vec<String> = outcomes
                .iter()
                .filter(|outcome| outcome["reason"] == code::google_needs_sign_in.name)
                .filter_map(|outcome| outcome["account"].as_str().map(String::from))
                .collect();
            if !again.is_empty() && again.len() == outcomes.len() {
                return Err(super::needs_sign_in(again));
            }
            return Err(problem(code::google_failed, json!({"accounts":outcomes})));
        }
        let mut result = json!({"items":items,"accounts":outcomes});
        if plan.compact() {
            let (events, truncated) =
                super::merge::compact_events(&items, super::merge::CALENDAR_BUDGET);
            result["items"] = json!(events);
            if let Some(truncated) = truncated {
                result["truncated"] = truncated;
            }
        }
        check_size(&result)?;
        frame(send, json!({"files":[]})).await?;
        return Ok(result);
    }
    let account = selected[0];
    let (mut worker, output, _folder) =
        invoke(workers, &program, account, &request, Some((recv, audit))).await?;
    check_size(&output.result)?;
    audit["outcomes"] = json!([{"account":account.account,"status":"ok"}]);
    audit["outputs"] = json!(
        output
            .files
            .iter()
            .map(|file| {
                plan.destination(&file.path)
                    .map(|path| json!({"path":path,"size":file.size,"sha256":file.sha256}))
            })
            .collect::<Result<Vec<_>, _>>()?
    );
    if let Some(files) = audit["inputs"].as_array_mut() {
        for (file, input) in files.iter_mut().zip(&plan.inputs) {
            file["path"] = json!(input.path);
        }
    }
    frame(send, json!({"files":output.files})).await?;
    for moved in &output.files {
        let hash = wire::copy(&mut worker.stdout, send, moved.size)
            .await
            .map_err(|_| problem(code::transfer_failed, json!({})))?;
        if hash != moved.sha256 {
            return Err(problem(code::checksum_mismatch, json!({})));
        }
    }
    worker.finish().await.map_err(worker_error)?;
    let mut answer = json!({"account":account.account,"result":output.result,"files":output.files});
    if !unreadable.is_empty() {
        answer["could_not_check"] = json!(unreadable);
    }
    Ok(answer)
}

/// The request as gog will run it: latch's own arguments lifted out of gog's
/// (they may follow `--`), the arguments read against the grammar, and a
/// calendar event list put in the owner's time zone (`zone`) unless the
/// agent chose one. `None` for `accounts`, which runs no gog.
pub(super) fn plan(request: &mut Request, zone: &str) -> Result<Option<policy::Plan>, Failure> {
    let lifted = policy::lift(&request.gog_args)?;
    request.gog_args = lifted.args;
    if let Some(accounts) = lifted.accounts {
        request.account = accounts;
    }
    request.confirm_conflict |= lifted.confirm_conflict;
    if request.gog_args == ["accounts"] {
        return Ok(None);
    }
    let plan = policy::parse(&request.gog_args)?;
    // A calendar id belongs to one account. Sent to every account asked, it
    // came back as the same events once per account that could read it and a
    // failure from each that could not.
    let calendar_ids = plan.flags.iter().any(|(name, _)| name == "calendars");
    if plan.fanout()
        && plan.command.starts_with("calendar.")
        && calendar_ids
        && request.account.len() != 1
    {
        return Err(Failure::new(
            code::account_required,
            "a calendar id needs its account: add --account <email> for the account that owns it, \
             or drop --calendars and name the accounts to read with --account a@x,b@y"
                .into(),
        ));
    }
    // gog names each event's day and local times in that event's own zone
    // unless told otherwise, so a 9pm meeting set in Sao Paulo would read as
    // the next day for an owner in California.
    if plan.command == "calendar.events" && !plan.help && plan.flag("timezone").is_none() {
        request
            .gog_args
            .extend(["--timezone".to_owned(), zone.to_owned()]);
        return policy::parse(&request.gog_args).map(Some);
    }
    Ok(Some(plan))
}

/// The owner's time zone: this host's, which is the owner's own machine.
fn owner_time_zone() -> String {
    jiff::tz::TimeZone::system()
        .iana_name()
        .unwrap_or("UTC")
        .to_owned()
}

/// How many accounts a fan-out read runs on at once.
const FANOUT_AT_ONCE: usize = 4;

/// Runs `futures` with at most `limit` of them in progress at a time, and
/// returns their outputs in the order given.
async fn at_most<F: std::future::Future>(limit: usize, futures: Vec<F>) -> Vec<F::Output> {
    let mut futures: Vec<_> = futures.into_iter().map(Box::pin).collect();
    let mut outputs: Vec<Option<F::Output>> = futures.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        // A future is only polled once a slot is free, so the ones past
        // `limit` have not started.
        let mut running = 0;
        for (future, output) in futures.iter_mut().zip(outputs.iter_mut()) {
            if output.is_some() {
                continue;
            }
            if running == limit {
                break;
            }
            match future.as_mut().poll(cx) {
                std::task::Poll::Ready(value) => *output = Some(value),
                std::task::Poll::Pending => running += 1,
            }
        }
        if outputs.iter().all(Option::is_some) {
            std::task::Poll::Ready(outputs.drain(..).map(|o| o.expect("ready")).collect())
        } else {
            std::task::Poll::Pending
        }
    })
    .await
}
/// The accounts a call runs on: those `--account` names, else every connected
/// one. Only a fan-out read, or `--help`, may run on more than one.
///
/// `'a` says the returned accounts are borrowed from `tokens`, so they can
/// be used only while `tokens` is.
pub(super) fn select<'a>(
    tokens: &'a Tokens,
    requested: &[String],
    fanout: bool,
) -> Result<Vec<&'a Account>, Failure> {
    let connected: Vec<_> = tokens.accounts.iter().map(|a| &a.account).collect();
    if requested.iter().any(|name| !connected.contains(&name)) {
        return Err(problem(
            code::unknown_account,
            json!({"accounts":connected}),
        ));
    }
    let selected: Vec<_> = tokens
        .accounts
        .iter()
        .filter(|a| requested.is_empty() || requested.contains(&a.account))
        .collect();
    if selected.is_empty() || (!fanout && selected.len() != 1) {
        return Err(problem(
            code::account_required,
            json!({"accounts":connected}),
        ));
    }
    Ok(selected)
}
/// Runs gog once, for one account, in a new sandboxed worker with its own
/// call folder. With `upload`, the client's files are streamed from `recv`
/// to the worker and recorded in the audit. Returns the worker, whose stdout
/// still carries the bytes of any files gog wrote; its output; and the call
/// folder, which is deleted when the returned `TempDir` is dropped.
async fn invoke(
    workers: &WorkerSetup,
    program: &Path,
    account: &Account,
    request: &Request,
    upload: Option<(&mut RecvStream, &mut Value)>,
) -> Result<(Running, Output, TempDir), Failure> {
    let token = account
        .access_token
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            if account.needs_sign_in {
                super::needs_sign_in(vec![account.account.clone()])
            } else {
                problem(code::google_failed, json!({"reason": "unavailable"}))
            }
        })?;
    // Only this worker is granted the unique private folder in OS temp.
    let folder = tempfile::Builder::new()
        .prefix("latch-google-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .map_err(|_| problem(code::google_worker_failed, json!({})))?;
    let job = process::Job {
        account: account.account.clone(),
        argv: request.gog_args.clone(),
        inputs: request
            .inputs
            .iter()
            .map(|i| super::Input {
                index: i.index,
                size: i.size,
                name: i.name.clone(),
            })
            .collect(),
    };
    let (mut worker, first) = workers
        .google(program, folder.path(), token, &job)
        .await
        .map_err(worker_error)?;
    if first["ready"] != true {
        return Err(problem(code::google_worker_failed, json!({})));
    }
    if let Some((recv, audit)) = upload {
        let mut files = Vec::new();
        for input in &request.inputs {
            let hash = wire::copy(
                recv,
                worker.child.stdin.as_mut().expect("piped"),
                input.size,
            )
            .await
            .map_err(|_| problem(code::transfer_failed, json!({})))?;
            files.push(json!({"name":input.name,"size":input.size,"sha256":hash}));
        }
        audit["inputs"] = json!(files);
    }
    drop(worker.child.stdin.take());
    let response: Response = serde_json::from_value(worker.line().await.map_err(worker_error)?)
        .map_err(|_| problem(code::google_worker_failed, json!({})))?;
    match response {
        Response::Ok(value) => {
            let output: Output = serde_json::from_value(value)
                .map_err(|_| problem(code::google_worker_failed, json!({})))?;
            Ok((worker, output, folder))
        }
        error @ Response::Error { .. } => {
            let _ = worker.finish().await;
            Err(error.into_result().expect_err("an error"))
        }
    }
}
/// Before a timed `calendar create`: is the proposed time busy on any
/// connected account? The owner's commitments are on the calendars they show
/// (`selected` in the calendar list) on every account, read-only ones
/// included, plus the calendar `booked` on `target`, shown or not.
/// Fails with `calendar_conflict` listing busy spans and any account that
/// could not be checked; otherwise returns the calendars Google will never
/// read (no access), which do not block a booking.
async fn conflict(
    workers: &WorkerSetup,
    program: &Path,
    tokens: &Tokens,
    plan: &policy::Plan,
    target: &Account,
    booked: &str,
) -> Result<Vec<String>, Failure> {
    let (Some(from), Some(to)) = (plan.flag("from"), plan.flag("to")) else {
        return Err(super::gog_failure(2, &target.account));
    };
    // Query only the proposed span. No event titles or descriptions enter
    // this check or its refusal response.
    let mut busy = Vec::new();
    let mut unchecked = Vec::new();
    let mut unreadable = Vec::new();
    for account in checked_accounts(tokens, target) {
        let listing = Request {
            gog_args: vec!["calendar".into(), "calendars".into(), "--all".into()],
            account: vec![],
            confirm_conflict: false,
            inputs: vec![],
        };
        let listed = match invoke(workers, program, account, &listing, None).await {
            Ok((worker, output, _folder)) => {
                worker.finish().await.map_err(worker_error)?;
                Some(output.result)
            }
            Err(_) => None,
        };
        let booking = (account.account == target.account).then_some(booked);
        let Some(calendars) = calendars_to_check(listed.as_ref(), booking) else {
            unchecked.push(account.account.clone());
            continue;
        };
        // Nothing shown and nothing booked here: nothing to be busy on.
        if calendars.is_empty() {
            continue;
        }
        let request = Request {
            gog_args: vec![
                "calendar".into(),
                "freebusy".into(),
                "--cal".into(),
                calendars.join(","),
                "--from".into(),
                from.into(),
                "--to".into(),
                to.into(),
            ],
            account: vec![],
            confirm_conflict: false,
            inputs: vec![],
        };
        let answer = match invoke(workers, program, account, &request, None).await {
            Ok((worker, output, _folder)) => {
                worker.finish().await.map_err(worker_error)?;
                free_busy(&output.result["calendars"])
            }
            Err(_) => None,
        };
        let Some((spans, never)) = answer else {
            unchecked.push(account.account.clone());
            continue;
        };
        for span in spans {
            busy.push(
                json!({"account": account.account, "start": span["start"], "end": span["end"]}),
            );
        }
        unreadable.extend(never);
    }
    if !busy.is_empty() || !unchecked.is_empty() {
        let mut details = json!({"busy":busy,"unchecked":unchecked});
        if !unreadable.is_empty() {
            details["could_not_check"] = json!(unreadable);
        }
        return Err(problem(code::calendar_conflict, details));
    }
    Ok(unreadable)
}

/// Whether `account` lets a fan-out read of `command` run: Gmail reads
/// need mail access, calendar reads calendar access.
fn can_read(account: &Account, command: &str) -> bool {
    if command.starts_with("gmail.") {
        account.capabilities.mail_read
    } else if command.starts_with("calendar.") {
        account.capabilities.calendar_read
    } else {
        true
    }
}

/// The accounts a booking's conflict check reads: every one with calendar
/// access, and the one being booked on.
fn checked_accounts<'a>(tokens: &'a Tokens, target: &Account) -> Vec<&'a Account> {
    tokens
        .accounts
        .iter()
        .filter(|account| account.capabilities.calendar_read || account.account == target.account)
        .collect()
}

/// The calendars to probe on one account: `booking`, the calendar being
/// booked when this is the account it books on, then the ones it shows, from
/// its calendar list (`listed`, `None` when listing failed). `None` when the
/// account cannot be checked.
fn calendars_to_check(listed: Option<&Value>, booking: Option<&str>) -> Option<Vec<String>> {
    // A list that answered is a check, even with nothing shown on it.
    listed?["calendars"].as_array()?;
    let mut calendars: Vec<String> = booking.into_iter().map(str::to_owned).collect();
    for id in shown_calendars(listed?) {
        if !calendars.contains(&id) {
            calendars.push(id);
        }
    }
    Some(calendars)
}

/// The ids of the calendars the owner shows, from `calendar calendars`.
fn shown_calendars(result: &Value) -> Vec<String> {
    result["calendars"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|calendar| calendar["selected"] == true)
        .filter_map(|calendar| calendar["id"].as_str())
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .collect()
}

/// One account's free/busy answer (`calendars` of `calendar freebusy`): the
/// busy spans of every calendar that answered, and the ids of those Google
/// will never read (`notFound`, as a subscribed holiday calendar answers on
/// every call). `None`, the account unchecked, when nothing answered or a
/// calendar failed for a reason that might clear (a rate limit could have
/// hidden the very commitment being booked over).
fn free_busy(calendars: &Value) -> Option<(Vec<Value>, Vec<String>)> {
    let mut spans = Vec::new();
    let mut never = Vec::new();
    let mut answered = 0;
    for (id, calendar) in calendars.as_object()? {
        if !calendar.is_object() {
            continue;
        }
        let errors = calendar["errors"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if !errors.is_empty() {
            if !errors.iter().all(|error| error["reason"] == "notFound") {
                return None;
            }
            never.push(id.clone());
            continue;
        }
        answered += 1;
        for span in calendar["busy"].as_array().into_iter().flatten() {
            if span["start"].is_string() && span["end"].is_string() {
                spans.push(json!({"start": span["start"], "end": span["end"]}));
            }
        }
    }
    (answered > 0).then_some((spans, never))
}
/// Fails with `output_too_large` when `value` is over `MAX_OUTPUT` as JSON.
fn check_size(value: &Value) -> Result<(), Failure> {
    if serde_json::to_vec(value).expect("JSON").len() > MAX_OUTPUT {
        Err(problem(code::output_too_large, json!({"limit":MAX_OUTPUT})))
    } else {
        Ok(())
    }
}
/// The gog to run: the copy install.sh puts in the state directory's
/// plugins folder, in a folder named for the pinned `version`, so a latch
/// with a new pin never picks up the old gog. There is no other place.
pub(crate) fn gog_program(state_dir: &Path, version: &str) -> Result<PathBuf, Failure> {
    let path = state_dir.join("plugins/gog").join(version).join("gog");
    if path.exists() {
        return Ok(path);
    }
    let message = format!(
        "gog {version} is not installed at {}; run install.sh",
        path.display()
    );
    Err(Failure::new(code::gog_not_configured, message))
}
/// Sends one `Ok` answer frame to the client.
async fn frame(send: &mut SendStream, value: Value) -> Result<(), Failure> {
    wire::write_frame(
        send,
        &serde_json::to_vec(&Response::Ok(value)).expect("JSON"),
    )
    .await
    .map_err(|_| problem(code::connection_lost, json!({})))
}
/// The short reason for an account's failure in `outcomes`: the failure's
/// `reason` detail when it has one (gog's), else its code.
fn reason(failure: &Failure) -> &str {
    failure.details["reason"].as_str().unwrap_or(&failure.code)
}
fn worker_error(error: crate::worker::WorkerError) -> Failure {
    Failure::reported(&error.code, error.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::Exit;
    #[test]
    fn gog_comes_only_from_the_state_dirs_plugins_folder() {
        let state = tempfile::tempdir().unwrap();
        let installed = |base: &Path, version: &str| {
            let path = base.join(version);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("gog"), b"").unwrap();
            path.join("gog")
        };
        let missing = gog_program(state.path(), "0.36.0").unwrap_err();
        assert_eq!(missing.code, "gog_not_configured");
        assert!(
            missing.message.contains("plugins/gog/0.36.0/gog")
                && missing.message.contains("install.sh")
        );
        // The old place (~/.local/share/latch/gog) is never used.
        installed(&state.path().join(".local/share/latch/gog"), "0.36.0");
        assert!(gog_program(state.path(), "0.36.0").is_err());

        let plugins = state.path().join("plugins/gog");
        let old = installed(&plugins, "0.36.0");
        assert_eq!(gog_program(state.path(), "0.36.0").unwrap(), old);
        // A bumped pin selects only its own folder, never the old gog.
        let bumped = gog_program(state.path(), "0.37.0").unwrap_err();
        assert_eq!(bumped.code, "gog_not_configured");
        let new = installed(&plugins, "0.37.0");
        assert_eq!(gog_program(state.path(), "0.37.0").unwrap(), new);
    }
    #[test]
    fn the_host_uses_the_compiled_pins_version() {
        let pin: Value = serde_json::from_str(include_str!("pin.json")).unwrap();
        assert_eq!(PIN.version, pin["version"].as_str().unwrap());
        assert_eq!(PIN.hashes.len(), 4);
        let state = tempfile::tempdir().unwrap();
        let missing = gog_program(state.path(), &PIN.version);
        let message = missing.unwrap_err().message;
        assert!(message.contains(&format!("gog/{}/gog", pin["version"].as_str().unwrap())));
    }
    #[test]
    fn write_deadlines_are_unknown_but_reads_and_help_are_failed() {
        let cases: &[(&[&str], Exit)] = &[
            (
                &[
                    "gmail",
                    "send",
                    "--to",
                    "test@example.invalid",
                    "--subject",
                    "test",
                    "--body",
                    "test",
                ],
                Exit::Unknown,
            ),
            (
                &["gmail", "drafts", "delete", "draft", "--force"],
                Exit::Unknown,
            ),
            (&["gmail", "archive", "message"], Exit::Unknown),
            (
                &[
                    "calendar",
                    "update",
                    "primary",
                    "event",
                    "--summary",
                    "test",
                ],
                Exit::Unknown,
            ),
            (
                &[
                    "calendar", "respond", "primary", "event", "--status", "accepted",
                ],
                Exit::Unknown,
            ),
            (&["gmail", "get", "message"], Exit::Failed),
            (&["gmail", "drafts", "get", "draft"], Exit::Failed),
            (&["calendar", "event", "primary", "event"], Exit::Failed),
            (&["gmail", "search", "test-marker"], Exit::Failed),
            (&["mail", "send", "--help"], Exit::Failed),
        ];
        for (gog_args, expected) in cases {
            let request =
                json!({"gog_args":gog_args,"account":[],"inputs":[],"confirm_conflict":false});
            assert_eq!(
                timeout_failure(&request, Duration::from_secs(90)).exit,
                *expected,
                "{gog_args:?}"
            );
        }
    }
    #[test]
    fn free_busy_names_calendars_google_never_reads_and_gives_up_on_ones_that_might_clear() {
        let busy = json!({"start": "2026-09-18T22:30:00Z", "end": "2026-09-18T23:00:00Z"});
        // notFound never clears (a subscribed holiday calendar answers that
        // way on every call), so the window still counts as answered.
        assert_eq!(
            free_busy(&json!({
                "primary": {"busy": [busy]},
                "gone": {"errors": [{"reason": "notFound"}]},
            })),
            Some((vec![busy], vec!["gone".into()]))
        );
        // A rate limit might have hidden the commitment being booked over.
        assert_eq!(
            free_busy(&json!({
                "primary": {"busy": []},
                "later": {"errors": [{"reason": "rateLimitExceeded"}]},
            })),
            None
        );
        // Nothing answered is not an empty diary.
        assert_eq!(
            free_busy(&json!({"gone": {"errors": [{"reason": "notFound"}]}})),
            None
        );
        assert_eq!(free_busy(&json!(null)), None);
    }
    #[test]
    fn an_account_that_shows_no_calendars_is_still_checked_on_the_one_being_booked() {
        let nothing_shown = json!({"calendars": [
            {"id": "hidden@example.com", "accessRole": "owner", "selected": false},
        ]});
        // The booking account: its calendar list answered, so the calendar
        // being booked is probed even though none is shown.
        assert_eq!(
            calendars_to_check(Some(&nothing_shown), Some("primary")),
            Some(vec!["primary".to_owned()])
        );
        assert_eq!(
            calendars_to_check(Some(&json!({"calendars": []})), Some("primary")),
            Some(vec!["primary".to_owned()])
        );
        // Another account that shows nothing has nothing to probe, and is
        // checked, not a hole in the check.
        assert_eq!(calendars_to_check(Some(&nothing_shown), None), Some(vec![]));
        // A list that failed, or did not answer with a calendar list, is.
        assert_eq!(calendars_to_check(None, Some("primary")), None);
        assert_eq!(calendars_to_check(Some(&json!({})), Some("primary")), None);
        // The booking calendar comes first, and only once.
        let shown = json!({"calendars": [{"id": "me@example.com", "selected": true}]});
        assert_eq!(
            calendars_to_check(Some(&shown), Some("me@example.com")),
            Some(vec!["me@example.com".to_owned()])
        );
        assert_eq!(
            calendars_to_check(Some(&shown), Some("team@group.calendar.google.com")),
            Some(vec![
                "team@group.calendar.google.com".to_owned(),
                "me@example.com".to_owned()
            ])
        );
    }
    #[test]
    fn the_conflict_check_reads_the_calendars_the_owner_shows() {
        let listing = json!({"calendars": [
            {"id": "me@example.com", "accessRole": "owner", "selected": true},
            {"id": "colleague@example.com", "accessRole": "reader", "selected": true},
            {"id": "hidden@example.com", "accessRole": "writer"},
            {"id": "holidays", "accessRole": "reader", "selected": false},
        ]});
        assert_eq!(
            shown_calendars(&listing),
            ["me@example.com", "colleague@example.com"]
        );
    }
    #[tokio::test]
    async fn a_fan_out_runs_at_most_four_accounts_at_once_in_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let running = AtomicUsize::new(0);
        let most = AtomicUsize::new(0);
        // How many ran when a later one started: four again if each starts
        // as soon as a slot frees, rather than after a whole batch.
        let most_later = AtomicUsize::new(0);
        let futures = (0..9)
            .map(|i| {
                let (running, most, most_later) = (&running, &most, &most_later);
                async move {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(now, Ordering::SeqCst);
                    if i >= FANOUT_AT_ONCE {
                        most_later.fetch_max(now, Ordering::SeqCst);
                    }
                    // Later ones finish first, so order must come from the input.
                    tokio::time::sleep(Duration::from_millis(40 - 4 * i as u64)).await;
                    running.fetch_sub(1, Ordering::SeqCst);
                    i
                }
            })
            .collect();
        assert_eq!(
            at_most(FANOUT_AT_ONCE, futures).await,
            (0..9).collect::<Vec<_>>()
        );
        assert_eq!(most.load(Ordering::SeqCst), 4);
        assert_eq!(most_later.load(Ordering::SeqCst), 4);
    }
    /// A personal account with everything, and a work one connected for
    /// Gmail only, as Plow's token answer lists them.
    fn mail_only_work_account() -> Tokens {
        serde_json::from_value(json!({
            "accounts": [
                { "account": "me@example.com", "access_token": "test-placeholder",
                  "is_default": true,
                  "capabilities": { "mail_read": true, "mail_write": true,
                                    "calendar_read": true, "calendar_write": true } },
                { "account": "work@example.com", "access_token": "test-placeholder",
                  "is_default": false,
                  "capabilities": { "mail_read": true, "mail_write": true,
                                    "calendar_read": false, "calendar_write": false } },
            ],
        }))
        .unwrap()
    }

    #[test]
    fn a_fan_out_reads_only_accounts_granted_that_kind_of_read() {
        let tokens = mail_only_work_account();
        let [me, work] = [&tokens.accounts[0], &tokens.accounts[1]];
        assert!(can_read(me, "calendar.events") && can_read(me, "gmail.search"));
        assert!(!can_read(work, "calendar.events") && !can_read(work, "calendar.freebusy"));
        assert!(can_read(work, "gmail.search"));
    }

    #[test]
    fn a_booking_checks_conflicts_only_where_calendars_can_be_read() {
        let tokens = mail_only_work_account();
        let checked = |target: &Account| -> Vec<String> {
            checked_accounts(&tokens, target)
                .iter()
                .map(|a| a.account.clone())
                .collect()
        };
        // The Gmail-only account is not read, so it does not leave the
        // check "unchecked" and refuse every booking.
        assert_eq!(checked(&tokens.accounts[0]), ["me@example.com"]);
        // Booking on it still reads it.
        assert_eq!(
            checked(&tokens.accounts[1]),
            ["me@example.com", "work@example.com"]
        );
    }

    #[test]
    fn an_unavailable_connected_account_still_requires_account_selection() {
        let tokens: Tokens = serde_json::from_value(json!({
            "accounts": [{ "account": "healthy@example.com", "access_token": "test-placeholder",
                           "is_default": true,
                           "capabilities": { "mail_read": true, "mail_write": true,
                                             "calendar_read": true, "calendar_write": true } }],
            "degraded": [{ "account": "expired@example.com", "reason": "needs_reauth" }],
        }))
        .unwrap();
        let failure = select(&tokens, &[], false)
            .err()
            .expect("several accounts need --account");
        assert_eq!(failure.code, "account_required");
    }
}
