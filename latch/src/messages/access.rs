//! Whether this host can read the owner's Messages or address book: Full
//! Disk Access as the host process sees it, checked from a worker it
//! starts (which macOS treats as the host's), never from the terminal
//! that asked. The worker opens the connector's stores read-only and reads
//! nothing from them.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::plugins::Connector;
use crate::worker::WorkerSetup;

/// System Settings' Full Disk Access pane.
pub const SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Granted,
    /// macOS refused: the host has no Full Disk Access.
    NotGranted,
    /// No such folder, or any other failure than a refusal.
    Unavailable,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Access {
    pub status: Status,
    pub reason: String,
}

/// The folder `connector` reads under `home`.
fn folder(connector: Connector, home: &Path) -> PathBuf {
    match connector {
        Connector::Messages => home.join("Library/Messages"),
        Connector::Contacts => home.join("Library/Application Support/AddressBook"),
        Connector::Google => unreachable!("Google needs no Full Disk Access"),
    }
}

/// Asks a worker whether it can open `connector`'s stores under `home`.
pub async fn check(workers: &WorkerSetup, connector: Connector, home: &Path) -> Access {
    let folder = folder(connector, home);
    // The worker's sandbox is built around the folder, so it must exist.
    if !folder.exists() {
        return Access {
            status: Status::Unavailable,
            reason: format!("{} does not exist", folder.display()),
        };
    }
    workers
        .access(connector, &folder)
        .await
        .unwrap_or_else(|err| Access {
            status: Status::Unavailable,
            reason: format!("the access check could not run: {}", err.message),
        })
}

/// The worker's side, once confined: finds `connector`'s stores in
/// `folder` and opens each read-only, reading nothing from them. Every one
/// must open, as the reader opens them all.
pub fn probe(connector: Connector, folder: &Path) -> Access {
    let stores = match stores(connector, folder) {
        Ok(stores) => stores,
        Err((path, err)) => return refused(&path, &err),
    };
    if stores.is_empty() {
        return Access {
            status: Status::Unavailable,
            reason: format!(
                "there is no {} store in {}",
                connector.name(),
                folder.display()
            ),
        };
    }
    for store in &stores {
        if let Err(err) = std::fs::File::open(store) {
            return refused(store, &err);
        }
    }
    Access {
        status: Status::Granted,
        reason: format!(
            "{} store(s) in {} can be opened",
            stores.len(),
            folder.display()
        ),
    }
}

/// Where the reader looks: Messages' `chat.db`; the address book's
/// `AddressBook-v22.abcddb`, at its top and in each of its `Sources`.
fn stores(connector: Connector, folder: &Path) -> Result<Vec<PathBuf>, (PathBuf, io::Error)> {
    const ADDRESS_BOOK: &str = "AddressBook-v22.abcddb";
    let list = |dir: &Path| -> Result<Vec<PathBuf>, (PathBuf, io::Error)> {
        let entries = std::fs::read_dir(dir).map_err(|err| (dir.to_path_buf(), err))?;
        entries
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|err| (dir.to_path_buf(), err))
            })
            .collect()
    };
    let candidates = match connector {
        Connector::Messages => {
            list(folder)?;
            vec![folder.join("chat.db")]
        }
        Connector::Contacts => {
            let mut found = vec![folder.join(ADDRESS_BOOK)];
            let sources = folder.join("Sources");
            if list(folder)?.contains(&sources) {
                // Only its folders hold stores, as the reader looks.
                let folders = list(&sources)?.into_iter().filter(|source| source.is_dir());
                found.extend(folders.map(|source| source.join(ADDRESS_BOOK)));
            }
            found
        }
        Connector::Google => unreachable!("Google needs no Full Disk Access"),
    };
    let mut stores = Vec::new();
    for candidate in candidates {
        match std::fs::metadata(&candidate) {
            Ok(_) => stores.push(candidate),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err((candidate, err)),
        }
    }
    Ok(stores)
}

/// What a failure to list or open `path` means: macOS refusing (EPERM, for
/// a host without Full Disk Access) or a permission denied is `not_granted`;
/// anything else, `unavailable`.
fn refused(path: &Path, err: &io::Error) -> Access {
    let status = if err.kind() == io::ErrorKind::PermissionDenied {
        Status::NotGranted
    } else {
        Status::Unavailable
    };
    Access {
        status,
        reason: format!("{}: {err}", path.display()),
    }
}
