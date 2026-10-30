//! The browser engines a host runs, as plugins: Chrome for Testing (CfT),
//! the release pinned in `cft.json`, which install.sh downloads into the
//! state directory's plugins folder, `<state-dir>/plugins/cft/<version>/`,
//! beside older versions, with that pin copied in as `manifest.json`; and
//! Camoufox, pinned in `camoufox.json` with the Node and Playwright runtime
//! its helper runs on, which install.sh adds only with `--browser camoufox`.
//! `latch browser setup` installs either later; an agent's call never
//! downloads one.
//!
//! The pin is compiled into latch and is what counts: latch hashes the
//! installed build's whole folder (`tree_hash`) and refuses one that differs
//! from the pin, so a changed library or helper is caught, not only a changed
//! main binary. A full hash that matches leaves a receipt beside the build:
//! the hashes it matched and every file's metadata then. Before each launch
//! latch compares the metadata alone, and hashes the whole folder again only
//! when anything in it was touched since (`checked`).

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::failure::{Failure, code};
use crate::state::to_hex;

/// One engine release: what install.sh fetches and latch launches.
#[derive(Deserialize)]
pub struct Pin {
    pub engine: String,
    pub version: String,
    /// The version of the driver operations this engine's driver speaks.
    pub driver_contract: u32,
    /// Per OS and architecture (`mac_arm64`, …).
    pub builds: BTreeMap<String, Build>,
    /// The launch arguments, always passed, before latch's own per-session ones.
    pub recipe: Vec<String>,
    /// Optional driver features this engine has.
    pub capabilities: Vec<String>,
}

#[derive(Deserialize)]
pub struct Build {
    /// Checked by browser-engines.sh, which downloads the zip (on Windows,
    /// `latch browser setup` itself).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub url: String,
    #[cfg_attr(not(windows), allow(dead_code))]
    pub zip_sha256: String,
    /// The folder the zip unpacks to, inside the version folder.
    pub folder: String,
    /// `tree_hash` of that folder, checked before every launch.
    pub tree_sha256: String,
    /// The program to start, inside `folder`.
    pub executable: String,
    /// Launch arguments for this build only, after the recipe's: the
    /// macOS builds' `--disable-crashpad-for-testing`, which on Linux arm64
    /// crashes the network service at once.
    #[serde(default)]
    pub args: Vec<String>,
}

pub static CFT: LazyLock<Pin> =
    LazyLock::new(|| serde_json::from_str(include_str!("cft.json")).expect("CfT pin"));

pub static CAMOUFOX: LazyLock<CamoufoxPin> =
    LazyLock::new(|| serde_json::from_str(include_str!("camoufox.json")).expect("Camoufox pin"));

/// The Camoufox release, and the runtime its helper (`camoufox/helper.mjs`)
/// runs on: a pinned Node, and the helper's packages (Playwright and
/// camoufox-js) as its lockfile pins them. install.sh puts each part in its
/// own folder of `<state-dir>/plugins/camoufox/<version>/`: `camoufox/`
/// (the app and the version.json camoufox-js reads), `node/` and
/// `runtime/` (the package, installed with `npm ci`).
// Camoufox has no Windows build: there only its pin's checks run.
#[derive(Deserialize)]
#[cfg_attr(windows, allow(dead_code))]
pub struct CamoufoxPin {
    pub engine: String,
    pub version: String,
    pub driver_contract: u32,
    pub builds: BTreeMap<String, CamoufoxBuild>,
    /// The operating system the fingerprint claims, independent of the build.
    pub fingerprint_os: String,
    pub capabilities: Vec<String>,
}

#[derive(Deserialize)]
pub struct CamoufoxBuild {
    /// The program inside the unpacked Camoufox folder.
    pub executable: String,
    pub camoufox: Download,
    pub node: Download,
    /// `tree_hash` of the installed runtime folder.
    pub runtime_tree_sha256: String,
}

/// One downloaded part: install.sh checks the archive's sha256, latch the
/// unpacked folder's tree hash before every launch.
#[derive(Deserialize)]
pub struct Download {
    #[allow(dead_code)]
    pub url: String,
    #[allow(dead_code)]
    pub sha256: String,
    pub tree_sha256: String,
}

/// An installed, checked Camoufox, ready to launch.
#[cfg_attr(windows, allow(dead_code))]
pub struct CamoufoxEngine {
    pub node: PathBuf,
    pub runtime: PathBuf,
    /// The folder camoufox-js reads the build from (`CAMOUFOX_INSTALL_DIR`).
    pub install: PathBuf,
    pub executable: PathBuf,
}

impl CamoufoxPin {
    pub fn folder(&self, state_dir: &Path) -> PathBuf {
        state_dir
            .join("plugins")
            .join(&self.engine)
            .join(&self.version)
    }

    /// Finds this machine's build in the state directory and checks every
    /// part against the pin. Run right before each launch.
    pub fn verified(&self, state_dir: &Path) -> Result<CamoufoxEngine, Failure> {
        let Some(build) = platform().and_then(|key| self.builds.get(key)) else {
            return Err(Failure::new(
                code::unsupported_platform,
                format!(
                    "no {} build for {}/{}",
                    self.engine,
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ),
            ));
        };
        let root = self.folder(state_dir);
        let engine = CamoufoxEngine {
            node: root.join("node/bin/node"),
            runtime: root.join("runtime"),
            install: root.join("camoufox"),
            executable: root.join("camoufox").join(&build.executable),
        };
        if !root.join("manifest.json").is_file() {
            return Err(not_installed(&self.engine, &self.version, &root));
        }
        let parts = [
            (root.join("camoufox"), build.camoufox.tree_sha256.as_str()),
            (root.join("node"), build.node.tree_sha256.as_str()),
            (root.join("runtime"), build.runtime_tree_sha256.as_str()),
        ];
        checked(&root.join(RECEIPT), &parts).map_err(|detail| {
            Failure::new(
                code::engine_unverified,
                format!("{}: {detail}", self.engine),
            )
        })?;
        Ok(engine)
    }
}

/// Launch arguments that would undo what latch relies on: Chrome's own
/// sandbox (unless the host cannot run it, `sandbox::page_isolation_off`,
/// where latch adds `--no-sandbox` itself), the private pipe as the only
/// control channel, a headed window unless the host asks for none (`serve
/// --headless-browsers`, which latch adds itself), and no password store. A
/// recipe carrying one is never used.
const FORBIDDEN: &[&str] = &[
    "--no-sandbox",
    "--disable-gpu-sandbox",
    "--no-zygote",
    "--single-process",
    "--remote-debugging-port",
    "--remote-debugging-address",
    "--headless",
    "--password-store",
];

/// The build key for this machine, as `builds` names them.
pub fn platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("mac_arm64"),
        ("macos", "x86_64") => Some("mac_x64"),
        ("linux", "aarch64") => Some("linux_arm64"),
        ("linux", "x86_64") => Some("linux_x64"),
        ("windows", "x86_64") => Some("win_x64"),
        _ => None,
    }
}

/// An installed, checked engine build, ready to launch.
pub struct Engine {
    pub executable: PathBuf,
    pub recipe: Vec<String>,
}

impl Pin {
    /// The version folder install.sh fills: `<state-dir>/plugins/<engine>/<version>`.
    pub fn folder(&self, state_dir: &Path) -> PathBuf {
        state_dir
            .join("plugins")
            .join(&self.engine)
            .join(&self.version)
    }

    /// Finds this machine's build in the state directory and checks it
    /// against the pin. Run right before each launch.
    pub fn verified(&self, state_dir: &Path) -> Result<Engine, Failure> {
        let Some(build) = platform().and_then(|key| self.builds.get(key)) else {
            return Err(Failure::new(
                code::unsupported_platform,
                format!(
                    "no {} build for {}/{}",
                    self.engine,
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ),
            ));
        };
        let recipe: Vec<String> = self.recipe.iter().chain(&build.args).cloned().collect();
        if let Some(arg) = recipe.iter().find(|arg| forbidden(arg)) {
            return Err(Failure::new(
                code::engine_unverified,
                format!("the {} launch recipe has {arg}", self.engine),
            ));
        }
        let root = self.folder(state_dir).join(&build.folder);
        let executable = root.join(&build.executable);
        if !executable.is_file() {
            return Err(not_installed(&self.engine, &self.version, &executable));
        }
        let receipt = self.folder(state_dir).join(RECEIPT);
        checked(&receipt, &[(root, build.tree_sha256.as_str())]).map_err(|detail| {
            Failure::new(
                code::engine_unverified,
                format!("{}: {detail}", self.engine),
            )
        })?;
        Ok(Engine { executable, recipe })
    }
}

/// Checks the installed build of `engine` (`cft` or `camoufox`) against its
/// pin, as a launch does. One not installed is refused naming the engines
/// the host has (`with_installed`).
pub fn verify(state_dir: &Path, engine: &str) -> Result<(), Failure> {
    let verified = match engine {
        "camoufox" => CAMOUFOX.verified(state_dir).map(drop),
        _ => CFT.verified(state_dir).map(drop),
    };
    verified.map_err(|failure| with_installed(state_dir, failure))
}

/// The engines whose builds check out here, as the commands name them, the
/// default (`default`'s choice) first.
fn installed(state_dir: &Path) -> Vec<&'static str> {
    let mut installed = Vec::new();
    if CFT.verified(state_dir).is_ok() {
        installed.push("chrome");
    }
    if CAMOUFOX.verified(state_dir).is_ok() {
        installed.push("camoufox");
    }
    installed
}

/// An `engine_not_installed` refusal, saying which engines the host has
/// and which of them an `open` that names none runs on (`details.installed`
/// and `details.default`, none when it has none). Any other failure as it is.
fn with_installed(state_dir: &Path, mut failure: Failure) -> Failure {
    if failure.code != code::engine_not_installed.name {
        return failure;
    }
    let installed = installed(state_dir);
    let default = installed.first().copied();
    // The engine asked for is missing, so the host has the other one at most.
    let has = match default {
        None => "this host has no browser engine installed".to_string(),
        Some(only) => format!("this host has {only}, its default"),
    };
    failure.message = format!("{}; {has}", failure.message);
    failure.details["installed"] = serde_json::json!(installed);
    failure.details["default"] = serde_json::json!(default);
    failure
}

/// The engine an `open` that names none runs on: Chrome when its build
/// checks out, else Camoufox when its does. With neither, the first one's
/// error that is not just "not installed" (a changed build, or no build for
/// this platform); with none installed, one naming the owner's setup.
pub fn default(state_dir: &Path) -> Result<&'static str, Failure> {
    let chrome = match CFT.verified(state_dir) {
        Ok(_) => return Ok("cft"),
        Err(failure) => failure,
    };
    let fox = match CAMOUFOX.verified(state_dir) {
        Ok(_) => return Ok("camoufox"),
        Err(failure) => failure,
    };
    for failure in [chrome, fox] {
        if failure.code != code::engine_not_installed.name {
            return Err(failure);
        }
    }
    let setup = "latch browser setup --engine chrome";
    let mut failure = Failure::new(
        code::engine_not_installed,
        format!(
            "no browser engine is installed; the host's owner installs one with `{setup}` \
             (or `--engine camoufox`) on the host"
        ),
    );
    failure.details = serde_json::json!({"setup": setup, "installed": [], "default": null});
    Err(failure)
}

/// Engine `engine` (`cft` or `camoufox`) as the commands name it.
pub fn named(engine: &str) -> &str {
    if engine == "cft" { "chrome" } else { engine }
}

/// The engine the commands name `name`, if there is one.
pub fn unnamed(name: &str) -> Option<&'static str> {
    match name {
        "chrome" => Some("cft"),
        "camoufox" => Some("camoufox"),
        _ => None,
    }
}

/// No engine where `at` should be: the owner installs it on the host with
/// `latch browser setup`, which the error names, in its details too.
fn not_installed(engine: &str, version: &str, at: &Path) -> Failure {
    let named = named(engine);
    let setup = format!("latch browser setup --engine {named}");
    let mut failure = Failure::new(
        code::engine_not_installed,
        format!(
            "{engine} {version} is not installed at {}; the host's owner installs it with \
             `{setup}` on the host",
            at.display()
        ),
    );
    failure.details = serde_json::json!({"engine": named, "setup": setup});
    failure
}

fn forbidden(arg: &str) -> bool {
    let name = arg.split('=').next().unwrap_or(arg);
    FORBIDDEN.contains(&name)
}

/// sha256 over one line per entry below `root`, sorted by relative path
/// bytes: `d PATH`, `l PATH\0TARGET` or `f PATH\0x|-\0SHA256` (`x` when any
/// execute bit is set), each ending in a newline. `scripts/cft-pin.py`
/// computes the same value for the pin.
pub fn tree_hash(root: &Path) -> io::Result<String> {
    let mut entries = Vec::new();
    walk(root, root, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut digest = Sha256::new();
    for (rel, path) in entries {
        let kind = fs::symlink_metadata(&path)?;
        let file_type = kind.file_type();
        if file_type.is_symlink() {
            digest.update(b"l ");
            digest.update(&rel);
            digest.update(b"\0");
            digest.update(fs::read_link(&path)?.as_os_str().as_encoded_bytes());
        } else if file_type.is_dir() {
            digest.update(b"d ");
            digest.update(&rel);
        } else if file_type.is_file() {
            let mode: &[u8] = if runs(&kind) { b"x" } else { b"-" };
            let file_sha256 = crate::google::process::sha256(&mut File::open(&path)?)?;
            digest.update(b"f ");
            digest.update(&rel);
            digest.update(b"\0");
            digest.update(mode);
            digest.update(b"\0");
            digest.update(file_sha256.as_bytes());
        } else {
            return Err(io::Error::other(format!(
                "{}: not a file, folder or link",
                path.display()
            )));
        }
        digest.update(b"\n");
    }
    Ok(to_hex(&digest.finalize()))
}

/// The receipt's file, beside the folders it vouches for.
const RECEIPT: &str = "receipt.json";

/// A full hash that matched: the tree hashes, and a hash of the metadata of
/// every entry in those folders then (`metadata_hash`).
#[derive(Serialize, Deserialize, PartialEq)]
struct Receipt {
    trees: Vec<String>,
    metadata: String,
}

/// Checks that each folder in `parts` has its pinned tree hash. Quick when
/// the receipt at `receipt` is for these pins and nothing in the folders was
/// touched since it was written; otherwise each folder is hashed whole, and a
/// match writes the receipt anew where latch can write (a sandboxed worker
/// can't, the host can). The error says which folder differs, and how.
fn checked(receipt: &Path, parts: &[(PathBuf, &str)]) -> Result<(), String> {
    let trees: Vec<String> = parts.iter().map(|(_, pinned)| pinned.to_string()).collect();
    let folders: Vec<&Path> = parts.iter().map(|(folder, _)| folder.as_path()).collect();
    let kept = fs::read(receipt)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Receipt>(&bytes).ok());
    // The metadata before the full hash: a receipt is written only if it
    // is the same after, so nothing written meanwhile is vouched for.
    let before = metadata_hash(&folders).ok();
    if let Some(kept) = &kept
        && before.as_ref() == Some(&kept.metadata)
        && kept.trees == trees
    {
        return Ok(());
    }
    for (folder, pinned) in parts {
        let hash = tree_hash(folder).map_err(|err| format!("{}: {err}", folder.display()))?;
        if hash != *pinned {
            return Err(format!(
                "{}: tree sha256 {hash} is not the pinned {pinned}",
                folder.display()
            ));
        }
    }
    if let Some(metadata) = before
        && metadata_hash(&folders).is_ok_and(|after| after == metadata)
    {
        let receipt_now = Receipt { trees, metadata };
        let _ =
            crate::state::write_private(receipt, &serde_json::to_vec(&receipt_now).expect("JSON"));
    }
    Ok(())
}

/// A hash of what the file system says of every entry in `folders`, without
/// reading a file: its path, type, size, mode, inode, and when its content
/// and its metadata last changed. Any write, replacement, rename or chmod
/// changes it.
fn metadata_hash(folders: &[&Path]) -> io::Result<String> {
    let mut digest = Sha256::new();
    for folder in folders {
        let mut entries = Vec::new();
        walk(folder, folder, &mut entries)?;
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        // Each folder's entries by their paths inside it: the same build
        // reached by another path (a link to it) reads alike.
        digest.update(b"\n");
        for (rel, path) in entries {
            let meta = fs::symlink_metadata(&path)?;
            digest.update(&rel);
            digest.update(said_of(&meta).as_bytes());
        }
    }
    Ok(to_hex(&digest.finalize()))
}

/// What the file system says of one entry for `metadata_hash`. On Windows,
/// which has no mode or inode, its attributes and creation time stand in.
fn said_of(meta: &fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        format!(
            "\0{:o} {} {} {}.{} {}.{}\n",
            meta.mode(),
            meta.len(),
            meta.ino(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec()
        )
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        format!(
            "\0{:x} {} {} {}\n",
            meta.file_attributes(),
            meta.len(),
            meta.creation_time(),
            meta.last_write_time()
        )
    }
}

/// Whether a file's mode lets it run. Windows has no such bit.
fn runs(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    return std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0;
    #[cfg(windows)]
    return {
        let _ = meta;
        false
    };
}

fn walk(root: &Path, dir: &Path, entries: &mut Vec<(Vec<u8>, PathBuf)>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let rel = path
            .strip_prefix(root)
            .expect("below root")
            .as_os_str()
            .as_encoded_bytes()
            .to_vec();
        // Windows' separator as `/`, as the pin script's hash has it.
        #[cfg(windows)]
        let rel: Vec<u8> = rel
            .into_iter()
            .map(|byte| if byte == b'\\' { b'/' } else { byte })
            .collect();
        let is_dir = fs::symlink_metadata(&path)?.is_dir();
        entries.push((rel, path.clone()));
        if is_dir {
            walk(root, &path, entries)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::failure::Exit;

    #[test]
    fn the_pinned_recipe_keeps_the_sandbox_the_pipe_and_a_window() {
        assert!(CFT.recipe.contains(&"--remote-debugging-pipe".to_string()));
        assert!(!CFT.recipe.iter().any(|arg| forbidden(arg)));
        assert!(forbidden("--remote-debugging-port=9222"));
        assert!(forbidden("--headless=new"));
        assert!(forbidden("--no-sandbox"));
        for key in [
            "mac_arm64",
            "mac_x64",
            "linux_arm64",
            "linux_x64",
            "win_x64",
        ] {
            assert!(CFT.builds.contains_key(key), "{key}");
            assert!(!CFT.builds[key].args.iter().any(|arg| forbidden(arg)));
        }
        // Linux arm64's build crashes with it; only the macOS builds get it.
        let crashpad = "--disable-crashpad-for-testing".to_string();
        assert!(!CFT.recipe.contains(&crashpad));
        assert!(CFT.builds["mac_arm64"].args.contains(&crashpad));
        assert!(CFT.builds["linux_arm64"].args.is_empty());
    }

    /// The same tree through `scripts/cft-pin.py`'s `tree_hash` gives this
    /// value, so the pin the script prints is the one latch checks.
    #[test]
    fn tree_hash_matches_the_pin_script() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("App/Contents/MacOS")).unwrap();
        fs::create_dir(root.join("b")).unwrap();
        fs::write(root.join("App/Contents/MacOS/run"), b"#!/bin/sh\n").unwrap();
        fs::set_permissions(
            root.join("App/Contents/MacOS/run"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::write(root.join("ABOUT"), b"about\n").unwrap();
        fs::write(root.join("b/data"), b"").unwrap();
        for plain in ["ABOUT", "b/data"] {
            fs::set_permissions(root.join(plain), fs::Permissions::from_mode(0o644)).unwrap();
        }
        std::os::unix::fs::symlink("App/Contents", root.join("Current")).unwrap();
        assert_eq!(
            tree_hash(root).unwrap(),
            "4d8ba5b9571459b48debc6c3d71ee63040ba0606a26e97a4f2d3f4129df6e880",
            "python3 scripts/cft-pin.py's tree_hash on the same tree"
        );
        // Any change to a file's bytes or execute bit changes the hash.
        let before = tree_hash(root).unwrap();
        fs::set_permissions(root.join("ABOUT"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_ne!(tree_hash(root).unwrap(), before);
    }

    #[test]
    fn a_receipt_spares_the_full_hash_until_anything_in_the_build_changes() {
        let dir = tempfile::tempdir().unwrap();
        let build = dir.path().join("build");
        fs::create_dir_all(build.join("lib")).unwrap();
        fs::write(build.join("run"), b"engine").unwrap();
        fs::write(build.join("lib/one"), b"library").unwrap();
        let pinned = tree_hash(&build).unwrap();
        let receipt = dir.path().join(RECEIPT);

        // A build that differs from its pin is refused, and leaves no receipt.
        let other = "0".repeat(64);
        let refused = checked(&receipt, &[(build.clone(), &other)]).unwrap_err();
        assert!(refused.contains("is not the pinned"), "{refused}");
        assert!(!receipt.exists());
        // One that matches leaves one.
        checked(&receipt, &[(build.clone(), &pinned)]).unwrap();
        assert!(receipt.exists());

        // With the receipt standing, nothing is read: a receipt for another
        // pin, over this very metadata, is taken at its word.
        let metadata = metadata_hash(&[build.as_path()]).unwrap();
        let vouched = Receipt {
            trees: vec![other.clone()],
            metadata,
        };
        fs::write(&receipt, serde_json::to_vec(&vouched).unwrap()).unwrap();
        checked(&receipt, &[(build.clone(), &other)]).unwrap();
        // Once anything in the build is touched, it is hashed whole again.
        fs::write(build.join("lib/one"), b"librarx").unwrap();
        let changed = checked(&receipt, &[(build.clone(), &other)]).unwrap_err();
        assert!(changed.contains("is not the pinned"), "{changed}");
        // And a receipt for other pins is never taken for this one's.
        fs::write(build.join("lib/one"), b"library").unwrap();
        checked(&receipt, &[(build.clone(), &pinned)]).unwrap();
        let kept: Receipt = serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
        assert_eq!(kept.trees, [pinned]);
    }

    /// Stands in an installed `engine` (`cft` or `camoufox`) in `state`: stub
    /// folders, and a receipt for the pinned hashes over them, which a check
    /// takes at its word until anything in them changes.
    fn fake_install(state: &Path, engine: &str) {
        let key = platform().unwrap();
        let (folder, parts) = if engine == "cft" {
            let build = &CFT.builds[key];
            let root = CFT.folder(state).join(&build.folder);
            let exe = root.join(&build.executable);
            fs::create_dir_all(exe.parent().unwrap()).unwrap();
            fs::write(&exe, b"chrome").unwrap();
            (CFT.folder(state), vec![(root, &build.tree_sha256)])
        } else {
            let build = &CAMOUFOX.builds[key];
            let root = CAMOUFOX.folder(state);
            for part in ["camoufox", "node", "runtime"] {
                fs::create_dir_all(root.join(part)).unwrap();
            }
            fs::write(root.join("manifest.json"), b"{}").unwrap();
            let parts = vec![
                (root.join("camoufox"), &build.camoufox.tree_sha256),
                (root.join("node"), &build.node.tree_sha256),
                (root.join("runtime"), &build.runtime_tree_sha256),
            ];
            (root, parts)
        };
        let folders: Vec<&Path> = parts.iter().map(|(folder, _)| folder.as_path()).collect();
        let receipt = Receipt {
            trees: parts.iter().map(|(_, tree)| tree.to_string()).collect(),
            metadata: metadata_hash(&folders).unwrap(),
        };
        fs::write(folder.join(RECEIPT), serde_json::to_vec(&receipt).unwrap()).unwrap();
    }

    #[test]
    fn with_no_engine_named_chrome_runs_if_it_checks_out_else_camoufox() {
        let none = tempfile::tempdir().unwrap();
        let missing = default(none.path()).unwrap_err();
        assert_eq!(missing.code, "engine_not_installed");
        assert_eq!(
            (&missing.details["installed"], &missing.details["default"]),
            (&serde_json::json!([]), &serde_json::Value::Null)
        );
        assert_eq!(
            missing.details["setup"],
            "latch browser setup --engine chrome"
        );
        assert!(
            missing.message.contains("--engine camoufox"),
            "{}",
            missing.message
        );

        // Camoufox alone, as on an agent's machine; then Chrome beside it.
        let state = tempfile::tempdir().unwrap();
        fake_install(state.path(), "camoufox");
        assert_eq!(default(state.path()).unwrap(), "camoufox");
        fake_install(state.path(), "cft");
        assert_eq!(default(state.path()).unwrap(), "cft");
        // A Chrome that no longer checks out gives way to Camoufox.
        let build = &CFT.builds[platform().unwrap()];
        let exe = CFT
            .folder(state.path())
            .join(&build.folder)
            .join(&build.executable);
        fs::write(&exe, b"not chrome").unwrap();
        assert_eq!(default(state.path()).unwrap(), "camoufox");

        // With nothing to give way to, its own error says why.
        let alone = tempfile::tempdir().unwrap();
        fake_install(alone.path(), "cft");
        assert_eq!(default(alone.path()).unwrap(), "cft");
        fs::write(
            CFT.folder(alone.path())
                .join(&build.folder)
                .join(&build.executable),
            b"x",
        )
        .unwrap();
        assert_eq!(default(alone.path()).unwrap_err().code, "engine_unverified");
    }

    #[test]
    fn a_missing_engine_names_the_ones_the_host_has_its_default_and_its_setup() {
        let state = tempfile::tempdir().unwrap();
        let none = verify(state.path(), "camoufox").unwrap_err();
        assert_eq!(none.code, "engine_not_installed");
        assert_eq!(
            none.details,
            serde_json::json!({"engine": "camoufox", "setup": "latch browser setup --engine camoufox",
                   "installed": [], "default": null})
        );
        assert!(
            none.message
                .contains("this host has no browser engine installed"),
            "{}",
            none.message
        );

        fake_install(state.path(), "cft");
        let fox = verify(state.path(), "camoufox").unwrap_err();
        assert_eq!(
            fox.details,
            serde_json::json!({"engine": "camoufox", "setup": "latch browser setup --engine camoufox",
                   "installed": ["chrome"], "default": "chrome"})
        );
        assert!(
            fox.message
                .contains("`latch browser setup --engine camoufox`")
                && fox.message.contains("this host has chrome, its default"),
            "{}",
            fox.message
        );

        let other = tempfile::tempdir().unwrap();
        fake_install(other.path(), "camoufox");
        let chrome = verify(other.path(), "cft").unwrap_err();
        assert_eq!(chrome.details["installed"], serde_json::json!(["camoufox"]));
        assert_eq!(chrome.details["default"], "camoufox");
        assert_eq!(
            chrome.details["setup"],
            "latch browser setup --engine chrome"
        );
    }

    #[test]
    fn a_changed_camoufox_part_is_refused_before_launch() {
        let state = tempfile::tempdir().unwrap();
        let missing = CAMOUFOX.verified(state.path()).err().unwrap();
        assert_eq!(missing.code, "engine_not_installed");
        assert_eq!(
            missing.details["setup"],
            "latch browser setup --engine camoufox"
        );
        let root = CAMOUFOX.folder(state.path());
        for part in ["camoufox", "node", "runtime"] {
            fs::create_dir_all(root.join(part)).unwrap();
        }
        fs::write(root.join("manifest.json"), b"{}").unwrap();
        let changed = CAMOUFOX.verified(state.path()).err().unwrap();
        assert_eq!(
            (changed.exit, changed.code.as_str()),
            (Exit::Denied, "engine_unverified")
        );
        assert!(changed.message.contains("camoufox"), "{}", changed.message);
        // The fingerprint choice is independent of the host OS.
        assert_eq!(CAMOUFOX.fingerprint_os, "macos");
    }

    #[test]
    fn a_changed_build_is_refused_before_launch() {
        let state = tempfile::tempdir().unwrap();
        let Some(build) = platform().and_then(|key| CFT.builds.get(key)) else {
            let none = CFT.verified(state.path()).err().unwrap();
            assert_eq!(none.code, "unsupported_platform");
            return;
        };
        let root = CFT.folder(state.path()).join(&build.folder);
        let missing = CFT.verified(state.path()).err().unwrap();
        assert_eq!(missing.code, "engine_not_installed");
        assert_eq!(
            missing.details["setup"],
            "latch browser setup --engine chrome"
        );
        let exe = root.join(&build.executable);
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"not chrome").unwrap();
        let changed = CFT.verified(state.path()).err().unwrap();
        assert_eq!(
            (changed.exit, changed.code.as_str()),
            (Exit::Denied, "engine_unverified")
        );
    }
}
