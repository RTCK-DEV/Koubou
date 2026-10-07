//! Timeline model: a `.kmotion` file = one JSON document.
//!
//! A timeline is a stack of tracks; track 0 composites at the bottom, later
//! tracks on top (NLE convention: higher track = higher z). Video tracks hold
//! media clips (and text clips — `src` empty, `text` set); audio tracks hold
//! media clips whose audio is mixed into the render; subtitle tracks hold
//! cues.
//!
//! Clip `inPoint`/`outPoint` are seconds into the source media; `offset` is
//! the clip's start on the timeline in seconds, so a clip covers
//! `offset .. offset + (outPoint - inPoint)`. Keyframe lists are `[[t, v]]`
//! pairs in *clip-local* seconds (0 = clip start on the timeline),
//! linear-interpolated, clamped at the ends. `x`/`y` are pixel offsets from
//! the canvas centre (0,0 centres the clip), matching NLE "position" UX.
//!
//! Everything serializes camelCase for JSON and the control channel.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// current .kmotion document version
pub const KMT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Timeline {
    #[serde(default = "version")]
    pub version: u32,
    pub name: String,
    /// canvas width in pixels
    pub w: u32,
    /// canvas height in pixels
    pub h: u32,
    /// frames per second
    pub fps: f64,
    /// bottom -> top
    #[serde(default)]
    pub tracks: Vec<Track>,
    /// next element id (monotonic, survives serialization)
    #[serde(default = "one")]
    pub next_id: u64,
}

fn version() -> u32 {
    KMT_VERSION
}

fn one() -> u64 {
    1
}

impl Timeline {
    pub fn new(name: impl Into<String>, w: u32, h: u32, fps: f64) -> Timeline {
        Timeline {
            version: KMT_VERSION,
            name: name.into(),
            w,
            h,
            fps,
            tracks: Vec::new(),
            next_id: 1,
        }
    }

    /// append a track; returns its index (position in `tracks`)
    pub fn add_track(&mut self, kind: TrackKind) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.tracks.push(Track {
            id,
            kind,
            name: String::new(),
            muted: false,
            clips: Vec::new(),
            cues: Vec::new(),
        });
        self.tracks.len() - 1
    }

    pub fn track(&self, index: usize) -> Option<&Track> {
        self.tracks.get(index)
    }

    pub fn track_mut(&mut self, index: usize) -> Option<&mut Track> {
        self.tracks.get_mut(index)
    }

    /// add a clip to a track; returns the assigned clip id
    pub fn add_clip(&mut self, track: usize, mut clip: Clip) -> Result<u64> {
        let t = self
            .tracks
            .get_mut(track)
            .with_context(|| format!("track index {track} out of range"))?;
        if t.kind == TrackKind::Subtitle {
            anyhow::bail!("subtitle tracks hold cues, not clips (use tl.addCue)");
        }
        let id = self.next_id;
        self.next_id += 1;
        clip.id = id;
        t.clips.push(clip);
        Ok(id)
    }

    pub fn clip(&self, id: u64) -> Option<&Clip> {
        self.tracks
            .iter()
            .flat_map(|t| &t.clips)
            .find(|c| c.id == id)
    }

    pub fn clip_mut(&mut self, id: u64) -> Option<&mut Clip> {
        self.tracks
            .iter_mut()
            .flat_map(|t| &mut t.clips)
            .find(|c| c.id == id)
    }

    /// (track index, clip index) of a clip id, for diagnostics
    pub fn find_clip(&self, id: u64) -> Option<(usize, usize)> {
        for (ti, t) in self.tracks.iter().enumerate() {
            if let Some(ci) = t.clips.iter().position(|c| c.id == id) {
                return Some((ti, ci));
            }
        }
        None
    }

    pub fn remove_clip(&mut self, id: u64) -> Option<Clip> {
        let (ti, ci) = self.find_clip(id)?;
        Some(self.tracks.get_mut(ti)?.clips.remove(ci))
    }

    /// add a subtitle cue; `track` picks a subtitle track — when None the
    /// first subtitle track is used. Errors when none exists.
    pub fn add_cue(&mut self, track: Option<usize>, cue: Cue) -> Result<()> {
        let idx = match track {
            Some(i) => {
                let t = self
                    .tracks
                    .get(i)
                    .with_context(|| format!("track index {i} out of range"))?;
                if t.kind != TrackKind::Subtitle {
                    anyhow::bail!("track {i} is {:?}, not a subtitle track", t.kind);
                }
                i
            }
            None => self
                .tracks
                .iter()
                .position(|t| t.kind == TrackKind::Subtitle)
                .context("no subtitle track — add one with tl.addTrack kind=subtitle")?,
        };
        let t = &mut self.tracks[idx];
        t.cues.push(cue);
        t.cues
            .sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
        Ok(())
    }

    /// timeline end in seconds: latest clip end or cue end
    pub fn duration(&self) -> f64 {
        let mut d = 0.0_f64;
        for t in &self.tracks {
            for c in &t.clips {
                d = d.max(c.end());
            }
            for c in &t.cues {
                d = d.max(c.end());
            }
        }
        d
    }

    /// video/audio clips visible at timeline time `t`, in composite order
    /// (bottom -> top): track index ascending, then clip order.
    pub fn clips_at(&self, t: f64) -> Vec<&Clip> {
        let mut out = Vec::new();
        for track in &self.tracks {
            if track.kind != TrackKind::Video || track.muted {
                continue;
            }
            for c in &track.clips {
                if c.active(t) {
                    out.push(c);
                }
            }
        }
        out
    }

    /// subtitle cues active at timeline time `t` (unmuted tracks, in order)
    pub fn cues_at(&self, t: f64) -> Vec<&Cue> {
        let mut out = Vec::new();
        for track in &self.tracks {
            if track.kind != TrackKind::Subtitle || track.muted {
                continue;
            }
            for c in &track.cues {
                if c.active(t) {
                    out.push(c);
                }
            }
        }
        out
    }

    /// media clips on audio tracks plus video-track clips whose source may
    /// carry audio, each with the clip's parent track muted flag resolved.
    pub fn audio_clips(&self) -> Vec<&Clip> {
        let mut out = Vec::new();
        for t in &self.tracks {
            if t.muted || t.kind == TrackKind::Subtitle {
                continue;
            }
            out.extend(t.clips.iter());
        }
        out
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn from_json(s: &str) -> Result<Timeline> {
        let mut tl: Timeline = serde_json::from_str(s).context("invalid .kmotion timeline JSON")?;
        tl.sanitize();
        Ok(tl)
    }

    /// clamp hostile values loaded from disk: NaN/negative numbers, empty
    /// dims, unsorted keyframes.
    pub fn sanitize(&mut self) {
        self.w = self.w.max(2);
        self.h = self.h.max(2);
        if !self.fps.is_finite() || self.fps <= 0.0 {
            self.fps = 30.0;
        }
        for t in &mut self.tracks {
            for c in &mut t.clips {
                c.sanitize();
            }
            for c in &mut t.cues {
                c.sanitize();
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.to_json()).with_context(|| format!("write {}", path.display()))
    }

    pub fn load(path: &Path) -> Result<Timeline> {
        let s =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        Timeline::from_json(&s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TrackKind {
    Video,
    Audio,
    Subtitle,
}

impl TrackKind {
    pub fn parse(s: &str) -> Option<TrackKind> {
        match s {
            "video" => Some(TrackKind::Video),
            "audio" => Some(TrackKind::Audio),
            "subtitle" => Some(TrackKind::Subtitle),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: u64,
    pub kind: TrackKind,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub muted: bool,
    /// video/audio clips (subtitle tracks leave this empty)
    #[serde(default)]
    pub clips: Vec<Clip>,
    /// subtitle cues (subtitle tracks only)
    #[serde(default)]
    pub cues: Vec<Cue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    pub id: u64,
    /// media path; empty for text clips
    #[serde(default)]
    pub src: String,
    /// text content for text clips (src == "")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// seconds into the source media where this clip starts
    #[serde(default)]
    pub in_point: f64,
    /// seconds into the source media where this clip ends
    #[serde(default)]
    pub out_point: f64,
    /// timeline position of the clip start, seconds
    #[serde(default)]
    pub offset: f64,
    /// opacity keyframes [[t, v]] clip-local seconds, v in 0..=1
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opacity: Vec<[f64; 2]>,
    /// scale keyframes [[t, v]]; 1.0 = source pixel size
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scale: Vec<[f64; 2]>,
    /// x keyframes [[t, v]]; pixels right of canvas centre
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub x: Vec<[f64; 2]>,
    /// y keyframes [[t, v]]; pixels below canvas centre
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub y: Vec<[f64; 2]>,
    /// fade-in transition length, seconds
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fade_in: f64,
    /// fade-out transition length, seconds
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fade_out: f64,
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

impl Clip {
    /// media clip constructor; id is assigned by `Timeline::add_clip`
    pub fn media(src: impl Into<String>, in_point: f64, out_point: f64, offset: f64) -> Clip {
        Clip {
            id: 0,
            src: src.into(),
            text: None,
            in_point,
            out_point,
            offset,
            opacity: Vec::new(),
            scale: Vec::new(),
            x: Vec::new(),
            y: Vec::new(),
            fade_in: 0.0,
            fade_out: 0.0,
        }
    }

    /// text clip constructor; `dur` sets out_point = dur
    pub fn text(text: impl Into<String>, offset: f64, dur: f64) -> Clip {
        let mut c = Clip::media("", 0.0, dur, offset);
        c.text = Some(text.into());
        c
    }

    pub fn is_text(&self) -> bool {
        self.src.is_empty()
    }

    /// clip span on the timeline, seconds
    pub fn duration(&self) -> f64 {
        (self.out_point - self.in_point).max(0.0)
    }

    /// timeline end of the clip, seconds
    pub fn end(&self) -> f64 {
        self.offset + self.duration()
    }

    /// is the clip visible at timeline time `t`
    pub fn active(&self, t: f64) -> bool {
        t >= self.offset && t < self.end()
    }

    /// clip-local time for timeline time `t` (0 = clip start)
    pub fn local(&self, t: f64) -> f64 {
        t - self.offset
    }

    pub fn opacity_at(&self, local: f64) -> f64 {
        eval_kf(&self.opacity, local, 1.0).clamp(0.0, 1.0)
    }

    pub fn scale_at(&self, local: f64) -> f64 {
        eval_kf(&self.scale, local, 1.0).clamp(0.0, 100.0)
    }

    pub fn x_at(&self, local: f64) -> f64 {
        eval_kf(&self.x, local, 0.0)
    }

    pub fn y_at(&self, local: f64) -> f64 {
        eval_kf(&self.y, local, 0.0)
    }

    /// fade-in/out gain at clip-local time, 0..=1
    pub fn fade_gain(&self, local: f64) -> f64 {
        let dur = self.duration();
        let mut g = 1.0_f64;
        if self.fade_in > 0.0 && local < self.fade_in {
            g *= (local / self.fade_in).clamp(0.0, 1.0);
        }
        if self.fade_out > 0.0 && local > dur - self.fade_out {
            g *= ((dur - local) / self.fade_out).clamp(0.0, 1.0);
        }
        g
    }

    /// combined opacity * fade gain at clip-local time
    pub fn alpha_at(&self, local: f64) -> f64 {
        (self.opacity_at(local) * self.fade_gain(local)).clamp(0.0, 1.0)
    }

    /// clamp hostile numbers: NaN/negative spans, unsorted keyframes
    pub(crate) fn sanitize(&mut self) {
        for v in [
            &mut self.in_point,
            &mut self.out_point,
            &mut self.offset,
            &mut self.fade_in,
            &mut self.fade_out,
        ] {
            if !v.is_finite() {
                *v = 0.0;
            }
        }
        self.offset = self.offset.max(0.0);
        self.in_point = self.in_point.max(0.0);
        self.out_point = self.out_point.max(self.in_point);
        self.fade_in = self.fade_in.clamp(0.0, self.duration());
        self.fade_out = self.fade_out.clamp(0.0, self.duration());
        for kfs in [&mut self.opacity, &mut self.scale, &mut self.x, &mut self.y] {
            kfs.retain(|k| k[0].is_finite() && k[1].is_finite());
            kfs.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal));
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cue {
    /// timeline start, seconds
    pub t: f64,
    /// duration, seconds
    pub dur: f64,
    pub text: String,
}

impl Cue {
    pub fn end(&self) -> f64 {
        self.t + self.dur.max(0.0)
    }

    pub fn active(&self, t: f64) -> bool {
        t >= self.t && t < self.end()
    }

    fn sanitize(&mut self) {
        if !self.t.is_finite() {
            self.t = 0.0;
        }
        if !self.dur.is_finite() {
            self.dur = 0.0;
        }
        self.t = self.t.max(0.0);
        self.dur = self.dur.max(0.0);
    }
}

/// linear-interpolated keyframe evaluation; `kfs` is `[[t, v]]` sorted by t.
/// Empty → `default`; before the first key → first value; after the last →
/// last value.
pub fn eval_kf(kfs: &[[f64; 2]], t: f64, default: f64) -> f64 {
    match kfs.first() {
        None => return default,
        Some(k) if t <= k[0] => return k[1],
        _ => {}
    }
    for w in kfs.windows(2) {
        let (t0, v0) = (w[0][0], w[0][1]);
        let (t1, v1) = (w[1][0], w[1][1]);
        if t <= t1 {
            if t1 <= t0 {
                return v1;
            }
            let f = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
            return v0 + (v1 - v0) * f;
        }
    }
    kfs[kfs.len() - 1][1]
}

/// build an ffmpeg expression for a keyframed value: nested `if(lte(var,t),..)`
/// producing piecewise-linear output in `var` (e.g. "t" or "T"). The caller
/// puts the whole thing in single quotes inside the filtergraph.
pub fn kf_expr(kfs: &[[f64; 2]], var: &str, default: f64) -> String {
    let mut ks: Vec<[f64; 2]> = kfs
        .iter()
        .copied()
        .filter(|k| k[0].is_finite() && k[1].is_finite())
        .collect();
    ks.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal));
    if ks.is_empty() {
        return num(default);
    }
    // innermost tail: last keyframe value
    let mut expr = num(ks[ks.len() - 1][1]);
    for i in (0..ks.len() - 1).rev() {
        let (t0, v0) = (ks[i][0], ks[i][1]);
        let (t1, v1) = (ks[i + 1][0], ks[i + 1][1]);
        if t1 > t0 {
            let lerp = format!(
                "{}+({}-{})*({}-{})/({}-{})",
                num(v0),
                num(v1),
                num(v0),
                var,
                num(t0),
                num(t1),
                num(t0)
            );
            expr = format!(
                "if(lte({},{}),{},if(lte({},{}),{},{}))",
                var,
                num(t0),
                num(v0),
                var,
                num(t1),
                lerp,
                expr
            );
        } else {
            // duplicate timestamp: hard cut at t0
            expr = format!("if(lte({},{}),{},{})", var, num(t0), num(v0), expr);
        }
    }
    // first keyframe guard (t <= t0 handled by outermost if above)
    expr
}

/// ffmpeg-expr-safe number formatting
fn num(v: f64) -> String {
    if v.is_finite() {
        format!("{:.6}", v)
    } else {
        "0.000000".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kf_eval_basic() {
        assert_eq!(eval_kf(&[], 3.0, 0.7), 0.7);
        let k = [[1.0, 0.0], [3.0, 1.0]];
        assert_eq!(eval_kf(&k, 0.0, 1.0), 0.0);
        assert_eq!(eval_kf(&k, 2.0, 1.0), 0.5);
        assert_eq!(eval_kf(&k, 9.0, 1.0), 1.0);
        let single = [[0.5, 0.25]];
        assert_eq!(eval_kf(&single, 10.0, 1.0), 0.25);
        assert_eq!(eval_kf(&single, 0.0, 1.0), 0.25);
    }

    #[test]
    fn kf_expr_runs_in_ffmpeg_shape() {
        // shape only — the ffmpeg eval is verified by the integration render test
        let e = kf_expr(&[[0.0, 0.0], [1.0, 1.0]], "t", 0.0);
        assert!(e.starts_with("if(lte(t,"));
        assert!(e.contains("(t-"));
        assert_eq!(kf_expr(&[], "t", 0.5), "0.500000");
    }

    #[test]
    fn timeline_duration_calc() {
        let mut tl = Timeline::new("t", 320, 240, 24.0);
        let v = tl.add_track(TrackKind::Video);
        tl.add_clip(v, Clip::media("a.mp4", 0.0, 2.0, 1.5)).unwrap();
        tl.add_clip(v, Clip::media("b.mp4", 0.0, 4.0, 0.5)).unwrap();
        let s = tl.add_track(TrackKind::Subtitle);
        tl.add_cue(
            Some(s),
            Cue {
                t: 9.0,
                dur: 2.0,
                text: "x".into(),
            },
        )
        .unwrap();
        assert_eq!(tl.duration(), 11.0);
    }

    #[test]
    fn serde_round_trip() {
        let mut tl = Timeline::new("demo", 1920, 1080, 30.0);
        let v = tl.add_track(TrackKind::Video);
        let mut c = Clip::media("a.mp4", 1.0, 3.0, 0.0);
        c.opacity = vec![[0.0, 0.0], [1.0, 1.0]];
        c.x = vec![[0.0, -50.0]];
        c.fade_in = 0.5;
        tl.add_clip(v, c).unwrap();
        let s = tl.add_track(TrackKind::Subtitle);
        tl.add_cue(
            Some(s),
            Cue {
                t: 0.0,
                dur: 1.0,
                text: "hi".into(),
            },
        )
        .unwrap();
        let j = tl.to_json();
        let back = Timeline::from_json(&j).unwrap();
        assert_eq!(back.w, 1920);
        assert_eq!(back.tracks.len(), 2);
        let c2 = &back.tracks[v].clips[0];
        assert_eq!(c2.in_point, 1.0);
        assert_eq!(c2.opacity, vec![[0.0, 0.0], [1.0, 1.0]]);
        assert_eq!(c2.fade_in, 0.5);
        assert_eq!(back.tracks[s].cues[0].text, "hi");
        // camelCase keys
        assert!(j.contains("\"inPoint\""));
        assert!(j.contains("\"fadeIn\""));
        assert!(j.contains("\"nextId\""));
        assert!(j.contains("\"kind\": \"video\""));
    }

    #[test]
    fn clip_fade_gain() {
        let mut c = Clip::media("a", 0.0, 4.0, 0.0);
        c.fade_in = 1.0;
        c.fade_out = 1.0;
        assert_eq!(c.fade_gain(0.5), 0.5);
        assert_eq!(c.fade_gain(2.0), 1.0);
        assert_eq!(c.fade_gain(3.5), 0.5);
        assert_eq!(c.fade_gain(4.5), 0.0);
    }

    #[test]
    fn hostile_json_does_not_panic() {
        let bad = r#"{"name":"x","w":0,"h":0,"fps":-3,"tracks":[{"id":1,"kind":"video","clips":[{"id":1,"src":"","inPoint":null,"outPoint":"wat","offset":-5,"opacity":[[null,2],[1,"a"]],"fadeIn":99}]}]}"#;
        // serde will fail on the wrong types — that's an error, not a panic
        let r = Timeline::from_json(bad);
        assert!(r.is_err());
        let nan = r#"{"name":"x","w":640,"h":480,"fps":0,"tracks":[{"id":1,"kind":"video","clips":[{"id":1,"src":"a","inPoint":5,"outPoint":2,"offset":-1,"fadeIn":99,"opacity":[[3,9],[1,0.5]]}]}]}"#;
        let tl = Timeline::from_json(nan).unwrap();
        let c = &tl.tracks[0].clips[0];
        assert_eq!(tl.fps, 30.0);
        assert_eq!(c.offset, 0.0);
        assert_eq!(c.out_point, c.in_point); // clamped to >= in
        assert_eq!(c.fade_in, 0.0); // fadeIn clamped to duration (0)
        assert_eq!(c.opacity[0], [1.0, 0.5]); // sorted by t
    }
}
