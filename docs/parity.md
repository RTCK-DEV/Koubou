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
| Shape layers (SVG path, fill+stroke, dash) | ✅ | `doc.addLayer kind=shape` |
| Shape generators (rect/roundRect/ellipse/star/line) + edit/remove | ✅ | `cmd:doc.addShape`, `doc.shapeSet`, `doc.shapeRemove` |
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

## Vector / design (vectorcraft)

| feature | status | command / note |
|---|---|---|
| Shape layers with SVG `d` paths | ✅ | `doc.addLayer kind=shape` |
| Parametric generators + dash strokes | ✅ | `cmd:doc.addShape`, `doc.shapeSet`, `doc.shapeRemove` |
| Boolean ops, path editor, node tools | ❌ | — |
| SVG import/export, artboards | ❌ | — |

## Motion / effects (filmcraft + effectcraft)

| feature | status | command / note |
|---|---|---|
| Timeline model (.kmotion): tracks, clips, cues | ✅ | `cmd:tl.new`, `tl.open`, `tl.save`, `tl.json`, `tl.addTrack` |
| Clip edit (in/out/offset, keyframed opacity/scale/x/y, fades) | ✅ | `tl.addClip`, `tl.setClip`, `tl.removeClip` |
| Subtitles (cues) + SRT burn-in | ✅ | `tl.addCue` |
| Frame preview + mp4 export via ffmpeg | ✅ | `tl.renderFrame`, `tl.render` |
| Silence detection, media probe | ✅ | `tl.detectSilence`, `tl.probe` |
| Generative clip insert (minimax-h3 endpoint) | ✅ | `tl.generateClip` |
| Speech-to-subtitle (Speech framework) | ✅ | app (SFSpeechRecognizer → `tl.addCue`) |
| Audio ducking/EQ/comp per clip | 🟡 | ffmpeg af params; no UI yet |
| Transitions beyond fade, speed ramp, multicam | ❌ | — |
| GPU realtime playback | ❌ | frame-by-frame only |

## Pages / layout (designcraft)

| feature | status | command / note |
|---|---|---|
| Multi-page doc (.kpages), masters | ✅ | `cmd:pg.new`, `pg.open`, `pg.save`, `pg.addPage`, `pg.setMaster` |
| Frames: text/image/rect/line, rotation | ✅ | `pg.addFrame`, `pg.setFrame`, `pg.removeFrame` |
| PDF export (PDF 1.4, Helvetica+JPEG+vectors) | ✅ | `pg.render` |
| Page PNG preview | ✅ | `pg.renderPng` |
| Text flow/threading, facing pages, bleed | ❌ | — |
| Character/paragraph styles, hyphenation | ❌ | — |

## Print / PDF (printcraft)

| feature | status | command / note |
|---|---|---|
| PDF import → raster layers per page | ✅ | app Import PDF (PDFKit → `doc.addLayer kind=raster`) |
| PDF export from pages domain | ✅ | `pg.render` |
| PDF annotation forms/sign/preflight | ❌ | — |
| Imposition, color management for print | ❌ | — |

## Automation surfaces
