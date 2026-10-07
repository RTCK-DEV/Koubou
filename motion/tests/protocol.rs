//! Integration tests driving the public tl.* command surface, including
//! ffmpeg-gated fixture renders (skipped when ffmpeg is absent).

use std::path::PathBuf;
use std::process::Command;

use koubou_motion::TlSession;
use serde_json::{json, Value};

fn have(bin: &str) -> bool {
    Command::new(bin)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kmotion-it-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// lavfi fixtures: 2s 320x240 testsrc2 video + 2s sine audio
fn fixtures(dir: &PathBuf) -> (PathBuf, PathBuf) {
    let v = dir.join("a.mp4");
    let a = dir.join("a.m4a");
    for (args, path) in [
        (
            vec![
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x240:rate=24:duration=2",
                "-pix_fmt",
                "yuv420p",
            ],
            &v,
        ),
        (
            vec!["-f", "lavfi", "-i", "sine=frequency=440:duration=2"],
            &a,
        ),
    ] {
        if !path.exists() {
            let st = Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .args(&args)
                .arg(path.to_str().unwrap())
                .status()
                .unwrap();
            assert!(st.success());
        }
    }
    (v, a)
}

fn dispatch(s: &mut TlSession, v: Value) -> Value {
    let id = v["id"].as_str().unwrap().to_string();
    s.dispatch(&id, &v)
        .unwrap_or_else(|e| panic!("{id} failed: {e:#}"))
}

#[test]
fn doc_cycle_and_duration() {
    let mut s = TlSession::new();
    assert_eq!(TlSession::command_ids().len(), 21);
    dispatch(
        &mut s,
        json!({"id": "tl.new", "w": 320, "h": 240, "fps": 24, "name": "doc"}),
    );
    let tr = dispatch(&mut s, json!({"id": "tl.addTrack", "kind": "video"}));
    assert_eq!(tr["track"], 0);
    let clip = dispatch(
        &mut s,
        json!({"id": "tl.addClip", "track": 0, "src": "a.mp4", "in": 0.5, "out": 2.5, "offset": 1.0}),
    );
    let clip_id = clip["clipId"].as_u64().unwrap();
    // text clip with dur shorthand
    dispatch(
        &mut s,
        json!({"id": "tl.addClip", "track": 0, "text": "title", "offset": 0.0, "dur": 1.5}),
    );
    dispatch(&mut s, json!({"id": "tl.addTrack", "kind": "subtitle"}));
    dispatch(
        &mut s,
        json!({"id": "tl.addCue", "t": 0.25, "dur": 0.75, "text": "hello"}),
    );
    let doc = dispatch(&mut s, json!({"id": "tl.json"}));
    assert_eq!(doc["tracks"].as_array().unwrap().len(), 2);
    // duration = media clip end (1.0 + 2.0) = 3.0
    dispatch(
        &mut s,
        json!({"id": "tl.setClip", "clip": clip_id, "offset": 2.0, "scale": [[0.0, 1.0], [1.0, 1.5]]}),
    );
    // bad edits error, don't panic
    assert!(s
        .dispatch("tl.setClip", &json!({"clip": 999, "x": 5.0}))
        .is_err());
    assert!(s
        .dispatch(
            "tl.addClip",
            &json!({"track": 9, "src": "x", "in": 0.0, "out": 1.0})
        )
        .is_err());
    assert!(s
        .dispatch("tl.addTrack", &json!({"kind": "bogus"}))
        .is_err());
    // renderFrame without ffmpeg inputs still needs real files; skip here
}

#[test]
fn probe_and_silence() {
    if !have("ffmpeg") || !have("ffprobe") {
        return;
    }
    let dir = tmpdir("probe");
    let (v, a) = fixtures(&dir);
    let mut s = TlSession::new();
    let r = dispatch(
        &mut s,
        json!({"id": "tl.probe", "path": v.to_string_lossy()}),
    );
    assert_eq!(r["w"], 320);
    assert_eq!(r["h"], 240);
    assert_eq!(r["hasVideo"], true);
    assert!((r["duration"].as_f64().unwrap() - 2.0).abs() < 0.35);
    assert!((r["fps"].as_f64().unwrap() - 24.0).abs() < 0.5);
    let ra = dispatch(
        &mut s,
        json!({"id": "tl.probe", "path": a.to_string_lossy()}),
    );
    assert_eq!(ra["hasAudio"], true);
    assert_eq!(ra["hasVideo"], false);

    // 1s silence + 1s tone + silence → detect the edges; sine file itself
    // is fully non-silent, so synthesize one with a gap.
    let g = dir.join("gap.m4a");
    let st = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=44100:cl=mono:d=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=44100:cl=mono:d=1",
            "-filter_complex",
            "[0:a][1:a][2:a]concat=n=3:v=0:a=1",
        ])
        .arg(g.to_str().unwrap())
        .status()
        .unwrap();
    assert!(st.success());
    let spans = dispatch(
        &mut s,
        json!({"id": "tl.detectSilence", "path": g.to_string_lossy(), "thresholdDB": -35, "minDur": 0.4}),
    );
    let arr = spans.as_array().unwrap();
    assert!(arr.len() >= 2, "expected >=2 silence spans, got {arr:?}");
    let first = arr[0]["start"].as_f64().unwrap();
    let last = arr.last().unwrap()["end"].as_f64().unwrap();
    assert!(first < 0.3);
    assert!(last > 2.5);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn render_frame_and_render() {
    if !have("ffmpeg") || !have("ffprobe") {
        return;
    }
    let dir = tmpdir("render");
    let (v, a) = fixtures(&dir);
    let png = dir.join("frame.png");
    let mp4 = dir.join("out.mp4");

    let mut s = TlSession::new();
    dispatch(
        &mut s,
        json!({"id": "tl.new", "w": 320, "h": 240, "fps": 24, "name": "r"}),
    );
    dispatch(&mut s, json!({"id": "tl.addTrack", "kind": "video"}));
    dispatch(
        &mut s,
        json!({
            "id": "tl.addClip", "track": 0, "src": v.to_string_lossy(),
            "in": 0.0, "out": 2.0, "offset": 0.0,
            "scale": [[0.0, 1.0], [1.0, 1.2]], "opacity": [[0.0, 0.0], [0.5, 1.0]],
            "fadeOut": 0.3,
        }),
    );
    dispatch(
        &mut s,
        json!({"id": "tl.addClip", "track": 0, "text": "KMT", "offset": 0.5, "dur": 1.0, "y": 40}),
    );
    dispatch(&mut s, json!({"id": "tl.addTrack", "kind": "audio"}));
    dispatch(
        &mut s,
        json!({"id": "tl.addClip", "track": 1, "src": a.to_string_lossy(), "in": 0.0, "out": 2.0, "offset": 0.0, "fadeIn": 0.1, "fadeOut": 0.1}),
    );
    dispatch(&mut s, json!({"id": "tl.addTrack", "kind": "subtitle"}));
    dispatch(
        &mut s,
        json!({"id": "tl.addCue", "t": 0.5, "dur": 1.0, "text": "sub"}),
    );

    // renderFrame → nonzero PNG with real pixels
    let r = dispatch(
        &mut s,
        json!({"id": "tl.renderFrame", "t": 0.7, "out": png.to_string_lossy()}),
    );
    assert_eq!(r["w"], 320);
    assert!(png.exists() && png.metadata().unwrap().len() > 500);
    let img = image::open(&png).unwrap().to_rgba8();
    let lit = img
        .pixels()
        .filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 30)
        .count();
    assert!(lit > 200, "frame looks empty ({lit} lit px)");

    // tl.render → one ffmpeg call → playable mp4
    let rr = dispatch(
        &mut s,
        json!({"id": "tl.render", "out": mp4.to_string_lossy(), "burnSubs": true}),
    );
    assert_eq!(rr["path"], json!(mp4.to_string_lossy()));
    let info = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(mp4.to_str().unwrap())
        .output()
        .unwrap();
    let j: Value = serde_json::from_slice(&info.stdout).unwrap();
    let kinds: Vec<&str> = j["streams"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["codec_type"].as_str())
        .collect();
    assert!(kinds.contains(&"video"), "no video stream: {j}");
    assert!(kinds.contains(&"audio"), "no audio stream: {j}");
    let dur: f64 = j["format"]["duration"].as_str().unwrap().parse().unwrap();
    assert!((dur - 2.0).abs() < 0.35, "duration {dur}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn generate_clip_endpoint_down_errors_cleanly() {
    let mut s = TlSession::new();
    dispatch(&mut s, json!({"id": "tl.new", "w": 64, "h": 64, "fps": 24}));
    dispatch(&mut s, json!({"id": "tl.addTrack", "kind": "video"}));
    // nothing listening on 127.0.0.1:59999 — must error, never panic/hang
    let e = s
        .dispatch(
            "tl.generateClip",
            &json!({"endpoint": "http://127.0.0.1:59999", "prompt": "x", "track": 0, "timeoutSecs": 10}),
        )
        .unwrap_err();
    let msg = format!("{e:#}");
    assert!(
        msg.contains("cannot reach") || msg.contains("failed"),
        "{msg}"
    );
}

/// mean absolute per-channel difference between two RGBA images
fn mad(a: &image::RgbaImage, b: &image::RgbaImage) -> f64 {
    let (w, h) = (a.width().min(b.width()), a.height().min(b.height()));
    let mut acc = 0u64;
    let mut n = 0u64;
    for y in 0..h {
        for x in 0..w {
            let p = a.get_pixel(x, y);
            let q = b.get_pixel(x, y);
            for ch in 0..3 {
                acc += (p[ch] as i64 - q[ch] as i64).unsigned_abs();
                n += 1;
            }
        }
    }
    acc as f64 / n.max(1) as f64
}

fn extract_frame(video: &PathBuf, t: f64, out: &PathBuf) {
    let st = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-ss",
            &format!("{t}"),
            "-i",
        ])
        .arg(video.to_str().unwrap())
        .args(["-frames:v", "1"])
        .arg(out.to_str().unwrap())
        .status()
        .unwrap();
    assert!(st.success());
}

/// regression for the PTS bug: a clip at offset>0 must show its *head*
/// when its window opens — before the fix the input clock ran from t=0 so
/// the clip was already `offset` seconds in (a 2s clip at offset 1.5
/// appeared half-finished and froze at its tail).
#[test]
fn offset_clip_shows_head_at_offset() {
    if !have("ffmpeg") || !have("ffprobe") {
        return;
    }
    let dir = tmpdir("pts");
    let (v, _a) = fixtures(&dir);
    let out = dir.join("off.mp4");

    let mut s = TlSession::new();
    dispatch(
        &mut s,
        json!({"id": "tl.new", "w": 320, "h": 240, "fps": 24, "name": "o"}),
    );
    dispatch(&mut s, json!({"id": "tl.addTrack", "kind": "video"}));
    // 2s testsrc2 parked at offset 1.5
    dispatch(
        &mut s,
        json!({"id": "tl.addClip", "track": 0, "src": v.to_string_lossy(), "in": 0.0, "out": 2.0, "offset": 1.5}),
    );
    let rr = dispatch(
        &mut s,
        json!({"id": "tl.render", "out": out.to_string_lossy()}),
    );
    assert_eq!(rr["path"], json!(out.to_string_lossy()));

    // before the window opens the frame must be black
    let f_pre = dir.join("pre.png");
    extract_frame(&out, 0.5, &f_pre);
    let pre = image::open(&f_pre).unwrap().to_rgba8();
    let lit = pre
        .pixels()
        .filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 30)
        .count();
    assert_eq!(lit, 0, "pixels drawn before clip offset");

    // just inside the window (t=1.6 ≈ clip-local 0.1) the output should
    // match the SOURCE's t=0.1, not the source's t=1.6
    let f_out = dir.join("at.png");
    let f_src_head = dir.join("src0.png");
    let f_src_late = dir.join("src16.png");
    extract_frame(&out, 1.6, &f_out);
    extract_frame(&v, 0.1, &f_src_head);
    extract_frame(&v, 1.6, &f_src_late);
    let o = image::open(&f_out).unwrap().to_rgba8();
    let h_img = image::open(&f_src_head).unwrap().to_rgba8();
    let l_img = image::open(&f_src_late).unwrap().to_rgba8();
    let d_head = mad(&o, &h_img);
    let d_late = mad(&o, &l_img);
    assert!(
        d_head < d_late,
        "clip at offset should play from its head (head-diff {d_head:.1} vs late-diff {d_late:.1})"
    );
    // and not frozen on a stale frame later in the window either
    let f_tail = dir.join("tail.png");
    extract_frame(&out, 3.3, &f_tail);
    let tail = image::open(&f_tail).unwrap().to_rgba8();
    let lit2 = tail
        .pixels()
        .filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 30)
        .count();
    assert!(lit2 > 1000, "clip vanished before its window ended");
    std::fs::remove_dir_all(&dir).ok();
}

/// transition + volume expressions must parse under real ffmpeg — graph
/// strings are unit-tested; this catches a filter-name/quoting slip.
#[test]
fn transitions_volume_and_duck_render_smoke() {
    if !have("ffmpeg") || !have("ffprobe") {
        return;
    }
    let dir = tmpdir("trans");
    let (v, a) = fixtures(&dir);
    let out = dir.join("t.mp4");

    let mut s = TlSession::new();
    dispatch(
        &mut s,
        json!({"id": "tl.new", "w": 320, "h": 240, "fps": 24, "name": "t"}),
    );
    dispatch(&mut s, json!({"id": "tl.addTrack", "kind": "video"}));
    dispatch(
        &mut s,
        json!({
            "id": "tl.addClip", "track": 0, "src": v.to_string_lossy(),
            "in": 0.0, "out": 2.0, "offset": 0.5,
            "transIn": {"type": "slide", "dur": 0.4},
            "transOut": {"type": "dip", "dur": 0.3, "color": [0.0, 0.0, 0.0, 1.0]},
            "volume": [[0.0, 1.0], [1.0, 0.5]],
        }),
    );
    dispatch(&mut s, json!({"id": "tl.addTrack", "kind": "audio"}));
    dispatch(
        &mut s,
        json!({"id": "tl.addClip", "track": 1, "src": a.to_string_lossy(), "in": 0.0, "out": 2.0, "offset": 0.0}),
    );
    dispatch(&mut s, json!({"id": "tl.addTrack", "kind": "subtitle"}));
    dispatch(
        &mut s,
        json!({"id": "tl.addCue", "t": 0.5, "dur": 1.0, "text": "字幕"}),
    );
    let r = dispatch(&mut s, json!({"id": "tl.duck", "track": 1, "amount": 0.25}));
    assert_eq!(r["dipped"], json!(1));
    let rr = dispatch(
        &mut s,
        json!({"id": "tl.render", "out": out.to_string_lossy(), "burnSubs": true}),
    );
    assert_eq!(rr["path"], json!(out.to_string_lossy()));
    assert_eq!(rr["audioStreams"], json!(1));
    // dip underlay element is counted as a visual
    assert_eq!(rr["clips"], json!(3));
    assert!(out.exists() && out.metadata().unwrap().len() > 1000);
    std::fs::remove_dir_all(&dir).ok();
}
