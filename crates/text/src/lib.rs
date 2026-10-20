//! Text for the charting library: the font, measuring, a glyph atlas and layout.
//!
//! Pure Rust and platform independent. Backends upload [`Atlas::pixels`] as a texture and
//! draw the [`GlyphQuad`]s that [`layout`] produces; the core uses [`Font::measure`] to size
//! boxes around labels.

mod atlas;
mod font;
mod layout;

pub use atlas::{Atlas, Glyph};
pub use font::{Font, GlyphMetrics};
pub use layout::{Align, GlyphQuad, layout};
