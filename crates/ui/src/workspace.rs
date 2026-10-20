use tickerlight_core::builtin::LinkedTime;
use tickerlight_core::{Align, Chart, Color, Event, Font, Key, PointerShape, Scene, Surface, Time};

use crate::ui::{BARS, ContextAt};
use crate::{Area, Theme, Ui, UiState};

/// How wide a tip gets before it goes on the next line, before scaling.
const TIP_WIDTH: f32 = 360.0;

/// Where a panel sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dock {
    /// The bar across the top; its controls go left to right.
    Top,
    /// The column on the right, under the bar, left of the strip.
    Right,
    /// A narrow column on the left, under the bar: tools as icons.
    Left,
    /// A narrow column at the right edge, under the bar: what the right column shows, as icons.
    Strip,
    /// A row under the chart, as wide as it: the visible range, the time zone, the price scale.
    Bottom,
    /// A row between the chart and the bottom row: the replay's player.
    Player,
    /// A row over the top of the chart, centered, as wide as its controls and only where it has any: for what is
    /// selected on the chart. The pointer goes through to the chart around it.
    Float,
    /// A line along the bottom of the chart's plot, drawn over it: for what the program has to say (loading,
    /// errors). The pointer goes through it to the chart.
    Overlay,
}

/// A group of controls. `ui` lays them out; it is called for every input event and every frame, and acts on the
/// chart and on the program's state `C` when a control is used.
pub trait Panel<C> {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, ctx: &mut C);
}

impl<C, F: FnMut(&mut Ui, &mut Chart, &mut C)> Panel<C> for F {
    fn ui(&mut self, ui: &mut Ui, chart: &mut Chart, ctx: &mut C) {
        self(ui, chart, ctx)
    }
}

/// How the charts share the room for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    #[default]
    One,
    /// Two side by side.
    Columns,
    /// Two, one above the other.
    Rows,
    /// Four, two by two.
    Grid,
}

impl Layout {
    pub const ALL: [Layout; 4] = [Layout::One, Layout::Columns, Layout::Rows, Layout::Grid];

    /// How many charts it shows.
    pub fn cells(self) -> usize {
        let (columns, rows) = self.grid();
        columns * rows
    }

    pub fn icon(self) -> crate::Icon {
        use crate::Icon;
        match self {
            Layout::One => Icon::LayoutOne,
            Layout::Columns => Icon::LayoutColumns,
            Layout::Rows => Icon::LayoutRows,
            Layout::Grid => Icon::LayoutGrid,
        }
    }

    /// What it is, for a tip.
    pub fn label(self) -> &'static str {
        match self {
            Layout::One => "One chart",
            Layout::Columns => "Two charts side by side",
            Layout::Rows => "Two charts, one above the other",
            Layout::Grid => "Four charts",
        }
    }

    /// Its name in files.
    pub fn name(self) -> &'static str {
        match self {
            Layout::One => "one",
            Layout::Columns => "columns",
            Layout::Rows => "rows",
            Layout::Grid => "grid",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.name() == name)
    }

    fn grid(self) -> (usize, usize) {
        match self {
            Layout::One => (1, 1),
            Layout::Columns => (2, 1),
            Layout::Rows => (1, 2),
            Layout::Grid => (2, 2),
        }
    }

    /// Chart `i`'s part of `area`, `gap` from the others, on whole pixels; the first is at the top left, then
    /// across, then down.
    fn cell(self, i: usize, area: Area, gap: f32) -> Area {
        let (columns, rows) = self.grid();
        let (column, row) = (i % columns, i / columns);
        let w = ((area.w - gap * (columns - 1) as f32) / columns as f32)
            .floor()
            .max(0.0);
        let h = ((area.h - gap * (rows - 1) as f32) / rows as f32).floor().max(0.0);
        // The last column and row take what rounding left.
        let x = area.x + column as f32 * (w + gap);
        let y = area.y + row as f32 * (h + gap);
        Area {
            x,
            y,
            w: if column + 1 == columns { area.x + area.w - x } else { w },
            h: if row + 1 == rows { area.y + area.h - y } else { h },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Region {
    /// The chart in that cell of the layout.
    Chart(usize),
    Panels(Dock),
    /// The open list of a dropdown, over everything.
    Menu,
}

impl Region {
    fn dock(self) -> Dock {
        match self {
            Region::Panels(dock) => dock,
            Region::Chart(_) | Region::Menu => Dock::Overlay,
        }
    }
}

/// A chart of the window and its scene, rebuilt only when the chart changed.
struct Cell {
    chart: Chart,
    scene: Scene,
    dirty: bool,
}

impl Cell {
    fn new(chart: Chart) -> Self {
        Self {
            chart,
            scene: Scene::new(),
            dirty: true,
        }
    }
}

/// Width of the right column, logical pixels.
const RIGHT_WIDTH: f32 = 280.0;
/// The narrowest the right column gets in a small window.
const RIGHT_MIN_WIDTH: f32 = 224.0;
/// Width of the left column.
const LEFT_WIDTH: f32 = 44.0;

/// The docks in the order their panels run.
const DOCKS: [Dock; 8] = [
    Dock::Top,
    Dock::Left,
    Dock::Right,
    Dock::Strip,
    Dock::Player,
    Dock::Bottom,
    Dock::Overlay,
    Dock::Float,
];

/// Where the ids of a dock's controls start: apart from every other dock's, so what one dock shows does not move
/// the ids of another.
fn first_id(dock: Dock) -> u64 {
    let n = DOCKS.iter().position(|&d| d == dock).expect("every dock is listed");
    (n as u64 + 1) << 32
}

/// Which of the bars that keep their rows `dock` is: the top bar, the player, the row under the chart.
fn counted(dock: Dock) -> Option<usize> {
    match dock {
        Dock::Top => Some(0),
        Dock::Player => Some(1),
        Dock::Bottom => Some(2),
        _ => None,
    }
}

/// How far below the top of the chart the floating row is.
const FLOAT_TOP: f32 = 34.0;
/// The first id of the controls of an open dropdown list, apart from the docks' ids.
const MENU_IDS: u64 = 64 << 32;

/// A window: the charts of its [`Layout`], with panels docked above them and to their right, and the program's
/// state `C` the panels act on. It is a [`Surface`], so a backend shows it like a bare chart.
///
/// The pointer goes to whichever part it is over; a drag stays with the part it began in. Keys go to the text
/// field being typed in, if any, else to the active chart, as do commands; ticks go to every chart. The panels
/// act on the active chart; a press on another makes that one active. The others show the time under the pointer
/// as a line, and follow the time the active one scrolls to.
pub struct Workspace<C> {
    charts: Vec<Cell>,
    active: usize,
    layout: Layout,
    /// The time under the pointer the other charts show.
    linked: Option<Time>,
    pub ctx: C,
    pub theme: Theme,
    panels: Vec<(Dock, Box<dyn Panel<C>>)>,
    state: UiState,
    size: (f32, f32),
    scale: f32,
    pointer: Option<(f32, f32)>,
    hover: Option<Region>,
    drag: Option<Region>,
    /// Rows of the counted bars the layout is made for (top, player, bottom; see [`counted`]).
    bar_rows: [usize; BARS],
    /// Where the controls of the floating row were in the last pass.
    float_extent: Option<Area>,
    /// Docks with panels that are not shown now (see [`Workspace::show`]).
    hidden: Vec<Dock>,
    /// The docks fading in, and how far they are, from 0 to 1 over [`APPEAR`] seconds.
    appear: Vec<(Dock, f32)>,
}

/// Seconds a dock takes to fade in when what it shows is new (another page, a drawing selected).
const APPEAR: f32 = 0.16;

impl<C> Workspace<C> {
    pub fn new(chart: Chart, ctx: C) -> Self {
        let theme = Theme::from_chart(&chart.style);
        Self {
            charts: vec![Cell::new(chart)],
            active: 0,
            layout: Layout::One,
            linked: None,
            ctx,
            theme,
            panels: Vec::new(),
            state: UiState::default(),
            size: (0.0, 0.0),
            scale: 1.0,
            pointer: None,
            hover: None,
            drag: None,
            bar_rows: [1; BARS],
            float_extent: None,
            hidden: Vec::new(),
            appear: Vec::new(),
        }
    }

    /// Fades the dock in from its top, as when it shows something else now (the program switched its page).
    pub fn fade_in(&mut self, dock: Dock) {
        self.appear.retain(|(d, _)| *d != dock);
        self.appear.push((dock, 0.0));
        self.state.scroll_to_top(first_id(dock));
    }

    /// How far the dock has faded in, eased, while it does.
    fn opacity(&self, dock: Dock) -> Option<f32> {
        let &(_, t) = self.appear.iter().find(|(d, _)| *d == dock)?;
        Some(t * t * (3.0 - 2.0 * t))
    }

    /// Moves the docks fading in `dt` seconds on. Returns whether any did.
    fn advance(&mut self, dt: f32) -> bool {
        let moving = !self.appear.is_empty();
        for (_, t) in &mut self.appear {
            *t = (*t + dt.max(0.0) / APPEAR).min(1.0);
        }
        self.appear.retain(|&(_, t)| t < 1.0);
        moving
    }

    /// Adds a panel below (or, in the bar, after) the panels of its dock.
    pub fn with(mut self, dock: Dock, panel: impl Panel<C> + 'static) -> Self {
        self.panels.push((dock, Box::new(panel)));
        self.lay_out();
        self
    }

    /// The active chart: the one the panels, keys and commands act on.
    pub fn chart(&self) -> &Chart {
        &self.charts[self.active].chart
    }

    pub fn chart_mut(&mut self) -> &mut Chart {
        self.chart_at_mut(self.active)
    }

    /// Shows another chart (another symbol, say) in place of the active one, and returns the old one.
    pub fn set_chart(&mut self, chart: Chart) -> Chart {
        self.set_chart_at(self.active, chart)
    }

    /// How many charts there are: as many as the layout has cells.
    pub fn charts(&self) -> usize {
        self.charts.len()
    }

    pub fn chart_at(&self, i: usize) -> &Chart {
        &self.charts[i].chart
    }

    pub fn chart_at_mut(&mut self, i: usize) -> &mut Chart {
        let cell = &mut self.charts[i];
        cell.dirty = true;
        &mut cell.chart
    }

    /// Shows another chart in cell `i`, and returns the old one.
    pub fn set_chart_at(&mut self, i: usize, chart: Chart) -> Chart {
        let old = std::mem::replace(&mut self.charts[i].chart, chart);
        self.charts[i].chart.set_scale(self.scale);
        self.charts[i].chart.resources_mut().insert(LinkedTime(None));
        self.lay_out();
        old
    }

    /// Which chart is active.
    pub fn active(&self) -> usize {
        self.active
    }

    pub fn set_active(&mut self, i: usize) {
        self.active = i.min(self.charts.len() - 1);
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Shows the charts in `layout`: cells it adds get charts from `new`, the last ones it drops are returned.
    /// An active chart in a cell that goes takes the last cell kept, in place of the chart there.
    pub fn set_layout(&mut self, layout: Layout, mut new: impl FnMut() -> Chart) -> Vec<Chart> {
        self.layout = layout;
        let n = layout.cells();
        if self.active >= n {
            self.charts.swap(self.active, n - 1);
            self.active = n - 1;
        }
        let dropped = self.charts.drain(n.min(self.charts.len())..).map(|c| c.chart).collect();
        while self.charts.len() < n {
            let mut chart = new();
            chart.set_scale(self.scale);
            self.charts.push(Cell::new(chart));
        }
        (self.hover, self.drag) = (None, None);
        self.link(None);
        self.lay_out();
        dropped
    }

    /// The controls: where they are, which field has the keys.
    pub fn ui_state(&self) -> &UiState {
        &self.state
    }

    /// Whether keys go to the controls now: a field has them or a dropdown list is open.
    pub fn typing(&self) -> bool {
        self.state.focus.is_some() || self.state.menu.is_some()
    }

    /// Puts the keys into the field named `name` with its text selected, as a first click would, then types `key`
    /// into it. Returns whether there is such a field.
    pub fn type_into(&mut self, name: &str, key: Option<Key>) -> bool {
        self.state.focus_on = Some(name.to_string());
        self.run(None, None);
        let found = self.state.focus_on.take().is_none();
        if found && let Some(key) = key {
            self.handle(Event::Key(key));
        }
        found
    }

    /// Where the active chart is in the window, in device pixels.
    pub fn chart_area(&self) -> Area {
        self.area(Region::Chart(self.active))
    }

    /// Shows or hides the panels of `dock`; the charts take the room of a hidden one.
    pub fn show(&mut self, dock: Dock, shown: bool) {
        if shown == !self.hidden.contains(&dock) {
            return;
        }
        if shown {
            self.hidden.retain(|&d| d != dock);
        } else {
            self.hidden.push(dock);
        }
        self.lay_out();
    }

    /// Whether `dock` has panels and shows them.
    fn has(&self, dock: Dock) -> bool {
        !self.hidden.contains(&dock) && self.panels.iter().any(|(d, _)| *d == dock)
    }

    fn area(&self, region: Region) -> Area {
        let (w, h) = self.size;
        // A gutter around the window and between the cards that are there.
        let g = self.theme.gutter * self.scale;
        // A bar of `rows` rows: one row of controls each, a spacing between them, padding around.
        let bar = |rows: usize| {
            let rows = rows.max(1) as f32;
            (rows * self.theme.row_height + (rows - 1.0) * self.theme.spacing + 2.0 * self.theme.padding) * self.scale
        };
        let top = if self.has(Dock::Top) {
            bar(self.bar_rows[0])
        } else {
            0.0
        };
        let strip = if self.has(Dock::Strip) {
            (LEFT_WIDTH * self.scale).min(w)
        } else {
            0.0
        };
        // A narrow window gives the chart more of its width: the column narrows down to `RIGHT_MIN_WIDTH`.
        let right = if self.has(Dock::Right) {
            let width = (0.28 * w).clamp(RIGHT_MIN_WIDTH * self.scale, RIGHT_WIDTH * self.scale);
            width.min((w - strip).max(0.0))
        } else {
            0.0
        };
        let left = if self.has(Dock::Left) {
            (LEFT_WIDTH * self.scale).min((w - right - strip).max(0.0))
        } else {
            0.0
        };
        let bottom = if self.has(Dock::Bottom) {
            bar(self.bar_rows[2])
        } else {
            0.0
        };
        let player = if self.has(Dock::Player) {
            bar(self.bar_rows[1])
        } else {
            0.0
        };
        let body_y = if top > 0.0 { g + top + g } else { g };
        let below = (h - body_y - g).max(0.0);
        // The chart, above the rows under it.
        let under = |row: f32| if row > 0.0 { row + g } else { 0.0 };
        let chart_h = (below - under(bottom) - under(player)).max(0.0);
        let chart_x = if left > 0.0 { g + left + g } else { g };
        // Where the right column ends: at the strip, or at the window's edge.
        let right_edge = w - g - under(strip);
        let chart_right = right_edge - under(right);
        let chart_w = (chart_right - chart_x).max(0.0);
        let charts = Area {
            x: chart_x,
            y: body_y,
            w: chart_w,
            h: chart_h,
        };
        let active = || self.layout.cell(self.active, charts, g);
        match region {
            Region::Panels(Dock::Top) => Area {
                x: g,
                y: g,
                w: (w - 2.0 * g).max(0.0),
                h: top,
            },
            Region::Panels(Dock::Right) => Area {
                x: right_edge - right,
                y: body_y,
                w: right,
                h: below,
            },
            Region::Panels(Dock::Strip) => Area {
                x: w - g - strip,
                y: body_y,
                w: strip,
                h: below,
            },
            Region::Panels(Dock::Left) => Area {
                x: g,
                y: body_y,
                w: left,
                h: below,
            },
            Region::Panels(Dock::Player) => Area {
                x: chart_x,
                y: body_y + chart_h + g,
                w: chart_w,
                h: player.min(below),
            },
            Region::Panels(Dock::Bottom) => Area {
                x: chart_x,
                y: body_y + (below - bottom).max(0.0),
                w: chart_w,
                h: bottom.min(below),
            },
            Region::Chart(i) => self.layout.cell(i, charts, g),
            // Along the bottom of the active chart's plot, above its time axis: clear of the legend and the
            // indicators' rows at the top.
            Region::Panels(Dock::Overlay) => {
                let chart = active();
                let h = ((self.theme.row_height + 2.0 * self.theme.padding) * self.scale).min(chart.h);
                let axis = self.chart().style.time_axis_height * self.scale;
                Area {
                    y: (chart.y + chart.h - axis - h).max(chart.y),
                    h,
                    ..chart
                }
            }
            // Centered on the active chart's plot, as wide and as tall (its rows wrap when the plot is narrow) as its
            // controls were in the last pass.
            Region::Panels(Dock::Float) => {
                let chart = active();
                let pad = self.theme.padding * self.scale;
                let plot_w = (chart.w - self.chart().style.price_axis_width * self.scale).max(0.0);
                let row = self.theme.row_height * self.scale;
                let (content_w, content_h) = self.float_extent.map_or((plot_w, row), |e| (e.w, e.h.max(row)));
                let w = (content_w + 2.0 * pad).min(chart.w);
                Area {
                    x: chart.x + ((plot_w - w) / 2.0).max(0.0),
                    y: chart.y + (FLOAT_TOP * self.scale).min(chart.h),
                    w,
                    h: (content_h + 2.0 * pad).min(chart.h),
                }
            }
            Region::Menu => self.menu_area().unwrap_or_default(),
        }
    }

    /// The box of the open dropdown list: under its button (above it when there is no room below), or to its right
    /// for a column's tools, in the window.
    fn menu_area(&self) -> Option<Area> {
        let menu = self.state.menu.as_ref()?;
        let (w, h) = Ui::menu_size(menu, &self.theme, self.scale);
        let (anchor, pad, gap) = (menu.anchor, self.theme.padding * self.scale, 4.0 * self.scale);
        if menu.beside {
            return Some(Area {
                x: (anchor.x + anchor.w + gap).min(self.size.0 - w).max(0.0),
                y: (anchor.y - pad).min(self.size.1 - h).max(0.0),
                w,
                h,
            });
        }
        let below = anchor.y + anchor.h + gap;
        let y = if below + h <= self.size.1 {
            below
        } else {
            (anchor.y - gap - h).max(0.0)
        };
        Some(Area {
            x: (anchor.x - pad).min(self.size.0 - w).max(0.0),
            y,
            w,
            h,
        })
    }

    fn region_at(&self, point: (f32, f32)) -> Region {
        if self.menu_area().is_some_and(|m| m.contains(point)) {
            return Region::Menu;
        }
        if self.float_extent.is_some() && self.area(Region::Panels(Dock::Float)).contains(point) {
            return Region::Panels(Dock::Float);
        }
        [
            Region::Panels(Dock::Top),
            Region::Panels(Dock::Right),
            Region::Panels(Dock::Strip),
            Region::Panels(Dock::Left),
            Region::Panels(Dock::Player),
            Region::Panels(Dock::Bottom),
        ]
        .into_iter()
        .find(|&r| self.has(r.dock()) && self.area(r).contains(point))
        .unwrap_or_else(|| {
            let cell = (0..self.charts.len()).find(|&i| self.area(Region::Chart(i)).contains(point));
            Region::Chart(cell.unwrap_or(self.active))
        })
    }

    fn lay_out(&mut self) {
        for i in 0..self.charts.len() {
            let area = self.area(Region::Chart(i));
            let cell = &mut self.charts[i];
            cell.chart.resize(area.w.max(1.0), area.h.max(1.0));
            cell.dirty = true;
        }
    }

    /// Lays the chart out again if a bar now takes another number of rows. Returns whether it did.
    fn fit_bar(&mut self) -> bool {
        let rows = self.state.bar_rows.map(|r| r.max(1));
        if rows == self.bar_rows {
            return false;
        }
        self.bar_rows = rows;
        self.lay_out();
        true
    }

    /// Settles how many rows the bar needs at this size, by laying the panels out without an event.
    fn measure(&mut self) {
        for _ in 0..3 {
            self.run(None, None);
            if !self.fit_bar() {
                break;
            }
        }
    }

    /// Gives chart `i` the event, in its own coordinates. When the event scrolls it, the others follow.
    fn send_to(&mut self, i: usize, event: Event) -> bool {
        let event = match event {
            Event::PointerMoved { x, y } => {
                let area = self.area(Region::Chart(i));
                Event::PointerMoved {
                    x: x - area.x,
                    y: y - area.y,
                }
            }
            other => other,
        };
        let before = self.right_time(i);
        let cell = &mut self.charts[i];
        let redraw = cell.chart.handle(event);
        cell.dirty |= redraw;
        if redraw && self.charts.len() > 1 && self.right_time(i) != before {
            self.follow(i);
        }
        redraw
    }

    /// The time at the right edge of chart `i`.
    fn right_time(&self, i: usize) -> Option<f64> {
        let chart = &self.charts[i].chart;
        chart.seen().time_of_bar(chart.time_scale().right)
    }

    /// Scrolls the other charts so the time at their right edge is chart `i`'s; their zoom stays.
    fn follow(&mut self, i: usize) {
        let Some(time) = self.right_time(i) else { return };
        for (j, cell) in self.charts.iter_mut().enumerate() {
            if j == i {
                continue;
            }
            let Some(bar) = cell.chart.seen().bar_of_time(time) else {
                continue;
            };
            let mut scale = *cell.chart.time_scale();
            scale.right = bar;
            cell.chart.set_time_scale(scale);
            cell.dirty = true;
        }
    }

    /// Shows the time under the pointer over chart `over` on the others (none when it is over no chart).
    /// Returns whether that changed.
    fn link(&mut self, over: Option<usize>) -> bool {
        let time = over.filter(|_| self.charts.len() > 1).and_then(|i| {
            let (x, _) = self.pointer?;
            let area = self.area(Region::Chart(i));
            let chart = &self.charts[i].chart;
            let bar = chart.time_scale().index_of((x - area.x) as f64).floor();
            (bar >= 0.0)
                .then(|| chart.seen().time().get(bar as usize).copied())
                .flatten()
        });
        if time == self.linked {
            return false;
        }
        self.linked = time;
        for (j, cell) in self.charts.iter_mut().enumerate() {
            let time = if Some(j) == over { None } else { time };
            cell.chart.resources_mut().insert(LinkedTime(time));
            cell.dirty = true;
        }
        true
    }

    fn send_to_panels(&mut self, event: &Event) -> bool {
        let (consumed, changed) = self.run(Some(event), None);
        // A control acted on the chart or on the state the panels show.
        self.charts[self.active].dirty |= changed;
        consumed
    }

    /// One pass over every panel: an event pass, or a draw pass into `scene`. An open dropdown list takes events
    /// first (a pick reaches its dropdown in the same pass), is drawn last, and hides the pointer from the controls
    /// under it.
    fn run(&mut self, event: Option<&Event>, mut scene: Option<&mut Scene>) -> (bool, bool) {
        self.state.begin_pass();
        let mut out = (false, false);
        if event.is_some() {
            out = self.run_menu(event, None);
        }
        let pointer = self.state.pointer;
        if pointer.is_some_and(|p| self.menu_area().is_some_and(|m| m.contains(p))) {
            self.state.pointer = None;
        }
        for dock in DOCKS {
            if !self.has(dock) {
                continue;
            }
            let area = self.area(Region::Panels(dock));
            let mark = scene.as_deref().map(Scene::mark);
            if let Some(scene) = scene.as_deref_mut() {
                // The floating row is a box of its own over the chart, lifted off it by a shadow (in a layer of
                // its own, as it reaches past the box).
                if dock == Dock::Float && self.float_extent.is_some() {
                    let radius = self.theme.radius * self.scale;
                    scene.set_clip(Some(self.chart_area().clip()));
                    shadow(scene, area, radius, self.scale);
                    scene.next_layer();
                    scene.set_clip(Some(area.clip()));
                    scene.fill_rounded(area.x, area.y, area.w, area.h, radius, self.theme.control);
                } else {
                    scene.set_clip(Some(area.clip()));
                }
            }
            // The floating row is laid out to the plot's right edge, not wrapped at last pass's width, so it can
            // grow (a drawing with more controls is selected).
            let mut layout = area;
            if dock == Dock::Float {
                let chart = self.chart_area();
                let right = chart.x + chart.w - self.chart().style.price_axis_width * self.scale;
                layout.w = layout.w.max(right - layout.x);
            }
            let first_rect = self.state.rects.len();
            let mut ui = Ui::new(
                &mut self.state,
                event,
                scene.as_deref_mut(),
                &self.theme,
                self.scale,
                layout,
                !matches!(dock, Dock::Right | Dock::Left | Dock::Strip),
                first_id(dock),
            );
            ui.bar = matches!(dock, Dock::Top | Dock::Float | Dock::Player | Dock::Bottom);
            ui.counted = counted(dock);
            for (_, panel) in self.panels.iter_mut().filter(|(d, _)| *d == dock) {
                panel.ui(&mut ui, &mut self.charts[self.active].chart, &mut self.ctx);
            }
            let (consumed, changed) = ui.finish();
            out = (out.0 | consumed, out.1 | changed);
            if dock == Dock::Float {
                let extent = self.state.rects[first_rect..].iter().map(|(_, a)| *a).reduce(|a, b| {
                    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
                    Area {
                        x,
                        y,
                        w: (a.x + a.w).max(b.x + b.w) - x,
                        h: (a.y + a.h).max(b.y + b.h) - y,
                    }
                });
                // The row shows up (a drawing was selected): it fades in.
                if self.float_extent.is_none() && extent.is_some() {
                    self.fade_in(Dock::Float);
                }
                self.float_extent = extent;
            }
            if let Some(scene) = scene.as_deref_mut() {
                if let Some(mark) = mark
                    && let Some(opacity) = self.opacity(dock)
                {
                    scene.fade_from(mark, opacity);
                }
                scene.next_layer();
            }
        }
        self.state.pointer = pointer;
        if let Some(scene) = scene {
            let (consumed, changed) = self.run_menu(None, Some(scene));
            out = (out.0 | consumed, out.1 | changed);
        }
        // A pick whose dropdown is gone (its panel changed) closes the list.
        if self.state.menu.as_ref().is_some_and(|m| m.picked.is_some()) {
            self.state.menu = None;
        }
        out
    }

    /// The open dropdown list's pass, if one is open: a box of its own over everything, lifted by a shadow.
    fn run_menu(&mut self, event: Option<&Event>, mut scene: Option<&mut Scene>) -> (bool, bool) {
        let Some(area) = self.menu_area() else {
            return (false, false);
        };
        if let Some(scene) = scene.as_deref_mut() {
            let radius = self.theme.radius * self.scale;
            scene.set_clip(None);
            shadow(scene, area, radius, self.scale);
            scene.next_layer();
            scene.set_clip(Some(area.clip()));
            scene.fill_rounded(area.x, area.y, area.w, area.h, radius, self.theme.control);
        }
        let mut ui = Ui::new(
            &mut self.state,
            event,
            scene.as_deref_mut(),
            &self.theme,
            self.scale,
            area,
            false,
            MENU_IDS,
        );
        ui.menu_list();
        let out = ui.finish();
        if let Some(scene) = scene {
            scene.next_layer();
        }
        out
    }

    /// The tip of the control under the pointer, below it (above it at the bottom of the window), over everything.
    fn draw_tip(&self, scene: &mut Scene) {
        let Some((text, control)) = &self.state.tip else {
            return;
        };
        let px = |v: f32| v * self.scale;
        let size = px(self.theme.font_size);
        let pad = px(self.theme.padding);
        // A long tip goes on several lines, no wider than a column of text should be.
        let max = px(TIP_WIDTH).min(self.size.0 - 2.0 * pad);
        let lines = crate::ui::wrap(text, max, size);
        let widest = lines.iter().map(|l| Font::ui().measure(l, size)).fold(0.0, f32::max);
        let w = widest + 2.0 * pad;
        let row = px(self.theme.font_size) * 1.4;
        let h = px(self.theme.row_height) - row + row * lines.len() as f32;
        let x = control.x.min(self.size.0 - w).max(0.0);
        let below = control.y + control.h + px(4.0);
        let y = if below + h <= self.size.1 {
            below
        } else {
            control.y - px(4.0) - h
        };
        scene.set_clip(None);
        let radius = px(self.theme.radius);
        shadow(scene, Area { x, y, w, h }, radius, self.scale);
        scene.fill_rounded(x, y, w, h, radius, self.theme.control_hover);
        let top = y + (h - row * lines.len() as f32) / 2.0;
        for (i, line) in lines.iter().enumerate() {
            let cy = top + row * (i as f32 + 0.5);
            scene.text(x + pad, cy, size, self.theme.text, Align::Left, line.as_str());
        }
        scene.next_layer();
    }

    /// The pointer is the chart's now: no control is under it.
    fn pointer_to_chart(&mut self) {
        self.state.pointer = None;
        self.state.hot = None;
    }
}

impl<C> Surface for Workspace<C> {
    fn resize(&mut self, width: f32, height: f32) {
        self.size = (width, height);
        self.lay_out();
        self.measure();
    }

    fn set_scale(&mut self, scale: f32) {
        self.scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        for cell in &mut self.charts {
            cell.chart.set_scale(self.scale);
        }
        self.lay_out();
        self.measure();
    }

    fn handle(&mut self, event: Event) -> bool {
        let hot = self.state.hot;
        let focus = self.state.focus;
        let active = self.active;
        let redraw = match event {
            Event::PointerMoved { x, y } => {
                self.pointer = Some((x, y));
                let under = self.region_at((x, y));
                let mut redraw = false;
                if let Some(Region::Chart(was)) = self.hover
                    && under != Region::Chart(was)
                    && self.drag.is_none()
                {
                    redraw |= self.send_to(was, Event::PointerLeft);
                }
                self.hover = Some(under);
                match self.drag.unwrap_or(under) {
                    Region::Chart(i) => {
                        self.pointer_to_chart();
                        redraw | self.send_to(i, event) | self.link(Some(i))
                    }
                    Region::Panels(_) | Region::Menu => {
                        self.state.pointer = Some((x, y));
                        redraw | self.send_to_panels(&event) | self.link(None)
                    }
                }
            }
            Event::PointerDown => {
                // A press outside an open list closes it, and does nothing else; on the dropdown's button it is
                // the button's.
                if let Some(menu) = &self.state.menu
                    && self.hover != Some(Region::Menu)
                    && !self.pointer.is_some_and(|p| menu.anchor.contains(p))
                {
                    self.state.menu = None;
                    self.drag = Some(Region::Menu);
                    return true;
                }
                let target = self.hover.unwrap_or(Region::Chart(self.active));
                self.drag = Some(target);
                self.state.focus_at_press = self.state.focus.take();
                match target {
                    Region::Chart(i) => {
                        self.active = i;
                        self.send_to(i, event)
                    }
                    Region::Panels(_) | Region::Menu => self.send_to_panels(&event),
                }
            }
            // A right press over a chart: the chart sees it first (a drawing under it is selected), then the panels
            // open the menu of what is there. Elsewhere it closes an open list.
            Event::SecondaryDown => {
                let had = self.state.menu.take().is_some();
                let (Some(Region::Chart(i)), Some(at)) = (self.hover, self.pointer) else {
                    return had;
                };
                self.active = i;
                self.send_to(i, event);
                let area = self.area(Region::Chart(i));
                self.state.context = Some(ContextAt {
                    window: at,
                    chart: (at.0 - area.x, at.1 - area.y),
                });
                self.send_to_panels(&event);
                self.state.context = None;
                true
            }
            Event::DoubleClick => match self.hover {
                Some(Region::Chart(i)) => self.send_to(i, event),
                Some(Region::Panels(_) | Region::Menu) => self.send_to_panels(&event),
                None => false,
            },
            Event::PointerUp => {
                let target = self.drag.take().or(self.hover).unwrap_or(Region::Chart(self.active));
                let mut redraw = match target {
                    Region::Chart(i) => self.send_to(i, event),
                    Region::Panels(_) | Region::Menu => self.send_to_panels(&event),
                };
                self.state.active = None;
                // The drag is over: the pointer belongs to what it is over again.
                if let Region::Chart(i) = target
                    && self.hover != Some(target)
                {
                    redraw |= self.send_to(i, Event::PointerLeft);
                    self.state.pointer = self.pointer;
                    redraw |= self.send_to_panels(&Event::PointerMoved {
                        x: self.pointer.map_or(0.0, |p| p.0),
                        y: self.pointer.map_or(0.0, |p| p.1),
                    });
                }
                redraw
            }
            Event::PointerLeft => {
                let was = self.drag.or(self.hover);
                (self.pointer, self.hover, self.drag) = (None, None, None);
                self.state.active = None;
                self.pointer_to_chart();
                let i = match was {
                    Some(Region::Chart(i)) => i,
                    _ => self.active,
                };
                self.send_to(i, event) | self.link(None)
            }
            Event::Wheel { .. } => match self.hover {
                Some(Region::Panels(_) | Region::Menu) => self.send_to_panels(&event),
                Some(Region::Chart(i)) => self.send_to(i, event),
                None => self.send_to(self.active, event),
            },
            Event::Key(tickerlight_core::Key::Escape) if self.state.menu.is_some() => {
                self.state.menu = None;
                true
            }
            Event::Key(_) if self.state.focus.is_some() => {
                // A field that is gone (its panel changed) lets go of the keys.
                self.send_to_panels(&event) || {
                    self.state.focus = None;
                    self.send_to(self.active, event)
                }
            }
            Event::Tick { dt } => {
                // A panel with work left does a part of it in each frame.
                let mut redraw = self.state.advance(dt) | self.advance(dt) | self.state.again;
                // The active chart moving on its own (a replay playing, a new bar) takes the others along.
                let before = self.right_time(self.active);
                for cell in &mut self.charts {
                    let changed = cell.chart.handle(event);
                    cell.dirty |= changed;
                    redraw |= changed;
                }
                if self.charts.len() > 1 && self.right_time(self.active) != before {
                    self.follow(self.active);
                }
                redraw
            }
            // Every chart knows the keys held, whichever the pointer comes to next.
            Event::Modifiers { .. } => {
                let mut redraw = false;
                for cell in &mut self.charts {
                    let changed = cell.chart.handle(event);
                    cell.dirty |= changed;
                    redraw |= changed;
                }
                redraw
            }
            Event::Key(_) | Event::Command(_) => self.send_to(self.active, event),
        };
        redraw || self.active != active || self.state.hot != hot || self.state.focus != focus
    }

    fn needs_tick(&self) -> bool {
        self.charts.iter().any(|c| c.chart.needs_tick())
            || self.state.fading()
            || !self.appear.is_empty()
            || self.state.again
    }

    fn animating(&self) -> bool {
        self.charts.iter().any(|c| c.chart.animating()) || self.state.fading() || !self.appear.is_empty()
    }

    fn take_copied(&mut self) -> Option<String> {
        self.state.take_copied()
    }

    fn takes_text(&self) -> bool {
        self.state.focus.is_some() || self.chart().takes_text()
    }

    fn pointer_shape(&self) -> PointerShape {
        match self.drag.or(self.hover) {
            Some(Region::Chart(i)) => self
                .charts
                .get(i)
                .map_or(PointerShape::Default, |c| c.chart.pointer_shape()),
            Some(Region::Panels(_) | Region::Menu) => self.state.shape,
            None => PointerShape::Default,
        }
    }

    fn build_scene(&mut self, scene: &mut Scene) {
        // What the bar shows may have changed its width (a longer status, say).
        self.measure();
        for cell in &mut self.charts {
            if cell.dirty {
                cell.chart.build_scene(&mut cell.scene);
                cell.dirty = false;
            }
        }
        scene.clear();
        scene.background = self.theme.canvas;
        let r = self.theme.card_radius * self.scale;
        for (i, cell) in self.charts.iter().enumerate() {
            let chart = self.area(Region::Chart(i));
            scene.fill_rounded(chart.x, chart.y, chart.w, chart.h, r, cell.chart.style.background);
            scene.append(&cell.scene, (chart.x, chart.y), chart.clip());
            // The chart draws to its square edges; a ring of the canvas whose inner edge is the card's rounds them.
            scene.stroke_rounded(
                chart.x - r,
                chart.y - r,
                chart.w + 2.0 * r,
                chart.h + 2.0 * r,
                2.0 * r,
                r,
                self.theme.canvas,
            );
            // Of several, the one the panels act on is outlined.
            if self.charts.len() > 1 && i == self.active {
                let line = self.scale.round().max(1.0);
                scene.stroke_rounded(chart.x, chart.y, chart.w, chart.h, r, line, self.theme.accent);
            }
        }
        for dock in [
            Dock::Top,
            Dock::Left,
            Dock::Right,
            Dock::Strip,
            Dock::Player,
            Dock::Bottom,
        ] {
            if self.has(dock) {
                let a = self.area(Region::Panels(dock));
                scene.fill_rounded(a.x, a.y, a.w, a.h, r, self.theme.background);
            }
        }
        scene.next_layer();
        self.run(None, Some(scene));
        self.draw_tip(scene);
    }
}

/// A soft dark shadow under a box that floats over something, a little below it.
fn shadow(scene: &mut Scene, area: Area, radius: f32, scale: f32) {
    let spread = 10.0 * scale;
    scene.fill_soft(
        area.x - spread,
        area.y - spread + 3.0 * scale,
        area.w + 2.0 * spread,
        area.h + 2.0 * spread,
        radius + spread,
        1.5 * spread,
        Color {
            a: 150,
            ..Color::rgb(0, 0, 0)
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use tickerlight_core::{Candle, Candles, Font, Key, Time};

    fn chart() -> Chart {
        let mut candles = Candles::new();
        for i in 0..200 {
            let p = 100.0 + (i as f64 * 0.3).sin() * 5.0;
            candles
                .push(Candle {
                    time: Time(i * 60_000),
                    open: p,
                    high: p + 1.0,
                    low: p - 1.0,
                    close: p + 0.5,
                    volume: 10.0,
                })
                .unwrap();
        }
        Chart::new(candles)
    }

    #[derive(Default)]
    struct State {
        clicks: usize,
        text: String,
        submitted: Option<String>,
    }

    fn workspace() -> Workspace<State> {
        let mut w = Workspace::new(chart(), State::default())
            .with(Dock::Top, |ui: &mut Ui, _: &mut Chart, s: &mut State| {
                if ui.button("Count") {
                    s.clicks += 1;
                }
                if ui.text_field(&mut s.text, 100.0, "Symbol").submitted {
                    s.submitted = Some(s.text.clone());
                }
            })
            .with(Dock::Right, |ui: &mut Ui, chart: &mut Chart, _: &mut State| {
                if ui.button("Replay") {
                    chart.handle(Event::Command(tickerlight_core::plugin::commands::REPLAY_TOGGLE));
                }
            });
        w.resize(1000.0, 600.0);
        // One pass lays the controls out.
        w.build_scene(&mut Scene::new());
        w
    }

    fn click(w: &mut Workspace<State>, (x, y): (f32, f32)) {
        w.handle(Event::PointerMoved { x, y });
        w.handle(Event::PointerDown);
        w.handle(Event::PointerUp);
    }

    fn at(w: &Workspace<State>, text: &str) -> (f32, f32) {
        w.ui_state()
            .rect_of(text)
            .unwrap_or_else(|| panic!("no {text}"))
            .center()
    }

    #[test]
    fn a_control_lights_up_under_the_pointer_over_a_few_frames_and_goes_out_after() {
        let mut w = workspace();
        let button = w.ui_state().rect_of("Count").unwrap();
        // The fill of the button in the bar: none until the pointer is on it.
        let fill = |w: &mut Workspace<State>| {
            let mut scene = Scene::new();
            w.build_scene(&mut scene);
            let rect = scene
                .rects
                .iter()
                .find(|r| r.x == button.x && r.y == button.y && r.w == button.w);
            rect.map_or(0, |r| r.color.a)
        };
        let full = w.theme.control.a;
        assert!(!w.animating());
        w.handle(Event::PointerMoved {
            x: button.center().0,
            y: button.center().1,
        });
        assert!(
            w.animating() && w.needs_tick(),
            "the window sends frames while it lights up"
        );
        assert_eq!(fill(&mut w), 0, "not at once");
        assert!(w.handle(Event::Tick { dt: 0.05 }));
        let half = fill(&mut w);
        assert!(0 < half && half < full, "{half} of {full}");
        for _ in 0..3 {
            w.handle(Event::Tick { dt: 0.05 });
        }
        assert_eq!(fill(&mut w), full);
        assert!(!w.animating() && !w.needs_tick(), "lit, it rests");

        // Away from it, it goes out the same way.
        w.handle(Event::PointerMoved { x: 500.0, y: 300.0 });
        assert!(w.animating());
        w.handle(Event::Tick { dt: 0.05 });
        let going = fill(&mut w);
        assert!(0 < going && going < full, "{going} of {full}");
        for _ in 0..3 {
            w.handle(Event::Tick { dt: 0.05 });
        }
        assert_eq!(fill(&mut w), 0);
        assert!(!w.animating());
    }

    #[test]
    fn a_dock_fades_in_over_a_few_frames_and_the_rest_stays() {
        let mut w = workspace();
        let alpha = |w: &mut Workspace<State>, text: &str| {
            let mut scene = Scene::new();
            w.build_scene(&mut scene);
            // The last: the chart has a Replay button of its own, drawn before the panels.
            scene
                .texts
                .iter()
                .rev()
                .find(|t| t.text == text)
                .map(|t| t.color.a)
                .unwrap()
        };
        let full = alpha(&mut w, "Replay");
        w.fade_in(Dock::Right);
        assert!(w.animating());
        assert_eq!(alpha(&mut w, "Replay"), 0);
        assert_eq!(alpha(&mut w, "Count"), full, "only that dock");
        w.handle(Event::Tick { dt: 0.08 });
        let half = alpha(&mut w, "Replay");
        assert!(0 < half && half < full, "{half} of {full}");
        for _ in 0..3 {
            w.handle(Event::Tick { dt: 0.05 });
        }
        assert_eq!(alpha(&mut w, "Replay"), full);
        assert!(!w.animating());
    }

    #[test]
    fn a_dropdown_s_list_opens_over_the_chart_and_a_pick_or_a_press_elsewhere_closes_it() {
        const SOURCES: [&str; 8] = [
            "Binance",
            "OKX",
            "Coinbase",
            "KuCoin",
            "Bitget",
            "Kraken",
            "Dukascopy",
            "File",
        ];
        let mut w =
            Workspace::new(chart(), State::default()).with(Dock::Top, |ui: &mut Ui, _: &mut Chart, s: &mut State| {
                if ui.dropdown(&mut s.clicks, &SOURCES) {
                    s.submitted = Some(SOURCES[s.clicks].to_string());
                }
                ui.text_field(&mut s.text, 100.0, "Symbol");
            });
        w.resize(1000.0, 600.0);
        w.build_scene(&mut Scene::new());
        let open = |w: &mut Workspace<State>| {
            let button = at(w, SOURCES[w.ctx.clicks]);
            click(w, button);
            w.build_scene(&mut Scene::new());
        };
        assert!(
            w.ui_state().rect_of("Kraken").is_none(),
            "closed, it shows the one picked"
        );
        open(&mut w);
        let kraken = w.ui_state().rect_of("Kraken").expect("the list is open");
        assert!(w.chart_area().contains(kraken.center()), "over the chart");

        // A press on the list is the list's: dragged from there, the chart under it does not pan.
        let right = w.chart().time_scale().right;
        w.handle(Event::PointerMoved {
            x: kraken.center().0,
            y: kraken.center().1,
        });
        w.handle(Event::PointerDown);
        w.handle(Event::PointerMoved {
            x: kraken.center().0 + 200.0,
            y: kraken.center().1 + 100.0,
        });
        w.handle(Event::PointerUp);
        assert_eq!(w.chart().time_scale().right, right);
        assert_eq!(
            (w.ctx.clicks, w.ctx.submitted.as_deref()),
            (0, None),
            "released off it: no pick"
        );

        click(&mut w, kraken.center());
        w.build_scene(&mut Scene::new());
        assert_eq!((w.ctx.clicks, w.ctx.submitted.as_deref()), (5, Some("Kraken")));
        assert!(w.ui_state().rect_of("Binance").is_none(), "picked, it closes");

        // A press elsewhere closes it and does nothing else: the field does not take the keys.
        open(&mut w);
        let field = at(&w, "Symbol");
        click(&mut w, field);
        w.build_scene(&mut Scene::new());
        assert!(w.ui_state().rect_of("Binance").is_none());
        assert!(!w.ui_state().has_focus());
        open(&mut w);
        w.handle(Event::Key(Key::Escape));
        w.build_scene(&mut Scene::new());
        assert!(w.ui_state().rect_of("Binance").is_none());
        assert_eq!(w.ctx.clicks, 5);
    }

    fn replaying(w: &Workspace<State>) -> bool {
        w.chart()
            .resources()
            .get::<tickerlight_core::builtin::Replay>()
            .unwrap()
            .active
    }

    #[test]
    fn the_chart_sits_below_the_bar_and_left_of_the_column() {
        let w = workspace();
        let area = w.chart_area();
        // Cards with a gutter around the window and between them.
        let t = Theme::default();
        let (g, bar) = (t.gutter, t.row_height + 2.0 * t.padding);
        assert_eq!((area.x, area.y), (g, g + bar + g));
        assert_eq!(
            (area.w, area.h),
            (1000.0 - RIGHT_WIDTH - 3.0 * g, 600.0 - bar - 3.0 * g)
        );
        let (bar_x, bar_y) = at(&w, "Count");
        assert!(bar_y < g + bar && bar_x < 100.0);
        assert!(at(&w, "Replay").0 > area.w);
    }

    #[test]
    fn a_click_is_down_and_up_on_the_same_control() {
        let mut w = workspace();
        let count = at(&w, "Count");
        click(&mut w, count);
        assert_eq!(w.ctx.clicks, 1);
        // Pressed on the button, released elsewhere: no click.
        w.handle(Event::PointerMoved { x: count.0, y: count.1 });
        w.handle(Event::PointerDown);
        w.handle(Event::PointerMoved { x: 500.0, y: 20.0 });
        w.handle(Event::PointerUp);
        assert_eq!(w.ctx.clicks, 1);
        // A control acts on the chart.
        let replay = at(&w, "Replay");
        click(&mut w, replay);
        assert!(replaying(&w));
    }

    #[test]
    fn the_chart_gets_the_pointer_in_its_own_coordinates() {
        let mut w = workspace();
        w.handle(Event::PointerMoved { x: 300.0, y: 140.0 });
        let area = w.chart_area();
        assert_eq!(w.chart().pointer().position, Some((300.0 - area.x, 140.0 - area.y)));
        // Over a panel the chart has no pointer.
        w.handle(Event::PointerMoved { x: 900.0, y: 300.0 });
        assert_eq!(w.chart().pointer().position, None);
    }

    #[test]
    fn a_drag_stays_with_the_part_it_began_in() {
        let mut w = workspace();
        w.handle(Event::PointerMoved { x: 300.0, y: 300.0 });
        w.handle(Event::PointerDown);
        let replay = at(&w, "Replay");
        w.handle(Event::PointerMoved {
            x: replay.0,
            y: replay.1,
        });
        assert!(w.chart().pointer().pressed, "the chart still has the drag");
        w.handle(Event::PointerUp);
        assert!(!replaying(&w), "releasing a chart drag over a button is no click");
        assert_eq!(w.chart().pointer().position, None);
        // The button works once the drag is over.
        w.handle(Event::PointerDown);
        w.handle(Event::PointerUp);
        assert!(replaying(&w));
    }

    #[test]
    fn the_first_click_in_a_field_selects_it_the_next_puts_the_caret_and_a_copy_reaches_the_window() {
        let mut w = workspace();
        w.ctx.text = "BTCUSDT".into();
        w.build_scene(&mut Scene::new());
        let area = w.ui_state().rect_of("Symbol").unwrap();
        // Just after "BTC": the padding, then the width of the three letters.
        let pad = w.theme.padding;
        let x = area.x + pad + Font::ui().measure("BTC", w.theme.font_size) + 1.0;
        click(&mut w, (x, area.y + area.h / 2.0));
        w.handle(Event::Key(Key::Char('E')));
        assert_eq!(w.ctx.text, "E", "what was there is replaced");
        w.ctx.text = "BTCUSDT".into();
        click(&mut w, (x, area.y + area.h / 2.0));
        w.handle(Event::Key(Key::Backspace));
        assert_eq!(w.ctx.text, "BTUSDT");
        assert_eq!(w.take_copied(), None);
        w.handle(Event::Key(Key::SelectAll));
        w.handle(Event::Key(Key::Copy));
        assert_eq!(w.take_copied().as_deref(), Some("BTUSDT"));
        assert_eq!(w.take_copied(), None, "taken once");
    }

    #[test]
    fn a_drag_in_a_field_selects_and_shift_with_an_arrow_widens_it() {
        let mut w = workspace();
        w.ctx.text = "BTCUSDT".into();
        w.build_scene(&mut Scene::new());
        let area = w.ui_state().rect_of("Symbol").unwrap();
        let (pad, size) = (w.theme.padding, w.theme.font_size);
        let x_after = |s: &str| area.x + pad + Font::ui().measure(s, size);
        let y = area.y + area.h / 2.0;
        // In the field already: from after "B" to after "BTC", the button held, "TC" is selected and goes with a
        // key.
        click(&mut w, (x_after("B"), y));
        w.handle(Event::PointerMoved { x: x_after("B"), y });
        w.handle(Event::PointerDown);
        w.handle(Event::PointerMoved { x: x_after("BTC"), y });
        w.handle(Event::PointerUp);
        w.handle(Event::Key(Key::Char('x')));
        assert_eq!(w.ctx.text, "BxUSDT");
        // Moving without the button selects nothing.
        w.handle(Event::PointerMoved { x: area.x + 2.0, y });
        w.handle(Event::Key(Key::Char('y')));
        assert_eq!(w.ctx.text, "BxyUSDT");
        // Shift: the caret moves, the selection from where it was.
        w.handle(Event::Key(Key::SelectRight));
        w.handle(Event::Key(Key::SelectRight));
        w.handle(Event::Key(Key::Delete));
        assert_eq!(w.ctx.text, "BxyDT");
        w.handle(Event::Key(Key::SelectHome));
        w.handle(Event::Key(Key::Char('z')));
        assert_eq!(w.ctx.text, "zDT");
    }

    #[test]
    fn keys_go_to_the_field_being_typed_in_else_to_the_chart() {
        let mut w = workspace();
        let field = at(&w, "Symbol");
        click(&mut w, field);
        assert!(w.ui_state().has_focus());
        for c in "eth".chars() {
            w.handle(Event::Key(Key::Char(c)));
        }
        w.handle(Event::Key(Key::Alt('P')));
        assert_eq!(w.ctx.text, "eth");
        assert!(!replaying(&w), "Alt+Shift+P went to the field");
        // A field has no undo of its own: it keeps the keys, and the drawings stay as they are.
        w.handle(Event::Key(Key::Undo));
        assert!(w.ui_state().has_focus());
        w.handle(Event::Key(Key::Enter));
        assert_eq!(w.ctx.submitted.as_deref(), Some("eth"));
        assert!(!w.ui_state().has_focus());
        // Now the chart has the keys: Alt+Shift+P starts the replay.
        w.handle(Event::PointerMoved { x: 300.0, y: 300.0 });
        w.handle(Event::Key(Key::Alt('P')));
        assert!(replaying(&w));
        // A click elsewhere leaves a field too.
        click(&mut w, field);
        click(&mut w, (300.0, 300.0));
        assert!(!w.ui_state().has_focus());
    }

    #[test]
    fn the_chart_is_drawn_inside_its_area_and_the_panels_above_it() {
        let mut w = workspace();
        let mut scene = Scene::new();
        w.build_scene(&mut scene);
        let layers = scene.layers();
        let area = w.chart_area().clip();
        let inside = |c: &tickerlight_core::ClipRect| {
            c.x >= area.x && c.y >= area.y && c.x + c.w <= area.x + area.w && c.y + c.h <= area.y + area.h
        };
        let chart_layers: Vec<_> = layers.iter().filter(|l| l.clip.as_ref().is_some_and(inside)).collect();
        assert!(chart_layers.len() > 3, "the chart's layers, cut to its area");
        let texts: Vec<&str> = scene.texts.iter().map(|t| t.text.as_str()).collect();
        let (count, replay) = (
            texts.iter().rposition(|&t| t == "Count").unwrap(),
            texts.iter().rposition(|&t| t == "Replay").unwrap(),
        );
        let last_chart_text = chart_layers.last().unwrap().texts.end;
        assert!(count >= last_chart_text && replay >= last_chart_text);
    }

    #[test]
    fn of_two_charts_a_press_picks_the_active_one_and_the_other_follows_it() {
        let mut w = workspace();
        assert!(w.set_layout(Layout::Columns, chart).is_empty());
        w.build_scene(&mut Scene::new());
        let (a, b) = (w.area(Region::Chart(0)), w.area(Region::Chart(1)));
        assert!(a.x + a.w < b.x && a.y == b.y && a.w > 100.0);
        let linked = |w: &Workspace<State>, i| w.chart_at(i).resources().get::<LinkedTime>().and_then(|l| l.0);
        // The time under the pointer over the first shows on the second.
        let center = |a: Area| (a.x + a.w / 2.0, a.y + a.h / 2.0);
        let (x, y) = center(a);
        w.handle(Event::PointerMoved { x, y });
        assert!(linked(&w, 1).is_some());
        assert_eq!(linked(&w, 0), None);
        // A press on the second makes it active; dragging it scrolls the first to the same time.
        let (x, y) = center(b);
        w.handle(Event::PointerMoved { x, y });
        assert!(linked(&w, 0).is_some());
        w.handle(Event::PointerDown);
        assert_eq!(w.active(), 1);
        let before = w.right_time(0).unwrap();
        w.handle(Event::PointerMoved { x: x + 120.0, y });
        w.handle(Event::PointerUp);
        let (first, second) = (w.right_time(0).unwrap(), w.right_time(1).unwrap());
        assert!(first < before && (first - second).abs() < 1.0);
        // Back to one chart: the second goes, the first is active.
        assert_eq!(w.set_layout(Layout::One, chart).len(), 1);
        assert_eq!((w.charts(), w.active()), (1, 0));
    }
}
