# Control protocol

Every operation in Koubou is a JSON command dispatched by `"id"`. One
dispatcher — `composer::commands::Session` — serves the macOS app, the CLI,
the TCP control channel and the MCP server.

## Transports

```sh
koubou-cli control              # JSON lines on stdin/stdout
koubou-cli control --port 7980  # JSON lines over TCP (one line = one request)
koubou-cli mcp                  # MCP tools/list + tools/call on stdio
```

The app dispatches through the same table via `kou_dispatch()` (see
`mac/koubou.h`, `composer/src/capi.rs`).

## Envelope

Request:  `{"id": "<command>", ...params}`  — one JSON object per line.
Response: `{"ok": true, "result": ...}` or `{"ok": false, "error": "..."}`.
A command that panics returns `{"ok": false, "error": "... panicked"}` —
the session stays alive (never-crash rule).

## Command reference

### Engine / library

| id | params | returns |
|---|---|---|
| `ping` | — | `{"name","version"}` |
| `commands` | — | list of all command ids |
| `scan` | `folder` | asset entries `[{path,name,kind,…}]` |
| `meta` | `path` | EXIF/make/model/width/height |
| `thumb` | `path`, `maxPx` (512), `out`? | PNG file or `{"pngB64"}` |
| `render` | `path`, `recipe`, `maxPx`, `out`? | PNG file or `{"pngB64"}` |
| `auto` | `path` | auto-analysis suggestions |
| `sidecar.read` | `path` | sidecar JSON |
| `sidecar.write` | `path`, `json` | `"ok"` |
| `setRating` | `path`, `rating` 0–5 | `"ok"` |
| `setLabel` | `path`, `label` | `"ok"` |

Images: pass `out` to write a PNG file; omit it for `{"pngB64","w","h"}`.
`recipe` may be an object or a JSON string; `{}`/absent = neutral recipe.

### Documents

| id | params | returns |
|---|---|---|
| `doc.new` | `w`, `h`, `name`? | `{"w","h"}` |
| `doc.fromPhoto` | `path` | `{"w","h"}` — base layer = live develop |
| `doc.open` | `path` (.koubou) | `"ok"` |
| `doc.importPsd` | `path` (.psd) | `"ok"` |
| `doc.save` | `path`? | `{"path"}` |
| `doc.json` | — | full document state |
| `doc.render` | `maxPx`, `out`? | PNG file or `{"pngB64"}` |
| `doc.addLayer` | `kind` + per-kind params | `{"layerId"}` |
| `doc.setLayer` | `layer` + props to change | `"ok"` |
| `doc.removeLayer` | `layer` | `"ok"` |
| `doc.reorder` | `layer`, `to` (0 = bottom) | `"ok"` |
| `doc.maskPaint` | `layer`, `cx`, `cy`, `r`, `value`, `softness` | `"ok"` |

Note the layer-id param name is **`layer`** — `"id"` is the command name.

### `doc.addLayer` kinds

| kind | params |
|---|---|
| `develop` | `path`, `recipe`? — live RAW layer |
| `rasterFile` | `path` — links a PNG/JPEG/TIFF file |
| `raster` | `w`, `h`, `rgbaB64` — embeds pixels |
| `adjustment` | `recipe`? — recipe applied to the stack below |
| `fill` | `color` [r,g,b,a] |
| `gradient` | `line` [x0,y0,x1,y1], `stops` [[pos,r,g,b,a]…] |
| `shape` | `shapes` [{`d` SVG-path, `fill`?, `stroke`?}] |
| `text` | `text` {text, font, size, tracking, leading, align, color, wrapWidth, bold, italic} |
| `group` | — (children nest via engine; UI can't yet) |

### `doc.setLayer` props

`name`, `visible`, `opacity` 0–1, `blend` (27 modes — `normal`, `multiply`,
`screen`, `overlay`, `softLight`, `colorDodge`, `hue`, `color`, …),
`x`, `y`, `scale`, `mask` (null clears), `recipe` (develop/adjustment),
`text` (text layers), `fill` (fill layers), `shapes` (shape layers).

## Example session

```jsonl
{"id":"doc.fromPhoto","path":"/pics/IMG_0001.ARW"}
{"id":"doc.addLayer","kind":"adjustment","name":"grade","recipe":{"contrast":0.3,"vibrance":0.4}}
{"id":"doc.addLayer","kind":"shape","name":"vig","shapes":[{"d":"M 0 0 L 3000 0 L 3000 2000 L 0 2000 Z","fill":[0,0,0,0.4]}]}
{"id":"doc.setLayer","layer":3,"blend":"overlay"}
{"id":"doc.maskPaint","layer":2,"cx":1500,"cy":1000,"r":400,"value":0,"softness":0.7}
{"id":"doc.render","out":"/tmp/final.png"}
{"id":"doc.save","path":"/pics/final.koubou"}
```

## Errors

All failures are `{"ok":false,"error":"…"}` with a human-readable chain
(`context` messages from the engine: missing file, bad recipe, unknown
layer, unsupported kind). Unknown `id` → `unknown command id: …`.

## Motion domain (`tl.*`) — .kmotion timelines

The same dispatch, routed to the timeline session. Envelope identical.

| id | params |
|---|---|
| `tl.new` | `w`, `h`, `fps`, `name`? |
| `tl.open` / `tl.save` | `path` |
| `tl.json` | — |
| `tl.addTrack` | `kind` video\|audio\|subtitle |
| `tl.addClip` | `track`, `src`, `in`?, `out`?, `offset`? |
| `tl.setClip` | `clip` + `in`/`out`/`offset`/`opacity`/`scale`/`fadeIn`/`fadeOut` |
| `tl.removeClip` | `clip` |
| `tl.addCue` | `t`, `dur`, `text` |
| `tl.probe` | `path` — ffprobe JSON |
| `tl.renderFrame` | `t` → `{"pngB64":…}` or `out` |
| `tl.render` | `out` — mp4 via ffmpeg |
| `tl.detectSilence` | `path` → `{"ranges":[{start,end}]}` |
| `tl.generateClip` | `endpoint`, `prompt` — minimax-h3/h3ui |

## Pages domain (`pg.*`) — .kpages layouts

| id | params |
|---|---|
| `pg.new` | `name`, `pageW`, `pageH`, `margins`? |
| `pg.open` / `pg.save` | `path` |
| `pg.json` | — |
| `pg.addPage` / `pg.removePage` | `page`? |
| `pg.setMaster` | `page`, master name |
| `pg.addFrame` | `page`, `kind` text\|image\|rect\|line + `x,y,w,h` (pt) |
| `pg.setFrame` / `pg.removeFrame` | `page`, `frame` |
| `pg.render` | `out` — PDF 1.4 |
| `pg.renderPng` | `page`, `out`/`pngB64`, `maxPx`? |
