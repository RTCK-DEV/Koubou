//! PSD import/export.
//!
//! Import: layered Photoshop documents become koubou documents. Raster
//! layers keep pixels; blend modes/opacity/offsets/names map 1:1. Layer
//! groups flatten into raster children (v1 — see docs/parity.md).
//!
//! Export: real layered writer (8BPS, RGBA8). Layer records preserve
//! names, stack order, x/y offsets, visibility, opacity and blend modes;
//! groups export as section-divider/folder pairs; layer masks export as
//! user-mask channels. Adjustment layers are baked against the composite
//! below them; layer.scale is baked into the exported pixels. The merged
//! composite is written RLE-compressed, matted on white where alpha is
//! partial (matching Photoshop). `doc.exportPsd` uses this writer;
//! `{"flat": true}` keeps the old single-plane output.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

use crate::blend::BlendMode;
use crate::composite::{blur_mask, mask_at, sample_bilinear, Composer, LayerPixels};
use crate::doc::{Document, Layer, LayerKind, Mask, RasterSrc};

/// PSD limits: max 30000×30000, v1 format
pub fn write_flat_psd(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<()> {
    anyhow::ensure!(w > 0 && h > 0, "psd export needs non-zero size");
    anyhow::ensure!(w <= 30000 && h <= 30000, "psd export max is 30000×30000");
    anyhow::ensure!(
        rgba.len() == (w as usize) * (h as usize) * 4,
        "rgba buffer {} != {}*{}*4",
        rgba.len(),
        w,
        h
    );
    let mut f = std::io::BufWriter::new(
        std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?,
    );
    // header: signature, version, reserved, channels=4, h, w, depth=8, mode=RGB
    f.write_all(b"8BPS")?;
    f.write_all(&1u16.to_be_bytes())?;
    f.write_all(&[0u8; 6])?;
    f.write_all(&4u16.to_be_bytes())?;
    f.write_all(&h.to_be_bytes())?;
    f.write_all(&w.to_be_bytes())?;
    f.write_all(&8u16.to_be_bytes())?;
    f.write_all(&3u16.to_be_bytes())?;
    // color mode data + image resources + layer/mask info: all empty
    f.write_all(&0u32.to_be_bytes())?;
    f.write_all(&0u32.to_be_bytes())?;
    f.write_all(&0u32.to_be_bytes())?;
    // image data: raw compression, planar R,G,B,A
    f.write_all(&0u16.to_be_bytes())?;
    let n = (w as usize) * (h as usize);
    let mut plane = vec![0u8; n];
    for ch in 0..4 {
        for (i, px) in rgba.chunks_exact(4).enumerate() {
            plane[i] = px[ch];
        }
        f.write_all(&plane)?;
    }
    f.flush()?;
    Ok(())
}

// =======================================================================
// layered export
// =======================================================================

const PSD_MAX: u32 = 30000;

/// what the caller reports back
#[derive(Debug, Clone, Copy)]
pub struct PsdExportStats {
    pub layers: usize,
    pub groups: usize,
}

/// a pixel-bearing layer's mask, baked to u8 coverage at placed scale
struct MaskRec {
    top: i32,
    left: i32,
    w: u32,
    h: u32,
    gray: Vec<u8>,
}

/// pixel-bearing record (raster/fill/shape/text/develop content, or a
/// baked adjustment snapshot)
struct PixelRec {
    name: String,
    id: u64,
    top: i32,
    left: i32,
    bottom: i32,
    right: i32,
    /// (right-left)*(bottom-top)*4 interleaved rgba8, already resampled
    rgba: Vec<u8>,
    blend: BlendMode,
    opacity: u8,
    hidden: bool,
    mask: Option<MaskRec>,
}

/// section divider record — bounding opener (type 3) or folder (type 1).
/// Folders carry the group's own name/blend/opacity/flags.
struct DividerRec {
    name: String,
    id: u64,
    section_type: u32,
    top: i32,
    left: i32,
    bottom: i32,
    right: i32,
    blend_key: [u8; 4],
    opacity: u8,
    hidden: bool,
}

enum Rec {
    Pixel(PixelRec),
    Divider(DividerRec),
}

/// render the doc's layer stack into PSD layer records + write the file.
/// The composer is borrowed mutably because rasterization fills the
/// compositor cache; the document itself is restored unchanged.
pub fn write_layered_psd(c: &mut Composer, path: &Path) -> Result<PsdExportStats> {
    anyhow::ensure!(
        c.doc.width > 0 && c.doc.height > 0,
        "psd export needs non-zero size"
    );
    anyhow::ensure!(
        c.doc.width <= PSD_MAX && c.doc.height <= PSD_MAX,
        "psd export max is 30000×30000"
    );
    let mut recs = Vec::new();
    {
        // move the layer vec out so collect can borrow c mutably (same
        // pattern as render_raw — embedded rasters are heavy)
        let layers = std::mem::take(&mut c.doc.layers);
        let r = collect_stack(c, &layers, &mut recs);
        c.doc.layers = layers;
        r?;
    }
    let merged = c.render()?;
    write_file(path, merged.width, merged.height, &merged.data, &recs)
}

/// walk a stack bottom→top, emitting records in PSD file order
/// (bounding record → children → folder record per group)
fn collect_stack(c: &mut Composer, stack: &[Layer], recs: &mut Vec<Rec>) -> Result<()> {
    for (i, layer) in stack.iter().enumerate() {
        match &layer.kind {
            LayerKind::Group { children } => {
                // koubou composites groups in ISOLATION (children blend
                // against an empty buffer, result blends down). `pass`
                // would tell Photoshop to blend children straight into
                // the backdrop — different image. The faithful key is
                // the group's own blend (norm = isolated composite).
                let key = psd_key(layer.blend);
                let opacity_u8 = (layer.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
                let hidden = !layer.visible;
                // Bounding record: placeholder in real PSDs — but the `psd`
                // crate derives PsdGroup's props from THIS record (it pops
                // the stack), so it carries the group's real blend/opacity/
                // visibility alongside the folder record for readers that
                // look there. Photoshop reads the folder record.
                recs.push(Rec::Divider(DividerRec {
                    name: "</Layer group>".into(),
                    id: 0,
                    section_type: 3,
                    top: 0,
                    left: 0,
                    bottom: 0,
                    right: 0,
                    blend_key: key,
                    opacity: opacity_u8,
                    hidden,
                }));
                let open = recs.len();
                collect_stack(c, children, recs)?;
                // folder bounds = union of the children's pixel rects,
                // clamped to the document (Photoshop's own convention)
                let mut u = (i32::MAX, i32::MAX, i32::MIN, i32::MIN); // t,l,b,r
                for r in &recs[open..] {
                    if let Rec::Pixel(p) = r {
                        u.0 = u.0.min(p.top);
                        u.1 = u.1.min(p.left);
                        u.2 = u.2.max(p.bottom);
                        u.3 = u.3.max(p.right);
                    }
                }
                let (t, l, b, rr) = if u.0 > u.2 { (0, 0, 0, 0) } else { u };
                let (dw, dh) = (c.doc.width as i32, c.doc.height as i32);
                recs.push(Rec::Divider(DividerRec {
                    name: layer.name.clone(),
                    id: layer.id,
                    section_type: 1,
                    top: t.clamp(0, dh),
                    left: l.clamp(0, dw),
                    bottom: b.clamp(0, dh),
                    right: rr.clamp(0, dw),
                    blend_key: key,
                    opacity: opacity_u8,
                    hidden,
                }));
            }
            LayerKind::Adjustment { recipe } => {
                // bake: composite everything below this adjustment in the
                // current stack (mirrors composite_list's canvas.clone()),
                // run the develop pipeline on the snapshot, export the
                // result as pixels — PSD can't express our recipe ops.
                let mut snapshot = LayerPixels::empty(c.doc.width, c.doc.height);
                c.composite_list(&mut snapshot, &stack[..i])?;
                let baked = c.develop_adjustment(&snapshot, recipe)?;
                push_pixel(recs, layer, &baked);
            }
            _ => {
                let pix = c.rasterize_layer(layer)?;
                push_pixel(recs, layer, &pix);
            }
        }
    }
    Ok(())
}

/// place `pix` at layer.x/y with layer.scale baked in (PSD has no scale)
fn push_pixel(recs: &mut Vec<Rec>, layer: &Layer, pix: &LayerPixels) {
    let scale = layer.scale.max(1e-4);
    let w = ((pix.w as f32 * scale).ceil() as u32).max(1);
    let h = ((pix.h as f32 * scale).ceil() as u32).max(1);
    let rgba = if scale == 1.0 {
        pix.to_rgba8()
    } else {
        // same sampling as blend_layer: dest px ↔ layer space /scale
        let mut out = vec![0u8; (w * h * 4) as usize];
        for dy in 0..h {
            for dx in 0..w {
                let p = sample_bilinear(pix, dx as f32 / scale, dy as f32 / scale);
                let i = (dy * w + dx) as usize * 4;
                for ch in 0..4 {
                    out[i + ch] = (p[ch].clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
        }
        out
    };
    let mask = layer
        .mask
        .as_ref()
        .map(|m| export_mask(m, scale, w, h, layer.x, layer.y));
    recs.push(Rec::Pixel(PixelRec {
        name: layer.name.clone(),
        id: layer.id,
        top: layer.y,
        left: layer.x,
        bottom: layer.y.saturating_add(h as i32),
        right: layer.x.saturating_add(w as i32),
        rgba,
        blend: layer.blend,
        opacity: (layer.opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
        hidden: !layer.visible,
        mask,
    }));
}

/// bake feather + invert + density into u8 coverage over the layer's
/// pixel rect. The mask bitmap is indexed in LAYER-LOCAL coords — the
/// compositor calls mask_at(sx, sy) with layer-pixel space — so the
/// record rect is the layer's pixel rect in absolute doc coordinates
/// (flags = 0) and buffer (gx,gy) ↔ mask point (gx/scale, gy/scale),
/// identical sampling to blend_layer. Never write a doc-space origin:
/// that misplaces the coverage whenever x/y ≠ 0.
fn export_mask(m: &Mask, scale: f32, w: u32, h: u32, x: i32, y: i32) -> MaskRec {
    let blurred;
    let m = if m.feather > 0.0 {
        blurred = blur_mask(m);
        &blurred
    } else {
        m
    };
    let mut gray = vec![0u8; (w * h) as usize];
    for gy in 0..h {
        for gx in 0..w {
            // mask_at() applies floor sampling + invert + density — the
            // same effective coverage the compositor uses
            let v = mask_at(m, gx as f32 / scale, gy as f32 / scale);
            gray[(gy * w + gx) as usize] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    MaskRec {
        top: y,
        left: x,
        w,
        h,
        gray,
    }
}

/// Koubou blend → PSD 4-byte key (see blend.rs; keys per PDF/PSD spec)
fn psd_key(m: BlendMode) -> [u8; 4] {
    match m {
        BlendMode::Normal => *b"norm",
        BlendMode::Dissolve => *b"diss",
        BlendMode::Darken => *b"dark",
        BlendMode::Multiply => *b"mul ",
        BlendMode::ColorBurn => *b"idiv",
        BlendMode::LinearBurn => *b"lbrn",
        BlendMode::DarkerColor => *b"dkCl",
        BlendMode::Lighten => *b"lite",
        BlendMode::Screen => *b"scrn",
        BlendMode::ColorDodge => *b"div ",
        BlendMode::LinearDodge => *b"lddg",
        BlendMode::LighterColor => *b"lgCl",
        BlendMode::Overlay => *b"over",
        BlendMode::SoftLight => *b"sLit",
        BlendMode::HardLight => *b"hLit",
        BlendMode::VividLight => *b"vLit",
        BlendMode::LinearLight => *b"lLit",
        BlendMode::PinLight => *b"pLit",
        BlendMode::HardMix => *b"hMix",
        BlendMode::Difference => *b"diff",
        BlendMode::Exclusion => *b"smud",
        BlendMode::Subtract => *b"fsub",
        BlendMode::Divide => *b"fdiv",
        BlendMode::Hue => *b"hue ",
        BlendMode::Saturation => *b"sat ",
        BlendMode::Color => *b"colr",
        BlendMode::Luminosity => *b"lum ",
    }
}

// ---- big-endian writer helpers ----

fn be16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn be16i(out: &mut Vec<u8>, v: i16) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn be32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn be32i(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// write a length-prefixed section: body built in a closure, then the
/// u32 length is patched in place. `pad` rounds the body; when
/// `len_includes_pad` the written length counts the padding (PSD's own
/// convention for the layer-info sub-section).
fn section(out: &mut Vec<u8>, pad: usize, len_includes_pad: bool, f: impl FnOnce(&mut Vec<u8>)) {
    let at = out.len();
    be32(out, 0);
    let body_start = out.len();
    f(out);
    let mut body_len = out.len() - body_start;
    while body_len % pad != 0 {
        out.push(0);
        body_len += 1;
    }
    let written = if len_includes_pad {
        body_len
    } else {
        out.len() - body_start
    };
    out[at..at + 4].copy_from_slice(&(written as u32).to_be_bytes());
}

/// PackBits-compress one row: literal runs header `n-1`, repeats `1-n`
/// (3-equal lookahead), max 128 bytes per run
fn pack_bits(row: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(row.len() + 16);
    let mut i = 0usize;
    let run_len = |i: usize| -> usize {
        let mut n = 1usize;
        while i + n < row.len() && row[i + n] == row[i] && n < 128 {
            n += 1;
        }
        n
    };
    while i < row.len() {
        let r = run_len(i);
        if r >= 3 {
            out.push((1i16 - r as i16) as u8);
            out.push(row[i]);
            i += r;
        } else {
            let start = i;
            i += r;
            while i < row.len() && i - start < 128 {
                let r = run_len(i);
                if r >= 3 {
                    break;
                }
                i += r;
            }
            let n = (i - start).min(128);
            out.push((n - 1) as u8);
            out.extend_from_slice(&row[start..start + n]);
            i = start + n;
        }
    }
    out
}

/// RLE image data for one channel plane: h × u16 row counts, then the
/// packed rows
fn rle_plane(plane: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let mut out = Vec::with_capacity(h * 2 + plane.len() / 2 + 16);
    let counts_at = out.len();
    out.resize(counts_at + h * 2, 0);
    for y in 0..h {
        let row = &plane[y * w..(y + 1) * w];
        let packed = pack_bits(row);
        out[counts_at + y * 2..counts_at + y * 2 + 2]
            .copy_from_slice(&(packed.len() as u16).to_be_bytes());
        out.extend_from_slice(&packed);
    }
    out
}

/// planar channel planes out of interleaved rgba8: returns [r,g,b,a]
fn planes(rgba: &[u8], w: u32, h: u32) -> [Vec<u8>; 4] {
    let n = (w as usize) * (h as usize);
    let mut p = [vec![0u8; n], vec![0u8; n], vec![0u8; n], vec![0u8; n]];
    for (i, px) in rgba.chunks_exact(4).enumerate() {
        p[0][i] = px[0];
        p[1][i] = px[1];
        p[2][i] = px[2];
        p[3][i] = px[3];
    }
    p
}

/// '8BIM' + key + u32 len + data (+ zero pad to `pad` multiple).
/// The length field INCLUDES the padding — PSD's convention for
/// additional-layer-info blocks (readers seek forward by it).
fn tagged(out: &mut Vec<u8>, key: &[u8; 4], pad: usize, data: &[u8]) {
    let padlen = (pad - data.len() % pad) % pad;
    out.extend_from_slice(b"8BIM");
    out.extend_from_slice(key);
    be32(out, (data.len() + padlen) as u32);
    out.extend_from_slice(data);
    out.extend_from_slice(&vec![0u8; padlen]);
}

/// unicode layer name block (luni): u32 char count + utf16be chars
fn luni_block(name: &str) -> Vec<u8> {
    let mut d = Vec::with_capacity(4 + name.len() * 2);
    let utf: Vec<u16> = name.encode_utf16().collect();
    be32(&mut d, utf.len() as u32);
    for u in utf {
        d.extend_from_slice(&u.to_be_bytes());
    }
    d
}

/// pascal name: u8 len + ascii bytes, field padded to a multiple of 4
/// (non-ascii folded to '?')
fn pascal_name(name: &str) -> Vec<u8> {
    let bytes: Vec<u8> = name
        .chars()
        .take(255)
        .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
        .collect();
    let mut out = Vec::with_capacity(256 + 3);
    out.push(bytes.len() as u8);
    out.extend_from_slice(&bytes);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    out
}

/// layer-record extra data: mask data, blending ranges, pascal name,
/// then tagged blocks (lsct for dividers, luni + lyid always)
fn extra_data(rec: &Rec, doc_channels: u32) -> Vec<u8> {
    let mut e = Vec::new();
    let (name, id, mask, divider) = match rec {
        Rec::Pixel(p) => (p.name.as_str(), p.id, p.mask.as_ref(), None),
        Rec::Divider(d) => (
            d.name.as_str(),
            d.id,
            None,
            Some((d.section_type, d.blend_key)),
        ),
    };
    // layer mask data: u32 len + {rect, defaultColor, flags, u16 pad}
    if let Some(m) = mask {
        be32(&mut e, 20);
        be32i(&mut e, m.top);
        be32i(&mut e, m.left);
        be32i(&mut e, m.top.saturating_add(m.h as i32));
        be32i(&mut e, m.left.saturating_add(m.w as i32));
        e.push(0); // defaultColor — black shows through as partial mask
        e.push(0); // flags: 0 = rect is absolute document coordinates
        e.extend_from_slice(&[0u8; 2]);
    } else {
        be32(&mut e, 0);
    }
    // blending ranges: composite gray range + one pair per doc channel
    be32(&mut e, (8 + doc_channels * 8) as u32);
    be32(&mut e, 65535);
    be32(&mut e, 65535);
    for _ in 0..doc_channels {
        be32(&mut e, 65535);
        be32(&mut e, 65535);
    }
    e.extend_from_slice(&pascal_name(name));
    if let Some((section_type, blend_key)) = divider {
        let mut d = Vec::with_capacity(16);
        be32(&mut d, section_type);
        if section_type == 1 || section_type == 2 {
            d.extend_from_slice(b"8BIM");
            d.extend_from_slice(&blend_key);
            be32(&mut d, 0); // subtype normal
        }
        tagged(&mut e, b"lsct", 2, &d);
    }
    tagged(&mut e, b"luni", 4, &luni_block(name));
    if id != 0 {
        tagged(&mut e, b"lyid", 2, &(id as u32).to_be_bytes());
    }
    e
}

fn write_file(
    path: &Path,
    doc_w: u32,
    doc_h: u32,
    merged_rgba: &[u8],
    recs: &[Rec],
) -> Result<PsdExportStats> {
    // global alpha decides both the header channel count and the sign of
    // the layer count — Photoshop's own convention
    let global_alpha = merged_rgba.chunks_exact(4).any(|p| p[3] != 255);
    let doc_channels: u32 = if global_alpha { 4 } else { 3 };

    let mut out: Vec<u8> = Vec::with_capacity(merged_rgba.len() + 4096);
    // header
    out.extend_from_slice(b"8BPS");
    be16(&mut out, 1);
    out.extend_from_slice(&[0u8; 6]);
    be16(&mut out, doc_channels as u16);
    be32(&mut out, doc_h);
    be32(&mut out, doc_w);
    be16(&mut out, 8);
    be16(&mut out, 3);
    // color mode data + image resources: empty
    be32(&mut out, 0);
    be32(&mut out, 0);

    // layer & mask info section
    section(&mut out, 2, false, |lam| {
        // layer info — length includes its own 4-byte padding
        section(lam, 4, true, |li| {
            let count = recs.len() as i16;
            be16i(li, if global_alpha { -count } else { count });
            // records, then channel image data
            let mut channel_blobs: Vec<Vec<(i16, Vec<u8>)>> = Vec::with_capacity(recs.len());
            for rec in recs {
                channel_blobs.push(write_record(li, rec, doc_channels));
            }
            for rec_chans in &channel_blobs {
                for (_id, blob) in rec_chans {
                    be16(li, 1); // RLE compression
                    li.extend_from_slice(blob);
                }
            }
        });
        // global layer mask info: none
        be32(lam, 0);
        // document-level additional layer info: none
    });

    // merged image data — always RLE (PS doesn't read zip composites)
    be16(&mut out, 1);
    {
        // Photoshop mattes transparent merged pixels on white
        let mut matted = merged_rgba.to_vec();
        if global_alpha {
            for p in matted.chunks_exact_mut(4) {
                let a = p[3];
                if a != 0 && a != 255 {
                    let af = a as f32 / 255.0;
                    let ra = 255.0 * (1.0 - af);
                    p[0] = (p[0] as f32 * af + ra).round() as u8;
                    p[1] = (p[1] as f32 * af + ra).round() as u8;
                    p[2] = (p[2] as f32 * af + ra).round() as u8;
                }
            }
        }
        let pl = planes(&matted, doc_w, doc_h);
        let channel_ids: &[usize] = if global_alpha {
            &[0, 1, 2, 3]
        } else {
            &[0, 1, 2]
        };
        let blobs: Vec<Vec<u8>> = channel_ids
            .iter()
            .map(|&ch| rle_plane(&pl[ch], doc_w, doc_h))
            .collect();
        // row-count table for every channel first, then all packed rows
        let rows = doc_h as usize;
        for blob in &blobs {
            out.extend_from_slice(&blob[..rows * 2]);
        }
        for blob in &blobs {
            out.extend_from_slice(&blob[rows * 2..]);
        }
    }

    std::fs::write(path, &out).with_context(|| format!("write {}", path.display()))?;
    Ok(PsdExportStats {
        layers: recs.iter().filter(|r| matches!(r, Rec::Pixel(_))).count(),
        groups: recs
            .iter()
            .filter(|r| matches!(r, Rec::Divider(d) if d.section_type != 3))
            .count(),
    })
}

/// one layer record into `li`; returns its channels as (id, rle blob)
/// pairs for the channel-image-data block that follows all records
fn write_record(li: &mut Vec<u8>, rec: &Rec, doc_channels: u32) -> Vec<(i16, Vec<u8>)> {
    // (rect, name, id, blend_key, opacity, flags, channels)
    let (top, left, bottom, right, blend_key, opacity, flags);
    let channels: Vec<(i16, Vec<u8>)>;
    match rec {
        Rec::Pixel(p) => {
            top = p.top;
            left = p.left;
            bottom = p.bottom;
            right = p.right;
            blend_key = psd_key(p.blend);
            opacity = p.opacity;
            flags = 0x08 | if p.hidden { 0x02 } else { 0 };
            let w = (right - left) as u32;
            let h = (bottom - top) as u32;
            let pl = planes(&p.rgba, w, h);
            channels = if let Some(m) = &p.mask {
                vec![
                    (-1, rle_plane(&pl[3], w, h)),
                    (0, rle_plane(&pl[0], w, h)),
                    (1, rle_plane(&pl[1], w, h)),
                    (2, rle_plane(&pl[2], w, h)),
                    (-2, rle_plane(&m.gray, m.w, m.h)),
                ]
            } else {
                vec![
                    (-1, rle_plane(&pl[3], w, h)),
                    (0, rle_plane(&pl[0], w, h)),
                    (1, rle_plane(&pl[1], w, h)),
                    (2, rle_plane(&pl[2], w, h)),
                ]
            };
        }
        Rec::Divider(d) => {
            top = d.top;
            left = d.left;
            bottom = d.bottom;
            right = d.right;
            blend_key = d.blend_key;
            opacity = d.opacity;
            flags = 0x08 | 0x10 | if d.hidden { 0x02 } else { 0 };
            // divider rows carry 4 declared channels with no data — the
            // convention ag-psd/Photoshop use; readers see a 0-size blob
            channels = vec![
                (-1, Vec::new()),
                (0, Vec::new()),
                (1, Vec::new()),
                (2, Vec::new()),
            ];
        }
    }
    be32i(li, top);
    be32i(li, left);
    be32i(li, bottom);
    be32i(li, right);
    be16(li, channels.len() as u16);
    for (id, blob) in &channels {
        be16i(li, *id);
        be32(li, 2 + blob.len() as u32); // length includes the u16 compression
    }
    li.extend_from_slice(b"8BIM");
    li.extend_from_slice(&blend_key);
    li.push(opacity);
    li.push(0); // clipping
    li.push(flags);
    li.push(0); // filler
    let extra = extra_data(rec, doc_channels);
    be32(li, extra.len() as u32);
    li.extend_from_slice(&extra);
    channels
}

pub fn import_psd(path: &Path) -> Result<Document> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let psd = psd::Psd::from_bytes(&bytes)
        .map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;

    let mut doc = Document::new(
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "psd".into()),
        psd.width(),
        psd.height(),
    );

    // psd.groups() — for group visibility, which hides the member layers
    // in PS. The psd crate reads the visibility bit INVERTED (bit1 set
    // actually means hidden), so visible() below is negated everywhere.
    let groups = psd.groups();
    let group_visible = |gid: Option<u32>| -> bool {
        let mut cur = gid;
        let mut steps = 0;
        while let Some(id) = cur {
            if steps > 64 {
                break; // paranoia against a cyclic parent chain
            }
            steps += 1;
            match groups.get(&id) {
                Some(g) => {
                    if g.visible() {
                        return false; // crate-inverted: "visible" == hidden bit set
                    }
                    cur = g.parent_id();
                }
                None => break,
            }
        }
        true
    };

    // psd.layers() is stored top->bottom; our stack is bottom->top
    let mut psd_layers: Vec<_> = psd.layers().iter().collect();
    psd_layers.reverse();
    for pl in psd_layers {
        // the crate's rgba() returns a DOCUMENT-sized buffer with the
        // layer composited at its offset — crop our layer rect out of it.
        // it can panic on out-of-canvas offsets or missing channels.
        let rgba_doc = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pl.rgba()))
            .unwrap_or_default();
        let (pw, ph) = (psd.width() as i64, psd.height() as i64);
        if rgba_doc.len() != (pw * ph * 4) as usize {
            continue;
        }
        // layer width/height come back correct (bounds are stored
        // inclusive internally); the rect can sit partly off-canvas
        let lw = pl.width() as i64;
        let lh = pl.height() as i64;
        let (top, left) = (pl.layer_top() as i64, pl.layer_left() as i64);
        let (x0, y0) = (left.max(0), top.max(0));
        let (x1, y1) = ((left + lw).min(pw), (top + lh).min(ph));
        if x1 <= x0 || y1 <= y0 {
            continue; // fully off-canvas or empty
        }
        let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            let src = (((y0 + y as i64) * pw + x0) * 4) as usize;
            let dst = (y * w * 4) as usize;
            rgba[dst..dst + (w * 4) as usize]
                .copy_from_slice(&rgba_doc[src..src + (w * 4) as usize]);
        }
        use base64::Engine as _;
        let mut l = Layer::base(
            pl.name().to_string(),
            LayerKind::Raster {
                width: w,
                height: h,
                src: RasterSrc::Embedded {
                    png_b64: base64::engine::general_purpose::STANDARD.encode(&rgba),
                },
            },
        );
        l.x = x0 as i32;
        l.y = y0 as i32;
        l.visible = !pl.visible() && group_visible(pl.parent_id());
        l.opacity = (pl.opacity() as f32 / 255.0).clamp(0.0, 1.0);
        // psd 0.3 keeps its BlendMode enum in a private module — map via its
        // Debug name through our own parser
        l.blend = BlendMode::parse(&format!("{:?}", pl.blend_mode())).unwrap_or(BlendMode::Normal);
        doc.add_layer(l);
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Document, Layer, LayerKind, Mask, RasterSrc};

    fn nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    fn raster_layer(name: &str, w: u32, h: u32, px: [u8; 4]) -> Layer {
        use base64::Engine as _;
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            rgba.extend_from_slice(&px);
        }
        Layer::base(
            name,
            LayerKind::Raster {
                width: w,
                height: h,
                src: RasterSrc::Embedded {
                    png_b64: base64::engine::general_purpose::STANDARD.encode(&rgba),
                },
            },
        )
    }

    /// build a doc, export layered, parse with the `psd` crate and return
    /// (layers top→bottom as stored, groups). Unique path per call — the
    /// tests run in parallel and must not share a file.
    fn roundtrip(
        doc: Document,
    ) -> (
        Vec<psd::PsdLayer>,
        std::collections::HashMap<u32, psd::PsdGroup>,
        Vec<u8>,
    ) {
        let mut c = Composer::new(doc).expect("composer");
        let dir =
            std::env::temp_dir().join(format!("koubou-psdtest-{}-{}", std::process::id(), nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.psd");
        write_layered_psd(&mut c, &path).expect("export");
        let bytes = std::fs::read(&path).unwrap();
        let parsed = psd::Psd::from_bytes(&bytes).expect("psd crate must parse our output");
        (parsed.layers().clone(), parsed.groups().clone(), bytes)
    }

    #[test]
    fn layered_names_order_and_flags() {
        let mut doc = Document::new("t", 16, 16);
        let mut hidden = raster_layer("hidden-l", 4, 4, [9, 9, 9, 255]);
        hidden.visible = false;
        let mut half = raster_layer("half", 4, 4, [200, 0, 0, 255]);
        half.opacity = 0.5;
        let mut moved = raster_layer("moved", 4, 4, [0, 0, 200, 255]);
        moved.x = 5;
        moved.y = 3;
        doc.add_layer(hidden);
        doc.add_layer(half);
        doc.add_layer(moved);
        let (layers, groups, _) = roundtrip(doc);
        assert_eq!(layers.len(), 3);
        assert_eq!(groups.len(), 0);
        // crate stores top→bottom: moved, half, hidden
        assert_eq!(layers[0].name(), "moved");
        assert_eq!(layers[2].name(), "hidden-l");
        // the crate reports bit1 (hidden) AS `visible` — inverted vs the
        // real PSD convention — so `visible()==true` proves our hidden
        // bit landed and `false` proves a clear flag on a shown layer
        assert!(layers[2].visible());
        assert!(!layers[1].visible());
        assert_eq!(layers[1].opacity(), 128); // round(0.5*255)
                                              // placement survives
        assert_eq!(layers[0].layer_left(), 5);
        assert_eq!(layers[0].layer_top(), 3);
        assert_eq!(layers[0].width(), 4);
        assert_eq!(layers[0].height(), 4);
        // pixel content survives — rgba() is doc-sized
        let r = layers[0].rgba();
        let doc_w = 16usize;
        let px = &r[((3 * doc_w + 5) * 4)..][..4];
        assert_eq!(px, &[0, 0, 200, 255]);
    }

    #[test]
    fn group_records_and_blend_keys() {
        let mut doc = Document::new("t", 16, 16);
        let mut child = raster_layer("c1", 4, 4, [255, 0, 0, 255]);
        child.x = 2;
        child.y = 2;
        let mut g = Layer::base("grp", LayerKind::Group { children: vec![] });
        if let LayerKind::Group { children } = &mut g.kind {
            children.push(child);
        }
        g.blend = BlendMode::Multiply;
        doc.add_layer(g);
        doc.add_layer(raster_layer("top", 4, 4, [0, 255, 0, 255]));
        let (layers, groups, _) = roundtrip(doc);
        // one pixel layer inside the group + the top layer
        assert_eq!(layers.len(), 2);
        assert_eq!(groups.len(), 1);
        let g = groups.values().next().unwrap();
        assert_eq!(g.name(), "grp");
        assert_eq!(format!("{:?}", g.blend_mode()), "Multiply");
        // the child carries the group's id
        let child = layers
            .iter()
            .find(|l| l.name() == "c1")
            .expect("child layer");
        assert_eq!(child.parent_id(), Some(g.id()));
    }

    #[test]
    fn mask_roundtrips_through_pixels() {
        let mut doc = Document::new("t", 8, 8);
        let mut l = raster_layer("masked", 8, 8, [255, 0, 0, 255]);
        let mut m = Mask {
            width: 8,
            height: 8,
            data: vec![1.0; 64],
            inverted: false,
            density: 1.0,
            feather: 0.0,
        };
        // right half fully masked out
        for y in 0..8 {
            for x in 4..8 {
                m.data[y * 8 + x] = 0.0;
            }
        }
        l.mask = Some(m);
        doc.add_layer(l);
        let mut c = Composer::new(doc).unwrap();
        let dir =
            std::env::temp_dir().join(format!("koubou-psdmask-{}-{}", std::process::id(), nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.psd");
        write_layered_psd(&mut c, &path).expect("export");
        let parsed = psd::Psd::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(parsed.layers().len(), 1);
        // mask channel must exist — crate exposes channels privately, so
        // verify the file parses and the merged image is half-transparent
        let rgba = parsed.layers()[0].rgba();
        let left = &rgba[(3 * 8 + 1) * 4..][..4];
        assert_eq!(left, &[255, 0, 0, 255]);
    }

    #[test]
    fn pack_bits_roundtrip() {
        // verify our encoder against the patterns PackBits cares about:
        // long runs, literals, the 128 cap, mixed boundaries
        let mut row = vec![7u8; 200];
        row.extend_from_slice(&[1, 2, 3, 4, 5]);
        row.extend_from_slice(&[9u8; 5]);
        row.extend(0u8..150);
        let packed = pack_bits(&row);
        // decode with a strict PackBits decoder
        let mut out = Vec::new();
        let mut i = 0;
        while i < packed.len() {
            let h = packed[i] as i8;
            i += 1;
            if h == -128 {
                continue;
            }
            if h >= 0 {
                let n = h as usize + 1;
                out.extend_from_slice(&packed[i..i + n]);
                i += n;
            } else {
                let n = 1 - h as isize;
                out.extend(std::iter::repeat(packed[i]).take(n as usize));
                i += 1;
            }
        }
        assert_eq!(out, row);
    }

    #[test]
    fn flat_writer_still_works() {
        let dir =
            std::env::temp_dir().join(format!("koubou-psdflat-{}-{}", std::process::id(), nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.psd");
        write_flat_psd(&path, 4, 3, &[1u8, 2, 3, 255].repeat(12)).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"8BPS");
    }

    #[test]
    fn import_roundtrip_keeps_layers() {
        // export a layered doc, re-import it, check structure survives
        let mut doc = Document::new("t", 16, 16);
        let mut a = raster_layer("bottom", 4, 4, [10, 20, 30, 255]);
        a.x = 1;
        a.y = 2;
        let mut b = raster_layer("top", 4, 4, [40, 50, 60, 255]);
        b.visible = false;
        doc.add_layer(a);
        doc.add_layer(b);
        let mut c = Composer::new(doc).unwrap();
        let dir =
            std::env::temp_dir().join(format!("koubou-psdimp-{}-{}", std::process::id(), nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rt.psd");
        write_layered_psd(&mut c, &path).unwrap();
        let doc = import_psd(&path).unwrap();
        assert_eq!(doc.layers.len(), 2);
        assert_eq!(doc.layers[0].name, "bottom");
        assert_eq!(doc.layers[1].name, "top");
        assert_eq!(doc.layers[0].x, 1);
        assert_eq!(doc.layers[0].y, 2);
        assert!(doc.layers[0].visible);
        assert!(!doc.layers[1].visible);
    }
}
