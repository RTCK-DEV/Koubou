# Koubou roadmap

Two honest ledgers, like the craft-suite docs that inspired this format:

- **The checklist** counts features that *exist* and pass their tests.
- **Where we stand** is how far Koubou actually replaces Lightroom /
  Photoshop in real work today. A ✅ row is not proof of parity — if a
  "done" feature turns out broken or shallow, downgrade it to 🟡 here and
  in `docs/parity.md` in the same commit.

## Where we stand

**vs Lightroom** — library + develop, ~85% capable for a hobbyist workflow:

- ✅ RAW decode breadth (LibRaw: CR3, NEF, ARW, DNG, RAF, …), embedded-JPEG
  fallback, pairs handling, folders + rating/label/sidecars
- ✅ Deep develop recipe (~60 params: tone, WB, curves incl. per-channel and
  five secondary curves, lift/gamma/gain + zones, qualifier, power windows,
  retouch spots/clone/dodge-burn, LUT, grain/vignette/glow, auto analysis)
- ✅ CPU reference pipeline + wgpu path + live scopes
- 🟡 No collections/smart collections, no keywording/map/book/print modules,
  no cloud sync, no HDR merge/pano
- 🟡 Camera colour = LibRaw's matrices; no per-model DCP calibration beyond it

**vs Photoshop** — layered editing, ~45%: the *core* model is here, the
polish isn't:

- ✅ Layers (raster/develop/adjustment/fill/shape/text/group), masks with
  feather, 27 blend modes (PDF/W3C), opacity/transform, PSD *import*,
  flat PSD/JPEG/TIFF/PNG export
- ✅ Doc inspector exposes the full recipe: light/wheels/curves/zones/
  qualifier/windows/mixer/detail/fx palettes — same UI as develop
- ✅ Undo/redo everywhere: per-domain snapshot stacks (cap 32) behind
  `doc.undo`/`tl.undo`/`pg.undo` (+`.redo`), wired to ⌘Z/⇧⌘Z in the app's
  Edit menu; `batch` runs atomic multi-command transactions
- ✅ Canvas direct manipulation: alpha-precise pick, drag-move with live
  outline, corner scale handles, marquee → multi-select, double-click
  on-canvas text editing (overlay windows painted via WindowServer ops
  during event tracking)
- ✅ Groups: disclosure nesting, ⌘-click multi-select, group/ungroup,
  drag reorder + reparent in the layers panel (`doc.moveLayer`)
- ✅ Canvas pan/zoom: scroll-wheel pan, ⌘+scroll zoom about cursor, pinch
  zoom, spacebar drag-pan, toolbar −/+ and % menu (⌘0/⌘=/⌘-)
- ✅ Vector node editing: double-click a shape → anchor handles
  (line=circle, curve=square), drag to move via `doc.shapeNodes`/
  `doc.moveNode`, Esc exits
- ✅ Mask UX improved: Invert chip + brush softness slider in the mask
  panel (`doc.maskInvert`, `softness` on `doc.maskPaint`)
- ✅ Layer styles: full PS set (shadows, glows, bevel/emboss, satin,
  color/gradient/pattern overlays, stroke) via `doc.styleSet` + inspector;
  no layered PSD export (flat only)
- ❌ Smart objects, channel ops, filter gallery, liquify, text-on-path

**vs the storytold craft suite** — one app now spans all seven domains:

- ✅ Beats on imaging: LibRaw decode breadth and camera colour where
  lightcraft falls back to embedded JPEGs and uncalibrated matrices
- ✅ Beats on unity: library + develop + layers + vector + motion + pages +
  PDF in one session/document protocol — the craft suite splits them
  across apps that can't share a file format or a session
- ✅ Motion: .kmotion timeline (tracks/clips/cues/keyframes/fades) with
  ffmpeg render, silence detection, Speech-framework transcription, and
  minimax-h3 generative clip inserts — filmcraft+effectcraft in one
- ✅ Pages: .kpages layout doc with frames/masters and a hand-rolled
  PDF 1.4 writer — designcraft's core; PDF *import* (PDFKit → layers)
  covers printcraft's read path
- ✅ Vector: parametric shape generators + dashed strokes on shape layers
- ✅ Transitions: per-edge slide/wipe/dip on clips (`transIn`/`transOut`);
  audio: per-clip volume keyframes + `tl.duck` auto-ducking under cues;
  CJK subtitle glyphs via fontdb fallback
- ✅ Pages depth: threaded text frames (`pg.linkFrames`), facing spreads
  (`pg.setSpread`), paragraph styles (`pg.setStyle`/`pg.applyStyle`),
  page size (`pg.setPageSize`), CJK-safe PDF text (rasterize+SMask)
- 🟡 Plumbing is catching up: per-domain undo stacks + Edit-menu ⌘Z are
  in, and every command now carries a generated MCP tool spec (single
  registry, no schema drift) + atomic `batch`; still no persisted command
  journal or xtask-style CI harness
- ✅ Motion edit depth: ripple ops (`tl.rippleDelete`/`tl.rippleInsert`),
  edge trims (`tl.trim`), constant playback rate (`rate` → setpts retime +
  chained atempo), per-clip 3-band EQ + compressor (`eq`/`comp` → ffmpeg
  equalizer/acompressor) — all with inspector UI
- ❌ No path boolean ops, no PDF annotation/preflight, no speed ramps or
  GPU realtime playback — the deep ends of each domain

## Where we're going (ordered)

1. **Mask UX** — mask thumbnail, gradient/brush tools, mask-from-qualifier
2. **Layered PSD export** — a real writer that keeps layers, not the
   current flat composite
3. **Boolean vector ops + SVG import/export**
4. **Speed ramps** — constant rate landed (`tl.setClip rate`); ramps
   = split + per-clip rate until keyframed rate exists
5. **Real playback in Motion** — GPU realtime instead of frame stepping
6. **Gradient editor for layer styles** — stops UI; params only today
7. **xtask CI** — fmt/clippy/layers/parity in one command (today:
   `scripts/parity_check.sh` + `cargo test --workspace`)
8. **Batch ops** — scan folder → apply recipe → export, via the CLI/MCP

## Engineering notes

- `develop` and `adjustment` layers run the same recipe engine — an
  adjustment layer is a live grade over everything below it, rasterized
  through the rgba16 CPU path. Geometry recipe fields (crop, rotate,
  keystone) are sanitized on adjustment layers.
- The compositor renders in sRGB-encoded u8 with per-pixel blend functions
  from the PDF/W3C spec; HSL modes are non-separable and handled in the
  same alpha framework. `Dissolve` currently falls back to normal blend —
  stochastic alpha jitter is not implemented (flagged 🟡 in parity).
- Layer pixels are cached by (id, gen); any content edit bumps gen.
