use tickerlight_core::{ChartStyle, Color};

/// Colors and sizes of the controls. Sizes are logical pixels.
///
/// The window is a near-black canvas with cards on it (the chart, the bar, the tool rail, the side column),
/// rounded and apart by a gutter instead of lines between them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
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
    /// Text in the accent's color on the dark background: the selected tool of the bar.
    pub accent_text: Color,
    /// Text on a filled control (the accent, buy, sell, a swatch).
    pub on_fill: Color,
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
        Self {
            canvas: Color::rgb(12, 13, 16),
            background: style.background,
            border: style.axis,
            control: Color::rgb(31, 34, 42),
            control_hover: Color::rgb(42, 46, 56),
            field: Color::rgb(16, 18, 22),
            text: style.hud_text,
            muted: style.axis_text,
            accent: style.accent,
            accent_text: Color::rgb(255, 189, 89),
            on_fill: Color::rgb(28, 20, 5),
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
