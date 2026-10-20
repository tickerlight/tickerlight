/// How prices are laid out along the vertical axis.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum PriceMode {
    /// Equal prices, equal distances.
    #[default]
    Linear,
    /// Equal ratios, equal distances: for long histories and big moves. Needs positive prices.
    Log,
    /// Linear, but the axis reads the change from `base` (the first visible close) in percent.
    Percent { base: f64 },
}

/// Vertical mapping between price and pixels: `max` at the top (y = 0), `min` at `height`.
/// `min` and `max` are in the scale's own space: prices, or their logarithms in [`PriceMode::Log`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PriceScale {
    pub min: f64,
    pub max: f64,
    pub height: f64,
    pub mode: PriceMode,
}

impl PriceScale {
    /// A linear scale from `min` to `max` prices.
    pub fn linear(min: f64, max: f64, height: f64) -> Self {
        Self {
            min,
            max,
            height,
            mode: PriceMode::Linear,
        }
    }

    /// Fits `(low, high)` into `height`, leaving `margin` (a fraction of the range) above and below.
    pub fn fit(extent: (f64, f64), height: f64, margin: f64) -> Self {
        Self::fit_mode(extent, height, margin, PriceMode::Linear)
    }

    /// Like [`fit`](Self::fit) in the given mode. A log scale needs positive prices: with anything else
    /// in the extent it falls back to linear.
    pub fn fit_mode((low, high): (f64, f64), height: f64, margin: f64, mode: PriceMode) -> Self {
        let mode = match mode {
            PriceMode::Log if !(low > 0.0 && high > 0.0) => PriceMode::Linear,
            PriceMode::Percent { base } if !(base.is_finite() && base != 0.0) => PriceMode::Linear,
            other => other,
        };
        let space = |p: f64| if mode == PriceMode::Log { p.ln() } else { p };
        let (mut min, mut max) = (space(low), space(high));
        if max.partial_cmp(&min) != Some(std::cmp::Ordering::Greater) {
            // Flat (or invalid) range: open a window around the price so the scale stays usable.
            let pad = if mode == PriceMode::Log {
                0.005
            } else if min.is_finite() && min != 0.0 {
                min.abs() * 0.005
            } else {
                1.0
            };
            min -= pad;
            max += pad;
        }
        let pad = (max - min) * margin;
        Self {
            min: min - pad,
            max: max + pad,
            height,
            mode,
        }
    }

    fn space(&self, price: f64) -> f64 {
        match self.mode {
            PriceMode::Log => price.max(f64::MIN_POSITIVE).ln(),
            _ => price,
        }
    }

    pub fn y_of(&self, price: f64) -> f64 {
        (self.max - self.space(price)) / (self.max - self.min) * self.height
    }

    pub fn price_of(&self, y: f64) -> f64 {
        let value = self.max - y / self.height * (self.max - self.min);
        match self.mode {
            PriceMode::Log => value.exp(),
            _ => value,
        }
    }

    /// What the axis shows for `price`: the price, or its change from the base in percent.
    pub fn display(&self, price: f64) -> f64 {
        match self.mode {
            PriceMode::Percent { base } => (price / base - 1.0) * 100.0,
            _ => price,
        }
    }

    /// The price that the axis shows as `value`.
    pub fn from_display(&self, value: f64) -> f64 {
        match self.mode {
            PriceMode::Percent { base } => base * (1.0 + value / 100.0),
            _ => value,
        }
    }

    /// The prices at the bottom and the top of the scale.
    pub fn price_range(&self) -> (f64, f64) {
        (self.price_of(self.height), self.price_of(0.0))
    }
}

/// A round step (1, 2, 5 times a power of ten) that gives about `target` steps over `range`.
pub fn nice_step(range: f64, target: f64) -> f64 {
    let raw = range / target.max(1.0);
    if !(raw > 0.0 && raw.is_finite()) {
        return 1.0;
    }
    let pow = 10f64.powf(raw.log10().floor());
    let f = raw / pow;
    let nice = if f < 1.5 {
        1.0
    } else if f < 3.5 {
        2.0
    } else if f < 7.5 {
        5.0
    } else {
        10.0
    };
    nice * pow
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_adds_margin_and_maps_ends() {
        let s = PriceScale::fit((100.0, 200.0), 500.0, 0.1);
        assert_eq!((s.min, s.max), (90.0, 210.0));
        assert_eq!(s.y_of(210.0), 0.0);
        assert_eq!(s.y_of(90.0), 500.0);
        assert!((s.price_of(s.y_of(123.0)) - 123.0).abs() < 1e-9);
    }

    #[test]
    fn flat_range_gets_a_window() {
        let s = PriceScale::fit((50.0, 50.0), 100.0, 0.0);
        assert!(s.max > s.min);
        assert!(s.y_of(50.0).is_finite());
        let zero = PriceScale::fit((0.0, 0.0), 100.0, 0.0);
        assert!(zero.max > zero.min);
    }

    #[test]
    fn log_scales_give_equal_ratios_equal_distances() {
        let s = PriceScale::fit_mode((10.0, 1000.0), 400.0, 0.0, PriceMode::Log);
        assert_eq!(s.mode, PriceMode::Log);
        assert!((s.y_of(100.0) - 200.0).abs() < 1e-9, "the geometric middle");
        assert!((s.y_of(10.0) - 400.0).abs() < 1e-9 && s.y_of(1000.0).abs() < 1e-9);
        assert!((s.price_of(s.y_of(37.0)) - 37.0).abs() < 1e-9);
        assert!(((s.y_of(20.0) - s.y_of(10.0)) - (s.y_of(200.0) - s.y_of(100.0))).abs() < 1e-9);
        // Prices that are not positive make a log scale impossible.
        assert_eq!(
            PriceScale::fit_mode((-1.0, 5.0), 100.0, 0.0, PriceMode::Log).mode,
            PriceMode::Linear
        );
        assert!(
            PriceScale::fit_mode((5.0, 5.0), 100.0, 0.0, PriceMode::Log)
                .y_of(5.0)
                .is_finite()
        );
        assert!(s.y_of(0.0).is_finite(), "no NaN below the scale");
    }

    #[test]
    fn percent_scales_are_linear_with_a_relative_axis() {
        let s = PriceScale::fit_mode((90.0, 120.0), 300.0, 0.0, PriceMode::Percent { base: 100.0 });
        assert_eq!(s.y_of(105.0), PriceScale::fit((90.0, 120.0), 300.0, 0.0).y_of(105.0));
        assert!((s.display(110.0) - 10.0).abs() < 1e-12);
        assert!((s.from_display(-5.0) - 95.0).abs() < 1e-12);
        assert_eq!(
            PriceScale::fit_mode((1.0, 2.0), 100.0, 0.0, PriceMode::Percent { base: 0.0 }).mode,
            PriceMode::Linear
        );
        let (low, high) = s.price_range();
        assert!((low - 90.0).abs() < 1e-9 && (high - 120.0).abs() < 1e-9);
    }

    #[test]
    fn nice_steps_are_round() {
        assert_eq!(nice_step(100.0, 10.0), 10.0);
        assert_eq!(nice_step(23.0, 10.0), 2.0);
        assert_eq!(nice_step(0.7, 10.0), 0.05);
        assert_eq!(nice_step(0.0, 10.0), 1.0);
    }
}
