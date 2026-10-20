//! Touch input turned into the chart's pointer events, so a finger works like the mouse does and two
//! fingers zoom. Backends feed it what their windowing layer reports and pass on the events it returns.

use crate::chart::Event;

/// Where a touch is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Start,
    Move,
    /// The finger was lifted or the system took the touch away.
    End,
}

#[derive(Debug, Clone, Copy)]
struct Finger {
    id: u64,
    x: f32,
    y: f32,
}

/// Keeps track of the fingers on the screen.
///
/// One finger is the pointer: it presses where it lands, drags, and releases where it lifts (and the
/// pointer leaves, since a lifted finger hovers nowhere). A second finger cancels that drag and the
/// pair zooms the time axis around their midpoint; whatever fingers stay after the pinch do nothing
/// until all are lifted. More than two fingers are ignored.
#[derive(Debug, Default)]
pub struct Touches {
    fingers: Vec<Finger>,
    /// Distance between the two fingers at the last event of the pinch.
    span: f32,
    /// A pinch happened and not every finger is up yet.
    pinched: bool,
}

impl Touches {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes one touch report and returns the chart events it amounts to, in order.
    pub fn handle(&mut self, id: u64, phase: Phase, x: f32, y: f32) -> Vec<Event> {
        let mut out = Vec::new();
        match phase {
            Phase::Start => {
                if self.fingers.len() >= 2 || self.fingers.iter().any(|f| f.id == id) {
                    return out;
                }
                self.fingers.push(Finger { id, x, y });
                match self.fingers.len() {
                    1 if !self.pinched => {
                        out.push(Event::PointerMoved { x, y });
                        out.push(Event::PointerDown);
                    }
                    2 => {
                        if !self.pinched {
                            // The first finger was a drag; the pinch takes over from it.
                            out.push(Event::PointerUp);
                            out.push(Event::PointerLeft);
                        }
                        self.pinched = true;
                        self.span = self.distance();
                    }
                    _ => {}
                }
            }
            Phase::Move => {
                let Some(finger) = self.fingers.iter_mut().find(|f| f.id == id) else {
                    return out;
                };
                (finger.x, finger.y) = (x, y);
                if self.pinched {
                    if self.fingers.len() == 2 {
                        let span = self.distance();
                        let (mx, my) = self.midpoint();
                        if self.span > 0.0 && span > 0.0 {
                            out.push(Event::PointerMoved { x: mx, y: my });
                            let delta = ((span / self.span) as f64).ln() / crate::builtin::WHEEL_ZOOM_PER_NOTCH.ln();
                            out.push(Event::Wheel {
                                x: mx,
                                delta: delta as f32,
                            });
                        }
                        self.span = span;
                    }
                } else {
                    out.push(Event::PointerMoved { x, y });
                }
            }
            Phase::End => {
                let Some(at) = self.fingers.iter().position(|f| f.id == id) else {
                    return out;
                };
                self.fingers.remove(at);
                if !self.pinched {
                    out.push(Event::PointerMoved { x, y });
                    out.push(Event::PointerUp);
                    out.push(Event::PointerLeft);
                } else if self.fingers.len() == 1 {
                    out.push(Event::PointerLeft);
                }
                if self.fingers.is_empty() {
                    self.pinched = false;
                }
            }
        }
        out
    }

    fn distance(&self) -> f32 {
        let [a, b] = [self.fingers[0], self.fingers[1]];
        (a.x - b.x).hypot(a.y - b.y)
    }

    fn midpoint(&self) -> (f32, f32) {
        let [a, b] = [self.fingers[0], self.fingers[1]];
        ((a.x + b.x) / 2.0, (a.y + b.y) / 2.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_finger_is_press_drag_release_and_leaves_no_hover() {
        let mut t = Touches::new();
        assert_eq!(
            t.handle(1, Phase::Start, 10.0, 20.0),
            [Event::PointerMoved { x: 10.0, y: 20.0 }, Event::PointerDown]
        );
        assert_eq!(
            t.handle(1, Phase::Move, 30.0, 20.0),
            [Event::PointerMoved { x: 30.0, y: 20.0 }]
        );
        assert_eq!(
            t.handle(1, Phase::End, 30.0, 20.0),
            [
                Event::PointerMoved { x: 30.0, y: 20.0 },
                Event::PointerUp,
                Event::PointerLeft
            ]
        );
    }

    #[test]
    fn a_second_finger_ends_the_drag_and_spreading_zooms_in_around_the_midpoint() {
        let mut t = Touches::new();
        t.handle(1, Phase::Start, 100.0, 50.0);
        assert_eq!(
            t.handle(2, Phase::Start, 200.0, 50.0),
            [Event::PointerUp, Event::PointerLeft]
        );
        let events = t.handle(2, Phase::Move, 300.0, 50.0); // span 100 -> 200
        let Event::Wheel { x, delta } = events[1] else {
            panic!("{events:?}")
        };
        assert_eq!(x, 200.0);
        assert!(delta > 0.0, "spreading zooms in");
        let events = t.handle(2, Phase::Move, 200.0, 50.0); // back to 100
        let Event::Wheel { delta: back, .. } = events[1] else {
            panic!()
        };
        assert!((back + delta).abs() < 1e-4, "the same way back undoes it");
    }

    #[test]
    fn what_is_left_after_a_pinch_does_nothing_until_everything_is_lifted() {
        let mut t = Touches::new();
        t.handle(1, Phase::Start, 100.0, 50.0);
        t.handle(2, Phase::Start, 200.0, 50.0);
        assert_eq!(t.handle(1, Phase::End, 100.0, 50.0), [Event::PointerLeft]);
        assert!(
            t.handle(2, Phase::Move, 250.0, 60.0).is_empty(),
            "no drag from the remaining finger"
        );
        assert!(t.handle(2, Phase::End, 250.0, 60.0).is_empty());
        assert_eq!(
            t.handle(3, Phase::Start, 5.0, 5.0).len(),
            2,
            "a fresh touch works again"
        );
    }

    #[test]
    fn a_third_finger_is_ignored() {
        let mut t = Touches::new();
        t.handle(1, Phase::Start, 0.0, 0.0);
        t.handle(2, Phase::Start, 10.0, 0.0);
        assert!(t.handle(3, Phase::Start, 50.0, 0.0).is_empty());
        assert!(t.handle(3, Phase::Move, 60.0, 0.0).is_empty());
        assert!(t.handle(3, Phase::End, 60.0, 0.0).is_empty());
    }
}
