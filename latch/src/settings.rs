//! `latch settings`: this machine's own choices for its host, kept in
//! `<state-dir>/settings.json`. A host reads them when it starts and
//! watches the file while it runs, so a change needs no restart and no
//! host at all; `serve -d --again` replays its flags, never these. Today
//! there is one: whether the host keeps this machine awake (see
//! `keep_awake.rs`).

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::failure::{Failure, code};
use crate::state::{StateDir, write_private};

pub const FILE: &str = "settings.json";

#[derive(Serialize, Deserialize, Default, Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    /// The host keeps this machine from idle sleep while it serves.
    #[serde(default)]
    pub keep_awake: bool,
}

/// The settings saved in `dir`; the defaults if there are none yet.
pub fn load(dir: &Path) -> Result<Settings, Failure> {
    let path = dir.join(FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|err| {
            Failure::new(
                code::bad_state,
                format!("{} is not valid: {err}", path.display()),
            )
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(err) => Err(Failure::new(
            code::bad_state,
            format!("{}: {err}", path.display()),
        )),
    }
}

/// `settings set`: saves the choice, before login too. That it is saved
/// is all this says; whether the host holds it is `status --local`'s.
pub fn set(state: &StateDir, keep_awake: bool) -> Result<Value, Failure> {
    state.create()?;
    let mut settings = load(&state.path)?;
    settings.keep_awake = keep_awake;
    let path = state.path.join(FILE);
    let text = serde_json::to_string_pretty(&settings).expect("settings serialize") + "\n";
    write_private(&path, text.as_bytes())
        .map_err(|err| Failure::new(code::state_io, format!("{}: {err}", path.display())))?;
    Ok(json!({"settings": settings, "next": "latch status --local"}))
}
