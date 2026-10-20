use super::drawings::{Drawing, Drawings};
use super::trading::{Account, Fill};
use crate::plugin::order;
use crate::{Chart, Event, EventCtx, Handler, Plugin, Response, Time};

/// What the host application (a page, a program embedding the chart) wants to hear about. Every method has
/// an empty default: implement the ones you need and add the listener with [`Observe`].
pub trait Listener {
    /// An order was filled. Called once per fill, in order, with the account as it is after the fill.
    fn fill(&mut self, _fill: &Fill, _account: &Account) {}

    /// The pointer moved over the chart. `bar` is the time of the bar under it, `price` the price under it in
    /// the pane it is over; both `None` when not over a bar or a plot, and the call comes once more with
    /// both `None` when the pointer leaves the chart.
    fn pointer(&mut self, _bar: Option<Time>, _price: Option<f64>) {}

    /// The replay started, stopped, or moved: `shown` bars out of `total` are visible.
    fn replay(&mut self, _active: bool, _shown: usize, _total: usize) {}

    /// The kept drawings changed (one was placed, moved, or deleted; a drag is reported when it ends). [`Drawings::save`] turns them into text to
    /// keep. Not called for what was there when the observer was added.
    fn drawings(&mut self, _drawings: &Drawings) {}
}

/// Reports what happens on the chart to a [`Listener`]. It is a handler like any other, but one that sees
/// every event, so it notices the changes that other handlers consumed.
pub struct Observe<L: Listener> {
    listener: L,
    fills_told: usize,
    replay: Option<(bool, usize, usize)>,
    drawings: Option<Vec<Drawing>>,
}

impl<L: Listener> Observe<L> {
    pub fn new(listener: L) -> Self {
        Self {
            listener,
            fills_told: 0,
            replay: None,
            drawings: None,
        }
    }
}

impl<L: Listener + 'static> Plugin for Observe<L>
where
    L: Clone,
{
    fn build(&self, chart: &mut Chart) {
        chart.add_handler("observer", order::OBSERVER_HANDLER, Observe::new(self.listener.clone()));
    }
}

impl<L: Listener> Handler for Observe<L> {
    fn observes_all(&self) -> bool {
        true
    }

    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        if let Some(account) = ctx.res.get::<Account>() {
            // A fill list never shrinks; fills made before the observer was added are not told.
            let fills = account.fills();
            if self.fills_told == 0 && self.replay.is_none() {
                self.fills_told = fills.len();
            }
            for fill in &fills[self.fills_told.min(fills.len())..] {
                self.listener.fill(fill, account);
            }
            self.fills_told = fills.len();
        }
        let replay = (ctx.cursor.is_replaying(), ctx.cursor.shown(), ctx.cursor.total());
        if self.replay != Some(replay) {
            // The first look only records a chart that is not replaying: nothing has changed yet.
            if self.replay.is_some() || replay.0 {
                self.listener.replay(replay.0, replay.1, replay.2);
            }
            self.replay = Some(replay);
        }
        // While a button is held a drawing is being dragged: it is reported once it is let go.
        if let Some(drawings) = ctx.res.get::<Drawings>().filter(|_| !ctx.pointer.pressed) {
            if self.drawings.as_ref().is_some_and(|known| *known != drawings.items) {
                self.listener.drawings(drawings);
            }
            if self.drawings.as_ref() != Some(&drawings.items) {
                self.drawings = Some(drawings.items.clone());
            }
        }
        match event {
            Event::PointerMoved { x, .. } => {
                let bar = ctx.pointer_pane().and_then(|_| {
                    let i = ctx.time.index_of(*x as f64).floor();
                    (i >= 0.0).then(|| ctx.view.time().get(i as usize).copied()).flatten()
                });
                self.listener.pointer(bar, ctx.pointer_price());
            }
            Event::PointerLeft => self.listener.pointer(None, None),
            _ => {}
        }
        Response::IGNORED
    }
}
