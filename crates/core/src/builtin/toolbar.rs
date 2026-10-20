use tickerlight_text::Font;

use crate::plugin::{names, order};
use crate::{Align, Chart, ChartStyle, DrawCtx, Event, EventCtx, Handler, Layer, Plugin, Resources, Response};

/// How a button is colored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Normal,
    /// The color of rising candles.
    Buy,
    /// The color of falling candles.
    Sell,
}

/// The text of a button: fixed, or computed from the shared resources (a toggle's current state).
#[derive(Clone, Copy)]
pub enum Label {
    Fixed(&'static str),
    Dynamic(fn(&Resources) -> String),
}

impl Label {
    fn get(&self, res: &Resources) -> String {
        match self {
            Label::Fixed(text) => (*text).to_string(),
            Label::Dynamic(f) => f(res),
        }
    }
}

/// One button. It knows nothing about what it does: a click sends `command`, and whichever plugin
/// listens for it reacts. The functions read the shared resources to decide what to show.
#[derive(Clone, Copy)]
pub struct ToolButton {
    pub command: &'static str,
    pub label: Label,
    pub visible: fn(&Resources) -> bool,
    /// Drawn in the accent color (a toggle that is on).
    pub highlighted: fn(&Resources) -> bool,
    pub kind: ButtonKind,
}

fn always(_: &Resources) -> bool {
    true
}

fn never(_: &Resources) -> bool {
    false
}

impl ToolButton {
    /// A plain, always visible button.
    pub fn new(command: &'static str, label: Label) -> Self {
        Self {
            command,
            label,
            visible: always,
            highlighted: never,
            kind: ButtonKind::Normal,
        }
    }

    pub fn kind(mut self, kind: ButtonKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn visible_when(mut self, visible: fn(&Resources) -> bool) -> Self {
        self.visible = visible;
        self
    }

    pub fn highlighted_when(mut self, highlighted: fn(&Resources) -> bool) -> Self {
        self.highlighted = highlighted;
        self
    }
}

/// The buttons, in the order they are shown. Add yours with [`Chart::add_button`].
#[derive(Default)]
pub struct Toolbar {
    pub buttons: Vec<ToolButton>,
    /// The command of the button the pointer went down on and has not been released from.
    pressed: Option<&'static str>,
}

const HEIGHT: f32 = 26.0;
const PADDING: f32 = 10.0;
const GAP: f32 = 6.0;
const MARGIN: f32 = 12.0;

/// Where a button is, in device pixels.
#[derive(Debug, Clone, Copy)]
pub struct ButtonRect {
    pub command: &'static str,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl ButtonRect {
    fn contains(&self, (px, py): (f32, f32)) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// The visible buttons laid out at the bottom left of the plot: in a row, which wraps into more rows
/// (growing upwards, the first buttons on the top row) when the plot is too narrow for one. Drawing and
/// hit testing both use this, so they cannot disagree.
pub fn layout(
    res: &Resources,
    style: &ChartStyle,
    scale: f32,
    plot_w: f32,
    plot_h: f32,
) -> Vec<(ToolButton, String, ButtonRect)> {
    let Some(toolbar) = res.get::<Toolbar>() else {
        return Vec::new();
    };
    let font = Font::ui();
    let size = style.font_size * scale;
    let (h, pad, gap) = (HEIGHT * scale, PADDING * scale, GAP * scale);
    let left = MARGIN * scale;
    let mut x = left;
    let mut row = 0;
    let mut out = Vec::new();
    for button in &toolbar.buttons {
        if !(button.visible)(res) {
            continue;
        }
        let label = button.label.get(res);
        let w = font.measure(&label, size) + 2.0 * pad;
        if x > left && x + w > plot_w - left {
            row += 1;
            x = left;
        }
        // `y` holds the row for now; it becomes a position once the number of rows is known.
        out.push((
            *button,
            label,
            ButtonRect {
                command: button.command,
                x,
                y: row as f32,
                w,
                h,
            },
        ));
        x += w + gap;
    }
    let rows = row + 1;
    for (_, _, r) in &mut out {
        let from_bottom = rows - 1 - r.y as usize;
        r.y = plot_h - MARGIN * scale - h - from_bottom as f32 * (h + gap);
    }
    out
}

pub struct ToolbarHandler;

impl Handler for ToolbarHandler {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        if !matches!(
            event,
            Event::PointerDown | Event::PointerUp | Event::PointerMoved { .. } | Event::PointerLeft
        ) {
            return Response::IGNORED;
        }
        let rects = layout(
            ctx.res,
            ctx.style,
            ctx.scale,
            ctx.plot.0,
            ctx.panes.first().map_or(ctx.plot.1, |p| p.height),
        );
        let hit = |p: Option<(f32, f32)>| {
            p.and_then(|p| rects.iter().find(|(_, _, r)| r.contains(p)))
                .map(|(b, _, _)| b.command)
        };
        let Some(toolbar) = ctx.res.get_mut::<Toolbar>() else {
            return Response::IGNORED;
        };
        match event {
            Event::PointerDown => match hit(ctx.pointer.position) {
                Some(command) => {
                    toolbar.pressed = Some(command);
                    Response::CONSUMED_REDRAW
                }
                None => Response::IGNORED,
            },
            Event::PointerMoved { .. } => {
                let changed = hit(ctx.previous_pointer.position) != hit(ctx.pointer.position);
                // While a button is held, the drag belongs to it, not to the chart.
                Response {
                    redraw: changed,
                    consumed: toolbar.pressed.is_some(),
                    ..Response::IGNORED
                }
            }
            Event::PointerUp => match toolbar.pressed.take() {
                Some(command) => {
                    if hit(ctx.pointer.position) == Some(command) {
                        ctx.emit(command);
                    }
                    Response::CONSUMED_REDRAW
                }
                None => Response::IGNORED,
            },
            _ => {
                let was = toolbar.pressed.take().is_some() || hit(ctx.previous_pointer.position).is_some();
                Response {
                    redraw: was,
                    ..Response::IGNORED
                }
            }
        }
    }
}

pub struct ToolbarLayer;

impl Layer for ToolbarLayer {
    fn draw(&mut self, ctx: &mut DrawCtx<'_>) {
        let style = ctx.style;
        let pressed = ctx.res.get::<Toolbar>().and_then(|t| t.pressed);
        let size = ctx.px(style.font_size);
        let radius = ctx.px(6.0);
        for (button, label, r) in layout(ctx.res, style, ctx.scale, ctx.plot.0, ctx.plot.1) {
            let hovered = ctx.pointer.position.is_some_and(|p| r.contains(p));
            let background = if pressed == Some(button.command) {
                style.accent
            } else if (button.highlighted)(ctx.res) {
                crate::Color { a: 230, ..style.accent }
            } else if hovered {
                style.panel_hover
            } else {
                style.panel
            };
            ctx.scene.fill_rounded(r.x, r.y, r.w, r.h, radius, background);
            let text = match button.kind {
                ButtonKind::Normal => style.hud_text,
                ButtonKind::Buy => style.candle.up,
                ButtonKind::Sell => style.candle.down,
            };
            let text = if pressed == Some(button.command) || (button.highlighted)(ctx.res) {
                style.crosshair_label_text
            } else {
                text
            };
            ctx.scene
                .text(r.x + r.w / 2.0, r.y + r.h / 2.0, size, text, Align::Center, label);
        }
    }
}

pub struct ToolbarPlugin;

impl Plugin for ToolbarPlugin {
    fn build(&self, chart: &mut Chart) {
        ensure_installed(chart);
    }
}

fn ensure_installed(chart: &mut Chart) {
    if !chart.resources().contains::<Toolbar>() {
        chart.resources_mut().insert(Toolbar::default());
    }
    if !chart.handler_names().contains(&names::TOOLBAR) {
        chart.add_handler(names::TOOLBAR, order::TOOLBAR_HANDLER, ToolbarHandler);
    }
    if !chart.layer_names().contains(&names::TOOLBAR) {
        chart.add_layer(names::TOOLBAR, order::TOOLBAR, ToolbarLayer);
    }
}

impl Chart {
    /// Adds a button to the toolbar (installing the toolbar if the chart has none). A button for the
    /// same command is replaced.
    pub fn add_button(&mut self, button: ToolButton) -> &mut Self {
        ensure_installed(self);
        let toolbar = self.resources_mut().get_mut::<Toolbar>().expect("installed above");
        match toolbar.buttons.iter_mut().find(|b| b.command == button.command) {
            Some(existing) => *existing = button,
            None => toolbar.buttons.push(button),
        }
        self
    }
}
