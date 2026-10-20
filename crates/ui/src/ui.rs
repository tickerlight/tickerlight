use std::collections::HashMap;

use tickerlight_core::{Align, ClipRect, Color, Event, Font, Key, PointerShape, Scene};

use crate::Theme;
use crate::icons::{self, Icon};

/// The fewest rows a table shows.
const TABLE_ROWS: usize = 6;

/// Which control: the order it was laid out in, so a control is the same one from pass to pass as long as the
/// panel lays out the same controls before it.
type Id = u64;

/// A rectangle in device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Area {
    pub fn contains(&self, (x, y): (f32, f32)) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn clip(&self) -> ClipRect {
        ClipRect {
            x: self.x,
            y: self.y,
            w: self.w,
            h: self.h,
        }
    }

    fn shrink(&self, by: f32) -> Area {
        Area {
            x: self.x + by,
            y: self.y + by,
            w: (self.w - 2.0 * by).max(0.0),
            h: (self.h - 2.0 * by).max(0.0),
        }
    }
}

/// What the controls remember between passes.
#[derive(Debug, Default)]
pub struct UiState {
    /// Where the pointer is, when it is over the controls.
    pub(crate) pointer: Option<(f32, f32)>,
    /// The control the pointer went down on, until it goes up.
    pub(crate) active: Option<Id>,
    /// The text field keys go to, and where its caret is.
    pub(crate) focus: Option<Id>,
    /// The text field that had the keys when the button last went down (a press takes them away first).
    pub(crate) focus_at_press: Option<Id>,
    /// The field, by its name, to put the keys into in the next pass, as a first click would.
    pub(crate) focus_on: Option<String>,
    caret: Caret,
    /// The button went down in the focused field: dragging selects from there. A first click selected all, and a
    /// shaking hand must not undo that.
    selecting: bool,
    /// Text copied or cut, for the window to put on the clipboard.
    copied: Option<String>,
    /// The control under the pointer.
    pub(crate) hot: Option<Id>,
    /// What the pointer looks like over the control under it.
    pub(crate) shape: PointerShape,
    /// How many rows of each table are scrolled past.
    scroll: HashMap<Id, usize>,
    /// How far each column of panels is scrolled down, device pixels, by its first id.
    offsets: HashMap<Id, f32>,
    /// How many rows each counted bar (the top bar, the rows under the chart) took in the last pass: they wrap
    /// when their controls do not fit across.
    pub(crate) bar_rows: [usize; BARS],
    /// Where each control was in the last pass, by its text.
    pub(crate) rects: Vec<(String, Area)>,
    /// The tip of the control under the pointer, and where that control is.
    pub(crate) tip: Option<(String, Area)>,
    /// How far each control has lit up under the pointer, from 0 to 1 over [`FADE`] seconds, and back when the
    /// pointer leaves it.
    glow: HashMap<Id, f32>,
    /// The list a dropdown opened, if one is open.
    pub(crate) menu: Option<Menu>,
    /// Where a right press over a chart asked for a menu, until a [`Ui::context_menu`] opens it.
    pub(crate) context: Option<ContextAt>,
    /// A panel has work left and asked for another pass (see [`Ui::again`]).
    pub(crate) again: bool,
}

/// The list of a dropdown, open over everything under its button (beside it for a column's tools). A pick in it
/// waits there for the dropdown to take it, in the same pass (the list's controls go first).
#[derive(Debug, Clone)]
pub(crate) struct Menu {
    owner: Id,
    /// The dropdown's button.
    pub(crate) anchor: Area,
    pub(crate) options: Vec<String>,
    /// An icon before each option, or none.
    icons: Vec<Icon>,
    /// Opened to the right of its button rather than under it.
    pub(crate) beside: bool,
    /// The option marked as the one in use.
    selected: Option<usize>,
    pub(crate) picked: Option<usize>,
}

/// The place of a right press: in the window, and in the chart it was over (the chart's own pixels).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ContextAt {
    pub(crate) window: (f32, f32),
    pub(crate) chart: (f32, f32),
}

/// What a list to open shows.
struct List<'a> {
    current: Option<usize>,
    options: &'a [&'a str],
    icons: &'a [Icon],
    beside: bool,
}

/// How many bars keep the number of rows they took: the top bar, the player and the row under the chart.
pub(crate) const BARS: usize = 3;

/// Seconds a control takes to light up under the pointer, or to go out.
const FADE: f32 = 0.12;

impl UiState {
    /// Whether a text field takes the keys.
    pub fn has_focus(&self) -> bool {
        self.focus.is_some()
    }

    /// Scrolls the column whose controls start at `first_id` back to its top.
    pub(crate) fn scroll_to_top(&mut self, first_id: Id) {
        self.offsets.remove(&first_id);
    }

    /// Where the first control with this text was in the last pass: a button by its label, a field by its hint.
    pub fn rect_of(&self, text: &str) -> Option<Area> {
        self.rects.iter().find(|(t, _)| t == text).map(|&(_, area)| area)
    }

    /// Text copied or cut from a field since the last call.
    pub fn take_copied(&mut self) -> Option<String> {
        self.copied.take()
    }

    /// Every control of the last pass with its text, in layout order.
    pub fn rects(&self) -> &[(String, Area)] {
        &self.rects
    }

    /// The tip shown now, if any.
    pub fn tip(&self) -> Option<&str> {
        self.tip.as_ref().map(|(t, _)| t.as_str())
    }

    pub(crate) fn begin_pass(&mut self) {
        self.rects.clear();
        self.hot = None;
        self.shape = PointerShape::Default;
        self.tip = None;
        self.again = false;
    }

    /// Moves the glow of the controls `dt` seconds on: the one under the pointer toward lit, the rest toward out.
    /// Returns whether any of them changed.
    pub(crate) fn advance(&mut self, dt: f32) -> bool {
        if let Some(hot) = self.hot {
            self.glow.entry(hot).or_insert(0.0);
        }
        let (hot, step) = (self.hot, dt.max(0.0) / FADE);
        let mut moved = false;
        self.glow.retain(|&id, t| {
            let next = if Some(id) == hot {
                (*t + step).min(1.0)
            } else {
                (*t - step).max(0.0)
            };
            moved |= next != *t;
            *t = next;
            next > 0.0 || Some(id) == hot
        });
        moved
    }

    /// Whether a control is still lighting up or going out.
    pub(crate) fn fading(&self) -> bool {
        self.hot.is_some_and(|h| self.glow.get(&h).is_none_or(|&t| t < 1.0))
            || self.glow.iter().any(|(&id, &t)| Some(id) != self.hot && t > 0.0)
    }

    /// How lit a control is, eased in and out.
    fn glow(&self, id: Id) -> f32 {
        let t = self.glow.get(&id).copied().unwrap_or(0.0);
        t * t * (3.0 - 2.0 * t)
    }
}

/// The caret of the focused text field and the other end of the selection, in characters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Caret {
    at: usize,
    anchor: usize,
}

impl Caret {
    fn selection(&self) -> std::ops::Range<usize> {
        self.at.min(self.anchor)..self.at.max(self.anchor)
    }

    fn to(at: usize) -> Self {
        Self { at, anchor: at }
    }
}

/// The byte where character `i` of `text` begins.
fn byte(text: &str, i: usize) -> usize {
    text.char_indices().nth(i).map_or(text.len(), |(b, _)| b)
}

/// Does what an editing key does to a field's text and caret; copied or cut text goes to `copied`. Returns
/// whether the text changed. Copying or cutting with nothing selected takes the whole text.
fn edit(text: &mut String, caret: &mut Caret, key: Key, copied: &mut Option<String>) -> bool {
    let len = text.chars().count();
    caret.at = caret.at.min(len);
    caret.anchor = caret.anchor.min(len);
    let sel = caret.selection();
    let bytes = |text: &str, r: &std::ops::Range<usize>| byte(text, r.start)..byte(text, r.end);
    let whole = 0..len;
    match key {
        Key::Char(_) | Key::Space if !matches!(key, Key::Char(c) if c.is_control()) => {
            let c = if let Key::Char(c) = key { c } else { ' ' };
            text.replace_range(bytes(text, &sel), &c.to_string());
            *caret = Caret::to(sel.start + 1);
            true
        }
        Key::Backspace | Key::Delete if !sel.is_empty() => {
            text.replace_range(bytes(text, &sel), "");
            *caret = Caret::to(sel.start);
            true
        }
        Key::Backspace if caret.at > 0 => {
            text.replace_range(bytes(text, &(caret.at - 1..caret.at)), "");
            *caret = Caret::to(caret.at - 1);
            true
        }
        Key::Delete if caret.at < len => {
            text.replace_range(bytes(text, &(caret.at..caret.at + 1)), "");
            *caret = Caret::to(caret.at);
            true
        }
        Key::Left => {
            *caret = Caret::to(if sel.is_empty() {
                caret.at.saturating_sub(1)
            } else {
                sel.start
            });
            false
        }
        Key::Right => {
            *caret = Caret::to(if sel.is_empty() {
                (caret.at + 1).min(len)
            } else {
                sel.end
            });
            false
        }
        Key::Home => {
            *caret = Caret::to(0);
            false
        }
        Key::End => {
            *caret = Caret::to(len);
            false
        }
        Key::SelectAll => {
            *caret = Caret { at: len, anchor: 0 };
            false
        }
        // The caret moves, the other end of the selection stays.
        Key::SelectLeft => {
            caret.at = caret.at.saturating_sub(1);
            false
        }
        Key::SelectRight => {
            caret.at = (caret.at + 1).min(len);
            false
        }
        Key::SelectHome => {
            caret.at = 0;
            false
        }
        Key::SelectEnd => {
            caret.at = len;
            false
        }
        Key::Copy | Key::Cut => {
            let taken = if sel.is_empty() { whole } else { sel };
            *copied = Some(text[bytes(text, &taken)].to_string());
            if key == Key::Cut && !taken.is_empty() {
                text.replace_range(bytes(text, &taken), "");
                *caret = Caret::to(taken.start);
                return true;
            }
            false
        }
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Interaction {
    hovered: bool,
    held: bool,
    clicked: bool,
    /// How lit it is under the pointer, 0 to 1, eased: what is drawn, where `hovered` is what it does.
    glow: f32,
}

/// What a text field did in this pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextResponse {
    pub changed: bool,
    /// Enter was pressed in it.
    pub submitted: bool,
    /// Escape was pressed in it.
    pub cancelled: bool,
    /// It takes the keys.
    pub focused: bool,
    /// The click that put the keys into it came this frame.
    pub gained: bool,
    /// Up (-1) or Down (1) was pressed in it: for a list it picks from.
    pub step: i32,
}

/// A column of a [`Ui::table`]: its share of the width and how its cells are aligned.
#[derive(Debug, Clone, Copy)]
pub struct Column {
    pub title: &'static str,
    pub width: f32,
    pub align: Align,
}

/// How a table is sorted: by which column, and whether from the largest down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub column: usize,
    pub descending: bool,
}

/// What was done to a table: a row clicked, or a row's remove mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TableResponse {
    pub clicked: Option<usize>,
    pub removed: Option<usize>,
}

/// One cell of a table; `color` is the text color, the theme's text color when `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub text: String,
    pub color: Option<Color>,
}

impl Cell {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            color: None,
        }
    }

    pub fn colored(text: impl Into<String>, color: Color) -> Self {
        Self {
            text: text.into(),
            color: Some(color),
        }
    }
}

/// The controls of a row of columns: the n-th is `unit` times its weight wide (1 past the fourth), `gap` apart.
#[derive(Debug, Clone, Copy)]
struct Cells {
    unit: f32,
    weights: [f32; 4],
    next: usize,
    gap: f32,
}

impl Cells {
    fn new(unit: f32, gap: f32) -> Self {
        Self {
            unit,
            weights: [1.0; 4],
            next: 0,
            gap,
        }
    }

    /// The width of the next control.
    fn take(&mut self) -> f32 {
        let w = self.unit * self.weights.get(self.next).copied().unwrap_or(1.0);
        self.next += 1;
        w
    }
}

/// Lays out controls in one panel and makes them work.
///
/// A panel calls the same controls in the same order on every pass. On an event pass (no scene) the controls
/// react to the event: a button returns `true` when it is clicked. On a draw pass they draw themselves. Controls
/// go top to bottom, each the width of the panel, or left to right inside [`horizontal`](Self::horizontal) and
/// [`columns`](Self::columns) (and everywhere in a bar).
pub struct Ui<'a> {
    state: &'a mut UiState,
    event: Option<&'a Event>,
    scene: Option<&'a mut Scene>,
    theme: &'a Theme,
    scale: f32,
    area: Area,
    inner: Area,
    first_id: Id,
    /// How far a column is scrolled down.
    offset: f32,
    next_id: Id,
    x: f32,
    y: f32,
    horizontal: bool,
    /// A bar: controls that do not fit across go on the next row, and plain buttons are only their text until the
    /// pointer is on them.
    pub(crate) bar: bool,
    /// Which of the bars whose rows are kept for the layout this is, if one.
    pub(crate) counted: Option<usize>,
    /// The widths of the controls in a row of columns, and the gap between them.
    cell: Option<Cells>,
    /// Where the last control went.
    last: Area,
    consumed: bool,
    changed: bool,
}

impl<'a> Ui<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        state: &'a mut UiState,
        event: Option<&'a Event>,
        scene: Option<&'a mut Scene>,
        theme: &'a Theme,
        scale: f32,
        area: Area,
        horizontal: bool,
        first_id: Id,
    ) -> Self {
        let inner = area.shrink(theme.padding * scale);
        let offset = if horizontal {
            0.0
        } else {
            state.offsets.get(&first_id).copied().unwrap_or(0.0)
        };
        Self {
            state,
            event,
            scene,
            theme,
            scale,
            area,
            inner,
            first_id,
            offset,
            next_id: first_id,
            x: inner.x,
            y: inner.y - offset,
            horizontal,
            bar: false,
            counted: None,
            cell: None,
            last: Area::default(),
            consumed: false,
            changed: false,
        }
    }

    /// Whether a text field takes the keys.
    pub fn typing(&self) -> bool {
        self.state.has_focus()
    }

    /// Whether the event was for the controls, and whether a control acted (so what it acts on changed). A
    /// column taller than its area scrolls with the wheel and shows how far it is scrolled.
    pub(crate) fn finish(mut self) -> (bool, bool) {
        if self.bar {
            let Some(k) = self.counted else {
                return (self.consumed || self.changed, self.changed);
            };
            self.state.bar_rows[k] = ((self.y - self.inner.y) / (self.row_height() + self.gap())).round() as usize + 1;
        } else {
            let content = self.y + self.offset - self.inner.y;
            let max = (content - self.inner.h).max(0.0);
            let mut offset = self.offset;
            if let Some(&Event::Wheel { delta, .. }) = self.event
                && !self.consumed
                && self.state.pointer.is_some_and(|p| self.area.contains(p))
            {
                offset -= delta * self.row_height() * 2.0;
                self.consumed = max > 0.0;
            }
            offset = offset.clamp(0.0, max);
            self.state.offsets.insert(self.first_id, offset);
            let (w, area, color) = (self.px(3.0), self.area, self.theme.border);
            if let Some(scene) = &mut self.scene
                && max > 0.0
            {
                let h = area.h * area.h / (area.h + max);
                let y = area.y + (area.h - h) * offset / max;
                scene.fill_rounded(area.x + area.w - w * 4.0 / 3.0, y, w, h, w / 2.0, color);
            }
        }
        (self.consumed || self.changed, self.changed)
    }

    /// A tip for the control just laid out (what it does, its key), shown by it while the pointer is over it and no
    /// dropdown list is open (it would cover the list).
    pub fn tip(&mut self, text: &str) {
        let shown = self.state.active.is_none()
            && self.state.menu.is_none()
            && self
                .state
                .pointer
                .is_some_and(|p| self.last.contains(p) && self.area.contains(p));
        if shown {
            self.state.tip = Some((text.to_string(), self.last));
        }
    }

    /// Asks for another pass soon: the panel has work left that it does a part of in each pass.
    pub fn again(&mut self) {
        self.state.again = true;
    }

    pub fn theme(&self) -> &Theme {
        self.theme
    }

    /// Device pixels of `logical` logical pixels.
    pub fn px(&self, logical: f32) -> f32 {
        logical * self.scale
    }

    /// Whether this pass draws (else it handles an event).
    pub fn drawing(&self) -> bool {
        self.scene.is_some()
    }

    /// Controls added by `add` go left to right, each as wide as its content.
    pub fn horizontal(&mut self, add: impl FnOnce(&mut Self)) {
        self.row(None, add);
    }

    /// The next `n` controls added by `add` share the row equally, a padding apart.
    pub fn columns(&mut self, n: usize, add: impl FnOnce(&mut Self)) {
        let gap = self.px(self.theme.padding);
        let n = n.max(1) as f32;
        let unit = (self.inner.w - gap * (n - 1.0)) / n;
        self.row(Some(Cells::new(unit, gap)), add);
    }

    /// The next controls added by `add` (at most four) share the row in proportion to `weights`, a padding apart.
    pub fn columns_weighted(&mut self, weights: &[f32], add: impl FnOnce(&mut Self)) {
        debug_assert!((1..=4).contains(&weights.len()));
        let gap = self.px(self.theme.padding);
        let n = weights.len().max(1) as f32;
        let unit = (self.inner.w - gap * (n - 1.0)) / weights.iter().sum::<f32>();
        let mut cells = Cells::new(unit, gap);
        for (w, weight) in cells.weights.iter_mut().zip(weights) {
            *w = *weight;
        }
        self.row(Some(cells), add);
    }

    fn row(&mut self, cell: Option<Cells>, add: impl FnOnce(&mut Self)) {
        let saved = (self.horizontal, self.bar, self.cell, self.x);
        (self.horizontal, self.bar, self.cell, self.x) = (true, false, cell, self.inner.x);
        add(self);
        (self.horizontal, self.bar, self.cell, self.x) = saved;
        if !self.horizontal {
            self.y += self.row_height() + self.gap();
        }
    }

    /// Empty space of `logical` pixels: across in a row, down in a column.
    pub fn space(&mut self, logical: f32) {
        if self.horizontal {
            self.x += self.px(logical);
        } else {
            self.y += self.px(logical);
        }
    }

    /// In a row, moves on so that buttons with these labels end at the row's right edge (on the next row of a bar
    /// when they do not fit on this one).
    pub fn to_right(&mut self, labels: &[&str]) {
        let pad = self.px(self.theme.padding);
        let gaps = self.gap() * labels.len().saturating_sub(1) as f32;
        let w: f32 = labels.iter().map(|t| self.text_width(t) + 2.0 * pad).sum::<f32>() + gaps;
        let right = self.inner.x + self.inner.w;
        if self.bar && self.x > self.inner.x && self.x + w > right {
            self.y += self.row_height() + self.gap();
        }
        self.x = right - w;
    }

    pub fn label(&mut self, text: &str) {
        let color = self.theme.text;
        self.label_colored(text, color);
    }

    /// A label in the color of a heading or a hint.
    pub fn heading(&mut self, text: &str) {
        let color = self.theme.muted;
        self.label_colored(text, color);
    }

    /// Text too wide for a column goes on as many rows as it needs; in a row it is cut short with "…".
    pub fn label_colored(&mut self, text: &str, color: Color) {
        let size = self.font_size();
        let lines = if self.horizontal {
            let mut w = self.text_width(text);
            if !self.bar && self.cell.is_none() {
                w = w.min((self.inner.x + self.inner.w - self.x).max(0.0));
            }
            let area = self.alloc(w);
            vec![(area, self.shorten(text, area.w))]
        } else {
            self.wrap(text, self.inner.w)
                .into_iter()
                .map(|line| (self.alloc(self.inner.w), line))
                .collect()
        };
        self.record(text, lines[0].0);
        if let Some(scene) = &mut self.scene {
            for (area, line) in lines {
                scene.text(area.x, area.y + area.h / 2.0, size, color, Align::Left, &line);
            }
        }
    }

    /// A line of text on a backdrop of its own, readable over the chart: what the program says over the plot.
    /// Too long a line is cut short with "…".
    pub fn notice(&mut self, text: &str, color: Color) {
        let pad = self.px(self.theme.padding);
        let room = (self.inner.x + self.inner.w - self.x).max(0.0);
        let area = self.alloc((self.text_width(text) + 2.0 * pad).min(room));
        self.record(text, area);
        let line = self.shorten(text, (area.w - 2.0 * pad).max(0.0));
        let (size, theme) = (self.font_size(), *self.theme);
        let (radius, ring) = (self.px(theme.radius), self.px(1.0).max(1.0));
        if let Some(scene) = &mut self.scene {
            scene.fill_rounded(area.x, area.y, area.w, area.h, radius, theme.background);
            scene.stroke_rounded(area.x, area.y, area.w, area.h, radius, ring, theme.border);
            scene.text(area.x + pad, area.y + area.h / 2.0, size, color, Align::Left, &line);
        }
    }

    fn wrap(&self, text: &str, max: f32) -> Vec<String> {
        wrap(text, max, self.font_size())
    }

    /// `text`, or as much of it as fits in `max` followed by "…".
    fn shorten(&self, text: &str, max: f32) -> String {
        if self.text_width(text) <= max {
            return text.to_string();
        }
        let mut cut = text.to_string();
        while cut.pop().is_some() {
            let shown = format!("{}…", cut.trim_end());
            if self.text_width(&shown) <= max {
                return shown;
            }
        }
        String::new()
    }

    /// A row of a name on the left and its value on the right. In a cell too narrow for both the name is cut
    /// short, so they never run into each other.
    pub fn value(&mut self, name: &str, value: &str, color: Option<Color>) {
        let (pad, value_w) = (self.px(self.theme.padding), self.text_width(value));
        let area = self.alloc(self.text_width(name) + pad + value_w);
        self.record(name, area);
        let (size, muted, color) = (self.font_size(), self.theme.muted, color.unwrap_or(self.theme.text));
        if self.scene.is_some() {
            let shown = self.shorten(name, (area.w - value_w - pad).max(0.0));
            let y = area.y + area.h / 2.0;
            if let Some(scene) = &mut self.scene {
                scene.text(area.x, y, size, muted, Align::Left, shown);
                scene.text(area.x + area.w, y, size, color, Align::Right, value);
            }
        }
    }

    /// The title of a part of a panel: small capitals in the muted color, set off from the part before by space
    /// (unless it is the first thing in the column).
    pub fn section(&mut self, title: &str) {
        if self.y > self.inner.y - self.offset + 1.0 {
            self.space(2.0 * self.theme.spacing);
        }
        let area = self.alloc(self.inner.w);
        self.record(title, area);
        let (size, color) = (self.font_size() * 0.85, self.theme.muted);
        if let Some(scene) = &mut self.scene {
            let y = area.y + area.h * 0.6;
            scene.text(area.x, y, size, color, Align::Left, title.to_uppercase());
        }
    }

    /// Space that sets groups apart: in a row, between groups of controls; in a column, a thin line across.
    pub fn separator(&mut self) {
        if self.horizontal {
            self.x += self.px(self.theme.spacing);
            return;
        }
        let y = self.y + self.px(self.theme.spacing);
        let (x, w, line, color) = (self.inner.x, self.inner.w, self.px(1.0).max(1.0), self.theme.control);
        if let Some(scene) = &mut self.scene {
            scene.fill(x, y, w, line, color);
        }
        self.y += 2.0 * self.px(self.theme.spacing) + line;
    }

    pub fn button(&mut self, text: &str) -> bool {
        self.button_with(text, None, false, true)
    }

    /// A button that does nothing and looks off when `enabled` is false.
    pub fn button_if(&mut self, text: &str, enabled: bool) -> bool {
        self.button_with(text, None, false, enabled)
    }

    /// A button filled with `color`, such as buy and sell.
    pub fn button_colored(&mut self, text: &str, color: Color) -> bool {
        self.button_with(text, Some(color), false, true)
    }

    /// A button lit while `on`. Returns whether it was clicked.
    pub fn toggle(&mut self, text: &str, on: bool) -> bool {
        self.button_with(text, None, on, true)
    }

    fn button_with(&mut self, text: &str, fill: Option<Color>, on: bool, enabled: bool) -> bool {
        let id = self.id();
        let w = self.text_width(text) + 2.0 * self.px(self.theme.padding);
        let area = self.alloc(w);
        self.record(text, area);
        let i = if enabled {
            self.interact(id, area)
        } else {
            Interaction::default()
        };
        if i.clicked {
            self.changed = true;
        }
        let theme = *self.theme;
        let (radius, size) = (self.px(theme.radius), self.font_size());
        // In the bar a plain button is only its text until the pointer is on it. A toggle that is on takes the
        // accent's tint, in the bar and in a panel alike: only a filled control (buy, sell) is loud.
        let ghost = self.bar && fill.is_none();
        let (base, ink) = match (fill, on) {
            (Some(color), _) => (color, theme.on_fill),
            (None, true) => (Color { a: 46, ..theme.accent }, theme.accent_text),
            (None, false) if ghost => (
                Color {
                    a: (theme.control.a as f32 * i.glow) as u8,
                    ..theme.control
                },
                theme.text,
            ),
            (None, false) => (theme.control, theme.text),
        };
        let ink = if enabled { ink } else { theme.muted };
        if let Some(scene) = &mut self.scene {
            let color = if enabled {
                shade(base, i, theme.lit)
            } else {
                Color { a: base.a / 2, ..base }
            };
            if color.a > 0 {
                scene.fill_rounded(area.x, area.y, area.w, area.h, radius, color);
            }
            let (cx, cy) = area.center();
            scene.text(cx, cy, size, ink, Align::Center, text);
        }
        i.clicked
    }

    /// A track across the rest of the row, for a span of time as fractions of it from 0 to 1: what is past `at`
    /// is dark (not shown yet), `played` is lit, `at` has a knob. The pointer over it or dragging on it shows
    /// a line there with `label` of that fraction as the tip. Returns the fraction where a click or a drag on it
    /// ended. `name` is what tests find it by.
    pub fn timeline(
        &mut self,
        name: &str,
        at: Option<f32>,
        played: Option<(f32, f32)>,
        label: impl Fn(f32) -> String,
    ) -> Option<f32> {
        let id = self.id();
        let rest = (self.inner.x + self.inner.w - self.x).max(self.px(40.0));
        let area = self.alloc(rest);
        self.record(name, area);
        let knob = self.px(5.0);
        // The ends of the track leave room for the knob.
        let (left, width) = (area.x + knob, (area.w - 2.0 * knob).max(1.0));
        let fraction = |x: f32| ((x - left) / width).clamp(0.0, 1.0);
        let over = self.state.pointer.is_some_and(|p| area.contains(p));
        let dragging = self.state.active == Some(id);
        if over && self.state.active.is_none_or(|a| a == id) {
            self.state.hot = Some(id);
            self.state.shape = PointerShape::Hand;
        }
        let mut released = None;
        match self.event {
            Some(Event::PointerDown) if over && self.state.active.is_none() => {
                self.state.active = Some(id);
                self.consumed = true;
            }
            Some(Event::PointerUp) if dragging => {
                released = self.state.pointer.map(|p| fraction(p.0));
                self.consumed = true;
                self.changed = true;
            }
            // The line follows the pointer.
            Some(Event::PointerMoved { .. }) if over || dragging => self.consumed = true,
            _ => {}
        }
        let pointed = self.state.pointer.filter(|_| over || dragging).map(|p| fraction(p.0));
        if let Some(f) = pointed
            && self.drawing()
        {
            let x = left + width * f;
            self.state.tip = Some((
                label(f),
                Area {
                    x,
                    y: area.y,
                    w: 1.0,
                    h: area.h,
                },
            ));
        }
        let theme = *self.theme;
        let Some(scene) = &mut self.scene else {
            return released;
        };
        let h = self.scale * 6.0;
        let y = area.y + (area.h - h) / 2.0;
        let x_of = |f: f32| left + width * f.clamp(0.0, 1.0);
        let hover = Interaction {
            hovered: over || dragging,
            glow: if dragging { 1.0 } else { self.state.glow(id) },
            held: dragging,
            clicked: false,
        };
        scene.fill_rounded(left, y, width, h, h / 2.0, shade(theme.control_hover, hover, theme.lit));
        if let Some(at) = at {
            let x = x_of(at);
            scene.fill_rounded(x, y, left + width - x, h, h / 2.0, theme.field);
            if let Some((from, to)) = played {
                let (a, b) = (x_of(from), x_of(to));
                scene.fill_rounded(a, y, (b - a).max(h), h, h / 2.0, Color { a: 150, ..theme.accent });
            }
            scene.circle((x, y + h / 2.0), knob, theme.accent);
        }
        if let Some(f) = pointed {
            let line = self.scale.max(1.0) * 2.0;
            let x = x_of(f);
            scene.fill(
                x - line / 2.0,
                area.y + area.h * 0.15,
                line,
                area.h * 0.7,
                theme.accent_text,
            );
        }
        released
    }

    /// A square button showing `icon`, lit while `on`; `name` is its tip's start and what tests find it by. In a
    /// column it is as wide as the column, in a row as tall as the row.
    pub fn icon_button(&mut self, icon: Icon, name: &str, on: bool) -> bool {
        let id = self.id();
        let area = self.alloc_square();
        self.record(name, area);
        let i = self.interact(id, area);
        if i.clicked {
            self.changed = true;
        }
        self.draw_icon_button(icon, area, on, i);
        i.clicked
    }

    fn draw_icon_button(&mut self, icon: Icon, area: Area, on: bool, i: Interaction) {
        let theme = *self.theme;
        let radius = self.px(theme.radius + 1.0);
        if let Some(scene) = &mut self.scene {
            let (fill, ink) = if on {
                (Some(Color { a: 46, ..theme.accent }), theme.accent_text)
            } else if i.glow > 0.0 {
                (Some(faded(theme.control_hover, i.glow)), theme.text)
            } else {
                (None, theme.text)
            };
            if let Some(fill) = fill {
                let fill = shade(fill, i, theme.lit);
                scene.fill_rounded(area.x, area.y, area.w, area.h, radius, fill);
            }
            icons::draw(scene, icon, area, ink, theme.up, theme.down, self.scale);
        }
    }

    /// A square of `color` to pick it with, ringed while `on`.
    pub fn swatch(&mut self, color: Color, name: &str, on: bool) -> bool {
        let id = self.id();
        let area = self.alloc_square();
        self.record(name, area);
        let i = self.interact(id, area);
        if i.clicked {
            self.changed = true;
        }
        let theme = *self.theme;
        let (inset, ring, radius) = (self.px(5.0), self.px(1.5).max(1.0), self.px(theme.radius));
        if let Some(scene) = &mut self.scene {
            if i.glow > 0.0 {
                let fill = faded(theme.control_hover, i.glow);
                scene.fill_rounded(area.x, area.y, area.w, area.h, radius, fill);
            }
            if on {
                scene.stroke_rounded(area.x, area.y, area.w, area.h, radius + ring, ring, theme.text);
            }
            let s = area.shrink(inset);
            // As the chart draws it.
            scene.fill_rounded(s.x, s.y, s.w, s.h, radius, theme.look.paint(color));
        }
        i.clicked
    }

    /// One of `icons` as a row of square segments with the selected one lit. Returns whether the selection changed.
    pub fn icon_choice(&mut self, selected: &mut usize, icons: &[Icon], names: &[&str]) -> bool {
        let before = *selected;
        for (i, (&icon, name)) in icons.iter().zip(names).enumerate() {
            if self.icon_button(icon, name, *selected == i) {
                *selected = i;
            }
        }
        *selected != before
    }

    /// Room for a square control.
    fn alloc_square(&mut self) -> Area {
        if self.horizontal {
            let side = self.row_height();
            return self.alloc(side);
        }
        let side = self.inner.w;
        let area = Area {
            x: self.inner.x,
            y: self.y,
            w: side,
            h: side,
        };
        self.y += side + self.px(2.0);
        self.last = area;
        area
    }

    /// One of `options` as a row of segments with the selected one lit. Returns whether the selection changed.
    pub fn choice(&mut self, selected: &mut usize, options: &[&str]) -> bool {
        self.choice_with(selected, options, None)
    }

    /// Like [`choice`](Self::choice), the selected segment filled with its own color: buy and sell.
    pub fn choice_colored(&mut self, selected: &mut usize, options: &[&str], colors: &[Color]) -> bool {
        self.choice_with(selected, options, Some(colors))
    }

    fn choice_with(&mut self, selected: &mut usize, options: &[&str], colors: Option<&[Color]>) -> bool {
        let pad = 2.0 * self.px(self.theme.padding);
        let widths: Vec<f32> = options.iter().map(|o| self.text_width(o) + pad).collect();
        let area = self.alloc(widths.iter().sum());
        // A full-width (or fixed-width) choice spreads its segments equally.
        let total: f32 = widths.iter().sum();
        let equal = (area.w - total).abs() > 0.5;
        let theme = *self.theme;
        let (radius, size) = (self.px(theme.radius), self.font_size());
        // In the bar the segments are text, the selected one tinted like a toggle; in a panel they sit in a track
        // and the selected one is raised.
        let bar = self.bar;
        let inset = if bar || colors.is_some() { 0.0 } else { self.px(2.0) };
        if let Some(scene) = &mut self.scene
            && !bar
        {
            scene.fill_rounded(area.x, area.y, area.w, area.h, radius, theme.control);
        }
        let before = *selected;
        let mut x = area.x;
        for (i, (option, &w)) in options.iter().zip(&widths).enumerate() {
            let w = if equal { area.w / options.len() as f32 } else { w };
            let segment = Area { x, w, ..area };
            x += w;
            let id = self.id();
            self.record(option, segment);
            let response = self.interact(id, segment);
            if response.clicked {
                *selected = i;
                self.changed = true;
            }
            if let Some(scene) = &mut self.scene {
                let lit = *selected == i;
                let (fill, ink) = match (lit, colors.and_then(|c| c.get(i))) {
                    (true, Some(&color)) => (Some(color), theme.on_fill),
                    (true, None) if bar => (Some(Color { a: 46, ..theme.accent }), theme.accent_text),
                    (true, None) => (Some(theme.control_hover), theme.text),
                    (false, _) if response.glow > 0.0 => (Some(faded(theme.control_hover, response.glow)), theme.text),
                    (false, _) => (None, theme.muted),
                };
                if let Some(fill) = fill {
                    let s = segment.shrink(inset);
                    scene.fill_rounded(s.x, s.y, s.w, s.h, radius, shade(fill, response, theme.lit));
                }
                let (cx, cy) = segment.center();
                scene.text(cx, cy, size, ink, Align::Center, *option);
            }
        }
        *selected != before
    }

    /// One of `options` as a button showing the selected one: a click opens the list of them under it, over
    /// everything, and a pick there (or a click outside it, or Escape) closes it. For a choice of many. Returns
    /// whether the selection changed.
    pub fn dropdown(&mut self, selected: &mut usize, options: &[&str]) -> bool {
        let label = options.get(*selected).copied().unwrap_or_default();
        match self.menu_button(label, Some(*selected), options) {
            Some(pick) if pick != *selected => {
                *selected = pick;
                true
            }
            _ => false,
        }
    }

    /// A button named `label` that opens the list of `options` under it, `current` marked in it. Returns the
    /// option picked.
    pub fn menu_button(&mut self, label: &str, current: Option<usize>, options: &[&str]) -> Option<usize> {
        self.menu(label, current, options, false)
    }

    /// Like [`menu_button`](Self::menu_button), lit like a toggle that is on while one of its options is the
    /// current one: a choice of the bar kept in a menu.
    pub fn menu_choice(&mut self, label: &str, current: Option<usize>, options: &[&str]) -> Option<usize> {
        self.menu(label, current, options, current.is_some())
    }

    fn menu(&mut self, label: &str, current: Option<usize>, options: &[&str], lit: bool) -> Option<usize> {
        let id = self.id();
        let theme = *self.theme;
        let chevron = self.px(8.0);
        let pad = self.px(theme.padding);
        // As wide as the widest option, so the bar does not move when another is picked.
        let widest = options
            .iter()
            .map(|o| self.text_width(o))
            .fold(self.text_width(label), f32::max);
        let area = self.alloc(widest + 3.0 * pad + chevron);
        self.record(label, area);
        let i = self.interact(id, area);
        let list = List {
            current,
            options,
            icons: &[],
            beside: false,
        };
        let (open, picked) = self.menu_of(id, area, i.clicked, list);
        if let Some(scene) = &mut self.scene {
            let radius = self.scale * theme.radius;
            let fill = match open || lit {
                true => Color { a: 46, ..theme.accent },
                false if self.bar => faded(theme.control, i.glow),
                false => theme.control,
            };
            if fill.a > 0 {
                scene.fill_rounded(area.x, area.y, area.w, area.h, radius, shade(fill, i, theme.lit));
            }
            let ink = if open || lit { theme.accent_text } else { theme.text };
            let size = self.scale * theme.font_size;
            let (_, cy) = area.center();
            scene.text(area.x + pad, cy, size, ink, Align::Left, label);
            // A chevron pointing down, up while the list is open.
            let (cx, half, rise) = (area.x + area.w - pad - chevron / 2.0, chevron / 2.0, chevron / 4.0);
            let (tip, ends) = if open {
                (cy - rise, cy + rise)
            } else {
                (cy + rise, cy - rise)
            };
            let line = self.scale * 1.5;
            scene.line((cx - half, ends), (cx, tip), line, ink);
            scene.line((cx, tip), (cx + half, ends), line, ink);
        }
        picked
    }

    /// An icon button that opens the list of `options` under it, each with its icon of `icons` (as many, or none),
    /// `current` marked in it. Returns the option picked.
    pub fn icon_menu(
        &mut self,
        icon: Icon,
        name: &str,
        current: Option<usize>,
        options: &[&str],
        icons: &[Icon],
    ) -> Option<usize> {
        let id = self.id();
        let area = self.alloc_square();
        self.record(name, area);
        let i = self.interact(id, area);
        let list = List {
            current,
            options,
            icons,
            beside: false,
        };
        let (open, picked) = self.menu_of(id, area, i.clicked, list);
        self.draw_icon_button(icon, area, open, i);
        picked
    }

    /// The button of a group of tools in a column: `tools[shown]` is the tool it shows, lit while `on`. A strip at
    /// its right edge, marked by a small corner and recorded as `more`, opens the list of the group's tools (at
    /// most eight, `current` marked) beside it. Returns whether the button was clicked and the tool picked in the
    /// list.
    pub fn tool_group(
        &mut self,
        tools: &[(Icon, &str)],
        shown: usize,
        on: bool,
        more: &str,
        current: Option<usize>,
    ) -> (bool, Option<usize>) {
        let (icon, name) = tools[shown];
        let (mut options, mut icons) = ([""; 8], [Icon::Cursor; 8]);
        let n = tools.len().min(8);
        for (k, &(icon, name)) in tools[..n].iter().enumerate() {
            (options[k], icons[k]) = (name, icon);
        }
        let (id, more_id) = (self.id(), self.id());
        let area = self.alloc_square();
        let strip = self.px(10.0);
        let main = Area {
            w: area.w - strip,
            ..area
        };
        let side = Area {
            x: main.x + main.w,
            w: strip,
            ..area
        };
        self.record(name, main);
        self.record(more, side);
        let i = self.interact(id, main);
        let s = self.interact(more_id, side);
        if i.clicked {
            self.changed = true;
        }
        let list = List {
            current,
            options: &options[..n],
            icons: &icons[..n],
            beside: true,
        };
        let (open, picked) = self.menu_of(more_id, area, s.clicked, list);
        let lit = Interaction {
            glow: i.glow.max(s.glow),
            ..i
        };
        self.draw_icon_button(icon, area, on || open, lit);
        let ink = if s.hovered || open {
            self.theme.text
        } else {
            self.theme.muted
        };
        let (x, y, d) = (
            area.x + area.w - self.px(3.0),
            area.y + area.h - self.px(3.0),
            self.px(4.0),
        );
        if let Some(scene) = &mut self.scene {
            // A small corner at the bottom right: there is more here.
            for k in 0..(d as i32) {
                let k = k as f32;
                scene.fill(x - d + k, y - k - 1.0, d - k, 1.0, ink);
            }
        }
        (i.clicked, picked)
    }

    /// Opens and closes the list of the control `id` at `area`, clicked or not. Returns whether it is open and
    /// the option picked in it.
    fn menu_of(&mut self, id: u64, area: Area, clicked: bool, list: List) -> (bool, Option<usize>) {
        let mut open = false;
        let mut picked = None;
        if let Some(menu) = self.state.menu.as_mut().filter(|m| m.owner == id) {
            match menu.picked {
                Some(pick) => {
                    picked = Some(pick);
                    self.state.menu = None;
                    self.changed = true;
                }
                // The bar may have wrapped since it opened.
                None => (menu.anchor, open) = (area, true),
            }
        }
        if clicked {
            self.changed = true;
            open = !open;
            self.state.menu = open.then(|| Menu {
                owner: id,
                anchor: area,
                options: list.options.iter().map(|o| o.to_string()).collect(),
                icons: list.icons.to_vec(),
                beside: list.beside,
                selected: list.current,
                picked: None,
            });
        }
        (open, picked)
    }

    /// Puts `text` on the clipboard (the window takes it after the pass).
    pub fn copy(&mut self, text: String) {
        self.state.copied = Some(text);
    }

    /// Puts the keys into the field with the hint `name` the next time it is laid out, its text selected.
    pub fn focus(&mut self, name: &str) {
        self.state.focus_on = Some(name.to_string());
        self.again();
    }

    /// Where in the chart (its own pixels) a right press asked for a menu in this pass, before
    /// [`Ui::context_menu`] opens it: what the menu offers may depend on what is there.
    pub fn context_at(&self) -> Option<(f32, f32)> {
        self.state.context.map(|c| c.chart)
    }

    /// The menu a right press over the chart opens where it was: `options`, `current` marked (none opens no menu).
    /// It has no place of its own; call it in every pass, the same way. Returns the option picked.
    pub fn context_menu(&mut self, options: &[&str], current: Option<usize>) -> Option<usize> {
        let id = self.id();
        let mut picked = None;
        if let Some(menu) = self.state.menu.as_ref().filter(|m| m.owner == id)
            && let Some(pick) = menu.picked
        {
            picked = Some(pick);
            self.state.menu = None;
            self.changed = true;
        }
        if let Some(at) = self.state.context.take() {
            self.changed = true;
            self.state.menu = (!options.is_empty()).then(|| Menu {
                owner: id,
                anchor: Area {
                    x: at.window.0,
                    y: at.window.1,
                    w: 0.0,
                    h: 0.0,
                },
                options: options.iter().map(|o| o.to_string()).collect(),
                icons: Vec::new(),
                beside: true,
                selected: current,
                picked: None,
            });
        }
        picked
    }

    /// The options of the open dropdown list, one under another: a click picks one for the dropdown.
    pub(crate) fn menu_list(&mut self) {
        let Some(menu) = self.state.menu.clone() else {
            return;
        };
        let theme = *self.theme;
        let (radius, size, pad) = (self.px(theme.radius), self.font_size(), self.px(theme.padding));
        // With icons each row begins with one, a row's height square.
        let lead = if menu.icons.is_empty() {
            0.0
        } else {
            self.px(theme.row_height)
        };
        for (n, option) in menu.options.iter().enumerate() {
            // After a tab, the key that does the same, at the row's end.
            let (label, keys) = option.split_once('\t').unwrap_or((option, ""));
            let id = self.id();
            let area = self.alloc(0.0);
            self.record(label, area);
            let i = self.interact(id, area);
            if i.clicked {
                self.changed = true;
                if let Some(menu) = &mut self.state.menu {
                    menu.picked = Some(n);
                }
            }
            if let Some(scene) = &mut self.scene {
                let (fill, ink) = if Some(n) == menu.selected {
                    (Color { a: 46, ..theme.accent }, theme.accent_text)
                } else {
                    (faded(theme.control_hover, i.glow), theme.text)
                };
                if fill.a > 0 {
                    scene.fill_rounded(area.x, area.y, area.w, area.h, radius, shade(fill, i, theme.lit));
                }
                if let Some(&icon) = menu.icons.get(n) {
                    let square = Area { w: area.h, ..area };
                    icons::draw(scene, icon, square, ink, theme.up, theme.down, self.scale);
                }
                let y = area.center().1;
                scene.text(area.x + lead + pad, y, size, ink, Align::Left, label);
                if !keys.is_empty() {
                    scene.text(area.x + area.w - pad, y, size, theme.muted, Align::Right, keys);
                }
            }
        }
        // No gap after the last: the box fits the list, with nothing to scroll.
        self.y -= self.gap();
    }

    /// How big the open list's box is, padding in: as wide as its widest option or its button, a row each.
    pub(crate) fn menu_size(menu: &Menu, theme: &Theme, scale: f32) -> (f32, f32) {
        let size = theme.font_size * scale;
        // A label and its keys a wide gap apart.
        let widest = menu
            .options
            .iter()
            .map(|o| match o.split_once('\t') {
                Some((label, keys)) => Font::ui().measure(label, size) + 3.0 * size + Font::ui().measure(keys, size),
                None => Font::ui().measure(o, size),
            })
            .fold(0.0, f32::max);
        let pad = theme.padding * scale;
        let n = menu.options.len() as f32;
        let lead = if menu.icons.is_empty() {
            0.0
        } else {
            theme.row_height * scale
        };
        let w = (widest + lead + 4.0 * pad).max(menu.anchor.w + 2.0 * pad);
        let h = n * theme.row_height * scale + (n - 1.0).max(0.0) * theme.spacing * scale + 2.0 * pad;
        (w, h)
    }

    /// A line of text to type in, `width` logical pixels wide in a row (the whole width in a column). The first
    /// click puts the keys into it with all of the text selected, so typing replaces it; a click in it then puts
    /// the caret where it was clicked. Enter submits, Escape or a click elsewhere leaves it. `hint` shows while it
    /// is empty and names it.
    pub fn text_field(&mut self, text: &mut String, width: f32, hint: &str) -> TextResponse {
        let id = self.id();
        let w = self.px(width);
        let area = self.alloc(w);
        self.record(hint, area);
        let i = self.interact(id, area);
        if i.hovered {
            self.state.shape = PointerShape::Text;
        }
        let theme = *self.theme;
        let (size, pad, line) = (self.font_size(), self.px(theme.padding), self.px(1.0).max(1.0));
        let room = (area.w - 2.0 * pad - 2.0 * line).max(0.0);
        let font = Font::ui();
        let width_to = |text: &str, i: usize| font.measure(&text[..byte(text, i)], size);
        // While typing, the text slides so that the caret stays in the field.
        let shift = |text: &str, caret: &Caret| (width_to(text, caret.at) - room).max(0.0);
        // The boundary between characters nearest the pointer.
        let under = |text: &str, px: f32, shifted: f32| {
            let x = px - area.x - pad + shifted;
            (0..=text.chars().count())
                .min_by(|&a, &b| (width_to(text, a) - x).abs().total_cmp(&(width_to(text, b) - x).abs()))
                .unwrap_or(0)
        };
        let mut gained = false;
        // Asked for by its name (a key typed over the chart): the keys come here with all of the text selected.
        if self.state.focus_on.as_deref() == Some(hint) {
            self.state.focus_on = None;
            self.state.focus = Some(id);
            self.state.caret = Caret {
                at: text.chars().count(),
                anchor: 0,
            };
            gained = true;
        }
        if i.hovered
            && self.event == Some(&Event::PointerDown)
            && let Some((px, _)) = self.state.pointer
        {
            let was = self.state.focus == Some(id) || self.state.focus_at_press == Some(id);
            self.state.focus = Some(id);
            self.state.caret = if was {
                Caret::to(under(text, px, shift(text, &self.state.caret)))
            } else {
                Caret {
                    at: text.chars().count(),
                    anchor: 0,
                }
            };
            gained = !was;
            self.state.selecting = was;
        }
        // Dragged from the press: the caret follows the pointer and selects from where it was pressed.
        if self.state.active == Some(id)
            && self.state.focus == Some(id)
            && matches!(self.event, Some(Event::PointerMoved { .. }))
            && let Some((px, _)) = self.state.pointer
        {
            if self.state.selecting {
                self.state.caret.at = under(text, px, shift(text, &self.state.caret));
            }
            self.consumed = true;
        }
        let focused = self.state.focus == Some(id);
        let mut response = TextResponse {
            gained,
            ..TextResponse::default()
        };
        if focused && let Some(&Event::Key(key)) = self.event {
            self.consumed = true;
            match key {
                Key::Enter => {
                    response.submitted = true;
                    self.state.focus = None;
                }
                Key::Escape => {
                    response.cancelled = true;
                    self.state.focus = None;
                }
                Key::Up => response.step = -1,
                Key::Down => response.step = 1,
                key => response.changed = edit(text, &mut self.state.caret, key, &mut self.state.copied),
            }
            self.changed |= response.changed || response.submitted || response.step != 0;
        }
        response.focused = self.state.focus == Some(id);
        let focused = response.focused;
        let caret = self.state.caret;
        let radius = self.px(theme.radius);
        if let Some(scene) = &mut self.scene {
            // A frame around a dark inside: lit when typing in, lighter when the pointer is on it.
            let frame = if focused {
                theme.accent
            } else if i.hovered {
                theme.muted
            } else {
                theme.control_hover
            };
            scene.fill_rounded(area.x, area.y, area.w, area.h, radius, frame);
            let inner = area.shrink(line);
            scene.fill_rounded(
                inner.x,
                inner.y,
                inner.w,
                inner.h,
                (radius - line).max(0.0),
                theme.field,
            );
            // The text is cut to the field.
            let x = area.x + pad - if focused { shift(text, &caret) } else { 0.0 };
            let field = Area {
                x: area.x + pad,
                w: room + line * 2.0,
                ..area
            };
            scene.next_layer();
            scene.set_clip(Some(field.clip().intersect(&self.area.clip())));
            let y = area.y + area.h / 2.0;
            let half = size * 0.6;
            let sel = caret.selection();
            if focused && !sel.is_empty() {
                let (from, to) = (width_to(text, sel.start), width_to(text, sel.end));
                let color = Color { a: 110, ..theme.accent };
                scene.fill(x + from, y - half, to - from, half * 2.0, color);
            }
            if text.is_empty() && !focused {
                scene.text(x, y, size, theme.muted, Align::Left, hint);
            } else {
                scene.text(x, y, size, theme.text, Align::Left, text.as_str());
            }
            if focused {
                let x = x + width_to(text, caret.at);
                scene.line((x, y - half), (x, y + half), line, theme.text);
            }
            scene.next_layer();
            scene.set_clip(Some(self.area.clip()));
        }
        response
    }

    /// A table of `len` rows under a header, filling the rest of the panel (no more than its rows need) and scrolled
    /// with the wheel. `row(i)` gives the cells of row `i`; only the rows on screen are asked for. The `selected` row
    /// is highlighted; returns the row clicked. Each row on screen is recorded by the text of its first cell.
    pub fn table(
        &mut self,
        columns: &[Column],
        len: usize,
        selected: Option<usize>,
        row: impl FnMut(usize) -> Vec<Cell>,
    ) -> Option<usize> {
        self.table_with(columns, len, selected, None, false, row).clicked
    }

    /// A table whose headers sort it, when `sort` is given (a click sorts by that column from the largest, a
    /// second from the smallest, a third not at all; the caller orders the rows), and whose rows have a mark to
    /// remove them under the pointer, when `removable`.
    pub fn table_with(
        &mut self,
        columns: &[Column],
        len: usize,
        selected: Option<usize>,
        mut sort: Option<&mut Option<Sort>>,
        removable: bool,
        mut row: impl FnMut(usize) -> Vec<Cell>,
    ) -> TableResponse {
        let id = self.id();
        let header_id = self.id();
        let row_h = self.px(self.theme.row_height * 0.8);
        // The rest of the panel, but always room for a few rows: the column scrolls when it does not fit. Fewer rows
        // take only their room, so what comes after the table follows it.
        let area = Area {
            x: self.inner.x,
            y: self.y,
            w: self.inner.w,
            h: (self.inner.y + self.inner.h - self.y)
                .max(row_h * (TABLE_ROWS + 1) as f32)
                .min(row_h * (len + 1) as f32),
        };
        self.y += area.h;
        let shown = ((area.h / row_h + 0.01) as usize).saturating_sub(1);
        let max_scroll = len.saturating_sub(shown);
        let mut scroll = self.state.scroll.get(&id).copied().unwrap_or(0).min(max_scroll);
        if let Some(&Event::Wheel { delta, .. }) = self.event
            && max_scroll > 0
            && self.state.pointer.is_some_and(|p| area.contains(p))
        {
            let rows = (delta.abs().ceil() as usize).max(1) * 3;
            scroll = if delta > 0.0 {
                scroll.saturating_sub(rows)
            } else {
                (scroll + rows).min(max_scroll)
            };
            self.consumed = true;
        }
        self.state.scroll.insert(id, scroll);
        let rows_area = Area {
            y: area.y + row_h,
            h: row_h * shown.min(len - scroll) as f32,
            ..area
        };
        let i = self.interact(id, rows_area);
        let pointed = self
            .state
            .pointer
            .filter(|_| i.hovered)
            .map(|(_, y)| scroll + ((y - rows_area.y) / row_h) as usize)
            .filter(|&r| r < len);
        // The remove mark is the last stretch of the row, as wide as a row is high.
        let mark = row_h;
        let on_mark = removable && self.state.pointer.is_some_and(|(x, _)| x >= area.x + area.w - mark);
        let mut response = TableResponse::default();
        if i.clicked {
            if on_mark {
                response.removed = pointed;
            } else {
                response.clicked = pointed;
            }
        }
        let total: f32 = columns.iter().map(|c| c.width).sum::<f32>().max(f32::EPSILON);
        let header = Area { h: row_h, ..area };
        if let Some(sort) = sort.as_deref_mut() {
            let h = self.interact(header_id, header);
            if h.clicked
                && let Some((px, _)) = self.state.pointer
            {
                let at = (px - area.x) / area.w * total;
                let mut edge = 0.0;
                let column = columns.iter().position(|c| {
                    edge += c.width;
                    at < edge
                });
                if let Some(column) = column {
                    *sort = match *sort {
                        Some(s) if s.column == column && s.descending => Some(Sort {
                            column,
                            descending: false,
                        }),
                        Some(s) if s.column == column => None,
                        _ => Some(Sort {
                            column,
                            descending: true,
                        }),
                    };
                }
            }
        }
        let sorted = sort.as_deref().copied().flatten();
        // The header cells are named by their titles.
        let mut x = area.x;
        for column in columns {
            let w = area.w * column.width / total;
            self.record(
                column.title,
                Area {
                    x,
                    y: area.y,
                    w,
                    h: row_h,
                },
            );
            x += w;
        }
        let cells: Vec<(usize, Vec<Cell>)> = (scroll..len).take(shown).map(|i| (i, row(i))).collect();
        for (n, (_, cells)) in cells.iter().enumerate() {
            if let Some(first) = cells.first() {
                let y = rows_area.y + row_h * n as f32;
                self.record(&first.text, Area { y, h: row_h, ..area });
                if removable {
                    let x = area.x + area.w - mark;
                    let at = Area {
                        x,
                        y,
                        w: mark,
                        h: row_h,
                    };
                    self.record(&format!("Remove row {}", first.text), at);
                }
            }
        }
        let theme = *self.theme;
        let (size, radius) = (self.font_size(), self.px(theme.radius));
        let Some(scene) = &mut self.scene else {
            return response;
        };
        let draw_row = |scene: &mut Scene, y: f32, cells: &mut dyn Iterator<Item = (String, Color)>| {
            let mut x = area.x;
            for (column, (text, color)) in columns.iter().zip(cells) {
                let w = area.w * column.width / total;
                let at = match column.align {
                    Align::Left => x,
                    Align::Center => x + w / 2.0,
                    Align::Right => x + w,
                };
                scene.text(at, y + row_h / 2.0, size, color, column.align, text);
                x += w;
            }
        };
        let titles = columns.iter().enumerate().map(|(k, c)| {
            let arrow = match sorted {
                Some(s) if s.column == k && s.descending => " ↓",
                Some(s) if s.column == k => " ↑",
                _ => "",
            };
            (format!("{}{arrow}", c.title), theme.muted)
        });
        draw_row(scene, area.y, &mut titles.into_iter());
        for (n, (i, cells)) in cells.into_iter().enumerate() {
            let y = rows_area.y + row_h * n as f32;
            if selected == Some(i) {
                scene.fill_rounded(area.x, y, area.w, row_h, radius, theme.control);
            } else if pointed == Some(i) {
                let hover = Color {
                    a: 110,
                    ..theme.control
                };
                scene.fill_rounded(area.x, y, area.w, row_h, radius, hover);
            }
            let under = pointed == Some(i) && removable;
            let cells = cells.into_iter().enumerate().map(|(k, c)| {
                // The last cell gives way to the remove mark under the pointer.
                if under && k + 1 == columns.len() {
                    (String::new(), theme.text)
                } else {
                    (c.text, c.color.unwrap_or(theme.text))
                }
            });
            draw_row(scene, y, &mut cells.into_iter());
            if under {
                let color = if on_mark { theme.text } else { theme.muted };
                let (cx, cy, d) = (area.x + area.w - mark / 2.0, y + row_h / 2.0, row_h * 0.18);
                let width = self.scale.max(1.0) * 1.2;
                scene.line((cx - d, cy - d), (cx + d, cy + d), width, color);
                scene.line((cx - d, cy + d), (cx + d, cy - d), width, color);
            }
        }
        response
    }

    fn id(&mut self) -> Id {
        self.next_id += 1;
        self.next_id
    }

    fn row_height(&self) -> f32 {
        self.px(self.theme.row_height)
    }

    fn gap(&self) -> f32 {
        self.px(self.theme.spacing)
    }

    fn font_size(&self) -> f32 {
        self.px(self.theme.font_size)
    }

    fn text_width(&self, text: &str) -> f32 {
        Font::ui().measure(text, self.font_size())
    }

    /// Room for a control `w` wide in a row, or the whole width in a column.
    fn alloc(&mut self, w: f32) -> Area {
        let area = self.place(w);
        self.last = area;
        area
    }

    fn place(&mut self, w: f32) -> Area {
        let (h, gap) = (self.row_height(), self.gap());
        if self.horizontal {
            let (w, gap) = match &mut self.cell {
                Some(cells) => (cells.take(), cells.gap),
                None => (w, gap),
            };
            if self.bar && self.x > self.inner.x && self.x + w > self.inner.x + self.inner.w {
                self.x = self.inner.x;
                self.y += h + gap;
            }
            let area = Area {
                x: self.x,
                y: self.y,
                w,
                h,
            };
            self.x += w + gap;
            area
        } else {
            let area = Area {
                x: self.inner.x,
                y: self.y,
                w: self.inner.w,
                h,
            };
            self.y += h + gap;
            area
        }
    }

    fn record(&mut self, text: &str, area: Area) {
        self.state.rects.push((text.to_string(), area));
    }

    /// Hover and clicks: a click is the pointer going down and up on the same control.
    fn interact(&mut self, id: Id, area: Area) -> Interaction {
        let over = self.state.pointer.is_some_and(|p| area.contains(p));
        let hovered = over && self.state.active.is_none_or(|a| a == id);
        if hovered {
            self.state.hot = Some(id);
            self.state.shape = PointerShape::Hand;
        }
        let mut clicked = false;
        match self.event {
            Some(Event::PointerDown) if hovered => {
                self.state.active = Some(id);
                self.consumed = true;
            }
            Some(Event::PointerUp) if self.state.active == Some(id) => {
                clicked = hovered;
                self.consumed = true;
            }
            _ => {}
        }
        Interaction {
            hovered,
            held: hovered && self.state.active == Some(id),
            clicked,
            glow: self.state.glow(id),
        }
    }
}

/// A control's color turned toward `lit` as it lights up under the pointer, and darkened while pressed.
fn shade(base: Color, i: Interaction, lit: Color) -> Color {
    if i.held {
        mix(base, Color::rgb(0, 0, 0), 0.2)
    } else {
        mix(base, lit, 0.12 * i.glow)
    }
}

/// `color` at `t` of its opacity: a fill that fades in under the pointer.
fn faded(color: Color, t: f32) -> Color {
    Color {
        a: (color.a as f32 * t.clamp(0.0, 1.0)).round() as u8,
        ..color
    }
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color {
        r: m(a.r, b.r),
        g: m(a.g, b.g),
        b: m(a.b, b.b),
        a: a.a,
    }
}

/// `text` in lines no wider than `max` at font `size`, broken after a space or a path separator where there is one.
pub(crate) fn wrap(text: &str, max: f32, size: f32) -> Vec<String> {
    let width = |t: &str| Font::ui().measure(t, size);
    let mut lines = Vec::new();
    let mut rest = text;
    while width(rest) > max {
        let (mut fit, mut cut) = (0, None);
        for (i, c) in rest.char_indices() {
            let end = i + c.len_utf8();
            if fit > 0 && width(&rest[..end]) > max {
                break;
            }
            fit = end;
            if matches!(c, ' ' | '/' | '\\') {
                cut = Some(end);
            }
        }
        let end = cut.unwrap_or(fit);
        lines.push(rest[..end].trim_end().to_string());
        rest = rest[end..].trim_start();
    }
    if !rest.is_empty() || lines.is_empty() {
        lines.push(rest.to_string());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Presses the keys in a field holding `text` with the caret at its end; returns the text and the caret.
    fn typed(text: &str, keys: &[Key]) -> (String, Caret, Option<String>) {
        let mut text = text.to_string();
        let mut caret = Caret::to(text.chars().count());
        let mut copied = None;
        for &key in keys {
            edit(&mut text, &mut caret, key, &mut copied);
        }
        (text, caret, copied)
    }

    #[test]
    fn keys_edit_at_the_caret() {
        use Key::*;
        assert_eq!(typed("8200", &[Left, Left, Char('5')]).0, "82500");
        assert_eq!(typed("82500", &[Left, Left, Backspace]).0, "8200");
        assert_eq!(typed("82500", &[Home, Delete, Char('9')]).0, "92500");
        assert_eq!(typed("ab", &[Home, Right, Space, End, Char('c')]).0, "a bc");
        // Nothing to take at the ends.
        assert_eq!(
            typed("ab", &[Delete, Home, Backspace, Left]),
            ("ab".into(), Caret::to(0), None)
        );
        // Characters, not bytes.
        assert_eq!(typed("回测✓", &[Left, Backspace, Char('x')]).0, "回x✓");
    }

    #[test]
    fn a_selection_is_replaced_copied_or_cut() {
        use Key::*;
        assert_eq!(typed("ETHUSDT", &[SelectAll, Char('s'), Char('o')]).0, "so");
        assert_eq!(typed("ETHUSDT", &[SelectAll, Backspace]).0, "");
        let (text, _, copied) = typed("2026-09-15", &[SelectAll, Copy]);
        assert_eq!((text.as_str(), copied.as_deref()), ("2026-09-15", Some("2026-09-15")));
        let (text, caret, copied) = typed("2026-09-15", &[SelectAll, Cut]);
        assert_eq!(
            (text.as_str(), caret, copied.as_deref()),
            ("", Caret::to(0), Some("2026-09-15"))
        );
        // Nothing selected: the whole text is copied.
        assert_eq!(typed("note", &[Left, Copy]).2.as_deref(), Some("note"));
        // An arrow ends the selection at its side.
        assert_eq!(typed("abc", &[SelectAll, Left, Char('x')]).0, "xabc");
        assert_eq!(typed("abc", &[SelectAll, Right, Char('x')]).0, "abcx");
        // With Shift the selection grows from where the caret was.
        assert_eq!(typed("abcdef", &[End, SelectLeft, SelectLeft, Char('x')]).0, "abcdx");
        assert_eq!(typed("abcdef", &[Home, SelectRight, SelectEnd, Backspace]).0, "");
        assert_eq!(typed("abcdef", &[End, Left, SelectHome, Char('z')]).0, "zf");
    }

    #[test]
    fn long_text_wraps_in_a_column_and_is_cut_short_in_a_row() {
        let (mut state, theme) = (UiState::default(), Theme::default());
        let mut scene = Scene::new();
        let area = Area {
            x: 0.0,
            y: 0.0,
            w: 160.0,
            h: 400.0,
        };
        let mut ui = Ui::new(&mut state, None, Some(&mut scene), &theme, 1.0, area, false, 0);
        ui.label("in /home/someone/.local/share/charts/exports");
        ui.label("回测リテスト리테스트回测リテスト리테스트回测リテスト");
        ui.columns(2, |ui| ui.label("A label wider than its cell"));
        let max = ui.inner.w;
        let texts: Vec<(String, f32)> = scene.texts.iter().map(|t| (t.text.clone(), t.y)).collect();
        let widths: Vec<f32> = texts.iter().map(|(t, _)| ui_width(&theme, t)).collect();
        assert!(widths.iter().all(|&w| w <= max), "{texts:?}");
        // The path breaks after a separator and loses nothing.
        let path: Vec<&str> = texts
            .iter()
            .map(|(t, _)| t.as_str())
            .take_while(|t| !t.starts_with('回'))
            .collect();
        assert!(
            path.len() > 1 && path[1..].iter().all(|l| !l.starts_with('/')),
            "{path:?}"
        );
        assert!(
            path[..path.len() - 1]
                .iter()
                .all(|l| l.ends_with('/') || !l.contains('/'))
        );
        assert_eq!(
            path.concat().replace(' ', ""),
            "in/home/someone/.local/share/charts/exports"
        );
        // Text without spaces breaks between characters; each line is a row below the last.
        let cjk = &texts[path.len()..texts.len() - 1];
        assert!(cjk.len() > 1 && cjk.windows(2).all(|p| p[1].1 > p[0].1), "{cjk:?}");
        assert!(texts.last().unwrap().0.ends_with('…'), "{texts:?}");
    }

    fn ui_width(theme: &Theme, text: &str) -> f32 {
        Font::ui().measure(text, theme.font_size)
    }
}
