# Koubou (工房)

An all-in-one photo application for macOS: a RAW developer and a layered
image editor in a single app — the Lightroom+Photoshop workflow without
round-tripping between two programs.

Koubou grows out of [araware](https://github.com/RTCK-DEV/araware) (same
LibRaw-based decode + develop engine, MIT) and adds what a single-photo
developer can't do: **layered `.koubou` documents** — live RAW develop
layers, raster layers, adjustment layers, 27 Photoshop blend modes, layer
masks, vector shapes, text, groups, and PSD import.

Every operation in the engine is a **command** dispatched by JSON id. The
macOS app, the CLI, a TCP **control channel** and an **MCP server** all run
the same dispatcher — so an agent (or a test harness, or another app) can
drive the whole editor without touching the UI.

## What's in the box

| crate | what it does |
|---|---|
| engine (`araware-core`, git dep) | LibRaw decode → demosaic → CPU/wgpu develop pipeline (~60-param recipe), catalog scan + SQLite + JSON sidecars, auto-analysis, C FFI — **lives in [araware](https://github.com/RTCK-DEV/araware)**; `cargo update -p araware-core` pulls new engine work |
| `composer` (`koubou-composer`) | `.koubou` document model, layer compositor (blend modes, masks, feather, groups), SVG-path shapes, fontdue text, PSD import, command dispatcher, FFI |
| `cli` (`koubou-cli`) | headless render/thumb/scan/meta, `.koubou` ↔ render, `control` JSON-lines server (TCP or stdio), `mcp` server |
| `mac/` | Koubou.app — SwiftUI library + develop editor + layered document editor |

## Build

```sh
brew install libraw rustup-init && rustup-init -y --default-toolchain stable
cargo build --release        # core + composer + cli
./mac/build.sh               # → mac/build/koubou.app (self-contained bundle)
```

## Drive it headlessly

```sh
# JSON-lines control channel (TCP on :7980, or stdio without --port)
target/release/koubou-cli control --port 7980

# MCP server on stdio — register in your agent's mcp_servers config
target/release/koubou-cli mcp

# one-shots
target/release/koubou-cli render IMG_1234.ARW out.png recipe.json
target/release/koubou-cli doc    edit.koubou out.png
target/release/koubou-cli psd    file.psd out.koubou
```

```jsonc
// control channel: build a layered doc, no UI involved
{"id": "doc.fromPhoto", "path": "/pics/IMG_1234.ARW"}
{"id": "doc.addLayer", "kind": "adjustment", "name": "warm",
 "recipe": {"temperature": 0.4, "exposure": 0.3}}
{"id": "doc.addLayer", "kind": "text",
 "text": {"text": "TITLE", "size": 140, "color": [1, 1, 1, 0.9]},
 "x": 100, "y": 1600}
{"id": "doc.setLayer", "layer": 2, "blend": "softLight"}
{"id": "doc.render", "out": "/tmp/out.png"}
```

See `docs/control-protocol.md`, `docs/mcp.md`.

## Honest status

This is a young app with a real engine, not a demo. The develop pipeline is
production-tested (it ships in araware); the layer system is new. Read
[`ROADMAP.md`](ROADMAP.md) — *Where we stand* — for the truthful gap list,
and [`docs/parity.md`](docs/parity.md) for the per-feature tracker.

## Layout

```
engine      araware-core from github.com/RTCK-DEV/araware (decode, develop, catalog, FFI) — not vendored here
composer/   doc model, compositor, shapes, text, psd, commands, FFI
cli/        koubou-cli (render, control, mcp)
mac/        Koubou.app (SwiftUI) + build.sh + koubou.h
docs/       control-protocol, mcp, parity
scripts/    parity_check.sh (docs ↔ command-table consistency)
```

MIT (see LICENSE). LibRaw is dynamically linked (LGPL preserved); no GPL
code in the tree.
