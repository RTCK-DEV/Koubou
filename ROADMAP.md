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

**vs Photoshop** — layered editing, ~35%: the *core* model is here, the
polish isn't:

- ✅ Layers (raster/develop/adjustment/fill/shape/text/group), masks with
  feather, 27 blend modes (PDF/W3C), opacity/transform, PSD *import*
- 🟡 Adjustment layers run the full develop recipe — but the doc editor UI
  only exposes a slider subset (curves/wheels/qualifier editable via
  commands, not yet the app's inspector)
- 🟡 No undo/redo in the doc editor yet (session store + doc.json diffing
  planned); the single-photo editor has recipe undo already
- 🟡 Canvas is view+paint only: no drag-move, no marquee/lasso, no on-canvas
  text editing — placement is numeric (x/y/scale) for now
- ❌ No PSD *export*, smart objects, layer styles (drop shadow/stroke),
  channel ops, filter gallery, liquify, text-on-path
- ❌ Groups composite correctly but the UI can't nest/drag layers into them yet

**vs the storytold craft suite** — what this beats / what it doesn't:

- ✅ Beats on imaging: LibRaw decode breadth and camera colour where
  lightcraft falls back to embedded JPEGs and uncalibrated matrices
- ✅ Beats on unity: Lightroom+Photoshop in one document model — the craft
  suite splits them across apps that can't share a file format
- 🟡 Behind on plumbing maturity: they have undo stacks, menus, shortcut
  maps and a full xtask CI harness; Koubou has the command core and the
  tracker, not yet the polish

## Where we're going (ordered)

1. **Undo/redo for documents** — command-level snapshot history in the
   session (bounded by memory), exposed as `doc.undo`/`doc.redo`
2. **Doc inspector parity** — curves, wheels, qualifier and power windows
   in the layer inspector (the recipe engine already does all of it)
3. **Canvas manipulation** — drag-move layers, marquee select, on-canvas
   text edit, transform handles
4. **Mask UX** — mask thumbnail, gradient/brush tools, mask-from-qualifier
5. **Export breadth** — JPEG/TIFF/WebP export with size/quality options;
   PSD export if a sane writer exists
6. **Groups in UI** — nesting, expand/collapse, drag between levels
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
