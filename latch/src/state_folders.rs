//! Other latch state folders inside a host's shares and `--allow-read`
//! paths (another peer's, a test's). Each holds a key and a Plow login, so
//! no worker may read one, including one made after the host started.
//!
//! The host searches the folders once when it starts, noting each
//! directory's modification time. Before every worker starts, it looks at
//! those times again: a directory gains a new time when an entry in it is
//! added, removed or renamed, so only the directories that changed are read
//! again, and a new folder below one is searched in full. Links are not
//! followed, and a state folder is not searched further.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::log::log;
use crate::state;

pub struct StateFolders {
    /// The host's own state folder, which workers are denied anyway.
    own: PathBuf,
    seen: Mutex<Seen>,
}

#[derive(Default)]
struct Seen {
    /// Every directory searched, with its modification time then.
    dirs: HashMap<PathBuf, SystemTime>,
    found: BTreeSet<PathBuf>,
}

impl StateFolders {
    /// Searches `roots` in full, now.
    pub fn new(roots: &[PathBuf], own: &Path) -> StateFolders {
        let folders = StateFolders {
            own: own.to_path_buf(),
            seen: Mutex::new(Seen::default()),
        };
        {
            let mut seen = folders.seen.lock().expect("state folders lock");
            for root in roots {
                folders.search(&mut seen, root.clone());
            }
        }
        folders
    }

    /// None, for a worker that sees no shares.
    pub fn none() -> StateFolders {
        StateFolders::new(&[], Path::new("/"))
    }

    /// The state folders there are now: the ones found before, less those
    /// gone, plus any in the directories that changed since.
    pub fn now(&self) -> Vec<PathBuf> {
        let mut seen = self.seen.lock().expect("state folders lock");
        let mut changed = Vec::new();
        for (dir, time) in seen.dirs.iter_mut() {
            match std::fs::symlink_metadata(dir).and_then(|meta| meta.modified()) {
                Ok(now) if now == *time => {}
                Ok(now) => {
                    *time = now;
                    changed.push(dir.clone());
                }
                Err(_) => changed.push(dir.clone()),
            }
        }
        for dir in changed {
            seen.dirs.remove(&dir);
            seen.found.remove(&dir);
            self.search(&mut seen, dir);
        }
        seen.found.retain(|dir| state::is_state_folder(dir));
        seen.found.iter().cloned().collect()
    }

    /// Searches `dir` and every directory below it not searched yet.
    fn search(&self, seen: &mut Seen, dir: PathBuf) {
        let mut pending = vec![dir];
        while let Some(dir) = pending.pop() {
            let Ok(time) = std::fs::symlink_metadata(&dir).and_then(|meta| meta.modified()) else {
                continue;
            };
            seen.dirs.insert(dir.clone(), time);
            if state::is_state_folder(&dir) {
                if dir != self.own && seen.found.insert(dir.clone()) {
                    log(&format!("hidden latch state folder: {}", dir.display()));
                }
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if entry.file_type().is_ok_and(|kind| kind.is_dir())
                    && !seen.dirs.contains_key(&path)
                {
                    pending.push(path);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const MARKER: &str = r#"{"format": 1, "written_by": "test"}"#;

    #[test]
    fn state_folders_are_found_at_start_and_when_made_later() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for path in [
            "share/a/peer/sub",
            "share/b",
            "share/c",
            "share/d/old",
            "own",
            "refs",
        ] {
            fs::create_dir_all(root.join(path)).unwrap();
        }
        fs::write(root.join("share/a/peer/latch.json"), MARKER).unwrap();
        fs::write(root.join("share/a/peer/sub/latch.json"), MARKER).unwrap();
        // Made before the marker: a key and a Plow login, no latch.json.
        fs::write(root.join("share/d/old/key"), "k").unwrap();
        fs::write(root.join("share/d/old/plow-token"), "t").unwrap();
        // A project's own latch.json is not latch's marker; a key alone is anyone's.
        fs::write(root.join("share/b/latch.json"), r#"{"name": "x"}"#).unwrap();
        fs::write(root.join("share/b/key"), "k").unwrap();
        std::os::unix::fs::symlink(root.join("share/a"), root.join("share/c/link")).unwrap();
        fs::write(root.join("own/latch.json"), MARKER).unwrap();

        let roots = [root.join("share"), root.join("refs"), root.join("own")];
        let folders = StateFolders::new(&roots, &root.join("own"));
        assert_eq!(
            folders.now(),
            [root.join("share/a/peer"), root.join("share/d/old")]
        );

        // After the start: a new folder deep in a new tree, a login added to
        // a folder that was there, and a found one removed.
        fs::create_dir_all(root.join("share/new/deeper/later")).unwrap();
        fs::write(root.join("share/new/deeper/later/latch.json"), MARKER).unwrap();
        fs::write(root.join("share/b/plow-token"), "t").unwrap();
        fs::remove_dir_all(root.join("share/d/old")).unwrap();
        assert_eq!(
            folders.now(),
            [
                root.join("share/b"),
                root.join("share/a/peer"),
                root.join("share/new/deeper/later")
            ]
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
        );
    }
}
