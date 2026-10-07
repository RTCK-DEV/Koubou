//! PSD import: layered Photoshop documents become koubou documents.
//! Raster layers keep pixels; blend modes/opacity/offsets/names map 1:1.
//! Layer groups flatten into raster children (v1 — see docs/parity.md).

use std::path::Path;

use anyhow::{Context, Result};

use crate::blend::BlendMode;
use crate::doc::{Document, Layer, LayerKind, RasterSrc};

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
        l.blend = BlendMode::parse(&format!("{:?}", pl.blend_mode()))
            .unwrap_or(BlendMode::Normal);
        doc.add_layer(l);
    }
    Ok(doc)
}
