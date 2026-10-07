//! ffmpeg/ffprobe/curl subprocess wrappers. Every external tool failure —
//! binary missing, non-zero exit, unparseable output — becomes an anyhow
//! error with the tool's stderr tail, never a panic.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result};
use image::RgbaImage;
use serde_json::Value;

/// GUI apps on macOS get a sparse PATH (`/usr/bin:/bin:…`) — brew-installed
/// tools like ffmpeg live at /opt/homebrew/bin. Resolve: KOUBOU_<NAME> env
/// override → well-known absolute paths → bare name (PATH lookup).
pub fn tool_path(name: &str) -> String {
    let env_key = format!("KOUBOU_{}", name.to_uppercase());
    if let Ok(p) = std::env::var(&env_key) {
        if std::path::Path::new(&p).exists() {
            return p;
        }
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"] {
        let p = format!("{dir}/{name}");
        if std::path::Path::new(&p).exists() {
            return p;
        }
    }
    name.to_string()
}

/// is `ffmpeg` available (GUI-safe)?
pub fn have_ffmpeg() -> bool {
    Command::new(tool_path("ffmpeg"))
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// is `ffprobe` available (GUI-safe)?
pub fn have_ffprobe() -> bool {
    Command::new(tool_path("ffprobe"))
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn tail(stderr: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(stderr);
    let s = s.trim();
    if s.len() <= max {
        return s.to_string();
    }
    s.chars()
        .skip(s.chars().count().saturating_sub(max))
        .collect()
}

/// run a subprocess; non-zero exit is an error carrying the stderr tail
fn run(prog: &str, args: &[String], stdin: Option<&[u8]>) -> Result<Output> {
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(match stdin {
            Some(_) => Stdio::piped(),
            None => Stdio::null(),
        });
    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to start '{prog}' — is it installed?"))?;
    if let Some(data) = stdin {
        if let Some(mut w) = child.stdin.take() {
            let _ = w.write_all(data); // broken pipe is fine; child output decides
        }
    }
    let out = child
        .wait_with_output()
        .with_context(|| format!("{prog} did not finish"))?;
    if !out.status.success() {
        anyhow::bail!("{prog} exited {}: {}", out.status, tail(&out.stderr, 1200));
    }
    Ok(out)
}

pub fn run_ffmpeg(args: &[String]) -> Result<Output> {
    run(&tool_path("ffmpeg"), args, None)
}

pub fn run_ffprobe(args: &[String]) -> Result<Output> {
    run(&tool_path("ffprobe"), args, None)
}

fn s(v: &impl std::fmt::Display) -> String {
    v.to_string()
}

/// condensed probe of a media file
#[derive(Debug, Clone)]
pub struct MediaInfo {
    pub duration: f64,
    pub fps: f64,
    pub w: u32,
    pub h: u32,
    pub has_video: bool,
    pub has_audio: bool,
    /// per-stream summary in input order
    pub streams: Vec<Value>,
}

/// `ffprobe -show_format -show_streams` → condensed info
pub fn probe(path: &str) -> Result<MediaInfo> {
    let out = run_ffprobe(&[
        "-v".into(),
        "quiet".into(),
        "-print_format".into(),
        "json".into(),
        "-show_format".into(),
        "-show_streams".into(),
        path.into(),
    ])
    .with_context(|| format!("ffprobe failed for '{path}'"))?;
    let j: Value = serde_json::from_slice(&out.stdout)
        .with_context(|| format!("ffprobe returned invalid JSON for '{path}'"))?;
    let mut info = MediaInfo {
        duration: j
            .get("format")
            .and_then(|f| f.get("duration"))
            .and_then(Value::as_str)
            .and_then(|d| d.parse().ok())
            .unwrap_or(0.0),
        fps: 0.0,
        w: 0,
        h: 0,
        has_video: false,
        has_audio: false,
        streams: Vec::new(),
    };
    if let Some(arr) = j.get("streams").and_then(Value::as_array) {
        for st in arr {
            let kind = st.get("codec_type").and_then(Value::as_str).unwrap_or("");
            let codec = st.get("codec_name").and_then(Value::as_str).unwrap_or("");
            let mut entry = serde_json::json!({
                "index": st.get("index").and_then(Value::as_u64).unwrap_or(0),
                "codecType": kind,
                "codecName": codec,
            });
            if kind == "video" {
                info.has_video = true;
                let w = st.get("width").and_then(Value::as_u64).unwrap_or(0) as u32;
                let h = st.get("height").and_then(Value::as_u64).unwrap_or(0) as u32;
                if info.w == 0 {
                    info.w = w;
                    info.h = h;
                }
                if info.fps == 0.0 {
                    info.fps = parse_rate(
                        st.get("avg_frame_rate")
                            .or_else(|| st.get("r_frame_rate"))
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                    );
                }
                entry["width"] = serde_json::json!(w);
                entry["height"] = serde_json::json!(h);
            }
            if kind == "audio" {
                info.has_audio = true;
            }
            if let Some(d) = st.get("duration").and_then(Value::as_str) {
                entry["duration"] = serde_json::json!(d.parse::<f64>().unwrap_or(0.0));
                if info.duration == 0.0 {
                    info.duration = d.parse().unwrap_or(0.0);
                }
            }
            info.streams.push(entry);
        }
    }
    Ok(info)
}

/// "30000/1001" or "24/1" → fps float
fn parse_rate(r: &str) -> f64 {
    let mut it = r.split('/');
    let n: f64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let d: f64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    if d > 0.0 {
        n / d
    } else {
        0.0
    }
}

/// extract one decoded frame at `t` seconds from `src` as RGBA pixels.
/// `-ss` before `-i` is frame-accurate in modern ffmpeg (accurate_seek).
pub fn extract_frame(src: &str, t: f64) -> Result<RgbaImage> {
    let out = run_ffmpeg(&[
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-ss".into(),
        s(&t.max(0.0)),
        "-i".into(),
        src.into(),
        "-frames:v".into(),
        "1".into(),
        "-f".into(),
        "image2pipe".into(),
        "-vcodec".into(),
        "png".into(),
        "-".into(),
    ])
    .with_context(|| format!("ffmpeg could not decode a frame at {t:.3}s from '{src}'"))?;
    if out.stdout.is_empty() {
        anyhow::bail!("ffmpeg produced no frame at {t:.3}s from '{src}' (past EOF?)");
    }
    let img = image::load_from_memory(&out.stdout)
        .with_context(|| format!("ffmpeg output was not a decodable PNG for '{src}'"))?;
    Ok(img.to_rgba8())
}

/// ffmpeg silencedetect → list of (start, end) silence spans.
/// stderr lines look like:
///   [silencedetect @ ..] silence_start: 1.23
///   [silencedetect @ ..] silence_end: 2.46 | silence_duration: 1.23
/// A trailing unclosed silence ends at the media duration.
pub fn detect_silence(path: &str, threshold_db: f64, min_dur: f64) -> Result<Vec<(f64, f64)>> {
    let args = vec![
        "-hide_banner".into(),
        "-nostats".into(),
        "-i".into(),
        path.into(),
        "-af".into(),
        format!(
            "silencedetect=noise={}dB:d={}",
            threshold_db,
            min_dur.max(0.001)
        ),
        "-f".into(),
        "null".into(),
        "-".into(),
    ];
    // silencedetect exits 0 with analysis on stderr
    let out = run_ffmpeg(&args).with_context(|| format!("silencedetect failed for '{path}'"))?;
    let text = String::from_utf8_lossy(&out.stderr);
    let mut spans = parse_silence_log(&text);
    // if a silence never closed, clamp it to the probed duration
    if let Some((_, end)) = spans.last_mut() {
        if *end < 0.0 {
            *end = probe(path).map(|m| m.duration).unwrap_or(0.0);
        }
    }
    spans.retain(|(a, b)| *b > *a);
    Ok(spans)
}

/// parse silencedetect log text into [(start, end)]; unclosed trailing
/// silence gets end = -1.0 (caller clamps to duration).
pub fn parse_silence_log(text: &str) -> Vec<(f64, f64)> {
    let mut spans = Vec::new();
    let mut open: Option<f64> = None;
    for line in text.lines() {
        if let Some(pos) = line.find("silence_start:") {
            if let Some(v) = parse_float_after(&line[pos + "silence_start:".len()..]) {
                open = Some(v);
            }
        } else if let Some(pos) = line.find("silence_end:") {
            if let (Some(start), Some(end)) = (
                open.take(),
                parse_float_after(&line[pos + "silence_end:".len()..]),
            ) {
                spans.push((start, end));
            }
        }
    }
    if let Some(start) = open {
        spans.push((start, -1.0));
    }
    spans
}

/// first float token after a `name: ` prefix — stops at space, '|' or EOL
fn parse_float_after(s: &str) -> Option<f64> {
    let t = s.trim_start();
    let end = t
        .find(|c: char| {
            !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E')
        })
        .unwrap_or(t.len());
    t[..end].parse().ok()
}

/// is a file extension a still image (vs video/audio)?
pub fn is_image_src(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default()
            .as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "tif" | "tiff" | "bmp" | "gif"
    )
}

/// `curl` with sane timeouts; returns body bytes. `fail` adds -f so HTTP
/// error statuses fail instead of producing a body.
pub fn curl(url: &str, args: &[String], timeout_secs: u64) -> Result<Vec<u8>> {
    let mut all = vec!["-sS".into(), "-m".into(), s(&timeout_secs)];
    all.extend_from_slice(args);
    all.push(url.into());
    let out = run(&tool_path("curl"), &all, None).with_context(|| format!("curl {url} failed"))?;
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_silence_log() {
        let log = r#"Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'a.m4a':
  Duration: 00:00:04.00, start: 0.000000, bitrate: 66 kb/s
[silencedetect @ 0x13b70] silence_start: 0.501167
[silencedetect @ 0x13b70] silence_end: 1.5 | silence_duration: 0.998833
[silencedetect @ 0x13b70] silence_start: 2.25
[silencedetect @ 0x13b70] silence_end: 2.875042 | silence_duration: 0.625042
"#;
        let spans = parse_silence_log(log);
        assert_eq!(spans.len(), 2);
        assert!((spans[0].0 - 0.501167).abs() < 1e-6);
        assert!((spans[0].1 - 1.5).abs() < 1e-6);
        assert!((spans[1].0 - 2.25).abs() < 1e-6);
    }

    #[test]
    fn unclosed_silence_marked_open() {
        let spans = parse_silence_log("[silencedetect] silence_start: 3.0\n");
        assert_eq!(spans, vec![(3.0, -1.0)]);
    }

    #[test]
    fn no_silence_empty() {
        assert!(parse_silence_log("nothing\nhere\n").is_empty());
    }

    #[test]
    fn rate_parse() {
        assert!((parse_rate("30000/1001") - 29.97).abs() < 0.01);
        assert_eq!(parse_rate("24/1"), 24.0);
        assert_eq!(parse_rate("0/0"), 0.0);
        assert_eq!(parse_rate(""), 0.0);
    }
}
