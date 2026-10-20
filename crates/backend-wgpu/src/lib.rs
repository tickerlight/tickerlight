//! wgpu backend: draws the core scene on the GPU, in a desktop window or a browser canvas.
//!
//! `Renderer` draws a `Scene` into any texture. `run` opens a window (native) or a canvas in the page
//! (WebAssembly). `Gpu` and `render_to_rgba` render offscreen for tests and screenshots (native only).

#[cfg(not(target_arch = "wasm32"))]
mod headless;
mod line_renderer;
mod rect_renderer;
mod renderer;
mod text_renderer;
mod window;

#[cfg(not(target_arch = "wasm32"))]
pub use headless::{Gpu, render_to_rgba};
pub use renderer::Renderer;
#[cfg(target_arch = "wasm32")]
pub use window::run_on_canvas;
#[cfg(not(target_arch = "wasm32"))]
pub use window::run_placed;
pub use window::{Place, Places, run};
