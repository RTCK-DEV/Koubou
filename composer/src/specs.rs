//! Command specs — the single source of truth for what every command does
//! and what params it takes. MCP tools, control-channel introspection and
//! the `commands` response all derive from this table (plus the motion and
//! pages crates' own `command_specs()`), so schemas cannot drift from the
//! dispatch arms.
//!
//! Shape: {"id", "name", "description", "inputSchema"} where `name` is the
//! MCP-safe tool name (dots replaced by underscores — MCP clients reject
//! dotted tool names).

use serde_json::{json, Value};

/// spec entry: doc field is an id literal; name is derived.
fn spec(id: &str, desc: &str, props: Value, required: &[&str]) -> Value {
    json!({
        "id": id,
        "name": id.replace('.', "_"),
        "description": desc,
        "inputSchema": {
            "type": "object",
            "properties": props,
            "required": required,
        }
    })
}

fn s(d: &str) -> Value {
    json!({"type": "string", "description": d})
}
fn n(d: &str) -> Value {
    json!({"type": "number", "description": d})
}
fn b(d: &str) -> Value {
    json!({"type": "boolean", "description": d})
}
fn o(d: &str) -> Value {
    json!({"type": "object", "description": d})
}

/// specs for the engine + document + session commands owned by this crate
pub fn base() -> Vec<Value> {
    vec![
        spec("ping", "Server name/version", json!({}), &[]),
        spec(
            "commands",
            "List every command id plus its full tool spec",
            json!({}),
            &[],
        ),
        spec(
            "batch",
            "Run a list of commands; atomic by default (a failing sub-command rolls back every change made so far)",
            json!({
                "commands": {"type": "array", "description": "[{\"id\": ..., ...params}, ...]", "items": {"type": "object"}},
                "atomic": b("roll back on first failure (default true)"),
            }),
            &["commands"],
        ),
        spec("scan", "Scan a folder for RAW+JPEG assets", json!({"folder": s("absolute folder path")}), &["folder"]),
        spec("meta", "EXIF/metadata for an image file", json!({"path": s("file path")}), &["path"]),
        spec("thumb", "Write a thumbnail PNG", json!({"path": s("file"), "out": s("output path"), "maxPx": n("max dimension, default 512")}), &["path"]),
        spec("render", "Develop an image file with a recipe, write PNG", json!({"path": s("file"), "recipe": o("recipe params"), "out": s("output path"), "maxPx": n("max dimension, 0=full")}), &["path"]),
        spec("auto", "Auto-analyze exposure/WB for a file", json!({"path": s("file")}), &["path"]),
        spec("sidecar.read", "Read the .araware.json sidecar of an asset", json!({"path": s("file")}), &["path"]),
        spec("sidecar.write", "Write the .araware.json sidecar of an asset", json!({"path": s("file"), "json": s("sidecar JSON string")}), &["path", "json"]),
        spec("setRating", "Set 0-5 rating on an asset", json!({"path": s("file"), "rating": n("0-5")}), &["path", "rating"]),
        spec("setLabel", "Set a colour label on an asset", json!({"path": s("file"), "label": s("label")}), &["path"]),
        // ---- document (.koubou) ----
        spec("doc.new", "Create a new empty document", json!({"name": s("name"), "w": n("px, default 1920"), "h": n("px, default 1080")}), &[]),
        spec("doc.fromPhoto", "Create a document whose base layer develops a photo", json!({"path": s("image file")}), &["path"]),
        spec("doc.open", "Open a .koubou document", json!({"path": s("doc path")}), &["path"]),
        spec("doc.save", "Save the open document as .koubou", json!({"path": s("doc path; default <name>.koubou")}), &[]),
        spec("doc.json", "Return the open document's JSON state", json!({}), &[]),
        spec("doc.info", "Document name/size/layer count", json!({}), &[]),
        spec("doc.importPsd", "Import a layered PSD as the open document", json!({"path": s("psd path")}), &["path"]),
        spec(
            "doc.addLayer",
            "Add a layer. kind: fill (color), gradient (line+stops), adjustment (recipe), text (text/font/size/align/color), shape (d or gen + shapes), develop (path+recipe), rasterFile (path), raster (w/h/rgbaB64), group. Common: name/x/y/opacity/blend",
            json!({
                "kind": s("fill|gradient|adjustment|text|shape|develop|rasterFile|raster|group"),
                "name": s("layer name"),
                "x": n("doc-space x"), "y": n("doc-space y"),
                "opacity": n("0-1"), "blend": s("blend mode, e.g. colorBurn"),
            }),
            &["kind"],
        ),
        spec(
            "doc.setLayer",
            "Edit a layer: name/visible/opacity/blend/x/y/scale plus kind fields (recipe, text, fill, shapes) or mask:null to clear",
            json!({"layer": n("layer id")}),
            &["layer"],
        ),
        spec("doc.removeLayer", "Remove a layer", json!({"layer": n("layer id")}), &["layer"]),
        spec("doc.duplicateLayer", "Clone a layer (fresh ids, offset +16,+16)", json!({"layer": n("layer id")}), &["layer"]),
        spec("doc.reorder", "Move a layer in the stack (0 = bottom)", json!({"layer": n("layer id"), "to": n("index")}), &["layer", "to"]),
        spec("doc.moveLayer", "Move a layer to (parent group, index) — reparent into/out of groups; 'parent' omitted = top level", json!({"layer": n("layer id"), "parent": n("group layer id, or null for top level"), "to": n("index within the parent")}), &["layer", "to"]),
        spec("doc.mergeDown", "Bake a layer together with the layer below into one raster layer", json!({"layer": n("layer id")}), &["layer"]),
        spec("doc.flatten", "Bake the whole composite into a single raster layer", json!({"name": s("layer name")}), &[]),
        spec("doc.resize", "Resize the canvas (content does not rescale)", json!({"w": n("px"), "h": n("px")}), &["w", "h"]),
        spec("doc.crop", "Crop the canvas; layers shift by -x,-y", json!({"x": n("px"), "y": n("px"), "w": n("px"), "h": n("px")}), &["w", "h"]),
        spec("doc.setBackdrop", "Canvas backdrop colour behind transparency", json!({"color": {"type": "array", "description": "[r,g,b,a] 0-1"}}), &["color"]),
        spec("doc.render", "Composite the open document (format: png|jpeg|tiff; pngB64 without 'out')", json!({"out": s("output path"), "format": s("png|jpeg|jpg|tiff (default png)"), "quality": n("jpeg quality 0-100, default 90"), "maxPx": n("preview size, 0=full")}), &[]),
        spec("doc.exportPsd", "Export a layered PSD (names, groups, masks, blends, offsets; flat:true for merged only)", json!({"path": s("output .psd path"), "flat": {"type": "boolean", "description": "true = flattened composite only (default layered)"}}), &["path"]),
        spec("doc.pick", "Topmost layer hit at doc point (x,y) — skips adjustment layers", json!({"x": n("doc x"), "y": n("doc y")}), &["x", "y"]),
        spec("doc.bounds", "Placed bounds [x,y,w,h] of one layer, or all layers when 'layer' omitted", json!({"layer": n("layer id (optional)")}), &[]),
        spec("doc.exportLayer", "Render one layer's own pixels to PNG (or pngB64)", json!({"layer": n("layer id"), "out": s("output path")}), &["layer"]),
        spec("doc.maskPaint", "Paint a soft round dab into a layer mask", json!({"layer": n("layer id"), "cx": n("x"), "cy": n("y"), "r": n("radius px"), "value": n("0-1 coverage"), "softness": n("0-1")}), &["layer", "cx", "cy"]),
        spec("doc.maskRect", "Replace a layer's mask with a rectangle (1 inside, 0 outside)", json!({"layer": n("layer id"), "x": n("rect x"), "y": n("rect y"), "w": n("px"), "h": n("px"), "feather": n("blur px")}), &["layer", "x", "y", "w", "h"]),
        spec("doc.maskInvert", "Invert a layer mask (absent mask = fully hidden)", json!({"layer": n("layer id")}), &["layer"]),
        spec("doc.styleSet", "Set/merge a layer effect's params (Photoshop layer styles). effect ∈ dropShadow|innerShadow|outerGlow|innerGlow|bevel|satin|colorOverlay|gradientOverlay|patternOverlay|stroke; params merges over defaults/current, e.g. {enabled,blend,color:[r,g,b,a],dx,dy,blur,spread}", json!({"layer": n("layer id"), "effect": s("effect name"), "params": o("partial effect params")}), &["layer", "effect"]),
        spec("doc.styleClear", "Remove one effect's params from a layer ('effect' omitted = clear all styles)", json!({"layer": n("layer id"), "effect": s("effect name")}), &["layer"]),
        spec("doc.styleScale", "PS 'Scale Effects': multiply every px-dimension param of a layer's styles (ratios/angles/opacities stay)", json!({"layer": n("layer id"), "scale": n("multiplier")}), &["layer", "scale"]),
        spec("doc.group", "Wrap the given layers (or all when 'layers' omitted) in a group at the topmost member's slot", json!({"layers": {"type": "array", "description": "layer ids to group", "items": {"type": "number"}}, "name": s("group name")}), &[]),
        spec("doc.ungroup", "Dissolve a group: children splice back into the stack (fresh ids)", json!({"layer": n("group layer id")}), &["layer"]),
        spec("doc.addShape", "Append a shape to a shape layer (gen: rect|roundRect|ellipse|star|line, or d)", json!({"layer": n("layer id"), "gen": s("shape generator"), "d": s("svg path"), "fill": {"type": "array", "description": "[r,g,b,a]"}, "stroke": o("{color,width,dash?}")}), &["layer"]),
        spec("doc.shapeSet", "Edit one shape on a shape layer by index (d/gen/fill/stroke/removeFill/removeStroke)", json!({"layer": n("layer id"), "index": n("shape index")}), &["layer", "index"]),
        spec("doc.shapeRemove", "Remove one shape from a shape layer by index", json!({"layer": n("layer id"), "index": n("shape index")}), &["layer", "index"]),
        spec("doc.shapeNodes", "List the editable anchor nodes of a shape layer's paths (flat index across shapes; kind l|c|q)", json!({"layer": n("layer id")}), &["layer"]),
        spec("doc.moveNode", "Move anchor node `index` of a shape layer to doc coords (x,y); adjacent control handles ride along", json!({"layer": n("layer id"), "index": n("flat node index"), "x": n("doc x"), "y": n("doc y")}), &["layer", "index", "x", "y"]),
        spec("doc.undo", "Undo the last document mutation", json!({}), &[]),
        spec("doc.redo", "Redo the last undone document mutation", json!({}), &[]),
        // ---- history for delegated domains is handled by Session itself ----
        spec("tl.undo", "Undo the last timeline mutation", json!({}), &[]),
        spec("tl.redo", "Redo the last undone timeline mutation", json!({}), &[]),
        spec("pg.undo", "Undo the last pages mutation", json!({}), &[]),
        spec("pg.redo", "Redo the last undone pages mutation", json!({}), &[]),
    ]
}
