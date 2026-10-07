//! Rendering: `render_frame` composites one frame with the image crate;
//! `render` builds a single `ffmpeg -filter_complex` invocation for the
//! whole timeline (trim/scale per clip, overlay ordered bottom→top by track
//! index, afade+adelay+amix audio, optional subtitle burn-in via
//! pre-rasterized cue PNGs — this ffmpeg build has no libass).

use std::path::Path;

use anyhow::{Context, Result};
use image::{imageops, Rgba, RgbaImage};
use serde_json::{json, Value};

use crate::ffmpeg;
use crate::model::{kf_expr, Clip, Cue, Timeline, TrackKind};
use crate::text;

/// subtitle bottom margin as a fraction of canvas height
const SUB_MARGIN: f64 = 0.05;
/// base font size for text clips / subtitles, as a fraction of canvas height
const TEXT_PX: f64 = 0.06;

// ---------------------------------------------------------------------------
// frame compositor (tl.renderFrame)

/// composite the frame at timeline time `t` into an RGBA image.
/// Black base; video clips bottom→top; text clips rasterized; subtitle cues
/// burned bottom-centre (preview shows them; `tl.render` gates on burnSubs).
pub fn render_frame(tl: &Timeline, t: f64) -> Result<RgbaImage> {
    let mut base = RgbaImage::from_pixel(tl.w, tl.h, Rgba([0, 0, 0, 255]));
    for clip in tl.clips_at(t) {
        let local = clip.local(t);
        let mut img = frame_for_clip(tl, clip, local)?;
        let scale = clip.scale_at(local);
        if (scale - 1.0).abs() > 1e-6 {
            let nw = ((img.width() as f64 * scale).round() as u32).max(1);
            let nh = ((img.height() as f64 * scale).round() as u32).max(1);
            img = imageops::resize(&img, nw, nh, imageops::FilterType::Lanczos3);
        }
        let px = (tl.w as i64 - img.width() as i64) / 2 + clip.x_at(local).round() as i64;
        let py = (tl.h as i64 - img.height() as i64) / 2 + clip.y_at(local).round() as i64;
        blend_over(&mut base, &img, px, py, clip.alpha_at(local));
    }
    for cue in tl.cues_at(t) {
        let img = cue_image(tl, cue)?;
        let px = (tl.w as i64 - img.width() as i64) / 2;
        let py = (tl.h as f64 * (1.0 - SUB_MARGIN)).round() as i64 - img.height() as i64;
        blend_over(&mut base, &img, px, py, 1.0);
    }
    Ok(base)
}

/// decoded (unscaled) source image for a clip at clip-local time `local`
fn frame_for_clip(tl: &Timeline, clip: &Clip, local: f64) -> Result<RgbaImage> {
    if clip.is_text() {
        let text = clip.text.as_deref().unwrap_or("");
        return text::rasterize(text, tl.h as f32 * TEXT_PX as f32, 1.0);
    }
    let media_t = clip.in_point + local;
    if ffmpeg::is_image_src(&clip.src) {
        return image::open(&clip.src)
            .map(|i| i.to_rgba8())
            .with_context(|| format!("cannot decode image '{}'", clip.src));
    }
    ffmpeg::extract_frame(&clip.src, media_t)
}

/// src-over alpha blending of `src` into `dst` at (x,y) with `opacity`
fn blend_over(dst: &mut RgbaImage, src: &RgbaImage, x: i64, y: i64, opacity: f64) {
    if opacity <= 0.0 {
        return;
    }
    let (dw, dh) = (dst.width() as i64, dst.height() as i64);
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + src.width() as i64).min(dw);
    let y1 = (y + src.height() as i64).min(dh);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    for dy in y0..y1 {
        for dx in x0..x1 {
            let sp = src.get_pixel((dx - x) as u32, (dy - y) as u32);
            let a = (sp[3] as f64 / 255.0) * opacity;
            if a <= 0.0 {
                continue;
            }
            let dp = dst.get_pixel_mut(dx as u32, dy as u32);
            for ch in 0..3 {
                dp[ch] = (sp[ch] as f64 * a + dp[ch] as f64 * (1.0 - a)).round() as u8;
            }
            let da = dp[3] as f64 / 255.0;
            dp[3] = ((a + da * (1.0 - a)) * 255.0).round() as u8;
        }
    }
}

fn cue_image(tl: &Timeline, cue: &Cue) -> Result<RgbaImage> {
    text::rasterize(&cue.text, tl.h as f32 * TEXT_PX as f32, 1.0)
}

// ---------------------------------------------------------------------------
// single-invocation ffmpeg render (tl.render)

/// one visual element in the overlay chain (media clip, image clip, text
/// clip PNG, or subtitle cue PNG)
struct VisualElement {
    /// ffmpeg -i index
    input: usize,
    /// timeline offset (start) seconds
    offset: f64,
    /// duration seconds
    dur: f64,
    /// keyframed transform (clip-local time)
    opacity: Vec<[f64; 2]>,
    scale: Vec<[f64; 2]>,
    x: Vec<[f64; 2]>,
    y: Vec<[f64; 2]>,
    fade_in: f64,
    fade_out: f64,
    /// subtitle cue: pinned bottom-centre instead of x/y keyframes
    is_subtitle: bool,
}

/// one audio element feeding the amix
struct AudioElement {
    input: usize,
    offset: f64,
    dur: f64,
    fade_in: f64,
    fade_out: f64,
}

/// render the whole timeline to `out` (mp4). `burn_subs` bakes subtitle cues
/// into the picture; without it they are omitted.
pub fn render(tl: &Timeline, out: &str, burn_subs: bool) -> Result<Value> {
    if !ffmpeg::have_ffmpeg() {
        anyhow::bail!("ffmpeg not found on PATH — install ffmpeg to render");
    }
    let duration = tl.duration();
    if duration <= 0.0 {
        anyhow::bail!("timeline is empty — nothing to render");
    }
    let fps = tl.fps;
    let tmp = std::env::temp_dir().join(format!("kmotion-render-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).with_context(|| format!("mkdir {}", tmp.display()))?;
    let r = render_inner(tl, out, burn_subs, duration, fps, &tmp);
    let _ = std::fs::remove_dir_all(&tmp);
    r
}

fn render_inner(
    tl: &Timeline,
    out: &str,
    burn_subs: bool,
    duration: f64,
    fps: f64,
    tmp: &Path,
) -> Result<Value> {
    let mut inputs: Vec<String> = Vec::new();
    let mut input_count = 0usize; // -i index (each push_* consumes one)
    let mut visuals: Vec<VisualElement> = Vec::new();
    let mut audios: Vec<AudioElement> = Vec::new();
    let mut probed: std::collections::HashMap<String, ffmpeg::MediaInfo> =
        std::collections::HashMap::new();

    // probe every unique media source once (audio presence + early failure)
    for clip in tl.audio_clips() {
        if clip.src.is_empty() || probed.contains_key(&clip.src) {
            continue;
        }
        let info = ffmpeg::probe(&clip.src)
            .with_context(|| format!("cannot probe clip source '{}'", clip.src))?;
        probed.insert(clip.src.clone(), info);
    }

    // visual elements, composite order: track index ascending
    for track in &tl.tracks {
        if track.kind != TrackKind::Video || track.muted {
            continue;
        }
        for clip in &track.clips {
            let dur = clip.duration();
            if dur <= 0.0 {
                continue;
            }
            let input = input_count;
            input_count += 1;
            if clip.is_text() {
                let png = tmp.join(format!("txt{input}.png"));
                let text = clip.text.as_deref().unwrap_or("");
                let img = text::rasterize(text, tl.h as f32 * TEXT_PX as f32, 1.0)?;
                img.save(&png)
                    .with_context(|| format!("write {}", png.display()))?;
                push_image_input(&mut inputs, &png, dur, fps);
            } else if ffmpeg::is_image_src(&clip.src) {
                push_image_src_input(&mut inputs, &clip.src, dur, fps);
            } else {
                push_media_input(&mut inputs, &clip.src, clip.in_point, dur);
            }
            visuals.push(VisualElement {
                input,
                offset: clip.offset,
                dur,
                opacity: clip.opacity.clone(),
                scale: clip.scale.clone(),
                x: clip.x.clone(),
                y: clip.y.clone(),
                fade_in: clip.fade_in,
                fade_out: clip.fade_out,
                is_subtitle: false,
            });
        }
    }

    // subtitle cue overlays (topmost) when burnSubs
    if burn_subs {
        for track in &tl.tracks {
            if track.kind != TrackKind::Subtitle || track.muted {
                continue;
            }
            for cue in &track.cues {
                if cue.dur <= 0.0 {
                    continue;
                }
                let input = input_count;
                input_count += 1;
                let png = tmp.join(format!("sub{input}.png"));
                let img = cue_image(tl, cue)?;
                img.save(&png)
                    .with_context(|| format!("write {}", png.display()))?;
                push_image_input(&mut inputs, &png, cue.dur, fps);
                visuals.push(VisualElement {
                    input,
                    offset: cue.t,
                    dur: cue.dur,
                    opacity: Vec::new(),
                    scale: Vec::new(),
                    x: Vec::new(),
                    y: Vec::new(),
                    fade_in: 0.0,
                    fade_out: 0.0,
                    is_subtitle: true,
                });
            }
        }
    }

    // audio elements: every clip on unmuted audio/video tracks whose source
    // has an audio stream. afade transitions + adelay offset + amix replace
    // acrossfade — acrossfade only fits back-to-back clips; our layout is
    // arbitrary offsets, so each clip is delayed to its offset and mixed.
    for clip in tl.audio_clips() {
        let dur = clip.duration();
        if clip.is_text() || dur <= 0.0 {
            continue;
        }
        if !probed.get(&clip.src).map(|m| m.has_audio).unwrap_or(false) {
            continue;
        }
        if ffmpeg::is_image_src(&clip.src) {
            continue; // image inputs carry no audio
        }
        let input = input_count;
        input_count += 1;
        push_media_input(&mut inputs, &clip.src, clip.in_point, dur);
        audios.push(AudioElement {
            input,
            offset: clip.offset,
            dur,
            fade_in: clip.fade_in,
            fade_out: clip.fade_out,
        });
    }

    // --- filter_complex graph ---------------------------------------------
    let mut g = String::new();
    let d_s = fmt6(duration);
    g.push_str(&format!(
        "color=c=black:s={}x{}:r={}:d={},format=rgba[base];",
        tl.w,
        tl.h,
        fmt6(fps),
        d_s
    ));
    let mut prev = String::from("[base]");
    for (k, el) in visuals.iter().enumerate() {
        let lt = format!("(t-{})", fmt6(el.offset)); // clip-local var for exprs
        let se = kf_expr(&el.scale, &lt, 1.0);
        let mut chain = format!(
            "[{}:v]setpts=PTS-STARTPTS,fps={},scale=w='iw*{}':h='ih*{}':eval=frame:flags=lanczos,format=rgba",
            el.input, fmt6(fps), se, se
        );
        if !el.opacity.is_empty() {
            let oe = kf_expr(&el.opacity, "T", 1.0);
            chain.push_str(&format!(
                ",geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='alpha(X,Y)*({})'",
                oe
            ));
        }
        if el.fade_in > 0.0 {
            chain.push_str(&format!(",fade=t=in:st=0:d={}:alpha=1", fmt6(el.fade_in)));
        }
        if el.fade_out > 0.0 {
            chain.push_str(&format!(
                ",fade=t=out:st={}:d={}:alpha=1",
                fmt6((el.dur - el.fade_out).max(0.0)),
                fmt6(el.fade_out)
            ));
        }
        chain.push_str(&format!("[v{k}];"));
        let (xe, ye) = if el.is_subtitle {
            (
                "(W-w)/2".to_string(),
                format!("H-h-{}", fmt6(tl.h as f64 * SUB_MARGIN)),
            )
        } else {
            (
                format!("(W-w)/2+{}", kf_expr(&el.x, &lt, 0.0)),
                format!("(H-h)/2+{}", kf_expr(&el.y, &lt, 0.0)),
            )
        };
        let stage = format!("[s{k}]");
        // half-open window [offset, end) — between() is closed on both ends
        chain.push_str(&format!(
            "{prev}[v{k}]overlay=x='{xe}':y='{ye}':eval=frame:alpha=straight:enable='gte(t,{})*lt(t,{})'{stage};",
            fmt6(el.offset),
            fmt6(el.offset + el.dur),
        ));
        g.push_str(&chain);
        prev = stage;
    }
    // final format/even-dims for h264
    g.push_str(&format!(
        "{prev}scale='trunc(iw/2)*2':'trunc(ih/2)*2',format=yuv420p[vout]"
    ));

    // audio chain
    let mut audio_map = false;
    if !audios.is_empty() {
        g.push(';');
        for (k, a) in audios.iter().enumerate() {
            let mut chain = format!(
                "[{}:a]asetpts=PTS-STARTPTS,aresample=48000,aformat=channel_layouts=stereo",
                a.input
            );
            if a.fade_in > 0.0 {
                chain.push_str(&format!(",afade=t=in:st=0:d={}", fmt6(a.fade_in)));
            }
            if a.fade_out > 0.0 {
                chain.push_str(&format!(
                    ",afade=t=out:st={}:d={}",
                    fmt6((a.dur - a.fade_out).max(0.0)),
                    fmt6(a.fade_out)
                ));
            }
            chain.push_str(&format!(
                ",adelay=delays={}:all=1[a{k}];",
                (a.offset * 1000.0).round() as u64
            ));
            g.push_str(&chain);
        }
        if audios.len() == 1 {
            g.push_str(&format!("[a0]atrim=duration={d_s}[aout]"));
        } else {
            let ins: String = (0..audios.len()).map(|k| format!("[a{k}]")).collect();
            g.push_str(&format!(
                "{ins}amix=inputs={}:duration=longest:normalize=0,atrim=duration={d_s}[aout]",
                audios.len()
            ));
        }
        audio_map = true;
    }

    // --- run ---------------------------------------------------------------
    let mut args = vec!["-hide_banner".into(), "-y".into()];
    args.extend(inputs);
    args.push("-filter_complex".into());
    args.push(g.clone());
    args.push("-map".into());
    args.push("[vout]".into());
    if audio_map {
        args.push("-map".into());
        args.push("[aout]".into());
    }
    args.extend([
        "-c:v".into(),
        "libx264".into(),
        "-crf".into(),
        "18".into(),
        "-preset".into(),
        "medium".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-r".into(),
        fmt6(fps),
    ]);
    if audio_map {
        args.extend(["-c:a".into(), "aac".into(), "-b:a".into(), "192k".into()]);
    }
    args.extend(["-movflags".into(), "+faststart".into(), out.into()]);

    log::info!("ffmpeg {}", args.join(" "));
    ffmpeg::run_ffmpeg(&args).with_context(|| format!("render failed for '{out}'"))?;

    let info = ffmpeg::probe(out).unwrap_or(ffmpeg::MediaInfo {
        duration,
        fps,
        w: tl.w,
        h: tl.h,
        has_video: true,
        has_audio: audio_map,
        streams: Vec::new(),
    });
    Ok(json!({
        "path": out,
        "duration": info.duration.max(0.0),
        "w": if info.w > 0 { info.w } else { tl.w },
        "h": if info.h > 0 { info.h } else { tl.h },
        "clips": visuals.len(),
        "audioStreams": audios.len(),
        "burnSubs": burn_subs,
    }))
}

fn push_media_input(inputs: &mut Vec<String>, src: &str, in_point: f64, dur: f64) {
    inputs.push("-ss".into());
    inputs.push(fmt6(in_point));
    inputs.push("-t".into());
    inputs.push(fmt6(dur));
    inputs.push("-i".into());
    inputs.push(src.into());
}

fn push_image_input(inputs: &mut Vec<String>, png: &Path, dur: f64, fps: f64) {
    inputs.push("-loop".into());
    inputs.push("1".into());
    inputs.push("-framerate".into());
    inputs.push(fmt6(fps));
    inputs.push("-t".into());
    inputs.push(fmt6(dur));
    inputs.push("-i".into());
    inputs.push(png.to_string_lossy().into_owned());
}

fn push_image_src_input(inputs: &mut Vec<String>, src: &str, dur: f64, fps: f64) {
    inputs.push("-loop".into());
    inputs.push("1".into());
    inputs.push("-framerate".into());
    inputs.push(fmt6(fps));
    inputs.push("-t".into());
    inputs.push(fmt6(dur));
    inputs.push("-i".into());
    inputs.push(src.into());
}

fn fmt6(v: f64) -> String {
    if v.is_finite() {
        format!("{:.6}", v)
    } else {
        "0.000000".into()
    }
}
