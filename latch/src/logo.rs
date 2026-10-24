//! The Plow logo, beside the overview a bare `latch` shows at a terminal
//! (`overview.rs`). It plays the old Latch app's welcome animation in text:
//! the mark's outline draws itself, the letters fill in Plow's volt green as
//! the outline fades, and a white glint sweeps across them. With `NO_COLOR`
//! the overview shows the finished mark, as the app did for people who
//! prefer reduced motion.
//!
//! The mark is Plow's lettermark (plow.co/assets/logos/v2), scaled to 32 ×
//! 32 pixels. Each character cell shows two pixels, one above the other
//! (`▀`, `▄`, `█`), so the logo is 32 columns by 16 rows.

use crate::paint::{self, FAINT, Rgb, SHINE, VOLT, mix};

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
/// The mark's size in character cells.
pub const COLUMNS: usize = SIZE;
pub const ROWS: usize = SIZE / 2;

/// What a pixel looks like in one frame: empty, or a color.
pub type Pixels = [[Option<Rgb>; SIZE]; SIZE];

/// The frames, about 2 seconds in all: the outline draws in, the letters
/// fill as it fades, the glint sweeps across, and the mark rests.
pub fn frames() -> Vec<(Pixels, u64)> {
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
pub fn finished() -> Pixels {
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

/// The mark as text, a line each: two pixel rows per line, the upper
/// pixel the character's top half, the lower its bottom half.
pub fn lines(pixels: &Pixels) -> Vec<String> {
    pixels
        .chunks(2)
        .map(|pair| {
            pair[0]
                .iter()
                .zip(&pair[1])
                .map(|(&top, &bottom)| match (top, bottom) {
                    (None, None) => " ".to_string(),
                    (Some(top), None) => paint::fg("▀", top),
                    (None, Some(bottom)) => paint::fg("▄", bottom),
                    (Some(top), Some(bottom)) if top == bottom => paint::fg("█", top),
                    (Some(top), Some(bottom)) => paint::fg_bg("▀", top, bottom),
                })
                .collect()
        })
        .collect()
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
