//! Text rasterization: fontdb for font discovery, fontdue for glyph coverage.
//! Produces a tight-bbox rgba8 bitmap (premultiplied color) + the offset where
//! the bitmap sits relative to the text origin (x = left edge, y = baseline).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::doc::{TextAlign, TextContent};

static FONT_DB: Mutex<Option<fontdb::Database>> = Mutex::new(None);
static FONTS: std::sync::LazyLock<Mutex<HashMap<String, fontdue::Font>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn db() -> std::sync::MutexGuard<'static, Option<fontdb::Database>> {
    let mut g = FONT_DB.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        let mut d = fontdb::Database::new();
        d.load_system_fonts();
        *g = Some(d);
    }
    g
}

/// pick a font: explicit path > family name (+bold/italic) > platform default
fn load_font(spec: &str, bold: bool, italic: bool) -> Result<fontdue::Font> {
    let key = format!("{spec}|{bold}|{italic}");
    if let Some(f) = FONTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
    {
        return Ok(f.clone());
    }

    // explicit file path
    let p = Path::new(spec);
    let font = if !spec.is_empty() && p.is_file() {
        let bytes = std::fs::read(p).with_context(|| format!("read font {}", p.display()))?;
        fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())
            .map_err(|e| anyhow::anyhow!("load font {}: {e}", p.display()))?
    } else {
        let guard = db();
        let d = guard.as_ref().unwrap();
        let weight = if bold {
            fontdb::Weight::BOLD
        } else {
            fontdb::Weight::NORMAL
        };
        let style = if italic {
            fontdb::Style::Italic
        } else {
            fontdb::Style::Normal
        };
        let mut q = d.query(&fontdb::Query {
            families: &[
                fontdb::Family::Name(spec),
                fontdb::Family::SansSerif,
                fontdb::Family::Serif,
            ],
            weight,
            style,
            ..fontdb::Query::default()
        });
        if q.is_none() {
            q = d.query(&fontdb::Query {
                families: &[fontdb::Family::SansSerif],
                ..fontdb::Query::default()
            });
        }
        let id = q.context("no usable system font found")?;
        let (src, index) = d.face_source(id).context("face source gone")?;
        let f = match src {
            fontdb::Source::File(path) => {
                let bytes = std::fs::read(&path)
                    .with_context(|| format!("read font {}", path.display()))?;
                fontdue::Font::from_bytes(
                    bytes,
                    fontdue::FontSettings {
                        collection_index: index,
                        ..Default::default()
                    },
                )
                .map_err(|e| anyhow::anyhow!("load font {:?}: {e}", path))?
            }
            fontdb::Source::Binary(data) | fontdb::Source::SharedFile(_, data) => {
                fontdue::Font::from_bytes(
                    data.as_ref().as_ref().to_vec(),
                    fontdue::FontSettings {
                        collection_index: index,
                        ..Default::default()
                    },
                )
                .map_err(|e| anyhow::anyhow!("load system font: {e}"))?
            }
        };
        f
    };
    FONTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, font.clone());
    Ok(font)
}

struct Glyph {
    bitmap: Vec<u8>,
    w: usize,
    h: usize,
    xmin: i32,
    ymin: i32,
    advance: f32,
}

fn raster_line(font: &fontdue::Font, s: &str, px: f32, tracking: f32) -> (Vec<Glyph>, f32, f32) {
    let mut out = Vec::new();
    let mut pen = 0.0f32;
    let mut max_above = 0.0f32;
    for ch in s.chars() {
        let (m, bmp) = font.rasterize(ch, px);
        if ch != ' ' && ch != '\t' {
            max_above = max_above.max(m.ymin as f32 + m.height as f32);
        }
        let adv = m.advance_width + tracking;
        out.push(Glyph {
            bitmap: bmp,
            w: m.width,
            h: m.height,
            xmin: m.xmin,
            ymin: m.ymin,
            advance: adv,
        });
        pen += adv;
    }
    (out, pen, max_above)
}

/// lay out `tc`: returns (rgba8, w, h, baseline_y_from_top).
/// Text is wrapped at wrap_width when > 0; whitespace breaks lines.
pub fn rasterize(tc: &TextContent) -> Result<(Vec<u8>, u32, u32)> {
    if tc.text.is_empty() {
        return Ok((Vec::new(), 0, 0));
    }
    let font = load_font(&tc.font, tc.bold, tc.italic)?;
    let px = tc.size.max(1.0);
    let metrics = font.horizontal_line_metrics(px);
    let (ascent, descent, gap) = metrics
        .map(|m| (m.ascent, m.descent, m.line_gap))
        .unwrap_or((px, -px * 0.25, 0.0));
    let line_step = ((ascent - descent + gap) * tc.leading.max(0.1)) as i32;
    let asc_i = ascent.ceil() as i32;
    let des_i = descent.floor() as i32;

    // split into lines: explicit newlines always break; greedy wrap at wrap_width
    let mut lines: Vec<String> = Vec::new();
    for para in tc.text.split('\n') {
        if tc.wrap_width <= 0.0 {
            lines.push(para.to_string());
            continue;
        }
        let mut cur = String::new();
        let mut cur_w = 0.0f32;
        for word in para.split_inclusive(' ') {
            let w: f32 = word
                .chars()
                .map(|c| font.metrics(c, px).advance_width + tc.tracking)
                .sum();
            if !cur.is_empty() && cur_w + w > tc.wrap_width {
                lines.push(cur.trim_end().to_string());
                cur = word.trim_start().to_string();
                cur_w = word.trim_start().chars().map(|c| font.metrics(c, px).advance_width + tc.tracking).sum();
            } else {
                cur.push_str(word);
                cur_w += w;
            }
        }
        lines.push(cur.trim_end().to_string());
    }

    // rasterize lines, measure total box
    let mut rast = Vec::with_capacity(lines.len());
    let mut max_w = 0.0f32;
    for l in &lines {
        let (glyphs, w, _above) = raster_line(&font, l, px, tc.tracking);
        max_w = max_w.max(w);
        rast.push(glyphs);
    }
    let w = (max_w.ceil() as i32).max(1);
    let h = (asc_i - des_i + line_step * (lines.len() as i32 - 1)).max(1);
    let mut buf = vec![0u8; (w * h * 4) as usize];

    let color = tc.color;
    for (li, glyphs) in rast.iter().enumerate() {
        let line_w: f32 = glyphs.iter().map(|g| g.advance).sum();
        let start_x: f32 = match tc.align {
            TextAlign::Left => 0.0,
            TextAlign::Center => (max_w - line_w) * 0.5,
            TextAlign::Right => max_w - line_w,
        };
        let baseline_y = asc_i + line_step * li as i32;
        let mut pen = start_x;
        for g in glyphs {
            let gx = (pen + g.xmin as f32).round() as i32;
            let gy = baseline_y - g.ymin - g.h as i32;
            for row in 0..g.h {
                for col in 0..g.w {
                    let a = g.bitmap[row * g.w + col];
                    if a == 0 {
                        continue;
                    }
                    let x = gx + col as i32;
                    let y = gy + row as i32;
                    if x < 0 || y < 0 || x >= w || y >= h {
                        continue;
                    }
                    let i = ((y * w + x) * 4) as usize;
                    let sa = a as f32 / 255.0 * color[3];
                    let da = buf[i + 3] as f32 / 255.0;
                    let ao = sa + da * (1.0 - sa);
                    if ao <= 0.0 {
                        continue;
                    }
                    for c in 0..3 {
                        let sc = color[c] * sa;
                        let dc = buf[i + c] as f32 / 255.0 * da;
                        buf[i + c] = ((sc + dc * (1.0 - sa)) / ao * 255.0).round() as u8;
                    }
                    buf[i + 3] = (ao * 255.0).round() as u8;
                }
            }
            pen += g.advance;
        }
    }
    Ok((buf, w as u32, h as u32))
}
