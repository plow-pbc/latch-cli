//! The view on a terminal: the second screen, keys read as they are
//! pressed, and the board redrawn as it changes and each second for the
//! clock. Ctrl-C stays the terminal's own (a signal), so the keys never
//! take it over.

use std::io::{Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiff::Timestamp;

use super::Board;
use super::view::{Asked, Key, View};
use crate::term;

/// How long a closing view waits to give the terminal back.
const GIVE_BACK_WITHIN: Duration = Duration::from_secs(1);

/// How a view ended.
#[derive(Debug, PartialEq)]
pub enum Ending {
    /// Ctrl-C (or the terminal going away), or an attached host that went
    /// away.
    Closed,
    /// `q`: the viewer left; the host runs on.
    Detached,
}

/// The screen `text`, drawn over the one before in place, as one update:
/// each line cleared as it is written and the rows below it cleared, never
/// the whole screen at once, so the terminal never shows it blank. A line
/// is cleared before it is written: after a line as wide as the screen,
/// the cursor sits on its last character.
pub(super) fn frame(text: &str, height: usize) -> String {
    let mut frame = format!("{}{}", term::SYNC_START, term::HOME);
    let lines: Vec<&str> = text.split('\n').collect();
    for (at, line) in lines.iter().enumerate() {
        if at > 0 {
            frame.push('\n');
        }
        frame.push_str(term::CLEAR_LINE_END);
        frame.push_str(line);
    }
    if lines.len() < height {
        frame.push('\n');
        frame.push_str(term::CLEAR_BELOW);
    }
    frame.push_str(term::SYNC_END);
    frame
}

/// Shows the boards `boards` yields until `until` (Ctrl-C) or `q`. An
/// `attached` view ends when the host does; a detached one says the host
/// went away and stays until it is closed. `notice` stands under the
/// header (a version mismatch).
pub async fn show(
    boards: tokio::sync::mpsc::Receiver<Board>,
    until: impl std::future::Future<Output = ()>,
    attached: bool,
    notice: Option<String>,
) -> Ending {
    // Key at a time until the view ends, then as the terminal was.
    let raw = Raw::start();
    let keys = read_keys(raw.is_some());
    let view = View::new(attached, notice);
    let ending = show_on(Box::new(std::io::stdout()), view, boards, keys, until).await;
    drop(raw);
    ending
}

/// `show` on `out` with keys from `keys`. Only a thread of its own writes
/// to `out`, so a terminal that takes no output (paused, or a stalled ssh
/// session) never holds up Ctrl-C or what follows it: the terminal is then
/// left as it is, after `GIVE_BACK_WITHIN`.
pub(super) async fn show_on(
    mut out: Box<dyn Write + Send>,
    view: View,
    mut boards: tokio::sync::mpsc::Receiver<Board>,
    mut keys: tokio::sync::mpsc::Receiver<Key>,
    until: impl std::future::Future<Output = ()>,
) -> Ending {
    let attached = view.attached;
    let view = Arc::new(Mutex::new(view));
    let (redraw, asked) = mpsc::sync_channel::<()>(1);
    let (given, back) = tokio::sync::oneshot::channel();
    let drawn = view.clone();
    std::thread::spawn(move || {
        let _ = write!(out, "{}{}", term::ALTERNATE_SCREEN, term::HIDE_CURSOR);
        // Each second, or sooner when asked, until the view is closed; a
        // frame like the last is not drawn again.
        let mut last = String::new();
        loop {
            let (width, height) = term::size();
            let text = drawn.lock().expect("view lock").screen(width, height);
            if text != last {
                let _ = write!(out, "{}", frame(&text, height));
                let _ = out.flush();
                last = text;
            }
            if let Err(RecvTimeoutError::Disconnected) = asked.recv_timeout(Duration::from_secs(1))
            {
                break;
            }
        }
        let _ = write!(out, "{}{}", term::SHOW_CURSOR, term::MAIN_SCREEN);
        let _ = out.flush();
        let _ = given.send(());
    });
    tokio::pin!(until);
    let mut live = true;
    let ending = loop {
        tokio::select! {
            next = boards.recv(), if live => match next {
                Some(next) => view.lock().expect("view lock").board = next,
                None if attached => break Ending::Closed,
                None => {
                    view.lock().expect("view lock").gone = Some(Timestamp::now());
                    live = false;
                }
            },
            Some(key) = keys.recv() => {
                if view.lock().expect("view lock").key(key) == Asked::Detach {
                    break Ending::Detached;
                }
            }
            () = &mut until => break Ending::Closed,
        }
        let _ = redraw.try_send(());
    };
    drop(redraw);
    let _ = tokio::time::timeout(GIVE_BACK_WITHIN, back).await;
    ending
}

/// The keys pressed on this terminal, as they are pressed, when it is
/// `raw` (see `Raw`); none otherwise.
fn read_keys(raw: bool) -> tokio::sync::mpsc::Receiver<Key> {
    let (send, keys) = tokio::sync::mpsc::channel(16);
    if !raw {
        return keys;
    }
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buffer = [0u8; 64];
        while let Ok(read) = stdin.read(&mut buffer) {
            if read == 0 {
                return;
            }
            for key in parse(&buffer[..read]) {
                if send.blocking_send(key).is_err() {
                    return;
                }
            }
        }
    });
    keys
}

/// The terminal in key-at-a-time mode, without echo, until dropped. Its
/// Ctrl-C stays a signal, and keys come as the escape sequences `parse` reads.
#[cfg(windows)]
struct Raw(#[allow(dead_code)] crate::term::Console);

#[cfg(windows)]
impl Raw {
    fn start() -> Option<Raw> {
        use windows_sys::Win32::System::Console::{
            ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_VIRTUAL_TERMINAL_INPUT,
        };
        crate::term::Console::set(crate::term::Console::stdin(), |mode| {
            mode & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT) | ENABLE_VIRTUAL_TERMINAL_INPUT
        })
        .map(Raw)
    }
}

/// The terminal in key-at-a-time mode, without echo, until dropped. Its
/// Ctrl-C stays a signal.
#[cfg(unix)]
struct Raw {
    saved: libc::termios,
}

#[cfg(unix)]
impl Raw {
    fn start() -> Option<Raw> {
        let fd = libc::STDIN_FILENO;
        // SAFETY: termios is a plain C struct that tcgetattr fills in.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: reading the terminal's settings into storage we own.
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            return None;
        }
        let mut keys = saved;
        keys.c_lflag &= !(libc::ICANON | libc::ECHO);
        keys.c_cc[libc::VMIN] = 1;
        keys.c_cc[libc::VTIME] = 0;
        // SAFETY: setting the terminal from a termios read just above.
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &keys) };
        Some(Raw { saved })
    }
}

#[cfg(unix)]
impl Drop for Raw {
    fn drop(&mut self) {
        // SAFETY: restoring what tcgetattr read.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.saved) };
    }
}

/// The keys in what one read of the terminal gave: arrows and End as
/// their escape sequences, a lone escape as Esc.
pub(super) fn parse(bytes: &[u8]) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let rest = &bytes[at..];
        let (key, used) = match rest {
            [0x1b, b'[' | b'O', b'A', ..] => (Some(Key::Up), 3),
            [0x1b, b'[' | b'O', b'B', ..] => (Some(Key::Down), 3),
            [0x1b, b'[' | b'O', b'C', ..] => (Some(Key::Right), 3),
            [0x1b, b'[' | b'O', b'D', ..] => (Some(Key::Left), 3),
            [0x1b, b'[' | b'O', b'F', ..] => (Some(Key::End), 3),
            [0x1b, b'[' | b'O', b'H', ..] => (Some(Key::Home), 3),
            [0x1b, b'[', b'1' | b'7', b'~', ..] => (Some(Key::Home), 4),
            [0x1b, b'[', b'4' | b'8', b'~', ..] => (Some(Key::End), 4),
            [0x1b, b'[', ..] => (None, 2),
            [0x1b, ..] => (Some(Key::Esc), 1),
            [b'\r' | b'\n', ..] => (Some(Key::Enter), 1),
            [b'\t', ..] => (Some(Key::Tab), 1),
            [0x7f | 0x08, ..] => (Some(Key::Backspace), 1),
            _ => {
                let text = String::from_utf8_lossy(rest);
                let ch = text.chars().next().unwrap_or(' ');
                let used = ch.len_utf8().min(rest.len());
                ((!ch.is_control()).then_some(Key::Char(ch)), used)
            }
        };
        keys.extend(key);
        at += used;
    }
    keys
}
