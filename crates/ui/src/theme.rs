use tickerlight_core::{ChartStyle, Color, Look};

/// Colors and sizes of the controls. Sizes are logical pixels.
///
/// The window is a canvas with cards on it (the chart, the bar, the tool rail, the side column), rounded and apart
/// by a gutter instead of lines between them: near black under graphite cards in the dark look, a warm grey under
/// paper cards in the light one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// Which look the colors are of; data colors shown in the controls (swatches) are painted with it.
    pub look: Look,
    /// Behind the cards.
    pub canvas: Color,
    /// The cards the panels are on.
    pub background: Color,
    /// Edges where a shape needs one (a field's frame, a floating box).
    pub border: Color,
    pub control: Color,
    pub control_hover: Color,
    /// Inside a text field, which is framed by `border`: fields are not mistaken for buttons.
    pub field: Color,
    pub text: Color,
    /// Labels and hints.
    pub muted: Color,
    /// A toggle that is on, a focused field.
    pub accent: Color,
    /// Text in the accent's color on the cards: the selected tool of the bar.
    pub accent_text: Color,
    /// Text on a filled control (the accent, buy, sell, a swatch).
    pub on_fill: Color,
    /// What a control's fill turns toward as it lights up under the pointer: lighter on dark, darker on light.
    pub lit: Color,
    /// Under a box that floats over the rest.
    pub shadow: Color,
    /// Buy and profit; sell and loss.
    pub up: Color,
    pub down: Color,
    pub font_size: f32,
    /// Height of a control and of a line of text.
    pub row_height: f32,
    /// Inside a panel, around its content.
    pub padding: f32,
    /// Between two controls.
    pub spacing: f32,
    /// Corners of controls.
    pub radius: f32,
    /// Corners of the cards.
    pub card_radius: f32,
    /// Between the cards and around them.
    pub gutter: f32,
}

impl Theme {
    /// Controls that match the chart.
    pub fn from_chart(style: &ChartStyle) -> Self {
        let dark = style.look == Look::Dark;
        let pick = |dark_color: Color, light_color: Color| if dark { dark_color } else { light_color };
        Self {
            look: style.look,
            canvas: pick(Color::rgb(12, 13, 16), Color::rgb(231, 228, 222)),
            background: style.background,
            border: style.axis,
            control: pick(Color::rgb(31, 34, 42), Color::rgb(238, 235, 230)),
            control_hover: pick(Color::rgb(42, 46, 56), Color::rgb(226, 222, 215)),
            field: pick(Color::rgb(16, 18, 22), Color::rgb(255, 255, 254)),
            text: style.hud_text,
            muted: style.axis_text,
            accent: style.accent,
            // Lighter than the accent on dark, deeper on light: both read as text on the card.
            accent_text: pick(Color::rgb(255, 189, 89), Color::rgb(160, 92, 0)),
            on_fill: Color::rgb(28, 20, 5),
            lit: pick(Color::rgb(255, 255, 255), Color::rgb(60, 52, 44)),
            shadow: pick(
                Color {
                    a: 150,
                    ..Color::rgb(0, 0, 0)
                },
                // A light warm haze: a black one looks dirty on paper.
                Color {
                    a: 40,
                    ..Color::rgb(80, 66, 50)
                },
            ),
            up: style.candle.up,
            down: style.candle.down,
            font_size: style.font_size,
            row_height: 26.0,
            padding: 10.0,
            spacing: 4.0,
            radius: 7.0,
            card_radius: 10.0,
            gutter: 6.0,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::from_chart(&ChartStyle::default())
    }
}
