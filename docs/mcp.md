# MCP server

`koubou-cli mcp` speaks JSON-RPC 2.0 on stdio and exposes **every** Koubou
command — engine, documents, timeline, pages — as an MCP tool. Any
MCP-capable agent can drive the whole studio with no UI involved.

## Wiring it up

```jsonc
// agent config (e.g. Devin MCP servers, Claude Desktop mcpServers)
{
  "command": "/path/to/koubou-cli",
  "args": ["mcp"]
}
```

The process is one-shot-per-session: one `Session` lives for the lifetime
of the stdio stream, so document/timeline/pages state persists across
calls.

## Command registry — single source of truth

Tool schemas are **generated**, not hand-written: `tools/list` is built
from `Session::command_specs()`, the same registry the `commands` tool
returns. Adding a command id + spec automatically adds an MCP tool — the
list can never drift from the dispatcher (a test asserts the spec set
equals `command_ids()`).

MCP tool names can't contain `.`, so each spec carries a `name` field —
the command id with dots replaced by underscores (`doc.setLayer` →
`doc_setLayer`). `tools/call` maps the name back to the real command id
through the registry, so names are unambiguous.

## Handshake

```jsonc
→ {"jsonrpc":"2.0","id":0,"method":"initialize","params":{...}}
← {"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{},"resources":{}},"serverInfo":{"name":"koubou","version":"0.1.0"}}}
→ {"jsonrpc":"2.0","method":"notifications/initialized"}
→ {"jsonrpc":"2.0","id":1,"method":"tools/list"}        // ~60 generated tools
→ {"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"doc_setLayer","arguments":{"layer":2,"opacity":0.5}}}
```

## Results

A tool result returns the command envelope in `content[0].text`
(pretty-printed) plus the unwrapped `result` value in
`structuredContent` — agents that understand structured results can skip
the text parse. Failed commands come back with `isError: true` and the
envelope's `error` string in the text.

## Resources — live session state

The open documents are readable as MCP resources, all
`application/json`:

| uri | content |
|---|---|
| `koubou://doc` | current `.koubou` document (`doc.json`) |
| `koubou://timeline` | current `.kmotion` timeline (`tl.json`, incl. computed `duration`) |
| `koubou://pages` | current `.kpages` layout (`pg.json`) |
| `koubou://commands` | the command registry: every id + tool spec |

Reading a resource for a domain with nothing open returns that command's
error text (`"no document"` etc.) — not a protocol error.

## Tools

Every command in `docs/control-protocol.md` is a tool — engine (`scan`,
`meta`, `thumb`, `render`, `auto`, `sidecar.*`, `setRating`, `setLabel`),
documents (`doc_*` incl. `doc_undo`/`doc_redo`/`doc_mergeDown`/
`doc_flatten`/`doc_exportLayer`/`doc_maskInvert`), timeline (`tl_*` incl.
`tl_splitClip`/`tl_duplicateClip`/`tl_setCue`/`tl_undo`), pages
(`pg_*` incl. `pg_duplicatePage`/`pg_moveFrame`), plus `batch` for atomic
multi-command runs.

A tool call is a straight pass-through: `arguments` becomes the command
object minus `"id"` — so `doc.setLayer` takes `"layer": 2`, never `"id"`.

## Failure mode

Unknown tool name → JSON-RPC `-32602`. Unknown resource uri → `-32602`.
A command error comes back as `isError: true` on the tool result — treat
it as a normal command failure, not a protocol failure.
