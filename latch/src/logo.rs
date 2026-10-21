//! The Plow logo, shown by a bare `latch` on a terminal. It plays the old
//! Latch app's welcome animation in text: the mark's outline draws itself,
//! the letters fill in Plow's volt green as the outline fades, and a white
//! glint sweeps across them. With `NO_COLOR` it shows the finished mark,
//! as the app did for people who prefer reduced motion.
//!
//! The mark is Plow's lettermark (plow.co/assets/logos/v2), scaled to 32 ×
//! 32 pixels. Each character cell shows two pixels, one above the other
//! (`▀`, `▄`, `█`), so the logo is 32 columns by 16 rows.

use std::io::Write;
use std::thread::sleep;
use std::time::Duration;

use crate::paint::{self, DIM, FAINT, Rgb, SHINE, VOLT, mix};
use crate::term;

/// The mark, one string per pixel row: `#` is part of the letters.
const MARK: [&str; 32] = [
    "......##...........##.......###.",
    "...#######......########....####",
    "..#########....##########...####",
    ".###########...###########..####",
    ".####...####..####....####..####",
    ".###.....###..###......###..####",
    "####.....###..###......###..####",
    "####.....###..###......###..####",
    "####....####..###......###..####",
    "####....####..####....####..####",
    "###########....##########...####",
    "###########....#########....####",
    "#########........######.....####",
    "#####.......................####",
    "####........................####",
    "####........................####",
    "####...####...####...####...####",
    "####...####...####...####...####",
    "####...####...####...####...####",
    "####...####...####...####...####",
    "####...####...####...####...####",
    "####...####...####...####...####",
    "####...####...####...####...####",
    "####...####...####...####...####",
    "####...####...####...####...####",
    "####...####...####...####...####",
    "####...####...####...####...####",
    "####...####...#####..#####.####.",
    "####....#####..################.",
    "####....#####..################.",
    "####.....####...##############..",
    ".###.......##......#......#.....",
];
const SIZE: usize = 32;

/// What a pixel looks like in one frame: empty, or a color.
type Pixels = [[Option<Rgb>; SIZE]; SIZE];

/// Plays the animation (or, without color, shows the finished mark) and
/// the word "Plow" under it.
pub fn show(animate: bool) {
    let mut out = std::io::stdout();
    let frames = if animate {
        frames()
    } else {
        vec![(finished(), 0)]
    };
    let height = SIZE / 2;
    for (i, (pixels, ms)) in frames.iter().enumerate() {
        let mut text = String::new();
        if i > 0 {
            // Back to the top of the logo, to draw the next frame over it.
            text.push_str(&term::up(height));
        }
        text.push_str(&render(pixels));
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
        sleep(Duration::from_millis(*ms));
    }
    let word = format!("{}{}", " ".repeat(SIZE / 2 - 2), paint::bold("Plow", VOLT));
    let hint = paint::fg("run `latch --help` to see what latch does", DIM);
    let _ = writeln!(out, "\n{word}\n\n{hint}");
}

/// The frames, about 2 seconds in all: the outline draws in, the letters
/// fill as it fades, the glint sweeps across, and the mark rests.
fn frames() -> Vec<(Pixels, u64)> {
    let outline = outline_in_drawing_order();
    let mut frames = Vec::new();

    // Draw: the outline appears point by point, in ~1 s.
    let steps = 20;
    for step in 1..=steps {
        let mut pixels = [[None; SIZE]; SIZE];
        for &(x, y) in &outline[..outline.len() * step / steps] {
            pixels[y][x] = Some(VOLT);
        }
        frames.push((pixels, 45));
    }
    // Fill: the letters come up from dark to volt, the outline with them.
    for step in 1..=8 {
        let fill = mix(FAINT, VOLT, step as f32 / 8.0);
        let mut pixels = filled(fill);
        for &(x, y) in &outline {
            pixels[y][x] = Some(VOLT);
        }
        frames.push((pixels, 60));
    }
    // Glint: a slanted white band crosses the letters, left to right.
    let mut sweep = -6;
    while sweep < 2 * SIZE as i32 + 6 {
        let mut pixels = finished();
        for (y, row) in pixels.iter_mut().enumerate() {
            for (x, pixel) in row.iter_mut().enumerate() {
                let distance = (x as i32 + y as i32 / 2 - sweep).abs();
                if let (Some(color), true) = (*pixel, distance <= 3) {
                    *pixel = Some(mix(SHINE, color, distance as f32 / 4.0));
                }
            }
        }
        frames.push((pixels, 30));
        sweep += 4;
    }
    frames.push((finished(), 0));
    frames
}

/// The resting mark: every letter pixel in volt.
fn finished() -> Pixels {
    filled(VOLT)
}

fn filled(color: Rgb) -> Pixels {
    let mut pixels = [[None; SIZE]; SIZE];
    for (y, row) in MARK.iter().enumerate() {
        for (x, ch) in row.chars().enumerate() {
            if ch == '#' {
                pixels[y][x] = Some(color);
            }
        }
    }
    pixels
}

/// The edge pixels of the letters (lit, with an unlit neighbour), ordered
/// as a pen would trace them: always on to the nearest one not yet drawn.
fn outline_in_drawing_order() -> Vec<(usize, usize)> {
    let lit = |x: i32, y: i32| {
        x >= 0
            && y >= 0
            && (x as usize) < SIZE
            && (y as usize) < SIZE
            && MARK[y as usize].as_bytes()[x as usize] == b'#'
    };
    let mut edge = Vec::new();
    for y in 0..SIZE as i32 {
        for x in 0..SIZE as i32 {
            let open_side = !lit(x - 1, y) || !lit(x + 1, y) || !lit(x, y - 1) || !lit(x, y + 1);
            if lit(x, y) && open_side {
                edge.push((x as usize, y as usize));
            }
        }
    }
    let mut ordered = Vec::with_capacity(edge.len());
    let mut at = edge.remove(0);
    ordered.push(at);
    while !edge.is_empty() {
        let distance = |&(x, y): &(usize, usize)| x.abs_diff(at.0).pow(2) + y.abs_diff(at.1).pow(2);
        let nearest = (0..edge.len())
            .min_by_key(|&i| distance(&edge[i]))
            .expect("not empty");
        at = edge.remove(nearest);
        ordered.push(at);
    }
    ordered
}

/// Two pixel rows per text row: the upper pixel is the character's top
/// half, the lower its bottom half.
fn render(pixels: &Pixels) -> String {
    let mut text = String::new();
    for pair in pixels.chunks(2) {
        for (&top, &bottom) in pair[0].iter().zip(&pair[1]) {
            let cell = match (top, bottom) {
                (None, None) => " ".to_string(),
                (Some(top), None) => paint::fg("▀", top),
                (None, Some(bottom)) => paint::fg("▄", bottom),
                (Some(top), Some(bottom)) if top == bottom => paint::fg("█", top),
                (Some(top), Some(bottom)) => paint::fg_bg("▀", top, bottom),
            };
            text.push_str(&cell);
        }
        text.push_str(term::CLEAR_LINE_END);
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_outline_covers_every_edge_pixel_once() {
        let outline = outline_in_drawing_order();
        let mut unique = outline.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), outline.len());
        assert!(outline.iter().all(|&(x, y)| MARK[y].as_bytes()[x] == b'#'));
    }
}
