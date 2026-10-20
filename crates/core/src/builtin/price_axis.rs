use crate::plugin::{commands, order};
use crate::{Chart, Event, EventCtx, Handler, Plugin, Response};

/// Drag on a price axis to stretch it, wheel over it to zoom it; with the range set by hand, dragging
/// in the plot moves it up and down as well. Autoscale comes back with [`commands::SCALE_AUTO`] or a double click
/// on the axis (Alt+R resets every pane).
///
/// The range lives in the pane stack (`manual_range`), so other plugins can set it too.
#[derive(Default)]
pub struct PriceAxisHandler {
    /// The pane whose axis the button went down on.
    axis_drag: Option<usize>,
    /// The pane whose plot the button went down in.
    plot_drag: Option<usize>,
}

/// Dragging an axis by this many pixels changes its range by a factor of e.
const DRAG_PIXELS_PER_E: f32 = 200.0;
const WHEEL_ZOOM_PER_NOTCH: f64 = 0.9;

fn pane_at_axis(ctx: &EventCtx<'_>) -> Option<usize> {
    let (x, y) = ctx.pointer.position?;
    (x >= ctx.plot.0).then_some(())?;
    ctx.panes.iter().position(|p| p.contains_y(y))
}

/// The range shown now: the manual one, or what the last frame autoscaled to.
fn current_range(ctx: &EventCtx<'_>, pane: usize) -> Option<(f64, f64)> {
    ctx.pane_stack
        .manual_range(pane)
        .or_else(|| ctx.prices.get(pane).copied().flatten().map(|p| (p.min, p.max)))
}

/// `factor` times the range, around its middle.
fn zoomed((min, max): (f64, f64), factor: f64) -> (f64, f64) {
    let (middle, half) = ((min + max) / 2.0, (max - min) / 2.0 * factor);
    (middle - half, middle + half)
}

impl Handler for PriceAxisHandler {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        match *event {
            Event::Command(commands::SCALE_AUTO) => {
                let pane = ctx.pointer_pane().or_else(|| pane_at_axis(ctx)).unwrap_or(0);
                let had = ctx.pane_stack.manual_range(pane).is_some();
                ctx.pane_stack.set_manual_range(pane, None);
                if had {
                    Response::CONSUMED_REDRAW
                } else {
                    Response::IGNORED
                }
            }
            // A double click on the axis fits the pane to its data again.
            Event::DoubleClick => match pane_at_axis(ctx) {
                Some(pane) => {
                    ctx.pane_stack.set_manual_range(pane, None);
                    Response::CONSUMED_REDRAW
                }
                None => Response::IGNORED,
            },
            Event::PointerDown if ctx.pointer.pressed => {
                self.axis_drag = pane_at_axis(ctx);
                self.plot_drag = if self.axis_drag.is_none() {
                    ctx.pointer_pane()
                } else {
                    None
                };
                if self.axis_drag.is_some() {
                    Response::CONSUMED
                } else {
                    Response::IGNORED
                }
            }
            Event::PointerMoved { .. } if ctx.pointer.pressed => {
                let (Some((_, before)), Some((_, now))) = (ctx.previous_pointer.position, ctx.pointer.position) else {
                    return Response::IGNORED;
                };
                let dy = now - before;
                if let Some(pane) = self.axis_drag {
                    let Some(range) = current_range(ctx, pane) else {
                        return Response::CONSUMED;
                    };
                    let factor = (dy / (DRAG_PIXELS_PER_E * ctx.scale)).exp() as f64;
                    ctx.pane_stack.set_manual_range(pane, Some(zoomed(range, factor)));
                    return Response::CONSUMED_REDRAW;
                }
                if let Some(pane) = self.plot_drag
                    && let Some((min, max)) = ctx.pane_stack.manual_range(pane)
                    && let Some(rect) = ctx.panes.get(pane)
                {
                    // The content follows the pointer: dragging down moves the range up.
                    let shift = dy as f64 / rect.height as f64 * (max - min);
                    ctx.pane_stack.set_manual_range(pane, Some((min + shift, max + shift)));
                    return Response::REDRAW;
                }
                Response::IGNORED
            }
            Event::PointerUp | Event::PointerLeft => {
                let was = self.axis_drag.take().is_some();
                self.plot_drag = None;
                if was { Response::CONSUMED } else { Response::IGNORED }
            }
            Event::Wheel { .. } => {
                let Some(pane) = pane_at_axis(ctx) else {
                    return Response::IGNORED;
                };
                let Event::Wheel { delta, .. } = *event else {
                    return Response::IGNORED;
                };
                let Some(range) = current_range(ctx, pane) else {
                    return Response::CONSUMED;
                };
                ctx.pane_stack
                    .set_manual_range(pane, Some(zoomed(range, WHEEL_ZOOM_PER_NOTCH.powf(delta as f64))));
                Response::CONSUMED_REDRAW
            }
            _ => Response::IGNORED,
        }
    }
}

pub struct PriceAxisPlugin;

impl Plugin for PriceAxisPlugin {
    fn build(&self, chart: &mut Chart) {
        chart.add_handler("price-axis", order::PANES_HANDLER + 3, PriceAxisHandler::default());
    }
}
