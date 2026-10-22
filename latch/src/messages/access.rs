//! Whether this host can read the owner's Messages or address book: Full
//! Disk Access as the host process sees it, checked from a worker it
//! starts (which macOS treats as the host's), never from the terminal
//! that asked. The worker lists the folder and reads nothing in it.

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
pub fn folder(connector: Connector, home: &Path) -> PathBuf {
    match connector {
        Connector::Messages => home.join("Library/Messages"),
        Connector::Contacts => home.join("Library/Application Support/AddressBook"),
        Connector::Google => unreachable!("Google needs no Full Disk Access"),
    }
}

/// Asks a worker whether it can list `folder`.
pub async fn check(workers: &WorkerSetup, folder: &Path) -> Access {
    workers.access(folder).await.unwrap_or_else(|err| Access {
        status: Status::Unavailable,
        reason: format!("the access check could not run: {}", err.message),
    })
}

/// The worker's side, once confined: lists `folder`.
pub fn probe(folder: &Path) -> Access {
    let (status, reason) = match std::fs::read_dir(folder) {
        Ok(_) => (Status::Granted, format!("{} can be read", folder.display())),
        // EPERM (privacy protection) and EACCES alike.
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
            (Status::NotGranted, format!("{}: {err}", folder.display()))
        }
        Err(err) => (Status::Unavailable, format!("{}: {err}", folder.display())),
    };
    Access { status, reason }
}
