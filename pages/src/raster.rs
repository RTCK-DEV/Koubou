//! PNG page preview: rasterize a page's resolved frames into an RGBA8 image.
//!
//! Each frame paints into a local stamp (its bbox at the render scale), then
//! the stamp is blit into the page with bilinear sampling under the frame's
//! rotation — one code path covers every kind. Page background is paper-white.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use image::{Rgba, RgbaImage};

use crate::flow::FrameFlow;
use crate::model::{Frame, FrameKind, PagesDoc, Stroke};
use crate::text::Layout;

/// render `page_idx` at `dpi` (pt → px = dpi/72); returns an RGBA image
pub fn render_page(doc: &PagesDoc, page_idx: usize, dpi: f32) -> Result<RgbaImage> {
    if !(dpi > 0.0 && dpi.is_finite()) {
        anyhow::bail!("bad dpi {dpi}");
    }
    let s = dpi / 72.0;
    let w = (doc.page_w * s).round().max(1.0) as u32;
    let h = (doc.page_h * s).round().max(1.0) as u32;
    let mut img = RgbaImage::from_pixel(w, h, Rgba([255, 255, 255, 255]));

    let flows = crate::flow::resolve_flow(doc)?;
    for f in &doc.resolved_frames(page_idx)? {
        draw_frame(&mut img, f, s, &flows)?;
    }
    Ok(img)
}

fn draw_frame(
    img: &mut RgbaImage,
    f: &Frame,
    s: f32,
    flows: &HashMap<u64, FrameFlow>,
) -> Result<()> {
    let sw = (f.w * s).round().max(1.0) as u32;
    let sh = (f.h * s).round().max(1.0) as u32;
    let mut stamp = RgbaImage::from_pixel(sw, sh, Rgba([0, 0, 0, 0]));

    match &f.kind {
        FrameKind::Rect { fill, stroke } => {
            if let Some(c) = fill {
                fill_rect(&mut stamp, 0, 0, sw, sh, *c);
            }
            if let Some(st) = stroke {
                stroke_rect(&mut stamp, st, s);
            }
        }
        FrameKind::Line { x2, y2, stroke } => {
            draw_line(&mut stamp, 0.0, 0.0, x2 * s, y2 * s, stroke, s);
        }
        FrameKind::Image { path, fit } => {
            paint_image(&mut stamp, path, fit, f.w, f.h, s)?;
        }
        FrameKind::Text { color, .. } => {
            // resolved through the flow map: linked frames show their
            // chain's continuation, and overset lines are already dropped
            if let Some(fl) = flows.get(&f.id) {
                paint_layout(&mut stamp, &fl.layout, *color, s);
            }
        }
    }

    blit_rotated(img, &stamp, f, s);
    Ok(())
}

/// source-over blend of `c` ([r,g,b,a] 0..1) at pixel (x,y)
fn blend_px(img: &mut RgbaImage, x: u32, y: u32, c: [f32; 4]) {
    let Some(px) = img.get_pixel_mut_checked(x, y) else {
        return;
    };
    let sa = c[3].clamp(0.0, 1.0);
    if sa <= 0.0 {
        return;
    }
    let da = px[3] as f32 / 255.0;
    let ao = sa + da * (1.0 - sa);
    if ao <= 0.0 {
        return;
    }
    for i in 0..3 {
        let sc = c[i].clamp(0.0, 1.0) * sa;
        let dc = px[i] as f32 / 255.0 * da;
        px[i] = ((sc + dc * (1.0 - sa)) / ao * 255.0).round() as u8;
    }
    px[3] = (ao * 255.0).round() as u8;
}

fn fill_rect(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, c: [f32; 4]) {
    let (iw, ih) = img.dimensions();
    let x2 = x.saturating_add(w).min(iw);
    let y2 = y.saturating_add(h).min(ih);
    for yy in y.min(ih)..y2 {
        for xx in x.min(iw)..x2 {
            blend_px(img, xx, yy, c);
        }
    }
}

/// stroke a rect just inside the stamp edge
fn stroke_rect(img: &mut RgbaImage, st: &Stroke, s: f32) {
    let (w, h) = img.dimensions();
    let t = (st.width * s).round().max(1.0) as u32;
    fill_rect(img, 0, 0, w, t, st.color);
    fill_rect(img, 0, h.saturating_sub(t), w, t, st.color);
    fill_rect(img, 0, 0, t, h, st.color);
    fill_rect(img, w.saturating_sub(t), 0, t, h, st.color);
}

/// thick line with per-pixel coverage AA
fn draw_line(img: &mut RgbaImage, x1: f32, y1: f32, x2: f32, y2: f32, st: &Stroke, s: f32) {
    let half = (st.width * s * 0.5).max(0.5);
    let (minx, maxx) = (x1.min(x2) - half - 1.0, x1.max(x2) + half + 1.0);
    let (miny, maxy) = (y1.min(y2) - half - 1.0, y1.max(y2) + half + 1.0);
    let (iw, ih) = (img.width() as i64, img.height() as i64);
    let dx = x2 - x1;
    let dy = y2 - y1;
    let len2 = (dx * dx + dy * dy).max(1e-6);
    for y in (miny.floor() as i64).max(0)..=(maxy.ceil() as i64).min(ih - 1) {
        for x in (minx.floor() as i64).max(0)..=(maxx.ceil() as i64).min(iw - 1) {
            // distance from pixel center to segment
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let t = ((px - x1) * dx + (py - y1) * dy) / len2;
            let t = t.clamp(0.0, 1.0);
            let cx = x1 + t * dx;
            let cy = y1 + t * dy;
            let dist = ((px - cx) * (px - cx) + (py - cy) * (py - cy)).sqrt();
            let cov = (half + 0.5 - dist).clamp(0.0, 1.0);
            if cov > 0.0 {
                let mut c = st.color;
                c[3] *= cov;
                blend_px(img, x as u32, y as u32, c);
            }
        }
    }
}

fn paint_image(
    stamp: &mut RgbaImage,
    path: &Path,
    fit: &crate::model::ImageFit,
    fw: f32,
    fh: f32,
    s: f32,
) -> Result<()> {
    let src = image::open(path)
        .with_context(|| format!("decode image {}", path.display()))?
        .to_rgba8();
    let (iw, ih) = src.dimensions();
    let (dx, dy, dw, dh) = fit.dest_rect(fw, fh, iw, ih);
    let (dw_px, dh_px) = (
        (dw * s).round().max(1.0) as u32,
        (dh * s).round().max(1.0) as u32,
    );
    let resized =
        image::imageops::resize(&src, dw_px, dh_px, image::imageops::FilterType::Triangle);
    let ox = (dx * s).round() as i64;
    let oy = (dy * s).round() as i64;
    // blit, clipped to the stamp (crop handles Fill-mode overflow)
    let (sw, sh) = (stamp.width() as i64, stamp.height() as i64);
    for sy in 0..resized.height() as i64 {
        let dy_ = oy + sy;
        if dy_ < 0 || dy_ >= sh {
            continue;
        }
        for sx in 0..resized.width() as i64 {
            let dx_ = ox + sx;
            if dx_ < 0 || dx_ >= sw {
                continue;
            }
            let p = resized.get_pixel(sx as u32, sy as u32).0;
            let c = [
                p[0] as f32 / 255.0,
                p[1] as f32 / 255.0,
                p[2] as f32 / 255.0,
                p[3] as f32 / 255.0,
            ];
            blend_px(stamp, dx_ as u32, dy_ as u32, c);
        }
    }
    Ok(())
}

/// transparent RGBA stamp of `w`×`h` pt at `s` px/pt with `lay`'s glyphs
/// painted in `color` — used by the PDF writer for CJK/non-Helvetica runs
pub fn text_stamp(lay: &Layout, color: [f32; 4], w_pt: f32, h_pt: f32, s: f32) -> RgbaImage {
    let sw = (w_pt * s).round().max(1.0) as u32;
    let sh = (h_pt * s).round().max(1.0) as u32;
    let mut stamp = RgbaImage::from_pixel(sw, sh, Rgba([0, 0, 0, 0]));
    paint_layout(&mut stamp, lay, color, s);
    stamp
}

/// paint a laid-out text flow into the stamp at `s` px/pt. Per-char font
/// fallback (CJK) happens via `lay.fonts.font_for`.
fn paint_layout(stamp: &mut RgbaImage, lay: &Layout, color: [f32; 4], s: f32) {
    let px_size = (lay.px * s).max(1.0);
    for line in &lay.lines {
        let mut pen = line.x_off * s;
        let baseline = line.baseline * s;
        for ch in line.text.chars() {
            let font = lay.fonts.font_for(ch);
            let (m, bmp) = font.rasterize(ch, px_size);
            let gx = (pen + m.xmin as f32).round() as i64;
            let gy = (baseline - m.ymin as f32 - m.height as f32).round() as i64;
            for row in 0..m.height {
                for col in 0..m.width {
                    let a = bmp[row * m.width + col];
                    if a == 0 {
                        continue;
                    }
                    let x = gx + col as i64;
                    let y = gy + row as i64;
                    if x < 0 || y < 0 || x >= stamp.width() as i64 || y >= stamp.height() as i64 {
                        continue;
                    }
                    let mut c = color;
                    c[3] *= a as f32 / 255.0;
                    blend_px(stamp, x as u32, y as u32, c);
                }
            }
            pen += m.advance_width;
        }
    }
}

/// rotate `stamp` about the frame center and source-over into `img`
fn blit_rotated(img: &mut RgbaImage, stamp: &RgbaImage, f: &Frame, s: f32) {
    let (sw, sh) = (stamp.width() as f32, stamp.height() as f32);
    let (cx, cy) = f.center();
    let (cx, cy) = (cx * s, cy * s);
    let t = f.rotation_deg.to_radians();
    let (cos, sin) = (t.cos(), t.sin());

    // dest aabb = rotated corners of the stamp rect (stamp is centered on cx,cy)
    let (hw, hh) = (sw * 0.5, sh * 0.5);
    let corners = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)];
    let mut minx = f32::MAX;
    let mut miny = f32::MAX;
    let mut maxx = f32::MIN;
    let mut maxy = f32::MIN;
    for (x, y) in corners {
        let rx = x * cos - y * sin;
        let ry = x * sin + y * cos;
        minx = minx.min(rx);
        maxx = maxx.max(rx);
        miny = miny.min(ry);
        maxy = maxy.max(ry);
    }
    let (iw, ih) = (img.width() as i64, img.height() as i64);
    let x0 = ((cx + minx).floor() as i64).max(0);
    let y0 = ((cy + miny).floor() as i64).max(0);
    let x1 = ((cx + maxx).ceil() as i64).min(iw - 1);
    let y1 = ((cy + maxy).ceil() as i64).min(ih - 1);

    for y in y0..=y1 {
        for x in x0..=x1 {
            // inverse-map dest pixel into stamp space
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let ux = dx * cos + dy * sin + hw;
            let uy = -dx * sin + dy * cos + hh;
            if let Some(c) = sample(stamp, ux, uy) {
                if c[3] > 0.0 {
                    blend_px(img, x as u32, y as u32, c);
                }
            }
        }
    }
}

/// bilinear sample of the stamp at (ux,uy); None outside
fn sample(stamp: &RgbaImage, ux: f32, uy: f32) -> Option<[f32; 4]> {
    let (w, h) = (stamp.width() as f32, stamp.height() as f32);
    if ux < -0.5 || uy < -0.5 || ux > w - 0.5 || uy > h - 0.5 {
        return None;
    }
    let x0 = ux.floor().clamp(0.0, w - 1.0) as u32;
    let y0 = uy.floor().clamp(0.0, h - 1.0) as u32;
    let x1 = (x0 + 1).min(stamp.width() - 1);
    let y1 = (y0 + 1).min(stamp.height() - 1);
    let fx = (ux - ux.floor()).clamp(0.0, 1.0);
    let fy = (uy - uy.floor()).clamp(0.0, 1.0);
    let px = |x: u32, y: u32| -> [f32; 4] {
        let p = stamp.get_pixel(x, y).0;
        [
            p[0] as f32 / 255.0,
            p[1] as f32 / 255.0,
            p[2] as f32 / 255.0,
            p[3] as f32 / 255.0,
        ]
    };
    let (a, b, c, d) = (px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1));
    let mut out = [0.0; 4];
    for i in 0..4 {
        let top = a[i] + (b[i] - a[i]) * fx;
        let bot = c[i] + (d[i] - c[i]) * fx;
        out[i] = top + (bot - top) * fy;
    }
    Some(out)
}
