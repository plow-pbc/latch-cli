//! The client's piped input, read before it connects, so a host never
//! runs a program on half a script.
//!
//! Nothing is guessed from what stdin looks like: a pipe that is merely
//! left open (as some tool runners leave it) never ends, so the read gets
//! the same time limit as the command, and anything wrong is a usage error
//! (exit 2) before anything runs.

use std::io::{IsTerminal, Read};
use std::sync::mpsc;
use std::time::Duration;

use crate::failure::{Failure, code};

/// Reads at most `limit + 1` bytes; the caller rejects input over its limit.
pub fn read_all(within: Duration, limit: usize) -> Result<Vec<u8>, Failure> {
    if std::io::stdin().is_terminal() {
        let message = "--stdin reads piped input, e.g. `latch exec -i -- sh < script.sh`; \
                       here stdin is a terminal"
            .to_string();
        return Err(Failure::new(code::stdin_is_terminal, message));
    }
    // A thread of its own does the read, so the wait can be cut off; if it
    // is, latch exits and the blocked read goes with it.
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        // One byte past the limit is enough to know the input is too big.
        let limit = limit as u64 + 1;
        let read = std::io::stdin().lock().take(limit).read_to_end(&mut bytes);
        let _ = sender.send(read.map(|_| bytes));
    });
    match receiver.recv_timeout(within) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(err)) => Err(Failure::new(
            code::stdin_unreadable,
            format!("cannot read stdin: {err}"),
        )),
        Err(_) => {
            let message = format!(
                "stdin did not end within {}s (--timeout); nothing was run",
                within.as_secs()
            );
            Err(Failure::new(code::stdin_timeout, message))
        }
    }
}
