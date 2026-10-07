//! koubou-motion: video timeline engine (`.kmotion`) driven entirely by a
//! JSON command protocol — tracks, clips, trims, transitions, keyframes,
//! subtitle cues — with ffmpeg-backed probe/render.
//!
//! The session dispatcher (`TlSession`) mirrors `composer::commands::Session`:
//! every operation is a `tl.*` command id on a JSON value. ffmpeg and ffprobe
//! are invoked as subprocesses; their absence produces clean errors, not
//! panics.

pub mod ffmpeg;
pub mod model;
pub mod render;
pub mod session;
pub mod text;

pub use model::{eval_kf, kf_expr, Clip, Cue, Timeline, Track, TrackKind};
pub use session::TlSession;
