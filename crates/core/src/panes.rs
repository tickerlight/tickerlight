use crate::PaneRect;
use crate::plugin::names;

/// The panes of a chart, top to bottom, and how they share the height of the plot. The main pane is
/// always first. Handlers reach it through [`EventCtx::pane_stack`](crate::EventCtx::pane_stack), which is how the
/// separator between two panes can be dragged.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneStack {
    entries: Vec<(&'static str, f32)>,
    scales: Vec<ScaleSetting>,
    manual: Vec<Option<(f64, f64)>>,
}

/// How a pane's price axis is laid out. `Percent` reads changes from the first visible close, so it
/// suits panes of prices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScaleSetting {
    #[default]
    Linear,
    Log,
    Percent,
}

impl ScaleSetting {
    /// `mode` on, or back to linear when it is on already.
    pub fn toggled(self, mode: Self) -> Self {
        if self == mode { Self::Linear } else { mode }
    }
}

impl Default for PaneStack {
    fn default() -> Self {
        Self {
            entries: vec![(names::MAIN_PANE, 1.0)],
            scales: vec![ScaleSetting::Linear],
            manual: vec![None],
        }
    }
}

impl PaneStack {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.entries.iter().map(|e| e.0)
    }

    pub fn name(&self, index: usize) -> Option<&'static str> {
        self.entries.get(index).map(|e| e.0)
    }

    pub fn weight(&self, index: usize) -> Option<f32> {
        self.entries.get(index).map(|e| e.1)
    }

    /// Sets the weight of the pane at `index`. Nonsense (zero, negative, not a number) is ignored.
    pub fn set_weight(&mut self, index: usize, weight: f32) {
        if let Some(entry) = self.entries.get_mut(index)
            && weight.is_finite()
            && weight > 0.0
        {
            entry.1 = weight;
        }
    }

    pub fn scale(&self, index: usize) -> ScaleSetting {
        self.scales.get(index).copied().unwrap_or_default()
    }

    /// Changing the setting also gives the pane back its autoscale: a manual range in one kind of
    /// axis means nothing in another.
    pub fn set_scale(&mut self, index: usize, setting: ScaleSetting) {
        if let Some(slot) = self.scales.get_mut(index) {
            *slot = setting;
            self.manual[index] = None;
        }
    }

    /// The range the user set by hand for the pane (`(min, max)` in the scale's own space, see
    /// [`PriceScale`](crate::PriceScale)), instead of the autoscale.
    pub fn manual_range(&self, index: usize) -> Option<(f64, f64)> {
        self.manual.get(index).copied().flatten()
    }

    /// Sets a range by hand, or `None` to autoscale again.
    pub fn set_manual_range(&mut self, index: usize, range: Option<(f64, f64)>) {
        let valid = range.is_none_or(|(min, max)| min.is_finite() && max.is_finite() && max > min);
        if let (Some(slot), true) = (self.manual.get_mut(index), valid) {
            *slot = range;
        }
    }

    pub(crate) fn add(&mut self, name: &'static str, weight: f32) {
        let weight = if weight.is_finite() && weight > 0.0 {
            weight
        } else {
            1.0
        };
        match self.entries.iter_mut().find(|e| e.0 == name) {
            Some(entry) => entry.1 = weight,
            None => {
                self.entries.push((name, weight));
                self.scales.push(ScaleSetting::Linear);
                self.manual.push(None);
            }
        }
    }

    pub(crate) fn remove(&mut self, name: &str) -> bool {
        match self.entries.iter().position(|e| e.0 == name) {
            Some(i) if name != names::MAIN_PANE => {
                self.entries.remove(i);
                self.scales.remove(i);
                self.manual.remove(i);
                true
            }
            _ => false,
        }
    }

    /// The panes for a plot `total` pixels tall: heights in proportion to the weights, whole pixels,
    /// at least one each; the last takes what is left.
    pub fn layout(&self, total: f32) -> Vec<PaneRect> {
        let sum: f32 = self.entries.iter().map(|e| e.1).sum();
        let mut top = 0.0;
        let mut out = Vec::with_capacity(self.entries.len());
        for (i, entry) in self.entries.iter().enumerate() {
            let height = if i + 1 == self.entries.len() {
                total - top
            } else {
                (total * entry.1 / sum).round()
            };
            let height = height.max(1.0);
            out.push(PaneRect { top, height });
            top += height;
        }
        out
    }
}
