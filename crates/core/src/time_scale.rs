use core::ops::Range;

/// Horizontal mapping between bar indices and pixels.
///
/// Index space: bar `i` covers `[i, i + 1)`, its center is `i + 0.5`. The right edge of the
/// plot (`x = width`) sits at index `right`, and one bar is `bar_spacing` pixels wide.
/// Like in lightweight-charts the axis is by bar, not by wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeScale {
    /// Plot width in pixels.
    pub width: f64,
    pub bar_spacing: f64,
    pub right: f64,
}

const MAX_BAR_SPACING: f64 = 200.0;
const ABSOLUTE_MIN_BAR_SPACING: f64 = 1e-4;
const MIN_BAR_SPACING_FLOOR: f64 = 1.0;

impl TimeScale {
    pub fn new(width: f64, bar_spacing: f64, right: f64) -> Self {
        Self {
            width,
            bar_spacing,
            right,
        }
    }

    /// Pixel x of a position in index space.
    pub fn x_of(&self, index: f64) -> f64 {
        self.width + (index - self.right) * self.bar_spacing
    }

    /// Position in index space under pixel x.
    pub fn index_of(&self, x: f64) -> f64 {
        self.right + (x - self.width) / self.bar_spacing
    }

    /// Indices of bars that overlap the plot, clipped to `0..len`.
    pub fn visible(&self, len: usize) -> Range<usize> {
        let left = (self.right - self.width / self.bar_spacing).floor().max(0.0);
        let start = (left as usize).min(len);
        let end = (self.right.ceil().max(0.0) as usize).min(len);
        start..end.max(start)
    }

    /// Drag by `dx` pixels: positive `dx` moves the content right, towards older bars.
    pub fn pan(&mut self, dx: f64) {
        self.right -= dx / self.bar_spacing;
    }

    /// Multiplies the bar spacing by `factor` (within the limits for `len` bars), keeping the
    /// index under pixel `anchor_x` in place.
    pub fn zoom(&mut self, factor: f64, anchor_x: f64, len: usize) {
        let anchor = self.index_of(anchor_x);
        // Limit the spacing first: `right` must be computed from the spacing that will be used,
        // otherwise hitting the zoom limit throws the chart far away from the data.
        self.bar_spacing = self.limit_spacing(self.bar_spacing * factor, len);
        self.right = anchor - (anchor_x - self.width) / self.bar_spacing;
        self.clamp(len);
    }

    /// Keeps the scale in sensible bounds for `len` bars: no zooming out past
    /// "everything fits", and no more than half a screen of empty space on either side.
    pub fn clamp(&mut self, len: usize) {
        self.bar_spacing = self.limit_spacing(self.bar_spacing, len);
        let len = len as f64;
        let half_screen = 0.5 * self.width / self.bar_spacing;
        let min_right = half_screen.min(len);
        let max_right = (len + half_screen).max(min_right);
        self.right = self.right.clamp(min_right, max_right);
    }

    fn limit_spacing(&self, spacing: f64, len: usize) -> f64 {
        // Zooming out stops when everything fits, but never below one pixel per bar is
        // demanded: with few bars "everything fits" would be a huge minimum, forcing bars wide.
        let fits = self.width / (len as f64 + 1.0);
        let min = fits.clamp(ABSOLUTE_MIN_BAR_SPACING, MIN_BAR_SPACING_FLOOR);
        spacing.clamp(min, MAX_BAR_SPACING.max(min))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scale() -> TimeScale {
        TimeScale::new(1000.0, 10.0, 100.0)
    }

    #[test]
    fn maps_index_to_pixels_and_back() {
        let s = scale();
        assert_eq!(s.x_of(100.0), 1000.0);
        assert_eq!(s.x_of(99.5), 995.0);
        assert_eq!(s.index_of(0.0), 0.0);
        assert_eq!(s.index_of(s.x_of(42.25)), 42.25);
    }

    #[test]
    fn visible_range_covers_partly_visible_bars() {
        let mut s = scale();
        assert_eq!(s.visible(1000), 0..100);
        s.right = 150.5;
        // Left edge at index 50.5, so bar 50 is partly visible; right edge cuts bar 150.
        assert_eq!(s.visible(1000), 50..151);
        assert_eq!(s.visible(120), 50..120);
        s.right = 5.0;
        assert_eq!(s.visible(1000), 0..5);
    }

    #[test]
    fn dragging_right_shows_older_bars() {
        let mut s = scale();
        s.pan(50.0);
        assert_eq!(s.right, 95.0);
    }

    #[test]
    fn zoom_keeps_the_anchor_bar_in_place() {
        let mut s = scale();
        let before = s.index_of(300.0);
        s.zoom(2.0, 300.0, 1000);
        assert_eq!(s.bar_spacing, 20.0);
        assert!((s.index_of(300.0) - before).abs() < 1e-9);
        assert!((s.x_of(before) - 300.0).abs() < 1e-9);
    }

    #[test]
    fn clamp_limits_zoom_out_and_scrolling() {
        let mut s = scale();
        s.zoom(1e-9, 500.0, 500);
        assert_eq!(s.bar_spacing, 1.0); // one pixel per bar is enough for 500 bars
        let mut s = scale();
        s.zoom(1e-9, 500.0, 100_000);
        assert!((s.bar_spacing - 1000.0 / 100_001.0).abs() < 1e-9); // many bars: until they all fit

        let mut s = scale();
        s.right = -1e6;
        s.clamp(500);
        assert_eq!(s.right, 50.0); // half a screen (50 bars) of empty space on the left
        s.right = 1e6;
        s.clamp(500);
        assert_eq!(s.right, 500.0 + 50.0); // and on the right

        let mut s = scale();
        s.bar_spacing = 1e6;
        s.clamp(500);
        assert_eq!(s.bar_spacing, 200.0);
    }

    #[test]
    fn zooming_out_repeatedly_at_the_limit_keeps_the_data_in_view() {
        let mut s = scale();
        for _ in 0..200 {
            s.zoom(0.5, 300.0, 500);
        }
        // The picture did not run away from the data: at least half of it is still on screen.
        assert_eq!(s.bar_spacing, 1.0);
        let visible = s.visible(500);
        assert!(visible.len() >= 250, "{visible:?}");
    }

    #[test]
    fn few_bars_are_not_stretched() {
        let mut s = TimeScale::new(1000.0, 8.0, 8.0);
        s.clamp(3);
        assert_eq!(s.bar_spacing, 8.0);
    }

    #[test]
    fn empty_data_does_not_break_clamp() {
        let mut s = scale();
        s.clamp(0);
        assert!(s.right.is_finite() && s.bar_spacing.is_finite());
        assert_eq!(s.visible(0), 0..0);
    }
}
