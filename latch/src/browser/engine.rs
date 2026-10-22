//! The browser engines a host runs, as plugins: Chrome for Testing (CfT),
//! the release pinned in `cft.json`, which install.sh downloads into the
//! state directory's plugins folder, `<state-dir>/plugins/cft/<version>/`,
//! beside older versions, with that pin copied in as `manifest.json`; and
//! Camoufox, pinned in `camoufox.json` with the Node and Playwright runtime
//! its helper runs on, which install.sh adds only with `--browser camoufox`.
//! `latch browser setup` installs either later; an agent's call never
//! downloads one.
//!
//! The pin is compiled into latch and is what counts: before every launch
//! latch hashes the installed build's whole folder (`tree_hash`) and refuses
//! one that differs from the pin, so a changed library or helper is caught,
//! not only a changed main binary.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::failure::{Exit, Failure};
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
    /// Checked by browser-engines.sh, which downloads the zip.
    #[allow(dead_code)]
    pub url: String,
    #[allow(dead_code)]
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
#[derive(Deserialize)]
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
            return Err(Failure::failed(
                "unsupported_platform",
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
        for (part, pinned) in [
            ("camoufox", &build.camoufox.tree_sha256),
            ("node", &build.node.tree_sha256),
            ("runtime", &build.runtime_tree_sha256),
        ] {
            let folder = root.join(part);
            let hash = tree_hash(&folder).map_err(|err| {
                Failure::new(
                    Exit::Denied,
                    "engine_unverified",
                    format!("{} at {}: {err}", self.engine, folder.display()),
                )
            })?;
            if hash != *pinned {
                return Err(Failure::new(
                    Exit::Denied,
                    "engine_unverified",
                    format!(
                        "{} at {}: tree sha256 {hash} is not the pinned {pinned}",
                        self.engine,
                        folder.display()
                    ),
                ));
            }
        }
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
            return Err(Failure::failed(
                "unsupported_platform",
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
                Exit::Denied,
                "engine_unverified",
                format!("the {} launch recipe has {arg}", self.engine),
            ));
        }
        let root = self.folder(state_dir).join(&build.folder);
        let executable = root.join(&build.executable);
        if !executable.is_file() {
            return Err(not_installed(&self.engine, &self.version, &executable));
        }
        let unverified = |detail: String| {
            Failure::new(
                Exit::Denied,
                "engine_unverified",
                format!("{} at {}: {detail}", self.engine, root.display()),
            )
        };
        let hash = tree_hash(&root).map_err(|err| unverified(err.to_string()))?;
        if hash != build.tree_sha256 {
            return Err(unverified(format!(
                "tree sha256 {hash} is not the pinned {}",
                build.tree_sha256
            )));
        }
        Ok(Engine { executable, recipe })
    }
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
    let mut failure = Failure::failed(
        "engine_not_installed",
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
            digest.update(fs::read_link(&path)?.as_os_str().as_bytes());
        } else if file_type.is_dir() {
            digest.update(b"d ");
            digest.update(&rel);
        } else if file_type.is_file() {
            let mode: &[u8] = if kind.permissions().mode() & 0o111 != 0 {
                b"x"
            } else {
                b"-"
            };
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

fn walk(root: &Path, dir: &Path, entries: &mut Vec<(Vec<u8>, PathBuf)>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let rel = path
            .strip_prefix(root)
            .expect("below root")
            .as_os_str()
            .as_bytes()
            .to_vec();
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
    use super::*;

    #[test]
    fn the_pinned_recipe_keeps_the_sandbox_the_pipe_and_a_window() {
        assert!(CFT.recipe.contains(&"--remote-debugging-pipe".to_string()));
        assert!(!CFT.recipe.iter().any(|arg| forbidden(arg)));
        assert!(forbidden("--remote-debugging-port=9222"));
        assert!(forbidden("--headless=new"));
        assert!(forbidden("--no-sandbox"));
        for key in ["mac_arm64", "mac_x64", "linux_arm64"] {
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
