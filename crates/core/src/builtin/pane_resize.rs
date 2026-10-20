use crate::plugin::{commands, order};
use crate::{Chart, Event, EventCtx, Handler, Key, Plugin, Response, ScaleSetting};

/// How close to a separator the pointer must be to grab it, in logical pixels.
const GRAB: f32 = 4.0;
/// The least height a pane keeps when its separator is dragged, in logical pixels.
const MIN_HEIGHT: f32 = 40.0;

/// Drag the separator between two panes to resize them. Only the two panes next to it change; the
/// rest keep their share.
#[derive(Default)]
pub struct PaneResizeHandler {
    /// Index of the upper pane of the separator being dragged, and how far below the separator it was grabbed.
    dragging: Option<(usize, f32)>,
}

impl PaneResizeHandler {
    fn separator_at(ctx: &EventCtx<'_>) -> Option<usize> {
        let (x, y) = ctx.pointer.position?;
        // The separator runs under the axis too, but not past the right edge of the chart.
        (x >= 0.0).then_some(())?;
        let grab = GRAB * ctx.scale;
        ctx.panes.windows(2).position(|w| (y - w[1].top).abs() <= grab)
    }
}

impl Handler for PaneResizeHandler {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        if self.dragging.is_some() || Self::separator_at(ctx).is_some() {
            *ctx.shape = crate::PointerShape::ResizeVertical;
        }
        match event {
            Event::PointerDown if ctx.pointer.pressed => match Self::separator_at(ctx) {
                Some(i) => {
                    let y = ctx.pointer.position.map_or(0.0, |p| p.1);
                    self.dragging = Some((i, y - ctx.panes[i + 1].top));
                    Response::CONSUMED
                }
                None => Response::IGNORED,
            },
            Event::PointerMoved { .. } if self.dragging.is_some() => {
                let Some((i, grab)) = self.dragging else {
                    return Response::IGNORED;
                };
                let (Some((_, y)), Some(&upper), Some(&lower)) =
                    (ctx.pointer.position, ctx.panes.get(i), ctx.panes.get(i + 1))
                else {
                    return Response::CONSUMED;
                };
                let together = upper.height + lower.height;
                let min = (MIN_HEIGHT * ctx.scale).min(together / 2.0);
                let new_upper = (y - grab - upper.top).clamp(min, together - min);
                // Weights are shares of the plot: keep the shares of everyone else, move the boundary.
                let total: f32 = ctx.panes.iter().map(|p| p.height).sum();
                let sum: f32 = (0..ctx.pane_stack.len()).filter_map(|k| ctx.pane_stack.weight(k)).sum();
                ctx.pane_stack.set_weight(i, new_upper / total * sum);
                ctx.pane_stack.set_weight(i + 1, (together - new_upper) / total * sum);
                Response::CONSUMED_REDRAW
            }
            Event::PointerUp | Event::PointerLeft if self.dragging.take().is_some() => Response::CONSUMED,
            _ => Response::IGNORED,
        }
    }
}

/// Turns log or percent on or off for the price axis of the pane under the pointer: Alt+L, Alt+P.
pub struct ScaleModeHandler;

impl Handler for ScaleModeHandler {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        let mode = match *event {
            Event::Command(commands::SCALE_LOG) => ScaleSetting::Log,
            Event::Command(commands::SCALE_PERCENT) => ScaleSetting::Percent,
            _ => return Response::IGNORED,
        };
        let pane = ctx.pointer_pane().unwrap_or(0);
        let next = ctx.pane_stack.scale(pane).toggled(mode);
        ctx.pane_stack.set_scale(pane, next);
        Response::CONSUMED_REDRAW
    }
}

/// Adds pane resizing and the price axis modes to a chart; `DefaultPlugins` has it.
pub struct PanesPlugin;

impl Plugin for PanesPlugin {
    fn build(&self, chart: &mut Chart) {
        chart.add_handler(
            crate::plugin::names::PANES,
            order::PANES_HANDLER,
            PaneResizeHandler::default(),
        );
        chart.add_handler("scale-mode", order::PANES_HANDLER + 1, ScaleModeHandler);
        chart.bind(Key::Alt('l'), commands::SCALE_LOG);
        chart.bind(Key::Alt('p'), commands::SCALE_PERCENT);
    }
}
