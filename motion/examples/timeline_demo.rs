//! End-to-end demo of the tl.* protocol: build a 2s timeline, probe the
//! sources, render a frame, render the mp4, and detect silence.
//! Run: `cargo run -p koubou-motion --example timeline_demo -- /tmp/kmdemo`

use koubou_motion::TlSession;
use serde_json::json;

fn d(s: &mut TlSession, v: serde_json::Value) -> serde_json::Value {
    let id = v["id"].as_str().unwrap_or("").to_string();
    match s.dispatch(&id, &v) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{id} -> error: {e:#}");
            serde_json::json!(null)
        }
    }
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/kmotion-demo".into());
    std::fs::create_dir_all(&dir).expect("mkdir");
    let video = format!("{dir}/a.mp4");
    let audio = format!("{dir}/a.m4a");
    for (args, out) in [
        (
            vec![
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=24:duration=2",
            ],
            &video,
        ),
        (
            vec!["-f", "lavfi", "-i", "sine=frequency=440:duration=2"],
            &audio,
        ),
    ] {
        let st = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(&args)
            .arg("-pix_fmt")
            .arg("yuv420p")
            .arg(out)
            .status()
            .expect("ffmpeg");
        assert!(st.success());
    }

    let mut s = TlSession::new();
    println!("commands: {:?}", TlSession::command_ids());
    d(
        &mut s,
        json!({"id":"tl.new","w":640,"h":360,"fps":24,"name":"demo"}),
    );
    d(&mut s, json!({"id":"tl.addTrack","kind":"video"}));
    d(
        &mut s,
        json!({"id":"tl.addClip","track":0,"src":video,"in":0,"out":2,"offset":0,
        "scale":[[0,1.0],[1.5,1.15]],"fadeIn":0.2,"fadeOut":0.3}),
    );
    d(
        &mut s,
        json!({"id":"tl.addClip","track":0,"text":"koubou motion","offset":0.4,"dur":1.2,"y":-120}),
    );
    d(&mut s, json!({"id":"tl.addTrack","kind":"audio"}));
    d(
        &mut s,
        json!({"id":"tl.addClip","track":1,"src":audio,"in":0,"out":2,"offset":0}),
    );
    d(&mut s, json!({"id":"tl.addTrack","kind":"subtitle"}));
    d(
        &mut s,
        json!({"id":"tl.addCue","t":0.3,"dur":1.4,"text":"rendered by tl.render"}),
    );

    println!(
        "probe: {}",
        d(&mut s, json!({"id":"tl.probe","path":video}))
    );
    println!(
        "frame: {}",
        d(
            &mut s,
            json!({"id":"tl.renderFrame","t":0.8,"out":format!("{dir}/frame.png")})
        )
    );
    println!(
        "render: {}",
        d(
            &mut s,
            json!({"id":"tl.render","out":format!("{dir}/out.mp4"),"burnSubs":true})
        )
    );
    println!(
        "silence: {}",
        d(&mut s, json!({"id":"tl.detectSilence","path":audio}))
    );
    println!("doc: {}", d(&mut s, json!({"id":"tl.json"})));
}
