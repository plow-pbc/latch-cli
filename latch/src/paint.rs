//! Colors for the few things latch shows a person at a terminal (the logo,
//! the `serve` dashboard). Agents read latch's JSON
//! through a pipe and never see any of this.
//!
//! Color is off unless `set_color(true)` was called, which latch does only
//! when stdout is a terminal and `NO_COLOR` is not set (see `term.rs`).

use std::sync::atomic::{AtomicBool, Ordering};

/// A 24-bit terminal color.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Plow's brand color ("volt"), as in the old Latch app.
pub const VOLT: Rgb = Rgb(213, 239, 138);
/// The glint that sweeps across the logo.
pub const SHINE: Rgb = Rgb(255, 255, 255);
pub const DIM: Rgb = Rgb(120, 126, 138);
pub const FAINT: Rgb = Rgb(64, 70, 80);
pub const RED: Rgb = Rgb(238, 96, 96);

static COLOR: AtomicBool = AtomicBool::new(false);

pub fn set_color(on: bool) {
    COLOR.store(on, Ordering::Relaxed);
}

/// `text` in `color`, or plain `text` when color is off.
pub fn fg(text: &str, color: Rgb) -> String {
    styled(text, Some(color), None, false)
}

/// `text` in bold `color`, or plain `text` when color is off.
pub fn bold(text: &str, color: Rgb) -> String {
    styled(text, Some(color), None, true)
}

/// `text` with a foreground and a background color (the logo uses this to
/// put two colored pixels in one character cell).
pub fn fg_bg(text: &str, color: Rgb, background: Rgb) -> String {
    styled(text, Some(color), Some(background), false)
}

/// The ANSI escape codes around `text`: `ESC[...m` switches the style on,
/// `ESC[0m` back off.
fn styled(text: &str, color: Option<Rgb>, background: Option<Rgb>, bold: bool) -> String {
    if !COLOR.load(Ordering::Relaxed) {
        return text.to_string();
    }
    let mut code = String::from("\x1b[0");
    if bold {
        code.push_str(";1");
    }
    if let Some(Rgb(r, g, b)) = color {
        code.push_str(&format!(";38;2;{r};{g};{b}"));
    }
    if let Some(Rgb(r, g, b)) = background {
        code.push_str(&format!(";48;2;{r};{g};{b}"));
    }
    format!("{code}m{text}\x1b[0m")
}

/// The color `t` of the way from `a` to `b` (`t` from 0.0 to 1.0).
pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let channel = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Rgb(channel(a.0, b.0), channel(a.1, b.1), channel(a.2, b.2))
}
