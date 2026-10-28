//! Text both the HUD and the settings panel ask the window layer to draw. Platform-free: the
//! window's renderer turns these into antialiased glyphs (GDI coverage mask) in embedded Geist.

use crate::gfx::Rgb;

/// The embedded Geist cuts (see `crates/app/fonts/`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Font {
    Regular,
    Medium,
    SemiBold,
    Mono,
    MonoMedium,
}

/// What `(x, y)` refers to on the measured text box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    TopLeft,
    Center,
    MidLeft,
    MidRight,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Text {
    pub s: String,
    pub x: f32,
    pub y: f32,
    pub anchor: Anchor,
    pub font: Font,
    pub px: i32,
    pub rgb: Rgb,
    /// Extra pixels between letters (the site's `tracking-[…em]` on uppercase labels).
    pub tracking: i32,
    pub upper: bool,
    /// Dark outline, for text drawn straight over the game (HUD labels).
    pub outline: bool,
}

impl Text {
    pub fn new(s: impl Into<String>, x: f32, y: f32, anchor: Anchor, font: Font, px: i32, rgb: Rgb) -> Self {
        Self { s: s.into(), x, y, anchor, font, px, rgb, tracking: 0, upper: false, outline: false }
    }

    /// Small uppercase letter-spaced label, the website's nav / button / caption style.
    pub fn caps(mut self, tracking: i32) -> Self {
        self.upper = true;
        self.tracking = tracking;
        self
    }

    pub fn outlined(mut self) -> Self {
        self.outline = true;
        self
    }
}

/// Geist Regular advance widths for ASCII 32..=126, per 1000 units (from the font's `hmtx`).
const GEIST_ADVANCE: [u16; 95] = [
    250, 213, 346, 478, 629, 802, 620, 178, 274, 274, 430, 558, 201, 419, 201, 480, 663, 384, 619, 613, 615, 626, 593, 524,
    604, 593, 297, 297, 544, 540, 544, 559, 906, 668, 680, 703, 694, 603, 590, 700, 713, 270, 597, 640, 580, 877, 743, 739,
    650, 733, 672, 640, 552, 689, 667, 945, 606, 576, 544, 347, 455, 347, 426, 557, 248, 551, 595, 546, 595, 561, 395, 594,
    581, 244, 260, 590, 267, 877, 581, 573, 595, 595, 379, 520, 392, 575, 536, 819, 585, 537, 537, 389, 264, 389, 523,
];

/// Width of `s` in Geist Regular at `px`. Anything outside ASCII (–, °, …) counts as an average
/// glyph. Medium and SemiBold run a few percent wider, which the wrap width absorbs.
pub fn measure(s: &str, px: i32) -> f32 {
    let units: u32 = s.chars().map(|c| GEIST_ADVANCE.get((c as usize).wrapping_sub(32)).copied().unwrap_or(580) as u32).sum();
    units as f32 * px as f32 / 1000.0
}

/// Word wrap to `width` px at `px`, balanced like CSS `text-wrap: balance`: each paragraph keeps
/// the fewest lines that fit, then the narrowest width that still needs only that many, so lines
/// come out even and a last line is never a lone word.
pub fn wrap(s: &str, width: f32, px: i32) -> Vec<String> {
    s.split('\n')
        .flat_map(|para| {
            let n = greedy(para, width, px).len();
            let (mut lo, mut hi) = (0.0, width);
            for _ in 0..12 {
                let mid = (lo + hi) / 2.0;
                if greedy(para, mid, px).len() <= n { hi = mid } else { lo = mid }
            }
            greedy(para, hi, px)
        })
        .collect()
}

fn greedy(para: &str, width: f32, px: i32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in para.split_whitespace() {
        if !line.is_empty() && measure(&format!("{line} {word}"), px) > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    lines.push(line);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_keeps_paragraphs_fits_and_balances() {
        assert_eq!(wrap("Title:\none two", 200.0, 12), ["Title:", "one two"]);
        let text = "Heroes Without Sound is free, and every feature stays free. If it helps you play, you can chip in.";
        let lines = wrap(text, 324.0, 12);
        assert!(lines.iter().all(|l| measure(l, 12) <= 324.0));
        // balanced: same line count as greedy, but no short last line
        assert_eq!(lines.len(), greedy(text, 324.0, 12).len());
        let (a, b) = (measure(&lines[0], 12), measure(lines.last().unwrap(), 12));
        assert!(b > 0.6 * a, "{lines:?}");
    }
}
