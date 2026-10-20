use std::collections::HashMap;

use crate::plugin::{names, order};
use crate::{Chart, Event, EventCtx, Handler, Key, Response};

/// Which key sends which command. Plugins add their bindings with [`Chart::bind`]; rebind or
/// unbind by inserting your own `Keymap` values through `Chart::resources_mut`.
#[derive(Default, Debug, Clone)]
pub struct Keymap {
    bindings: HashMap<Key, &'static str>,
}

impl Keymap {
    pub fn bind(&mut self, key: Key, command: &'static str) -> &mut Self {
        self.bindings.insert(key, command);
        self
    }

    pub fn unbind(&mut self, key: Key) -> &mut Self {
        self.bindings.remove(&key);
        self
    }

    /// The command of `key`; a letter is looked up in lower case, and a key with Shift that has no binding of its
    /// own as the key alone, so Shift does not change what it does. Alt with a letter keeps its case.
    pub fn command(&self, key: Key) -> Option<&'static str> {
        if let Some(&command) = self.bindings.get(&key) {
            return Some(command);
        }
        let key = match key {
            Key::Char(c) => Key::Char(c.to_lowercase().next().unwrap_or(c)),
            Key::SelectLeft => Key::Left,
            Key::SelectRight => Key::Right,
            Key::SelectHome => Key::Home,
            Key::SelectEnd => Key::End,
            _ => return None,
        };
        self.bindings.get(&key).copied()
    }
}

/// Turns bound keys into commands and swallows them.
pub struct KeymapHandler;

impl Handler for KeymapHandler {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx<'_>) -> Response {
        let Event::Key(key) = *event else {
            return Response::IGNORED;
        };
        match ctx.res.get::<Keymap>().and_then(|map| map.command(key)) {
            Some(command) => {
                ctx.emit(command);
                Response::CONSUMED
            }
            None => Response::IGNORED,
        }
    }
}

impl Chart {
    /// Binds `key` to `command`, making sure the keymap handler is installed.
    pub fn bind(&mut self, key: Key, command: &'static str) -> &mut Self {
        if !self.resources().contains::<Keymap>() {
            self.resources_mut().insert(Keymap::default());
        }
        self.resources_mut()
            .get_mut::<Keymap>()
            .expect("just inserted")
            .bind(key, command);
        if !self.handler_names().contains(&names::KEYMAP) {
            self.add_handler(names::KEYMAP, order::KEYMAP, KeymapHandler);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_installs_the_handler_once_and_rebinding_replaces() {
        let mut chart = Chart::empty(crate::Candles::new());
        chart.bind(Key::Char('x'), "one").bind(Key::Char('y'), "two");
        assert_eq!(chart.handler_names(), [names::KEYMAP]);
        chart.bind(Key::Char('x'), "three");
        let map = chart.resources().get::<Keymap>().unwrap();
        assert_eq!(map.command(Key::Char('x')), Some("three"));
        assert_eq!(map.command(Key::Char('y')), Some("two"));
        assert_eq!(map.command(Key::Char('z')), None);
        assert_eq!(map.command(Key::Char('Y')), Some("two"), "with Shift");
        chart.bind(Key::Right, "right");
        let map = chart.resources().get::<Keymap>().unwrap();
        assert_eq!(
            map.command(Key::SelectRight),
            Some("right"),
            "Shift without a binding of its own"
        );
        chart.bind(Key::SelectRight, "step").bind(Key::Alt('r'), "reset");
        let map = chart.resources().get::<Keymap>().unwrap();
        assert_eq!(map.command(Key::SelectRight), Some("step"));
        assert_eq!(map.command(Key::Alt('R')), None, "Alt with Shift is another key");
    }
}
