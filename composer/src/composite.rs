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
    /// last rendered doc-space canvas + the render key it was produced for —
    /// identical renders are returned without re-blending the stack.
    out_cache: Option<(u64, LayerPixels)>,
}

impl Composer {
    pub fn new(doc: Document) -> Result<Composer> {
        Ok(Composer {
            doc,
            engine: Engine::new()?,
            cache: HashMap::new(),
            out_cache: None,
        })
    }

    /// fingerprint of everything that affects the composite — layer stack
    /// state, content gens, masks and styles. When it matches the last
    /// render, the cached canvas is returned as-is.
    fn render_key(&self) -> u64 {
        fn fnv(h: u64, b: &[u8]) -> u64 {
            b.iter()
                .fold(h, |h, v| h.wrapping_mul(0x100000001b3) ^ (*v as u64))
        }
        fn key_layers(h: u64, layers: &[Layer]) -> u64 {
            layers.iter().fold(h, |mut h, l| {
                h = fnv(h, &l.id.to_le_bytes());
                h = fnv(h, &l.gen.to_le_bytes());
                h = fnv(h, &[l.visible as u8]);
                h = fnv(h, &l.opacity.to_bits().to_le_bytes());
                h = fnv(h, &[l.blend as u8]);
                h = fnv(h, &l.x.to_le_bytes());
                h = fnv(h, &l.y.to_le_bytes());
                h = fnv(h, &l.scale.to_bits().to_le_bytes());
                if let Some(m) = &l.mask {
                    // masks are doc-sized; hash a checksum over the data so
                    // dab-level edits register without an O(n) walk per pixel
                    let mut sum = 0u64;
                    for (i, v) in m.data.iter().enumerate() {
                        sum = sum.wrapping_add((v.to_bits() as u64).wrapping_mul((i as u64) | 1));
                    }
                    h = fnv(h, &m.width.to_le_bytes());
                    h = fnv(h, &m.height.to_le_bytes());
                    h = fnv(h, &sum.to_le_bytes());
                    h = fnv(h, &[m.inverted as u8]);
                    h = fnv(h, &m.density.to_bits().to_le_bytes());
                    h = fnv(h, &m.feather.to_bits().to_le_bytes());
                }
                if let Ok(sj) = serde_json::to_vec(&l.styles) {
                    h = fnv(h, &sj);
                }
                if let LayerKind::Group { children } = &l.kind {
                    key_layers(h, children)
                } else {
                    h
                }
            })
        }
        let mut h = 0xcbf29ce484222325u64;
        h = fnv(h, &self.doc.width.to_le_bytes());
        h = fnv(h, &self.doc.height.to_le_bytes());
        h = key_layers(h, &self.doc.layers);
        h
    }

    /// doc-sized composite with backdrop applied, rgba8
    pub fn render(&mut self) -> Result<RgbaImage> {
        let mut canvas = self.render_raw()?;
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
            width: self.doc.width,
            height: self.doc.height,
            data: canvas.to_rgba8(),
        })
    }

    /// doc-sized composite WITHOUT the backdrop — straight-alpha stack.
    /// Served from out_cache when the render key is unchanged.
    pub fn render_raw(&mut self) -> Result<LayerPixels> {
        let key = self.render_key();
        if let Some((k, pix)) = &self.out_cache {
            if *k == key {
                return Ok(pix.clone());
            }
        }
        let w = self.doc.width;
        let h = self.doc.height;
        let mut canvas = LayerPixels::empty(w, h);
        // move the layer vec out rather than cloning it — embedded rasters
        // and masks are heavy and this list gets walked every render
        let layers = std::mem::take(&mut self.doc.layers);
        let r = self.composite_list(&mut canvas, &layers);
        self.doc.layers = layers;
        r?;
        self.out_cache = Some((key, canvas.clone()));
        Ok(canvas)
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
    /// Backdrop-dependent blends (multiply etc.) bake against the real
    /// composite below the pair — matching Photoshop's merge-down result —
    /// while coverage outside the pair stays transparent so the layers
    /// underneath still show through.
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
        let below = self.doc.layers[..i - 1].to_vec();
        // pair alpha coverage, rendered in isolation
        let mut cov = LayerPixels::empty(self.doc.width, self.doc.height);
        self.composite_list(&mut cov, &pair)?;
        // pair composited over the real backdrop of everything below it
        let mut buf = LayerPixels::empty(self.doc.width, self.doc.height);
        self.composite_list(&mut buf, &below)?;
        self.composite_list(&mut buf, &pair)?;
        // merged pixel = buf rgb where the pair covered anything; alpha is
        // the pair's own coverage (the backdrop rgb is baked in for blended
        // areas, like PS does)
        for k in 0..buf.data.len() {
            buf.data[k][3] = cov.data[k][3];
        }
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

    /// bake the layer stack into a single raster layer named `name`.
    /// The backdrop is NOT baked in — it stays a document setting so the
    /// flattened composite is pixel-identical to what was on screen.
    pub fn flatten(&mut self, name: &str) -> Result<u64> {
        let img = self.render_raw()?;
        let mut l = Layer::raster(name, img.w, img.h, img.to_rgba8());
        l.id = self.doc.next_id;
        self.doc.next_id += 1;
        let id = l.id;
        self.doc.layers = vec![l];
        self.cache.clear();
        self.out_cache = None;
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
                    self.draw_shadow(canvas, &gbuf, layer);
                    self.blend_layer(canvas, &gbuf, layer);
                }
                _ => {
                    let pix = self.rasterize_layer(layer)?;
                    self.draw_shadow(canvas, &pix, layer);
                    self.blend_layer(canvas, &pix, layer);
                }
            }
        }
        Ok(())
    }

    /// layer styles: drop shadow = the layer's own alpha silhouette, filled
    /// with the shadow colour, blurred and offset, composited beneath the
    /// layer (above everything below it) with a normal src-over blend.
    fn draw_shadow(&self, canvas: &mut LayerPixels, pix: &LayerPixels, layer: &Layer) {
        let Some(sh) = layer.styles.drop_shadow.as_ref() else {
            return;
        };
        if sh.color[3] <= 0.0 {
            return;
        }
        // silhouette at layer scale — shadow offset scales with the layer,
        // like Photoshop
        let scale = layer.scale.max(1e-4);
        let sw = ((pix.w as f32 * scale).ceil() as u32).max(1);
        let sh_px = ((pix.h as f32 * scale).ceil() as u32).max(1);
        let mut alpha = vec![0.0f32; (sw * sh_px) as usize];
        for dy in 0..sh_px {
            for dx in 0..sw {
                let sx = (dx as f32 / scale).min(pix.w.saturating_sub(1) as f32);
                let sy = (dy as f32 / scale).min(pix.h.saturating_sub(1) as f32);
                alpha[(dy * sw + dx) as usize] =
                    pix.data[(sy.floor() as u32 * pix.w + sx.floor() as u32) as usize][3];
            }
        }
        // spread: threshold-widen the silhouette before the blur
        if sh.spread > 0.0 {
            let grow = (sh.blur.max(1.0) * sh.spread.clamp(0.0, 1.0)).round() as usize;
            if grow > 0 {
                dilate(&mut alpha, sw, sh_px, grow);
            }
        }
        let r = sh.blur.max(0.0).round() as usize;
        if r > 0 {
            box_blur(&mut alpha, sw, sh_px, r, 3);
        }
        let mut shadow = LayerPixels::empty(sw, sh_px);
        for (i, a) in alpha.iter().enumerate() {
            let a = (*a * sh.color[3]).clamp(0.0, 1.0);
            if a > 0.0 {
                shadow.data[i] = [sh.color[0], sh.color[1], sh.color[2], a];
            }
        }
        // reuse blend_layer with a synthetic placement layer (normal blend,
        // full opacity, no mask/styles)
        let mut sl = Layer::raster("shadow", sw, sh_px, Vec::new());
        sl.x = layer.x + sh.dx.round() as i32;
        sl.y = layer.y + sh.dy.round() as i32;
        sl.scale = 1.0;
        sl.opacity = layer.opacity;
        self.blend_layer(canvas, &shadow, &sl);
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

    /// placed bounds of a layer in document coords [x, y, w, h].
    /// Group/adjustment/develop layers cover their placement at doc scale;
    /// the pixel bounds come from the compositor's own raster cache.
    pub fn layer_bounds(&mut self, id: u64) -> Result<[f32; 4]> {
        let layer = self
            .doc
            .layer(id)
            .with_context(|| format!("layer {id} not found"))?
            .clone();
        let (w, h) = self.placed_size(&layer)?;
        Ok([layer.x as f32, layer.y as f32, w, h])
    }

    /// placed bounds for every top-level layer, bottom→top order
    pub fn all_bounds(&mut self) -> Vec<(u64, [f32; 4])> {
        let ids: Vec<u64> = self.doc.layers.iter().map(|l| l.id).collect();
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Ok(b) = self.layer_bounds(id) {
                out.push((id, b));
            }
        }
        out
    }

    /// placed (scaled) size of a layer; doc-sized for content kinds that
    /// rasterize in document space
    fn placed_size(&mut self, layer: &Layer) -> Result<(f32, f32)> {
        let s = layer.scale.max(1e-4);
        let (w, h) = match &layer.kind {
            LayerKind::Raster { width, height, .. } => (*width as f32, *height as f32),
            LayerKind::Text { text } => {
                let (_, tw, th) = crate::text::rasterize(text)?;
                (tw as f32, th as f32)
            }
            _ => (self.doc.width as f32, self.doc.height as f32),
        };
        Ok((w * s, h * s))
    }

    /// topmost layer whose placed, masked pixel at doc point (x, y) has
    /// alpha > 0. Adjustment layers are skipped — they cover the whole
    /// canvas and would swallow every pick.
    pub fn pick(&mut self, x: f32, y: f32) -> Result<Option<u64>> {
        let layers = self.doc.layers.clone();
        for layer in layers.iter().rev() {
            if !layer.visible || matches!(layer.kind, LayerKind::Adjustment { .. }) {
                continue;
            }
            if let LayerKind::Group { children } = &layer.kind {
                // hit-test the group's rendered buffer
                let mut gbuf = LayerPixels::empty(self.doc.width, self.doc.height);
                self.composite_list(&mut gbuf, children)?;
                let ix = (x - layer.x as f32).floor() as i64;
                let iy = (y - layer.y as f32).floor() as i64;
                if ix >= 0 && iy >= 0 && ix < gbuf.w as i64 && iy < gbuf.h as i64 {
                    if gbuf.data[(iy as u32 * gbuf.w + ix as u32) as usize][3] > 0.0 {
                        return Ok(Some(layer.id));
                    }
                }
                continue;
            }
            let pix = self.rasterize_layer(layer)?;
            let s = layer.scale.max(1e-4);
            let lx = (x - layer.x as f32) / s;
            let ly = (y - layer.y as f32) / s;
            if lx < 0.0 || ly < 0.0 || lx >= pix.w as f32 || ly >= pix.h as f32 {
                continue;
            }
            let sx = lx.floor() as u32;
            let sy = ly.floor() as u32;
            let mut a = pix.data[(sy * pix.w + sx) as usize][3];
            if a <= 0.0 {
                continue;
            }
            if let Some(m) = &layer.mask {
                let m = if m.feather > 0.0 {
                    blur_mask(m)
                } else {
                    m.clone()
                };
                a *= m.at(
                    sx.min(m.width.saturating_sub(1)),
                    sy.min(m.height.saturating_sub(1)),
                );
            }
            if a * layer.opacity.clamp(0.0, 1.0) > 0.0 {
                return Ok(Some(layer.id));
            }
        }
        Ok(None)
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
        // fully off-canvas — nothing to blend
        if x0 >= cw || y0 >= ch || x0 + dw <= 0 || y0 + dh <= 0 {
            return;
        }
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

/// separable box blur, `passes` iterations (3 ≈ gaussian), in place
fn box_blur(data: &mut [f32], w: u32, h: u32, r: usize, passes: usize) {
    let (w, h) = (w as usize, h as usize);
    if r == 0 || w == 0 || h == 0 {
        return;
    }
    let mut tmp = vec![0.0f32; w * h];
    for _ in 0..passes {
        for y in 0..h {
            let mut s = 0.0f32;
            let mut n = 0usize;
            for k in 0..(r + 1).min(w) {
                s += data[y * w + k];
                n += 1;
            }
            tmp[y * w] = s / n as f32;
            for x in 1..w {
                if x + r < w {
                    s += data[y * w + x + r];
                    n += 1;
                }
                if x > r {
                    s -= data[y * w + x - r - 1];
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
                data[y * w + x] = s / n as f32;
            }
        }
    }
}

/// max-filter dilation of a scalar field (morphological grow), in place
fn dilate(data: &mut [f32], w: u32, h: u32, r: usize) {
    let (w, h) = (w as usize, h as usize);
    if r == 0 || w == 0 || h == 0 {
        return;
    }
    let mut tmp = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut m = 0.0f32;
            for k in x.saturating_sub(r)..(x + r + 1).min(w) {
                m = m.max(data[y * w + k]);
            }
            tmp[y * w + x] = m;
        }
    }
    for y in 0..h {
        for x in 0..w {
            let mut m = 0.0f32;
            for k in y.saturating_sub(r)..(y + r + 1).min(h) {
                m = m.max(tmp[k * w + x]);
            }
            data[y * w + x] = m;
        }
    }
}

/// 3-pass box blur ≈ gaussian for mask feathering
fn blur_mask(m: &Mask) -> Mask {
    let r = m.feather.max(0.0).round() as usize;
    if r == 0 {
        return m.clone();
    }
    let mut out = m.clone();
    box_blur(&mut out.data, m.width, m.height, r, 3);
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
