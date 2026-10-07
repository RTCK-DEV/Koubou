//! Text rasterization for text clips and subtitle cues.
//!
//! This ffmpeg build has no drawtext/libass, so text clips and (optionally)
//! subtitle cues are rasterized to RGBA here — the same image feeds both
//! `tl.renderFrame` compositing and the `tl.render` overlay graph, keeping
//! preview and export identical.

use std::sync::OnceLock;

use anyhow::{Context, Result};
use image::{Rgba, RgbaImage};

/// default sans-serif font, loaded once per process
static FONT: OnceLock<Option<fontdue::Font>> = OnceLock::new();

fn default_font() -> Result<&'static fontdue::Font> {
    FONT.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let families = [
            fontdb::Family::SansSerif,
            fontdb::Family::Name("Helvetica"),
            fontdb::Family::Name("Arial"),
        ];
        let id = db.query(&fontdb::Query {
            families: &families,
            weight: fontdb::Weight::NORMAL,
            stretch: fontdb::Stretch::Normal,
            style: fontdb::Style::Normal,
        })?;
        let face = db.face(id)?;
        let font = match &face.source {
            fontdb::Source::File(path) => {
                let bytes = std::fs::read(path).ok()?;
                fontdue::Font::from_bytes(
                    bytes,
                    fontdue::FontSettings {
                        collection_index: face.index,
                        ..Default::default()
                    },
                )
                .ok()
            }
            fontdb::Source::Binary(data) | fontdb::Source::SharedFile(_, data) => {
                let owned: Vec<u8> = data.as_ref().as_ref().to_vec();
                fontdue::Font::from_bytes(
                    owned,
                    fontdue::FontSettings {
                        collection_index: face.index,
                        ..Default::default()
                    },
                )
                .ok()
            }
        };
        font
    })
    .as_ref()
    .context("no usable system font found for text rasterization")
}

/// rasterize `text` at `px` point size into a tight RGBA image:
/// white fill with a dark outline so it reads over any background.
/// `scale` multiplies the point size. Returns an empty 1x1 image for empty
/// text (callers treat it as nothing to draw).
pub fn rasterize(text: &str, px: f32, scale: f32) -> Result<RgbaImage> {
    if text.trim().is_empty() {
        return Ok(RgbaImage::new(1, 1));
    }
    let font = default_font()?;
    let size = (px * scale.max(0.01)).clamp(4.0, 2048.0);
    // measure lines
    let lines: Vec<&str> = text.split('\n').collect();
    let line_h = (size * 1.25).ceil().max(1.0) as u32;
    let mut width = 1u32;
    for line in &lines {
        let mut w = 0.0f32;
        let mut prev: Option<char> = None;
        for ch in line.chars() {
            if let Some(p) = prev {
                w += font.horizontal_kern(p, ch, size).unwrap_or(0.0);
            }
            let (m, _) = font.rasterize(ch, size);
            w += m.advance_width;
            prev = Some(ch);
        }
        width = width.max(w.ceil().max(1.0) as u32);
    }
    let outline = (size / 14.0).ceil().max(1.0) as u32;
    let pad = outline + 2;
    let w = width + pad * 2;
    let h = (line_h * lines.len() as u32).max(1) + pad * 2;
    let mut cov = vec![0.0f32; (w * h) as usize]; // coverage buffer for outline expansion
    let mut ink = vec![0.0f32; (w * h) as usize]; // interior glyph coverage
    let mut y_pen = pad as f32;
    for line in &lines {
        let mut x_pen = pad as f32;
        let mut prev: Option<char> = None;
        for ch in line.chars() {
            if let Some(p) = prev {
                x_pen += font.horizontal_kern(p, ch, size).unwrap_or(0.0);
            }
            let (m, bmp) = font.rasterize(ch, size);
            prev = Some(ch);
            if m.width > 0 && m.height > 0 {
                let gx = (x_pen + m.xmin as f32).round() as i64;
                let gy = (y_pen + m.ymin as f32).round() as i64;
                for by in 0..m.height {
                    for bx in 0..m.width {
                        let c = bmp[by * m.width + bx] as f32 / 255.0;
                        if c <= 0.0 {
                            continue;
                        }
                        let px = gx + bx as i64;
                        let py = gy + by as i64;
                        if px < 0 || py < 0 || px >= w as i64 || py >= h as i64 {
                            continue;
                        }
                        let idx = (py as u32 * w + px as u32) as usize;
                        ink[idx] = ink[idx].max(c);
                        // dilate coverage by `outline` px for the border
                        for oy in -(outline as i64)..=(outline as i64) {
                            for ox in -(outline as i64)..=(outline as i64) {
                                let qx = px + ox;
                                let qy = py + oy;
                                if qx < 0 || qy < 0 || qx >= w as i64 || qy >= h as i64 {
                                    continue;
                                }
                                let qi = (qy as u32 * w + qx as u32) as usize;
                                cov[qi] = cov[qi].max(c);
                            }
                        }
                    }
                }
            }
            x_pen += m.advance_width;
        }
        y_pen += line_h as f32;
    }
    let mut img = RgbaImage::new(w, h);
    for py in 0..h {
        for px in 0..w {
            let i = (py * w + px) as usize;
            let fill = ink[i];
            let edge = cov[i];
            if edge <= 0.0 && fill <= 0.0 {
                continue;
            }
            // outline = coverage minus fill; fill wins where present
            let (r, g, b, a) = if fill > 0.0 {
                (255u8, 255u8, 255u8, fill)
            } else {
                (20u8, 20u8, 20u8, edge)
            };
            img.put_pixel(px, py, Rgba([r, g, b, (a * 255.0).round() as u8]));
        }
    }
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rasterizes_nonempty() {
        let img = rasterize("hello", 48.0, 1.0).unwrap();
        assert!(img.width() > 10 && img.height() > 10);
        let lit = img.pixels().filter(|p| p[3] > 0).count();
        assert!(lit > 50);
    }

    #[test]
    fn empty_text_empty_image() {
        let img = rasterize("", 48.0, 1.0).unwrap();
        assert!(img.width() <= 8);
    }
}
