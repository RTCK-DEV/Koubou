# Parity tracker — Lightroom + Photoshop

One row per feature. Status: ✅ works · 🟡 partial/shallow · ❌ missing ·
➖ out of scope (v1). `cmd:` cites the dispatch id implementing the row —
`scripts/parity_check.sh` verifies every cited id exists in
`Session::command_ids`.

## Library / catalog (Lightroom)

| feature | status | command / note |
|---|---|---|
| Folder scan (RAW + JPEG + pairs) | ✅ | `cmd:scan` |
| EXIF/metadata readout | ✅ | `cmd:meta` |
| Star rating 0–5 + colour labels | ✅ | `cmd:setRating`, `cmd:setLabel` |
| JSON sidecars (non-destructive edits) | ✅ | `cmd:sidecar.read`, `cmd:sidecar.write` |
| Thumbnails | ✅ | `cmd:thumb` |
| Grid sort/filter (name, date, rating, size, label, camera, lens) | ✅ | app UI |
| Collections / smart collections | ❌ | — |
| Keywording, face detection, map | ❌ | — |
| Import/copy-as-DNG, rename templates | ❌ | — |
| Cloud sync / shared albums | ❌ | — |

## Develop (Lightroom develop module + DaVinci-style color)

| feature | status | command / note |
|---|---|---|
| RAW decode breadth | ✅ | LibRaw — CR3/NEF/ARW/DNG/RAF/ORF/RW2… |
| Render with recipe | ✅ | `cmd:render` |
| Basic tone (exposure..blacks, contrast, pivot, rolloffs) | ✅ | recipe |
| White balance (as-shot/auto/manual/pick) | ✅ | recipe `wb_mode`, `wb_pick` |
| Curves (Y + RGB + H·H/H·S/H·L/L·S/S·S) | ✅ | recipe `curve`, `curve_r/g/b`, … |
| Lift/gamma/gain + offset + zone wheels | ✅ | recipe |
| Split-tone / color mixer / mono | ✅ | recipe |
| HSL qualifier (key → shift, invert, clean, blur, show) | ✅ | recipe `qh/qs/ql/qadj/…` |
| Power windows (circle/gradient, invert, opacity) | ✅ | recipe `windows` |
| Retouch (heal spots, dodge/burn, clone) | ✅ | recipe `spots`, `lights`, `clones` |
| Detail (sharpen, NR luma/chroma, clarity, beauty) | ✅ | recipe |
| Effects (vignette, grain, glow, flare, deband, CA fix) | ✅ | recipe |
| Geometry (crop, rotate, keystone V/H) | ✅ | recipe `crop`, `rotation_deg`, `key_v/h` |
| .cube LUT import | ✅ | recipe `lut_file`, `lut_amount` |
| Auto analysis (straighten, keystone, NR, CA, vibrance, zones) | ✅ | `cmd:auto` |
| GPU (wgpu) pipeline + scopes (hist/wave/vector/CIE) | ✅ | `koubou_scopes` FFI |
| Camera-matched profiles (DCP) | 🟡 | LibRaw matrices only |
| Lens corrections (distortion/LCP) | ❌ | — |
| HDR merge / panorama | ❌ | — |
| History/snapshots in UI | ✅ | grade versions + undo in single-photo editor |

## Layers & compositing (Photoshop)

| feature | status | command / note |
|---|---|---|
| Layer stack (add/remove/reorder/select) | ✅ | `cmd:doc.addLayer`, `doc.removeLayer`, `doc.reorder`, `doc.setLayer` |
| Opacity + visibility | ✅ | `doc.setLayer` |
| Blend modes — 27 per PDF/W3C | ✅ | `BlendMode`; `Dissolve` falls back to normal (🟡) |
| Layer masks (paint, density, feather, invert) | ✅ | `cmd:doc.maskPaint` + `Layer.mask` |
| Develop layers (live RAW develop inside stack) | ✅ | `doc.addLayer kind=develop` |
| Adjustment layers (full recipe over the stack below) | ✅ | `doc.addLayer kind=adjustment` |
| Fill layers (solid/linear gradient) | ✅ | `doc.addLayer kind=fill/gradient` |
| Shape layers (SVG path, fill+stroke) | ✅ | `doc.addLayer kind=shape` |
| Text layers (font, size, tracking, leading, align, wrap) | ✅ | `doc.addLayer kind=text` |
| Group layers (isolated composite) | 🟡 | engine ✅; UI nesting ❌ |
| Raster layers (embedded PNG / file link) | ✅ | `doc.addLayer kind=raster/rasterFile` |
| Document model (.koubou JSON), open/save | ✅ | `doc.new`, `doc.open`, `doc.save`, `doc.json`, `doc.fromPhoto` |
| Full-res export + preview render | ✅ | `doc.render` (PNG only — JPEG/TIFF ❌) |
| PSD import (layers, blend modes, masks→flattened) | ✅ | `doc.importPsd` / `koubou-cli psd` |
| PSD export | ❌ | — |
| Canvas: pan/zoom | 🟡 | fit-to-view only |
| Canvas: drag-move, transform handles, marquee | ❌ | numeric x/y/scale via inspector |
| On-canvas text editing | ❌ | inspector text field only |
| Undo/redo (documents) | ❌ | — |
| Layer styles (shadow, glow, stroke, bevel) | ❌ | — |
| Smart objects / linked docs | ❌ | — |
| Channels palette, alpha ops | ❌ | — |
| Filter gallery / liquify / warp | ❌ | — |
| Guides, grids, snapping | ❌ | — |
| Text on path, paragraph styles | ❌ | — |
| Content-aware fill / generative tools | ❌ | — |

## Automation surfaces

| surface | status | note |
|---|---|---|
| JSON command dispatch (all ops by id) | ✅ | `composer::commands::Session` |
| TCP control channel + stdio | ✅ | `koubou-cli control [--port N]` |
| MCP server (tools/list, tools/call) | ✅ | `koubou-cli mcp` |
| macOS FFI (engine + doc dispatch) | ✅ | `araware_*` (engine), `kou_*` (docs) exports |
| Undo-safe command log (for scripted sessions) | 🟡 | commands are idempotent-ish; no formal journal |
| Headless UI snapshot (app-level) | ❌ | render verification via `doc.render` instead |

## Top gaps (ordered by user impact)

1. Doc undo/redo — losing edits is the #1 trust gap
2. Canvas manipulation (drag-move, on-canvas text, marquee)
3. Inspector coverage for the full recipe (curves/wheels/qualifier)
4. JPEG/TIFF export from documents
5. Mask UX (thumbnail, gradient tool, mask view)
6. Group nesting in the UI
7. PSD export

## Out of scope (v1, deliberate)

Video editing, print/PDF layout, page composition, 3D — the craft suite's
other domains are separate products, not Koubou's job.
