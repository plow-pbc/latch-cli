//! Key art: OpenSSH's "drunken bishop" picture of a key (what
//! `ssh-keygen -lv` prints). A bishop starts in the middle of a 17 × 9
//! board and takes one diagonal step for every two bits of the key; each
//! square shows how often he landed there. Two different keys almost never
//! draw the same picture, so a person can compare keys by eye: the art a
//! host shows for itself matches the art an agent shows for that host.
//!
//! latch walks the 32 bytes of the ed25519 public key itself.

use crate::seal::from_hex;

pub const COLS: usize = 17;
pub const ROWS: usize = 9;

/// What OpenSSH draws for 0, 1, 2, … visits; `S` and `E` mark the start
/// and the end of the walk.
const SYMBOLS: &[u8] = b" .o+=*BOX@%&#/^";

/// The art for a 64-character hex key, as `ROWS + 2` lines of
/// `COLS + 2` characters (the board in a frame), or `None` if `key_hex`
/// is not a key.
pub fn art(key_hex: &str, title: &str) -> Option<Vec<String>> {
    let key: [u8; 32] = from_hex(key_hex)?.try_into().ok()?;
    let mut visits = [[0usize; COLS]; ROWS];
    let start = (COLS / 2, ROWS / 2);
    let (mut x, mut y) = start;
    for byte in key {
        let mut bits = byte;
        // Low bits first: bit 0 picks left or right, bit 1 up or down.
        for _ in 0..4 {
            x = if bits & 1 == 1 {
                (x + 1).min(COLS - 1)
            } else {
                x.saturating_sub(1)
            };
            y = if bits & 2 == 2 {
                (y + 1).min(ROWS - 1)
            } else {
                y.saturating_sub(1)
            };
            visits[y][x] += 1;
            bits >>= 2;
        }
    }

    let mut lines = vec![frame_line(&format!("[{title}]"))];
    for (row, counts) in visits.iter().enumerate() {
        let mut line = String::from("|");
        for (col, &count) in counts.iter().enumerate() {
            let symbol = if (col, row) == (x, y) {
                'E'
            } else if (col, row) == start {
                'S'
            } else {
                SYMBOLS[count.min(SYMBOLS.len() - 1)] as char
            };
            line.push(symbol);
        }
        line.push('|');
        lines.push(line);
    }
    lines.push(frame_line("[ED25519]"));
    Some(lines)
}

/// `+---[label]---+`, `COLS + 2` wide, with `label` centred.
fn frame_line(label: &str) -> String {
    let label: String = label.chars().take(COLS).collect();
    let dashes = COLS - label.chars().count();
    let left = dashes / 2;
    format!("+{}{label}{}+", "-".repeat(left), "-".repeat(dashes - left))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_is_framed_and_fixed_for_a_key() {
        let key = "516e8dd4bdc8f13724a23f67b5a44a3748e1ca0555adb918bee2eff00ce55096";
        let lines = art(key, "agent").unwrap();
        assert_eq!(lines.len(), ROWS + 2);
        assert!(lines.iter().all(|line| line.chars().count() == COLS + 2));
        assert_eq!(lines[0], "+-----[agent]-----+");
        assert_eq!(art(key, "agent"), Some(lines));
        assert!(art("not a key", "x").is_none());
    }

    #[test]
    fn different_keys_draw_different_art() {
        let a = art(&"00".repeat(32), "k").unwrap();
        let b = art(&"ff".repeat(32), "k").unwrap();
        assert_ne!(a, b);
    }
}
