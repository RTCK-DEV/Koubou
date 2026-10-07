//! PSD import: layered Photoshop documents become koubou documents.
//! Raster layers keep pixels; blend modes/opacity/offsets/names map 1:1.
//! Layer groups flatten into raster children (v1 — see docs/parity.md).
//!
//! PSD export: minimal flat-file writer (merged RGBA composite, raw
//! compression). Opens in Photoshop/GIMP/Photopea as a flattened image —
//! enough for interchange; layered export is on the roadmap.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

use crate::blend::BlendMode;
use crate::doc::{Document, Layer, LayerKind, RasterSrc};

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

    // psd.layers() is stored top->bottom; our stack is bottom->top
    let mut psd_layers: Vec<_> = psd.layers().iter().collect();
    psd_layers.reverse();
    for pl in psd_layers {
        let rgba = pl.rgba();
        let w = (pl.layer_right() - pl.layer_left()) as u32;
        let h = (pl.layer_bottom() - pl.layer_top()) as u32;
        if w == 0 || h == 0 || rgba.len() != (w * h * 4) as usize {
            continue; // skip empty/group container rows
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
        l.x = pl.layer_left();
        l.y = pl.layer_top();
        l.visible = pl.visible();
        l.opacity = (pl.opacity() as f32 / 255.0).clamp(0.0, 1.0);
        // psd 0.3 keeps its BlendMode enum in a private module — map via its
        // Debug name through our own parser
        l.blend = BlendMode::parse(&format!("{:?}", pl.blend_mode())).unwrap_or(BlendMode::Normal);
        doc.add_layer(l);
    }
    Ok(doc)
}
