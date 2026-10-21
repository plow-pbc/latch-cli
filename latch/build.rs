//! Records where this build came from, for `latch --version`: the commit,
//! with `-dirty` when the crate had uncommitted changes, and when it was
//! built. A build with no git (a container given only the source) can pass
//! the commit in `LATCH_BUILD_COMMIT`; otherwise it says `unknown`.

use std::process::Command;

fn main() {
    let commit = std::env::var("LATCH_BUILD_COMMIT")
        .ok()
        .filter(|commit| !commit.is_empty())
        .or_else(git_commit)
        .unwrap_or_else(|| "unknown".to_string());
    let built = jiff::Timestamp::now().strftime("%Y-%m-%d %H:%M UTC");
    println!("cargo:rustc-env=LATCH_COMMIT={commit}");
    println!("cargo:rustc-env=LATCH_BUILT={built}");

    // Run again when the commit or the source changes, and only then.
    println!("cargo:rerun-if-env-changed=LATCH_BUILD_COMMIT");
    for path in ["build.rs", "Cargo.toml", "Cargo.lock", "src", "tests"] {
        println!("cargo:rerun-if-changed={path}");
    }
    let branch = git(&["rev-parse", "--symbolic-full-name", "HEAD"]);
    for name in ["HEAD", "index"].into_iter().chain(branch.as_deref()) {
        if let Some(path) = git(&["rev-parse", "--git-path", name]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}

/// HEAD's short hash, plus `-dirty` if a tracked file in this crate differs.
fn git_commit() -> Option<String> {
    let commit = git(&["rev-parse", "--short=9", "HEAD"])?;
    let changes = git(&["status", "--porcelain", "--untracked-files=no", "--", "."])?;
    Some(if changes.is_empty() {
        commit
    } else {
        format!("{commit}-dirty")
    })
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}
