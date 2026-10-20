use crate::{Atlas, Font};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// One glyph to draw: a pixel rectangle and the atlas region to show in it.
/// `repr(C)` so a backend can upload the array as is.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct GlyphQuad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
}

/// Appends the glyph quads of a single line of `text` to `out`.
///
/// `x` is the left, center or right end of the line depending on `align`; `y` is the middle of
/// the capital letters, which is what a label centered on a tick wants. Glyphs land on whole pixels.
pub fn layout(
    font: &Font,
    atlas: &mut Atlas,
    text: &str,
    size: f32,
    (x, y): (f32, f32),
    align: Align,
    out: &mut Vec<GlyphQuad>,
) {
    let width = font.measure(text, size);
    let start = match align {
        Align::Left => x,
        Align::Center => x - width / 2.0,
        Align::Right => x - width,
    };
    let baseline = (y + font.cap_height(size) / 2.0).round();
    let mut pen = start.round();
    let mut prev: Option<char> = None;
    for ch in text.chars() {
        if let Some(p) = prev {
            pen += font.kerning(p, ch, size);
        }
        let glyph = atlas.glyph(font, ch, size);
        if glyph.width > 0.0 {
            out.push(GlyphQuad {
                x: (pen + glyph.xmin).round(),
                y: baseline - glyph.ymin - glyph.height,
                w: glyph.width,
                h: glyph.height,
                u0: glyph.uv[0],
                v0: glyph.uv[1],
                u1: glyph.uv[2],
                v1: glyph.uv[3],
            });
        }
        pen += glyph.advance;
        prev = Some(ch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quads(text: &str, align: Align) -> Vec<GlyphQuad> {
        let mut atlas = Atlas::new(256);
        let mut out = Vec::new();
        layout(Font::ui(), &mut atlas, text, 14.0, (100.0, 50.0), align, &mut out);
        out
    }

    #[test]
    fn one_quad_per_visible_glyph() {
        assert_eq!(quads("12 34", Align::Left).len(), 4); // the space has no quad
        assert!(quads("", Align::Left).is_empty());
    }

    #[test]
    fn alignment_moves_the_line() {
        let left = quads("1234", Align::Left);
        let center = quads("1234", Align::Center);
        let right = quads("1234", Align::Right);
        let width = Font::ui().measure("1234", 14.0);
        assert!(left[0].x >= 100.0 - 1.0 && left[0].x <= 100.0 + 3.0, "starts at x");
        let shift = |q: &[GlyphQuad]| q[0].x - left[0].x;
        assert!((shift(&center) + width / 2.0).abs() <= 1.0);
        assert!((shift(&right) + width).abs() <= 1.0);
    }

    #[test]
    fn digits_are_centered_vertically_on_y() {
        let q = quads("8", Align::Left);
        let middle = q[0].y + q[0].h / 2.0;
        assert!((middle - 50.0).abs() <= 1.0, "middle of the digit is at {middle}");
    }

    #[test]
    fn glyphs_are_placed_on_whole_pixels() {
        for q in quads("1234.56", Align::Center) {
            assert_eq!(q.x, q.x.round());
            assert_eq!(q.y, q.y.round());
        }
    }

    #[test]
    fn same_digit_reuses_the_same_atlas_region() {
        let q = quads("11", Align::Left);
        assert_eq!((q[0].u0, q[0].v0), (q[1].u0, q[1].v0));
        assert!(q[1].x > q[0].x);
    }
}
