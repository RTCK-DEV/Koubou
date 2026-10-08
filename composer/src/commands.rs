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

/// max snapshots kept per domain (doc snapshots carry embedded rasters —
/// bounded so scrubbing sliders doesn't grow memory without limit)
const HISTORY_CAP: usize = 32;

/// one app session: an imaging engine plus an optional open document,
/// an optional motion timeline and an optional pages layout. `tl.*` and
/// `pg.*` ids are delegated to those crates — one Session, every domain.
pub struct Session {
    engine: Engine,
    pub composer: Option<Composer>,
    motion: Option<koubou_motion::TlSession>,
    pages: Option<koubou_pages::PgSession>,
    /// snapshot stacks per domain ("doc"/"tl"/"pg") for *.undo/*.redo
    undo: std::collections::HashMap<&'static str, std::collections::VecDeque<Value>>,
    redo: std::collections::HashMap<&'static str, std::collections::VecDeque<Value>>,
}

/// which history domain a command belongs to
fn history_domain(id: &str) -> Option<&'static str> {
    if id.starts_with("doc.") {
        Some("doc")
    } else if id.starts_with("tl.") {
        Some("tl")
    } else if id.starts_with("pg.") {
        Some("pg")
    } else {
        None
    }
}

/// commands that change document state (snapshot before them); reads,
/// renders, saves and the history commands themselves are excluded.
fn is_mutating(id: &str) -> bool {
    !matches!(
        id,
        "doc.json"
            | "doc.render"
            | "doc.exportLayer"
            | "doc.exportPsd"
            | "doc.pick"
            | "doc.bounds"
            | "doc.info"
            | "doc.save"
            | "doc.undo"
            | "doc.redo"
            | "tl.json"
            | "tl.probe"
            | "tl.renderFrame"
            | "tl.render"
            | "tl.detectSilence"
            | "tl.save"
            | "tl.undo"
            | "tl.redo"
            | "pg.json"
            | "pg.render"
            | "pg.renderPng"
            | "pg.save"
            | "pg.undo"
            | "pg.redo"
    )
}

impl Session {
    pub fn new() -> Result<Session> {
        Ok(Session {
            engine: Engine::new()?,
            composer: None,
            motion: None,
            pages: None,
            undo: std::collections::HashMap::new(),
            redo: std::collections::HashMap::new(),
        })
    }

    /// serialised state of one domain's document (Null when none open)
    fn domain_snapshot(&self, d: &str) -> Value {
        let inner = match d {
            "doc" => self
                .composer
                .as_ref()
                .and_then(|c| serde_json::to_value(&c.doc).ok()),
            "tl" => self
                .motion
                .as_ref()
                .and_then(|m| m.timeline.as_ref())
                .and_then(|t| serde_json::to_value(t).ok()),
            "pg" => self
                .pages
                .as_ref()
                .and_then(|p| p.doc.as_ref())
                .and_then(|d| serde_json::to_value(d).ok()),
            _ => None,
        };
        inner.unwrap_or(Value::Null)
    }

    /// replace one domain's document state from a snapshot
    fn domain_restore(&mut self, d: &str, snap: Value) -> Result<()> {
        match d {
            "doc" => {
                if snap.is_null() {
                    self.composer = None;
                } else {
                    let doc: Document = serde_json::from_value(snap).context("restore document")?;
                    self.composer = Some(Composer::new(doc)?);
                }
            }
            "tl" => {
                let s = self
                    .motion
                    .get_or_insert_with(koubou_motion::TlSession::new);
                s.timeline = if snap.is_null() {
                    None
                } else {
                    Some(serde_json::from_value(snap).context("restore timeline")?)
                };
            }
            "pg" => {
                let s = self.pages.get_or_insert_with(koubou_pages::PgSession::new);
                s.doc = if snap.is_null() {
                    None
                } else {
                    Some(serde_json::from_value(snap).context("restore pages doc")?)
                };
            }
            _ => {}
        }
        Ok(())
    }

    fn history(&mut self, id: &str) -> Result<Value> {
        let dom = history_domain(id).context("not a history command")?;
        let undoing = id.ends_with("undo");
        let src = if undoing {
            &mut self.undo
        } else {
            &mut self.redo
        };
        let Some(snap) = src.get_mut(dom).and_then(|s| s.pop_back()) else {
            return Ok(json!({"changed": false}));
        };
        let cur = self.domain_snapshot(dom);
        self.domain_restore(dom, snap)?;
        let dst = if undoing {
            &mut self.redo
        } else {
            &mut self.undo
        };
        dst.entry(dom).or_default().push_back(cur);
        Ok(json!({"changed": true}))
    }

    /// run a JSON list of commands in order. atomic (default): on the first
    /// failure every domain state touched so far is rolled back and the
    /// error is reported with the failing index. `"atomic": false` runs the
    /// whole list and returns each sub-response.
    fn run_batch(&mut self, v: &Value) -> Result<Value> {
        let list = v
            .get("commands")
            .and_then(Value::as_array)
            .context("batch needs 'commands': [...]")?;
        let atomic = v.get("atomic").and_then(Value::as_bool).unwrap_or(true);
        // pre-state for rollback + per-domain undo stack sizes so a rolled
        // back batch leaves no half-committed history entries
        let domains = ["doc", "tl", "pg"];
        let snaps: Vec<Value> = domains.iter().map(|d| self.domain_snapshot(d)).collect();
        let undo_lens: Vec<usize> = domains
            .iter()
            .map(|d| self.undo.get(*d).map(|s| s.len()).unwrap_or(0))
            .collect();
        // successful sub-commands clear their domain's redo stack — keep a
        // copy so a failed atomic batch restores redo too, not just state
        let redo_snaps: Vec<Vec<Value>> = domains
            .iter()
            .map(|d| {
                self.redo
                    .get(*d)
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default()
            })
            .collect();
        let mut results = Vec::with_capacity(list.len());
        for (i, sub) in list.iter().enumerate() {
            let r = self.dispatch_one(sub, true);
            let ok = r.get("ok") == Some(&Value::Bool(true));
            results.push(r.clone());
            if !ok {
                if atomic {
                    for (i2, (d, snap)) in domains.iter().zip(snaps.iter()).enumerate() {
                        self.domain_restore(d, snap.clone())?;
                        if let Some(s) = self.undo.get_mut(*d) {
                            s.truncate(undo_lens[i2]);
                        }
                        let rs = self.redo.entry(*d).or_default();
                        rs.clear();
                        for v in redo_snaps[i2].iter().cloned() {
                            rs.push_back(v);
                        }
                    }
                    let err = r
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("error")
                        .to_string();
                    anyhow::bail!("batch command {i} failed: {err}");
                }
            }
        }
        Ok(json!({"results": results}))
    }

    pub fn dispatch(&mut self, v: &Value) -> Value {
        let id = v.get("id").and_then(Value::as_str).unwrap_or("");
        // history + batch are session-level, not domain commands
        if matches!(
            id,
            "doc.undo" | "doc.redo" | "tl.undo" | "tl.redo" | "pg.undo" | "pg.redo"
        ) {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.history(id)));
            return match r {
                Ok(Ok(result)) => json!({"ok": true, "result": result}),
                Ok(Err(e)) => json!({"ok": false, "error": format!("{e:#}")}),
                Err(_) => json!({"ok": false, "error": format!("command '{id}' panicked")}),
            };
        }
        if id == "batch" {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run_batch(v)));
            return match r {
                Ok(Ok(result)) => json!({"ok": true, "result": result}),
                Ok(Err(e)) => json!({"ok": false, "error": format!("{e:#}")}),
                Err(_) => json!({"ok": false, "error": "batch panicked"}),
            };
        }
        self.dispatch_one(v, true)
    }

    fn dispatch_one(&mut self, v: &Value, record: bool) -> Value {
        let id = v.get("id").and_then(Value::as_str).unwrap_or("");
        // capture pre-state BEFORE running; commit it to the undo stack
        // only when the command actually succeeded.
        let dom = if record {
            history_domain(id)
                .filter(|_| is_mutating(id))
                .map(|d| (d, self.domain_snapshot(d)))
        } else {
            None
        };
        let r = self.dispatch_routed(id, v);
        if let (Some((d, snap)), true) = (dom, r.get("ok") == Some(&Value::Bool(true))) {
            let s = self.undo.entry(d).or_default();
            s.push_back(snap);
            while s.len() > HISTORY_CAP {
                s.pop_front();
            }
            self.redo.entry(d).or_default().clear();
        }
        r
    }

    fn dispatch_routed(&mut self, id: &str, v: &Value) -> Value {
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

    /// every command this session understands, as MCP-shaped tool specs
    /// ({id, name, description, inputSchema}). `name` is the id with '.'
    /// replaced by '_' — MCP tool names cannot contain dots.
    pub fn command_specs() -> Vec<Value> {
        let mut out = crate::specs::base();
        out.extend(koubou_motion::command_specs());
        out.extend(koubou_pages::command_specs());
        out
    }

    /// the command ids this dispatcher understands (parity checks read this)
    /// every command id this session understands. The doc/misc ids are
    /// owned here; the tl.*/pg.* ids come from the domain sessions so a
    /// new motion/pages command is reachable (and MCP-visible) without
    /// touching this list. tl/pg undo/redo stay here — the unified undo
    /// router dispatches on them itself.
    pub fn command_ids() -> Vec<&'static str> {
        static BASE: &[&str] = &[
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
            "batch",
            "doc.new",
            "doc.fromPhoto",
            "doc.open",
            "doc.save",
            "doc.json",
            "doc.info",
            "doc.importPsd",
            "doc.addLayer",
            "doc.setLayer",
            "doc.removeLayer",
            "doc.duplicateLayer",
            "doc.reorder",
            "doc.moveLayer",
            "doc.mergeDown",
            "doc.flatten",
            "doc.resize",
            "doc.crop",
            "doc.setBackdrop",
            "doc.render",
            "doc.exportLayer",
            "doc.exportPsd",
            "doc.pick",
            "doc.bounds",
            "doc.maskPaint",
            "doc.maskRect",
            "doc.maskInvert",
            "doc.styleSet",
            "doc.styleClear",
            "doc.styleScale",
            "doc.group",
            "doc.ungroup",
            "doc.addShape",
            "doc.shapeSet",
            "doc.shapeRemove",
            "doc.shapeNodes",
            "doc.moveNode",
            "doc.undo",
            "doc.redo",
            // cross-domain undo routing is owned by this session
            "tl.undo",
            "tl.redo",
            "pg.undo",
            "pg.redo",
        ];
        BASE.iter()
            .copied()
            .chain(koubou_motion::TlSession::command_ids())
            .chain(koubou_pages::PgSession::command_ids())
            .collect()
    }

    fn run(&mut self, id: &str, v: &Value) -> Result<Value> {
        match id {
            "ping" => Ok(json!({"name": "koubou", "version": env!("CARGO_PKG_VERSION")})),
            "commands" => Ok(json!({
                "ids": Self::command_ids(),
                "tools": Self::command_specs(),
            })),
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
            "doc.styleSet" => {
                let c = self.composer.as_mut().context("no document")?;
                let id = layer_id(v)?;
                let effect = req_str(v, "effect")?;
                let params = v.get("params").cloned().unwrap_or(json!({}));
                let l = c
                    .doc
                    .layer_mut(id)
                    .with_context(|| format!("layer {id} not found"))?;
                l.styles
                    .set_effect(&effect, &params)
                    .with_context(|| format!("effect '{effect}'"))?;
                l.gen += 1;
                Ok(json!("ok"))
            }
            "doc.styleClear" => {
                let c = self.composer.as_mut().context("no document")?;
                let id = layer_id(v)?;
                let effect = v.get("effect").and_then(Value::as_str);
                let l = c
                    .doc
                    .layer_mut(id)
                    .with_context(|| format!("layer {id} not found"))?;
                l.styles.clear_effect(effect)?;
                l.gen += 1;
                Ok(json!("ok"))
            }
            "doc.styleScale" => {
                let c = self.composer.as_mut().context("no document")?;
                let id = layer_id(v)?;
                let s = v
                    .get("scale")
                    .and_then(Value::as_f64)
                    .context("'scale' (multiplier) required")?;
                let l = c
                    .doc
                    .layer_mut(id)
                    .with_context(|| format!("layer {id} not found"))?;
                l.styles.scale(s as f32);
                l.gen += 1;
                Ok(json!("ok"))
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
            "doc.moveLayer" => {
                let c = self.composer.as_mut().context("no document")?;
                let id = layer_id(v)?;
                let parent = v.get("parent").and_then(Value::as_u64);
                let to = v.get("to").and_then(Value::as_u64).unwrap_or(0) as usize;
                if !c.doc.move_layer(id, parent, to) {
                    anyhow::bail!(
                        "cannot move layer {id} (missing, or target is not a group, or cycle)"
                    );
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
                write_image_fmt(
                    &img,
                    v.get("out").and_then(Value::as_str),
                    v.get("format").and_then(Value::as_str).unwrap_or("png"),
                    v.get("quality").and_then(Value::as_f64).unwrap_or(90.0) as u8,
                )
            }
            "doc.exportPsd" => {
                let c = self.composer.as_mut().context("no document")?;
                let p = req_str(v, "path")?;
                let img = c.render()?;
                crate::psd::write_flat_psd(Path::new(&p), img.width, img.height, &img.data)?;
                Ok(json!({"path": p}))
            }
            "doc.pick" => {
                let c = self.composer.as_mut().context("no document")?;
                let x = v.get("x").and_then(Value::as_f64).unwrap_or(0.0) as f32;
                let y = v.get("y").and_then(Value::as_f64).unwrap_or(0.0) as f32;
                match c.pick(x, y)? {
                    Some(id) => Ok(json!({"layer": id})),
                    None => Ok(json!({"layer": Value::Null})),
                }
            }
            "doc.bounds" => {
                let c = self.composer.as_mut().context("no document")?;
                match v.get("layer").and_then(Value::as_u64) {
                    Some(id) => {
                        let b = c.layer_bounds(id)?;
                        Ok(json!({"layer": id, "bounds": b}))
                    }
                    None => {
                        let all = c.all_bounds();
                        Ok(json!(all
                            .iter()
                            .map(|(id, b)| json!({"layer": id, "bounds": b}))
                            .collect::<Vec<_>>()))
                    }
                }
            }
            "doc.maskRect" => {
                let c = self.composer.as_mut().context("no document")?;
                let id = layer_id(v)?;
                let x = v.get("x").and_then(Value::as_f64).unwrap_or(0.0) as f32;
                let y = v.get("y").and_then(Value::as_f64).unwrap_or(0.0) as f32;
                let w = req_u64(v, "w")? as u32;
                let h = req_u64(v, "h")? as u32;
                let feather = v.get("feather").and_then(Value::as_f64).unwrap_or(0.0) as f32;
                let (dw, dh) = (c.doc.width, c.doc.height);
                let l = c.doc.layer_mut(id).context("layer not found")?;
                // rect mask: 1 inside the rect, 0 outside — replaces the mask
                let mut m = Mask {
                    width: dw,
                    height: dh,
                    data: vec![0.0; (dw * dh) as usize],
                    inverted: false,
                    density: 1.0,
                    feather,
                };
                let (x0, y0) = (x.max(0.0) as u32, y.max(0.0) as u32);
                let x1 = ((x + w as f32).ceil() as u32).min(dw);
                let y1 = ((y + h as f32).ceil() as u32).min(dh);
                for py in y0.min(dh)..y1 {
                    for px in x0.min(dw)..x1 {
                        m.data[(py * dw + px) as usize] = 1.0;
                    }
                }
                l.mask = Some(m);
                l.gen += 1;
                Ok(json!("ok"))
            }
            "doc.group" => {
                let c = self.composer.as_mut().context("no document")?;
                let name = v.get("name").and_then(Value::as_str).unwrap_or("Group");
                // wrap the given layers (or all when absent) preserving order
                let ids: Vec<u64> = v
                    .get("layers")
                    .and_then(|a| serde_json::from_value::<Vec<u64>>(a.clone()).ok())
                    .unwrap_or_else(|| c.doc.layers.iter().map(|l| l.id).collect());
                if ids.is_empty() {
                    anyhow::bail!("no layers to group");
                }
                // top-level members determine where the group lands:
                // above the highest one. Nested members keep coming along
                // but don't occupy top-level slots.
                let top_ids: std::collections::HashSet<u64> =
                    c.doc.layers.iter().map(|l| l.id).collect();
                let pos = ids
                    .iter()
                    .filter(|id| top_ids.contains(*id))
                    .filter_map(|id| c.doc.index_of(*id))
                    .max();
                let mut members = Vec::with_capacity(ids.len());
                for id in &ids {
                    if let Some(l) = c.doc.remove_layer(*id) {
                        members.push(l);
                    }
                }
                if members.is_empty() {
                    anyhow::bail!("no layers to group");
                }
                let top_n = members.iter().filter(|l| top_ids.contains(&l.id)).count();
                let mut g = Layer::group(name, members);
                g.id = c.doc.next_id;
                c.doc.next_id += 1;
                let gid = g.id;
                let at = match pos {
                    Some(p) => (p + 1).saturating_sub(top_n).min(c.doc.layers.len()),
                    None => c.doc.layers.len(),
                };
                c.doc.layers.insert(at, g);
                Ok(json!({"layerId": gid}))
            }
            "doc.ungroup" => {
                let c = self.composer.as_mut().context("no document")?;
                let id = layer_id(v)?;
                let i = c.doc.index_of(id).context("layer not found")?;
                let l = c.doc.layers.remove(i);
                let children = match l.kind {
                    LayerKind::Group { children } => children,
                    _ => anyhow::bail!("layer {id} is not a group"),
                };
                for (k, mut child) in children.into_iter().enumerate() {
                    child.x += l.x;
                    child.y += l.y;
                    // fresh ids so the compositor cache can't collide
                    child.id = c.doc.next_id;
                    c.doc.next_id += 1;
                    child.gen += 1;
                    c.doc.layers.insert(i + k, child);
                }
                Ok(json!("ok"))
            }
            "doc.maskPaint" => {
                let c = self.composer.as_mut().context("no document")?;
                mask_paint(&mut c.doc, v)
            }
            "doc.info" => {
                let c = self.composer.as_ref().context("no document")?;
                Ok(json!({
                    "name": c.doc.name,
                    "w": c.doc.width,
                    "h": c.doc.height,
                    "layers": c.doc.layers.len(),
                    "backdrop": c.doc.backdrop,
                }))
            }
            "doc.duplicateLayer" => {
                let c = self.composer.as_mut().context("no document")?;
                let id = layer_id(v)?;
                let i = c.doc.index_of(id).context("layer not found")?;
                let mut l = c.doc.layers[i].clone();
                l.name = format!("{} copy", l.name);
                l.x += 16;
                l.y += 16;
                let new_id = c.doc.add_layer_at(l, i + 1);
                Ok(json!({"layerId": new_id}))
            }
            "doc.mergeDown" => {
                let c = self.composer.as_mut().context("no document")?;
                let id = layer_id(v)?;
                let new_id = c.merge_down(id)?;
                Ok(json!({"layerId": new_id}))
            }
            "doc.flatten" => {
                let c = self.composer.as_mut().context("no document")?;
                let name = v.get("name").and_then(Value::as_str).unwrap_or("Flattened");
                let id = c.flatten(name)?;
                Ok(json!({"layerId": id}))
            }
            "doc.resize" => {
                let c = self.composer.as_mut().context("no document")?;
                let w = req_u64(v, "w")? as u32;
                let h = req_u64(v, "h")? as u32;
                if w == 0 || h == 0 {
                    anyhow::bail!("doc.resize needs positive w/h");
                }
                c.doc.width = w;
                c.doc.height = h;
                c.doc.bump_all_gens();
                Ok(json!({"w": w, "h": h}))
            }
            "doc.crop" => {
                let c = self.composer.as_mut().context("no document")?;
                let x = v.get("x").and_then(Value::as_i64).unwrap_or(0) as i32;
                let y = v.get("y").and_then(Value::as_i64).unwrap_or(0) as i32;
                let w = req_u64(v, "w")? as u32;
                let h = req_u64(v, "h")? as u32;
                for l in c.doc.layers.iter_mut() {
                    l.x -= x;
                    l.y -= y;
                }
                c.doc.width = w;
                c.doc.height = h;
                c.doc.bump_all_gens();
                Ok(json!({"w": w, "h": h}))
            }
            "doc.setBackdrop" => {
                let c = self.composer.as_mut().context("no document")?;
                let col = v
                    .get("color")
                    .and_then(|c| serde_json::from_value::<[f32; 4]>(c.clone()).ok())
                    .context("need 'color': [r,g,b,a]")?;
                c.doc.backdrop = col;
                Ok(json!("ok"))
            }
            "doc.exportLayer" => {
                let c = self.composer.as_mut().context("no document")?;
                let id = layer_id(v)?;
                let img = c.render_layer(id)?;
                write_image(&img, v.get("out").and_then(Value::as_str))
            }
            "doc.maskInvert" => {
                let c = self.composer.as_mut().context("no document")?;
                let id = layer_id(v)?;
                let (dw, dh) = (c.doc.width, c.doc.height);
                let l = c.doc.layer_mut(id).context("layer not found")?;
                match &mut l.mask {
                    Some(m) => m.inverted = !m.inverted,
                    None => {
                        // inverting an absent mask = fully hidden
                        let mut m = Mask::full(dw, dh);
                        m.inverted = true;
                        l.mask = Some(m);
                    }
                }
                l.gen += 1;
                Ok(json!("ok"))
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
            "doc.shapeNodes" => {
                let c = self.composer.as_ref().context("no document")?;
                let id = layer_id(v)?;
                let l = c.doc.layer(id).context("layer not found")?;
                let shapes = match &l.kind {
                    LayerKind::Shape { shapes } => shapes,
                    _ => anyhow::bail!("layer {id} is not a shape layer"),
                };
                let mut nodes = Vec::new();
                for (si, s) in shapes.iter().enumerate() {
                    for (x, y, k) in crate::shape::path_nodes(&s.d).unwrap_or_default() {
                        nodes.push(json!({"x": x, "y": y, "kind": k.to_string(), "shape": si}));
                    }
                }
                Ok(json!({"nodes": nodes}))
            }
            "doc.moveNode" => {
                let c = self.composer.as_mut().context("no document")?;
                let id = layer_id(v)?;
                let ix = v.get("index").and_then(Value::as_u64).context("index")? as usize;
                let x = v.get("x").and_then(Value::as_f64).context("x")? as f32;
                let y = v.get("y").and_then(Value::as_f64).context("y")? as f32;
                let l = c.doc.layer_mut(id).context("layer not found")?;
                let shapes = match &mut l.kind {
                    LayerKind::Shape { shapes } => shapes,
                    _ => anyhow::bail!("layer {id} is not a shape layer"),
                };
                // flat index → (shape, anchor): node order is per-shape, in path order
                let mut rem = ix;
                let mut done = None;
                for (si, s) in shapes.iter_mut().enumerate() {
                    let cnt = crate::shape::path_nodes(&s.d).map(|n| n.len()).unwrap_or(0);
                    if rem < cnt {
                        s.d =
                            crate::shape::move_node(&s.d, rem, x, y).context("move_node failed")?;
                        done = Some(si);
                        break;
                    }
                    rem -= cnt;
                }
                let si = done.with_context(|| format!("node index {ix} out of range"))?;
                l.gen += 1;
                Ok(json!({"shape": si}))
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
    write_image_fmt(img, out, "png", 90)
}

/// encode a rendered image. format: "png" (default, alpha-capable, also the
/// b64 return format), "jpeg"/"jpg" (file only; alpha composited over the
/// backdrop colour — JPEG has no alpha), "tiff" (file only, rgba8).
fn write_image_fmt(
    img: &koubou_core::develop::RgbaImage,
    out: Option<&str>,
    format: &str,
    quality: u8,
) -> Result<Value> {
    use image::ImageEncoder as _;
    let fmt = format.to_ascii_lowercase();
    let encode = |w: &mut dyn std::io::Write| -> Result<()> {
        match fmt.as_str() {
            "jpeg" | "jpg" => {
                // flatten onto opaque — JPEG stores no alpha; the pixels are
                // already composited over the document backdrop
                let rgb: Vec<u8> = img
                    .data
                    .chunks_exact(4)
                    .flat_map(|p| [p[0], p[1], p[2]])
                    .collect();
                image::codecs::jpeg::JpegEncoder::new_with_quality(w, quality)
                    .write_image(&rgb, img.width, img.height, image::ExtendedColorType::Rgb8)
                    .map_err(Into::into)
            }
            "tiff" | "tif" => {
                // TiffEncoder wants Write+Seek — buffer through a Cursor
                let mut buf = std::io::Cursor::new(Vec::new());
                image::codecs::tiff::TiffEncoder::new(&mut buf).write_image(
                    &img.data,
                    img.width,
                    img.height,
                    image::ExtendedColorType::Rgba8,
                )?;
                w.write_all(&buf.into_inner()).map_err(Into::into)
            }
            _ => image::codecs::png::PngEncoder::new(w)
                .write_image(
                    &img.data,
                    img.width,
                    img.height,
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(Into::into),
        }
    };
    match out {
        Some(path) => {
            let mut f = std::io::BufWriter::new(
                std::fs::File::create(path).with_context(|| format!("create {path}"))?,
            );
            encode(&mut f).with_context(|| format!("save {path}"))?;
            Ok(json!({"path": path, "w": img.width, "h": img.height}))
        }
        None => {
            use base64::Engine as _;
            let mut buf = std::io::Cursor::new(Vec::new());
            encode(&mut buf)?;
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
    if let Some(s) = v.get("styles") {
        l.styles = serde_json::from_value(s.clone()).context("styles")?;
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
    if v.get("styles").is_some_and(|s| s.is_null()) {
        l.styles = Default::default();
        l.gen += 1;
    } else if let Some(s) = v.get("styles") {
        l.styles = serde_json::from_value(s.clone()).context("styles")?;
        l.gen += 1;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn s() -> Session {
        Session::new().expect("engine")
    }

    fn d(v: &Value) -> Value {
        v.clone()
    }

    fn new_doc(s: &mut Session) {
        let r = s.dispatch(&d(&json!({"id": "doc.new", "name": "t", "w": 64, "h": 48})));
        assert_eq!(r["ok"], true, "doc.new failed: {r}");
    }

    #[test]
    fn undo_redo_roundtrip() {
        let mut s = s();
        new_doc(&mut s);
        let r = s.dispatch(&d(
            &json!({"id": "doc.addLayer", "kind": "fill", "name": "a", "color": [1,0,0,1]}),
        ));
        assert_eq!(r["ok"], true);
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert_eq!(doc["result"]["layers"].as_array().unwrap().len(), 1);

        let u = s.dispatch(&d(&json!({"id": "doc.undo"})));
        assert_eq!(u["result"]["changed"], true);
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert_eq!(doc["result"]["layers"].as_array().unwrap().len(), 0);

        let r = s.dispatch(&d(&json!({"id": "doc.redo"})));
        assert_eq!(r["result"]["changed"], true);
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert_eq!(doc["result"]["layers"].as_array().unwrap().len(), 1);
        assert_eq!(doc["result"]["layers"][0]["name"], "a");

        // doc.new is itself undoable — undoing it restores "no document"
        let u = s.dispatch(&d(&json!({"id": "doc.undo"})));
        assert_eq!(u["result"]["changed"], true);
        let u = s.dispatch(&d(&json!({"id": "doc.undo"})));
        assert_eq!(u["result"]["changed"], true);
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert_eq!(doc["ok"], false, "undo should have removed the document");
        // stack drained
        let u = s.dispatch(&d(&json!({"id": "doc.undo"})));
        assert_eq!(u["result"]["changed"], false);
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut s = s();
        new_doc(&mut s);
        s.dispatch(&d(
            &json!({"id": "doc.addLayer", "kind": "fill", "name": "a", "color": [1,0,0,1]}),
        ));
        s.dispatch(&d(&json!({"id": "doc.undo"})));
        s.dispatch(&d(
            &json!({"id": "doc.addLayer", "kind": "fill", "name": "b", "color": [0,1,0,1]}),
        ));
        let r = s.dispatch(&d(&json!({"id": "doc.redo"})));
        assert_eq!(
            r["result"]["changed"], false,
            "redo must be cleared by a new edit"
        );
    }

    #[test]
    fn batch_atomic_rolls_back() {
        let mut s = s();
        new_doc(&mut s);
        let r = s.dispatch(&d(&json!({
            "id": "batch",
            "commands": [
                {"id": "doc.addLayer", "kind": "fill", "name": "a", "color": [1,0,0,1]},
                {"id": "doc.nonsense"},
                {"id": "doc.addLayer", "kind": "fill", "name": "b", "color": [0,1,0,1]}
            ]
        })));
        assert_eq!(r["ok"], false);
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert_eq!(
            doc["result"]["layers"].as_array().unwrap().len(),
            0,
            "failed atomic batch must leave no edits"
        );
        // and no history entries either
        let u = s.dispatch(&d(&json!({"id": "doc.undo"})));
        assert_eq!(u["result"]["changed"], true, "doc.new itself is undoable");
        s.dispatch(&d(&json!({"id": "doc.redo"})));
    }

    #[test]
    fn batch_non_atomic_collects_results() {
        let mut s = s();
        new_doc(&mut s);
        let r = s.dispatch(&d(&json!({
            "id": "batch", "atomic": false,
            "commands": [
                {"id": "doc.addLayer", "kind": "fill", "name": "a", "color": [1,0,0,1]},
                {"id": "doc.nonsense"},
                {"id": "doc.addLayer", "kind": "fill", "name": "b", "color": [0,1,0,1]}
            ]
        })));
        assert_eq!(r["ok"], true);
        let res = r["result"]["results"].as_array().unwrap();
        assert_eq!(res[0]["ok"], true);
        assert_eq!(res[1]["ok"], false);
        assert_eq!(res[2]["ok"], true);
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert_eq!(doc["result"]["layers"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn duplicate_merge_flatten_export() {
        let mut s = s();
        new_doc(&mut s);
        s.dispatch(&d(
            &json!({"id": "doc.addLayer", "kind": "fill", "name": "bottom", "color": [1,0,0,1]}),
        ));
        s.dispatch(&d(
            &json!({"id": "doc.addLayer", "kind": "fill", "name": "top", "color": [0,0,1,0.5]}),
        ));
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        let top_id = doc["result"]["layers"][1]["id"].as_u64().unwrap();

        let r = s.dispatch(&d(&json!({"id": "doc.duplicateLayer", "layer": top_id})));
        assert_eq!(r["ok"], true);
        assert_eq!(
            s.dispatch(&d(&json!({"id": "doc.json"})))["result"]["layers"]
                .as_array()
                .unwrap()
                .len(),
            3
        );

        let r = s.dispatch(&d(&json!({"id": "doc.mergeDown", "layer": top_id})));
        assert_eq!(r["ok"], true, "mergeDown: {r}");
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        let layers = doc["result"]["layers"].as_array().unwrap();
        assert_eq!(layers.len(), 2);
        // merged raster takes the bottom slot, keeps the lower layer's name
        assert_eq!(layers[0]["type"], "raster");
        assert_eq!(layers[0]["name"], "bottom");
        let merged_id = layers[0]["id"].as_u64().unwrap();

        let r = s.dispatch(&d(
            &json!({"id": "doc.exportLayer", "layer": merged_id, "out": "/tmp/kb_export_test.png"}),
        ));
        assert_eq!(r["ok"], true, "exportLayer: {r}");
        assert!(std::path::Path::new("/tmp/kb_export_test.png").exists());

        let r = s.dispatch(&d(&json!({"id": "doc.flatten", "name": "flat"})));
        assert_eq!(r["ok"], true);
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        let layers = doc["result"]["layers"].as_array().unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0]["name"], "flat");
    }

    #[test]
    fn resize_crop_maskinvert() {
        let mut s = s();
        new_doc(&mut s);
        s.dispatch(&d(&json!({"id": "doc.addLayer", "kind": "fill", "name": "bg", "color": [1,1,1,1], "x": 10, "y": 5})));

        let r = s.dispatch(&d(&json!({"id": "doc.resize", "w": 100, "h": 80})));
        assert_eq!(r["ok"], true);
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert_eq!(doc["result"]["width"], 100);

        let r = s.dispatch(&d(
            &json!({"id": "doc.crop", "x": 10, "y": 5, "w": 32, "h": 24}),
        ));
        assert_eq!(r["ok"], true, "crop: {r}");
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert_eq!(doc["result"]["width"], 32);
        assert_eq!(doc["result"]["height"], 24);
        assert_eq!(doc["result"]["layers"][0]["x"], 0);
        assert_eq!(doc["result"]["layers"][0]["y"], 0);

        let lid = doc["result"]["layers"][0]["id"].as_u64().unwrap();
        let r = s.dispatch(&d(&json!({"id": "doc.maskInvert", "layer": lid})));
        assert_eq!(r["ok"], true);
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert_eq!(doc["result"]["layers"][0]["mask"]["inverted"], true);
        let r = s.dispatch(&d(&json!({"id": "doc.maskInvert", "layer": lid})));
        assert_eq!(doc["result"]["layers"][0]["mask"]["inverted"], true);
        assert_eq!(r["ok"], true);
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert_eq!(doc["result"]["layers"][0]["mask"]["inverted"], false);
    }

    #[test]
    fn specs_cover_every_command() {
        // the MCP tool list is generated from command_specs — it must cover
        // exactly the dispatchable command set, or a tool silently 404s.
        let ids = Session::command_ids();
        let specs = Session::command_specs();
        let spec_ids: Vec<&str> = specs
            .iter()
            .filter_map(|s| s.get("id").and_then(Value::as_str))
            .collect();
        for id in &ids {
            assert!(spec_ids.contains(id), "command '{id}' has no spec");
        }
        for sid in &spec_ids {
            assert!(ids.contains(sid), "spec '{sid}' has no command");
        }
        // MCP names must be unique and dot-free
        let names: Vec<&str> = specs
            .iter()
            .filter_map(|s| s.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(names.len(), spec_ids.len());
        for (i, a) in names.iter().enumerate() {
            assert!(!a.contains('.'));
            for b in &names[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn tl_undo_isolated_from_doc() {
        let mut s = s();
        new_doc(&mut s);
        let r = s.dispatch(&d(&json!({"id": "tl.new", "w": 640, "h": 360, "fps": 30})));
        assert_eq!(r["ok"], true, "tl.new: {r}");
        let r = s.dispatch(&d(
            &json!({"id": "tl.addTrack", "kind": "video", "name": "v1"}),
        ));
        assert_eq!(r["ok"], true);
        let u = s.dispatch(&d(&json!({"id": "tl.undo"})));
        assert_eq!(u["result"]["changed"], true);
        let t = s.dispatch(&d(&json!({"id": "tl.json"})));
        assert_eq!(t["result"]["tracks"].as_array().unwrap().len(), 0);
        // doc untouched by tl.undo
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert!(doc["ok"] == true);
    }

    #[test]
    fn tl_split_duplicate_track_cue_ops() {
        let mut s = s();
        s.dispatch(&d(&json!({"id": "tl.new", "w": 320, "h": 180, "fps": 30})));
        s.dispatch(&d(
            &json!({"id": "tl.addTrack", "kind": "video", "name": "v1"}),
        ));
        let r = s.dispatch(&d(&json!({
            "id": "tl.addClip", "track": 0, "text": "hello", "dur": 4.0, "offset": 0.0
        })));
        assert_eq!(r["ok"], true, "addClip: {r}");
        let clip = r["result"]["clipId"].as_u64().unwrap();

        let r = s.dispatch(&d(&json!({"id": "tl.splitClip", "clip": clip, "t": 1.5})));
        assert_eq!(r["ok"], true, "splitClip: {r}");
        let t = s.dispatch(&d(&json!({"id": "tl.json"})));
        let clips = &t["result"]["tracks"][0]["clips"];
        assert_eq!(clips.as_array().unwrap().len(), 2);
        assert_eq!(clips[0]["outPoint"], json!(1.5));
        assert_eq!(clips[1]["offset"], json!(1.5));

        let dup_clip = clips[1]["id"].as_u64().unwrap();
        let r = s.dispatch(&d(&json!({"id": "tl.duplicateClip", "clip": dup_clip})));
        assert_eq!(r["ok"], true, "duplicateClip: {r}");
        let t = s.dispatch(&d(&json!({"id": "tl.json"})));
        assert_eq!(
            t["result"]["tracks"][0]["clips"].as_array().unwrap().len(),
            3
        );

        let r = s.dispatch(&d(
            &json!({"id": "tl.setTrack", "track": 0, "name": "renamed", "muted": true}),
        ));
        assert_eq!(r["ok"], true);
        let t = s.dispatch(&d(&json!({"id": "tl.json"})));
        assert_eq!(t["result"]["tracks"][0]["name"], "renamed");
        assert_eq!(t["result"]["tracks"][0]["muted"], true);

        s.dispatch(&d(&json!({"id": "tl.addTrack", "kind": "subtitle"})));
        s.dispatch(&d(
            &json!({"id": "tl.addCue", "t": 0.0, "dur": 1.0, "text": "hi"}),
        ));
        let r = s.dispatch(&d(
            &json!({"id": "tl.setCue", "index": 0, "text": "bye", "t": 2.0}),
        ));
        assert_eq!(r["ok"], true, "setCue: {r}");
        let t = s.dispatch(&d(&json!({"id": "tl.json"})));
        assert_eq!(t["result"]["tracks"][1]["cues"][0]["text"], "bye");
        let r = s.dispatch(&d(&json!({"id": "tl.removeCue", "index": 0})));
        assert_eq!(r["ok"], true);

        // splitting outside a clip errors, not panics
        let r = s.dispatch(&d(&json!({"id": "tl.splitClip", "clip": clip, "t": 99.0})));
        assert_eq!(r["ok"], false);
        let r = s.dispatch(&d(&json!({"id": "tl.removeTrack", "track": 0})));
        assert_eq!(r["ok"], true);
        let t = s.dispatch(&d(&json!({"id": "tl.json"})));
        assert_eq!(t["result"]["tracks"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn pg_duplicate_move_undo() {
        let mut s = s();
        s.dispatch(&d(&json!({"id": "pg.new", "name": "p"})));
        s.dispatch(&d(&json!({"id": "pg.addPage"})));
        let r = s.dispatch(&d(&json!({
            "id": "pg.addFrame", "page": 0, "kind": "rect", "x": 10.0, "y": 10.0, "w": 50.0, "h": 20.0
        })));
        assert_eq!(r["ok"], true, "addFrame: {r}");
        let fid = r["result"]["frame"].as_u64().unwrap();

        let r = s.dispatch(&d(&json!({"id": "pg.duplicatePage", "page": 0})));
        assert_eq!(r["ok"], true);
        let p = s.dispatch(&d(&json!({"id": "pg.json"})));
        assert_eq!(p["result"]["pages"].as_array().unwrap().len(), 2);
        // duplicated page's frame has a different id
        let dup_fid = p["result"]["pages"][1]["frames"][0]["id"].as_u64().unwrap();
        assert_ne!(fid, dup_fid);

        let r = s.dispatch(&d(&json!({"id": "pg.moveFrame", "frame": fid, "page": 1})));
        assert_eq!(r["ok"], true, "moveFrame: {r}");
        let p = s.dispatch(&d(&json!({"id": "pg.json"})));
        assert_eq!(
            p["result"]["pages"][0]["frames"].as_array().unwrap().len(),
            0
        );
        assert_eq!(
            p["result"]["pages"][1]["frames"].as_array().unwrap().len(),
            2
        );

        let u = s.dispatch(&d(&json!({"id": "pg.undo"})));
        assert_eq!(u["result"]["changed"], true);
        let p = s.dispatch(&d(&json!({"id": "pg.json"})));
        assert_eq!(
            p["result"]["pages"][0]["frames"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn shape_nodes_and_move() {
        let mut s = s();
        s.dispatch(&d(
            &json!({"id": "doc.new", "name": "t", "w": 100, "h": 100}),
        ));
        let r = s.dispatch(&d(&json!({"id": "doc.addLayer", "kind": "shape",
            "shapes": [{"d": "M10 10 L50 10 L50 40 L10 40 Z",
                        "fill": [1.0, 0.0, 0.0, 1.0]}]})));
        assert_eq!(r["ok"], true, "addLayer: {r}");
        let lid = r["result"]["layerId"].as_u64().unwrap();

        let r = s.dispatch(&d(&json!({"id": "doc.shapeNodes", "layer": lid})));
        assert_eq!(r["ok"], true, "shapeNodes: {r}");
        let nodes = r["result"]["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 4);
        assert_eq!(nodes[1]["x"], json!(50.0));

        let r = s.dispatch(&d(&json!({"id": "doc.moveNode", "layer": lid, "index": 1,
                    "x": 60.0, "y": 15.0})));
        assert_eq!(r["ok"], true, "moveNode: {r}");
        let r = s.dispatch(&d(&json!({"id": "doc.shapeNodes", "layer": lid})));
        assert_eq!(r["result"]["nodes"][1]["x"], json!(60.0));
        assert_eq!(r["result"]["nodes"][1]["y"], json!(15.0));

        // hostile: bad index / non-shape layer error without panic
        let r = s.dispatch(&d(&json!({"id": "doc.moveNode", "layer": lid, "index": 99,
                    "x": 0.0, "y": 0.0})));
        assert_eq!(r["ok"], false);
        let r = s.dispatch(&d(&json!({"id": "doc.shapeNodes", "layer": 9999})));
        assert_eq!(r["ok"], false);
    }

    #[test]
    fn pick_bounds_maskrect() {
        let mut s = s();
        s.dispatch(&d(
            &json!({"id": "doc.new", "name": "t", "w": 100, "h": 100}),
        ));
        // opaque red raster 20×20 at (10, 10)
        let px: Vec<u8> = (0..20 * 20 * 4)
            .map(|i| if i % 4 == 3 { 255 } else { 200 })
            .collect();
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&px);
        let r = s.dispatch(&d(&json!({
            "id": "doc.addLayer", "kind": "raster", "name": "sq",
            "w": 20, "h": 20, "rgbaB64": b64, "x": 10, "y": 10,
        })));
        assert_eq!(r["ok"], true, "addLayer: {r}");
        let lid = r["result"]["layerId"].as_u64().unwrap();

        let r = s.dispatch(&d(&json!({"id": "doc.pick", "x": 15, "y": 15})));
        assert_eq!(r["result"]["layer"], json!(lid));
        let r = s.dispatch(&d(&json!({"id": "doc.pick", "x": 50, "y": 50})));
        assert_eq!(r["result"]["layer"], Value::Null);

        let r = s.dispatch(&d(&json!({"id": "doc.bounds", "layer": lid})));
        assert_eq!(r["result"]["bounds"], json!([10.0, 10.0, 20.0, 20.0]));
        let r = s.dispatch(&d(&json!({"id": "doc.bounds"})));
        assert_eq!(r["result"].as_array().unwrap().len(), 1);

        // rect mask cutting the layer in half (mask coords are layer-pixel
        // space like maskPaint): layer covers x 0..10 → doc 10..20 picks,
        // doc 20..30 does not
        let r = s.dispatch(&d(&json!({
            "id": "doc.maskRect", "layer": lid, "x": 0, "y": 0, "w": 10, "h": 20
        })));
        assert_eq!(r["ok"], true, "maskRect: {r}");
        let r = s.dispatch(&d(&json!({"id": "doc.pick", "x": 15, "y": 15})));
        assert_eq!(r["result"]["layer"], json!(lid));
        let r = s.dispatch(&d(&json!({"id": "doc.pick", "x": 25, "y": 15})));
        assert_eq!(r["result"]["layer"], Value::Null);
    }

    #[test]
    fn group_ungroup() {
        let mut s = s();
        new_doc(&mut s);
        s.dispatch(&d(
            &json!({"id": "doc.addLayer", "kind": "fill", "name": "a", "color": [1,0,0,1]}),
        ));
        s.dispatch(&d(
            &json!({"id": "doc.addLayer", "kind": "fill", "name": "b", "color": [0,1,0,1]}),
        ));
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        let ids: Vec<u64> = doc["result"]["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["id"].as_u64().unwrap())
            .collect();

        let r = s.dispatch(&d(&json!({
            "id": "doc.group", "layers": ids, "name": "g1"
        })));
        assert_eq!(r["ok"], true, "group: {r}");
        let gid = r["result"]["layerId"].as_u64().unwrap();
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        let layers = doc["result"]["layers"].as_array().unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0]["type"], "group");
        assert_eq!(
            layers[0]["children"].as_array().unwrap().len(),
            2,
            "group holds both members"
        );

        // nested children are addressable — setLayer works inside the group
        let child = layers[0]["children"][0]["id"].as_u64().unwrap();
        let r = s.dispatch(&d(
            &json!({"id": "doc.setLayer", "layer": child, "opacity": 0.5}),
        ));
        assert_eq!(r["ok"], true, "setLayer on child: {r}");

        let r = s.dispatch(&d(&json!({"id": "doc.ungroup", "layer": gid})));
        assert_eq!(r["ok"], true, "ungroup: {r}");
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert_eq!(doc["result"]["layers"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn move_layer_reparents() {
        let mut s = s();
        new_doc(&mut s);
        s.dispatch(&d(
            &json!({"id": "doc.addLayer", "kind": "fill", "name": "a", "color": [1,0,0,1]}),
        ));
        s.dispatch(&d(
            &json!({"id": "doc.addLayer", "kind": "fill", "name": "b", "color": [0,1,0,1]}),
        ));
        s.dispatch(&d(
            &json!({"id": "doc.addLayer", "kind": "fill", "name": "c", "color": [0,0,1,1]}),
        ));
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        let ids: Vec<u64> = doc["result"]["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["id"].as_u64().unwrap())
            .collect();
        let (a, b, c) = (ids[0], ids[1], ids[2]);

        // group a+b, then reparent c into it and back out
        let r = s.dispatch(&d(&json!({
            "id": "doc.group", "layers": [a, b], "name": "g"
        })));
        assert_eq!(r["ok"], true, "group: {r}");
        let gid = r["result"]["layerId"].as_u64().unwrap();

        let r = s.dispatch(&d(&json!({
            "id": "doc.moveLayer", "layer": c, "parent": gid, "to": 0
        })));
        assert_eq!(r["ok"], true, "moveLayer into group: {r}");
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        let layers = doc["result"]["layers"].as_array().unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0]["children"].as_array().unwrap().len(), 3);
        assert_eq!(
            layers[0]["children"][0]["id"].as_u64().unwrap(),
            c,
            "c lands at index 0 inside the group"
        );

        // moving the group into its own member is a cycle — refused,
        // tree untouched
        let r = s.dispatch(&d(&json!({
            "id": "doc.moveLayer", "layer": gid, "parent": gid, "to": 0
        })));
        assert_eq!(r["ok"], false);
        let r = s.dispatch(&d(&json!({
            "id": "doc.moveLayer", "layer": gid, "parent": c, "to": 0
        })));
        assert_eq!(r["ok"], false, "non-group parent must fail");
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert_eq!(doc["result"]["layers"].as_array().unwrap().len(), 1);

        // move c back out to top level below the group
        let r = s.dispatch(&d(&json!({
            "id": "doc.moveLayer", "layer": c, "to": 0
        })));
        assert_eq!(r["ok"], true, "moveLayer to top level: {r}");
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        let layers = doc["result"]["layers"].as_array().unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0]["id"].as_u64().unwrap(), c);
        assert_eq!(layers[1]["children"].as_array().unwrap().len(), 2);

        // bad layer / bad parent ids error, don't panic, don't lose layers
        let r = s.dispatch(&d(&json!({
            "id": "doc.moveLayer", "layer": 9999, "to": 0
        })));
        assert_eq!(r["ok"], false);
        let r = s.dispatch(&d(&json!({
            "id": "doc.moveLayer", "layer": c, "parent": 9999, "to": 0
        })));
        assert_eq!(r["ok"], false);
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert_eq!(
            doc["result"]["layers"].as_array().unwrap().len()
                + doc["result"]["layers"][1]["children"]
                    .as_array()
                    .unwrap()
                    .len(),
            4,
            "every layer still present after failed moves"
        );
    }

    #[test]
    fn drop_shadow_renders() {
        let mut s = s();
        s.dispatch(&d(&json!({"id": "doc.new", "name": "t", "w": 64, "h": 64})));
        let px: Vec<u8> = (0..16 * 16 * 4)
            .map(|i| if i % 4 == 3 { 255 } else { 255 })
            .collect();
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&px);
        let r = s.dispatch(&d(&json!({
            "id": "doc.addLayer", "kind": "raster", "name": "sq",
            "w": 16, "h": 16, "rgbaB64": b64, "x": 20, "y": 20,
            "styles": {"dropShadow": {"dx": 8.0, "dy": 8.0, "blur": 2.0,
                "color": [1.0, 0.0, 0.0, 1.0], "spread": 0.0}}
        })));
        assert_eq!(r["ok"], true, "addLayer: {r}");
        let r = s.dispatch(&d(
            &json!({"id": "doc.render", "out": "/tmp/kb_shadow.png"}),
        ));
        assert_eq!(r["ok"], true, "render: {r}");
        // shadow covers 28..43 (offset 8 + 16px); (42,42) is inside it and
        // outside the white square (20..35) → red shadow tint
        let img = image::open("/tmp/kb_shadow.png").unwrap().to_rgba8();
        let p = img.get_pixel(42, 42);
        assert!(
            p[0] > 40 && p[3] > 0,
            "shadow should tint the canvas: {p:?}"
        );
    }

    #[test]
    fn flatten_no_double_backdrop() {
        let mut s = s();
        s.dispatch(&d(&json!({"id": "doc.new", "name": "t", "w": 32, "h": 32})));
        s.dispatch(&d(
            &json!({"id": "doc.setBackdrop", "color": [1.0, 0.0, 0.0, 1.0]}),
        ));
        // semi-transparent white — over red backdrop = pink
        let px: Vec<u8> = (0..8 * 8 * 4)
            .map(|i| match i % 4 {
                3 => 128,
                _ => 255,
            })
            .collect();
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&px);
        s.dispatch(&d(&json!({
            "id": "doc.addLayer", "kind": "raster", "name": "sq",
            "w": 8, "h": 8, "rgbaB64": b64, "x": 0, "y": 0,
        })));
        let r = s.dispatch(&d(
            &json!({"id": "doc.render", "out": "/tmp/kb_before.png"}),
        ));
        assert_eq!(r["ok"], true);
        let before = image::open("/tmp/kb_before.png").unwrap().to_rgba8();
        let before_px = *before.get_pixel(4, 4);

        let r = s.dispatch(&d(&json!({"id": "doc.flatten"})));
        assert_eq!(r["ok"], true, "flatten: {r}");
        let r = s.dispatch(&d(&json!({"id": "doc.render", "out": "/tmp/kb_after.png"})));
        assert_eq!(r["ok"], true);
        let after = image::open("/tmp/kb_after.png").unwrap().to_rgba8();
        let after_px = *after.get_pixel(4, 4);
        for c in 0..4 {
            let d = (before_px[c] as i32 - after_px[c] as i32).abs();
            assert!(
                d <= 2,
                "flatten changed pixel: {before_px:?} vs {after_px:?}"
            );
        }
    }

    #[test]
    fn export_formats_and_psd() {
        let mut s = s();
        s.dispatch(&d(&json!({"id": "doc.new", "name": "t", "w": 16, "h": 16})));
        s.dispatch(&d(
            &json!({"id": "doc.addLayer", "kind": "fill", "name": "bg", "color": [0.2,0.4,0.6,1.0]}),
        ));
        for (fmt, path) in [
            ("jpeg", "/tmp/kb_f.jpg"),
            ("tiff", "/tmp/kb_f.tiff"),
            ("png", "/tmp/kb_f.png"),
        ] {
            let r = s.dispatch(&d(&json!({"id": "doc.render", "out": path, "format": fmt})));
            assert_eq!(r["ok"], true, "render {fmt}: {r}");
            assert!(std::path::Path::new(path).exists());
        }
        let r = s.dispatch(&d(&json!({"id": "doc.exportPsd", "path": "/tmp/kb_f.psd"})));
        assert_eq!(r["ok"], true, "exportPsd: {r}");
        let head = std::fs::read("/tmp/kb_f.psd").unwrap();
        assert_eq!(&head[..4], b"8BPS");
        // and it round-trips through our own importer
        let r = s.dispatch(&d(&json!({"id": "doc.importPsd", "path": "/tmp/kb_f.psd"})));
        assert_eq!(r["ok"], true, "import exported psd: {r}");
    }

    #[test]
    fn batch_restores_redo_on_failure() {
        let mut s = s();
        new_doc(&mut s);
        s.dispatch(&d(
            &json!({"id": "doc.addLayer", "kind": "fill", "name": "a", "color": [1,0,0,1]}),
        ));
        s.dispatch(&d(&json!({"id": "doc.undo"}))); // redo stack now holds the addLayer
        let r = s.dispatch(&d(&json!({
            "id": "batch",
            "commands": [
                {"id": "doc.addLayer", "kind": "fill", "name": "b", "color": [0,1,0,1]},
                {"id": "doc.nonsense"}
            ]
        })));
        assert_eq!(r["ok"], false);
        // the successful sub-command cleared redo, but rollback must restore it
        let r = s.dispatch(&d(&json!({"id": "doc.redo"})));
        assert_eq!(
            r["result"]["changed"], true,
            "redo must survive failed atomic batch"
        );
    }

    fn add_raster(s: &mut Session) -> u64 {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(vec![255u8; 8 * 8 * 4]);
        let r = s.dispatch(&d(
            &json!({"id": "doc.addLayer", "kind": "raster", "name": "sq",
                    "w": 8, "h": 8, "x": 10, "y": 10, "rgbaB64": b64}),
        ));
        assert_eq!(r["ok"], true, "{r}");
        r["result"]["layerId"].as_u64().unwrap()
    }

    #[test]
    fn style_set_clear_scale() {
        let mut s = s();
        new_doc(&mut s);
        let lid = add_raster(&mut s);

        // defaults come up when only a flag is merged
        let r = s.dispatch(&d(
            &json!({"id": "doc.styleSet", "layer": lid, "effect": "dropShadow",
                    "params": {"dx": 12}}),
        ));
        assert_eq!(r["ok"], true, "{r}");
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        let st = &doc["result"]["layers"][0]["styles"]["dropShadow"];
        assert_eq!(st["dx"], 12.0);
        assert_eq!(st["enabled"], true);

        // merge keeps earlier params
        let r = s.dispatch(&d(
            &json!({"id": "doc.styleSet", "layer": lid, "effect": "dropShadow",
                    "params": {"blur": 4}}),
        ));
        assert_eq!(r["ok"], true);
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        let st = &doc["result"]["layers"][0]["styles"]["dropShadow"];
        assert_eq!(st["dx"], 12.0);
        assert_eq!(st["blur"], 4.0);

        // eye toggle via merge
        s.dispatch(&d(
            &json!({"id": "doc.styleSet", "layer": lid, "effect": "dropShadow",
                    "params": {"enabled": false}}),
        ));
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert_eq!(
            doc["result"]["layers"][0]["styles"]["dropShadow"]["enabled"],
            false
        );

        // unknown effect errors, doesn't panic
        let r = s.dispatch(&d(
            &json!({"id": "doc.styleSet", "layer": lid, "effect": "neon"}),
        ));
        assert_eq!(r["ok"], false);

        // styleScale multiplies px params, leaves ratios
        let r = s.dispatch(&d(
            &json!({"id": "doc.styleScale", "layer": lid, "scale": 2}),
        ));
        assert_eq!(r["ok"], true);
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        let st = &doc["result"]["layers"][0]["styles"]["dropShadow"];
        assert_eq!(st["dx"], 24.0);
        assert_eq!(st["blur"], 8.0);

        // clear one effect, then all
        s.dispatch(&d(
            &json!({"id": "doc.styleClear", "layer": lid, "effect": "dropShadow"}),
        ));
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert!(doc["result"]["layers"][0]["styles"]["dropShadow"].is_null());
        s.dispatch(&d(
            &json!({"id": "doc.styleSet", "layer": lid, "effect": "stroke",
                    "params": {"size": 5}}),
        ));
        s.dispatch(&d(&json!({"id": "doc.styleClear", "layer": lid})));
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert!(doc["result"]["layers"][0]["styles"]["stroke"].is_null());

        // undo restores the styles
        s.dispatch(&d(&json!({"id": "doc.undo"})));
        let doc = s.dispatch(&d(&json!({"id": "doc.json"})));
        assert_eq!(doc["result"]["layers"][0]["styles"]["stroke"]["size"], 5.0);
    }
}
