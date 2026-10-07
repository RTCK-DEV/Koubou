# Koubou — instructions for agents

All-in-one macOS photo app: Lightroom-style library + RAW develop plus a
Photoshop-style layered document engine (`koubou-composer`). MIT. LibRaw
is dynamically linked; no GPL code in the tree.

The RAW engine is **`araware-core`, consumed as a git dependency** tracking
araware's `devin/initial-implementation` branch — never vendor or fork
engine code here. Engine bugs/features are fixed in araware and pulled in
via `cargo update -p araware-core`; only Koubou-specific glue (documents,
compositing, FFI, UI) lives in this tree.

## Start every session here

1. Read `ROADMAP.md` → *Where we stand* and *Where we're going*. The
   checklist counts features that exist; the real gaps are quality and
   polish. A ✅ row is not proof of parity — if you find a ✅ feature that
   is wrong or shallow, downgrade it to 🟡 with a note in the same commit.
2. Pick work from `docs/parity.md` → *Top gaps* (ordered by user impact).
   When you land a feature, update its rows and the gap list in the same
   commit, and the ROADMAP sections when a listed gap closes.
3. Run `scripts/parity_check.sh` before committing — it verifies every
   `cmd:` id cited in `docs/parity.md` exists in `Session::command_ids`.

## Never crash (outranks feature work)

People trust the app with their photo libraries and edits. A malformed
RAW/JPEG/PSD/.koubou, a bad command argument, or a corrupt catalog must
produce an actionable error, never a panic.

- Non-test Rust: avoid `unwrap`/`expect`/`panic!`/`unreachable!`/`todo!` —
  return errors through `anyhow`/`Result`. FFI code (`capi.rs`) must never
  let a panic cross the boundary: `catch_unwind` at the API edge.
- `Session::dispatch` already wraps `run()` in `catch_unwind` — keep it
  that way; it's a safety net, not a licence for sloppy code.
- Input-derived numbers are hostile: `get()` not `[i]`, checked/saturating
  math for offsets and counts, cap allocations sized by input.
- Every crash fix lands with a small regression test that panicked before
  the fix.

## Everything is a command

All engine and document operations are dispatch ids (`docs/control-protocol.md`).
UI, CLI, control channel and MCP all go through `Session::dispatch`. When
you add a capability, expose it as a command id + params, add it to
`command_ids`, the CLI tool schemas (`mcp_tools`), and `docs/parity.md`'s
`cmd:` citations — not just to a Swift view.

## The layer-id protocol quirk

`"id"` is the command name. Layer-targeting commands take `"layer"` —
never `"id"` — for the layer id (`doc.setLayer`, `doc.removeLayer`,
`doc.reorder`, `doc.maskPaint`). Keep Swift dispatchers and tool schemas
consistent with this.

## Conventions

- **No Adobe assets** — no icons, presets, profiles (DCP/LCP), fonts, or
  UI bitmaps. Layout/behaviour imitation is fine; copying assets is not.
- **No GPL code** — no darktable/RawTherapee-derived code; LibRaw stays a
  dynamically-linked library (build.sh bundles the dylib).
- Blend modes follow the PDF/W3C spec exactly (`composer::blend`); serde
  keys are camelCase (`"colorBurn"`), `BlendMode::parse` accepts PSD/friendly
  names.
- Recipe fields are normalised (image-independent); documents store
  recipes verbatim — resolution independence is a hard requirement.
- Adjustments reuse the develop recipe engine; geometry fields (crop,
  rotate, keystone) are sanitized to no-ops on `adjustment` layers.

## Quality gates

- `cargo test --workspace` green; `cargo fmt` applied; no new warnings.
- `./mac/build.sh` must produce a runnable `mac/build/koubou.app`.
- After user-visible changes, run the app and look at it (screenshots).
- Cache invalidation: any command that changes layer *content* bumps
  `l.gen` — forgetting it leaves stale pixels in the compositor cache.

## Map of the code

- engine (external): `araware-core` git dep — `decode` (LibRaw+raster) →
  `develop` (recipe ops) → `engine` (scan/render/thumb/meta/sidecars/auto)
  → `araware_*` C FFI. Sidecars are `<stem>.araware.json` (shared with
  araware); env: `ARA_DISABLE_GPU`, `ARAWARE_HOME`.
- `composer/`: `doc` (model+serde), `composite` (layer stack renderer),
  `blend` (27 modes), `shape` (SVG path → tiny-skia + generators + dash),
  `text` (fontdue + fontdb), `psd` (import), `commands` (dispatch — routes
  `tl.*`→motion, `pg.*`→pages), `capi` (`kou_*` FFI)
- `motion/`: `.kmotion` timeline (tracks/clips/keyframes/cues) + ffmpeg
  render + silencedetect + minimax-h3 `tl.generateClip`
- `pages/`: `.kpages` multi-page layout + hand-rolled PDF 1.4 writer
- `cli/`: `main.rs` — file verbs, `control` (TCP/stdio), `mcp`
- `mac/`: `KoubouApp`+`StudioView` (mode switch: Library|Layers|Motion|
  Pages), `LibraryView` (grid), `EditorView` (single-photo develop),
  `DocEditorView` (layers workspace + vector generators + PDF import via
  PDFKit), `MotionView` (timeline + Speech transcription + h3ui generate),
  `PagesView` (page thumbnails + frame inspector), `Engine.swift`
  (`EngineSession`, `DocSession` FFI wrappers)
