use crate::{Color, Columns, PriceScale, Scene, TimeScale};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CandleStyle {
    pub up: Color,
    pub down: Color,
    /// Body width as a fraction of the space one bar takes.
    pub body_ratio: f64,
    /// Wick width in logical pixels.
    pub wick_width: f64,
}

impl Default for CandleStyle {
    fn default() -> Self {
        Self {
            // Jade and coral, close in lightness so neither shouts.
            up: Color::rgb(30, 190, 135),
            down: Color::rgb(255, 110, 125),
            body_ratio: 0.8,
            wick_width: 1.0,
        }
    }
}

/// Appends one candlestick per column. Wicks and bodies are snapped to whole pixels.
/// `scale` is device pixels per logical pixel: it sets the wick width and the smallest body height.
pub fn emit_candles(
    columns: &Columns,
    time: &TimeScale,
    price: &PriceScale,
    style: &CandleStyle,
    scale: f64,
    scene: &mut Scene,
) {
    let line = (style.wick_width * scale).round().max(1.0);
    scene.rects.reserve(columns.len() * 2);
    for i in 0..columns.len() {
        let (open, close) = (columns.open[i], columns.close[i]);
        let color = if close >= open { style.up } else { style.down };

        let first = columns.first[i] as f64;
        let count = columns.count[i] as f64;
        let center = time.x_of(first + count / 2.0);
        let slot = count * time.bar_spacing;
        let mut body_w = (slot * style.body_ratio).round().max(line);
        // Same parity as the wick, so both are centered on the same point.
        if (body_w - line) % 2.0 != 0.0 {
            body_w -= 1.0;
        }

        let wick_x = (center - line / 2.0).round();
        let wick_top = price.y_of(columns.high[i]);
        let wick_bottom = price.y_of(columns.low[i]);
        scene.fill(
            wick_x as f32,
            wick_top as f32,
            line as f32,
            (wick_bottom - wick_top).max(line) as f32,
            color,
        );

        let body_top = price.y_of(open.max(close));
        let body_bottom = price.y_of(open.min(close));
        let body_x = (center - body_w / 2.0).round();
        scene.fill(
            body_x as f32,
            body_top as f32,
            body_w as f32,
            (body_bottom - body_top).max(line) as f32,
            color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_candle() -> Columns {
        Columns {
            open: vec![10.0],
            high: vec![20.0],
            low: vec![0.0],
            close: vec![15.0],
            first: vec![0],
            count: vec![1],
        }
    }

    #[test]
    fn candle_maps_price_and_index_to_pixels() {
        // Bar 0 is the last one: its center is at x = 100 - 10 / 2.
        let time = TimeScale::new(100.0, 10.0, 1.0);
        let price = PriceScale::linear(0.0, 20.0, 100.0);
        let mut scene = Scene::new();
        emit_candles(&one_candle(), &time, &price, &CandleStyle::default(), 1.0, &mut scene);

        assert_eq!(scene.rects.len(), 2);
        let wick = scene.rects[0];
        assert_eq!((wick.x, wick.y, wick.w, wick.h), (95.0, 0.0, 1.0, 100.0));
        let body = scene.rects[1];
        // 8 px would be off-center around a 1 px wick, so the body is 7 px: [92, 99) around 95.5.
        assert_eq!((body.x, body.y, body.w, body.h), (92.0, 25.0, 7.0, 25.0));
        assert_eq!(body.color, CandleStyle::default().up);
    }

    #[test]
    fn wick_and_body_share_the_same_center_at_any_scale() {
        for scale in [1.0, 1.5, 2.0, 3.0] {
            for spacing in [4.0, 7.0, 10.0, 13.0, 21.0] {
                let time = TimeScale::new(400.0, spacing, 20.0);
                let price = PriceScale::linear(0.0, 20.0, 100.0);
                let mut scene = Scene::new();
                emit_candles(&one_candle(), &time, &price, &CandleStyle::default(), scale, &mut scene);
                let (wick, body) = (scene.rects[0], scene.rects[1]);
                let (wick_mid, body_mid) = (wick.x + wick.w / 2.0, body.x + body.w / 2.0);
                assert!(
                    (wick_mid - body_mid).abs() < 1e-4,
                    "scale {scale} spacing {spacing}: {wick_mid} vs {body_mid}"
                );
            }
        }
    }

    #[test]
    fn wick_width_follows_the_scale() {
        let time = TimeScale::new(100.0, 20.0, 1.0);
        let price = PriceScale::linear(0.0, 20.0, 100.0);
        let mut scene = Scene::new();
        emit_candles(&one_candle(), &time, &price, &CandleStyle::default(), 2.0, &mut scene);
        assert_eq!(scene.rects[0].w, 2.0);
    }

    #[test]
    fn falling_candle_uses_the_down_color() {
        let mut cols = one_candle();
        cols.close = vec![5.0];
        let mut scene = Scene::new();
        let price = PriceScale::linear(0.0, 20.0, 100.0);
        emit_candles(
            &cols,
            &TimeScale::new(100.0, 10.0, 1.0),
            &price,
            &CandleStyle::default(),
            1.0,
            &mut scene,
        );
        assert_eq!(scene.rects[1].color, CandleStyle::default().down);
    }

    #[test]
    fn flat_candle_stays_visible() {
        let cols = Columns {
            open: vec![5.0],
            high: vec![5.0],
            low: vec![5.0],
            close: vec![5.0],
            first: vec![0],
            count: vec![1],
        };
        let price = PriceScale::fit((5.0, 5.0), 100.0, 0.0);
        let mut scene = Scene::new();
        emit_candles(
            &cols,
            &TimeScale::new(100.0, 10.0, 1.0),
            &price,
            &CandleStyle::default(),
            1.0,
            &mut scene,
        );
        assert!(scene.rects.iter().all(|r| r.y.is_finite() && r.h >= 1.0));
    }

    #[test]
    fn grouped_column_is_centered_over_its_bars() {
        let cols = Columns {
            open: vec![1.0],
            high: vec![2.0],
            low: vec![0.0],
            close: vec![1.0],
            first: vec![0],
            count: vec![4],
        };
        // 4 bars of 0.5 px: the group is 2 px wide and ends at x = 100, so its center is 99.
        let time = TimeScale::new(100.0, 0.5, 4.0);
        let price = PriceScale::linear(0.0, 2.0, 10.0);
        let mut scene = Scene::new();
        emit_candles(&cols, &time, &price, &CandleStyle::default(), 1.0, &mut scene);
        assert_eq!(scene.rects[0].x, 99.0);
        assert_eq!(scene.rects[1].w, 1.0);
    }
}
