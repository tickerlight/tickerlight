//! Line icons, drawn with the scene's lines and rounded boxes: no images to load, sharp at any scale.

use tickerlight_core::{Color, Scene};

use crate::Area;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Cursor,
    Trend,
    Ray,
    Extended,
    Arrow,
    HLine,
    HRay,
    VLine,
    Cross,
    Channel,
    Rect,
    Fib,
    PriceRange,
    DateRange,
    Ruler,
    Long,
    Short,
    Text,
    Magnet,
    Eye,
    EyeOff,
    Trash,
    Clone,
    Lock,
    Unlock,
    /// A line this many logical pixels thick.
    Width(u8),
    Solid,
    Dashed,
    Dotted,
    /// The charts of a window: one, two side by side, two one above the other, four.
    LayoutOne,
    LayoutColumns,
    LayoutRows,
    LayoutGrid,
    /// A second window over a corner of the first: a chart in a window of its own.
    NewWindow,
    /// Symbols in rows: the watchlist.
    List,
    /// Prices going up and down: trading.
    Trade,
    /// A flow of steps: a strategy.
    Strategy,
    /// A book with lines: the journal of sessions.
    Journal,
    /// The looks: light and dark.
    Sun,
    Moon,
}

/// Draws `icon` centered in `area`, on an 18 by 18 grid of logical pixels.
pub fn draw(scene: &mut Scene, icon: Icon, area: Area, color: Color, up: Color, down: Color, scale: f32) {
    let unit = scale;
    let (cx, cy) = area.center();
    let at = |x: f32, y: f32| (cx + (x - 9.0) * unit, cy + (y - 9.0) * unit);
    let w = (1.25 * scale).max(1.0);
    let line = |scene: &mut Scene, x0: f32, y0: f32, x1: f32, y1: f32| scene.line(at(x0, y0), at(x1, y1), w, color);
    let ring = |scene: &mut Scene, x: f32, y: f32| {
        let r = 2.0 * unit;
        let (px, py) = at(x, y);
        scene.stroke_rounded(px - r, py - r, 2.0 * r, 2.0 * r, r, w, color);
    };
    let poly = |scene: &mut Scene, points: &[(f32, f32)], closed: bool| {
        let n = points.len();
        let segments = if closed { n } else { n - 1 };
        for i in 0..segments {
            let (a, b) = (points[i], points[(i + 1) % n]);
            scene.line(at(a.0, a.1), at(b.0, b.1), w, color);
        }
    };
    let arc = |scene: &mut Scene, x: f32, y: f32, r: f32, from: f32, to: f32| {
        let steps = 10;
        let points: Vec<(f32, f32)> = (0..=steps)
            .map(|i| {
                let t = from + (to - from) * i as f32 / steps as f32;
                (x + r * t.cos(), y + r * t.sin())
            })
            .collect();
        for pair in points.windows(2) {
            scene.line(at(pair[0].0, pair[0].1), at(pair[1].0, pair[1].1), w, color);
        }
    };
    let fill = |scene: &mut Scene, x0: f32, y0: f32, x1: f32, y1: f32, c: Color| {
        let (a, b) = (at(x0, y0), at(x1, y1));
        scene.fill(a.0, a.1, b.0 - a.0, b.1 - a.1, c);
    };
    use std::f32::consts::PI;
    match icon {
        Icon::Cursor => poly(
            scene,
            &[
                (5.0, 2.5),
                (5.0, 15.0),
                (8.2, 12.0),
                (10.5, 16.5),
                (12.3, 15.6),
                (10.1, 11.2),
                (14.2, 11.0),
            ],
            true,
        ),
        Icon::Trend => {
            line(scene, 5.4, 12.6, 12.6, 5.4);
            ring(scene, 4.0, 14.0);
            ring(scene, 14.0, 4.0);
        }
        Icon::Ray => {
            line(scene, 5.4, 12.6, 8.6, 9.4);
            line(scene, 11.4, 6.6, 17.0, 1.0);
            ring(scene, 4.0, 14.0);
            ring(scene, 10.0, 8.0);
        }
        Icon::Extended => {
            line(scene, 1.0, 17.0, 4.6, 13.4);
            line(scene, 7.4, 10.6, 10.6, 7.4);
            line(scene, 13.4, 4.6, 17.0, 1.0);
            ring(scene, 6.0, 12.0);
            ring(scene, 12.0, 6.0);
        }
        Icon::Arrow => {
            line(scene, 4.0, 14.0, 14.0, 4.0);
            line(scene, 14.0, 4.0, 8.0, 4.6);
            line(scene, 14.0, 4.0, 13.4, 10.0);
        }
        Icon::HLine => {
            line(scene, 1.0, 9.0, 7.0, 9.0);
            line(scene, 11.0, 9.0, 17.0, 9.0);
            ring(scene, 9.0, 9.0);
        }
        Icon::HRay => {
            line(scene, 6.0, 9.0, 17.0, 9.0);
            ring(scene, 4.0, 9.0);
        }
        Icon::VLine => {
            line(scene, 9.0, 1.0, 9.0, 7.0);
            line(scene, 9.0, 11.0, 9.0, 17.0);
            ring(scene, 9.0, 9.0);
        }
        Icon::Cross => {
            line(scene, 1.0, 9.0, 7.0, 9.0);
            line(scene, 11.0, 9.0, 17.0, 9.0);
            line(scene, 9.0, 1.0, 9.0, 7.0);
            line(scene, 9.0, 11.0, 9.0, 17.0);
            ring(scene, 9.0, 9.0);
        }
        Icon::Channel => {
            line(scene, 2.0, 10.0, 12.0, 2.0);
            line(scene, 6.0, 16.0, 16.0, 8.0);
            ring(scene, 2.5, 10.0);
            ring(scene, 15.5, 8.5);
        }
        Icon::Rect => {
            poly(scene, &[(3.0, 5.0), (15.0, 5.0), (15.0, 13.0), (3.0, 13.0)], true);
            ring(scene, 3.0, 5.0);
            ring(scene, 15.0, 13.0);
        }
        Icon::Fib => {
            for y in [3.0, 7.0, 11.0, 15.0] {
                line(scene, 3.0, y, 15.0, y);
            }
            ring(scene, 3.0, 15.0);
            ring(scene, 15.0, 3.0);
        }
        Icon::PriceRange => {
            line(scene, 5.0, 2.0, 13.0, 2.0);
            line(scene, 5.0, 16.0, 13.0, 16.0);
            line(scene, 9.0, 3.0, 9.0, 15.0);
            line(scene, 9.0, 3.0, 6.5, 5.5);
            line(scene, 9.0, 3.0, 11.5, 5.5);
            line(scene, 9.0, 15.0, 6.5, 12.5);
            line(scene, 9.0, 15.0, 11.5, 12.5);
        }
        Icon::DateRange => {
            line(scene, 2.0, 5.0, 2.0, 13.0);
            line(scene, 16.0, 5.0, 16.0, 13.0);
            line(scene, 3.0, 9.0, 15.0, 9.0);
            line(scene, 3.0, 9.0, 5.5, 6.5);
            line(scene, 3.0, 9.0, 5.5, 11.5);
            line(scene, 15.0, 9.0, 12.5, 6.5);
            line(scene, 15.0, 9.0, 12.5, 11.5);
        }
        Icon::Ruler => {
            poly(scene, &[(1.5, 6.0), (16.5, 6.0), (16.5, 12.0), (1.5, 12.0)], true);
            for (i, x) in [4.5, 7.5, 10.5, 13.5].into_iter().enumerate() {
                line(scene, x, 6.0, x, if i % 2 == 0 { 9.5 } else { 8.0 });
            }
        }
        Icon::Long | Icon::Short => {
            let (first, second) = if icon == Icon::Long { (up, down) } else { (down, up) };
            let split = if icon == Icon::Long { 10.5 } else { 7.5 };
            fill(scene, 3.0, 3.0, 15.0, split, Color { a: 110, ..first });
            fill(scene, 3.0, split, 15.0, 15.0, Color { a: 110, ..second });
            poly(scene, &[(3.0, 3.0), (15.0, 3.0), (15.0, 15.0), (3.0, 15.0)], true);
            line(scene, 3.0, split, 15.0, split);
        }
        Icon::Text => {
            line(scene, 3.5, 3.5, 14.5, 3.5);
            line(scene, 9.0, 3.5, 9.0, 15.0);
            line(scene, 3.5, 3.5, 3.5, 5.5);
            line(scene, 14.5, 3.5, 14.5, 5.5);
            line(scene, 7.0, 15.0, 11.0, 15.0);
        }
        Icon::Magnet => {
            line(scene, 4.5, 3.0, 4.5, 10.0);
            line(scene, 13.5, 3.0, 13.5, 10.0);
            line(scene, 7.5, 3.0, 7.5, 10.0);
            line(scene, 10.5, 3.0, 10.5, 10.0);
            line(scene, 4.5, 3.0, 7.5, 3.0);
            line(scene, 10.5, 3.0, 13.5, 3.0);
            line(scene, 4.5, 6.0, 7.5, 6.0);
            line(scene, 10.5, 6.0, 13.5, 6.0);
            arc(scene, 9.0, 10.0, 4.5, 0.0, PI);
            arc(scene, 9.0, 10.0, 1.5, 0.0, PI);
        }
        Icon::Eye | Icon::EyeOff => {
            arc(scene, 9.0, 15.0, 9.5, -PI * 0.79, -PI * 0.21);
            arc(scene, 9.0, 3.0, 9.5, PI * 0.21, PI * 0.79);
            ring(scene, 9.0, 9.0);
            if icon == Icon::EyeOff {
                line(scene, 3.0, 15.0, 15.0, 3.0);
            }
        }
        Icon::Trash => {
            line(scene, 2.5, 5.0, 15.5, 5.0);
            poly(scene, &[(7.0, 5.0), (7.0, 2.5), (11.0, 2.5), (11.0, 5.0)], false);
            poly(scene, &[(4.5, 5.0), (5.5, 16.0), (12.5, 16.0), (13.5, 5.0)], false);
            line(scene, 7.7, 8.0, 7.9, 13.0);
            line(scene, 10.3, 8.0, 10.1, 13.0);
        }
        Icon::Clone => {
            poly(scene, &[(6.5, 6.5), (15.5, 6.5), (15.5, 15.5), (6.5, 15.5)], true);
            poly(
                scene,
                &[(3.5, 11.5), (2.5, 11.5), (2.5, 2.5), (11.5, 2.5), (11.5, 3.5)],
                false,
            );
        }
        Icon::Lock | Icon::Unlock => {
            poly(scene, &[(4.0, 8.5), (14.0, 8.5), (14.0, 16.0), (4.0, 16.0)], true);
            line(scene, 9.0, 11.0, 9.0, 13.5);
            // The shackle; open, it is lifted out of the body on the left.
            let top = if icon == Icon::Unlock { 3.0 } else { 5.5 };
            arc(scene, 9.0, top, 3.5, PI, 2.0 * PI);
            line(scene, 12.5, top, 12.5, 8.5);
            line(scene, 5.5, top, 5.5, if icon == Icon::Unlock { 5.0 } else { 8.5 });
        }
        Icon::Width(px) => {
            let t = px as f32 * scale;
            let (a, b) = (at(2.0, 9.0), at(16.0, 9.0));
            scene.fill(a.0, a.1 - t / 2.0, b.0 - a.0, t, color);
        }
        Icon::Solid => line(scene, 1.0, 9.0, 17.0, 9.0),
        Icon::Dashed => {
            for x in [1.0, 7.0, 13.0] {
                line(scene, x, 9.0, x + 4.0, 9.0);
            }
        }
        Icon::Dotted => {
            for i in 0..6 {
                let x = 1.5 + i as f32 * 3.0;
                line(scene, x, 9.0, x + 1.2, 9.0);
            }
        }
        Icon::LayoutOne | Icon::LayoutColumns | Icon::LayoutRows | Icon::LayoutGrid => {
            poly(scene, &[(2.0, 3.0), (16.0, 3.0), (16.0, 15.0), (2.0, 15.0)], true);
            if matches!(icon, Icon::LayoutColumns | Icon::LayoutGrid) {
                line(scene, 9.0, 3.0, 9.0, 15.0);
            }
            if matches!(icon, Icon::LayoutRows | Icon::LayoutGrid) {
                line(scene, 2.0, 9.0, 16.0, 9.0);
            }
        }
        Icon::NewWindow => {
            poly(scene, &[(2.0, 6.0), (12.0, 6.0), (12.0, 15.0), (2.0, 15.0)], true);
            poly(
                scene,
                &[(6.0, 6.0), (6.0, 3.0), (16.0, 3.0), (16.0, 12.0), (12.0, 12.0)],
                false,
            );
        }
        Icon::List => {
            for y in [4.5, 9.0, 13.5] {
                ring(scene, 3.5, y);
                line(scene, 7.0, y, 15.5, y);
            }
        }
        Icon::Trade => {
            poly(scene, &[(2.0, 13.0), (6.5, 8.5), (9.5, 11.0), (15.5, 4.5)], false);
            poly(scene, &[(11.5, 4.5), (15.5, 4.5), (15.5, 8.5)], false);
            line(scene, 2.0, 16.0, 16.0, 16.0);
        }
        Icon::Strategy => {
            poly(scene, &[(2.0, 2.5), (8.0, 2.5), (8.0, 7.0), (2.0, 7.0)], true);
            poly(scene, &[(10.0, 11.0), (16.0, 11.0), (16.0, 15.5), (10.0, 15.5)], true);
            poly(scene, &[(5.0, 7.0), (5.0, 13.25), (10.0, 13.25)], false);
            poly(scene, &[(8.0, 4.75), (13.0, 4.75), (13.0, 11.0)], false);
        }
        Icon::Journal => {
            poly(scene, &[(4.0, 2.0), (14.5, 2.0), (14.5, 16.0), (4.0, 16.0)], true);
            line(scene, 6.5, 2.0, 6.5, 16.0);
            for y in [6.0, 9.0, 12.0] {
                line(scene, 9.0, y, 12.5, y);
            }
        }
        Icon::Sun => {
            let (x, y) = at(9.0, 9.0);
            let r = 3.5 * unit;
            scene.stroke_rounded(x - r, y - r, 2.0 * r, 2.0 * r, r, w, color);
            for k in 0..8 {
                let (s, c) = (k as f32 * PI / 4.0).sin_cos();
                line(scene, 9.0 + 5.8 * c, 9.0 + 5.8 * s, 9.0 + 7.8 * c, 9.0 + 7.8 * s);
            }
        }
        Icon::Moon => {
            // A crescent: the edge of a disc, less where a smaller one up and to the right covers it.
            arc(scene, 9.0, 9.0, 6.5, 0.2098, 4.5026);
            arc(scene, 12.0, 6.0, 5.5, 0.9142, 3.7990);
        }
    }
}
