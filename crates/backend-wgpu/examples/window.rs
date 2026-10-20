//! Opens a window with a sample chart: drag to pan, wheel to zoom.
//!
//! cargo run --release -p tickerlight-backend-wgpu --example window

use tickerlight_core::sample::random_walk;
use tickerlight_core::{Chart, Color, Sma};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tickerlight_backend_wgpu::run(Chart::new(random_walk(100_000)).with(Sma::new(50, Color::rgb(255, 183, 77))))
}
