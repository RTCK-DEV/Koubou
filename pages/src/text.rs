//! Text layout: fontdb for discovery, fontdue for metrics. Produces wrapped
//! lines positioned inside a frame box (frame-local pt, y down, top-aligned).
//! Shared by the PDF writer and the PNG preview rasterizer so both backends
//! break lines identically.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::model::TextAlign;

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

/// resolve a font spec: absolute file path > family name > Helvetica >
/// any sans-serif. Never fails unless the system has no usable font at all.
pub fn load_font(spec: &str) -> Result<fontdue::Font> {
    if let Some(f) = FONTS.lock().unwrap_or_else(|e| e.into_inner()).get(spec) {
        return Ok(f.clone());
    }

    let p = Path::new(spec);
    let font = if !spec.is_empty() && p.is_file() {
        let bytes = std::fs::read(p).with_context(|| format!("read font {}", p.display()))?;
        fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())
            .map_err(|e| anyhow::anyhow!("load font {}: {e}", p.display()))?
    } else {
        let guard = db();
        let d = guard.as_ref().context("fontdb not initialized")?;
        let mut families: Vec<fontdb::Family> = Vec::new();
        if !spec.is_empty() {
            families.push(fontdb::Family::Name(spec));
        }
        // requested-name fallback chain: Helvetica, then any sans
        families.push(fontdb::Family::Name("Helvetica"));
        families.push(fontdb::Family::SansSerif);
        let id = d
            .query(&fontdb::Query {
                families: &families,
                ..fontdb::Query::default()
            })
            .context("no usable system font found")?;
        let (src, index) = d.face_source(id).context("face source gone")?;
        match src {
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
                    data.as_ref().as_ref(),
                    fontdue::FontSettings {
                        collection_index: index,
                        ..Default::default()
                    },
                )
                .map_err(|e| anyhow::anyhow!("load system font: {e}"))?
            }
        }
    };
    FONTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(spec.to_string(), font.clone());
    Ok(font)
}

/// one wrapped line positioned inside the frame
#[derive(Debug, Clone)]
pub struct LaidLine {
    pub text: String,
    /// x offset from the frame's left edge, pt (alignment already applied)
    pub x_off: f32,
    /// baseline y from the frame's top edge, pt
    pub baseline: f32,
    /// unaligned width of the line, pt (for decorations/hit-testing)
    pub width: f32,
}

/// result of flowing a text spec into a frame of width `wrap_w` pt
#[derive(Debug)]
pub struct Layout {
    pub font: fontdue::Font,
    pub lines: Vec<LaidLine>,
    /// distance between successive baselines, pt
    pub line_step: f32,
    /// ascent at this size (baseline of first line), pt
    pub ascent: f32,
    /// total ink height of the flow, pt; > frame h means overflow
    pub flow_height: f32,
}

/// greedy word-wrap `text` at `wrap_w` pt; explicit newlines always break.
/// `leading` multiplies the font's natural line height.
pub fn layout_text(
    spec: &str,
    text: &str,
    size: f32,
    leading: f32,
    align: TextAlign,
    wrap_w: f32,
) -> Result<Layout> {
    let font = load_font(spec)?;
    let px = size.max(0.1);
    let m = font
        .horizontal_line_metrics(px)
        .map(|m| (m.ascent, m.descent, m.line_gap))
        .unwrap_or((px, -px * 0.25, 0.0));
    let (ascent, descent, gap) = m;
    let line_step = ((ascent - descent + gap) * leading.max(0.1)).max(0.1);
    let wrap_w = wrap_w.max(1.0);

    let width_of = |s: &str| -> f32 { s.chars().map(|c| font.metrics(c, px).advance_width).sum() };

    // split into lines
    let mut raw: Vec<String> = Vec::new();
    for para in text.split('\n') {
        let mut cur = String::new();
        let mut cur_w = 0.0f32;
        for word in para.split_inclusive(' ') {
            let w = width_of(word);
            if !cur.is_empty() && cur_w + w > wrap_w {
                raw.push(cur.trim_end().to_string());
                let w_trim = word.trim_start();
                cur = w_trim.to_string();
                cur_w = width_of(w_trim);
            } else {
                cur.push_str(word);
                cur_w += w;
            }
        }
        raw.push(cur.trim_end().to_string());
    }
    if raw.is_empty() {
        raw.push(String::new());
    }

    // a single word wider than the frame still overflows horizontally —
    // the frame clip keeps output sane; don't split words.

    let mut lines = Vec::with_capacity(raw.len());
    for (i, l) in raw.iter().enumerate() {
        let w = width_of(l);
        let x_off = match align {
            TextAlign::Left => 0.0,
            TextAlign::Center => ((wrap_w - w) * 0.5).max(0.0),
            TextAlign::Right => (wrap_w - w).max(0.0),
        };
        lines.push(LaidLine {
            text: l.clone(),
            x_off,
            baseline: ascent + line_step * i as f32,
            width: w,
        });
    }
    let flow_height = ascent - descent + line_step * (raw.len().saturating_sub(1)) as f32;
    Ok(Layout {
        font,
        lines,
        line_step,
        ascent,
        flow_height,
    })
}

/// encode text for a WinAnsi (CP1252) PDF literal string: smart punctuation
/// and common symbols map to their WinAnsi bytes; anything else becomes '?'.
pub fn to_winansi(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| {
            let u = c as u32;
            if u < 0x80 {
                c as u8
            } else {
                match c {
                    '\u{00A0}'..='\u{00FF}' => c as u8, // Latin-1 range == WinAnsi
                    '\u{2013}' => 0x96,                 // en dash
                    '\u{2014}' => 0x97,                 // em dash
                    '\u{2018}' => 0x91,                 // ‘
                    '\u{2019}' => 0x92,                 // ’
                    '\u{201A}' => 0x82,                 // ‚
                    '\u{201C}' => 0x93,                 // “
                    '\u{201D}' => 0x94,                 // ”
                    '\u{201E}' => 0x84,                 // „
                    '\u{2020}' => 0x86,                 // †
                    '\u{2021}' => 0x87,                 // ‡
                    '\u{2022}' => 0x95,                 // •
                    '\u{2026}' => 0x85,                 // …
                    '\u{2030}' => 0x89,                 // ‰
                    '\u{2039}' => 0x8B,                 // ‹
                    '\u{203A}' => 0x9B,                 // ›
                    '\u{20AC}' => 0x80,                 // €
                    '\u{2122}' => 0x99,                 // ™
                    '\u{0152}' => 0x8C,                 // Œ
                    '\u{0153}' => 0x9C,                 // œ
                    '\u{0160}' => 0x8A,                 // Š
                    '\u{0161}' => 0x9A,                 // š
                    '\u{0178}' => 0x9F,                 // Ÿ
                    '\u{017D}' => 0x8E,                 // Ž
                    '\u{017E}' => 0x9E,                 // ž
                    '\u{0192}' => 0x83,                 // ƒ
                    '\u{02C6}' => 0x88,                 // ˆ
                    '\u{02DC}' => 0x98,                 // ˜
                    _ => b'?',
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_inside_frame_width() {
        // all laid lines must fit the frame width (modulo overlong words)
        let l = layout_text(
            "",
            "the quick brown fox jumps over the lazy dog again and again",
            12.0,
            1.2,
            TextAlign::Left,
            100.0,
        )
        .unwrap();
        assert!(l.lines.len() > 1, "expected wrapping, got {:?}", l.lines);
        for line in &l.lines {
            assert!(line.width <= 100.01, "line too wide: {}pt", line.width);
        }
        // baselines strictly increase by line_step
        for w in l.lines.windows(2) {
            assert!((w[1].baseline - w[0].baseline - l.line_step).abs() < 0.01);
        }
    }

    #[test]
    fn newlines_and_alignment() {
        let l = layout_text("Helvetica", "a\nbb", 12.0, 1.0, TextAlign::Right, 200.0).unwrap();
        assert_eq!(l.lines.len(), 2);
        assert!(l.lines[1].x_off < l.lines[0].x_off || l.lines[0].x_off > 0.0);
        let c = layout_text("Helvetica", "mid", 12.0, 1.0, TextAlign::Center, 200.0).unwrap();
        let expected = (200.0 - c.lines[0].width) * 0.5;
        assert!((c.lines[0].x_off - expected).abs() < 0.01);
    }

    #[test]
    fn winansi_encodes() {
        assert_eq!(to_winansi("abc"), b"abc");
        assert_eq!(to_winansi("“q”…"), vec![0x93, b'q', 0x94, 0x85]);
        assert_eq!(to_winansi("日本語"), vec![b'?', b'?', b'?']);
    }
}
