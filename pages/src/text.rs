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
        font_from_face(d, id)?
    };
    FONTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(spec.to_string(), font.clone());
    Ok(font)
}

/// load a fontdue font for a fontdb face id
fn font_from_face(d: &fontdb::Database, id: fontdb::ID) -> Result<fontdue::Font> {
    let (src, index) = d.face_source(id).context("face source gone")?;
    match src {
        fontdb::Source::File(path) => {
            let bytes =
                std::fs::read(&path).with_context(|| format!("read font {}", path.display()))?;
            fontdue::Font::from_bytes(
                bytes,
                fontdue::FontSettings {
                    collection_index: index,
                    ..Default::default()
                },
            )
            .map_err(|e| anyhow::anyhow!("load font {:?}: {e}", path))
        }
        fontdb::Source::Binary(data) | fontdb::Source::SharedFile(_, data) => {
            fontdue::Font::from_bytes(
                data.as_ref().as_ref(),
                fontdue::FontSettings {
                    collection_index: index,
                    ..Default::default()
                },
            )
            .map_err(|e| anyhow::anyhow!("load system font: {e}"))
        }
    }
}

/// load exactly the family named — no fallback chain. `None` when the
/// system lacks it.
fn load_family_strict(name: &str) -> Option<fontdue::Font> {
    let guard = db();
    let d = guard.as_ref()?;
    let id = d.query(&fontdb::Query {
        families: &[fontdb::Family::Name(name)],
        ..fontdb::Query::default()
    })?;
    font_from_face(d, id).ok()
}

/// CJK-capable families tried in order for characters the primary font
/// doesn't cover (macOS first, then cross-platform).
const CJK_FAMILY_NAMES: &[&str] = &[
    "Hiragino Sans",
    "Hiragino Kaku Gothic ProN",
    "Yu Gothic",
    "Apple SD Gothic Neo",
    "Noto Sans CJK JP",
];

/// the subset of CJK_FAMILY_NAMES that exists on this system, loaded once.
static CJK_FONTS: std::sync::LazyLock<Vec<fontdue::Font>> = std::sync::LazyLock::new(|| {
    CJK_FAMILY_NAMES
        .iter()
        .filter_map(|n| load_family_strict(n))
        .collect()
});

/// a laid-out run's font resolution: the primary face plus per-character
/// CJK fallback for glyphs it doesn't cover. The first fallback family that
/// loads AND covers the char wins.
#[derive(Debug, Clone)]
pub struct FontStack {
    pub primary: fontdue::Font,
}

impl FontStack {
    /// font to measure/rasterize `c` with: primary when it has a glyph for
    /// it, else the first covering CJK fallback, else primary (.notdef box).
    pub fn font_for(&self, c: char) -> &fontdue::Font {
        if self.primary.lookup_glyph_index(c) != 0 {
            return &self.primary;
        }
        for f in CJK_FONTS.iter() {
            if f.lookup_glyph_index(c) != 0 {
                return f;
            }
        }
        &self.primary
    }
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
    /// byte offset into the source text where this line's content begins
    /// (used to slice overflow text for linked frames)
    pub src_start: usize,
}

/// result of flowing a text spec into a frame of width `wrap_w` pt
#[derive(Debug)]
pub struct Layout {
    /// per-char font resolution (primary + CJK fallback)
    pub fonts: FontStack,
    /// the size this layout was computed at, pt
    pub px: f32,
    pub lines: Vec<LaidLine>,
    /// distance between successive baselines, pt
    pub line_step: f32,
    /// ascent at this size (baseline of first line), pt
    pub ascent: f32,
    /// descent at this size (below-baseline ink), pt — negative
    pub descent: f32,
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
    let fonts = FontStack {
        primary: load_font(spec)?,
    };
    let px = size.max(0.1);
    let m = fonts
        .primary
        .horizontal_line_metrics(px)
        .map(|m| (m.ascent, m.descent, m.line_gap))
        .unwrap_or((px, -px * 0.25, 0.0));
    let (ascent, descent, gap) = m;
    let line_step = ((ascent - descent + gap) * leading.max(0.1)).max(0.1);
    let wrap_w = wrap_w.max(1.0);

    let width_of = |s: &str| -> f32 {
        s.chars()
            .map(|c| fonts.font_for(c).metrics(c, px).advance_width)
            .sum()
    };

    // split into lines, tracking each line's source byte offset so overflow
    // can be handed to a linked frame as raw text (it re-wraps there)
    let mut raw: Vec<(String, usize)> = Vec::new();
    let mut pbase = 0usize; // byte offset of the current paragraph in `text`
    for para in text.split('\n') {
        let mut cur = String::new();
        let mut cur_w = 0.0f32;
        let mut cur_start = pbase;
        let mut off = pbase; // byte offset of the current word
        for word in para.split_inclusive(' ') {
            let w = width_of(word);
            if !cur.is_empty() && cur_w + w > wrap_w {
                raw.push((cur.trim_end().to_string(), cur_start));
                let w_trim = word.trim_start();
                cur = w_trim.to_string();
                cur_w = width_of(w_trim);
                cur_start = off + (word.len() - w_trim.len());
            } else {
                cur.push_str(word);
                cur_w += w;
            }
            off += word.len();
        }
        raw.push((cur.trim_end().to_string(), cur_start));
        pbase += para.len() + 1; // + the '\n' split away
    }
    if raw.is_empty() {
        raw.push((String::new(), 0));
    }

    // a single word wider than the frame still overflows horizontally —
    // the frame clip keeps output sane; don't split words.

    let mut lines = Vec::with_capacity(raw.len());
    for (i, (l, src_start)) in raw.iter().enumerate() {
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
            src_start: *src_start,
        });
    }
    let flow_height = flow_height(ascent, descent, line_step, lines.len());
    Ok(Layout {
        fonts,
        px,
        lines,
        line_step,
        ascent,
        descent,
        flow_height,
    })
}

/// ink height of `n` laid lines (0 when n == 0)
fn flow_height(ascent: f32, descent: f32, line_step: f32, n: usize) -> f32 {
    if n == 0 {
        0.0
    } else {
        ascent - descent + line_step * (n - 1) as f32
    }
}

/// split a text run at a frame's available height: lays `text` out at
/// `wrap_w`, keeps the lines whose ink box fits inside `avail_h`, and hands
/// back the unlaid remainder text for a linked frame (or overset reporting).
pub fn split_flow(
    spec: &str,
    text: &str,
    size: f32,
    leading: f32,
    align: TextAlign,
    wrap_w: f32,
    avail_h: f32,
) -> Result<(Layout, String)> {
    let mut lay = layout_text(spec, text, size, leading, align, wrap_w)?;
    // a line fits when its ink box bottom stays inside avail_h
    let line_bottom = |i: usize| lay.line_step * i as f32 + (lay.ascent - lay.descent);
    let n_fit = lay
        .lines
        .iter()
        .enumerate()
        .take_while(|(i, _)| line_bottom(*i) <= avail_h + 0.001)
        .count();
    let rest = lay
        .lines
        .get(n_fit)
        .map(|l| text.get(l.src_start..).unwrap_or("").to_string())
        .unwrap_or_default();
    lay.lines.truncate(n_fit);
    lay.flow_height = flow_height(lay.ascent, lay.descent, lay.line_step, n_fit);
    Ok((lay, rest))
}

/// does `s` survive the WinAnsi (CP1252) encoding used for the fast Type1
/// text path? Non-covered runs are rasterized instead (`to_winansi` would
/// emit '?').
pub fn winansi_covers(s: &str) -> bool {
    s.chars().all(|c| winansi_byte(c).is_some())
}

/// one char's WinAnsi byte, or None when CP1252 can't represent it
fn winansi_byte(c: char) -> Option<u8> {
    let u = c as u32;
    if u < 0x80 {
        return Some(c as u8);
    }
    Some(match c {
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
        _ => return None,
    })
}

/// encode text for a WinAnsi (CP1252) PDF literal string: smart punctuation
/// and common symbols map to their WinAnsi bytes; anything else becomes '?'.
pub fn to_winansi(s: &str) -> Vec<u8> {
    s.chars().map(|c| winansi_byte(c).unwrap_or(b'?')).collect()
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
        // coverage check agrees: smart quotes fine, CJK not
        assert!(winansi_covers("héllo — “world”"));
        assert!(!winansi_covers("建築確認申請"));
    }

    #[test]
    fn split_flow_returns_fitting_lines_and_remainder() {
        let text = "one two three four five six seven eight nine ten eleven twelve";
        // full layout for reference
        let full = layout_text("", text, 12.0, 1.2, TextAlign::Left, 80.0).unwrap();
        assert!(full.lines.len() >= 3, "want multi-line layout");
        // a height that fits only the first line: its ink box is
        // ascent-descent; add half a step so exactly 1 line fits
        let avail = full.ascent - full.descent + 0.5;
        let (fits, rest) = split_flow("", text, 12.0, 1.2, TextAlign::Left, 80.0, avail).unwrap();
        assert_eq!(fits.lines.len(), 1);
        assert!(!rest.is_empty());
        // the remainder re-lays into the same wrapping as the tail of the
        // full layout (same width/font → identical breaks)
        let tail = layout_text("", &rest, 12.0, 1.2, TextAlign::Left, 80.0).unwrap();
        let mut recomposed: Vec<String> = fits.lines.iter().map(|l| l.text.clone()).collect();
        recomposed.extend(tail.lines.iter().map(|l| l.text.clone()));
        let original: Vec<String> = full.lines.iter().map(|l| l.text.clone()).collect();
        assert_eq!(recomposed, original);
    }

    #[test]
    fn split_flow_zero_height_gives_all_rest() {
        let text = "alpha beta gamma";
        let (fits, rest) = split_flow("", text, 12.0, 1.2, TextAlign::Left, 80.0, 0.0).unwrap();
        assert!(fits.lines.is_empty());
        assert_eq!(rest, text);
    }

    #[test]
    fn cjk_fallback_covers_cjk() {
        // when the primary face lacks a glyph, font_for must return a font
        // that actually covers it — on systems with no CJK font at all this
        // degrades to .notdef and the test has nothing to check
        if CJK_FONTS.is_empty() {
            eprintln!("no CJK fallback font installed; skipping");
            return;
        }
        let stack = FontStack {
            primary: load_font("Helvetica").unwrap(),
        };
        for ch in "建築確認申請".chars() {
            let f = stack.font_for(ch);
            assert!(f.lookup_glyph_index(ch) != 0, "no fallback glyph for {ch}");
        }
        // latin stays on the primary font
        assert!(std::ptr::eq(stack.font_for('A'), &stack.primary));
        // and the fallback actually rasterizes (non-empty bitmap)
        let f = stack.font_for('建');
        let (m, bmp) = f.rasterize('建', 24.0);
        assert!(m.width > 0 && m.height > 0);
        assert!(bmp.iter().any(|&a| a > 0));
    }
}
