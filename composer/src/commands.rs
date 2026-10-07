//! Everything-is-a-command: a single JSON dispatcher shared by the CLI, the
//! control channel, the MCP server and the macOS FFI.
//!
//! Requests:  {"id": "<command>", ...params}
//! Responses: {"ok": true, "result": ...} or {"ok": false, "error": "..."}
//!
//! Command ids are stable — docs/parity.md tracks them and `cargo xtask`-style
//! checks (scripts/parity_check.sh) verify every id cited there still exists.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use koubou_core::{Engine, Recipe};

use crate::blend::BlendMode;
use crate::composite::Composer;
use crate::doc::{Document, Fill, Layer, LayerKind, Mask, Shape, Stroke, TextAlign, TextContent};

/// one app session: an imaging engine plus an optional open document,
/// an optional motion timeline and an optional pages layout. `tl.*` and
/// `pg.*` ids are delegated to those crates — one Session, every domain.
pub struct Session {
    engine: Engine,
    pub composer: Option<Composer>,
    motion: Option<koubou_motion::TlSession>,
    pages: Option<koubou_pages::PgSession>,
}

impl Session {
    pub fn new() -> Result<Session> {
        Ok(Session {
            engine: Engine::new()?,
            composer: None,
            motion: None,
            pages: None,
        })
    }

    pub fn dispatch(&mut self, v: &Value) -> Value {
        let id = v.get("id").and_then(Value::as_str).unwrap_or("");
        // domain routing: tl.* → motion timeline session, pg.* → pages
        // session. Their dispatch(id, v) -> Result<Value> mirrors run();
        // same never-crash guard + envelope at this edge.
        if id.starts_with("tl.") {
            let s = self
                .motion
                .get_or_insert_with(koubou_motion::TlSession::new);
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.dispatch(id, v)));
            return match r {
                Ok(Ok(result)) => json!({"ok": true, "result": result}),
                Ok(Err(e)) => json!({"ok": false, "error": format!("{e:#}")}),
                Err(_) => json!({"ok": false, "error": format!("command '{id}' panicked")}),
            };
        }
        if id.starts_with("pg.") {
            let s = self.pages.get_or_insert_with(koubou_pages::PgSession::new);
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.dispatch(id, v)));
            return match r {
                Ok(Ok(result)) => json!({"ok": true, "result": result}),
                Ok(Err(e)) => json!({"ok": false, "error": format!("{e:#}")}),
                Err(_) => json!({"ok": false, "error": format!("command '{id}' panicked")}),
            };
        }
        // never-crash guard: a panicking command becomes an error, not a dead session
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run(id, v)));
        match r {
            Ok(Ok(result)) => json!({"ok": true, "result": result}),
            Ok(Err(e)) => json!({"ok": false, "error": format!("{e:#}")}),
            Err(_) => json!({"ok": false, "error": format!("command '{id}' panicked")}),
        }
    }

    /// the command ids this dispatcher understands (parity checks read this)
    pub fn command_ids() -> &'static [&'static str] {
        &[
            "ping",
            "commands",
            "scan",
            "meta",
            "thumb",
            "render",
            "auto",
            "sidecar.read",
            "sidecar.write",
            "setRating",
            "setLabel",
            "doc.new",
            "doc.fromPhoto",
            "doc.open",
            "doc.save",
            "doc.json",
            "doc.importPsd",
            "doc.addLayer",
            "doc.setLayer",
            "doc.removeLayer",
            "doc.reorder",
            "doc.render",
            "doc.addShape",
            "doc.shapeSet",
            "doc.shapeRemove",
            "doc.maskPaint",
            // motion domain (koubou-motion / .kmotion)
            "tl.new",
            "tl.open",
            "tl.save",
            "tl.json",
            "tl.addTrack",
            "tl.addClip",
            "tl.setClip",
            "tl.removeClip",
            "tl.addCue",
            "tl.probe",
            "tl.renderFrame",
            "tl.render",
            "tl.detectSilence",
            "tl.generateClip",
            // pages domain (koubou-pages / .kpages)
            "pg.new",
            "pg.open",
            "pg.save",
            "pg.json",
            "pg.addPage",
            "pg.removePage",
            "pg.addFrame",
            "pg.setFrame",
            "pg.removeFrame",
            "pg.setMaster",
            "pg.render",
            "pg.renderPng",
        ]
    }

    fn run(&mut self, id: &str, v: &Value) -> Result<Value> {
        match id {
            "ping" => Ok(json!({"name": "koubou", "version": env!("CARGO_PKG_VERSION")})),
            "commands" => Ok(json!(Self::command_ids())),
            "scan" => {
                let f = req_str(v, "folder")?;
                let entries = self.engine.scan(Path::new(&f))?;
                Ok(serde_json::to_value(entries)?)
            }
            "meta" => {
                let p = req_str(v, "path")?;
                self.engine.metadata(Path::new(&p))
            }
            "thumb" => {
                let p = req_str(v, "path")?;
                let max = v.get("maxPx").and_then(Value::as_u64).unwrap_or(512) as u32;
                let img = self.engine.thumbnail(Path::new(&p), max)?;
                write_image(&img, v.get("out").and_then(Value::as_str))
            }
            "render" => {
                let p = req_str(v, "path")?;
                let recipe = recipe_arg(v)?;
                let max = v.get("maxPx").and_then(Value::as_u64).unwrap_or(0) as u32;
                let img = self.engine.render(Path::new(&p), &recipe, max)?;
                write_image(&img, v.get("out").and_then(Value::as_str))
            }
            "auto" => {
                let p = req_str(v, "path")?;
                let r = self.engine.auto_analyze(Path::new(&p))?;
                serde_json::to_value(r).map_err(Into::into)
            }
            "sidecar.read" => {
                let p = req_str(v, "path")?;
                let s = self.engine.read_sidecar(Path::new(&p))?;
                serde_json::to_value(s).map_err(Into::into)
            }
            "sidecar.write" => {
                let p = req_str(v, "path")?;
                let j = req_str(v, "json")?;
                let s: koubou_core::recipe::Sidecar =
                    serde_json::from_str(&j).context("sidecar json")?;
                self.engine.write_sidecar(Path::new(&p), &s)?;
                Ok(json!("ok"))
            }
            "setRating" => {
                let p = req_str(v, "path")?;
                let r = v.get("rating").and_then(Value::as_i64).unwrap_or(0) as i32;
                self.engine.set_rating(Path::new(&p), r)?;
                Ok(json!("ok"))
            }
            "setLabel" => {
                let p = req_str(v, "path")?;
                let l = v.get("label").and_then(Value::as_str).unwrap_or("");
                self.engine.set_label(Path::new(&p), l)?;
                Ok(json!("ok"))
            }
            "doc.new" => {
                let w = v.get("w").and_then(Value::as_u64).unwrap_or(1920) as u32;
                let h = v.get("h").and_then(Value::as_u64).unwrap_or(1080) as u32;
                let name = v.get("name").and_then(Value::as_str).unwrap_or("Untitled");
                self.composer = Some(Composer::new(Document::new(name, w, h))?);
                Ok(json!({"w": w, "h": h}))
            }
            "doc.fromPhoto" => {
                let p = req_str(v, "path")?;
                let path = PathBuf::from(&p);
                let meta = self.engine.metadata(&path)?;
                // raster dims may be absent on older araware-core — fall back
                // to reading the file header directly.
                let dims = || -> Result<(u64, u64)> {
                    image::image_dimensions(&path)
                        .map_err(anyhow::Error::from)
                        .map(|(w, h)| (w as u64, h as u64))
                };
                let w = meta["width"]
                    .as_u64()
                    .or_else(|| dims().ok().map(|d| d.0))
                    .context("meta width")? as u32;
                let h = meta["height"]
                    .as_u64()
                    .or_else(|| dims().ok().map(|d| d.1))
                    .context("meta height")? as u32;
                self.composer = Some(Composer::new(Document::from_photo(&path, w, h))?);
                Ok(json!({"w": w, "h": h}))
            }
            "doc.open" => {
                let p = req_str(v, "path")?;
                let d = Document::load(Path::new(&p))?;
                self.composer = Some(Composer::new(d)?);
                Ok(json!("ok"))
            }
            "doc.save" => {
                let c = self.composer.as_ref().context("no document")?;
                let p = v
                    .get("path")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(format!("{}.koubou", c.doc.name)));
                c.doc.save(&p)?;
                Ok(json!({"path": p.to_string_lossy()}))
            }
            "doc.json" => {
                let c = self.composer.as_ref().context("no document")?;
                serde_json::to_value(&c.doc).map_err(Into::into)
            }
            "doc.importPsd" => {
                let p = req_str(v, "path")?;
                let d = crate::psd::import_psd(Path::new(&p))?;
                self.composer = Some(Composer::new(d)?);
                Ok(json!("ok"))
            }
            "doc.addLayer" => {
                let c = self.composer.as_mut().context("no document")?;
                let layer = layer_from(v)?;
                let id = c.doc.add_layer(layer);
                Ok(json!({"layerId": id}))
            }
            "doc.setLayer" => {
                let c = self.composer.as_mut().context("no document")?;
                let id = layer_id(v)?;
                set_layer(&mut c.doc, id, v)
            }
            "doc.removeLayer" => {
                let c = self.composer.as_mut().context("no document")?;
                let id = layer_id(v)?;
                c.doc.remove_layer(id).context("layer not found")?;
                Ok(json!("ok"))
            }
            "doc.reorder" => {
                let c = self.composer.as_mut().context("no document")?;
                let id = layer_id(v)?;
                let to = v.get("to").and_then(Value::as_u64).unwrap_or(0) as usize;
                if !c.doc.reorder(id, to) {
                    anyhow::bail!("layer {id} not found");
                }
                Ok(json!("ok"))
            }
            "doc.render" => {
                let c = self.composer.as_mut().context("no document")?;
                let max = v.get("maxPx").and_then(Value::as_u64).unwrap_or(0) as u32;
                let img = if max > 0 {
                    c.render_preview(max)?
                } else {
                    c.render()?
                };
                write_image(&img, v.get("out").and_then(Value::as_str))
            }
            "doc.maskPaint" => {
                let c = self.composer.as_mut().context("no document")?;
                mask_paint(&mut c.doc, v)
            }
            "doc.addShape" => {
                let c = self.composer.as_mut().context("no document")?;
                let id = layer_id(v)?;
                let l = c.doc.layer_mut(id).context("layer not found")?;
                let shapes = match &mut l.kind {
                    LayerKind::Shape { shapes } => shapes,
                    _ => anyhow::bail!("layer {id} is not a shape layer"),
                };
                let d = v
                    .get("d")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| crate::shape::gen_path(v))
                    .context("need 'd' or 'gen'")?;
                let fill = v
                    .get("fill")
                    .and_then(|c| serde_json::from_value::<[f32; 4]>(c.clone()).ok());
                let stroke = v
                    .get("stroke")
                    .and_then(|c| serde_json::from_value::<Stroke>(c.clone()).ok());
                shapes.push(Shape { d, fill, stroke });
                let ix = shapes.len() - 1;
                l.gen += 1;
                Ok(json!({"index": ix}))
            }
            "doc.shapeSet" => {
                let c = self.composer.as_mut().context("no document")?;
                let id = layer_id(v)?;
                let ix = v.get("index").and_then(Value::as_u64).context("index")? as usize;
                let l = c.doc.layer_mut(id).context("layer not found")?;
                let shapes = match &mut l.kind {
                    LayerKind::Shape { shapes } => shapes,
                    _ => anyhow::bail!("layer {id} is not a shape layer"),
                };
                let s = shapes.get_mut(ix).with_context(|| format!("shape {ix}"))?;
                if let Some(d) = v.get("d").and_then(Value::as_str) {
                    s.d = d.to_string();
                } else if let Some(d) = crate::shape::gen_path(v) {
                    s.d = d;
                }
                if v.get("removeFill")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    s.fill = None;
                } else if let Some(f) = v.get("fill") {
                    s.fill = serde_json::from_value(f.clone()).ok();
                }
                if v.get("removeStroke")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    s.stroke = None;
                } else if let Some(st) = v.get("stroke") {
                    s.stroke = serde_json::from_value(st.clone()).ok();
                }
                l.gen += 1;
                Ok(json!("ok"))
            }
            "doc.shapeRemove" => {
                let c = self.composer.as_mut().context("no document")?;
                let id = layer_id(v)?;
                let ix = v.get("index").and_then(Value::as_u64).context("index")? as usize;
                let l = c.doc.layer_mut(id).context("layer not found")?;
                match &mut l.kind {
                    LayerKind::Shape { shapes } => {
                        if ix >= shapes.len() {
                            anyhow::bail!("shape {ix} out of range");
                        }
                        shapes.remove(ix);
                    }
                    _ => anyhow::bail!("layer {id} is not a shape layer"),
                }
                l.gen += 1;
                Ok(json!("ok"))
            }
            _ => anyhow::bail!("unknown command id: {id}"),
        }
    }
}

fn req_str(v: &Value, k: &str) -> Result<String> {
    v.get(k)
        .and_then(Value::as_str)
        .map(str::to_string)
        .with_context(|| format!("missing param '{k}'"))
}

fn req_u64(v: &Value, k: &str) -> Result<u64> {
    v.get(k)
        .and_then(Value::as_u64)
        .with_context(|| format!("missing param '{k}'"))
}

fn recipe_arg(v: &Value) -> Result<Recipe> {
    match v.get("recipe") {
        None => Ok(Recipe::default()),
        Some(Value::Null) => Ok(Recipe::default()),
        Some(r) if r.is_string() => {
            Recipe::from_json(r.as_str().unwrap()).context("invalid recipe JSON")
        }
        Some(r) => serde_json::from_value(r.clone()).map_err(Into::into),
    }
}

fn write_image(img: &koubou_core::develop::RgbaImage, out: Option<&str>) -> Result<Value> {
    let rgba = image::RgbaImage::from_raw(img.width, img.height, img.data.clone())
        .context("image buffer")?;
    match out {
        Some(path) => {
            rgba.save(path).with_context(|| format!("save {path}"))?;
            Ok(json!({"path": path, "w": img.width, "h": img.height}))
        }
        None => {
            use base64::Engine as _;
            let mut buf = std::io::Cursor::new(Vec::new());
            rgba.write_to(&mut buf, image::ImageFormat::Png)?;
            Ok(json!({
                "pngB64": base64::engine::general_purpose::STANDARD.encode(buf.into_inner()),
                "w": img.width,
                "h": img.height,
            }))
        }
    }
}

/// build a Layer from a doc.addLayer spec:
/// {"kind":"fill","name":"bg","color":[r,g,b,a]}
/// {"kind":"gradient","line":[x0,y0,x1,y1],"stops":[[p,r,g,b,a],...]}
/// {"kind":"adjustment","name":"curves","recipe":{...}}
/// {"kind":"text","text":"...","font":"","size":96,"x":..,"y":..}
/// {"kind":"shape","shapes":[{"d":"M ...","fill":[..],"stroke":{...}}]}
/// {"kind":"develop","path":"/abs/file"}
/// {"kind":"rasterFile","path":"/abs/img.png"}
/// {"kind":"raster","name":"px","w":..,"h":..,"rgbaB64":".."}
/// {"kind":"group","name":"g"}
fn layer_from(v: &Value) -> Result<Layer> {
    let kind = req_str(v, "kind")?;
    let name = v.get("name").and_then(Value::as_str).unwrap_or("Layer");
    let mut l = match kind.as_str() {
        "fill" => {
            let c = v
                .get("color")
                .and_then(|c| serde_json::from_value::<[f32; 4]>(c.clone()).ok())
                .unwrap_or([1.0, 1.0, 1.0, 1.0]);
            Layer::fill(name, Fill::Solid { color: c })
        }
        "gradient" => {
            let line = v
                .get("line")
                .and_then(|c| serde_json::from_value::<[f32; 4]>(c.clone()).ok())
                .unwrap_or([0.0, 0.0, 1.0, 0.0]);
            let stops = v
                .get("stops")
                .and_then(|c| serde_json::from_value::<Vec<[f32; 5]>>(c.clone()).ok())
                .unwrap_or_else(|| vec![[0.0, 0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0, 1.0]]);
            Layer::fill(name, Fill::LinearGradient { line, stops })
        }
        "adjustment" => Layer::adjustment(name, recipe_arg(v)?),
        "text" => {
            let mut t: TextContent = match v.get("text") {
                Some(Value::Object(_)) => {
                    serde_json::from_value(v["text"].clone()).context("text spec")?
                }
                Some(Value::String(s)) => TextContent {
                    text: s.clone(),
                    ..Default::default()
                },
                _ => TextContent::default(),
            };
            if let Some(s) = v.get("size").and_then(Value::as_f64) {
                t.size = s as f32;
            }
            if let Some(s) = v.get("font").and_then(Value::as_str) {
                t.font = s.to_string();
            }
            if let Some(a) = v.get("align").and_then(Value::as_str) {
                t.align = match a {
                    "center" => TextAlign::Center,
                    "right" => TextAlign::Right,
                    _ => TextAlign::Left,
                };
            }
            if let Some(c) = v
                .get("color")
                .and_then(|c| serde_json::from_value::<[f32; 4]>(c.clone()).ok())
            {
                t.color = c;
            }
            Layer::text(name, t)
        }
        "shape" => {
            let mut shapes: Vec<Shape> = Vec::new();
            let d_str = v
                .get("d")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| crate::shape::gen_path(v));
            if let Some(d) = d_str {
                let fill = v
                    .get("fill")
                    .and_then(|c| serde_json::from_value::<[f32; 4]>(c.clone()).ok());
                let stroke = v
                    .get("stroke")
                    .and_then(|c| serde_json::from_value::<Stroke>(c.clone()).ok());
                shapes.push(Shape { d, fill, stroke });
            }
            if let Some(arr) = v.get("shapes") {
                shapes = serde_json::from_value(arr.clone()).context("shapes")?;
            }
            Layer::shape(name, shapes)
        }
        "develop" => {
            let p = req_str(v, "path")?;
            let mut l = Layer::develop(Path::new(&p));
            if let Ok(r) = recipe_arg(v) {
                if let LayerKind::Develop { recipe, .. } = &mut l.kind {
                    *recipe = r;
                }
            }
            l
        }
        "rasterFile" => {
            let p = req_str(v, "path")?;
            Layer::base(
                name,
                LayerKind::Raster {
                    width: 0,
                    height: 0,
                    src: crate::doc::RasterSrc::File { path: p.into() },
                },
            )
        }
        "raster" => {
            use base64::Engine as _;
            let w = req_u64(v, "w")? as u32;
            let h = req_u64(v, "h")? as u32;
            let b64 = req_str(v, "rgbaB64")?;
            let raw = base64::engine::general_purpose::STANDARD
                .decode(&b64)
                .context("rgbaB64")?;
            if raw.len() != (w * h * 4) as usize {
                anyhow::bail!("raster: {} bytes != {}*{}*4", raw.len(), w, h);
            }
            Layer::base(
                name,
                LayerKind::Raster {
                    width: w,
                    height: h,
                    src: crate::doc::RasterSrc::Embedded { png_b64: b64 },
                },
            )
        }
        "group" => Layer::group(name, Vec::new()),
        _ => anyhow::bail!("unknown layer kind: {kind}"),
    };
    if let Some(x) = v.get("x").and_then(Value::as_i64) {
        l.x = x as i32;
    }
    if let Some(y) = v.get("y").and_then(Value::as_i64) {
        l.y = y as i32;
    }
    if let Some(o) = v.get("opacity").and_then(Value::as_f64) {
        l.opacity = o as f32;
    }
    if let Some(b) = v.get("blend").and_then(Value::as_str) {
        l.blend = BlendMode::parse(b).unwrap_or(BlendMode::Normal);
    }
    Ok(l)
}

/// layer id param — "id" is taken by the command name, so layer-targeting
/// commands use "layer": {"id":"doc.setLayer","layer":3,...}
fn layer_id(v: &Value) -> Result<u64> {
    v.get("layer")
        .and_then(Value::as_u64)
        .with_context(|| "missing param 'layer' (numeric layer id)")
}

/// doc.setLayer params — all optional
fn set_layer(doc: &mut Document, id: u64, v: &Value) -> Result<Value> {
    let l = doc
        .layer_mut(id)
        .with_context(|| format!("layer {id} not found"))?;
    if let Some(n) = v.get("name").and_then(Value::as_str) {
        l.name = n.to_string();
    }
    if let Some(b) = v.get("visible").and_then(Value::as_bool) {
        l.visible = b;
    }
    if let Some(o) = v.get("opacity").and_then(Value::as_f64) {
        l.opacity = o as f32;
    }
    if let Some(b) = v.get("blend").and_then(Value::as_str) {
        l.blend = BlendMode::parse(b).with_context(|| format!("blend '{b}'"))?;
    }
    if let Some(x) = v.get("x").and_then(Value::as_i64) {
        l.x = x as i32;
    }
    if let Some(y) = v.get("y").and_then(Value::as_i64) {
        l.y = y as i32;
    }
    if let Some(s) = v.get("scale").and_then(Value::as_f64) {
        l.scale = s as f32;
    }
    if v.get("mask").is_some_and(|m| m.is_null()) {
        l.mask = None;
    }
    if let Some(r) = v.get("recipe") {
        let recipe = if r.is_string() {
            Recipe::from_json(r.as_str().unwrap()).context("recipe json")?
        } else {
            serde_json::from_value(r.clone()).context("recipe")?
        };
        match &mut l.kind {
            LayerKind::Develop { recipe: rc, .. } | LayerKind::Adjustment { recipe: rc } => {
                *rc = recipe;
            }
            _ => anyhow::bail!("layer {id} is not a develop/adjustment layer"),
        }
        l.gen += 1;
    }
    if let Some(t) = v.get("text") {
        match &mut l.kind {
            LayerKind::Text { text } => {
                if t.is_string() {
                    text.text = t.as_str().unwrap().to_string();
                } else {
                    *text = serde_json::from_value(t.clone()).context("text")?;
                }
            }
            _ => anyhow::bail!("layer {id} is not a text layer"),
        }
        l.gen += 1;
    }
    if let Some(f) = v.get("fill") {
        match &mut l.kind {
            LayerKind::Fill { fill } => {
                *fill = serde_json::from_value(f.clone()).context("fill")?;
            }
            _ => anyhow::bail!("layer {id} is not a fill layer"),
        }
        l.gen += 1;
    }
    if let Some(s) = v.get("shapes") {
        match &mut l.kind {
            LayerKind::Shape { shapes } => {
                *shapes = serde_json::from_value(s.clone()).context("shapes")?;
            }
            _ => anyhow::bail!("layer {id} is not a shape layer"),
        }
        l.gen += 1;
    }
    // any param edit invalidates the cache
    l.gen += 1;
    Ok(json!("ok"))
}

/// paint a soft round dab into a layer's mask (layer pixel space coords)
fn mask_paint(doc: &mut Document, v: &Value) -> Result<Value> {
    let id = layer_id(v)?;
    let cx = v.get("cx").and_then(Value::as_f64).unwrap_or(0.0) as f32;
    let cy = v.get("cy").and_then(Value::as_f64).unwrap_or(0.0) as f32;
    let r = v.get("r").and_then(Value::as_f64).unwrap_or(16.0) as f32;
    let val = v.get("value").and_then(Value::as_f64).unwrap_or(0.0) as f32;
    let soft = v.get("softness").and_then(Value::as_f64).unwrap_or(0.5) as f32;
    let (w, h) = match doc.layer(id) {
        Some(Layer {
            kind: LayerKind::Adjustment { .. },
            ..
        })
        | None => (doc.width, doc.height),
        Some(_l) => (doc.width, doc.height), // masks live in layer space = doc space for now
    };
    let l = doc.layer_mut(id).context("layer not found")?;
    let m = l.mask.get_or_insert_with(|| Mask::full(w, h));
    let r2 = r * r;
    let (x0, x1) = (
        (cx - r - 1.0).max(0.0) as u32,
        ((cx + r + 1.0) as u32).min(m.width),
    );
    let (y0, y1) = (
        (cy - r - 1.0).max(0.0) as u32,
        ((cy + r + 1.0) as u32).min(m.height),
    );
    for y in y0..y1.min(m.height) {
        for x in x0..x1.min(m.width) {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let d2 = dx * dx + dy * dy;
            if d2 > r2 {
                continue;
            }
            let d = d2.sqrt();
            // hard edge at r*(1-soft), fades to r
            let edge = r * (1.0 - soft.clamp(0.0, 0.99));
            let a = if d <= edge {
                1.0
            } else {
                1.0 - (d - edge) / (r - edge).max(1.0)
            };
            let i = (y * m.width + x) as usize;
            // paint toward `value`, weighted by dab alpha
            m.data[i] = (m.data[i] * (1.0 - a) + val.clamp(0.0, 1.0) * a).clamp(0.0, 1.0);
        }
    }
    l.gen += 1;
    Ok(json!("ok"))
}
