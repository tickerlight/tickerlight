use crate::{Chart, Event, PointerShape, Scene};

/// What a backend drives: something that takes input events and draws itself as a scene. The chart is
/// one; a program's window with panels around the chart is another.
pub trait Surface {
    /// The size in device pixels.
    fn resize(&mut self, width: f32, height: f32);
    /// Device pixels per logical pixel.
    fn set_scale(&mut self, scale: f32);
    /// Returns whether a redraw is needed.
    fn handle(&mut self, event: Event) -> bool;
    /// Whether [`Event::Tick`]s are wanted.
    fn needs_tick(&self) -> bool;
    /// Whether they are wanted every frame (something moves); otherwise a few a second do (live data coming, a
    /// save waiting).
    fn animating(&self) -> bool {
        self.needs_tick()
    }
    fn build_scene(&mut self, scene: &mut Scene);
    /// The title of the window; empty leaves it as it is.
    fn title(&self) -> String {
        String::new()
    }
    /// Text copied or cut since the last call, for the clipboard.
    fn take_copied(&mut self) -> Option<String> {
        None
    }
    /// Whether what is typed goes into text now (a field, a note): a paste is then typed in, else it is
    /// [`Key::Paste`](crate::Key::Paste) for what was copied on the chart.
    fn takes_text(&self) -> bool {
        true
    }
    /// What the mouse pointer looks like where it is now.
    fn pointer_shape(&self) -> PointerShape {
        PointerShape::Default
    }
    /// A second window the surface shows a part of itself in (a chart on another screen), if it can.
    fn apart_surface(&mut self) -> Option<&mut dyn Apart> {
        None
    }
}

/// The second window of a [`Surface`]: the backend opens it while [`Apart::apart_open`] says so and drives it as
/// it drives the surface's own window, through these. Ticks go to the surface alone; it ticks what both show.
pub trait Apart {
    /// Whether the second window is wanted now.
    fn apart_open(&self) -> bool;
    /// The user closed the second window.
    fn close_apart(&mut self);
    /// Its size in device pixels.
    fn apart_resize(&mut self, width: f32, height: f32);
    fn apart_set_scale(&mut self, scale: f32);
    /// An event in the second window; returns whether a redraw (of either window) is needed.
    fn apart_handle(&mut self, event: Event) -> bool;
    fn apart_scene(&mut self, scene: &mut Scene);
    fn apart_title(&self) -> String;
    fn apart_pointer_shape(&self) -> PointerShape;
}

impl Surface for Chart {
    fn resize(&mut self, width: f32, height: f32) {
        Chart::resize(self, width, height);
    }

    fn set_scale(&mut self, scale: f32) {
        Chart::set_scale(self, scale);
    }

    fn handle(&mut self, event: Event) -> bool {
        Chart::handle(self, event)
    }

    fn needs_tick(&self) -> bool {
        Chart::needs_tick(self)
    }

    fn animating(&self) -> bool {
        Chart::animating(self)
    }

    fn build_scene(&mut self, scene: &mut Scene) {
        Chart::build_scene(self, scene);
    }

    fn pointer_shape(&self) -> PointerShape {
        Chart::pointer_shape(self)
    }

    fn takes_text(&self) -> bool {
        Chart::takes_text(self)
    }
}
