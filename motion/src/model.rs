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

/// per-edge clip transition (`transIn`/`transOut`): `slide`/`wipe` animate
/// the clip's position (wipe rides the same motion, no separate mask yet);
/// `dip` fades through a solid colour via an underlay element + alpha ramp.
/// A transition replaces that edge's plain fade when set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    /// "slide" | "wipe" | "dip"
    #[serde(rename = "type")]
    pub kind: TransKind,
    /// transition length in seconds
    #[serde(default)]
    pub dur: f64,
    /// dip colour [r,g,b,a] 0..=1 (default black)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[f32; 4]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransKind {
    Wipe,
    Slide,
    Dip,
}

impl Transition {
    /// dip colour as rgba bytes (src-over), default opaque black
    pub fn dip_rgba8(&self) -> [u8; 4] {
        let c = self.color.unwrap_or([0.0, 0.0, 0.0, 1.0]);
        [
            (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
            (c[3].clamp(0.0, 1.0) * 255.0).round() as u8,
        ]
    }

    /// dip colour as an ffmpeg `color=` hex literal (`0xRRGGBB`)
    pub fn dip_hex(&self) -> String {
        let c = self.dip_rgba8();
        format!("0x{:02x}{:02x}{:02x}", c[0], c[1], c[2])
    }

    /// clamp hostile values; `clip_dur` caps the window
    pub(crate) fn sanitize(&mut self, clip_dur: f64) {
        if !self.dur.is_finite() {
            self.dur = 0.0;
        }
        self.dur = self.dur.clamp(0.0, clip_dur.max(0.0));
        if let Some(c) = &mut self.color {
            for v in c.iter_mut() {
                if !v.is_finite() {
                    *v = 0.0;
                }
                *v = v.clamp(0.0, 1.0);
            }
        }
    }
}

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

    /// split clip `id` at timeline time `t`: the original covers the first
    /// part, a new clip (returned id) covers the rest. Keyframes are split
    /// at the cut and re-zeroed for the second half; the seam keeps the
    /// outer fades (A keeps fade_in, B keeps fade_out).
    pub fn split_clip(&mut self, id: u64, t: f64) -> Result<u64> {
        let (ti, ci) = self
            .find_clip(id)
            .with_context(|| format!("clip {id} not found"))?;
        let clip = self.tracks[ti].clips[ci].clone();
        let local = t - clip.offset;
        if !local.is_finite() || local <= 0.0 || local >= clip.duration() {
            anyhow::bail!(
                "split point {t} outside clip span {:.2}..{:.2}",
                clip.offset,
                clip.end()
            );
        }
        // `clip` is already a clone, so split the keyframes in pure data
        // first — indexing self.tracks mutably for four fields at once
        // doesn't satisfy the borrow checker.
        let split = |kfs: &[[f64; 2]]| -> (Vec<[f64; 2]>, Vec<[f64; 2]>) {
            let (a_part, b_part): (Vec<[f64; 2]>, Vec<[f64; 2]>) =
                kfs.iter().partition(|kf| kf[0] <= local);
            (
                a_part,
                b_part
                    .iter()
                    .map(|kf| [(kf[0] - local).max(0.0), kf[1]])
                    .collect(),
            )
        };
        let (op_a, op_b) = split(&clip.opacity);
        let (sc_a, sc_b) = split(&clip.scale);
        let (x_a, x_b) = split(&clip.x);
        let (y_a, y_b) = split(&clip.y);
        let (vo_a, vo_b) = split(&clip.volume);
        let mut b = clip;
        b.in_point += local;
        b.offset = t;
        b.opacity = op_b;
        b.scale = sc_b;
        b.x = x_b;
        b.y = y_b;
        b.volume = vo_b;
        {
            let a = &mut self.tracks[ti].clips[ci];
            a.out_point = a.in_point + local;
            a.fade_out = 0.0;
            a.trans_out = None; // the seam is a cut, not the clip's tail
            a.opacity = op_a;
            a.scale = sc_a;
            a.x = x_a;
            a.y = y_a;
            a.volume = vo_a;
            a.sanitize();
        }
        b.fade_in = 0.0;
        b.trans_in = None; // the cut edge keeps no in-transition
        b.sanitize();
        b.id = self.next_id;
        self.next_id += 1;
        let new_id = b.id;
        self.tracks[ti].clips.insert(ci + 1, b);
        Ok(new_id)
    }

    /// clone a clip onto the same track, parked right after the original
    pub fn duplicate_clip(&mut self, id: u64) -> Result<u64> {
        let (ti, ci) = self
            .find_clip(id)
            .with_context(|| format!("clip {id} not found"))?;
        let mut c = self.tracks[ti].clips[ci].clone();
        c.id = self.next_id;
        self.next_id += 1;
        c.offset += c.duration();
        c.sanitize();
        let new_id = c.id;
        self.tracks[ti].clips.insert(ci + 1, c);
        Ok(new_id)
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

    /// duck the clips on `track` under the cue windows of `cue_track` (or
    /// the first subtitle track): every non-text clip gains volume
    /// keyframes ramping its gain down to `amount` `attack` seconds before
    /// each cue and back `release` seconds after it. Merges overlapping
    /// cue spans; combines with existing volume keyframes by multiplying
    /// the two curves at every knot. Returns the number of clips changed.
    pub fn duck(
        &mut self,
        track: usize,
        cue_track: Option<usize>,
        amount: f64,
        attack: f64,
        release: f64,
    ) -> Result<usize> {
        let amount = amount.clamp(0.0, 1.0);
        let attack = attack.max(0.0);
        let release = release.max(0.0);
        let ci = match cue_track {
            Some(i) => {
                let t = self
                    .tracks
                    .get(i)
                    .with_context(|| format!("cue track index {i} out of range"))?;
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
        // merged cue windows (sorted, overlapping/touching spans unioned)
        let mut spans: Vec<(f64, f64)> = self.tracks[ci]
            .cues
            .iter()
            .filter(|c| c.dur > 0.0)
            .map(|c| (c.t, c.end()))
            .collect();
        spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut merged: Vec<(f64, f64)> = Vec::new();
        for (s, e) in spans {
            if let Some(last) = merged.last_mut() {
                if s <= last.1 {
                    last.1 = last.1.max(e);
                    continue;
                }
            }
            merged.push((s, e));
        }
        if merged.is_empty() {
            anyhow::bail!("subtitle track {ci} has no cues to duck under");
        }
        let tr = self
            .tracks
            .get_mut(track)
            .with_context(|| format!("track index {track} out of range"))?;
        if tr.kind == TrackKind::Subtitle {
            anyhow::bail!("track {track} is a subtitle track — duck a video or audio track");
        }
        let mut n = 0usize;
        for clip in &mut tr.clips {
            if clip.is_text() || clip.duration() <= 0.0 {
                continue;
            }
            // cue spans in clip-local seconds, intersected with the clip
            let dip: Vec<(f64, f64)> = merged
                .iter()
                .map(|(s, e)| (s - clip.offset, e - clip.offset))
                .filter(|(s, e)| *e > 0.0 && *s < clip.duration())
                .collect();
            if dip.is_empty() {
                continue;
            }
            clip.volume =
                duck_keyframes(&clip.volume, &dip, clip.duration(), amount, attack, release);
            clip.sanitize();
            n += 1;
        }
        Ok(n)
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
    /// volume gain keyframes [[t, v]] clip-local seconds, v in 0..=2
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume: Vec<[f64; 2]>,
    /// in-transition (slide/wipe/dip); replaces the plain fade-in when set
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trans_in: Option<Transition>,
    /// out-transition; replaces the plain fade-out when set
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trans_out: Option<Transition>,
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
            volume: Vec::new(),
            trans_in: None,
            trans_out: None,
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

    /// effective fade-in length: a `trans_in` replaces the plain fade —
    /// dip fades over `trans_in.dur`, slide/wipe don't fade at all.
    pub fn eff_fade_in(&self) -> f64 {
        match &self.trans_in {
            Some(t) if t.kind == TransKind::Dip => t.dur,
            Some(_) => 0.0,
            None => self.fade_in,
        }
    }

    /// effective fade-out length (see `eff_fade_in`)
    pub fn eff_fade_out(&self) -> f64 {
        match &self.trans_out {
            Some(t) if t.kind == TransKind::Dip => t.dur,
            Some(_) => 0.0,
            None => self.fade_out,
        }
    }

    /// fade-in/out gain at clip-local time, 0..=1 (effective fades)
    pub fn fade_gain(&self, local: f64) -> f64 {
        let dur = self.duration();
        let fi = self.eff_fade_in();
        let fo = self.eff_fade_out();
        let mut g = 1.0_f64;
        if fi > 0.0 && local < fi {
            g *= (local / fi).clamp(0.0, 1.0);
        }
        if fo > 0.0 && local > dur - fo {
            g *= ((dur - local) / fo).clamp(0.0, 1.0);
        }
        g
    }

    /// volume gain at clip-local time, 0..=2 (audio-only; visual no-op)
    pub fn volume_at(&self, local: f64) -> f64 {
        eval_kf(&self.volume, local, 1.0).clamp(0.0, 2.0)
    }

    /// horizontal transition offset at clip-local time, in pixels of the
    /// clip's own width `w`: slide/wipe-in animates -w -> 0, slide/wipe-out
    /// 0 -> +w. Dip has no positional component.
    pub fn trans_dx(&self, local: f64, w: f64) -> f64 {
        let cd = self.duration();
        let mut dx = 0.0;
        if let Some(tr) = &self.trans_in {
            if matches!(tr.kind, TransKind::Slide | TransKind::Wipe)
                && tr.dur > 0.0
                && local < tr.dur
            {
                dx += -w * (1.0 - (local / tr.dur).clamp(0.0, 1.0));
            }
        }
        if let Some(tr) = &self.trans_out {
            if matches!(tr.kind, TransKind::Slide | TransKind::Wipe) && tr.dur > 0.0 {
                let start = cd - tr.dur;
                if local > start {
                    dx += w * ((local - start) / tr.dur).clamp(0.0, 1.0);
                }
            }
        }
        dx
    }

    /// the dip colour underlay active at clip-local time, if any: dip-in
    /// covers [0, dur), dip-out covers [end-dur, end). Renderers draw this
    /// colour under the clip while its alpha ramps through the window.
    pub fn dip_underlay(&self, local: f64) -> Option<[u8; 4]> {
        if let Some(tr) = &self.trans_in {
            if tr.kind == TransKind::Dip && tr.dur > 0.0 && local < tr.dur {
                return Some(tr.dip_rgba8());
            }
        }
        if let Some(tr) = &self.trans_out {
            if tr.kind == TransKind::Dip && tr.dur > 0.0 && local > self.duration() - tr.dur {
                return Some(tr.dip_rgba8());
            }
        }
        None
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
        let cd = self.duration();
        if let Some(t) = &mut self.trans_in {
            t.sanitize(cd);
        }
        if let Some(t) = &mut self.trans_out {
            t.sanitize(cd);
        }
        for kfs in [
            &mut self.opacity,
            &mut self.scale,
            &mut self.x,
            &mut self.y,
            &mut self.volume,
        ] {
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

    pub(crate) fn sanitize(&mut self) {
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

/// ducking curve: volume keyframes dipping to `amount` across each
/// clip-local span `(a, b)` — down-ramp `attack` before a, up-ramp
/// `release` after b. Times outside [0, dur] are kept (a ramp starting
/// before the clip just resumes mid-way). Merges with existing keyframes
/// by evaluating both curves at the union of their knots — the dip is
/// multiplicative so authored fades still apply.
pub fn duck_keyframes(
    existing: &[[f64; 2]],
    spans: &[(f64, f64)],
    clip_dur: f64,
    amount: f64,
    attack: f64,
    release: f64,
) -> Vec<[f64; 2]> {
    // the dip shape alone: 1 → amount over `attack`, hold, → 1 over `release`
    let mut shape: Vec<[f64; 2]> = Vec::new();
    for &(a, b) in spans {
        shape.push([a - attack, 1.0]);
        shape.push([a, amount]);
        shape.push([b, amount]);
        shape.push([b + release, 1.0]);
    }
    shape.sort_by(|x, y| x[0].partial_cmp(&y[0]).unwrap_or(std::cmp::Ordering::Equal));
    shape.retain(|k| k[0].is_finite());
    // prune to the clip window plus one ramp of slack each side — knots
    // outside are unreachable and would only noise up the kf list
    let lo = -attack - 1.0;
    let hi = clip_dur + release + 1.0;
    shape.retain(|k| (lo..=hi).contains(&k[0]));
    if existing.is_empty() {
        return shape;
    }
    // combined curve = existing * shape. Shape knots are kept verbatim
    // (including duplicate-t step pairs — dropping one side would turn a
    // hard cut into a slow ramp); existing knots that don't collide with
    // a shape knot are sampled against the shape curve.
    let mut out: Vec<[f64; 2]> = Vec::new();
    for &[t, v] in &shape {
        if !(lo..=hi).contains(&t) {
            continue;
        }
        out.push([t, (v * eval_kf(existing, t, 1.0)).clamp(0.0, 2.0)]);
    }
    for &[t, v] in existing {
        if !(lo..=hi).contains(&t) || !t.is_finite() {
            continue;
        }
        if shape.iter().any(|k| (k[0] - t).abs() < 1e-9) {
            continue; // already sampled by the shape side
        }
        out.push([t, (v * eval_kf(&shape, t, 1.0)).clamp(0.0, 2.0)]);
    }
    out.sort_by(|x, y| x[0].partial_cmp(&y[0]).unwrap_or(std::cmp::Ordering::Equal));
    out
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

    fn clip(dur: f64) -> Clip {
        Clip::media("a.mp4", 0.0, dur, 0.0)
    }

    #[test]
    fn transition_sanitize() {
        let mut c = clip(4.0);
        c.trans_in = Some(Transition {
            kind: TransKind::Slide,
            dur: 99.0,
            color: None,
        });
        c.trans_out = Some(Transition {
            kind: TransKind::Dip,
            dur: -2.0,
            color: Some([2.0, f32::NAN, 0.5, 1.0]),
        });
        c.sanitize();
        assert_eq!(c.trans_in.as_ref().map(|t| t.dur), Some(4.0)); // clamped to clip dur
        assert_eq!(c.trans_out.as_ref().map(|t| t.dur), Some(0.0));
        let col = c.trans_out.as_ref().unwrap().color.unwrap();
        assert_eq!(col[0], 1.0); // >1 clamped
        assert!(col[1].is_finite() && col[1] == 0.0); // NaN -> 0
                                                      // serde: camelCase names
        c.trans_in = None;
        c.trans_out = Some(Transition {
            kind: TransKind::Dip,
            dur: 0.5,
            color: Some([1.0, 1.0, 1.0, 1.0]),
        });
        let j = serde_json::to_value(&c).unwrap();
        assert_eq!(j["transOut"]["type"], serde_json::json!("dip"));
        let parsed: Clip = serde_json::from_str(
            r#"{"id":1,"src":"a","inPoint":0,"outPoint":4,"offset":0,
               "transIn":{"type":"wipe","dur":0.5}}"#,
        )
        .unwrap();
        assert_eq!(parsed.trans_in.map(|t| t.kind), Some(TransKind::Wipe));
    }

    #[test]
    fn volume_at_eval() {
        let mut c = clip(4.0);
        assert_eq!(c.volume_at(2.0), 1.0); // empty = unity
        c.volume = vec![[1.0, 1.0], [2.0, 0.0], [3.0, 2.0]];
        assert_eq!(c.volume_at(0.5), 1.0); // before first knot holds first val
        assert_eq!(c.volume_at(1.5), 0.5);
        assert_eq!(c.volume_at(2.5), 1.0);
        assert_eq!(c.volume_at(9.0), 2.0); // after last knot, clamped 0..=2
    }

    #[test]
    fn transitions_drive_dx_and_underlay() {
        let mut c = clip(4.0);
        c.offset = 10.0;
        c.trans_in = Some(Transition {
            kind: TransKind::Slide,
            dur: 1.0,
            color: None,
        });
        // halfway through slide-in at 200px wide → -100px offset
        assert_eq!(c.trans_dx(0.5, 200.0), -100.0);
        assert_eq!(c.trans_dx(1.0, 200.0), 0.0);
        assert_eq!(c.trans_dx(2.0, 200.0), 0.0); // past window
        assert!(c.dip_underlay(0.5).is_none()); // slide has no underlay
        c.trans_in = Some(Transition {
            kind: TransKind::Dip,
            dur: 0.5,
            color: Some([1.0, 0.0, 0.0, 1.0]),
        });
        assert_eq!(c.dip_underlay(0.25), Some([255, 0, 0, 255]));
        assert_eq!(c.dip_underlay(1.0), None);
        // dip replaces plain fade for the alpha ramp
        c.fade_in = 0.9;
        assert_eq!(c.eff_fade_in(), 0.5);
        c.trans_in = None;
        assert_eq!(c.eff_fade_in(), 0.9);
    }

    #[test]
    fn duck_keyframes_shape() {
        // no existing volume → plain dip shape with ramps
        let k = duck_keyframes(&[], &[(2.0, 4.0)], 10.0, 0.25, 0.5, 0.5);
        assert_eq!(k, vec![[1.5, 1.0], [2.0, 0.25], [4.0, 0.25], [4.5, 1.0]]);
        // zero attack/release → duplicate knots encode an instant step:
        // eval gives the pre-value just before the boundary, the dip value
        // at and after it
        let k2 = duck_keyframes(&[], &[(2.0, 4.0), (7.0, 8.0)], 10.0, 0.5, 0.0, 0.0);
        assert_eq!(
            k2,
            vec![
                [2.0, 1.0],
                [2.0, 0.5],
                [4.0, 0.5],
                [4.0, 1.0],
                [7.0, 1.0],
                [7.0, 0.5],
                [8.0, 0.5],
                [8.0, 1.0]
            ]
        );
        assert_eq!(eval_kf(&k2, 3.0, 1.0), 0.5); // dipped mid-span
        assert_eq!(eval_kf(&k2, 6.0, 1.0), 1.0); // recovered between spans
                                                 // existing keyframes multiply the dip curve: base 0.5 × dip 0.25
                                                 // → 0.125 held inside the span (duplicate-t knots keep it a hard
                                                 // step, not a slow ramp)
        let k3 = duck_keyframes(
            &[[0.0, 0.5], [10.0, 0.5]],
            &[(2.0, 4.0)],
            10.0,
            0.25,
            0.0,
            0.0,
        );
        assert_eq!(eval_kf(&k3, 3.0, 1.0), 0.125, "{k3:?}");
        assert_eq!(eval_kf(&k3, 1.0, 1.0), 0.5, "{k3:?}");
        assert_eq!(eval_kf(&k3, 5.0, 1.0), 0.5, "{k3:?}");
        assert!(k3.iter().all(|k| (0.0..=2.0).contains(&k[1])));
        // clip boundary clamps
        let k4 = duck_keyframes(&[], &[(0.0, 20.0)], 5.0, 0.5, 1.0, 1.0);
        assert!(k4.iter().all(|k| k[0] >= -1.0 - 1e-9 && k[0] <= 6.0 + 1e-9));
    }

    #[test]
    fn duck_end_to_end_on_timeline() {
        let mut tl = Timeline::new("t", 640, 480, 30.0);
        let at = tl.add_track(TrackKind::Audio);
        let st = tl.add_track(TrackKind::Subtitle);
        tl.add_clip(at, Clip::media("a.m4a", 0.0, 10.0, 0.0))
            .unwrap();
        tl.add_cue(
            Some(st),
            Cue {
                t: 2.0,
                dur: 3.0,
                text: "one".into(),
            },
        )
        .unwrap();
        tl.add_cue(
            Some(st),
            Cue {
                t: 6.0,
                dur: 1.0,
                text: "two".into(),
            },
        )
        .unwrap();
        let n = tl.duck(at, None, 0.25, 0.5, 0.5).unwrap();
        assert_eq!(n, 1);
        let c = &tl.tracks[at].clips[0];
        assert_eq!(c.volume_at(1.0), 1.0);
        assert_eq!(c.volume_at(3.0), 0.25);
        assert_eq!(c.volume_at(6.5), 0.25);
        assert_eq!(c.volume_at(9.5), 1.0);
        // ducking the subtitle track itself errors
        assert!(tl.duck(st, None, 0.25, 0.5, 0.5).is_err());
    }
}
