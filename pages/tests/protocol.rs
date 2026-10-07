//! End-to-end protocol tests: drive PgSession with the same JSON the control
//! channel would send, then validate the emitted PDF with an external tool
//! (mutool / pdfinfo / qpdf / sips — whichever is installed; skipped if none).

use std::path::PathBuf;
use std::process::Command;

use koubou_pages::PgSession;
use serde_json::{json, Value};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kpages_test_{}_{}", std::process::id(), name));
    let _ = std::fs::create_dir_all(&d);
    d.join(name)
}

fn make_png(path: &PathBuf) {
    // 40x20 gradient — deterministic, non-trivial bytes
    let mut img = image::RgbaImage::new(40, 20);
    for y in 0..20u32 {
        for x in 0..40u32 {
            img.put_pixel(x, y, image::Rgba([(x * 6) as u8, (y * 12) as u8, 128, 255]));
        }
    }
    img.save(path).unwrap();
}

fn dispatch(s: &mut PgSession, v: Value) -> Value {
    let id = v["id"].as_str().unwrap().to_string();
    match s.dispatch(&id, &v) {
        Ok(r) => r,
        Err(e) => panic!("{id} failed: {e:#}"),
    }
}

#[test]
fn full_protocol_flow() {
    let mut s = PgSession::new();
    assert!(PgSession::command_ids().contains(&"pg.render"));

    let png = tmp("img.png");
    make_png(&png);
    let pdf_path = tmp("out.pdf");
    let png_path = tmp("page0.png");
    let save_path = tmp("doc.kpages");

    dispatch(&mut s, json!({"id":"pg.new","w":300,"h":400,"name":"Spec"}));
    let r = dispatch(&mut s, json!({"id":"pg.addMaster","name":"A"}));
    let master = r["master"].as_u64().unwrap();

    for _ in 0..2 {
        dispatch(&mut s, json!({"id":"pg.addPage"}));
    }
    dispatch(
        &mut s,
        json!({"id":"pg.setMaster","page":0,"master":master}),
    );

    // master gets a folio line; page 0 gets text + rect + image
    dispatch(
        &mut s,
        json!({"id":"pg.addFrame","master":master,"kind":"line",
               "x":20,"y":390,"w":260,"h":8,"x2":260,"y2":0,
               "stroke":{"color":[0,0,0,1],"width":1}}),
    );
    let t = dispatch(
        &mut s,
        json!({"id":"pg.addFrame","page":0,"kind":"text",
               "x":20,"y":20,"w":200,"h":60,
               "text":"hello koubou pages, this text should wrap inside the frame",
               "font":"Helvetica","size":14,"align":"left","leading":1.3}),
    );
    let text_id = t["frame"].as_u64().unwrap();
    dispatch(
        &mut s,
        json!({"id":"pg.addFrame","page":0,"kind":"rect",
               "x":20,"y":100,"w":100,"h":50,"rotationDeg":15,
               "fill":[0.9,0.2,0.1,1.0],"stroke":{"color":[0,0,0,1],"width":2}}),
    );
    dispatch(
        &mut s,
        json!({"id":"pg.addFrame","page":0,"kind":"image",
               "x":20,"y":160,"w":120,"h":80,"path":png.to_string_lossy(),"fit":"fit"}),
    );

    // setFrame edits geometry + kind fields
    dispatch(
        &mut s,
        json!({"id":"pg.setFrame","frame":text_id,"x":24,"size":16,"align":"center","z":5}),
    );

    // serde round-trip through pg.json + pg.open
    let doc = dispatch(&mut s, json!({"id":"pg.json"}));
    assert_eq!(doc["pages"].as_array().unwrap().len(), 2);
    let r = dispatch(
        &mut s,
        json!({"id":"pg.save","path":save_path.to_string_lossy()}),
    );
    assert!(save_path.exists(), "save wrote {}", r);
    let r = dispatch(
        &mut s,
        json!({"id":"pg.open","path":save_path.to_string_lossy()}),
    );
    assert_eq!(r["pages"], 2);
    assert_eq!(r["masters"], 1);

    // PNG preview: nonzero and contains non-white pixels
    let r = dispatch(
        &mut s,
        json!({"id":"pg.renderPng","page":0,"out":png_path.to_string_lossy(),"dpi":96}),
    );
    assert_eq!(r["w"], 400); // 300pt @96dpi
    let img = image::open(&png_path).unwrap().to_rgba8();
    let nonwhite = img.pixels().filter(|p| p.0[..3] != [255, 255, 255]).count();
    assert!(nonwhite > 1000, "page rendered nearly blank");

    // PDF: magic, one /Type /Page per page, externally openable
    let r = dispatch(
        &mut s,
        json!({"id":"pg.render","out":pdf_path.to_string_lossy()}),
    );
    assert_eq!(r["pages"], 2);
    let bytes = std::fs::read(&pdf_path).unwrap();
    assert!(bytes.starts_with(b"%PDF-"));
    let s_pdf = String::from_utf8_lossy(&bytes);
    assert_eq!(s_pdf.matches("/Type /Page").count() - 1, 2); // minus the /Pages node
    external_pdf_check(&pdf_path);

    // removeFrame / removePage
    dispatch(&mut s, json!({"id":"pg.removeFrame","frame":text_id}));
    dispatch(&mut s, json!({"id":"pg.removePage","page":1}));
    let doc = dispatch(&mut s, json!({"id":"pg.json"}));
    assert_eq!(doc["pages"].as_array().unwrap().len(), 1);
}

/// open the PDF with whichever validator is installed; skip when none exist
fn external_pdf_check(path: &PathBuf) {
    let p = path.to_string_lossy().to_string();
    let attempts: &[(&str, Vec<&str>)] = &[
        ("mutool", vec!["info", &p]),
        ("pdfinfo", vec![&p]),
        ("qpdf", vec!["--check", &p]),
        ("sips", vec!["-g", "pixelWidth", &p]),
    ];
    for (tool, args) in attempts {
        if Command::new(tool).arg("--version").output().is_err() && !which(tool) {
            continue;
        }
        let out = Command::new(tool)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("{tool} spawn: {e}"));
        assert!(
            out.status.success(),
            "{tool} rejected {}:\n{}{}",
            p,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    eprintln!("no pdf validator installed; skipping external check");
}

fn which(tool: &str) -> bool {
    Command::new("which")
        .arg(tool)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
