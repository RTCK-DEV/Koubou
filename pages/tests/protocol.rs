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

/// dispatch that should fail — returns the error string
fn dispatch_err(s: &mut PgSession, v: Value) -> String {
    let id = v["id"].as_str().unwrap().to_string();
    match s.dispatch(&id, &v) {
        Ok(r) => panic!("{id} should have failed, got {r}"),
        Err(e) => format!("{e:#}"),
    }
}

#[test]
fn linked_frames_flow_overflow_text() {
    let mut s = PgSession::new();
    dispatch(&mut s, json!({"id":"pg.new","w":200,"h":200,"name":"Flow"}));
    dispatch(&mut s, json!({"id":"pg.addPage"}));

    // a text frame too small for its text, plus a linked target
    let long: String = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu".into();
    let a = dispatch(
        &mut s,
        json!({"id":"pg.addFrame","page":0,"kind":"text",
               "x":10,"y":10,"w":80,"h":20,"text":long,"size":10}),
    )["frame"]
        .as_u64()
        .unwrap();
    let b = dispatch(
        &mut s,
        json!({"id":"pg.addFrame","page":0,"kind":"text",
               "x":10,"y":60,"w":80,"h":100,"text":"B own text","size":10}),
    )["frame"]
        .as_u64()
        .unwrap();

    // before linking, B shows its own text
    let doc = dispatch(&mut s, json!({"id":"pg.json"}));
    assert_eq!(
        doc["textFlow"][b.to_string()]["text"].as_str().unwrap(),
        "B own text"
    );

    // link: the overflow continues into B
    dispatch(&mut s, json!({"id":"pg.linkFrames","frame":a,"to":b}));
    let doc = dispatch(&mut s, json!({"id":"pg.json"}));
    let frames = doc["pages"][0]["frames"].as_array().unwrap();
    assert_eq!(frames[0]["next"].as_u64().unwrap(), b);
    let fa = &doc["textFlow"][a.to_string()];
    let fb = &doc["textFlow"][b.to_string()];
    let shown_a = fa["text"].as_str().unwrap();
    let shown_b = fb["text"].as_str().unwrap();
    assert!(!shown_a.is_empty() && !shown_b.is_empty());
    assert!(
        !shown_b.contains("B own"),
        "linked target shows the story, not its own text"
    );
    assert!(
        shown_b.contains("zulu") || fb["overset"].as_bool().unwrap(),
        "remainder should reach B (or be marked overset): B='{shown_b}'"
    );
    // A spilled into B, so A itself is not overset
    assert_eq!(fa["overset"], false);
    // the PNG render of the page paints text in both frame areas
    let png = tmp("flow.png");
    dispatch(
        &mut s,
        json!({"id":"pg.renderPng","page":0,"out":png.to_string_lossy(),"dpi":96}),
    );
    let img = image::open(&png).unwrap().to_rgba8();
    // page is 200pt @96dpi → ~266px; B sits at y=60..160pt → 80..213px.
    // scan B's band for ink
    let ink = img
        .pixels()
        .enumerate()
        .filter(|(i, p)| {
            let y = *i as u32 / img.width();
            y >= 90 && y <= 200 && p.0[..3] != [255, 255, 255]
        })
        .count();
    assert!(ink > 50, "linked frame rendered no text (ink={ink})");

    // cycle rejection: B → A would close the chain
    let e = dispatch_err(&mut s, json!({"id":"pg.linkFrames","frame":b,"to":a}));
    assert!(e.contains("cycle"), "{e}");

    // duplicating the page remaps in-page links onto the new frames
    dispatch(&mut s, json!({"id":"pg.duplicatePage","page":0}));
    let doc = dispatch(&mut s, json!({"id":"pg.json"}));
    let dup_frames = doc["pages"][1]["frames"].as_array().unwrap();
    let dup_next = dup_frames[0]["next"].as_u64().unwrap();
    assert_eq!(
        dup_next,
        dup_frames[1]["id"].as_u64().unwrap(),
        "dup should link to the dup's own B, not the original {b}"
    );

    // unlink: B reverts to its own text
    dispatch(&mut s, json!({"id":"pg.linkFrames","frame":a,"to":null}));
    let doc = dispatch(&mut s, json!({"id":"pg.json"}));
    assert_eq!(
        doc["textFlow"][b.to_string()]["text"].as_str().unwrap(),
        "B own text"
    );
}

#[test]
fn page_size_spread_and_styles() {
    let mut s = PgSession::new();
    dispatch(&mut s, json!({"id":"pg.new","w":595,"h":842,"name":"S"}));
    for _ in 0..5 {
        dispatch(&mut s, json!({"id":"pg.addPage"}));
    }

    // pg.setPageSize resizes the document (frames keep positions)
    let r = dispatch(&mut s, json!({"id":"pg.setPageSize","w":612,"h":792}));
    assert_eq!(r["pageW"], 612.0);
    let doc = dispatch(&mut s, json!({"id":"pg.json"}));
    assert_eq!(doc["pageW"], 612.0);
    assert_eq!(doc["pageH"], 792.0);
    let e = dispatch_err(&mut s, json!({"id":"pg.setPageSize","w":-5,"h":792}));
    assert!(e.contains("bad page size"), "{e}");
    // PDF adopts the new size
    let pdf_path = tmp("size.pdf");
    dispatch(
        &mut s,
        json!({"id":"pg.render","out":pdf_path.to_string_lossy()}),
    );
    let pdf = std::fs::read(&pdf_path).unwrap();
    let s_pdf = String::from_utf8_lossy(&pdf);
    assert!(
        s_pdf.contains("/MediaBox [0 0 612 792]"),
        "no resized MediaBox"
    );

    // facing pages: [0] [1,2] [3,4]
    let r = dispatch(&mut s, json!({"id":"pg.setSpread","facing":true}));
    assert_eq!(r["facing"], true);
    assert_eq!(
        r["pairs"],
        json!([[0], [1, 2], [3, 4]]),
        "even=verso left, odd=recto right"
    );
    let doc = dispatch(&mut s, json!({"id":"pg.json"}));
    assert_eq!(doc["facing"], true);
    assert_eq!(doc["spread"]["pairs"], json!([[0], [1, 2], [3, 4]]));
    dispatch(&mut s, json!({"id":"pg.setSpread","facing":false}));
    let doc = dispatch(&mut s, json!({"id":"pg.json"}));
    assert_eq!(doc["spread"]["pairs"].as_array().unwrap().len(), 5);

    // styles: define then apply
    dispatch(
        &mut s,
        json!({"id":"pg.setStyle","name":"body","font":"Hiragino Sans","size":18,"leading":1.4,"color":[0.2,0.2,0.2,1]}),
    );
    let f = dispatch(
        &mut s,
        json!({"id":"pg.addFrame","page":0,"kind":"text",
               "x":10,"y":10,"w":100,"h":40,"text":"見出し","size":12}),
    )["frame"]
        .as_u64()
        .unwrap();
    dispatch(
        &mut s,
        json!({"id":"pg.applyStyle","frame":f,"name":"body"}),
    );
    let doc = dispatch(&mut s, json!({"id":"pg.json"}));
    let fr = &doc["pages"][0]["frames"][0];
    assert_eq!(fr["size"], 18.0);
    assert_eq!(fr["font"], "Hiragino Sans");
    assert_eq!(fr["style"], "body");
    assert!(doc["styles"]["body"]["size"].as_f64().unwrap() == 18.0);
    // merge semantics: updating one field keeps the others
    dispatch(&mut s, json!({"id":"pg.setStyle","name":"body","size":22}));
    let doc = dispatch(&mut s, json!({"id":"pg.json"}));
    assert_eq!(doc["styles"]["body"]["font"], "Hiragino Sans");
    // errors: unknown style, non-text frame
    let e = dispatch_err(
        &mut s,
        json!({"id":"pg.applyStyle","frame":f,"name":"nope"}),
    );
    assert!(e.contains("not defined"), "{e}");
    let r_id = dispatch(
        &mut s,
        json!({"id":"pg.addFrame","page":0,"kind":"rect","x":0,"y":0,"w":10,"h":10}),
    )["frame"]
        .as_u64()
        .unwrap();
    let e = dispatch_err(
        &mut s,
        json!({"id":"pg.applyStyle","frame":r_id,"name":"body"}),
    );
    assert!(e.contains("not a text frame"), "{e}");
    // style survives save/open
    let save = tmp("styled.kpages");
    dispatch(
        &mut s,
        json!({"id":"pg.save","path":save.to_string_lossy()}),
    );
    dispatch(
        &mut s,
        json!({"id":"pg.open","path":save.to_string_lossy()}),
    );
    let doc = dispatch(&mut s, json!({"id":"pg.json"}));
    assert_eq!(doc["styles"]["body"]["size"], 22.0);
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
