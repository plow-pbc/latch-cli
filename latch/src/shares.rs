//! `latch shares`: the folders `latch serve` shares when it is named none,
//! and the paths it lets callers read besides, saved in the state folder so
//! a service (the Mac app's, say) starts the host the same way a person at
//! a terminal set it up.

use std::path::PathBuf;

use serde::Serialize;

use crate::cli::SharesAction;
use crate::failure::Failure;
use crate::serve;
use crate::state::{Shares, StateDir};

#[derive(Serialize)]
pub struct SharesOutput {
    pub shares: Vec<PathBuf>,
    pub allow_read: Vec<PathBuf>,
}

pub fn run(state: &StateDir, action: Option<SharesAction>) -> Result<SharesOutput, Failure> {
    let saved = match action {
        None => state.load_shares()?,
        Some(SharesAction::Set {
            folders,
            allow_read,
        }) => {
            // Checked and made absolute now, as `latch serve` would.
            let shares = Shares {
                folders: serve::shared_folders(&folders, std::env::home_dir().as_deref())?,
                allow_read: serve::allowed_reads(
                    &allow_read,
                    std::env::home_dir().as_deref(),
                    &serve::hidden_paths(),
                    &state.path,
                )?,
            };
            state.create()?;
            state.save_shares(&shares)?;
            shares
        }
    };
    Ok(SharesOutput {
        shares: saved.folders,
        allow_read: saved.allow_read,
    })
}
