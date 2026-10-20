use core::ops::Range;

use tickerlight_text::Align;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Text that reads on this color: near black on a light one, near white on a dark one.
    pub fn ink(self) -> Color {
        let lum = 0.2126 * self.r as f32 + 0.7152 * self.g as f32 + 0.0722 * self.b as f32;
        if lum > 140.0 {
            Color::rgb(24, 20, 14)
        } else {
            Color::rgb(245, 242, 238)
        }
    }
}

/// Filled rectangle in pixels. `repr(C)` so a backend can upload the array as is.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: Color,
    /// Corner radius in pixels, `0.0` for sharp corners.
    pub radius: f32,
    /// `0.0` fills the rectangle; more draws only its outline, this many pixels wide, inside the edge.
    pub border: f32,
    /// The color at the far end of a gradient: `color` at the top (the left, [`Direction::Right`]), this at the
    /// bottom (the right), blended in between. The same as `color` for a flat fill.
    pub to: Color,
    /// How many pixels inside its edge the rectangle fades in over: soft edges, glows and shadows. `0.0` keeps
    /// the edges crisp.
    pub feather: f32,
    pub direction: Direction,
}

impl Rect {
    /// A flat fill with crisp square corners.
    pub const fn new(x: f32, y: f32, w: f32, h: f32, color: Color) -> Self {
        Self {
            x,
            y,
            w,
            h,
            color,
            radius: 0.0,
            border: 0.0,
            to: color,
            feather: 0.0,
            direction: Direction::Down,
        }
    }
}

/// Which way a gradient runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Direction {
    /// From the top down.
    #[default]
    Down = 0,
    /// From the left to the right.
    Right = 1,
}

/// A line segment of `width` pixels, anti-aliased. `repr(C)` so a backend can upload the array as is.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct Line {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub width: f32,
    pub color: Color,
}

/// One line of text. `(x, y)` is where the line is anchored: `align` says whether `x` is its left,
/// center or right end, and `y` is the middle of the capital letters.
#[derive(Debug, Clone, PartialEq)]
pub struct Text {
    pub x: f32,
    pub y: f32,
    /// Font size in pixels.
    pub size: f32,
    pub color: Color,
    pub align: Align,
    pub text: String,
}

/// A rectangle in pixels that a layer is cut to: nothing is drawn outside it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl ClipRect {
    /// The part both rectangles cover (empty, with zero size, when they do not meet).
    pub fn intersect(&self, other: &ClipRect) -> ClipRect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.w).min(other.x + other.w);
        let bottom = (self.y + self.h).min(other.y + other.h);
        ClipRect {
            x,
            y,
            w: (right - x).max(0.0),
            h: (bottom - y).max(0.0),
        }
    }
}

/// Where a layer of the scene ends in each primitive list (exclusive).
#[derive(Debug, Clone, Copy, PartialEq)]
struct LayerEnd {
    rects: usize,
    lines: usize,
    texts: usize,
    clip: Option<ClipRect>,
}

impl LayerEnd {
    const START: Self = Self {
        rects: 0,
        lines: 0,
        texts: 0,
        clip: None,
    };

    fn same_extent(&self, other: &Self) -> bool {
        (self.rects, self.lines, self.texts) == (other.rects, other.lines, other.texts)
    }
}

/// Which primitives of the scene belong to one layer.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerRange {
    pub rects: Range<usize>,
    pub lines: Range<usize>,
    pub texts: Range<usize>,
    /// What the layer is cut to, if anything.
    pub clip: Option<ClipRect>,
}

/// Drawing primitives in batches. Backends only draw this.
///
/// Primitives are kept in flat lists (fast to build and to upload) and split into ordered layers.
/// Layers are drawn one after another, bottom first; inside a layer all rects come first, then
/// lines, then text. So text can sit above rects of the same layer, and a later layer (a crosshair
/// label, say) covers everything of the earlier ones, text included.
#[derive(Debug, Clone)]
pub struct Scene {
    pub background: Color,
    pub rects: Vec<Rect>,
    pub lines: Vec<Line>,
    pub texts: Vec<Text>,
    layer_ends: Vec<LayerEnd>,
    clip: Option<ClipRect>,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            background: Color::rgb(21, 23, 28),
            rects: Vec::new(),
            lines: Vec::new(),
            texts: Vec::new(),
            layer_ends: Vec::new(),
            clip: None,
        }
    }
}

impl Scene {
    pub fn new() -> Self {
        Self::default()
    }

    /// Empties the scene but keeps its allocations for the next frame.
    pub fn clear(&mut self) {
        self.rects.clear();
        self.lines.clear();
        self.texts.clear();
        self.layer_ends.clear();
        self.clip = None;
    }

    /// Cuts the layer being filled (the primitives added from now until [`next_layer`](Self::next_layer))
    /// to `clip`. Layers start unclipped.
    pub fn set_clip(&mut self, clip: Option<ClipRect>) {
        self.clip = clip;
    }

    pub fn fill(&mut self, x: f32, y: f32, w: f32, h: f32, color: Color) {
        self.rects.push(Rect::new(x, y, w, h, color));
    }

    /// A rectangle going from one color to another, down or to the right. A color with an alpha of zero fades out
    /// without darkening on the way (colors blend premultiplied).
    pub fn fill_gradient(&mut self, x: f32, y: f32, w: f32, h: f32, (from, to): (Color, Color), direction: Direction) {
        self.rects.push(Rect {
            to,
            direction,
            ..Rect::new(x, y, w, h, from)
        });
    }

    /// A rectangle that fades out over `feather` pixels inside its edge, with rounded corners: a soft zone, a
    /// glow or (dark and under something) a shadow.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_soft(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, feather: f32, color: Color) {
        self.rects.push(Rect {
            radius: radius.min(w.min(h) / 2.0).max(0.0),
            feather: feather.min(w.min(h) / 2.0).max(0.0),
            ..Rect::new(x, y, w, h, color)
        });
    }

    /// A rectangle with rounded corners. The radius is limited to half the shorter side.
    pub fn fill_rounded(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: Color) {
        self.rects.push(Rect {
            radius: radius.min(w.min(h) / 2.0).max(0.0),
            ..Rect::new(x, y, w, h, color)
        });
    }

    /// The outline of a rectangle with rounded corners, `width` pixels wide, inside its edge. With a radius of
    /// half the side it is a ring.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_rounded(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, width: f32, color: Color) {
        self.rects.push(Rect {
            radius: radius.min(w.min(h) / 2.0).max(0.0),
            border: width.max(f32::MIN_POSITIVE),
            ..Rect::new(x, y, w, h, color)
        });
    }

    /// A filled circle around `(x, y)`.
    pub fn circle(&mut self, (x, y): (f32, f32), r: f32, color: Color) {
        self.fill_rounded(x - r, y - r, 2.0 * r, 2.0 * r, r, color);
    }

    pub fn line(&mut self, from: (f32, f32), to: (f32, f32), width: f32, color: Color) {
        self.lines.push(Line {
            x0: from.0,
            y0: from.1,
            x1: to.0,
            y1: to.1,
            width,
            color,
        });
    }

    /// Moves everything added since the given list lengths down by `dy` pixels: a layer draws in the
    /// coordinates of its pane and the chart shifts it into place.
    pub fn shift_down_from(&mut self, (rects, lines, texts): (usize, usize, usize), dy: f32) {
        self.rects[rects..].iter_mut().for_each(|r| r.y += dy);
        self.lines[lines..].iter_mut().for_each(|l| {
            l.y0 += dy;
            l.y1 += dy;
        });
        self.texts[texts..].iter_mut().for_each(|t| t.y += dy);
    }

    /// How many primitives of each kind the scene holds: a mark for [`shift_down_from`](Self::shift_down_from).
    pub fn mark(&self) -> (usize, usize, usize) {
        (self.rects.len(), self.lines.len(), self.texts.len())
    }

    /// Everything added since `mark` at `opacity` (0 to 1) of its own: a part of the scene fading in.
    pub fn fade_from(&mut self, (rects, lines, texts): (usize, usize, usize), opacity: f32) {
        let t = opacity.clamp(0.0, 1.0);
        let fade = |c: &mut Color| c.a = (c.a as f32 * t).round() as u8;
        for r in self.rects.iter_mut().skip(rects) {
            fade(&mut r.color);
            fade(&mut r.to);
        }
        for l in self.lines.iter_mut().skip(lines) {
            fade(&mut l.color);
        }
        for x in self.texts.iter_mut().skip(texts) {
            fade(&mut x.color);
        }
    }

    /// Closes the current layer: everything added from now on is drawn above everything so far.
    /// The next layer starts unclipped.
    pub fn next_layer(&mut self) {
        let end = LayerEnd {
            rects: self.rects.len(),
            lines: self.lines.len(),
            texts: self.texts.len(),
            clip: self.clip.take(),
        };
        let last = self.layer_ends.last().copied().unwrap_or(LayerEnd::START);
        if !end.same_extent(&last) {
            self.layer_ends.push(end);
        }
    }

    /// The layers in drawing order. The layer still being filled comes last (if it has anything).
    pub fn layers(&self) -> Vec<LayerRange> {
        let mut out = Vec::with_capacity(self.layer_ends.len() + 1);
        let mut start = LayerEnd::START;
        let last = LayerEnd {
            rects: self.rects.len(),
            lines: self.lines.len(),
            texts: self.texts.len(),
            clip: self.clip,
        };
        for &end in self.layer_ends.iter().chain(core::iter::once(&last)) {
            if !end.same_extent(&start) {
                out.push(LayerRange {
                    rects: start.rects..end.rects,
                    lines: start.lines..end.lines,
                    texts: start.texts..end.texts,
                    clip: end.clip,
                });
            }
            start = end;
        }
        out
    }

    /// Adds the layers of `other` above everything so far, moved by `(dx, dy)` and each cut to `clip` as well as
    /// to its own clip: how one surface shows another that draws in its own coordinates.
    pub fn append(&mut self, other: &Scene, (dx, dy): (f32, f32), clip: ClipRect) {
        self.next_layer();
        for layer in other.layers() {
            let own = layer.clip.map(|c| ClipRect {
                x: c.x + dx,
                y: c.y + dy,
                ..c
            });
            self.rects.extend(other.rects[layer.rects].iter().map(|r| Rect {
                x: r.x + dx,
                y: r.y + dy,
                ..*r
            }));
            self.lines.extend(other.lines[layer.lines].iter().map(|l| Line {
                x0: l.x0 + dx,
                y0: l.y0 + dy,
                x1: l.x1 + dx,
                y1: l.y1 + dy,
                ..*l
            }));
            self.texts.extend(other.texts[layer.texts].iter().map(|t| Text {
                x: t.x + dx,
                y: t.y + dy,
                ..t.clone()
            }));
            self.set_clip(Some(own.map_or(clip, |own| own.intersect(&clip))));
            self.next_layer();
        }
    }

    pub fn text(&mut self, x: f32, y: f32, size: f32, color: Color, align: Align, text: impl Into<String>) {
        self.texts.push(Text {
            x,
            y,
            size,
            color,
            align,
            text: text.into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color = Color::rgb(255, 0, 0);

    #[test]
    fn an_empty_scene_has_no_layers() {
        assert!(Scene::new().layers().is_empty());
    }

    #[test]
    fn without_next_layer_everything_is_one_layer() {
        let mut s = Scene::new();
        s.fill(0.0, 0.0, 1.0, 1.0, RED);
        s.line((0.0, 0.0), (1.0, 1.0), 1.0, RED);
        s.text(0.0, 0.0, 12.0, RED, Align::Left, "a");
        assert_eq!(
            s.layers(),
            [LayerRange {
                rects: 0..1,
                lines: 0..1,
                texts: 0..1,
                clip: None
            }]
        );
    }

    #[test]
    fn next_layer_splits_every_list() {
        let mut s = Scene::new();
        s.fill(0.0, 0.0, 1.0, 1.0, RED);
        s.text(0.0, 0.0, 12.0, RED, Align::Left, "a");
        s.next_layer();
        s.fill(0.0, 0.0, 1.0, 1.0, RED);
        s.fill(0.0, 0.0, 1.0, 1.0, RED);
        s.line((0.0, 0.0), (1.0, 1.0), 1.0, RED);
        s.next_layer();
        s.text(0.0, 0.0, 12.0, RED, Align::Left, "b");
        assert_eq!(
            s.layers(),
            [
                LayerRange {
                    rects: 0..1,
                    lines: 0..0,
                    texts: 0..1,
                    clip: None
                },
                LayerRange {
                    rects: 1..3,
                    lines: 0..1,
                    texts: 1..1,
                    clip: None
                },
                LayerRange {
                    rects: 3..3,
                    lines: 1..1,
                    texts: 1..2,
                    clip: None
                },
            ]
        );
    }

    #[test]
    fn empty_layers_are_skipped_and_clear_resets_them() {
        let mut s = Scene::new();
        s.next_layer(); // nothing yet
        s.fill(0.0, 0.0, 1.0, 1.0, RED);
        s.next_layer();
        s.next_layer(); // nothing new
        assert_eq!(s.layers().len(), 1);
        s.clear();
        assert!(s.layers().is_empty());
        s.fill(0.0, 0.0, 1.0, 1.0, RED);
        assert_eq!(
            s.layers(),
            [LayerRange {
                rects: 0..1,
                lines: 0..0,
                texts: 0..0,
                clip: None
            }]
        );
    }

    #[test]
    fn a_layer_keeps_its_clip_and_the_next_one_starts_without() {
        let clip = ClipRect {
            x: 1.0,
            y: 2.0,
            w: 3.0,
            h: 4.0,
        };
        let mut s = Scene::new();
        s.set_clip(Some(clip));
        s.fill(0.0, 0.0, 1.0, 1.0, RED);
        s.next_layer();
        s.fill(0.0, 0.0, 1.0, 1.0, RED);
        s.next_layer();
        s.set_clip(Some(clip));
        s.fill(0.0, 0.0, 1.0, 1.0, RED); // still open: reported with its clip
        let layers = s.layers();
        assert_eq!(
            layers.iter().map(|l| l.clip).collect::<Vec<_>>(),
            [Some(clip), None, Some(clip)]
        );
        // An empty clipped layer does not leak its clip into the next layer.
        let mut s = Scene::new();
        s.set_clip(Some(clip));
        s.next_layer();
        s.fill(0.0, 0.0, 1.0, 1.0, RED);
        assert_eq!(s.layers()[0].clip, None);
    }

    #[test]
    fn an_appended_scene_moves_and_keeps_its_layers_inside_the_clip() {
        let mut inner = Scene::new();
        inner.fill(0.0, 0.0, 10.0, 10.0, Color::rgb(1, 1, 1));
        inner.next_layer();
        inner.set_clip(Some(ClipRect {
            x: 5.0,
            y: 5.0,
            w: 100.0,
            h: 100.0,
        }));
        inner.line((0.0, 0.0), (1.0, 1.0), 1.0, Color::rgb(2, 2, 2));
        inner.text(3.0, 4.0, 12.0, Color::rgb(3, 3, 3), Align::Left, "x");

        let mut outer = Scene::new();
        outer.fill(0.0, 0.0, 1.0, 1.0, Color::rgb(9, 9, 9));
        let region = ClipRect {
            x: 20.0,
            y: 30.0,
            w: 50.0,
            h: 40.0,
        };
        outer.append(&inner, (20.0, 30.0), region);
        let layers = outer.layers();
        assert_eq!(layers.len(), 3, "the outer layer, then the two inner ones");
        assert_eq!(layers[0].clip, None);
        assert_eq!(layers[1].clip, Some(region));
        assert_eq!(
            layers[2].clip,
            Some(ClipRect {
                x: 25.0,
                y: 35.0,
                w: 45.0,
                h: 35.0
            })
        );
        assert_eq!((outer.rects[1].x, outer.rects[1].y), (20.0, 30.0));
        assert_eq!((outer.lines[0].x1, outer.lines[0].y1), (21.0, 31.0));
        assert_eq!((outer.texts[0].x, outer.texts[0].y), (23.0, 34.0));
        // Anything added later is above the appended layers.
        outer.fill(0.0, 0.0, 1.0, 1.0, Color::rgb(9, 9, 9));
        assert_eq!(outer.layers().len(), 4);
    }
}
