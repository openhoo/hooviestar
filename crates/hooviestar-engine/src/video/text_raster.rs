//! Deterministic Linux text rasterization. CPU work is limited to static text;
//! the resulting straight-alpha RGBA8 bitmap is uploaded once by Vulkan;
//! premultiplication happens in the fragment shader (shaders/item.frag).

use crate::project::TextAlign;
use anyhow::{Result, anyhow};
use fontdb::{Database, Family, Query, Style, Weight};
use fontdue::{
    Font, FontSettings,
    layout::{
        CoordinateSystem, HorizontalAlign, Layout, LayoutSettings, TextStyle, VerticalAlign,
        WrapStyle,
    },
};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

/// Upper bound for font pixel sizes; corrupt project files must not trigger
/// unbounded glyph coverage allocations in fontdue.
const MAX_PX_SIZE: f32 = 4096.0;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct TextKey {
    pub text: String,
    pub family: String,
    pub size_bits: u32,
    pub weight: u16,
    pub color: [u8; 4],
    pub background: [u8; 4],
    pub align: TextAlignKey,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum TextAlignKey {
    Left,
    Center,
    Right,
}
#[derive(Clone, Debug)]
pub struct RasterizedText {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}
#[derive(Default)]
pub struct TextCache {
    entries: HashMap<TextKey, RasterizedText>,
}

impl TextCache {
    pub fn rasterize(&mut self, key: TextKey) -> Result<&RasterizedText> {
        if !self.entries.contains_key(&key) {
            let value = rasterize(&key)?;
            self.entries.insert(key.clone(), value);
        }
        self.entries
            .get(&key)
            .ok_or_else(|| anyhow!("text cache insert failed"))
    }
    #[cfg(test)]
    /// Miss-path rasterization against an injected font database so tests can
    /// run hermetically on the bundled fixture instead of system fonts.
    fn rasterize_with_db(&mut self, db: &Database, key: TextKey) -> Result<&RasterizedText> {
        if !self.entries.contains_key(&key) {
            let value = rasterize_with_db(db, &key)?;
            self.entries.insert(key.clone(), value);
        }
        self.entries
            .get(&key)
            .ok_or_else(|| anyhow!("text cache insert failed"))
    }
    pub fn retain(&mut self, keep: impl Fn(&TextKey) -> bool) {
        self.entries.retain(|key, _| keep(key));
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

pub fn parse_color(value: &str) -> [u8; 4] {
    const FALLBACK: [u8; 4] = [255, 255, 255, 255];
    let Some(hex) = value.strip_prefix('#') else {
        return FALLBACK;
    };
    let bytes = hex.as_bytes();
    if bytes.len() != 6 && bytes.len() != 8 {
        return FALLBACK;
    }
    let nibble = |byte: u8| -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    };
    let channel = |index: usize| -> Option<u8> {
        Some(nibble(bytes[index])? * 16 + nibble(bytes[index + 1])?)
    };
    let alpha = if bytes.len() == 8 {
        channel(6)
    } else {
        Some(255)
    };
    match (channel(0), channel(2), channel(4), alpha) {
        (Some(r), Some(g), Some(b), Some(a)) => [r, g, b, a],
        _ => FALLBACK,
    }
}

/// System font database, built once: scanning font directories on every text
/// cache miss stalls the render thread for tens of milliseconds per new key.
fn font_db() -> &'static Database {
    static FONT_DB: LazyLock<Database> = LazyLock::new(|| {
        let mut db = Database::new();
        db.load_system_fonts();
        db
    });
    &FONT_DB
}

/// Parsed faces keyed by fontdb face id: constructing a fontdue
/// Font copies and parses every table of the whole file, so caching here keeps
/// that cost at once per face instead of once per unique text key on the
/// render thread.
static PARSED_FONTS: LazyLock<Mutex<HashMap<fontdb::ID, Arc<Font>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
fn load_font(db: &Database, id: fontdb::ID) -> Result<Arc<Font>> {
    if let Some(font) = PARSED_FONTS.lock().get(&id) {
        return Ok(Arc::clone(font));
    }
    let (font_bytes, face_index) = db
        .with_face_data(id, |data, index| (data.to_vec(), index))
        .ok_or_else(|| anyhow!("font data unavailable"))?;
    let font = Arc::new(
        Font::from_bytes(
            font_bytes,
            FontSettings {
                collection_index: face_index,
                ..FontSettings::default()
            },
        )
        .map_err(|e| anyhow!("font parse failed: {e:?}"))?,
    );
    PARSED_FONTS
        .lock()
        .entry(id)
        .or_insert_with(|| Arc::clone(&font));
    Ok(font)
}

fn rasterize(key: &TextKey) -> Result<RasterizedText> {
    rasterize_with_db(font_db(), key)
}

/// Db-injected rasterization core: prod resolves the system database once;
/// tests pass their own in-memory database built from the bundled fixture
/// font, so coverage never depends on which system fonts a host ships.
fn rasterize_with_db(db: &Database, key: &TextKey) -> Result<RasterizedText> {
    let families = [Family::Name(&key.family), Family::SansSerif];
    let id = db
        .query(&Query {
            families: &families,
            weight: Weight(key.weight.clamp(1, 1000)),
            style: Style::Normal,
            ..Query::default()
        })
        .ok_or_else(|| anyhow!("font family '{}' is unavailable", key.family))?;
    let font = load_font(db, id)?;
    let width = key.width.max(1) as usize;
    let height = key.height.max(1) as usize;
    let bg = key.background;
    let fg = key.color;
    let size = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| {
            anyhow!(
                "text raster {}x{} exceeds size limits",
                key.width,
                key.height
            )
        })?;
    let mut pixels = vec![0u8; size];

    for px in pixels.as_chunks_mut::<4>().0 {
        px.copy_from_slice(&bg);
    }
    // Upper bound so a corrupt project file cannot ask fontdue for unbounded
    // glyph coverage buffers. clamp() would propagate NaN into those buffers;
    // the max/min chain deliberately collapses NaN/negative to 1.
    #[allow(clippy::manual_clamp)]
    let px_size = f32::from_bits(key.size_bits).max(1.0).min(MAX_PX_SIZE);
    let horizontal_align = match key.align {
        TextAlignKey::Left => HorizontalAlign::Left,
        TextAlignKey::Center => HorizontalAlign::Center,
        TextAlignKey::Right => HorizontalAlign::Right,
    };
    let fonts = [font];
    let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
    layout.reset(&LayoutSettings {
        x: 0.0,
        y: 0.0,
        max_width: Some(width as f32),
        max_height: Some(height as f32),
        horizontal_align,
        vertical_align: VerticalAlign::Middle,
        line_height: 1.25,
        wrap_style: WrapStyle::Word,
        wrap_hard_breaks: true,
    });
    layout.append(&fonts, &TextStyle::new(&key.text, px_size, 0));
    for positioned in layout.glyphs() {
        if positioned.parent.is_control() {
            continue;
        }
        let (metrics, coverage) = fonts[positioned.font_index].rasterize_config(positioned.key);
        let gx = positioned.x.round() as i32;
        let gy = positioned.y.round() as i32;
        for yy in 0..metrics.height {
            for xx in 0..metrics.width {
                let px = gx + xx as i32;
                let py = gy + yy as i32;
                if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                    continue;
                }
                let coverage = coverage[yy * metrics.width + xx] as u32;
                let source_alpha = coverage * u32::from(fg[3]) / 255;
                let inverse = 255 - source_alpha;
                let dst = (py as usize * width + px as usize) * 4;
                // Blend against the current destination pixel: earlier
                // glyphs may already cover this pixel (overlapping rasters),
                // so use the accumulated alpha/color, not the canvas fill.
                let background_alpha = u32::from(pixels[dst + 3]);
                let output_alpha = source_alpha + background_alpha * inverse / 255;
                for channel in 0..3 {
                    let premultiplied = u32::from(fg[channel]) * source_alpha / 255
                        + u32::from(pixels[dst + channel]) * background_alpha * inverse
                            / (255 * 255);
                    pixels[dst + channel] = (premultiplied * 255)
                        .checked_div(output_alpha)
                        .unwrap_or(0)
                        .min(255) as u8;
                }
                pixels[dst + 3] = output_alpha.min(255) as u8;
            }
        }
    }
    Ok(RasterizedText {
        width: width as u32,
        height: height as u32,
        rgba8: pixels,
    })
}

impl From<TextAlign> for TextAlignKey {
    fn from(value: TextAlign) -> Self {
        match value {
            TextAlign::Left => Self::Left,
            TextAlign::Center => Self::Center,
            TextAlign::Right => Self::Right,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Coarse test-only lock for every test that touches PARSED_FONTS:
    // adwaita_db() clears that process-global parse cache while cargo runs
    // test bodies on parallel threads, so a concurrent clear landing between
    // another test's populate and its assertions fails it spuriously.
    // Serializing these tests fixes that. This must stay separate from
    // PARSED_FONTS's own mutex: rasterize_with_db/load_font take that
    // internally and parking_lot Mutexes are non-reentrant, so holding it
    // across those calls would self-deadlock.
    static FONT_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Hermetic single-face database over the bundled fixture: every test
    /// runs against exactly these bytes on every host - no system fonts,
    /// no silent skips. Fixture provenance: adwaita-fonts package, license
    /// copied alongside as AdwaitaFonts-LICENSE.txt.
    fn adwaita_db() -> Database {
        let mut db = Database::new();
        db.load_font_data(
            include_bytes!("../../tests/fixtures/fonts/AdwaitaSans-Regular.ttf").to_vec(),
        );
        // fontdb IDs are per-database slotmap keys: identical fixture bytes
        // yield the same ID in every fresh database, so without this reset all
        // tests would share whichever cache entry ran first. Clearing here
        // keeps each test provably cold-started - future cross-test cache
        // reuse fails attributably in the test that caused it instead of
        // passing silently on shared state.
        PARSED_FONTS.lock().clear();
        db
    }

    fn key_for(text: &str) -> TextKey {
        TextKey {
            text: text.to_string(),
            family: "Adwaita Sans".into(),
            size_bits: 24.0f32.to_bits(),
            weight: 400,
            color: [255, 255, 255, 255],
            background: [0, 0, 0, 255],
            align: TextAlignKey::Left,
            width: 320,
            height: 96,
        }
    }

    #[test]
    fn multiline_raster_spans_the_requested_panel() {
        let _font_guard = FONT_TEST_LOCK.lock();
        let key = TextKey {
            text: "Linux Vulkan Text".into(),
            family: "Adwaita Sans".into(),
            size_bits: 56.0f32.to_bits(),
            weight: 700,
            color: [255, 255, 255, 255],
            background: [32, 48, 80, 255],
            align: TextAlignKey::Center,
            width: 600,
            height: 200,
        };
        let raster = rasterize_with_db(&adwaita_db(), &key).expect("rasterize");
        let changed = raster
            .rgba8
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .filter_map(|(index, pixel)| (*pixel != key.background).then_some(index % 600))
            .collect::<Vec<_>>();
        let min = changed.iter().copied().min().expect("text pixels");
        let max = changed.iter().copied().max().expect("text pixels");
        // Gemessen an Adwaita Sans Regular 56px/700: Glyphenspann
        // x=[62, 539] in der 600px-Panels. Schwellen sitzen >=35px innerhalb
        // des Messwerts und bleiben streng genug, um Ausrichtungs- und
        // Blend-Regressionen (z. B. Doppel-Praemultiplikation verfaelscht
        // Pixelwerte und kollabiert den erkannten Spann) zu erkennen.
        assert!(min < 100 && max > 500);
        let left_margin = min as i32;
        let right_margin = (599 - max) as i32;
        assert!((left_margin - right_margin).abs() < 15);
    }

    #[test]
    fn parse_color_rejects_multibyte_input_without_panicking() {
        assert_eq!(parse_color("#aéabc"), [255, 255, 255, 255]);
        assert_eq!(parse_color("#aéabcde"), [255, 255, 255, 255]);
    }

    #[test]
    fn parse_color_still_parses_ascii_hex() {
        assert_eq!(parse_color("#ff8000"), [255, 128, 0, 255]);
        assert_eq!(parse_color("#FF800080"), [255, 128, 0, 128]);
        assert_eq!(parse_color("nope"), [255, 255, 255, 255]);
        assert_eq!(parse_color("#zzzzzz"), [255, 255, 255, 255]);
    }

    #[test]
    fn parse_color_wrong_length_hex_returns_fallback() {
        // Covers the explicit length guard: anything that is neither 6 nor 8
        // nibbles after '#' must yield the white FALLBACK constant.
        const FALLBACK: [u8; 4] = [255, 255, 255, 255];
        assert_eq!(parse_color("#fff"), FALLBACK);
        assert_eq!(parse_color("#12345"), FALLBACK);
        assert_eq!(parse_color("#112233445"), FALLBACK);
    }

    #[test]
    fn text_cache_inserts_on_miss_reuses_entry_and_supports_retain_clear() {
        let _font_guard = FONT_TEST_LOCK.lock();
        let db = adwaita_db();
        let key = key_for("cache probe");
        let mut cache = TextCache::default();

        // Miss: rasterizes through the injected db and stores the entry.
        let first = cache
            .rasterize_with_db(&db, key.clone())
            .expect("miss rasterizes and inserts");
        assert_eq!(first.width, 320);
        assert_eq!(first.height, 96);
        assert_eq!(first.rgba8.len(), 320 * 96 * 4);

        // Hit: poison the stored bytes via the private map; the next lookup
        // must return exactly the stored entry, proving the miss path did
        // not fire again.
        cache.entries.get_mut(&key).unwrap().rgba8[0] = 42;
        let again = cache
            .rasterize_with_db(&db, key.clone())
            .expect("hit returns stored entry");
        assert_eq!(again.rgba8[0], 42);

        cache.retain(|kept| kept.text != key.text);
        assert!(cache.entries.is_empty());

        cache
            .rasterize_with_db(&db, key.clone())
            .expect("reinsert after retain");
        assert_eq!(cache.entries.len(), 1);
        cache.clear();
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn rasterize_rejects_oversized_canvas_instead_of_overflowing() {
        let _font_guard = FONT_TEST_LOCK.lock();
        let mut cache = TextCache::default();
        let key = TextKey {
            text: "overflow".into(),
            family: "Adwaita Sans".into(),
            size_bits: 32.0f32.to_bits(),
            weight: 400,
            color: [255, 255, 255, 255],
            background: [0, 0, 0, 255],
            align: TextAlignKey::Left,
            width: u32::MAX,
            height: u32::MAX,
        };
        assert!(cache.rasterize_with_db(&adwaita_db(), key).is_err());
    }

    #[test]
    fn same_family_second_text_skips_font_reparse() {
        let _font_guard = FONT_TEST_LOCK.lock();
        let db = adwaita_db();
        let families = [Family::Name("Adwaita Sans"), Family::SansSerif];
        let id = db
            .query(&Query {
                families: &families,
                weight: Weight(400),
                style: Style::Normal,
                ..Query::default()
            })
            .expect("face id");
        // Face id resolved BEFORE any rasterization: the assertions below can
        // only pass if rasterize_with_db itself primes PARSED_FONTS - an
        // explicit load_font could otherwise self-prime the cache and mask a
        // production path that stopped caching.
        rasterize_with_db(&db, &key_for("first sample")).expect("first rasterize");
        let cached = PARSED_FONTS
            .lock()
            .get(&id)
            .cloned()
            .expect("rasterize path primes PARSED_FONTS");
        rasterize_with_db(&db, &key_for("second sample")).expect("second rasterize");
        assert!(
            PARSED_FONTS.lock().contains_key(&id),
            "rasterize path must populate PARSED_FONTS"
        );
        // Second load_font for the same face id must return the cached Arc
        // clone instead of re-parsing the font file - and it must be the very
        // Arc the rasterize path stored, not a freshly parsed face.
        let first = load_font(&db, id).expect("cached parse");
        let second = load_font(&db, id).expect("cached reuse");
        assert!(Arc::ptr_eq(&first, &second));
        assert!(Arc::ptr_eq(&cached, &first));
    }
}
