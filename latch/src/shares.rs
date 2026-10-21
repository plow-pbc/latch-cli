//! `latch shares`: the folders `latch serve` shares when it is named none,
//! saved in the state folder so a service (the Mac app's, say) starts the
//! host the same way a person at a terminal set it up.

use std::path::PathBuf;

use serde::Serialize;

use crate::cli::SharesAction;
use crate::failure::Failure;
use crate::serve;
use crate::state::StateDir;

#[derive(Serialize)]
pub struct SharesOutput {
    pub shares: Vec<PathBuf>,
}

pub fn run(state: &StateDir, action: Option<SharesAction>) -> Result<SharesOutput, Failure> {
    let folders = match action {
        None => state.load_shares()?,
        Some(SharesAction::Set { folders }) => {
            // Checked and made absolute now, as `latch serve` would.
            let folders = serve::shared_folders(&folders)?;
            state.create()?;
            state.save_shares(&folders)?;
            folders
        }
    };
    Ok(SharesOutput { shares: folders })
}
