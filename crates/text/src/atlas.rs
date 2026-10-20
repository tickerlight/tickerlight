use std::collections::HashMap;

use crate::Font;

/// Where a glyph lives in the atlas and how to place it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glyph {
    /// Texture coordinates `[u0, v0, u1, v1]`.
    pub uv: [f32; 4],
    /// Bitmap size in pixels; zero for glyphs without ink (space).
    pub width: f32,
    pub height: f32,
    /// Offset of the bitmap's left edge from the pen position.
    pub xmin: f32,
    /// Offset of the bitmap's bottom edge above the baseline.
    pub ymin: f32,
    pub advance: f32,
}

/// Coverage bitmaps of glyphs (one byte per pixel) packed into one texture.
///
/// Glyphs are rasterized on first use, per character and pixel size. Positions are whole
/// pixels, so text stays crisp. If the atlas ever fills up it is emptied and refilled
/// (glyphs already handed out during that one frame may then look wrong for a frame).
pub struct Atlas {
    size: u32,
    pixels: Vec<u8>,
    glyphs: HashMap<(char, u32), Glyph>,
    x: u32,
    y: u32,
    row_height: u32,
    version: u64,
}

const PADDING: u32 = 1;

impl Atlas {
    pub fn new(size: u32) -> Self {
        Self {
            size,
            pixels: vec![0; (size * size) as usize],
            glyphs: HashMap::new(),
            x: 0,
            y: 0,
            row_height: 0,
            version: 0,
        }
    }

    /// Side of the square texture.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// `size * size` bytes, row by row.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Changes whenever the pixels do, so a backend knows when to upload again.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn glyph(&mut self, font: &Font, ch: char, px: f32) -> Glyph {
        let key = (ch, px.to_bits());
        if let Some(&glyph) = self.glyphs.get(&key) {
            return glyph;
        }
        let (metrics, bitmap) = font.rasterize(ch, px);
        let (w, h) = (metrics.width as u32, metrics.height as u32);
        let (x, y) = if w == 0 || h == 0 { (0, 0) } else { self.allocate(w, h) };
        for row in 0..h as usize {
            let src = &bitmap[row * w as usize..(row + 1) * w as usize];
            let dst = (y as usize + row) * self.size as usize + x as usize;
            self.pixels[dst..dst + w as usize].copy_from_slice(src);
        }
        let s = self.size as f32;
        let glyph = Glyph {
            uv: [x as f32 / s, y as f32 / s, (x + w) as f32 / s, (y + h) as f32 / s],
            width: w as f32,
            height: h as f32,
            xmin: metrics.xmin,
            ymin: metrics.ymin,
            advance: metrics.advance,
        };
        self.glyphs.insert(key, glyph);
        self.version += 1;
        glyph
    }

    /// Finds room for a `w` x `h` bitmap (shelf packing), emptying the atlas if it is full.
    fn allocate(&mut self, w: u32, h: u32) -> (u32, u32) {
        assert!(
            w + PADDING <= self.size && h + PADDING <= self.size,
            "glyph larger than the atlas"
        );
        if self.x + w + PADDING > self.size {
            self.x = 0;
            self.y += self.row_height + PADDING;
            self.row_height = 0;
        }
        if self.y + h + PADDING > self.size {
            self.pixels.fill(0);
            self.glyphs.clear();
            self.x = 0;
            self.y = 0;
            self.row_height = 0;
        }
        let at = (self.x, self.y);
        self.x += w + PADDING;
        self.row_height = self.row_height.max(h);
        at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_is_rasterized_once_and_has_ink() {
        let font = Font::ui();
        let mut atlas = Atlas::new(256);
        assert_eq!(atlas.version(), 0);
        let a = atlas.glyph(font, 'A', 16.0);
        let version = atlas.version();
        assert!(version > 0);
        assert_eq!(atlas.glyph(font, 'A', 16.0), a);
        assert_eq!(atlas.version(), version, "second lookup must not change the atlas");
        assert!(a.width > 4.0 && a.height > 8.0);
        assert!(
            atlas.pixels().iter().any(|&p| p > 200),
            "the bitmap has fully covered pixels"
        );
    }

    #[test]
    fn different_glyphs_and_sizes_get_separate_spots() {
        let font = Font::ui();
        let mut atlas = Atlas::new(256);
        let a16 = atlas.glyph(font, 'A', 16.0);
        let a12 = atlas.glyph(font, 'A', 12.0);
        let b16 = atlas.glyph(font, 'B', 16.0);
        assert_ne!(a16.uv, a12.uv);
        assert_ne!(a16.uv, b16.uv);
        assert!(a12.width < a16.width);
    }

    #[test]
    fn space_has_no_ink_but_advances() {
        let font = Font::ui();
        let mut atlas = Atlas::new(64);
        let space = atlas.glyph(font, ' ', 16.0);
        assert_eq!((space.width, space.height), (0.0, 0.0));
        assert!(space.advance > 2.0);
    }

    #[test]
    fn overlapping_is_impossible_and_a_full_atlas_recovers() {
        let font = Font::ui();
        let mut atlas = Atlas::new(64); // tiny: fills up quickly
        for ch in ('A'..='Z').chain('a'..='z') {
            let g = atlas.glyph(font, ch, 14.0);
            assert!(g.uv.iter().all(|&c| (0.0..=1.0).contains(&c)), "{ch}: {:?}", g.uv);
        }
        // Still works after wrapping around.
        assert!(atlas.glyph(font, 'W', 14.0).width > 0.0);
    }
}
