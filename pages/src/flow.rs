//! Threaded text flow: when a text frame's content overflows its height,
//! the unlaid remainder continues into the frame its `next` points to —
//! InDesign-style linked text frames.
//!
//! Model rules:
//! - a chain starts at a text frame no other frame links to (a "head");
//!   its own `text` is the story.
//! - each frame lays the incoming text out with its own font/size/align/
//!   leading, wrapped at its own width, shows the lines that fit its
//!   height, and hands the rest to `next`.
//! - chains end on: empty rest, no `next`, a missing/non-text target, or a
//!   cycle (rejected by `pg.linkFrames`, but a hand-edited .kpages can
//!   still contain one — the visited set stops it).
//! - a text frame no chain reaches shows its own `text` — so a freshly
//!   linked target never renders blank by surprise.
//! - remainder left over on the last frame of a chain is *overset* text:
//!   hidden from render, reported via `pg.json`'s `textFlow` map.

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::model::{Frame, FrameKind, PagesDoc};
use crate::text::{split_flow, Layout};

/// resolved display state of one text frame
#[derive(Debug)]
pub struct FrameFlow {
    /// the lines that fit inside the frame box (already truncated)
    pub layout: Layout,
    /// text that did not fit — handed to `next`, or overset at chain end
    pub rest: String,
    /// true when `rest` could not be placed anywhere
    pub overset: bool,
}

/// text-frame fields pulled out for flow resolution
struct TextSpec<'a> {
    text: &'a str,
    font: &'a str,
    size: f32,
    leading: f32,
    align: crate::model::TextAlign,
    next: Option<u64>,
}

fn text_spec(f: &Frame) -> Option<TextSpec<'_>> {
    match &f.kind {
        FrameKind::Text {
            text,
            font,
            size,
            align,
            leading,
            ..
        } => Some(TextSpec {
            text,
            font,
            size: *size,
            leading: *leading,
            align: *align,
            next: f.next,
        }),
        _ => None,
    }
}

/// lay out every text frame in the doc: chains first (heads are text
/// frames nothing links to), then standalone/pass-by frames. Deterministic:
/// heads are visited in document order (pages then masters, frame order).
pub fn resolve_flow(doc: &PagesDoc) -> Result<HashMap<u64, FrameFlow>> {
    let mut out: HashMap<u64, FrameFlow> = HashMap::new();

    // all frames in stable document order
    let all: Vec<&Frame> = doc
        .pages
        .iter()
        .flat_map(|p| p.frames.iter())
        .chain(doc.masters.iter().flat_map(|m| m.frames.iter()))
        .collect();
    let targets: HashSet<u64> = all.iter().filter_map(|f| f.next).collect();

    // walk each chain from its head
    for head in &all {
        if targets.contains(&head.id) {
            continue; // mid-chain: filled by its head's walk
        }
        let Some(spec) = text_spec(head) else {
            continue;
        };
        let mut text = spec.text.to_string();
        let mut cur = *head;
        let mut visited: HashSet<u64> = HashSet::new();
        loop {
            visited.insert(cur.id);
            let Some(ts) = text_spec(cur) else { break };
            let (layout, rest) =
                split_flow(ts.font, &text, ts.size, ts.leading, ts.align, cur.w, cur.h)?;
            let next = ts.next;
            out.insert(
                cur.id,
                FrameFlow {
                    layout,
                    rest: rest.clone(),
                    // provisional: a mid-chain frame's rest goes to `next`
                    overset: !rest.is_empty(),
                },
            );
            if rest.is_empty() {
                break;
            }
            match next {
                Some(n) if !visited.contains(&n) => match doc.frame(n) {
                    Some(f) if text_spec(f).is_some() => {
                        // the rest placed onward is not overset
                        if let Some(ff) = out.get_mut(&cur.id) {
                            ff.overset = false;
                        }
                        cur = f;
                        text = rest;
                    }
                    _ => break, // target missing or not a text frame
                },
                _ => break,
            }
        }
    }

    // frames no chain reached (link targets whose chain ran dry, frames in
    // detached rings) render their own text standalone
    for f in &all {
        if out.contains_key(&f.id) {
            continue;
        }
        let Some(ts) = text_spec(f) else { continue };
        let (layout, rest) = split_flow(ts.font, ts.text, ts.size, ts.leading, ts.align, f.w, f.h)?;
        out.insert(
            f.id,
            FrameFlow {
                layout,
                overset: !rest.is_empty(),
                rest,
            },
        );
    }

    Ok(out)
}
