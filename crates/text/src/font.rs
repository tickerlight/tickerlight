use std::sync::OnceLock;

/// Noto Sans Regular, SIL Open Font License 1.1 (see `fonts/NotoSans-OFL.txt`).
const NOTO_SANS: &[u8] = include_bytes!("../fonts/NotoSans-Regular.ttf");

/// The size and place of a glyph's bitmap, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GlyphMetrics {
    pub width: usize,
    pub height: usize,
    /// Offset of the bitmap's left edge from the pen position.
    pub xmin: f32,
    /// Offset of the bitmap's bottom edge above the baseline.
    pub ymin: f32,
    pub advance: f32,
}

/// The built-in font, with the system's fonts behind it for the characters it lacks: Chinese, Japanese and
/// Korean, symbols and so on. Without a font for a character it is drawn as the built-in font's box.
pub struct Font {
    inner: fontdue::Font,
    #[cfg(not(target_arch = "wasm32"))]
    fallback: fallback::Fallback,
}

impl Font {
    /// The built-in UI font.
    pub fn ui() -> &'static Font {
        static UI: OnceLock<Font> = OnceLock::new();
        UI.get_or_init(|| Font::from_bytes(NOTO_SANS).expect("the built-in font is valid"))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, &'static str> {
        Ok(Self {
            inner: fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())?,
            #[cfg(not(target_arch = "wasm32"))]
            fallback: fallback::Fallback::default(),
        })
    }

    pub fn raw(&self) -> &fontdue::Font {
        &self.inner
    }

    /// Whether the font itself has `ch`, without the system's fonts.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn has(&self, ch: char) -> bool {
        ch.is_control() || self.inner.lookup_glyph_index(ch) != 0
    }

    /// The bitmap of `ch` at `size` px (one byte of coverage per pixel, row by row) and where it goes.
    pub fn rasterize(&self, ch: char, size: f32) -> (GlyphMetrics, Vec<u8>) {
        #[cfg(not(target_arch = "wasm32"))]
        if !self.has(ch)
            && let Some(glyph) = self.fallback.rasterize(ch, size)
        {
            return glyph;
        }
        let (m, bitmap) = self.inner.rasterize(ch, size);
        let metrics = GlyphMetrics {
            width: m.width,
            height: m.height,
            xmin: m.xmin as f32,
            ymin: m.ymin as f32,
            advance: m.advance_width,
        };
        (metrics, bitmap)
    }

    fn advance(&self, ch: char, size: f32) -> f32 {
        #[cfg(not(target_arch = "wasm32"))]
        if !self.has(ch)
            && let Some(advance) = self.fallback.advance(ch, size)
        {
            return advance;
        }
        self.inner.metrics(ch, size).advance_width
    }

    /// Width of `text` in pixels at `size` px, including kerning.
    pub fn measure(&self, text: &str, size: f32) -> f32 {
        let mut width = 0.0;
        let mut prev: Option<char> = None;
        for ch in text.chars() {
            if let Some(p) = prev {
                width += self.kerning(p, ch, size);
            }
            width += self.advance(ch, size);
            prev = Some(ch);
        }
        width
    }

    pub(crate) fn kerning(&self, left: char, right: char, size: f32) -> f32 {
        self.inner.horizontal_kern(left, right, size).unwrap_or(0.0)
    }

    /// Height of capital letters and digits at `size` px.
    pub fn cap_height(&self, size: f32) -> f32 {
        self.inner.metrics('H', size).bounds.height
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod fallback {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    use ab_glyph::{Font as _, FontArc, FontVec, PxScale, ScaleFont as _, VariableFont as _, point};

    use super::GlyphMetrics;

    /// Sans-serif families that cover much of Unicode, the most likely first: on Linux, Windows and macOS.
    const PREFERRED: [&str; 22] = [
        "Noto Sans",
        "Noto Sans CJK SC",
        "Noto Sans CJK JP",
        "Noto Sans CJK KR",
        "Noto Sans CJK TC",
        "Source Han Sans SC",
        "Microsoft YaHei",
        "Yu Gothic",
        "Meiryo",
        "Malgun Gothic",
        "Segoe UI",
        "Segoe UI Symbol",
        "PingFang SC",
        "Hiragino Sans",
        "Hiragino Sans GB",
        "Apple SD Gothic Neo",
        "Apple Symbols",
        "Droid Sans Fallback",
        "DejaVu Sans",
        "Noto Sans Symbols",
        "Noto Sans Symbols 2",
        "Arial Unicode MS",
    ];

    /// The system's fonts, looked at the first time a character is missing, and the font found for each
    /// character.
    #[derive(Default)]
    pub struct Fallback {
        system: OnceLock<System>,
        chars: Mutex<HashMap<char, Option<FontArc>>>,
    }

    struct System {
        db: fontdb::Database,
        /// Every face, the preferred families and regular styles first.
        order: Vec<fontdb::ID>,
        loaded: Mutex<HashMap<fontdb::ID, Option<FontArc>>>,
    }

    impl System {
        fn scan() -> Self {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            let rank = |face: &fontdb::FaceInfo| {
                let family = face.families.first().map_or("", |(name, _)| name.as_str());
                let preferred = PREFERRED.iter().position(|&p| p == family).unwrap_or(PREFERRED.len());
                let italic = face.style != fontdb::Style::Normal;
                (preferred, italic, face.weight.0.abs_diff(400))
            };
            let mut faces: Vec<&fontdb::FaceInfo> = db.faces().collect();
            faces.sort_by_key(|f| rank(f));
            let order = faces.iter().map(|f| f.id).collect();
            Self {
                db,
                order,
                loaded: Mutex::default(),
            }
        }

        fn covers(&self, id: fontdb::ID, ch: char) -> bool {
            self.db
                .with_face_data(id, |data, index| {
                    ttf_parser::Face::parse(data, index).is_ok_and(|face| face.glyph_index(ch).is_some())
                })
                .unwrap_or(false)
        }

        fn load(&self, id: fontdb::ID) -> Option<FontArc> {
            let mut loaded = self.loaded.lock().unwrap_or_else(|e| e.into_inner());
            loaded
                .entry(id)
                .or_insert_with(|| {
                    let mut font = self
                        .db
                        .with_face_data(id, |data, index| FontVec::try_from_vec_and_index(data.to_vec(), index))?
                        .ok()?;
                    // A variable font in its regular weight.
                    font.set_variation(b"wght", 400.0);
                    Some(FontArc::new(font))
                })
                .clone()
        }

        fn find(&self, ch: char) -> Option<FontArc> {
            self.order
                .iter()
                .find(|&&id| self.covers(id, ch))
                .and_then(|&id| self.load(id))
        }
    }

    impl Fallback {
        fn font(&self, ch: char) -> Option<FontArc> {
            let mut chars = self.chars.lock().unwrap_or_else(|e| e.into_inner());
            chars
                .entry(ch)
                .or_insert_with(|| self.system.get_or_init(System::scan).find(ch))
                .clone()
        }

        /// The scale at which the font's em is `size` px, as the built-in font is sized.
        fn scale(font: &FontArc, size: f32) -> PxScale {
            let em = font.units_per_em().unwrap_or(1000.0);
            PxScale::from(size * font.height_unscaled() / em)
        }

        pub fn advance(&self, ch: char, size: f32) -> Option<f32> {
            let font = self.font(ch)?;
            let scaled = font.as_scaled(Self::scale(&font, size));
            Some(scaled.h_advance(font.glyph_id(ch)))
        }

        pub fn rasterize(&self, ch: char, size: f32) -> Option<(GlyphMetrics, Vec<u8>)> {
            let font = self.font(ch)?;
            let scale = Self::scale(&font, size);
            let id = font.glyph_id(ch);
            let advance = font.as_scaled(scale).h_advance(id);
            let Some(outline) = font.outline_glyph(id.with_scale_and_position(scale, point(0.0, 0.0))) else {
                let metrics = GlyphMetrics {
                    advance,
                    ..GlyphMetrics::default()
                };
                return Some((metrics, Vec::new()));
            };
            let bounds = outline.px_bounds();
            let (width, height) = (bounds.width() as usize, bounds.height() as usize);
            let mut bitmap = vec![0; width * height];
            outline.draw(|x, y, coverage| {
                if let Some(p) = bitmap.get_mut(y as usize * width + x as usize) {
                    *p = (coverage.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            });
            let metrics = GlyphMetrics {
                width,
                height,
                xmin: bounds.min.x,
                ymin: -bounds.max.y,
                advance,
            };
            Some((metrics, bitmap))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_text() {
        let font = Font::ui();
        assert_eq!(font.measure("", 12.0), 0.0);
        let w = font.measure("0", 12.0);
        assert!(w > 4.0 && w < 12.0, "digit is {w} px wide");
        assert!((font.measure("0000", 12.0) - 4.0 * w).abs() < 1e-3);
        assert!(font.measure("0", 24.0) > 1.9 * w);
    }

    #[test]
    fn digits_have_the_same_width() {
        // Price labels must not jitter while the numbers change.
        let font = Font::ui();
        let zero = font.measure("0", 12.0);
        for d in '1'..='9' {
            assert!((font.measure(&d.to_string(), 12.0) - zero).abs() < 1e-3, "digit {d}");
        }
    }

    #[test]
    fn characters_the_font_lacks_come_from_the_system() {
        let font = Font::ui();
        let latin = font.measure("A", 20.0);
        for text in ["中文", "日本語", "한글", "→✓"] {
            let width = font.measure(text, 20.0);
            let inked = text.chars().all(|ch| {
                let (m, bitmap) = font.rasterize(ch, 20.0);
                m.width > 0 && bitmap.iter().any(|&p| p > 128)
            });
            // Only where the system has such fonts (it does on the machines this is built on).
            if inked {
                assert!(width > latin, "{text}: {width}");
            } else {
                eprintln!("no font for {text} on this system");
            }
        }
    }

    #[test]
    fn cap_height_is_reasonable() {
        let font = Font::ui();
        let h = font.cap_height(20.0);
        assert!(h > 12.0 && h < 16.0, "{h}");
    }

    #[test]
    fn cyrillic_is_covered() {
        let font = Font::ui();
        // A missing glyph would fall back to the same "notdef" box for every letter.
        assert_ne!(font.measure("Ш", 14.0), font.measure("I", 14.0));
    }
}
