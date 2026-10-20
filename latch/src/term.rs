//! What latch needs to know about, and do to, a terminal: whether stdout is
//! one, whether a person is there to answer a question, whether they asked
//! for no color, its size, and the few escape codes the dashboard uses. Plain ANSI escape sequences, which
//! every terminal latch runs in understands.

use std::io::IsTerminal;

/// Switch to the terminal's second screen (what full-screen programs like
/// `less` or `top` draw on), and back, leaving the person's scrollback alone.
pub const ALTERNATE_SCREEN: &str = "\x1b[?1049h";
pub const MAIN_SCREEN: &str = "\x1b[?1049l";
pub const HIDE_CURSOR: &str = "\x1b[?25l";
pub const SHOW_CURSOR: &str = "\x1b[?25h";
/// Cursor to the top-left corner, then clear the whole screen.
pub const CLEAR: &str = "\x1b[H\x1b[2J";
/// Clear from the cursor to the end of the line.
pub const CLEAR_LINE_END: &str = "\x1b[K";

/// Showy output is only for a person at a terminal: an agent reads
/// latch's stdout through a pipe.
pub fn stdout_is_terminal() -> bool {
    std::io::stdout().is_terminal()
}

/// Whether a person can be asked something: stdin and stderr are both a
/// terminal. Questions go to stderr, so stdout stays the command's JSON.
pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Asks `question` on stderr and returns the line typed, trimmed.
/// (Empty if stdin closes.)
pub fn ask(question: &str) -> String {
    eprint!("{question}");
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    answer.trim().to_string()
}

/// Asks for a secret on stderr without echoing what is typed (when stdin is
/// a terminal; piped input is read as it comes). Never from argv or the
/// environment.
pub fn ask_secret(question: &str) -> String {
    eprint!("{question}");
    let fd = libc::STDIN_FILENO;
    // SAFETY: termios is a plain C struct that tcgetattr fills in.
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    let terminal =
        std::io::stdin().is_terminal() && unsafe { libc::tcgetattr(fd, &mut saved) } == 0;
    if terminal {
        let mut quiet = saved;
        quiet.c_lflag &= !libc::ECHO;
        // SAFETY: sets the attributes just read, with echo off.
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &quiet) };
    }
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    if terminal {
        // SAFETY: restores what tcgetattr read.
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &saved) };
        eprintln!();
    }
    answer.trim_end_matches(['\n', '\r']).to_string()
}

/// Asks a yes-or-no `question`; only an answer starting with y means yes.
pub fn confirm(question: &str) -> bool {
    ask(&format!("{question} [y/N] "))
        .to_lowercase()
        .starts_with('y')
}

/// <https://no-color.org>: `NO_COLOR` set and not empty means no color.
pub fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty())
}

/// Moves the cursor to the start of the line `lines` lines up.
pub fn up(lines: usize) -> String {
    format!("\x1b[{lines}F")
}

/// The terminal's size in columns and rows (80 × 24 if it cannot be read).
pub fn size() -> (usize, usize) {
    // `winsize` is the C struct the TIOCGWINSZ request fills in.
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ only writes into `size`, which outlives the call.
    let status = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) };
    if status != 0 || size.ws_col == 0 || size.ws_row == 0 {
        return (80, 24);
    }
    (size.ws_col as usize, size.ws_row as usize)
}

/// `text` on one line: a line break shows as `⏎`, and every other control
/// character (tabs, and the escape character that starts terminal
/// commands) as a space. Text from a caller, such as an `exec` argv, can
/// hold any of them; shown raw, they would break a row or drive the
/// terminal.
pub fn one_line(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '\n' | '\r' => '⏎',
            ch if ch.is_control() => ' ',
            ch => ch,
        })
        .collect()
}

/// `text` on one line (see `one_line`), cut or padded to exactly `width`
/// characters, ending in "…" if it was cut. (Every character latch shows
/// is one column wide.)
pub fn fit(text: &str, width: usize) -> String {
    let text = one_line(text);
    let count = text.chars().count();
    if count <= width {
        return format!("{text}{}", " ".repeat(width - count));
    }
    if width == 0 {
        return String::new();
    }
    let mut cut: String = text.chars().take(width - 1).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_never_reach_the_screen() {
        assert_eq!(one_line("a\nb\r\nc"), "a⏎b⏎⏎c");
        assert_eq!(one_line("tab\there \x1b[2J"), "tab here  [2J");
        assert_eq!(fit("echo a\necho b", 8), "echo a⏎…");
    }

    #[test]
    fn fit_pads_or_cuts_to_the_width() {
        assert_eq!(fit("exec", 6), "exec  ");
        assert_eq!(fit("sh -c 'ls | wc -l'", 8), "sh -c '…");
        assert_eq!(fit("abc", 3), "abc");
    }
}
