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

/// Greedy word wrap by character count. ponytail: assumes ~6.5 px per character at 12 px Geist;
/// measure with the renderer if help texts ever overflow.
pub fn wrap(s: &str, max_chars: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for para in s.split('\n') {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && line.len() + 1 + word.len() > max_chars {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    #[test]
    fn wrap_keeps_paragraphs_and_breaks_long_lines() {
        let w = super::wrap("Title:\none two three four five", 10);
        assert_eq!(w, ["Title:", "one two", "three four", "five"]);
    }
}
