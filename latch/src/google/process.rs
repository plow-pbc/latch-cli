//! Runs the pinned gog inside an already confined worker. Only this process
//! receives a Google token; file names are rewritten into its call folder.
use std::fs::{self, File};
use std::io::{self, BufRead, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::{File as MovedFile, Input, MAX_OUTPUT, Output, gog_failure, policy, problem};
use crate::failure::{Failure, code};
use crate::wire::Response;

/// What the host tells the worker, as one JSON line on its stdin. The bytes
/// of each input file follow it on stdin.
#[derive(Deserialize, serde::Serialize)]
pub struct Job {
    pub account: String,
    pub argv: Vec<String>,
    pub inputs: Vec<Input>,
}

/// Refuses to run gog unless its sha256 is one of the pinned release's.
pub fn verify(program: &Path) -> Result<(), Failure> {
    // A test build's stand-in is a latch built with it (see `fake.rs`):
    // its bytes hold the marker. (A worker has no /proc to find its own.)
    #[cfg(feature = "fake-gog")]
    if fs::read(program).is_ok_and(|bytes| {
        let marker = super::fake::MARKER.as_bytes();
        bytes.windows(marker.len()).any(|window| window == marker)
    }) {
        return Ok(());
    }
    let mut file = File::open(program).map_err(|_| problem(code::gog_unverified, json!({})))?;
    let digest = sha256(&mut file).map_err(|_| problem(code::gog_unverified, json!({})))?;
    if !super::PIN.hashes.contains(&digest) {
        return Err(problem(code::gog_unverified, json!({})));
    }
    Ok(())
}

/// The worker's whole job: read the `Job`, parse its arguments again, check
/// gog, say "ready", take the input files, run gog, then write one response
/// line followed by the bytes of each file gog wrote.
pub fn run(program: &Path) -> Result<(), Failure> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut line = String::new();
    input.read_line(&mut line).map_err(io_error)?;
    let job: Job =
        serde_json::from_str(&line).map_err(|_| problem(code::bad_request, json!({})))?;
    let mut plan = policy::parse(&job.argv)?;
    verify(program)?;
    let root = std::env::current_dir().map_err(io_error)?;
    fs::create_dir(root.join("in")).map_err(io_error)?;
    fs::create_dir(root.join("out")).map_err(io_error)?;
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{}", json!({"ready":true})).map_err(io_error)?;
    stdout.flush().map_err(io_error)?;
    let result = execute(
        program,
        &root,
        &mut plan,
        &job.inputs,
        &job.account,
        &mut input,
    );
    let response = match &result {
        Ok(output) => Response::Ok(serde_json::to_value(output).expect("output serializes")),
        Err(failure) => Response::error(failure),
    };
    writeln!(
        stdout,
        "{}",
        serde_json::to_string(&response).expect("response serializes")
    )
    .map_err(io_error)?;
    if let Ok(output) = result {
        for moved in output.files {
            let mut file = File::open(root.join("out").join(moved.path)).map_err(io_error)?;
            io::copy(&mut file, &mut stdout).map_err(io_error)?;
        }
    }
    stdout.flush().map_err(io_error)
}

/// Writes the input files into `in/`, points gog's file arguments into the
/// call folder, runs gog with an environment of just its token and home
/// folder, and collects its JSON and the files it wrote under `out/`.
fn execute(
    program: &Path,
    root: &Path,
    plan: &mut policy::Plan,
    inputs: &[Input],
    account: &str,
    input: &mut impl Read,
) -> Result<Output, Failure> {
    if plan.inputs.len() != inputs.len() {
        return Err(problem(code::not_allowed, json!({})));
    }
    for (n, (wanted, sent)) in plan.inputs.iter().zip(inputs).enumerate() {
        if sent.index != wanted.index
            || Path::new(&sent.name).file_name().and_then(|s| s.to_str())
                != Some(sent.name.as_str())
        {
            return Err(problem(code::not_allowed, json!({})));
        }
        let folder = root.join("in").join(n.to_string());
        fs::create_dir(&folder).map_err(io_error)?;
        let path = folder.join(&sent.name);
        let mut file = File::create(&path).map_err(io_error)?;
        let copied = io::copy(&mut input.take(sent.size), &mut file).map_err(io_error)?;
        if copied != sent.size {
            return Err(problem(code::transfer_failed, json!({})));
        }
        plan.argv[wanted.index] = path.to_string_lossy().into_owned();
    }
    let mut home = root.join("home");
    for (n, output) in plan.outputs.iter().enumerate() {
        let folder = root.join("out").join(n.to_string());
        fs::create_dir(&folder).map_err(io_error)?;
        if output.index == usize::MAX {
            home = folder;
            continue;
        }
        let path = if output.directory {
            folder
        } else {
            folder.join("download")
        };
        plan.argv[output.index] = path.to_string_lossy().into_owned();
    }
    fs::create_dir_all(&home).map_err(io_error)?;
    let token = std::env::var("GOG_ACCESS_TOKEN")
        .map_err(|_| super::needs_sign_in_on(vec![account.to_string()]))?;
    // An AppContainer may not open the NUL device `Stdio::null` opens: on
    // Windows gog's stdin and stderr are pipes whose other ends close as it
    // starts, so it reads nothing and its errors go nowhere.
    #[cfg(windows)]
    let (stdin, stderr) = (Stdio::piped(), Stdio::piped());
    #[cfg(unix)]
    let (stdin, stderr) = (Stdio::null(), Stdio::null());
    let mut command = Command::new(program);
    command
        .args(["--json", "--no-input", "--wrap-untrusted", "--color=never"])
        .args(if plan.fanout() && plan.projected() {
            vec!["--results-only"]
        } else {
            vec![]
        })
        .arg(format!(
            "--enable-commands-exact={}",
            policy::commands().join(",")
        ))
        .arg("--account")
        .arg(account)
        .arg("--home")
        .arg(&home)
        .args(&plan.argv)
        .env_clear()
        .env("GOG_ACCESS_TOKEN", &token)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .current_dir(root)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(stderr);
    // Go starts Windows' sockets only with SystemRoot set. The folders it
    // asks Windows for are the call's: its home, and a temp folder apart
    // from the files it writes.
    #[cfg(windows)]
    {
        if let Some(system) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system);
        }
        for name in ["USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
            command.env(name, &home);
        }
        let temp = root.join("tmp");
        fs::create_dir(&temp).map_err(io_error)?;
        command.env("TEMP", &temp).env("TMP", &temp);
    }
    let mut child = command.spawn().map_err(io_error)?;
    drop((child.stdin.take(), child.stderr.take()));
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .expect("piped")
        .take(MAX_OUTPUT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > MAX_OUTPUT {
        let _ = child.kill();
        let _ = child.wait();
        return Err(problem(code::output_too_large, json!({"limit":MAX_OUTPUT})));
    }
    let status = child.wait().map_err(io_error)?;
    if status.code() == Some(3) {
        return Ok(Output {
            result: json!({}),
            files: vec![],
        });
    }
    if !status.success() {
        return Err(gog_failure(status.code().unwrap_or(1), account));
    }
    if !token.is_empty() && bytes.windows(token.len()).any(|w| w == token.as_bytes()) {
        return Err(problem(code::unsafe_output, json!({})));
    }
    let mut result = if plan.help {
        json!({"usage":String::from_utf8_lossy(&bytes)})
    } else {
        serde_json::from_slice(&bytes)
            .map_err(|_| problem(code::google_failed, json!({"reason":"invalid_output"})))?
    };
    let mut files = Vec::new();
    collect(&root.join("out"), &root.join("out"), &mut files).map_err(io_error)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    if plan.command == "gmail.attachment" && !files.is_empty() {
        // The standalone attachment command returns file metadata only.
        result = json!({});
    } else {
        rewrite_download_paths(&mut result, &root.join("out"), plan)?;
    }
    Ok(Output { result, files })
}
/// gog's JSON names downloaded files by their path in the worker's call
/// folder; replace each with the path the file will have on the client.
fn rewrite_download_paths(
    value: &mut serde_json::Value,
    output: &Path,
    plan: &policy::Plan,
) -> Result<(), Failure> {
    match value {
        serde_json::Value::Object(fields) => {
            if let Some(path) = fields.get("path").and_then(|v| v.as_str())
                && let Some(relative) = relative(Path::new(path), output)
            {
                fields.insert("path".into(), json!(plan.destination(&relative)?));
            }
            for value in fields.values_mut() {
                rewrite_download_paths(value, output, plan)?;
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                rewrite_download_paths(value, output, plan)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Lists every file below `folder` with its size and sha256, named by its
/// path relative to `root`. Anything that is not a file or folder is an error.
fn collect(root: &Path, folder: &Path, files: &mut Vec<MovedFile>) -> io::Result<()> {
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(root, &entry.path(), files)?;
        } else if kind.is_file() {
            let mut file = File::open(entry.path())?;
            let size = file.metadata()?.len();
            let hash = sha256(&mut file)?;
            files.push(MovedFile {
                path: relative(&entry.path(), root).expect("child"),
                size,
                sha256: hash,
            });
        } else {
            return Err(io::Error::other("unexpected output file type"));
        }
    }
    Ok(())
}
/// `path` below `root`, its parts joined by `/` on every system, as the
/// client's `Plan::destination` reads it.
fn relative(path: &Path, root: &Path) -> Option<String> {
    let parts: Vec<_> = path
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    Some(parts.join("/"))
}

/// An I/O failure inside the worker. Only the code `google_worker_failed`
/// is reported, not the error's text.
fn io_error(_: io::Error) -> Failure {
    problem(code::google_worker_failed, json!({}))
}

pub(crate) fn sha256(input: &mut impl Read) -> io::Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 256 * 1024];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(crate::state::to_hex(&hash.finalize()))
}
