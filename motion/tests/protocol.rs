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
    assert_eq!(TlSession::command_ids().len(), 20);
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
