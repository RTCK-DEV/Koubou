//! Everything-is-a-command: the timeline session dispatcher, mirroring
//! `composer::commands::Session`. Requests are `{"id": "tl.*", ...params}`;
//! `dispatch` returns `Ok(result)` or `Err(anyhow)` — callers wrap it in the
//! `{"ok": bool, ...}` envelope. Panics are caught at the top and converted
//! into errors so a bad command can never kill the session.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::ffmpeg;
use crate::model::{Clip, Cue, Timeline, TrackKind};
use crate::render;

/// one timeline-editing session: an optional open .kmotion document
pub struct TlSession {
    pub timeline: Option<Timeline>,
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

impl TlSession {
    pub fn new() -> TlSession {
        TlSession { timeline: None }
    }

    /// dispatch by command id; panics inside a command become errors
    pub fn dispatch(&mut self, id: &str, v: &Value) -> Result<Value> {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run(id, v)));
        match r {
            Ok(res) => res,
            Err(_) => anyhow::bail!("command '{id}' panicked"),
        }
    }

    /// the command ids this dispatcher understands
    pub fn command_ids() -> Vec<&'static str> {
        vec![
            "tl.new",
            "tl.open",
            "tl.save",
            "tl.json",
            "tl.addTrack",
            "tl.addClip",
            "tl.setClip",
            "tl.removeClip",
            "tl.addCue",
            "tl.probe",
            "tl.renderFrame",
            "tl.render",
            "tl.detectSilence",
            "tl.generateClip",
        ]
    }

    fn tl(&mut self) -> Result<&mut Timeline> {
        self.timeline
            .as_mut()
            .context("no timeline — run tl.new or tl.open first")
    }

    fn run(&mut self, id: &str, v: &Value) -> Result<Value> {
        match id {
            "tl.new" => {
                let w = opt_u64(v, "w", 1920) as u32;
                let h = opt_u64(v, "h", 1080) as u32;
                let fps = opt_f64(v, "fps", 30.0);
                let name = v.get("name").and_then(Value::as_str).unwrap_or("Untitled");
                let mut tl = Timeline::new(name, w, h, fps);
                tl.sanitize();
                self.timeline = Some(tl);
                Ok(json!({"w": w, "h": h, "fps": fps}))
            }
            "tl.open" => {
                let p = req_str(v, "path")?;
                self.timeline = Some(Timeline::load(Path::new(&p))?);
                Ok(json!("ok"))
            }
            "tl.save" => {
                let t = self.timeline.as_ref().context("no timeline")?;
                let p = v
                    .get("path")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(format!("{}.kmotion", t.name)));
                t.save(&p)?;
                Ok(json!({"path": p.to_string_lossy()}))
            }
            "tl.json" => {
                let t = self.timeline.as_ref().context("no timeline")?;
                serde_json::to_value(t).map_err(Into::into)
            }
            "tl.addTrack" => {
                let kind_s = req_str(v, "kind")?;
                let kind = TrackKind::parse(&kind_s)
                    .with_context(|| format!("unknown track kind '{kind_s}'"))?;
                let t = self.tl()?;
                let idx = t.add_track(kind);
                let id = t.tracks[idx].id;
                if let Some(n) = v.get("name").and_then(Value::as_str) {
                    t.tracks[idx].name = n.to_string();
                }
                if let Some(m) = v.get("muted").and_then(Value::as_bool) {
                    t.tracks[idx].muted = m;
                }
                Ok(json!({"track": idx, "trackId": id}))
            }
            "tl.addClip" => {
                let track = req_u64(v, "track")? as usize;
                let src = v.get("src").and_then(Value::as_str).unwrap_or("");
                let text = v.get("text").and_then(Value::as_str);
                if src.is_empty() && text.is_none() {
                    anyhow::bail!("tl.addClip needs 'src' (media) or 'text' (text clip)");
                }
                let in_p = opt_f64(v, "in", 0.0);
                // media: 'out' required; text: out = in + dur (default 3s)
                let out_p = match v.get("out").and_then(Value::as_f64) {
                    Some(o) => o,
                    None => {
                        if text.is_some() && src.is_empty() {
                            in_p + opt_f64(v, "dur", 3.0)
                        } else {
                            return Err(anyhow::anyhow!("missing param 'out'"));
                        }
                    }
                };
                if out_p <= in_p {
                    anyhow::bail!("clip out ({out_p}) must be > in ({in_p})");
                }
                let offset = opt_f64(v, "offset", 0.0);
                let mut clip = Clip::media(src, in_p, out_p, offset);
                if let Some(t) = text {
                    clip.text = Some(t.to_string());
                }
                apply_clip_params(&mut clip, v)?;
                let t = self.tl()?;
                let id = t.add_clip(track, clip)?;
                Ok(json!({"clipId": id}))
            }
            "tl.setClip" => {
                let id = req_u64(v, "clip")?;
                let t = self.tl()?;
                let clip = t
                    .clip_mut(id)
                    .with_context(|| format!("clip {id} not found"))?;
                apply_clip_params(clip, v)?;
                clip.sanitize();
                Ok(json!("ok"))
            }
            "tl.removeClip" => {
                let id = req_u64(v, "clip")?;
                let t = self.tl()?;
                t.remove_clip(id)
                    .with_context(|| format!("clip {id} not found"))?;
                Ok(json!("ok"))
            }
            "tl.addCue" => {
                let cue = Cue {
                    t: req_f64(v, "t")?,
                    dur: req_f64(v, "dur")?,
                    text: req_str(v, "text")?,
                };
                let track = v.get("track").and_then(Value::as_u64).map(|i| i as usize);
                self.tl()?.add_cue(track, cue)?;
                Ok(json!("ok"))
            }
            "tl.probe" => {
                let p = req_str(v, "path")?;
                if !ffmpeg::have_ffprobe() {
                    anyhow::bail!("ffprobe not found on PATH — install ffmpeg");
                }
                let info = ffmpeg::probe(&p)?;
                Ok(json!({
                    "path": p,
                    "duration": info.duration,
                    "fps": info.fps,
                    "w": info.w,
                    "h": info.h,
                    "hasVideo": info.has_video,
                    "hasAudio": info.has_audio,
                    "streams": info.streams,
                }))
            }
            "tl.renderFrame" => {
                let t_sec = req_f64(v, "t")?;
                let out = req_str(v, "out")?;
                let tl = self.timeline.as_ref().context("no timeline")?;
                let img = render::render_frame(tl, t_sec)?;
                img.save(&out).with_context(|| format!("save {out}"))?;
                Ok(json!({"path": out, "w": img.width(), "h": img.height(), "t": t_sec}))
            }
            "tl.render" => {
                let out = req_str(v, "out")?;
                let burn = v.get("burnSubs").and_then(Value::as_bool).unwrap_or(false);
                let tl = self.timeline.as_ref().context("no timeline")?;
                render::render(tl, &out, burn)
            }
            "tl.detectSilence" => {
                let p = req_str(v, "path")?;
                let thr = opt_f64(v, "thresholdDB", -35.0);
                let min = opt_f64(v, "minDur", 0.5);
                let spans = ffmpeg::detect_silence(&p, thr, min)?;
                let arr: Vec<Value> = spans
                    .iter()
                    .map(|(a, b)| json!({"start": a, "end": b}))
                    .collect();
                Ok(json!(arr))
            }
            "tl.generateClip" => self.generate_clip(v),
            _ => anyhow::bail!("unknown command id: {id}"),
        }
    }

    /// `tl.generateClip` — POST {prompt} to a minimax-h3 h3ui-style backend,
    /// poll the job, download the mp4, add it as a clip on `track`.
    /// Params: endpoint (default http://127.0.0.1:8000), prompt, track,
    /// optional size/length/quality/seed/out/offset/timeoutSecs.
    fn generate_clip(&mut self, v: &Value) -> Result<Value> {
        let endpoint = v
            .get("endpoint")
            .and_then(Value::as_str)
            .unwrap_or("http://127.0.0.1:8000")
            .trim_end_matches('/');
        let prompt = req_str(v, "prompt")?;
        let track = req_u64(v, "track")? as usize;
        let timeout = opt_f64(v, "timeoutSecs", 1800.0).max(5.0);
        let out_path = v.get("out").and_then(Value::as_str).map(String::from);

        // track must exist and be a video track before we spend a generation
        {
            let tl = self.tl()?;
            let t = tl
                .track(track)
                .with_context(|| format!("track index {track} out of range"))?;
            if t.kind != TrackKind::Video {
                anyhow::bail!("track {track} is {:?}, not a video track", t.kind);
            }
        }

        // submit the job
        let mut body = json!({"prompt": prompt});
        for k in ["size", "length", "quality", "image", "imageLast"] {
            if let Some(val) = v.get(k) {
                body[k] = val.clone();
            }
        }
        if let Some(seed) = v.get("seed").and_then(Value::as_u64) {
            body["seed"] = json!(seed);
        }
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let body_path =
            std::env::temp_dir().join(format!("kmotion-gen-{}-{seq}.json", std::process::id()));
        std::fs::write(&body_path, body.to_string())
            .with_context(|| format!("write {}", body_path.display()))?;
        let gen = curl_json(
            &format!("{endpoint}/api/generate"),
            &[
                "-X".into(),
                "POST".into(),
                "-H".into(),
                "Content-Type: application/json".into(),
                "-d".into(),
                format!("@{}", body_path.to_string_lossy()),
                "-f".into(),
            ],
            30,
        );
        let _ = std::fs::remove_file(&body_path);
        let gen = gen
            .with_context(|| format!("generateClip: cannot reach {endpoint} — is h3ui running?"))?;
        if gen.get("ok").and_then(Value::as_bool) != Some(true) {
            let msg = gen
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("generate rejected");
            anyhow::bail!("generateClip: {msg}");
        }
        let job_id = gen
            .get("id")
            .and_then(Value::as_str)
            .context("generate response had no job id")?
            .to_string();

        // poll /api/state until our job lands in history (done/error/canceled)
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout);
        let mut video_rel = String::new();
        loop {
            if std::time::Instant::now() > deadline {
                anyhow::bail!("generateClip: job {job_id} timed out after {timeout:.0}s");
            }
            let state = curl_json(&format!("{endpoint}/api/state"), &["-f".into()], 15)
                .with_context(|| format!("generateClip: lost contact with {endpoint}"))?;
            let mut found = false;
            if let Some(hist) = state.get("history").and_then(Value::as_array) {
                for e in hist {
                    if e.get("id").and_then(Value::as_str) != Some(job_id.as_str()) {
                        continue;
                    }
                    found = true;
                    match e.get("status").and_then(Value::as_str) {
                        Some("done") => {
                            video_rel = e
                                .get("video")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                        }
                        Some(st) => {
                            let err = e.get("error").and_then(Value::as_str).unwrap_or(st);
                            anyhow::bail!("generateClip: job {job_id} ended {st}: {err}");
                        }
                        None => {}
                    }
                    break;
                }
            }
            if found {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        if video_rel.is_empty() {
            anyhow::bail!("generateClip: job {job_id} finished without a video path");
        }

        // download the mp4
        let dst = out_path.unwrap_or_else(|| {
            std::env::temp_dir()
                .join(format!("koubou-gen-{job_id}.mp4"))
                .to_string_lossy()
                .into_owned()
        });
        ffmpeg::curl(
            &format!("{endpoint}/video/{video_rel}"),
            &["-f".into(), "-o".into(), dst.clone()],
            120,
        )
        .with_context(|| format!("generateClip: download of {video_rel} failed"))?;

        // probe duration so the clip spans the whole file
        let dur = ffmpeg::probe(&dst).map(|m| m.duration).unwrap_or(0.0);
        if dur <= 0.0 {
            anyhow::bail!("generateClip: downloaded '{dst}' is not a playable mp4");
        }
        let tl = self.tl()?;
        let offset = match v.get("offset").and_then(Value::as_f64) {
            Some(o) => o,
            // default: append after the last clip on this track
            None => tl
                .track(track)
                .map(|t| t.clips.iter().map(|c| c.end()).fold(0.0, f64::max))
                .unwrap_or(0.0),
        };
        let clip_id = tl.add_clip(track, Clip::media(&dst, 0.0, dur, offset))?;
        Ok(json!({
            "clipId": clip_id,
            "jobId": job_id,
            "path": dst,
            "duration": dur,
            "offset": offset,
        }))
    }
}

/// curl returning parsed JSON (or an error for non-JSON bodies)
fn curl_json(url: &str, args: &[String], timeout_secs: u64) -> Result<Value> {
    let bytes = ffmpeg::curl(url, args, timeout_secs)?;
    serde_json::from_slice(&bytes).with_context(|| {
        let preview: String = String::from_utf8_lossy(&bytes).chars().take(200).collect();
        format!("{url} returned non-JSON: {preview}")
    })
}

fn req_str(v: &Value, k: &str) -> Result<String> {
    v.get(k)
        .and_then(Value::as_str)
        .map(str::to_string)
        .with_context(|| format!("missing param '{k}'"))
}

fn req_u64(v: &Value, k: &str) -> Result<u64> {
    v.get(k)
        .and_then(Value::as_u64)
        .with_context(|| format!("missing param '{k}'"))
}

fn req_f64(v: &Value, k: &str) -> Result<f64> {
    v.get(k)
        .and_then(Value::as_f64)
        .with_context(|| format!("missing param '{k}'"))
}

fn opt_u64(v: &Value, k: &str, default: u64) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(default)
}

fn opt_f64(v: &Value, k: &str, default: f64) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(default)
}

/// apply the optional setClip/addClip params: offset, in, out, keyframe
/// lists (accept a bare number → constant), fades, text.
fn apply_clip_params(clip: &mut Clip, v: &Value) -> Result<()> {
    if let Some(o) = v.get("offset").and_then(Value::as_f64) {
        clip.offset = o.max(0.0);
    }
    if let Some(i) = v.get("in").and_then(Value::as_f64) {
        clip.in_point = i.max(0.0);
    }
    if let Some(o) = v.get("out").and_then(Value::as_f64) {
        clip.out_point = o;
    }
    for (key, dst) in [
        ("opacity", &mut clip.opacity),
        ("scale", &mut clip.scale),
        ("x", &mut clip.x),
        ("y", &mut clip.y),
    ] {
        if let Some(val) = v.get(key) {
            *dst = parse_kfs(val).with_context(|| format!("bad keyframes for '{key}'"))?;
        }
    }
    if let Some(f) = v.get("fadeIn").and_then(Value::as_f64) {
        clip.fade_in = f.max(0.0);
    }
    if let Some(f) = v.get("fadeOut").and_then(Value::as_f64) {
        clip.fade_out = f.max(0.0);
    }
    if let Some(t) = v.get("text") {
        clip.text = if t.is_null() {
            None
        } else {
            t.as_str().map(str::to_string)
        };
    }
    if clip.out_point <= clip.in_point {
        anyhow::bail!(
            "clip out ({}) must be > in ({})",
            clip.out_point,
            clip.in_point
        );
    }
    Ok(())
}

/// keyframes from JSON: `[[t,v],...]` or a bare number (constant)
fn parse_kfs(v: &Value) -> Result<Vec<[f64; 2]>> {
    if let Some(n) = v.as_f64() {
        return Ok(vec![[0.0, n]]);
    }
    serde_json::from_value(v.clone()).context("expected [[t,v],...] or a number")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess_with_timeline() -> TlSession {
        let mut s = TlSession::new();
        s.dispatch(
            "tl.new",
            &json!({"w": 320, "h": 240, "fps": 24, "name": "t"}),
        )
        .unwrap();
        s
    }

    #[test]
    fn command_ids_complete() {
        let ids = TlSession::command_ids();
        for c in [
            "tl.new",
            "tl.open",
            "tl.save",
            "tl.json",
            "tl.addTrack",
            "tl.addClip",
            "tl.setClip",
            "tl.removeClip",
            "tl.addCue",
            "tl.probe",
            "tl.renderFrame",
            "tl.render",
            "tl.detectSilence",
            "tl.generateClip",
        ] {
            assert!(ids.contains(&c), "missing {c}");
        }
        assert_eq!(ids.len(), 14);
    }

    #[test]
    fn commands_require_timeline() {
        let mut s = TlSession::new();
        assert!(s.dispatch("tl.json", &json!({})).is_err());
        assert!(s
            .dispatch("tl.addTrack", &json!({"kind": "video"}))
            .is_err());
        assert!(s.dispatch("tl.render", &json!({"out": "x.mp4"})).is_err());
        // unknown id
        assert!(s.dispatch("tl.nope", &json!({})).is_err());
        assert!(s.dispatch("", &json!({})).is_err());
    }

    #[test]
    fn build_edit_cycle() {
        let mut s = sess_with_timeline();
        let r = s
            .dispatch("tl.addTrack", &json!({"kind": "video"}))
            .unwrap();
        let track = r["track"].as_u64().unwrap();
        let r = s
            .dispatch(
                "tl.addClip",
                &json!({"track": track, "src": "a.mp4", "in": 1.0, "out": 3.0, "offset": 0.5}),
            )
            .unwrap();
        let clip = r["clipId"].as_u64().unwrap();
        // setClip scalar + keyframe list
        s.dispatch(
            "tl.setClip",
            &json!({"clip": clip, "opacity": 0.5, "x": [[0.0, -10.0], [2.0, 30.0]], "fadeIn": 0.25}),
        )
        .unwrap();
        let doc = s.dispatch("tl.json", &json!({})).unwrap();
        let c = &doc["tracks"][0]["clips"][0];
        assert_eq!(c["opacity"], json!([[0.0, 0.5]]));
        assert_eq!(c["x"], json!([[0.0, -10.0], [2.0, 30.0]]));
        // remove
        s.dispatch("tl.removeClip", &json!({"clip": clip})).unwrap();
        let doc = s.dispatch("tl.json", &json!({})).unwrap();
        assert_eq!(doc["tracks"][0]["clips"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn addcue_needs_subtitle_track() {
        let mut s = sess_with_timeline();
        // no subtitle track yet → actionable error
        let e = s
            .dispatch("tl.addCue", &json!({"t": 0.0, "dur": 1.0, "text": "hi"}))
            .unwrap_err();
        assert!(format!("{e:#}").contains("subtitle"));
        s.dispatch("tl.addTrack", &json!({"kind": "subtitle"}))
            .unwrap();
        s.dispatch("tl.addCue", &json!({"t": 0.0, "dur": 1.0, "text": "hi"}))
            .unwrap();
        // cue on a video track → error
        s.dispatch("tl.addTrack", &json!({"kind": "video"}))
            .unwrap();
        assert!(s
            .dispatch(
                "tl.addCue",
                &json!({"t": 0.0, "dur": 1.0, "text": "x", "track": 1})
            )
            .is_err());
    }

    #[test]
    fn save_open_round_trip() {
        let dir = std::env::temp_dir().join(format!("kmotion-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.kmotion");
        let mut s = sess_with_timeline();
        s.dispatch("tl.addTrack", &json!({"kind": "video"}))
            .unwrap();
        let r = s
            .dispatch("tl.save", &json!({"path": path.to_string_lossy()}))
            .unwrap();
        assert_eq!(r["path"], json!(path.to_string_lossy()));
        let mut s2 = TlSession::new();
        s2.dispatch("tl.open", &json!({"path": path.to_string_lossy()}))
            .unwrap();
        let doc = s2.dispatch("tl.json", &json!({})).unwrap();
        assert_eq!(doc["name"], json!("t"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
