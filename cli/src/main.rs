use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;

use anyhow::{Context, Result};
use koubou_composer::commands::Session;
use koubou_composer::Composer;
use serde_json::{json, Value};

fn usage() -> ! {
    eprintln!(
        "usage:
  koubou-cli render <img> <out.png> [recipe.json] [max_px]
  koubou-cli thumb <img> <out.png> [max_px]
  koubou-cli reference <raw> <out.png>
  koubou-cli scan <folder>
  koubou-cli meta <file>
  koubou-cli auto <file>
  koubou-cli rate <file> <0-5>
  koubou-cli doc <file.koubou> <out.png> [max_px]
  koubou-cli psd <file.psd> <out.koubou>
  koubou-cli control [--port <N>]   JSON-lines command channel (TCP, or stdio without --port)
  koubou-cli mcp                    MCP server on stdio (tools/list, tools/call)"
    );
    std::process::exit(2);
}

fn main() -> Result<()> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        usage();
    }
    let cmd = args[1].as_str();
    match cmd {
        // ---- single-shot file ops (araware-compatible CLI surface) ----
        "render" | "thumb" | "reference" | "scan" | "meta" | "auto" | "rate" => {
            file_cmd(&args)
        }
        "doc" => {
            let inp = args.get(2).context("doc path")?;
            let out = args.get(3).map(String::as_str).unwrap_or("doc.png");
            let max: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
            let d = koubou_composer::Document::load(Path::new(inp))?;
            let mut c = Composer::new(d)?;
            let img = if max > 0 { c.render_preview(max)? } else { c.render()? };
            image::RgbaImage::from_raw(img.width, img.height, img.data)
                .context("buffer")?
                .save(out)?;
            println!("{out}");
            Ok(())
        }
        "psd" => {
            let inp = args.get(2).context("psd path")?;
            let out = args
                .get(3)
                .map(String::as_str)
                .unwrap_or("doc.koubou");
            let d = koubou_composer::psd::import_psd(Path::new(inp))?;
            d.save(Path::new(out))?;
            println!("{out} ({} layers)", d.layers.len());
            Ok(())
        }
        "control" => control(&args[2..]),
        "mcp" => mcp(),
        _ => usage(),
    }
}

fn file_cmd(args: &[String]) -> Result<()> {
    let mut s = Session::new().context("engine init")?;
    let cmd = args[1].as_str();
    let req = match cmd {
        "render" => {
            let path = args.get(2).context("img")?;
            let out = args.get(3).map(String::as_str).unwrap_or("out.png");
            let recipe_v: Value = match args.get(4) {
                Some(p) => serde_json::from_str(
                    &std::fs::read_to_string(p).context("recipe")?,
                )
                .context("recipe json")?,
                None => Value::Null,
            };
            let max: u32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);
            json!({"id": "render", "path": path, "out": out, "recipe": recipe_v, "maxPx": max})
        }
        "thumb" => json!({
            "id": "thumb",
            "path": args.get(2).context("img")?,
            "out": args.get(3).map(String::as_str).unwrap_or("thumb.png"),
            "maxPx": args.get(4).and_then(|s| s.parse().ok()).unwrap_or(512),
        }),
        "reference" => json!({
            "id": "render",
            "path": args.get(2).context("img")?,
            "out": args.get(3).map(String::as_str).unwrap_or("ref.png"),
            "recipe": {"_reference": true},
        }),
        "scan" => {
            let r = s.dispatch(&json!({"id": "scan", "folder": args.get(2).context("folder")?}));
            println!("{}", serde_json::to_string_pretty(&r["result"]).unwrap());
            return Ok(());
        }
        "meta" => {
            let r = s.dispatch(&json!({"id": "meta", "path": args.get(2).context("file")?}));
            println!("{}", serde_json::to_string_pretty(&r["result"]).unwrap());
            return Ok(());
        }
        "auto" => {
            let r = s.dispatch(&json!({"id": "auto", "path": args.get(2).context("file")?}));
            println!("{}", serde_json::to_string_pretty(&r["result"]).unwrap());
            return Ok(());
        }
        "rate" => {
            let r = s.dispatch(&json!({
                "id": "setRating",
                "path": args.get(2).context("file")?,
                "rating": args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0),
            }));
            println!("{}", serde_json::to_string_pretty(&r).unwrap());
            return Ok(());
        }
        _ => unreachable!(),
    };
    let r = s.dispatch(&req);
    if r["ok"] == true {
        println!("{}", serde_json::to_string_pretty(&r["result"]).unwrap());
        Ok(())
    } else {
        anyhow::bail!("{}", r["error"])
    }
}

// ---------- control channel ----------

fn control(args: &[String]) -> Result<()> {
    let port = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse::<u16>().ok());
    let session = std::sync::Arc::new(std::sync::Mutex::new(
        Session::new().context("engine init")?,
    ));
    match port {
        Some(port) => {
            let listener = TcpListener::bind(("127.0.0.1", port))
                .with_context(|| format!("bind 127.0.0.1:{port}"))?;
            eprintln!("[control] listening on 127.0.0.1:{port} (JSON lines)");
            for conn in listener.incoming() {
                match conn {
                    Ok(stream) => {
                        let session = std::sync::Arc::clone(&session);
                        std::thread::spawn(move || {
                            if let Err(e) = handle_conn(stream, &session) {
                                eprintln!("[control] conn error: {e:#}");
                            }
                        });
                    }
                    Err(e) => eprintln!("[control] accept: {e:#}"),
                }
            }
            Ok(())
        }
        None => {
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            for line in stdin.lock().lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let resp = handle_line(&line, &session);
                writeln!(out, "{resp}")?;
                out.flush()?;
            }
            Ok(())
        }
    }
}

fn handle_conn(
    stream: std::net::TcpStream,
    session: &std::sync::Arc<std::sync::Mutex<Session>>,
) -> Result<()> {
    let mut w = stream.try_clone()?;
    for line in BufReader::new(stream).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let resp = handle_line(&line, session);
        writeln!(w, "{resp}")?;
        w.flush()?;
    }
    Ok(())
}

fn handle_line(line: &str, session: &std::sync::Arc<std::sync::Mutex<Session>>) -> String {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return json!({"ok": false, "error": format!("bad json: {e}")}).to_string(),
    };
    session
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .dispatch(&req)
        .to_string()
}

// ---------- MCP (JSON-RPC over stdio) ----------

fn mcp() -> Result<()> {
    let session = std::sync::Mutex::new(Session::new().context("engine init")?);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let resp = match method {
            "initialize" => json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "koubou", "version": env!("CARGO_PKG_VERSION")}
                }
            }),
            "notifications/initialized" | "initialized" => continue,
            "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            "tools/list" => json!({"jsonrpc": "2.0", "id": id, "result": {"tools": mcp_tools()}}),
            "tools/call" => {
                let name = req["params"]["name"].as_str().unwrap_or("");
                let args = req["params"]["arguments"].clone();
                let mut call = args;
                call["id"] = Value::String(name.to_string());
                let r = session.lock().unwrap_or_else(|e| e.into_inner()).dispatch(&call);
                let text = serde_json::to_string_pretty(&r).unwrap();
                if r["ok"] == true {
                    json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": text}]}})
                } else {
                    json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": text}], "isError": true}})
                }
            }
            _ => {
                if id.is_null() {
                    continue;
                }
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("unknown method {method}")}})
            }
        };
        writeln!(out, "{resp}")?;
        out.flush()?;
    }
    Ok(())
}

fn mcp_tools() -> Value {
    let tool = |name: &str, desc: &str, props: Value, required: &[&str]| {
        json!({
            "name": name,
            "description": desc,
            "inputSchema": {
                "type": "object",
                "properties": props,
                "required": required,
            }
        })
    };
    let s = |d: &str| json!({"type": "string", "description": d});
    let n = |d: &str| json!({"type": "number", "description": d});
    json!([
        tool("scan", "Scan a folder for RAW+JPEG assets", json!({"folder": s("absolute folder path")}), &["folder"]),
        tool("meta", "EXIF/metadata for an image file", json!({"path": s("file path")}), &["path"]),
        tool("thumb", "Write a thumbnail PNG", json!({"path": s("file"), "out": s("output path"), "maxPx": n("max dimension")}), &["path", "out"]),
        tool("render", "Develop an image file with a recipe JSON, write PNG", json!({"path": s("file"), "recipe": {"type": "object", "description": "recipe params"}, "out": s("output"), "maxPx": n("max dimension, 0=full")}), &["path", "out"]),
        tool("auto", "Auto-analyze exposure/WB for a file", json!({"path": s("file")}), &["path"]),
        tool("sidecar.read", "Read the .koubou.json sidecar of an asset", json!({"path": s("file")}), &["path"]),
        tool("setRating", "Set 0-5 rating on an asset", json!({"path": s("file"), "rating": n("0-5")}), &["path", "rating"]),
        tool("setLabel", "Set a colour label on an asset", json!({"path": s("file"), "label": s("label")}), &["path"]),
        tool("doc.new", "Create a new empty document", json!({"name": s("name"), "w": n("px"), "h": n("px")}), &[]),
        tool("doc.fromPhoto", "Create a document whose base layer develops a photo", json!({"path": s("image file")}), &["path"]),
        tool("doc.open", "Open a .koubou document", json!({"path": s("doc path")}), &["path"]),
        tool("doc.importPsd", "Import a layered PSD as the open document", json!({"path": s("psd path")}), &["path"]),
        tool("doc.json", "Return the open document's JSON state", json!({}), &[]),
        tool("doc.addLayer", "Add a layer (kind: fill/gradient/adjustment/text/shape/develop/rasterFile/raster/group)", json!({"kind": s("layer kind"), "name": s("layer name")}), &["kind"]),
        tool("doc.setLayer", "Edit a layer (layer=id + name/visible/opacity/blend/x/y/scale/recipe/text)", json!({"layer": n("layer id")}), &["layer"]),
        tool("doc.removeLayer", "Remove a layer", json!({"layer": n("layer id")}), &["layer"]),
        tool("doc.reorder", "Move a layer in the stack (0 = bottom)", json!({"layer": n("layer id"), "to": n("index")}), &["layer", "to"]),
        tool("doc.maskPaint", "Paint a soft dab into a layer mask", json!({"layer": n("layer id"), "cx": n("x"), "cy": n("y"), "r": n("radius px"), "value": n("0-1 coverage"), "softness": n("0-1")}), &["layer", "cx", "cy"]),
        tool("doc.render", "Composite the open document to PNG", json!({"out": s("output path"), "maxPx": n("preview size, 0=full")}), &["out"]),
        tool("doc.save", "Save the open document as .koubou", json!({"path": s("doc path")}), &["path"]),
    ])
}
