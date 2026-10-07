//! Everything-is-a-command: JSON dispatcher mirroring `composer::Session`.
//!
//! Unlike the composer dispatcher (which returns the `{ok,error}` envelope
//! itself), this one returns `Result<Value>` — the caller wraps it. A
//! `catch_unwind` at the top still turns any panic into an `Err`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::model::{Frame, FrameKind, FrameTarget, ImageFit, PagesDoc, Stroke, TextAlign};

/// one layout session: an optional open .kpages document
pub struct PgSession {
    pub doc: Option<PagesDoc>,
    /// last save/open target — `pg.save` without a path reuses it
    pub path: Option<PathBuf>,
}

impl PgSession {
    pub fn new() -> PgSession {
        PgSession {
            doc: None,
            path: None,
        }
    }

    /// dispatch a command; `id` is the command name, `v` the whole request
    /// object. Panics are caught and surfaced as errors (never-crash rule).
    pub fn dispatch(&mut self, id: &str, v: &Value) -> Result<Value> {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run(id, v)));
        match r {
            Ok(res) => res,
            Err(_) => Err(anyhow::anyhow!("command '{id}' panicked")),
        }
    }

    /// the command ids this dispatcher understands
    pub fn command_ids() -> Vec<&'static str> {
        vec![
            "pg.new",
            "pg.open",
            "pg.save",
            "pg.json",
            "pg.addPage",
            "pg.addMaster",
            "pg.removePage",
            "pg.duplicatePage",
            "pg.addFrame",
            "pg.setFrame",
            "pg.removeFrame",
            "pg.moveFrame",
            "pg.setMaster",
            "pg.setPageSize",
            "pg.linkFrames",
            "pg.setSpread",
            "pg.setStyle",
            "pg.applyStyle",
            "pg.render",
            "pg.renderPng",
        ]
    }

    fn run(&mut self, id: &str, v: &Value) -> Result<Value> {
        match id {
            "pg.new" => {
                let w = opt_f(v, "w")?.unwrap_or(612.0); // US Letter default
                let h = opt_f(v, "h")?.unwrap_or(792.0);
                if !(w > 0.0 && h > 0.0) {
                    anyhow::bail!("bad page size {w}×{h}");
                }
                let name = v.get("name").and_then(Value::as_str).unwrap_or("Untitled");
                let mut d = PagesDoc::new(name, w, h);
                if let Some(m) = v.get("margins") {
                    d.margins = serde_json::from_value(m.clone()).context("margins")?;
                }
                d.facing = v.get("facing").and_then(Value::as_bool).unwrap_or(false);
                self.doc = Some(d);
                self.path = None;
                Ok(json!({"name": name, "pageW": w, "pageH": h}))
            }
            "pg.open" => {
                let p = req_str(v, "path")?;
                let d = PagesDoc::load(Path::new(&p))?;
                let (np, nm) = (d.pages.len(), d.masters.len());
                self.doc = Some(d);
                self.path = Some(PathBuf::from(&p));
                Ok(json!({"pages": np, "masters": nm}))
            }
            "pg.save" => {
                let d = self.doc.as_ref().context("no document")?;
                let p = v
                    .get("path")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .or_else(|| self.path.clone())
                    .unwrap_or_else(|| PathBuf::from(format!("{}.kpages", d.name)));
                d.save(&p)?;
                self.path = Some(p.clone());
                Ok(json!({"path": p.to_string_lossy()}))
            }
            "pg.json" => {
                let d = self.doc.as_ref().context("no document")?;
                let mut v = serde_json::to_value(d)?;
                if let Some(obj) = v.as_object_mut() {
                    // view hints: how pages pair into spreads, and what text
                    // each frame actually shows after link resolution
                    obj.insert(
                        "spread".to_string(),
                        json!({"facing": d.facing, "pairs": d.spread_pairs()}),
                    );
                    let flows = crate::flow::resolve_flow(d)?;
                    let mut tf = serde_json::Map::new();
                    for (id, fl) in &flows {
                        let shown: String = fl
                            .layout
                            .lines
                            .iter()
                            .map(|l| l.text.as_str())
                            .collect::<Vec<_>>()
                            .join("\n");
                        tf.insert(
                            id.to_string(),
                            json!({"text": shown, "overset": fl.overset}),
                        );
                    }
                    obj.insert("textFlow".to_string(), Value::Object(tf));
                }
                Ok(v)
            }
            "pg.addPage" => {
                let d = self.doc.as_mut().context("no document")?;
                let master = match v.get("master") {
                    Some(Value::Null) | None => None,
                    Some(m) => Some(
                        m.as_u64()
                            .context("param 'master' must be a master index or null")?
                            as usize,
                    ),
                };
                let idx = d.add_page(master)?;
                Ok(json!({"page": idx}))
            }
            "pg.addMaster" => {
                let d = self.doc.as_mut().context("no document")?;
                let name = v
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("Master {}", d.masters.len() + 1));
                let idx = d.add_master(name);
                Ok(json!({"master": idx}))
            }
            "pg.removePage" => {
                let d = self.doc.as_mut().context("no document")?;
                let idx = req_usize(v, "page")?;
                d.remove_page(idx)?;
                Ok(json!({"pages": d.pages.len()}))
            }
            "pg.duplicatePage" => {
                let d = self.doc.as_mut().context("no document")?;
                let idx = req_usize(v, "page")?;
                let new_idx = d.duplicate_page(idx)?;
                Ok(json!({"page": new_idx}))
            }
            "pg.addFrame" => {
                let d = self.doc.as_mut().context("no document")?;
                let target = frame_target(v)?;
                let f = frame_from(v)?;
                let id = d.add_frame(target, f)?;
                Ok(json!({"frame": id}))
            }
            "pg.setFrame" => {
                let d = self.doc.as_mut().context("no document")?;
                let id = req_u64(v, "frame")?;
                set_frame(d, id, v)
            }
            "pg.moveFrame" => {
                let d = self.doc.as_mut().context("no document")?;
                let id = req_u64(v, "frame")?;
                let target = frame_target(v)?;
                d.move_frame(id, target)?;
                Ok(json!("ok"))
            }
            "pg.removeFrame" => {
                let d = self.doc.as_mut().context("no document")?;
                let id = req_u64(v, "frame")?;
                d.remove_frame(id)
                    .with_context(|| format!("frame {id} not found"))?;
                Ok(json!("ok"))
            }
            "pg.setPageSize" => {
                let d = self.doc.as_mut().context("no document")?;
                let w = opt_f(v, "w")?.context("missing param 'w'")?;
                let h = opt_f(v, "h")?.context("missing param 'h'")?;
                if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
                    anyhow::bail!("bad page size {w}×{h}");
                }
                // frames keep their positions — same behaviour as changing
                // page size in InDesign's Document Setup
                d.page_w = w;
                d.page_h = h;
                Ok(json!({"pageW": w, "pageH": h}))
            }
            "pg.linkFrames" => {
                let d = self.doc.as_mut().context("no document")?;
                let id = req_u64(v, "frame")?;
                if !is_text_frame(d, id) {
                    anyhow::bail!("frame {id} is not a text frame");
                }
                let to = match v.get("to") {
                    None => anyhow::bail!("missing param 'to' (frame id or null)"),
                    Some(Value::Null) => None,
                    Some(t) => Some(
                        t.as_u64()
                            .context("param 'to' must be a frame id or null")?,
                    ),
                };
                if let Some(t) = to {
                    if t == id {
                        anyhow::bail!("cannot link a frame to itself");
                    }
                    if !is_text_frame(d, t) {
                        anyhow::bail!("frame {t} is not a text frame");
                    }
                    // reject links that would close a cycle: walking `next`
                    // from the target must never reach the source
                    let mut cur = t;
                    let mut seen = std::collections::HashSet::new();
                    loop {
                        if cur == id {
                            anyhow::bail!("link would create a cycle");
                        }
                        if !seen.insert(cur) {
                            break; // existing ring unrelated to `id` — let render's guard handle
                        }
                        match d.frame(cur).and_then(|f| f.next) {
                            Some(n) => cur = n,
                            None => break,
                        }
                    }
                }
                // unwrap is safe: existence checked above
                let f = d.frame_mut(id).context("frame vanished")?;
                f.next = to;
                Ok(json!("ok"))
            }
            "pg.setSpread" => {
                let d = self.doc.as_mut().context("no document")?;
                let facing = v
                    .get("facing")
                    .and_then(Value::as_bool)
                    .context("missing param 'facing'")?;
                d.facing = facing;
                Ok(json!({"facing": facing, "pairs": d.spread_pairs()}))
            }
            "pg.setStyle" => {
                let d = self.doc.as_mut().context("no document")?;
                let name = req_str(v, "name")?;
                if name.is_empty() {
                    anyhow::bail!("style name cannot be empty");
                }
                // merge semantics (like pg.setFrame): provided fields
                // overwrite, explicit null clears, omitted fields keep
                let st = d.styles.entry(name.clone()).or_default();
                if let Some(f) = v.get("font") {
                    st.font = match f {
                        Value::Null => None,
                        x => Some(
                            x.as_str()
                                .context("param 'font' must be a string")?
                                .to_string(),
                        ),
                    };
                }
                if let Some(x) = v.get("size") {
                    match x {
                        Value::Null => st.size = None,
                        x => {
                            let s = x.as_f64().context("param 'size' must be a number")? as f32;
                            if !(s > 0.0 && s.is_finite()) {
                                anyhow::bail!("style size must be > 0");
                            }
                            st.size = Some(s);
                        }
                    }
                }
                if let Some(x) = v.get("color") {
                    match x {
                        Value::Null => st.color = None,
                        x => {
                            st.color = Some(
                                serde_json::from_value::<[f32; 4]>(x.clone())
                                    .context("param 'color' must be [r,g,b,a]")?,
                            );
                        }
                    }
                }
                if let Some(x) = v.get("leading") {
                    match x {
                        Value::Null => st.leading = None,
                        x => {
                            let l = x.as_f64().context("param 'leading' must be a number")? as f32;
                            if !(l > 0.0 && l.is_finite()) {
                                anyhow::bail!("style leading must be > 0");
                            }
                            st.leading = Some(l);
                        }
                    }
                }
                Ok(json!({"style": name}))
            }
            "pg.applyStyle" => {
                let d = self.doc.as_mut().context("no document")?;
                let id = req_u64(v, "frame")?;
                let name = req_str(v, "name")?;
                let st = d
                    .styles
                    .get(&name)
                    .with_context(|| format!("style '{name}' not defined (see pg.setStyle)"))?
                    .clone();
                let f = d
                    .frame_mut(id)
                    .with_context(|| format!("frame {id} not found"))?;
                match &mut f.kind {
                    FrameKind::Text {
                        font,
                        size,
                        color,
                        leading,
                        style,
                        ..
                    } => {
                        if let Some(x) = &st.font {
                            *font = x.clone();
                        }
                        if let Some(x) = st.size {
                            *size = x;
                        }
                        if let Some(x) = st.color {
                            *color = x;
                        }
                        if let Some(x) = st.leading {
                            *leading = x;
                        }
                        *style = name;
                    }
                    _ => anyhow::bail!("frame {id} is not a text frame"),
                }
                Ok(json!("ok"))
            }
            "pg.setMaster" => {
                let d = self.doc.as_mut().context("no document")?;
                let page = req_usize(v, "page")?;
                let master = match v.get("master") {
                    Some(Value::Null) | None => None,
                    Some(m) => Some(
                        m.as_u64()
                            .context("param 'master' must be a master index or null")?
                            as usize,
                    ),
                };
                if let Some(mi) = master {
                    if mi >= d.masters.len() {
                        anyhow::bail!("master {mi} out of range ({} masters)", d.masters.len());
                    }
                }
                let n_pages = d.pages.len();
                let p = d
                    .pages
                    .get_mut(page)
                    .with_context(|| format!("page {page} out of range ({n_pages} pages)"))?;
                p.master = master;
                Ok(json!("ok"))
            }
            "pg.render" => {
                let d = self.doc.as_ref().context("no document")?;
                let out = req_str(v, "out")?;
                let bytes = crate::pdf::render_pdf(d)?;
                std::fs::write(&out, &bytes).with_context(|| format!("write {out}"))?;
                Ok(json!({
                    "path": out,
                    "pages": d.pages.len(),
                    "bytes": bytes.len(),
                }))
            }
            "pg.renderPng" => {
                let d = self.doc.as_ref().context("no document")?;
                let page = req_usize(v, "page")?;
                let out = req_str(v, "out")?;
                let dpi = opt_f(v, "dpi")?.unwrap_or(144.0);
                let img = crate::raster::render_page(d, page, dpi)?;
                img.save(&out).with_context(|| format!("save {out}"))?;
                Ok(json!({"path": out, "w": img.width(), "h": img.height()}))
            }
            _ => anyhow::bail!("unknown command id: {id}"),
        }
    }
}

impl Default for PgSession {
    fn default() -> Self {
        PgSession::new()
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

fn req_usize(v: &Value, k: &str) -> Result<usize> {
    Ok(req_u64(v, k)? as usize)
}

fn is_text_frame(d: &PagesDoc, id: u64) -> bool {
    matches!(d.frame(id), Some(f) if matches!(f.kind, FrameKind::Text { .. }))
}

/// optional numeric param — errors if present but not a number
fn opt_f(v: &Value, k: &str) -> Result<Option<f32>> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(n) => n
            .as_f64()
            .map(|f| Some(f as f32))
            .with_context(|| format!("param '{k}' must be a number")),
    }
}

/// optional [r,g,b,a] color param
fn opt_color(v: &Value, k: &str) -> Result<Option<[f32; 4]>> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(c) => serde_json::from_value::<[f32; 4]>(c.clone())
            .map(Some)
            .with_context(|| format!("param '{k}' must be [r,g,b,a]")),
    }
}

/// optional Stroke param (object or null)
fn opt_stroke(v: &Value, k: &str) -> Result<Option<Stroke>> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(s) => serde_json::from_value::<Stroke>(s.clone())
            .map(Some)
            .with_context(|| format!("param '{k}' must be {{color,width}}")),
    }
}

/// `pg.addFrame` container: `master` (index) wins over `page` (default 0)
fn frame_target(v: &Value) -> Result<FrameTarget> {
    if let Some(m) = v.get("master") {
        let i = m
            .as_u64()
            .context("param 'master' must be a master index")? as usize;
        return Ok(FrameTarget::Master(i));
    }
    Ok(FrameTarget::Page(
        v.get("page").and_then(Value::as_u64).unwrap_or(0) as usize,
    ))
}

/// shared optional frame fields: x, y, w, h, rotationDeg, z
fn apply_frame_geom(f: &mut Frame, v: &Value) -> Result<()> {
    if let Some(n) = opt_f(v, "x")? {
        f.x = n;
    }
    if let Some(n) = opt_f(v, "y")? {
        f.y = n;
    }
    if let Some(n) = opt_f(v, "w")? {
        if n <= 0.0 {
            anyhow::bail!("frame w must be > 0");
        }
        f.w = n;
    }
    if let Some(n) = opt_f(v, "h")? {
        if n <= 0.0 {
            anyhow::bail!("frame h must be > 0");
        }
        f.h = n;
    }
    if let Some(n) = opt_f(v, "rotationDeg")? {
        f.rotation_deg = n;
    }
    if let Some(n) = v.get("z").and_then(Value::as_i64) {
        f.z = n as i32;
    }
    Ok(())
}

/// build a Frame from a `pg.addFrame` request:
/// {"page":0,"kind":"text","x":..,"y":..,"w":..,"h":..,"text":"..","size":18}
/// {"kind":"image","path":"/abs/img.png","fit":"fit"}
/// {"kind":"rect","fill":[r,g,b,a],"stroke":{"color":[..],"width":2}}
/// {"kind":"line","x2":w,"y2":h,"stroke":{...}}
fn frame_from(v: &Value) -> Result<Frame> {
    let kind = req_str(v, "kind")?;
    let mut f = match kind.as_str() {
        "text" => Frame::new(
            FrameKind::Text {
                text: v
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                font: v
                    .get("font")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                size: opt_f(v, "size")?.unwrap_or(12.0),
                color: opt_color(v, "color")?.unwrap_or([0.0, 0.0, 0.0, 1.0]),
                align: v
                    .get("align")
                    .and_then(Value::as_str)
                    .map(TextAlign::parse)
                    .unwrap_or_default(),
                leading: opt_f(v, "leading")?.unwrap_or(1.2),
                style: String::new(),
            },
            0.0,
            0.0,
            100.0,
            100.0,
        ),
        "image" => Frame::new(
            FrameKind::Image {
                path: PathBuf::from(req_str(v, "path")?),
                fit: v
                    .get("fit")
                    .and_then(Value::as_str)
                    .map(|s| {
                        ImageFit::parse(s)
                            .with_context(|| format!("bad fit '{s}' (fill|fit|stretch)"))
                    })
                    .transpose()?
                    .unwrap_or_default(),
            },
            0.0,
            0.0,
            100.0,
            100.0,
        ),
        "rect" => Frame::new(
            FrameKind::Rect {
                fill: opt_color(v, "fill")?,
                stroke: opt_stroke(v, "stroke")?,
            },
            0.0,
            0.0,
            100.0,
            100.0,
        ),
        "line" => Frame::new(
            FrameKind::Line {
                x2: opt_f(v, "x2")?.unwrap_or(100.0),
                y2: opt_f(v, "y2")?.unwrap_or(0.0),
                stroke: opt_stroke(v, "stroke")?.unwrap_or(Stroke {
                    color: [0.0, 0.0, 0.0, 1.0],
                    width: 1.0,
                }),
            },
            0.0,
            0.0,
            100.0,
            100.0,
        ),
        _ => anyhow::bail!("unknown frame kind: {kind} (text|image|rect|line)"),
    };
    apply_frame_geom(&mut f, v)?;
    Ok(f)
}

/// `pg.setFrame` — all fields optional; kind-specific fields apply only when
/// they match the frame's kind (setting `text` on a rect is an error).
fn set_frame(doc: &mut PagesDoc, id: u64, v: &Value) -> Result<Value> {
    let f = doc
        .frame_mut(id)
        .with_context(|| format!("frame {id} not found"))?;
    apply_frame_geom(f, v)?;
    match &mut f.kind {
        FrameKind::Text {
            text,
            font,
            size,
            color,
            align,
            leading,
            ..
        } => {
            if let Some(t) = v.get("text").and_then(Value::as_str) {
                *text = t.to_string();
            }
            if let Some(s) = v.get("font").and_then(Value::as_str) {
                *font = s.to_string();
            }
            if let Some(n) = opt_f(v, "size")? {
                *size = n;
            }
            if let Some(c) = opt_color(v, "color")? {
                *color = c;
            }
            if let Some(a) = v.get("align").and_then(Value::as_str) {
                *align = TextAlign::parse(a);
            }
            if let Some(n) = opt_f(v, "leading")? {
                *leading = n;
            }
        }
        FrameKind::Image { path, fit } => {
            if let Some(p) = v.get("path").and_then(Value::as_str) {
                *path = PathBuf::from(p);
            }
            if let Some(s) = v.get("fit").and_then(Value::as_str) {
                *fit = ImageFit::parse(s)
                    .with_context(|| format!("bad fit '{s}' (fill|fit|stretch)"))?;
            }
        }
        FrameKind::Rect { fill, stroke } => {
            if v.get("fill").is_some() {
                *fill = opt_color(v, "fill")?; // null clears the fill
            }
            if v.get("stroke").is_some() {
                *stroke = opt_stroke(v, "stroke")?;
            }
        }
        FrameKind::Line { x2, y2, stroke } => {
            if let Some(n) = opt_f(v, "x2")? {
                *x2 = n;
            }
            if let Some(n) = opt_f(v, "y2")? {
                *y2 = n;
            }
            if v.get("stroke").is_some() {
                *stroke = opt_stroke(v, "stroke")?.context("line stroke cannot be null")?;
            }
        }
    }
    Ok(json!("ok"))
}

/// MCP-shaped tool specs ({id, name, description, inputSchema}) for every
/// pg.* command — the single source of truth the Session registry and the
/// MCP server both read.
pub fn command_specs() -> Vec<Value> {
    let spec = |id: &str, desc: &str, props: Value, required: &[&str]| {
        json!({
            "id": id,
            "name": id.replace('.', "_"),
            "description": desc,
            "inputSchema": {"type": "object", "properties": props, "required": required},
        })
    };
    let s = |d: &str| json!({"type": "string", "description": d});
    let n = |d: &str| json!({"type": "number", "description": d});
    let o = |d: &str| json!({"type": "object", "description": d});
    vec![
        spec(
            "pg.new",
            "Create a new pages document (default 612x792pt Letter)",
            json!({"name": s("name"), "w": n("page width pt"), "h": n("page height pt"), "margins": o("{top,right,bottom,left} pt")}),
            &[],
        ),
        spec(
            "pg.open",
            "Open a .kpages document",
            json!({"path": s("doc path")}),
            &["path"],
        ),
        spec(
            "pg.save",
            "Save the pages document",
            json!({"path": s("path; default <name>.kpages")}),
            &[],
        ),
        spec(
            "pg.json",
            "Return the pages document's JSON state",
            json!({}),
            &[],
        ),
        spec(
            "pg.addPage",
            "Append a page (master: index or null)",
            json!({"master": n("master index")}),
            &[],
        ),
        spec(
            "pg.addMaster",
            "Add a master page template",
            json!({"name": s("master name")}),
            &[],
        ),
        spec(
            "pg.removePage",
            "Remove a page",
            json!({"page": n("page index")}),
            &["page"],
        ),
        spec(
            "pg.duplicatePage",
            "Clone a page with fresh frame ids, right after it",
            json!({"page": n("page index")}),
            &["page"],
        ),
        spec(
            "pg.addFrame",
            "Add a frame (kind: text|image|rect|line) on a page (default 0) or master",
            json!({"page": n("page index"), "master": n("master index (wins over page)"), "kind": s("frame kind"), "x": n("pt"), "y": n("pt"), "w": n("pt"), "h": n("pt"), "text": s("text content"), "path": s("image path"), "fit": s("fill|fit|stretch"), "x2": n("line end x"), "y2": n("line end y")}),
            &["kind"],
        ),
        spec(
            "pg.setFrame",
            "Edit a frame by global id (geometry + kind fields; 'master'/'page' not needed)",
            json!({"frame": n("frame id")}),
            &["frame"],
        ),
        spec(
            "pg.removeFrame",
            "Remove a frame by global id",
            json!({"frame": n("frame id")}),
            &["frame"],
        ),
        spec(
            "pg.moveFrame",
            "Move a frame onto another page or master",
            json!({"frame": n("frame id"), "page": n("page index"), "master": n("master index")}),
            &["frame"],
        ),
        spec(
            "pg.setMaster",
            "Assign/remove a page's master (master: index or null)",
            json!({"page": n("page index"), "master": n("master index or null")}),
            &["page"],
        ),
        spec(
            "pg.setPageSize",
            "Resize the document's pages (w/h in pt; frames keep positions)",
            json!({"w": n("page width pt"), "h": n("page height pt")}),
            &["w", "h"],
        ),
        spec(
            "pg.linkFrames",
            "Thread text frames: overflow beyond a frame's height continues into 'to' (frame id or null to unlink)",
            json!({"frame": n("source frame id"), "to": n("target text frame id, or null")}),
            &["frame", "to"],
        ),
        spec(
            "pg.setSpread",
            "Facing pages on/off: pages >= 1 pair as verso/recto in pg.json's spread field (PDF stays one page per sheet)",
            json!({"facing": json!({"type": "boolean", "description": "facing pages"})}),
            &["facing"],
        ),
        spec(
            "pg.setStyle",
            "Define/update a named text style (fields omitted keep current values; null clears)",
            json!({"name": s("style name"), "font": s("family name or path"), "size": n("pt"), "color": json!({"type": "array", "description": "[r,g,b,a] 0..1"}), "leading": n("multiplier")}),
            &["name"],
        ),
        spec(
            "pg.applyStyle",
            "Apply a named style's set fields onto a text frame",
            json!({"frame": n("text frame id"), "name": s("style name")}),
            &["frame", "name"],
        ),
        spec(
            "pg.render",
            "Export the document to PDF",
            json!({"out": s("output path")}),
            &["out"],
        ),
        spec(
            "pg.renderPng",
            "Render a page to PNG at a dpi (needs out+dpi)",
            json!({"page": n("page index"), "out": s("output path"), "dpi": n("render dpi, e.g. 96")}),
            &["page", "out", "dpi"],
        ),
    ]
}
