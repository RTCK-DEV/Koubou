# MCP server

`koubou-cli mcp` speaks JSON-RPC 2.0 on stdio and exposes every Koubou
command as an MCP tool. Any MCP-capable agent can drive the full engine —
scan a folder, develop RAWs, build layered documents — with no UI involved.

## Wiring it up

```jsonc
// agent config (e.g. Devin MCP servers, Claude Desktop mcpServers)
{
  "command": "/path/to/koubou-cli",
  "args": ["mcp"]
}
```

The process is one-shot-per-session: one `Session` lives for the lifetime
of the stdio stream, so document state persists across calls.

## Handshake

```jsonc
→ {"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"x","version":"1"}}}
← {"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"koubou","version":"0.1.0"}}}
→ {"jsonrpc":"2.0","method":"notifications/initialized"}
→ {"jsonrpc":"2.0","id":1,"method":"tools/list"}
→ {"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"scan","arguments":{"folder":"/pics"}}}
```

Tool results are the command's `result` JSON serialized into
`content[0].text` (PNG renders also include an `image` content block when
the client asks for a binary-bearing tool — `thumb`, `render`,
`doc.render` with no `out` path).

## Tools (mirrors `docs/control-protocol.md`)

Engine: `scan`, `meta`, `thumb`, `render`, `auto`, `sidecar.read`,
`sidecar.write`, `setRating`, `setLabel`.
Documents: `doc.new`, `doc.fromPhoto`, `doc.open`, `doc.save`, `doc.json`,
`doc.importPsd`, `doc.addLayer`, `doc.setLayer`, `doc.removeLayer`,
`doc.reorder`, `doc.render`, `doc.maskPaint`.

A tool call is a straight pass-through: `arguments` becomes the command
object minus `"id"` — so `doc.setLayer` takes `"layer": 2`, never `"id"`.

## Failure mode

Unknown tool name → JSON-RPC `-32601`. A command error comes back as a
successful JSON-RPC response whose tool text starts with `error:` — agents
should treat that as a normal command failure, not a protocol failure.
