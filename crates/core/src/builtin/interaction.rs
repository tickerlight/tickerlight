use crate::plugin::{commands, names, order};
use crate::{Chart, Event, EventCtx, Handler, Key, Plugin, Response};

pub(crate) const WHEEL_ZOOM_PER_NOTCH: f64 = 1.15;
/// Below this speed (px/s) letting go of the chart does not start a glide.
const MIN_FLING_SPEED: f64 = 60.0;
/// The glide stops when it has slowed to this.
const STOP_SPEED: f64 = 15.0;
/// Speed decays like `exp(-DECAY * t)`.
const DECAY: f64 = 3.0;
/// How much of the newest speed sample goes into the running estimate.
const SMOOTHING: f64 = 0.5;
/// How much a key zooms.
const KEY_ZOOM: f64 = 1.25;

/// Drag to pan, wheel to zoom around the pointer, and the keys that move the view (arrows a bar, Ctrl+Up and Down
/// zoom, Ctrl+Left and Right to the first and the newest bar, Alt+R or a double click on the time axis resets).
/// Letting go of a fast drag makes the chart glide on and slow down (kinetic scroll). The speed comes from the
/// ticks that flow while dragging, so it needs no timestamps on pointer events.
#[derive(Default)]
pub struct PanZoomHandler {
    dragging: bool,
    /// Pixels dragged since the last tick.
    moved: f64,
    /// Smoothed speed in px/s, positive towards older bars.
    velocity: f64,
    gliding: bool,
}

impl Handler for PanZoomHandler {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        match *event {
            Event::PointerDown if ctx.pointer.pressed => {
                self.dragging = true;
                self.gliding = false;
                self.moved = 0.0;
                self.velocity = 0.0;
            }
            Event::PointerMoved { .. } if ctx.pointer.pressed => {
                if let (Some(before), Some(now)) = (ctx.previous_pointer.position, ctx.pointer.position) {
                    let dx = (now.0 - before.0) as f64;
                    ctx.time.pan(dx);
                    self.moved += dx;
                }
            }
            Event::PointerUp | Event::PointerLeft if self.dragging => {
                self.dragging = false;
                self.gliding = self.velocity.abs() >= MIN_FLING_SPEED;
            }
            Event::Wheel { x, delta } => {
                self.gliding = false;
                ctx.time
                    .zoom(WHEEL_ZOOM_PER_NOTCH.powf(delta as f64), x as f64, ctx.len);
            }
            // A double click on the time axis resets the view.
            Event::DoubleClick
                if ctx
                    .pointer
                    .position
                    .is_some_and(|(x, y)| x < ctx.plot.0 && y >= ctx.plot.1) =>
            {
                ctx.emit(commands::VIEW_RESET);
                return Response::CONSUMED;
            }
            Event::Tick { dt } if dt > 0.0 => {
                let dt = dt as f64;
                if self.dragging {
                    // Standing still between ticks means a speed of zero, so a pause before letting go
                    // does not fling.
                    self.velocity += SMOOTHING * (self.moved / dt - self.velocity);
                    self.moved = 0.0;
                } else if self.gliding {
                    let before = ctx.time.right;
                    ctx.time.pan(self.velocity * dt);
                    ctx.time.clamp(ctx.len);
                    self.velocity *= (-DECAY * dt).exp();
                    // Hitting the end of the data stops it dead.
                    if self.velocity.abs() < STOP_SPEED || ctx.time.right == before {
                        self.gliding = false;
                    }
                }
            }
            Event::Command(command) => {
                let len = ctx.len as f64;
                let time = &mut *ctx.time;
                match command {
                    commands::VIEW_BACK => time.right -= 1.0,
                    commands::VIEW_FORWARD => time.right += 1.0,
                    commands::VIEW_ZOOM_IN => time.zoom(KEY_ZOOM, time.width, ctx.len),
                    commands::VIEW_ZOOM_OUT => time.zoom(1.0 / KEY_ZOOM, time.width, ctx.len),
                    commands::VIEW_START => time.right = time.width / time.bar_spacing,
                    commands::VIEW_END => time.right = len + crate::chart::DEFAULT_RIGHT_MARGIN_BARS,
                    commands::VIEW_RESET => {
                        time.bar_spacing = crate::chart::DEFAULT_BAR_SPACING * ctx.scale as f64;
                        time.right = len + crate::chart::DEFAULT_RIGHT_MARGIN_BARS;
                        for pane in 0..ctx.pane_stack.len() {
                            ctx.pane_stack.set_manual_range(pane, None);
                        }
                    }
                    _ => return Response::IGNORED,
                }
                self.gliding = false;
                ctx.time.clamp(ctx.len);
                return Response::CONSUMED_REDRAW;
            }
            _ => {}
        }
        if self.dragging && ctx.pointer.pressed {
            *ctx.shape = crate::PointerShape::Grabbing;
        }
        // The chart notices the changed time scale and redraws by itself.
        Response {
            animate: self.dragging || self.gliding,
            ..Response::IGNORED
        }
    }
}

pub struct InteractionPlugin;

impl Plugin for InteractionPlugin {
    fn build(&self, chart: &mut Chart) {
        chart.add_handler(names::INTERACTION, order::INTERACTION, PanZoomHandler::default());
        chart.bind(Key::Left, commands::VIEW_BACK);
        chart.bind(Key::Right, commands::VIEW_FORWARD);
        chart.bind(Key::CtrlUp, commands::VIEW_ZOOM_IN);
        chart.bind(Key::CtrlDown, commands::VIEW_ZOOM_OUT);
        chart.bind(Key::CtrlLeft, commands::VIEW_START);
        chart.bind(Key::CtrlRight, commands::VIEW_END);
        chart.bind(Key::Alt('r'), commands::VIEW_RESET);
    }
}
