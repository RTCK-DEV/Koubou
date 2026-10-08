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
| Layer stack (add/remove/reorder/select/duplicate/reparent) | ✅ | `cmd:doc.addLayer`, `doc.removeLayer`, `doc.reorder`, `doc.moveLayer`, `doc.setLayer`, `doc.duplicateLayer` + panel drag/drop |
| Opacity + visibility | ✅ | `doc.setLayer` |
| Blend modes — 27 per PDF/W3C | ✅ | `BlendMode`; `Dissolve` falls back to normal (🟡) |
| Layer masks (paint, density, feather, invert) | ✅ | `cmd:doc.maskPaint`, `doc.maskInvert`, `doc.maskRect` + `Layer.mask`; brush softness slider in the mask panel |
| Develop layers (live RAW develop inside stack) | ✅ | `doc.addLayer kind=develop` |
| Adjustment layers (full recipe over the stack below) | ✅ | `doc.addLayer kind=adjustment` |
| Fill layers (solid/linear gradient) | ✅ | `doc.addLayer kind=fill/gradient` |
| Shape layers (SVG path, fill+stroke, dash) | ✅ | `doc.addLayer kind=shape` |
| Shape generators (rect/roundRect/ellipse/star/line) + edit/remove | ✅ | `cmd:doc.addShape`, `doc.shapeSet`, `doc.shapeRemove` |
| Text layers (font, size, tracking, leading, align, wrap) | ✅ | `doc.addLayer kind=text` |
| Group layers (isolated composite) | ✅ | `cmd:doc.group`, `doc.ungroup` + UI nesting (disclosure rows, ⌘-click multi-select, drag into/out of groups) |
| Raster layers (embedded PNG / file link) | ✅ | `doc.addLayer kind=raster/rasterFile` |
| Document model (.koubou JSON), open/save | ✅ | `doc.new`, `doc.open`, `doc.save`, `doc.json`, `doc.fromPhoto` |
| Full-res export + preview render | ✅ | `cmd:doc.render` (PNG/JPEG/TIFF, `format`+`quality` params), `doc.exportLayer` |
| PSD import (layers, blend modes, masks→flattened) | ✅ | `doc.importPsd` / `koubou-cli psd` |
| PSD export | ✅ | `cmd:doc.exportPsd` — layered writer: names/stack/offsets/visibility/opacity/27 blend keys, groups as section-divider+folder records, masks as user-mask channels (layer-space coverage, absolute rect); groups written with their real blend key (koubou composites groups isolated — `norm`, never `pass`); adjustment layers baked against the composite below, `scale` baked into pixels; `flat:true` for merged-only |
| Canvas: pan/zoom | ✅ | scroll-wheel pan, ⌘+scroll zoom about cursor, pinch zoom, spacebar drag-pan, toolbar −/+ and % menu (Fit ⌘0, 50–400%) |
| Canvas: drag-move, transform handles, marquee | ✅ | `cmd:doc.pick`, `doc.bounds` + canvas gestures (alpha-precise pick, corner scale handles, marquee → selectedSet) |
| On-canvas text editing | ✅ | double-click a text layer → in-place editor (Esc commits) |
| Undo/redo (all domains, ⌘Z in-app) | ✅ | `cmd:doc.undo`, `doc.redo`, `tl.undo`, `tl.redo`, `pg.undo`, `pg.redo` — snapshot stacks, cap 32 |
| Merge down / flatten / canvas resize+crop | ✅ | `doc.mergeDown`, `doc.flatten`, `doc.resize`, `doc.crop`, `doc.setBackdrop` |
| Layer styles | ✅ | `cmd:doc.styleSet`/`doc.styleClear`/`doc.styleScale` + `doc.setLayer styles={...}` — dropShadow, innerShadow, outerGlow, innerGlow (edge/center), bevel (5 styles, dir, altitude), satin, color/gradient/pattern overlays, stroke (out/in/center); per-effect blend+eye; inspector panel + fx badge |
| Mask shortcuts (rect fill, feather) | ✅ | `cmd:doc.maskRect` (layer-space, feather param) |
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
| MCP server (generated tools, resources, structuredContent) | ✅ | `koubou-cli mcp` — tools/list from `Session::command_specs()`; `koubou://doc|timeline|pages|commands` resources |
| macOS FFI (engine + doc dispatch) | ✅ | `araware_*` (engine), `kou_*` (docs) exports |
| Atomic multi-command batch | ✅ | `cmd:batch` — rollback restores doc state + history stacks |
| Undo-safe command log (for scripted sessions) | 🟡 | undo stacks exist; no persisted journal |
| Headless UI snapshot (app-level) | ❌ | render verification via `doc.render` instead |

## Top gaps (ordered by user impact)

1. Mask UX (thumbnail, gradient tool, mask view)
2. Boolean vector ops / SVG import-export
3. Speed ramps (rate is constant per clip today; ramps = split+rate)
4. Gradient editor for layer styles (stops UI; params only today)
5. PSD round-trip depth: layer styles as native effects, clipping masks,
   smart objects (export today keeps pixels/metadata, not editability)

## Vector / design (vectorcraft)

| feature | status | command / note |
|---|---|---|
| Shape layers with SVG `d` paths | ✅ | `doc.addLayer kind=shape` |
| Parametric generators + dash strokes | ✅ | `cmd:doc.addShape`, `doc.shapeSet`, `doc.shapeRemove` |
| Boolean ops | ❌ | — |
| Node editing (path point handles) | ✅ | `cmd:doc.shapeNodes` (read anchors), `cmd:doc.moveNode` — double-click a shape → anchor handles (circle=line, square=curve), drag to move, Esc exits |
| SVG import/export, artboards | ❌ | — |

## Motion / effects (filmcraft + effectcraft)

| feature | status | command / note |
|---|---|---|
| Timeline model (.kmotion): tracks, clips, cues | ✅ | `cmd:tl.new`, `tl.open`, `tl.save`, `tl.json`, `tl.addTrack` |
| Clip edit (in/out/offset, keyframed opacity/scale/x/y, fades) | ✅ | `tl.addClip`, `tl.setClip`, `tl.removeClip`, `tl.splitClip`, `tl.duplicateClip` |
| Track edit (name/mute/remove) | ✅ | `tl.setTrack`, `tl.removeTrack` |
| Subtitles (cues) + SRT burn-in, CJK glyphs | ✅ | `tl.addCue`, `tl.setCue`, `tl.removeCue` — fontdb CJK fallback |
| Frame preview + mp4 export via ffmpeg | ✅ | `tl.renderFrame`, `tl.render` |
| Silence detection, media probe | ✅ | `tl.detectSilence`, `tl.probe` |
| Generative clip insert (minimax-h3 endpoint) | ✅ | `tl.generateClip` |
| Speech-to-subtitle (Speech framework) | ✅ | app (SFSpeechRecognizer → `tl.addCue`) |
| Audio: per-clip volume keyframes + auto-duck under cues | ✅ | `tl.setClip volume`, `cmd:tl.duck` |
| Transitions (slide/wipe/dip per clip edge) | 🟡 | `tl.setClip transIn/transOut` — no multicam |
| Ripple ops + edge trim | ✅ | `cmd:tl.rippleDelete`, `tl.rippleInsert`, `tl.trim` + UI trim ±/Ripple Delete buttons |
| Playback rate (constant per clip) | ✅ | `tl.setClip rate` — setpts retime + chained atempo; clip keeps its span, source consumed faster/slower |
| EQ/comp per clip | ✅ | `tl.setClip eq={low,mid,high}dB`, `comp={threshold,ratio,attack,release,makeup}` → ffmpeg equalizer×3 + acompressor; inspector sliders |
| GPU realtime playback | ❌ | frame-by-frame only |

## Pages / layout (designcraft)

| feature | status | command / note |
|---|---|---|
| Multi-page doc (.kpages), masters, page size | ✅ | `cmd:pg.new`, `pg.open`, `pg.save`, `pg.addPage`, `pg.duplicatePage`, `pg.addMaster`, `pg.setMaster`, `pg.setPageSize` |
| Frames: text/image/rect/line, rotation, move across pages | ✅ | `pg.addFrame`, `pg.setFrame`, `pg.removeFrame`, `pg.moveFrame` |
| Threaded text frames (flow across frames/pages) | ✅ | `cmd:pg.linkFrames` |
| Facing-page spreads | ✅ | `cmd:pg.setSpread` |
| Paragraph styles (define/apply) | 🟡 | `cmd:pg.setStyle`, `pg.applyStyle` — no hyphenation |
| PDF export (PDF 1.4, Helvetica+JPEG+vectors) | ✅ | `pg.render` |
| PDF CJK text | ✅ | non-WinAnsi runs rasterise+embed w/ /SMask |
| Page PNG preview | ✅ | `pg.renderPng` |
| Bleed, hyphenation | ❌ | — |

## Print / PDF (printcraft)

| feature | status | command / note |
|---|---|---|
| PDF import → raster layers per page | ✅ | app Import PDF (PDFKit → `doc.addLayer kind=raster`) |
| PDF export from pages domain | ✅ | `pg.render` |
| PDF annotation forms/sign/preflight | ❌ | — |
| Imposition, color management for print | ❌ | — |

## Automation surfaces
