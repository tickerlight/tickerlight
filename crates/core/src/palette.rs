//! The colors of what is drawn on the chart (drawings, indicators, strategies): tuned to read on its warm dark
//! graphite and to be told apart from each other and from the candles' jade and coral. On the light look each
//! gives way to its paper version ([`on_paper`]).

use crate::Color;

pub const BLUE: Color = Color::rgb(96, 140, 255);
pub const CORAL: Color = Color::rgb(255, 110, 125);
pub const AMBER: Color = Color::rgb(245, 165, 36);
pub const YELLOW: Color = Color::rgb(238, 214, 92);
pub const JADE: Color = Color::rgb(30, 190, 135);
pub const CYAN: Color = Color::rgb(70, 200, 225);
pub const VIOLET: Color = Color::rgb(170, 130, 255);
pub const MAGENTA: Color = Color::rgb(240, 110, 200);
pub const STONE: Color = Color::rgb(205, 200, 193);
pub const GRAY: Color = Color::rgb(140, 136, 130);

/// The palette's colors as they read on light paper: the same hues, deep enough for about 3:1 against it (the
/// yellow a little less, or it would be brown); the light stone becomes the paper's ink. Other colors stay.
pub const PAPER: [(Color, Color); 9] = [
    (BLUE, Color::rgb(52, 110, 230)),
    (CORAL, Color::rgb(226, 72, 90)),
    (AMBER, Color::rgb(214, 130, 12)),
    (YELLOW, Color::rgb(172, 134, 0)),
    (JADE, Color::rgb(8, 153, 107)),
    (CYAN, Color::rgb(0, 140, 170)),
    (VIOLET, Color::rgb(130, 90, 230)),
    (MAGENTA, Color::rgb(200, 60, 160)),
    (STONE, Color::rgb(64, 60, 56)),
];

/// `color` as it reads on light paper (see [`PAPER`]).
pub const fn on_paper(color: Color) -> Color {
    let mut i = 0;
    while i < PAPER.len() {
        let (dark, paper) = PAPER[i];
        if dark.r == color.r && dark.g == color.g && dark.b == color.b && dark.a == color.a {
            return paper;
        }
        i += 1;
    }
    color
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Look;

    #[test]
    fn the_light_look_paints_the_palette_deeper_and_keeps_the_rest() {
        assert_eq!(Look::Dark.paint(YELLOW), YELLOW);
        let faint = Color { a: 40, ..YELLOW };
        assert_eq!(Look::Light.paint(faint), Color { a: 40, ..PAPER[3].1 });
        assert_eq!(Look::Light.paint(GRAY), GRAY);
        for (dark, paper) in PAPER {
            assert_eq!(on_paper(dark), paper);
            // Painted once: a paper color is no key, so a scene painted again stays as it is.
            assert_eq!(on_paper(paper), paper, "{paper:?}");
            // Darker than the dark look's color, to read on white.
            let lum = |c: Color| 0.2126 * c.r as f32 + 0.7152 * c.g as f32 + 0.0722 * c.b as f32;
            assert!(lum(paper) < lum(dark));
        }
    }
}
