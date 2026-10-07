//! Compositor: rasterizes layers and blends them bottom→top onto a doc-sized
//! sRGB f32 canvas. Layer pixel buffers are cached by (id, gen) so scrubbing a
//! slider on one layer does not re-render the others.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use koubou_core::decode::Decoded;
use koubou_core::develop::{self, RgbaImage};
use koubou_core::{Engine, Recipe, WbMode};

use crate::blend::{blend_pixel, BlendMode};
use crate::doc::{Document, Fill, Layer, LayerKind, Mask};

/// a rasterized layer in its own pixel space (straight alpha, sRGB)
#[derive(Debug, Clone)]
pub struct LayerPixels {
    pub w: u32,
    pub h: u32,
    pub data: Vec<[f32; 4]>,
}

impl LayerPixels {
    fn empty(w: u32, h: u32) -> LayerPixels {
        LayerPixels {
            w,
            h,
            data: vec![[0.0; 4]; (w * h) as usize],
        }
    }

    fn from_rgba8(w: u32, h: u32, rgba: &[u8]) -> LayerPixels {
        let data = rgba
            .chunks_exact(4)
            .map(|c| {
                [
                    c[0] as f32 / 255.0,
                    c[1] as f32 / 255.0,
                    c[2] as f32 / 255.0,
                    c[3] as f32 / 255.0,
                ]
            })
            .collect();
        LayerPixels { w, h, data }
    }

    fn to_rgba16(&self) -> Vec<u16> {
        let mut out = Vec::with_capacity(self.data.len() * 4);
        out.extend(self.data.iter().flat_map(|p| {
            [
                (p[0].clamp(0.0, 1.0) * 65535.0).round() as u16,
                (p[1].clamp(0.0, 1.0) * 65535.0).round() as u16,
                (p[2].clamp(0.0, 1.0) * 65535.0).round() as u16,
                (p[3].clamp(0.0, 1.0) * 65535.0).round() as u16,
            ]
        }));
        out
    }

    fn to_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() * 4);
        out.extend(self.data.iter().flat_map(|p| {
            [
                (p[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                (p[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                (p[2].clamp(0.0, 1.0) * 255.0).round() as u8,
                (p[3].clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        }));
        out
    }

    /// bounding box of pixels with alpha > 0, None when fully transparent
    fn opaque_bbox(&self) -> Option<(u32, u32, u32, u32)> {
        let (mut x0, mut y0, mut x1, mut y1) = (self.w, self.h, 0u32, 0u32);
        for y in 0..self.h {
            for x in 0..self.w {
                if self.data[(y * self.w + x) as usize][3] > 0.0 {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
        (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
    }

    /// crop to a pixel-space rect
    fn crop(&self, x0: u32, y0: u32, w: u32, h: u32) -> LayerPixels {
        let mut data = Vec::with_capacity((w * h) as usize);
        for y in y0..y0 + h {
            let row = (y * self.w + x0) as usize;
            data.extend_from_slice(&self.data[row..row + w as usize]);
        }
        LayerPixels { w, h, data }
    }
}

pub struct Composer {
    pub doc: Document,
    engine: Engine,
    /// layer id -> (gen, pixels). Invalidated when the layer's gen differs.
    cache: HashMap<u64, (u64, LayerPixels)>,
}

impl Composer {
    pub fn new(doc: Document) -> Result<Composer> {
        Ok(Composer {
            doc,
            engine: Engine::new()?,
            cache: HashMap::new(),
        })
    }

    /// doc-sized composite, rgba8
    pub fn render(&mut self) -> Result<RgbaImage> {
        let w = self.doc.width;
        let h = self.doc.height;
        let mut canvas = LayerPixels::empty(w, h);
        // move the layer vec out rather than cloning it — embedded rasters
        // and masks are heavy and this list gets walked every render
        let layers = std::mem::take(&mut self.doc.layers);
        let r = self.composite_list(&mut canvas, &layers);
        self.doc.layers = layers;
        r?;
        // backdrop under everything
        let bd = self.doc.backdrop;
        for p in canvas.data.iter_mut() {
            if p[3] < 1.0 {
                let a = p[3];
                for c in 0..3 {
                    p[c] = p[c] * a + bd[c] * bd[3] * (1.0 - a);
                }
                p[3] = a + bd[3] * (1.0 - a);
            }
        }
        Ok(RgbaImage {
            width: w,
            height: h,
            data: canvas.to_rgba8(),
        })
    }

    /// rasterize one layer's own pixels (no blending) — for layer export
    pub fn render_layer(&mut self, id: u64) -> Result<RgbaImage> {
        let layer = self.doc.layer(id).context("layer not found")?.clone();
        let pix = self.rasterize_layer(&layer)?;
        Ok(RgbaImage {
            width: pix.w,
            height: pix.h,
            data: pix.to_rgba8(),
        })
    }

    /// merge `id` with the layer below it into one raster layer; the new
    /// layer keeps the lower layer's name and takes both layers' place.
    /// Returns the merged layer's id.
    pub fn merge_down(&mut self, id: u64) -> Result<u64> {
        let i = self
            .doc
            .index_of(id)
            .with_context(|| format!("layer {id} not found"))?;
        if i == 0 {
            anyhow::bail!("layer {id} is the bottom layer — nothing to merge into");
        }
        let below_name = self.doc.layers[i - 1].name.clone();
        let pair = self.doc.layers[i - 1..=i].to_vec();
        let mut buf = LayerPixels::empty(self.doc.width, self.doc.height);
        self.composite_list(&mut buf, &pair)?;
        let (pix, x, y) = match buf.opaque_bbox() {
            Some((x0, y0, x1, y1)) => (buf.crop(x0, y0, x1 - x0, y1 - y0), x0 as i32, y0 as i32),
            None => (buf.crop(0, 0, 1, 1), 0, 0),
        };
        let mut merged = Layer::raster(below_name, pix.w, pix.h, pix.to_rgba8());
        merged.x = x;
        merged.y = y;
        merged.visible = true;
        merged.opacity = 1.0;
        merged.blend = BlendMode::Normal;
        self.doc.layers.remove(i);
        self.doc.layers.remove(i - 1);
        let new_id = self.doc.add_layer_at(merged, i - 1);
        Ok(new_id)
    }

    /// bake the whole composite into a single raster layer named `name`
    pub fn flatten(&mut self, name: &str) -> Result<u64> {
        let img = self.render()?;
        let mut l = Layer::raster(name, img.width, img.height, img.data);
        l.id = self.doc.next_id;
        self.doc.next_id += 1;
        let id = l.id;
        self.doc.layers = vec![l];
        self.cache.clear();
        Ok(id)
    }

    /// render then fit inside max_px (for previews)
    pub fn render_preview(&mut self, max_px: u32) -> Result<RgbaImage> {
        let img = self.render()?;
        if max_px == 0 || img.width.max(img.height) <= max_px {
            return Ok(img);
        }
        let s = max_px as f32 / img.width.max(img.height) as f32;
        let nw = ((img.width as f32 * s) as u32).max(1);
        let nh = ((img.height as f32 * s) as u32).max(1);
        let src = image::RgbaImage::from_raw(img.width, img.height, img.data)
            .context("composite buffer")?;
        let out = image::imageops::resize(&src, nw, nh, image::imageops::FilterType::Triangle);
        Ok(RgbaImage {
            width: nw,
            height: nh,
            data: out.into_raw(),
        })
    }

    fn composite_list(&mut self, canvas: &mut LayerPixels, layers: &[Layer]) -> Result<()> {
        for layer in layers {
            if !layer.visible {
                continue;
            }
            match &layer.kind {
                LayerKind::Adjustment { recipe } => {
                    // snapshot below, run the develop pipeline on it, blend back
                    let snapshot = canvas.clone();
                    let adjusted = self.develop_adjustment(&snapshot, recipe)?;
                    self.blend_layer(canvas, &adjusted, layer);
                }
                LayerKind::Group { children } => {
                    let mut gbuf = LayerPixels::empty(canvas.w, canvas.h);
                    self.composite_list(&mut gbuf, children)?;
                    self.blend_layer(canvas, &gbuf, layer);
                }
                _ => {
                    let pix = self.rasterize_layer(layer)?;
                    self.blend_layer(canvas, &pix, layer);
                }
            }
        }
        Ok(())
    }

    /// the develop pipeline applied to an in-memory composite (raster input)
    fn develop_adjustment(&self, snapshot: &LayerPixels, recipe: &Recipe) -> Result<LayerPixels> {
        let mut r = sanitize_adjustment(recipe);
        // keep user temp/tint but neutralize WB mode semantics on a raster
        if matches!(r.wb_mode, WbMode::Pick) {
            r.wb_mode = WbMode::AsShot;
        }
        let d = Decoded::Raster {
            rgba: snapshot.to_rgba16(),
            w: snapshot.w as usize,
            h: snapshot.h as usize,
            // no camera info for an in-memory raster (literal so we don't
            // depend on CameraInfo: Default in the engine crate)
            info: koubou_core::decode::CameraInfo {
                make: String::new(),
                model: String::new(),
                lens: String::new(),
                iso: 0.0,
                shutter: 0.0,
                aperture: 0.0,
                focal: 0.0,
                timestamp: 0,
                flip: 0,
            },
            flip: 0,
        };
        let out = develop::develop_cpu(&d, &r, 0);
        Ok(LayerPixels::from_rgba8(out.width, out.height, &out.data))
    }

    /// rasterize a layer's content into layer pixel space (cached)
    fn rasterize_layer(&mut self, layer: &Layer) -> Result<LayerPixels> {
        if let Some((gen, pix)) = self.cache.get(&layer.id) {
            if *gen == layer.gen {
                return Ok(pix.clone());
            }
        }
        let pix = match &layer.kind {
            LayerKind::Develop { path, recipe } => {
                // render at the resolution the doc actually samples:
                // doc size / scale (so a 1:1 photo layer renders full res)
                let doc_max = self.doc.width.max(self.doc.height) as f32;
                let need = (doc_max / layer.scale.max(0.01)).ceil() as u32;
                self.engine
                    .render(Path::new(path), recipe, need)
                    .map(|i| LayerPixels::from_rgba8(i.width, i.height, &i.data))?
            }
            LayerKind::Raster { width, height, src } => {
                let (dw, dh, raw) = src.decode()?;
                let (w, h) = if dw > 0 { (dw, dh) } else { (*width, *height) };
                if raw.len() != (w * h * 4) as usize {
                    anyhow::bail!(
                        "raster layer {}: expected {} bytes, got {}",
                        layer.name,
                        w * h * 4,
                        raw.len()
                    );
                }
                LayerPixels::from_rgba8(w, h, &raw)
            }
            LayerKind::Fill { fill } => rasterize_fill(fill, self.doc.width, self.doc.height),
            LayerKind::Shape { shapes } => {
                let px = crate::shape::rasterize(shapes, self.doc.width, self.doc.height)
                    .unwrap_or_else(|| vec![0; (self.doc.width * self.doc.height * 4) as usize]);
                LayerPixels::from_rgba8(self.doc.width, self.doc.height, &px)
            }
            LayerKind::Text { text } => {
                let (rgba, w, h) = crate::text::rasterize(text)?;
                LayerPixels::from_rgba8(w, h, &rgba)
            }
            LayerKind::Group { .. } | LayerKind::Adjustment { .. } => {
                unreachable!("handled in composite_list")
            }
        };
        self.cache.insert(layer.id, (layer.gen, pix.clone()));
        Ok(pix)
    }

    /// blend a layer's pixels onto the canvas honouring offset/scale/mask/opacity
    fn blend_layer(&self, canvas: &mut LayerPixels, pix: &LayerPixels, layer: &Layer) {
        if layer.opacity <= 0.0 {
            return;
        }
        let scale = layer.scale.max(1e-4);
        let dw = (pix.w as f32 * scale).ceil() as i64;
        let dh = (pix.h as f32 * scale).ceil() as i64;

        let x0 = layer.x as i64;
        let y0 = layer.y as i64;
        let cw = canvas.w as i64;
        let ch = canvas.h as i64;
        let feathered = layer
            .mask
            .as_ref()
            .filter(|m| m.feather > 0.0)
            .map(|m| blur_mask(m));
        let mask = feathered.as_ref().or(layer.mask.as_ref());
        // 1:1 placement is the dominant case (photo layers) — integer-index
        // sampling skips the four-tap bilinear entirely. Same result: at
        // integral coords bilinear returns the pixel itself.
        if scale == 1.0 {
            let opacity = layer.opacity.clamp(0.0, 1.0);
            for dy in y0.max(0)..(y0 + dh).min(ch) {
                let sy = (dy - y0) as u32;
                if sy >= pix.h {
                    continue;
                }
                for dx in x0.max(0)..(x0 + dw).min(cw) {
                    let sx = (dx - x0) as u32;
                    if sx >= pix.w {
                        continue;
                    }
                    let p = pix.data[(sy * pix.w + sx) as usize];
                    if p[3] <= 0.0 {
                        continue;
                    }
                    let mut a = p[3] * opacity;
                    if let Some(m) = mask {
                        a *= mask_at(m, sx as f32, sy as f32);
                    }
                    if a <= 0.0 {
                        continue;
                    }
                    let i = (dy as u32 * canvas.w + dx as u32) as usize;
                    blend_pixel(&mut canvas.data[i], [p[0], p[1], p[2], a], layer.blend);
                }
            }
            return;
        }
        for dy in y0.max(0)..(y0 + dh).min(ch) {
            for dx in x0.max(0)..(x0 + dw).min(cw) {
                // bilinear sample in layer space (pixel centers on integers)
                let sx = (dx - x0) as f32 / scale;
                let sy = (dy - y0) as f32 / scale;
                let p = sample_bilinear(pix, sx, sy);
                if p[3] <= 0.0 {
                    continue;
                }
                let mut a = p[3] * layer.opacity.clamp(0.0, 1.0);
                if let Some(m) = mask {
                    a *= mask_at(m, sx, sy);
                }
                if a <= 0.0 {
                    continue;
                }
                let i = (dy as u32 * canvas.w + dx as u32) as usize;
                blend_pixel(&mut canvas.data[i], [p[0], p[1], p[2], a], layer.blend);
            }
        }
    }
}

fn sample_bilinear(pix: &LayerPixels, x: f32, y: f32) -> [f32; 4] {
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let get = |ix: i64, iy: i64| -> [f32; 4] {
        if ix < 0 || iy < 0 || ix >= pix.w as i64 || iy >= pix.h as i64 {
            return [0.0; 4];
        }
        pix.data[(iy as u32 * pix.w + ix as u32) as usize]
    };
    let (a, b, c, d) = (
        get(x0, y0),
        get(x0 + 1, y0),
        get(x0, y0 + 1),
        get(x0 + 1, y0 + 1),
    );
    let mut out = [0.0f32; 4];
    for ch in 0..4 {
        out[ch] = a[ch] * (1.0 - fx) * (1.0 - fy)
            + b[ch] * fx * (1.0 - fy)
            + c[ch] * (1.0 - fx) * fy
            + d[ch] * fx * fy;
    }
    out
}

fn mask_at(m: &Mask, x: f32, y: f32) -> f32 {
    let ix = x.floor().max(0.0) as u32;
    let iy = y.floor().max(0.0) as u32;
    m.at(
        ix.min(m.width.saturating_sub(1)),
        iy.min(m.height.saturating_sub(1)),
    )
}

/// 3-pass box blur ≈ gaussian for mask feathering
fn blur_mask(m: &Mask) -> Mask {
    let r = m.feather.max(0.0).round() as usize;
    if r == 0 {
        return m.clone();
    }
    let (w, h) = (m.width as usize, m.height as usize);
    let mut cur = m.data.clone();
    let mut tmp = vec![0.0f32; w * h];
    for _ in 0..3 {
        for y in 0..h {
            let mut s = 0.0f32;
            let mut n = 0usize;
            // sliding window seeded at x=0
            for k in 0..(r + 1).min(w) {
                s += cur[y * w + k];
                n += 1;
            }
            tmp[y * w] = s / n as f32;
            for x in 1..w {
                if x + r < w {
                    s += cur[y * w + x + r];
                    n += 1;
                }
                if x > r {
                    s -= cur[y * w + x - r - 1];
                    n -= 1;
                }
                tmp[y * w + x] = s / n as f32;
            }
        }
        for y in 0..h {
            for x in 0..w {
                let mut s = 0.0;
                let mut n = 0;
                for k in y.saturating_sub(r)..(y + r + 1).min(h) {
                    s += tmp[k * w + x];
                    n += 1;
                }
                cur[y * w + x] = s / n as f32;
            }
        }
    }
    let mut out = m.clone();
    out.data = cur;
    out.feather = 0.0;
    out
}

fn rasterize_fill(fill: &Fill, w: u32, h: u32) -> LayerPixels {
    let mut p = LayerPixels::empty(w, h);
    match fill {
        Fill::Solid { color } => p.data.fill(*color),
        Fill::LinearGradient { line, stops } => {
            if stops.is_empty() {
                return p;
            }
            let (x0, y0, x1, y1) = (line[0], line[1], line[2], line[3]);
            let dx = x1 - x0;
            let dy = y1 - y0;
            let len2 = (dx * dx + dy * dy).max(1e-6);
            for y in 0..h {
                for x in 0..w {
                    let t = (((x as f32 / w as f32) - x0) * dx + ((y as f32 / h as f32) - y0) * dy)
                        / len2;
                    p.data[(y * w + x) as usize] = grad_at(stops, t);
                }
            }
        }
    }
    p
}

fn grad_at(stops: &[[f32; 5]], t: f32) -> [f32; 4] {
    let t = t.clamp(0.0, 1.0);
    let mut i = 0;
    while i + 1 < stops.len() && stops[i + 1][0] < t {
        i += 1;
    }
    let a = stops[i];
    if i + 1 >= stops.len() {
        return [a[1], a[2], a[3], a[4]];
    }
    let b = stops[i + 1];
    let f = if b[0] > a[0] {
        (t - a[0]) / (b[0] - a[0])
    } else {
        0.0
    };
    [
        a[1] + (b[1] - a[1]) * f,
        a[2] + (b[2] - a[2]) * f,
        a[3] + (b[3] - a[3]) * f,
        a[4] + (b[4] - a[4]) * f,
    ]
}

/// zero out geometry-affecting recipe fields — an adjustment layer must never
/// reshape the composite.
fn sanitize_adjustment(r: &Recipe) -> Recipe {
    let mut r = r.clone();
    r.crop = [0.0; 4];
    r.rotation_deg = 0.0;
    r.key_v = 0.0;
    r.key_h = 0.0;
    r.spots.clear();
    r.clones.clear();
    r.wb_pick = [0.5, 0.5];
    if matches!(r.wb_mode, WbMode::Pick) {
        r.wb_mode = WbMode::AsShot;
    }
    r
}
