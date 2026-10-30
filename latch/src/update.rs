//! `latch update`: installs the newest latch, or the build a tag names, from
//! latch's releases; and the host's daily look for a newer build.
//!
//! Releases are published under `RELEASES`. Each release's files are in
//! `releases/<tag>/` and never change: the archive for each platform,
//! `install.sh`, `SHA256SUMS` and that file's minisign signature.
//! `latest.json`, signed too, names the newest release. An update downloads
//! this platform's archive, checks it against the signed `SHA256SUMS` and,
//! on macOS, checks who signed the app inside it. Then it runs the release's
//! own `install-binary.sh`, which replaces latch and restarts a host just as
//! running `install.sh` again would.
//!
//! Nothing installs unless the owner runs `latch update`, or an agent runs
//! it on its own computer, where latch is installed alone. A host only
//! looks, once a day, and the newer build shows up in
//! `status --local` and on its dashboard.

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::cli::UpdateArgs;
use crate::dashboard::Dashboard;
use crate::failure::{Failure, code};
use crate::log::log;
use crate::state::{StateDir, to_hex, write_private};

/// Where latch's releases are published.
pub const RELEASES: &str = "https://releases.plow.co/latch-cli";

/// The minisign public key (id DA695E5C4D2D36B9) that releases'
/// `SHA256SUMS` and `latest.json` are signed with: the base64 line of its
/// public key file. Its secret half is kept in 1Password, and only the
/// release workflow loads it.
const RELEASE_KEY: &str = "RWS5Ni1NXF5p2g7U9HvReN/6scLD8gpzhEZVW//t8Dkn4SyhqAdExFTu";

/// Who signs the app a macOS release carries, and the app's own id. A
/// different signer, or a re-signed app, would also lose the Full Disk
/// Access the owner granted to the one installed.
const APP_SIGNER: Signer = Signer {
    team: "3559PD337Z",
    identifier: "co.plow.latch.cli",
};

/// How often a host looks for a newer build, whether the last look worked
/// or not.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// One release: its tag, `v<version>-<build>`, and its build number.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Release {
    pub tag: String,
    pub build: u64,
}

impl Release {
    /// The release `tag` names. Only a release's tag, `v1.2.3-45`, names one.
    fn named(tag: &str) -> Result<Release, Failure> {
        let parsed = tag.strip_prefix('v').and_then(|rest| {
            let (version, build) = rest.split_once('-')?;
            let parts: Vec<&str> = version.split('.').collect();
            let numbers = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
            (parts.len() == 3 && parts.iter().all(|part| numbers(part)) && numbers(build))
                .then(|| build.parse().ok())
                .flatten()
        });
        let build = parsed.ok_or_else(|| {
            let message = format!("{tag:?} is not a release tag; tags look like v0.1.0-1327");
            Failure::new(code::bad_tag, message)
        })?;
        Ok(Release {
            tag: tag.to_string(),
            build,
        })
    }
}

/// Where releases come from, and what they must be signed with.
pub struct Source {
    url: String,
    key: PublicKey,
    /// Who must have signed a macOS release's app; none elsewhere.
    signer: Option<Signer>,
}

#[derive(Clone, Copy)]
struct Signer {
    team: &'static str,
    identifier: &'static str,
}

impl Source {
    /// latch's own releases.
    pub fn releases() -> Source {
        Source {
            url: RELEASES.to_string(),
            key: PublicKey::from_base64(RELEASE_KEY).expect("the release key is valid"),
            signer: cfg!(target_os = "macos").then_some(APP_SIGNER),
        }
    }

    /// One file under the releases' URL.
    async fn get(&self, path: &str) -> Result<Vec<u8>, Failure> {
        let url = format!("{}/{path}", self.url);
        let unreachable = |why: String| {
            let message = format!("could not download {url}: {why}");
            Failure::new(code::releases_unreachable, message)
        };
        let response = http()?
            .get(&url)
            .send()
            .await
            .map_err(|err| unreachable(err.to_string()))?;
        match response.status() {
            status if status.is_success() => Ok(response
                .bytes()
                .await
                .map_err(|err| unreachable(err.to_string()))?
                .to_vec()),
            reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::FORBIDDEN => Err(Failure::new(
                code::no_such_release,
                format!("{url} does not exist"),
            )),
            status => Err(unreachable(status.to_string())),
        }
    }

    /// `path`, once its minisign signature (`path.minisig`) checks out.
    async fn signed(&self, path: &str) -> Result<Vec<u8>, Failure> {
        let bytes = self.get(path).await?;
        let refused = |why: &str| {
            let message = format!("{}/{path} {why}; nothing was installed", self.url);
            Failure::new(code::bad_signature, message)
        };
        let signature = match self.get(&format!("{path}.minisig")).await {
            Err(failure) if failure.code == "no_such_release" => {
                return Err(refused("is not signed"));
            }
            signature => signature?,
        };
        check_signature(&self.key, &bytes, &signature).map_err(refused)?;
        Ok(bytes)
    }

    /// The newest release, as the signed `latest.json` names it.
    async fn latest(&self) -> Result<Release, Failure> {
        let bytes = self.signed("latest.json").await?;
        let latest: Release = serde_json::from_slice(&bytes).map_err(|err| {
            Failure::new(
                code::bad_release,
                format!("latest.json is not valid: {err}"),
            )
        })?;
        if Release::named(&latest.tag)? != latest {
            let message = format!(
                "latest.json names build {} for {}",
                latest.build, latest.tag
            );
            return Err(Failure::new(code::bad_release, message));
        }
        Ok(latest)
    }
}

/// Why `signature`, a `.minisig` file, does not sign `bytes` with `key`.
fn check_signature(key: &PublicKey, bytes: &[u8], signature: &[u8]) -> Result<(), &'static str> {
    let signature = std::str::from_utf8(signature)
        .ok()
        .and_then(|text| Signature::decode(text).ok())
        .ok_or("has a signature that cannot be read")?;
    key.verify(bytes, &signature, false)
        .map_err(|_| "is not signed with latch's release key")
}

/// `install.sh`'s check, on a Mac, that `file` is signed with `key` (a
/// minisign public key's base64 line) by `file.minisig` beside it. The
/// installer runs it only once it has checked who signed this app.
pub fn verify_file(key: &str, file: &Path) -> Result<Value, Failure> {
    let key = PublicKey::from_base64(key).map_err(|_| {
        Failure::new(
            code::bad_signature,
            format!("{key} is not a minisign public key"),
        )
    })?;
    let read = |path: &Path| {
        std::fs::read(path).map_err(|err| io_failure(&path.display().to_string(), err))
    };
    let mut signature = file.as_os_str().to_owned();
    signature.push(".minisig");
    check_signature(&key, &read(file)?, &read(Path::new(&signature))?)
        .map_err(|why| Failure::new(code::bad_signature, format!("{} {why}", file.display())))?;
    Ok(json!({ "verified": file }))
}

fn http() -> Result<reqwest::Client, Failure> {
    crate::plow::http(
        reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .retry(reqwest::retry::never()),
    )
}

/// This latch's build number; none for a local build.
pub fn current() -> Option<u64> {
    crate::cli::BUILD_NUMBER.and_then(|number| number.parse().ok())
}

/// `latch update [--check] [TAG]`.
pub async fn run(state: &StateDir, args: UpdateArgs) -> Result<Value, Failure> {
    may_update(state, &installations())?;
    let source = Source::releases();
    let bin = bin_dir();
    let tag = args.tag.as_deref();
    update(state, &source, current(), bin.as_deref(), tag, args.check).await
}

/// Where the installer puts latch. On Linux, the folder this latch runs
/// from, so the update replaces it there and not in the installer's default
/// folder. On macOS, none: the installer replaces the app, in
/// /Applications or ~/Applications, which every link to latch leads into.
fn bin_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return None;
    }
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}

/// Installs the release `tag` names, or the newest one if it is newer than
/// build `current`, into `bin` if given; with `check`, only says whether
/// there is a newer one.
async fn update(
    state: &StateDir,
    source: &Source,
    current: Option<u64>,
    bin: Option<&Path>,
    tag: Option<&str>,
    check: bool,
) -> Result<Value, Failure> {
    // A local build has no number to update from: refused before anything
    // is downloaded.
    if current.is_none() && !check {
        let message = "this latch was built from a checkout, not released; \
                       build it again, or install a release with install.sh";
        return Err(Failure::new(code::local_build, message.into()));
    }
    let release = match tag {
        Some(tag) => Release::named(tag)?,
        None => source.latest().await?,
    };
    if check {
        let available = newer(Some(release.build), current);
        return Ok(json!({"current": current, "latest": release, "available": available}));
    }
    let current = current.expect("a local build was refused above");
    // Only a named tag goes back to an older build. This build installs
    // again, unnamed, when helpers it runs are missing.
    let repair = match tag.is_none() && release.build == current {
        true => missing_helpers(&state.path),
        false => Vec::new(),
    };
    if tag.is_none() && release.build <= current && repair.is_empty() {
        return Ok(json!({"updated": false, "current": current, "latest": release}));
    }
    let before = crate::status::local(state)?.this_machine.hosting;
    install(state, source, &release, bin).await?;
    let mut answer = json!({"updated": true, "from": current, "to": release});
    if !repair.is_empty() {
        answer["repaired"] = json!(repair);
    }
    let after = crate::status::local(state)?.this_machine.hosting.running;
    if let Some(host) = host_after(before.running, before.attached, after) {
        answer["host"] = host;
    }
    Ok(answer)
}

/// Whether this login may update this machine's latch. An update replaces
/// the latch every state folder here runs, and restarts the host other
/// callers use, so on a host only the owner's own login runs one. An agent
/// may update its own computer: latch installed alone in its state folder
/// (`install-binary.sh --client-only`), and no host running on this machine
/// from the latch the update replaces, whatever its state folder. No host
/// operation starts an update, so no caller reaches one through a host.
fn may_update(state: &StateDir, installations: &[PathBuf]) -> Result<(), Failure> {
    let Some(config) = state.load_config()? else {
        return Ok(());
    };
    if config.peer.kind == "device" {
        return Ok(());
    }
    let client_only = state.path.join("plugins/client-only").is_file();
    if client_only
        && !crate::status::local(state)?.this_machine.hosting.running
        && !host_runs_this_latch(installations)
    {
        return Ok(());
    }
    let message = "this machine is a host, and a host's latch is updated by its owner; \
                   an agent updates latch only on its own computer, installed with install.sh --client-only";
    Err(Failure::new(code::owner_only, message.into()))
}

/// The latch an update replaces: this one, and on macOS the app in each
/// folder the installer puts it in, which it moves this one into: the
/// shared one (`$LATCH_APPLICATIONS`, as the installer reads it, else
/// /Applications) and the user's own.
fn installations() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::env::current_exe().into_iter().collect();
    if cfg!(target_os = "macos") {
        let shared = std::env::var_os("LATCH_APPLICATIONS").unwrap_or("/Applications".into());
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let folders = [
            Some(PathBuf::from(shared)),
            home.map(|home| home.join("Applications")),
        ];
        found.extend(
            folders
                .into_iter()
                .flatten()
                .map(|folder| folder.join("Plow Latch.app/Contents/MacOS/latch")),
        );
    }
    found
        .into_iter()
        .filter_map(|path| path.canonicalize().ok())
        .collect()
}

/// Whether a host runs on this machine from one of `installations`: a
/// `latch serve` process of any state folder, any user's. When the
/// processes can't be listed, one is taken to run.
fn host_runs_this_latch(installations: &[PathBuf]) -> bool {
    let Some(hosts) = hosts() else {
        return true;
    };
    hosts
        .iter()
        .filter_map(|exe| exe.canonicalize().ok())
        .any(|exe| installations.contains(&exe))
}

/// The executables of this machine's `latch serve` processes but this one.
#[cfg(target_os = "linux")]
fn hosts() -> Option<Vec<PathBuf>> {
    let me = std::process::id().to_string();
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let pid = entry.file_name();
        let pid = pid.to_string_lossy();
        if !pid.bytes().all(|b| b.is_ascii_digit()) || pid == me {
            continue;
        }
        let Ok(argv) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let mut words = argv.split(|b| *b == 0).map(String::from_utf8_lossy);
        let program = words.next().unwrap_or_default().into_owned();
        if serves(words) {
            // Another user's process hides its executable; its command
            // line still names it.
            let exe = std::fs::read_link(format!("/proc/{pid}/exe"));
            found.push(exe.unwrap_or_else(|_| PathBuf::from(program)));
        }
    }
    Some(found)
}

/// Windows cannot list them here yet: a host is taken to run.
#[cfg(windows)]
fn hosts() -> Option<Vec<PathBuf>> {
    None
}

/// The executables of this machine's `latch serve` processes but this one.
#[cfg(target_os = "macos")]
fn hosts() -> Option<Vec<PathBuf>> {
    let listed = std::process::Command::new("ps")
        .args(["-A", "-ww", "-o", "pid=,args="])
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let me = std::process::id();
    let mut found = Vec::new();
    for line in String::from_utf8_lossy(&listed.stdout).lines() {
        let mut words = line.split_whitespace();
        let Some(pid) = words.next().and_then(|pid| pid.parse::<i32>().ok()) else {
            continue;
        };
        if pid as u32 == me || !serves(words.map(std::borrow::Cow::Borrowed)) {
            continue;
        }
        let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: the buffer is as long as the size passed, which
        // proc_pidpath writes at most.
        let written =
            unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
        if written > 0 {
            path.truncate(written as usize);
            use std::os::unix::ffi::OsStringExt;
            found.push(PathBuf::from(std::ffi::OsString::from_vec(path)));
        }
    }
    Some(found)
}

/// Whether a command line's words after the program run `latch serve`.
#[cfg(unix)]
fn serves<'a>(mut words: impl Iterator<Item = std::borrow::Cow<'a, str>>) -> bool {
    words.any(|word| word == "serve")
}

/// What an update did to the host on this state folder, when one ran. The
/// installer stops it, and starts again only one `serve -d` started; one
/// that ran in a terminal stays stopped, with the command that starts it.
fn host_after(was_running: bool, attached: bool, running: bool) -> Option<Value> {
    if !was_running {
        return None;
    }
    if running {
        return Some(json!({"running": true}));
    }
    let start = if attached {
        "latch serve"
    } else {
        "latch serve --foreground"
    };
    Some(json!({"running": false, "next": start}))
}

/// Downloads `release`'s archive for this platform, checks it, and runs its
/// `install-binary.sh` on this state folder, into `bin` if given.
async fn install(
    state: &StateDir,
    source: &Source,
    release: &Release,
    bin: Option<&Path>,
) -> Result<(), Failure> {
    let name = format!("latch-{}-{}", release.tag, target()?);
    let folder = format!("releases/{}", release.tag);
    let sums = source.signed(&format!("{folder}/SHA256SUMS")).await?;
    let archive = source.get(&format!("{folder}/{name}.tar.gz")).await?;
    let listed = String::from_utf8_lossy(&sums)
        .lines()
        .find_map(|line| {
            let (hash, file) = line.split_once("  ")?;
            (file == format!("{name}.tar.gz")).then(|| hash.to_string())
        })
        .ok_or_else(|| {
            let message = format!("{}'s SHA256SUMS does not list {name}.tar.gz", release.tag);
            Failure::new(code::bad_release, message)
        })?;
    if to_hex(&Sha256::digest(&archive)) != listed {
        let message = format!(
            "the downloaded {name}.tar.gz is not the one {}'s SHA256SUMS lists; nothing was installed",
            release.tag
        );
        return Err(Failure::new(code::bad_download, message));
    }
    let unpacked = tempfile::tempdir().map_err(|err| io_failure("a download folder", err))?;
    let path = unpacked.path().join(format!("{name}.tar.gz"));
    std::fs::write(&path, &archive).map_err(|err| io_failure(&path.display().to_string(), err))?;
    // What tar and the installer print is for the owner, in trace.log: this
    // command's stdout is its JSON, its stderr only its error line.
    let log = crate::trace::file(state)?;
    let (out, err) = crate::trace::output(&log)?;
    let untar = tokio::process::Command::new("tar")
        .arg("-xzf")
        .arg(&path)
        .arg("-C")
        .arg(unpacked.path())
        .stdout(out)
        .stderr(err)
        .status()
        .await
        .map_err(|err| io_failure("tar", err))?;
    if !untar.success() {
        return Err(Failure::new(
            code::bad_download,
            format!(
                "{name}.tar.gz could not be unpacked; tar's output is in {}",
                state.path.join(crate::trace::FILE).display()
            ),
        ));
    }
    let setup = unpacked.path().join(&name);
    if let Some(signer) = source.signer {
        signer.check(&setup.join("Plow Latch.app")).await?;
    }
    let dir = state.path.canonicalize().unwrap_or(state.path.clone());
    let (out, err) = crate::trace::output(&log)?;
    let mut installer = tokio::process::Command::new("sh");
    installer
        .arg(setup.join("install-binary.sh"))
        .arg("--state-dir")
        .arg(&dir);
    if let Some(bin) = bin {
        installer.arg("--bin-dir").arg(bin);
    }
    let installed = installer
        .arg("--non-interactive")
        .args(helpers(&dir))
        .arg(setup.join("latch"))
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .status()
        .await
        .map_err(|err| io_failure("install-binary.sh", err))?;
    if !installed.success() {
        let message = format!(
            "{}'s install-binary.sh failed; its output, and what it says it left in place, is in {}",
            release.tag,
            state.path.join(crate::trace::FILE).display()
        );
        return Err(Failure::new(code::install_failed, message));
    }
    Ok(())
}

/// The release target this latch runs on.
fn target() -> Result<&'static str, Failure> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-musl"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-musl"),
        (os, arch) => Err(Failure::new(
            code::unsupported_platform,
            format!("latch has no release for {os} on {arch}"),
        )),
    }
}

/// What the installer puts in beside latch: nothing on a machine that
/// `install-binary.sh --client-only` left latch alone on; else the host's
/// helpers and the browser engines installed in `state`, or none.
fn helpers(state: &Path) -> Vec<&'static str> {
    if state.join("plugins/client-only").is_file() {
        return vec!["--client-only"];
    }
    let mut flags = Vec::new();
    if state.join("plugins/cft").is_dir() {
        flags.extend(["--browser", "chrome"]);
    }
    if state.join("plugins/camoufox").is_dir() {
        flags.extend(["--browser", "camoufox"]);
    }
    if flags.is_empty() {
        flags.push("--no-browser");
    }
    flags
}

/// The helpers `helpers` has the installer keep up to date whose versions
/// this latch runs are not installed: what an update cut off after the
/// installer replaced latch, before it fetched the new pins, leaves out.
fn missing_helpers(state: &Path) -> Vec<&'static str> {
    if state.join("plugins/client-only").is_file() {
        return Vec::new();
    }
    let not_installed = |engine: &str| {
        crate::browser::engine::verify(state, engine)
            .is_err_and(|failure| failure.code == code::engine_not_installed.name)
    };
    let mut missing = Vec::new();
    if crate::google::host::gog_program(state, &crate::google::PIN.version).is_err() {
        missing.push("gog");
    }
    if state.join("plugins/cft").is_dir() && not_installed("cft") {
        missing.push("chrome");
    }
    if state.join("plugins/camoufox").is_dir() && not_installed("camoufox") {
        missing.push("camoufox");
    }
    missing
}

impl Signer {
    /// Refuses `app` unless this signer signed it, intact, as this app,
    /// and Gatekeeper accepts it.
    async fn check(&self, app: &Path) -> Result<(), Failure> {
        let refused = |why: String| {
            let message = format!(
                "the release's {} {why}; nothing was installed",
                app.display()
            );
            Failure::new(code::bad_app_signature, message)
        };
        let shown = tool("codesign", &["-dvv".as_ref(), app.as_os_str()]).await?;
        let field = |name: &str| {
            shown
                .lines()
                .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
                .unwrap_or("none")
                .to_string()
        };
        let (team, identifier) = (field("TeamIdentifier"), field("Identifier"));
        if team != self.team || identifier != self.identifier {
            return Err(refused(format!(
                "is {identifier} signed by team {team}, not {} by team {}",
                self.identifier, self.team
            )));
        }
        let strict = ["--verify", "--deep", "--strict"];
        let args: Vec<&std::ffi::OsStr> = strict
            .iter()
            .map(|arg| arg.as_ref())
            .chain([app.as_os_str()])
            .collect();
        tool("codesign", &args)
            .await
            .map_err(|_| refused("is not intact as signed".into()))?;
        tool(
            "spctl",
            &[
                "--assess".as_ref(),
                "--type".as_ref(),
                "execute".as_ref(),
                app.as_os_str(),
            ],
        )
        .await
        .map_err(|_| refused("is not accepted by Gatekeeper".into()))?;
        Ok(())
    }
}

/// Runs a system tool; what it printed on stderr, where codesign prints
/// what it shows, if it succeeded.
async fn tool(program: &str, args: &[&std::ffi::OsStr]) -> Result<String, Failure> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|err| io_failure(program, err))?;
    if !output.status.success() {
        let message = format!(
            "{program}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return Err(Failure::new(code::tool_failed, message));
    }
    Ok(String::from_utf8_lossy(&output.stderr).to_string())
}

fn io_failure(what: &str, err: std::io::Error) -> Failure {
    Failure::new(code::update_io, format!("{what}: {err}"))
}

/// Whether `latest` is a newer build than `current`. A local build (no
/// number) is never behind.
fn newer(latest: Option<u64>, current: Option<u64>) -> bool {
    matches!((latest, current), (Some(latest), Some(current)) if latest > current)
}

/// What the host last learned, in `<state-dir>/update.json`.
#[derive(Serialize, Deserialize, Default, Debug)]
struct Checked {
    /// When it last looked, whether that worked or not.
    attempted_at: Option<Timestamp>,
    /// When a look last worked.
    checked_at: Option<Timestamp>,
    /// The newest release the last look that worked found.
    latest: Option<Release>,
    /// When a terminal was last told of a newer build.
    noticed_at: Option<Timestamp>,
}

impl Checked {
    fn path(state: &Path) -> PathBuf {
        state.join("update.json")
    }

    /// What is saved; nothing if nothing (or nothing readable) is.
    fn load(state: &Path) -> Checked {
        std::fs::read(Checked::path(state))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, state: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).expect("update state serializes") + "\n";
        write_private(&Checked::path(state), text.as_bytes())
    }

    /// When the next look is due: a day after the last; now if none.
    fn due(&self) -> Option<Timestamp> {
        let day = SignedDuration::try_from(CHECK_EVERY).expect("a day fits");
        self.attempted_at?.checked_add(day).ok()
    }
}

/// `status --local`'s `update`: whether a newer build is known, which, and
/// when a host last looked.
#[derive(Serialize, Debug, PartialEq)]
pub struct Status {
    pub available: bool,
    pub build: Option<u64>,
    pub checked_at: Option<Timestamp>,
}

pub fn status(state: &Path) -> Status {
    status_of(&Checked::load(state), current())
}

fn status_of(checked: &Checked, current: Option<u64>) -> Status {
    let build = checked.latest.as_ref().map(|latest| latest.build);
    Status {
        available: newer(build, current),
        build,
        checked_at: checked.checked_at,
    }
}

/// `LATCH_NO_UPDATE_CHECK=1` turns off every look and every notice.
fn checks_off() -> bool {
    std::env::var_os("LATCH_NO_UPDATE_CHECK").is_some_and(|value| value == "1")
}

/// Starts the host's daily look for a newer build, which shows on its
/// `dashboard` and in `status --local`. A local build, or one with checks
/// turned off, never looks. A look that fails is logged and tried again a
/// day later; it never affects the host.
pub fn watch(state: &Path, dashboard: &Dashboard) {
    let Some(current) = current() else {
        return;
    };
    if checks_off() {
        log("updates: not looking for a newer build (LATCH_NO_UPDATE_CHECK=1)");
        return;
    }
    let source = Source::releases();
    let (state, dashboard) = (state.to_path_buf(), dashboard.clone());
    tokio::spawn(async move {
        loop {
            let checked = Checked::load(&state);
            dashboard.available(available_line(&checked, current));
            let wait = checked.due().map_or(Duration::ZERO, |due| {
                Duration::try_from(due.duration_since(Timestamp::now())).unwrap_or(Duration::ZERO)
            });
            tokio::time::sleep(wait).await;
            if let Err(failure) = look(&source, &state).await {
                log(&format!(
                    "updates: could not look for a newer build: {}",
                    failure.message
                ));
            }
        }
    });
}

/// Looks for the newest release once, and saves when it looked and, if
/// that worked, what it found.
async fn look(source: &Source, state: &Path) -> Result<Release, Failure> {
    let found = source.latest().await;
    let mut checked = Checked::load(state);
    let now = Timestamp::now();
    checked.attempted_at = Some(now);
    if let Ok(release) = &found {
        checked.latest = Some(release.clone());
        checked.checked_at = Some(now);
    }
    checked
        .save(state)
        .map_err(|err| io_failure(&Checked::path(state).display().to_string(), err))?;
    found
}

/// The dashboard's line for a newer build, if there is one.
fn available_line(checked: &Checked, current: u64) -> Option<String> {
    let latest = checked.latest.as_ref()?;
    (latest.build > current)
        .then(|| format!("Build {} is available. Run `latch update`.", latest.build))
}

/// At a terminal (stdout and stderr both, and not CI), one line on stderr
/// once a day while a newer build is known. Anywhere else, nothing.
pub fn notice(state: &Path) {
    let terminal = crate::term::stdout_is_terminal() && crate::term::stderr_is_terminal();
    if !terminal || checks_off() || std::env::var_os("CI").is_some() {
        return;
    }
    let Some(current) = current() else {
        return;
    };
    let mut checked = Checked::load(state);
    let now = Timestamp::now();
    if let Some(line) = notice_line(&checked, current, now) {
        eprintln!("{line}");
        checked.noticed_at = Some(now);
        let _ = checked.save(state);
    }
}

fn notice_line(checked: &Checked, current: u64, now: Timestamp) -> Option<String> {
    let day = SignedDuration::try_from(CHECK_EVERY).expect("a day fits");
    if checked
        .noticed_at
        .is_some_and(|at| now.duration_since(at) < day)
    {
        return None;
    }
    let latest = checked.latest.as_ref()?;
    (latest.build > current).then(|| {
        format!(
            "latch: build {} is available; run `latch update` to install it",
            latest.build
        )
    })
}
