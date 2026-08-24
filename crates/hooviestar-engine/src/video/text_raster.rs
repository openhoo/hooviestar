//! Deterministic Linux text rasterization. CPU work is limited to static text;
//! the resulting premultiplied RGBA8 bitmap is uploaded once by Vulkan.

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
use std::collections::HashMap;

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

fn rasterize(key: &TextKey) -> Result<RasterizedText> {
    let mut db = Database::new();
    db.load_system_fonts();
    let families = [Family::Name(&key.family), Family::SansSerif];
    let id = db
        .query(&Query {
            families: &families,
            weight: Weight(key.weight.clamp(1, 1000)),
            style: Style::Normal,
            ..Query::default()
        })
        .ok_or_else(|| anyhow!("font family '{}' is unavailable", key.family))?;
    let (font_bytes, face_index) = db
        .with_face_data(id, |data, index| (data.to_vec(), index))
        .ok_or_else(|| anyhow!("font data unavailable"))?;
    let font = Font::from_bytes(
        font_bytes,
        FontSettings {
            collection_index: face_index,
            ..FontSettings::default()
        },
    )
    .map_err(|e| anyhow!("font parse failed: {e:?}"))?;
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
    let px_size = f32::from_bits(key.size_bits).max(1.0);
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
                let background_alpha = u32::from(bg[3]);
                let output_alpha = source_alpha + background_alpha * inverse / 255;
                for channel in 0..3 {
                    let premultiplied = u32::from(fg[channel]) * source_alpha / 255
                        + u32::from(bg[channel]) * background_alpha * inverse / (255 * 255);
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

    #[test]
    fn multiline_raster_spans_the_requested_panel() {
        let key = TextKey {
            text: "Linux Vulkan Text".into(),
            family: "DejaVu Sans".into(),
            size_bits: 56.0f32.to_bits(),
            weight: 700,
            color: [255, 255, 255, 255],
            background: [32, 48, 80, 255],
            align: TextAlignKey::Center,
            width: 600,
            height: 200,
        };
        let raster = rasterize(&key).expect("rasterize");
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
        assert!(min < 200 && max > 400);
        let left_margin = min as i32;
        let right_margin = (599 - max) as i32;
        assert!((left_margin - right_margin).abs() < 30);
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
    fn rasterize_rejects_oversized_canvas_instead_of_overflowing() {
        let mut cache = TextCache::default();
        let key = TextKey {
            text: "overflow".into(),
            family: "DejaVu Sans".into(),
            size_bits: 32.0f32.to_bits(),
            weight: 400,
            color: [255, 255, 255, 255],
            background: [0, 0, 0, 255],
            align: TextAlignKey::Left,
            width: u32::MAX,
            height: u32::MAX,
        };
        assert!(cache.rasterize(key).is_err());
    }
}
