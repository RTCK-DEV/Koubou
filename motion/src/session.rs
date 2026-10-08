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
            "tl.setTrack",
            "tl.removeTrack",
            "tl.addClip",
            "tl.setClip",
            "tl.removeClip",
            "tl.splitClip",
            "tl.duplicateClip",
            "tl.addCue",
            "tl.setCue",
            "tl.removeCue",
            "tl.rippleDelete",
            "tl.rippleInsert",
            "tl.trim",
            "tl.probe",
            "tl.renderFrame",
            "tl.render",
            "tl.detectSilence",
            "tl.generateClip",
            "tl.duck",
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
                // duration is computed, not stored — the playhead/UI needs it
                let mut j = serde_json::to_value(t)?;
                j.as_object_mut()
                    .map(|o| o.insert("duration".into(), json!(t.duration())));
                Ok(j)
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
            "tl.setTrack" => {
                let idx = req_u64(v, "track")? as usize;
                let t = self.tl()?;
                let tr = t
                    .tracks
                    .get_mut(idx)
                    .with_context(|| format!("track index {idx} out of range"))?;
                if let Some(n) = v.get("name").and_then(Value::as_str) {
                    tr.name = n.to_string();
                }
                if let Some(m) = v.get("muted").and_then(Value::as_bool) {
                    tr.muted = m;
                }
                Ok(json!("ok"))
            }
            "tl.removeTrack" => {
                let idx = req_u64(v, "track")? as usize;
                let t = self.tl()?;
                if idx >= t.tracks.len() {
                    anyhow::bail!("track index {idx} out of range");
                }
                t.tracks.remove(idx);
                Ok(json!("ok"))
            }
            "tl.splitClip" => {
                let id = req_u64(v, "clip")?;
                let t_sec = req_f64(v, "t")?;
                let new_id = self.tl()?.split_clip(id, t_sec)?;
                Ok(json!({"clipId": new_id}))
            }
            "tl.duplicateClip" => {
                let id = req_u64(v, "clip")?;
                let new_id = self.tl()?.duplicate_clip(id)?;
                Ok(json!({"clipId": new_id}))
            }
            "tl.rippleDelete" => {
                let id = req_u64(v, "clip")?;
                if !self.tl()?.ripple_delete(id) {
                    anyhow::bail!("clip {id} not found");
                }
                Ok(json!("ok"))
            }
            "tl.rippleInsert" => {
                // same clip params as tl.addClip, but later clips shift right
                let track = req_u64(v, "track")? as usize;
                let src = v.get("src").and_then(Value::as_str).unwrap_or("");
                let text = v.get("text").and_then(Value::as_str);
                if src.is_empty() && text.is_none() {
                    anyhow::bail!("tl.rippleInsert needs 'src' (media) or 'text' (text clip)");
                }
                let in_p = opt_f64(v, "in", 0.0);
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
                let id = self.tl()?.ripple_insert(track, clip)?;
                Ok(json!({"clipId": id}))
            }
            "tl.trim" => {
                let id = req_u64(v, "clip")?;
                let edge = v.get("edge").and_then(Value::as_str).unwrap_or("out");
                let delta = req_f64(v, "delta")?;
                let edge_in = match edge {
                    "in" => true,
                    "out" => false,
                    other => anyhow::bail!("tl.trim edge must be 'in' or 'out', got '{other}'"),
                };
                if !self.tl()?.trim_clip(id, edge_in, delta) {
                    anyhow::bail!("clip {id} not found");
                }
                Ok(json!("ok"))
            }
            "tl.setCue" => {
                let idx = req_u64(v, "index")? as usize;
                let track = v.get("track").and_then(Value::as_u64).map(|i| i as usize);
                let t = self.tl()?;
                let cues = cue_list_mut(t, track)?;
                let cue = cues
                    .get_mut(idx)
                    .with_context(|| format!("cue index {idx} out of range"))?;
                if let Some(x) = v.get("t").and_then(Value::as_f64) {
                    cue.t = x;
                }
                if let Some(x) = v.get("dur").and_then(Value::as_f64) {
                    cue.dur = x;
                }
                if let Some(x) = v.get("text").and_then(Value::as_str) {
                    cue.text = x.to_string();
                }
                cue.sanitize();
                cues.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
                Ok(json!("ok"))
            }
            "tl.removeCue" => {
                let idx = req_u64(v, "index")? as usize;
                let track = v.get("track").and_then(Value::as_u64).map(|i| i as usize);
                let t = self.tl()?;
                let cues = cue_list_mut(t, track)?;
                if idx >= cues.len() {
                    anyhow::bail!("cue index {idx} out of range");
                }
                cues.remove(idx);
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
                let tl = self.timeline.as_ref().context("no timeline")?;
                let img = render::render_frame(tl, t_sec)?;
                match v.get("out").and_then(Value::as_str) {
                    // `out` path → write file and report it
                    Some(out) => {
                        img.save(out).with_context(|| format!("save {out}"))?;
                        Ok(json!({"path": out, "w": img.width(), "h": img.height(), "t": t_sec}))
                    }
                    // no path → inline PNG like doc.render's pngB64
                    None => {
                        use base64::Engine as _;
                        let mut buf = std::io::Cursor::new(Vec::new());
                        img.write_to(&mut buf, image::ImageFormat::Png)
                            .context("encode png")?;
                        Ok(json!({
                            "pngB64": base64::engine::general_purpose::STANDARD
                                .encode(buf.into_inner()),
                            "w": img.width(), "h": img.height(), "t": t_sec,
                        }))
                    }
                }
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
            "tl.duck" => {
                let track = req_u64(v, "track")? as usize;
                let cue_track = v
                    .get("cueTrack")
                    .and_then(Value::as_u64)
                    .map(|i| i as usize);
                let amount = opt_f64(v, "amount", 0.25);
                let attack = opt_f64(v, "attack", 0.15);
                let release = opt_f64(v, "release", 0.3);
                let n = self.tl()?.duck(track, cue_track, amount, attack, release)?;
                Ok(json!({"dipped": n}))
            }
            _ => anyhow::bail!("unknown command id: {id}"),
        }
    }

    /// `tl.generateClip` — POST {prompt} to a minimax-h3 h3ui-style backend,
    /// poll the job, download the mp4, add it as a clip on `track`.
    /// Params: endpoint (default http://127.0.0.1:8000; loopback hosts only
    /// unless `allowRemote: true`), prompt, track (default: first video
    /// track, created when none exists),
    /// optional size/length/quality/seed/out/offset/timeoutSecs.
    fn generate_clip(&mut self, v: &Value) -> Result<Value> {
        let endpoint = v
            .get("endpoint")
            .and_then(Value::as_str)
            .unwrap_or("http://127.0.0.1:8000")
            .trim_end_matches('/');
        // SSRF guard: the endpoint is posted to and polled with the job's
        // paths — keep it loopback unless the caller opts out explicitly.
        let allow_remote = v.get("allowRemote").and_then(Value::as_bool) == Some(true);
        if !allow_remote && !is_loopback_endpoint(endpoint) {
            anyhow::bail!(
                "generateClip: endpoint '{endpoint}' is not a loopback host \
                 (127.0.0.1, ::1, localhost) — pass allowRemote:true to override"
            );
        }
        let prompt = req_str(v, "prompt")?;
        let timeout = opt_f64(v, "timeoutSecs", 1800.0).max(5.0);
        let out_path = v.get("out").and_then(Value::as_str).map(String::from);

        // resolve the track before we spend a generation: explicit index,
        // else first video track, else create one.
        let track = match v.get("track").and_then(Value::as_u64) {
            Some(i) => {
                let tl = self.tl()?;
                let t = tl
                    .track(i as usize)
                    .with_context(|| format!("track index {i} out of range"))?;
                if t.kind != TrackKind::Video {
                    anyhow::bail!("track {i} is {:?}, not a video track", t.kind);
                }
                i as usize
            }
            None => {
                let tl = self.tl()?;
                match tl.tracks.iter().position(|t| t.kind == TrackKind::Video) {
                    Some(i) => i,
                    None => tl.add_track(TrackKind::Video),
                }
            }
        };

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

/// `generateClip` endpoint guard: only http(s) to a loopback host unless
/// the caller passes allowRemote. Hosts must parse as loopback IPs or the
/// literal `localhost` — prefix matching would let `127.x.y.z.evil.com`
/// and userinfo tricks (`evil.com@127.0.0.1` is fine; `127.0.0.1.evil.com`
/// is not) through.
fn is_loopback_endpoint(endpoint: &str) -> bool {
    let rest = match endpoint.split_once("://") {
        Some((scheme, rest)) => {
            if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
                return false;
            }
            rest
        }
        None => endpoint,
    };
    // host[:port] up to the first '/', after any userinfo
    let hostport = rest.split('/').next().unwrap_or("");
    let hostport = hostport.rsplit('@').next().unwrap_or("");
    let host = if let Some(h) = hostport.strip_prefix('[') {
        // [v6addr]:port
        h.split(']').next().unwrap_or("")
    } else {
        hostport.split(':').next().unwrap_or("")
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if let Ok(v4) = host.parse::<std::net::Ipv4Addr>() {
        return v4.is_loopback();
    }
    if let Ok(v6) = host.parse::<std::net::Ipv6Addr>() {
        return v6.is_loopback();
    }
    false
}

/// curl returning parsed JSON (or an error for non-JSON bodies)
fn curl_json(url: &str, args: &[String], timeout_secs: u64) -> Result<Value> {
    let bytes = ffmpeg::curl(url, args, timeout_secs)?;
    serde_json::from_slice(&bytes).with_context(|| {
        let preview: String = String::from_utf8_lossy(&bytes).chars().take(200).collect();
        format!("{url} returned non-JSON: {preview}")
    })
}

/// pick the cue list of a subtitle track — `track` as an index, or the
/// first subtitle track when absent (mirrors `Timeline::add_cue`).
fn cue_list_mut<'a>(t: &'a mut Timeline, track: Option<usize>) -> Result<&'a mut Vec<Cue>> {
    let idx = match track {
        Some(i) => {
            let tr = t
                .tracks
                .get(i)
                .with_context(|| format!("track index {i} out of range"))?;
            if tr.kind != TrackKind::Subtitle {
                anyhow::bail!("track {i} is {:?}, not a subtitle track", tr.kind);
            }
            i
        }
        None => t
            .tracks
            .iter()
            .position(|tr| tr.kind == TrackKind::Subtitle)
            .context("no subtitle track — add one with tl.addTrack kind=subtitle")?,
    };
    Ok(&mut t.tracks[idx].cues)
}

/// MCP-shaped tool specs ({id, name, description, inputSchema}) for every
/// tl.* command — the single source of truth the Session registry and the
/// MCP server both read.
pub fn command_specs() -> Vec<Value> {
    let spec = |id: &str, desc: &str, props: Value, required: &[&str]| {
        json!({
            "id": id,
            "name": id.replace('.', "_"),
            "description": desc,
            "inputSchema": {"type": "object", "properties": props, "required": required},
        })
    };
    let s = |d: &str| json!({"type": "string", "description": d});
    let n = |d: &str| json!({"type": "number", "description": d});
    let b = |d: &str| json!({"type": "boolean", "description": d});
    vec![
        spec("tl.new", "Create a new video timeline", json!({"w": n("px"), "h": n("px"), "fps": n("frames/sec"), "name": s("name")}), &[]),
        spec("tl.open", "Open a .kmotion timeline", json!({"path": s("timeline path")}), &["path"]),
        spec("tl.save", "Save the timeline as .kmotion", json!({"path": s("path; default <name>.kmotion")}), &[]),
        spec("tl.json", "Return the timeline's JSON state (includes computed duration)", json!({}), &[]),
        spec("tl.addTrack", "Add a track (kind: video|audio|subtitle)", json!({"kind": s("track kind"), "name": s("name"), "muted": b("muted")}), &["kind"]),
        spec("tl.setTrack", "Edit a track (name, muted)", json!({"track": n("track index"), "name": s("name"), "muted": b("muted")}), &["track"]),
        spec("tl.removeTrack", "Remove a track and everything on it", json!({"track": n("track index")}), &["track"]),
        spec("tl.addClip", "Add a clip to a track — media via 'src' (needs 'out'), text via 'text' (optional 'dur')", json!({"track": n("track index"), "src": s("media path"), "text": s("text clip content"), "in": n("source in sec"), "out": n("source out sec"), "dur": n("text clip seconds"), "offset": n("timeline offset sec")}), &["track"]),
        spec("tl.setClip", "Edit a clip (in/out/offset/opacity/scale/x/y/volume/fadeIn/fadeOut — scalars or keyframe [[t,v]] lists; volume v in 0..=2. transIn/transOut: {type:'slide'|'wipe'|'dip', dur:sec, color?:[r,g,b,a] 0-1} or null to clear — a set transition replaces that edge's fadeIn/fadeOut; slide/wipe animate position (wipe = slide for now), dip fades through color)", json!({"clip": n("clip id"), "in": n("source in sec"), "out": n("source out sec"), "offset": n("timeline offset sec"), "opacity": json!({"description": "scalar or [[t,v]] keyframes, 0..=1"}), "scale": json!({"description": "scalar or [[t,v]] keyframes"}), "x": json!({"description": "scalar or [[t,v]] keyframes, px right of centre"}), "y": json!({"description": "scalar or [[t,v]] keyframes, px below centre"}), "volume": json!({"description": "scalar or [[t,v]] keyframes, gain 0..=2"}), "fadeIn": n("fade-in sec"), "fadeOut": n("fade-out sec"), "transIn": json!({"type": "object", "description": "{type:'slide'|'wipe'|'dip', dur:sec, color?:[r,g,b,a]}", "properties": {"type": s("slide|wipe|dip"), "dur": n("sec"), "color": json!({"type": "array", "description": "[r,g,b,a] 0..=1 (dip only)"})}, "required": ["type", "dur"]}), "transOut": json!({"type": "object", "description": "same shape as transIn", "properties": {"type": s("slide|wipe|dip"), "dur": n("sec"), "color": json!({"type": "array", "description": "[r,g,b,a] 0..=1 (dip only)"})}, "required": ["type", "dur"]}), "text": s("text clip content"), "rate": n("playback rate: 1=realtime, 2=2x, 0.5=slow-mo; retimes media inside the same clip span"), "eq": json!({"type": "object", "description": "3-band EQ {low,mid,high} dB, or null to clear", "properties": {"low": n("dB @120Hz"), "mid": n("dB @1kHz"), "high": n("dB @8kHz")}}), "comp": json!({"type": "object", "description": "compressor {threshold(dB), ratio, attack(ms), release(ms), makeup(dB)}, or null to clear", "properties": {"threshold": n("dB <=0"), "ratio": n("compression ratio"), "attack": n("ms"), "release": n("ms"), "makeup": n("dB")}})}), &["clip"]),
        spec("tl.removeClip", "Remove a clip", json!({"clip": n("clip id")}), &["clip"]),
        spec("tl.splitClip", "Split a clip at timeline second t into two clips", json!({"clip": n("clip id"), "t": n("timeline sec")}), &["clip", "t"]),
        spec("tl.duplicateClip", "Clone a clip onto its track right after the original", json!({"clip": n("clip id")}), &["clip"]),
        spec("tl.addCue", "Append a subtitle cue (track: subtitle track index; default = first)", json!({"t": n("sec"), "dur": n("sec"), "text": s("cue text"), "track": n("subtitle track index")}), &["t", "dur", "text"]),
        spec("tl.setCue", "Edit a cue by index (t/dur/text; same 'track' rule as tl.addCue)", json!({"index": n("cue index"), "track": n("subtitle track index")}), &["index"]),
        spec("tl.removeCue", "Remove a cue by index", json!({"index": n("cue index"), "track": n("subtitle track index")}), &["index"]),
        spec("tl.probe", "ffprobe a media file (duration/fps/streams)", json!({"path": s("media path")}), &["path"]),
        spec("tl.renderFrame", "Render one frame at t seconds (PNG to 'out', else pngB64 inline)", json!({"t": n("sec"), "out": s("output path")}), &["t"]),
        spec("tl.render", "Render the timeline to mp4 via ffmpeg", json!({"out": s("output path"), "burnSubs": b("burn subtitle cues")}), &["out"]),
        spec("tl.detectSilence", "ffmpeg silencedetect on a media file → [{start,end}]", json!({"path": s("media path"), "thresholdDB": n("dB, default -35"), "minDur": n("sec, default 0.5")}), &["path"]),
        spec("tl.generateClip", "Generate a clip with a minimax-h3 h3ui-style backend and add it. endpoint is restricted to loopback hosts (127.x, ::1, localhost) unless allowRemote=true", json!({"endpoint": s("backend base URL, default http://127.0.0.1:8000; loopback only"), "allowRemote": b("allow a non-loopback endpoint (SSRF override)"), "prompt": s("generation prompt"), "track": n("track index; default = first video track, created if none"), "size": s("resolution"), "length": n("seconds"), "quality": s("quality"), "seed": n("rng seed"), "out": s("download path"), "offset": n("timeline offset sec; default = after last clip on the track"), "timeoutSecs": n("poll timeout, default 1800")}), &["prompt"]),
        spec("tl.duck", "Duck a track's audio under subtitle cues: inserts volume keyframes on every non-text clip of the track so gain dips to `amount` while any cue on cueTrack (default: first subtitle track) is active — down-ramp `attack` s before, up-ramp `release` s after", json!({"track": n("video/audio track index to duck"), "cueTrack": n("subtitle track index; default = first"), "amount": n("gain during cues, default 0.25"), "attack": n("fade-down sec, default 0.15"), "release": n("fade-up sec, default 0.3")}), &["track"]),
        spec("tl.rippleDelete", "Remove a clip and close the gap: every later clip on the same track shifts left by its duration", json!({"clip": n("clip id")}), &["clip"]),
        spec("tl.rippleInsert", "Insert a clip (same params as tl.addClip) pushing every clip at or after 'offset' on the track right by its duration", json!({"track": n("track index"), "src": s("media path"), "text": s("text clip content"), "in": n("source in sec"), "out": n("source out sec"), "dur": n("text clip seconds"), "offset": n("timeline offset sec")}), &["track"]),
        spec("tl.trim", "Trim a clip edge by delta sec (positive extends): edge 'in' moves the source start (clip keeps offset, shortens from the left); edge 'out' extends/cuts the end", json!({"clip": n("clip id"), "edge": s("'in' or 'out' (default 'out')"), "delta": n("seconds, signed")}), &["clip", "delta"]),
    ]
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
        ("volume", &mut clip.volume),
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
    for (key, slot) in [
        ("transIn", &mut clip.trans_in),
        ("transOut", &mut clip.trans_out),
    ] {
        if let Some(val) = v.get(key) {
            *slot = if val.is_null() {
                None
            } else {
                Some(serde_json::from_value(val.clone()).with_context(|| {
                    format!("bad '{key}' — expected {{type: 'slide'|'wipe'|'dip', dur, color?}}")
                })?)
            };
        }
    }
    if let Some(t) = v.get("text") {
        clip.text = if t.is_null() {
            None
        } else {
            t.as_str().map(str::to_string)
        };
    }
    if let Some(r) = v.get("rate").and_then(Value::as_f64) {
        clip.rate = r;
    }
    if let Some(val) = v.get("eq") {
        clip.eq = if val.is_null() {
            None
        } else {
            Some(
                serde_json::from_value(val.clone())
                    .with_context(|| format!("bad 'eq' — expected {{low, mid, high}} dB"))?,
            )
        };
    }
    if let Some(val) = v.get("comp") {
        clip.comp = if val.is_null() {
            None
        } else {
            Some(serde_json::from_value(val.clone()).with_context(|| {
                format!("bad 'comp' — expected {{threshold, ratio, attack, release, makeup}}")
            })?)
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
            "tl.setTrack",
            "tl.removeTrack",
            "tl.addClip",
            "tl.setClip",
            "tl.removeClip",
            "tl.splitClip",
            "tl.duplicateClip",
            "tl.addCue",
            "tl.setCue",
            "tl.removeCue",
            "tl.rippleDelete",
            "tl.rippleInsert",
            "tl.trim",
            "tl.probe",
            "tl.renderFrame",
            "tl.render",
            "tl.detectSilence",
            "tl.generateClip",
            "tl.duck",
        ] {
            assert!(ids.contains(&c), "missing {c}");
        }
        assert_eq!(ids.len(), 24);
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
    fn setclip_transitions_and_volume() {
        let mut s = sess_with_timeline();
        s.dispatch("tl.addTrack", &json!({"kind": "video"}))
            .unwrap();
        let r = s
            .dispatch(
                "tl.addClip",
                &json!({"track": 0, "src": "a.mp4", "in": 0.0, "out": 4.0}),
            )
            .unwrap();
        let clip = r["clipId"].as_u64().unwrap();
        s.dispatch(
            "tl.setClip",
            &json!({
                "clip": clip,
                "transIn": {"type": "slide", "dur": 0.5},
                "transOut": {"type": "dip", "dur": 0.4, "color": [1.0, 1.0, 1.0, 1.0]},
                "volume": [[0.0, 1.0], [2.0, 0.5]],
            }),
        )
        .unwrap();
        let doc = s.dispatch("tl.json", &json!({})).unwrap();
        let c = &doc["tracks"][0]["clips"][0];
        assert_eq!(c["transIn"]["type"], json!("slide"));
        assert_eq!(c["transOut"]["type"], json!("dip"));
        assert_eq!(c["volume"], json!([[0.0, 1.0], [2.0, 0.5]]));
        // null clears a transition
        s.dispatch("tl.setClip", &json!({"clip": clip, "transIn": null}))
            .unwrap();
        let doc = s.dispatch("tl.json", &json!({})).unwrap();
        assert!(
            doc["tracks"][0]["clips"][0]["transIn"].is_null()
                || doc["tracks"][0]["clips"][0].get("transIn").is_none()
        );
        // a bad transition type is an error, not a panic
        assert!(s
            .dispatch(
                "tl.setClip",
                &json!({"clip": clip, "transIn": {"type": "spin", "dur": 1.0}})
            )
            .is_err());
    }

    #[test]
    fn duck_inserts_volume_keyframes() {
        let mut s = sess_with_timeline();
        s.dispatch("tl.addTrack", &json!({"kind": "audio"}))
            .unwrap();
        let r = s
            .dispatch(
                "tl.addClip",
                &json!({"track": 0, "src": "a.m4a", "in": 0.0, "out": 10.0, "offset": 0.0}),
            )
            .unwrap();
        let clip = r["clipId"].as_u64().unwrap();
        s.dispatch("tl.addTrack", &json!({"kind": "subtitle"}))
            .unwrap();
        s.dispatch("tl.addCue", &json!({"t": 2.0, "dur": 2.0, "text": "hi"}))
            .unwrap();
        // no cue track arg → first subtitle track
        let r = s
            .dispatch(
                "tl.duck",
                &json!({"track": 0, "amount": 0.25, "attack": 0.5, "release": 0.5}),
            )
            .unwrap();
        assert_eq!(r["dipped"], json!(1));
        let doc = s.dispatch("tl.json", &json!({})).unwrap();
        let vol = doc["tracks"][0]["clips"][0]["volume"].as_array().unwrap();
        // expect 1→0.25 ramp from 1.5→2.0, hold, ramp back 4.0→4.5
        let pts: Vec<(f64, f64)> = vol
            .iter()
            .map(|k| (k[0].as_f64().unwrap(), k[1].as_f64().unwrap()))
            .collect();
        assert_eq!(pts, vec![(1.5, 1.0), (2.0, 0.25), (4.0, 0.25), (4.5, 1.0)]);
        // ducking a subtitle track is an error
        assert!(s.dispatch("tl.duck", &json!({"track": 1})).is_err());
        // unknown clip-less track is fine (no-op) but out-of-range errors
        assert!(s.dispatch("tl.duck", &json!({"track": 9})).is_err());
        let _ = clip;
    }

    #[test]
    fn generate_clip_rejects_remote_endpoint() {
        let mut s = sess_with_timeline();
        // SSRF guard fires before any network use — even without a track
        let e = s
            .dispatch(
                "tl.generateClip",
                &json!({"endpoint": "http://169.254.169.254/latest", "prompt": "x"}),
            )
            .unwrap_err();
        assert!(format!("{e:#}").contains("loopback"), "{e:#}");
        // loopback passes the guard (then fails on connect — that's fine)
        let e2 = s
            .dispatch(
                "tl.generateClip",
                &json!({"endpoint": "http://127.0.0.1:59998", "prompt": "x", "timeoutSecs": 5}),
            )
            .unwrap_err();
        assert!(!format!("{e2:#}").contains("loopback"), "{e2:#}");
        // allowRemote bypasses the guard (errors later on connect)
        let e3 = s
            .dispatch(
                "tl.generateClip",
                &json!({"endpoint": "http://169.254.169.254:9", "prompt": "x", "allowRemote": true, "timeoutSecs": 5}),
            )
            .unwrap_err();
        assert!(!format!("{e3:#}").contains("loopback"), "{e3:#}");
    }

    #[test]
    fn loopback_classifier() {
        assert!(is_loopback_endpoint("http://127.0.0.1:8000"));
        assert!(is_loopback_endpoint("http://localhost:8000"));
        assert!(is_loopback_endpoint("http://[::1]:8000"));
        assert!(is_loopback_endpoint("127.0.0.1:8000"));
        assert!(is_loopback_endpoint("https://127.0.0.5"));
        assert!(is_loopback_endpoint("http://user:pass@127.0.0.1:8000"));
        assert!(!is_loopback_endpoint("http://169.254.169.254"));
        assert!(!is_loopback_endpoint("http://example.com"));
        assert!(!is_loopback_endpoint("file:///etc/passwd"));
        assert!(!is_loopback_endpoint("http://127.0.0.1.evil.com"));
        assert!(!is_loopback_endpoint("http://0.0.0.0:8000"));
        assert!(!is_loopback_endpoint(""));
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
