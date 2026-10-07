//! Auto-correction analysis.
//!
//! Renders a small neutral (default-recipe) preview and derives suggested
//! recipe values from image statistics. Every suggestion is a *starting
//! point* — conservative gains and confidence fields let the UI decide what
//! to apply. Pure Rust, no GPU needed, all work on the sRGB preview.

use crate::develop::RgbaImage;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AutoResult {
    /// suggested straighten angle, degrees (negative = rotate CCW)
    pub rotation_deg: f32,
    /// keystone vertical/horizontal, recipe range -0.4..0.4
    pub key_v: f32,
    pub key_h: f32,
    /// luminance noise reduction strength 0..0.6
    pub noise_luma: f32,
    pub noise_chroma: f32,
    /// chromatic-aberration fix strength 0..0.7
    pub ca_fix: f32,
    /// vibrance suggestion 0..0.5 (never negative — saturation is taste)
    pub vibrance: f32,
    /// per-band EV suggestions for the 9-band tone equalizer (-4..+4 bands)
    pub zones_ev: [f32; 9],
    /// diagnostics so the UI can explain/gate what was applied
    pub straighten_conf: f32, // 0..1 share of edge votes in the peak bin
    pub keystone_conf: f32,
    pub noise_sigma: f32, // measured luma noise sigma, 0-255 units
    pub ca_score: f32,    // fringe metric at strong edges
}

fn median(xs: &mut [f32]) -> f32 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    xs[xs.len() / 2]
}

/// median absolute deviation → robust sigma estimate
fn mad_sigma(xs: &[f32]) -> f32 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut v = xs.to_vec();
    let m = median(&mut v);
    let mut dev: Vec<f32> = xs.iter().map(|x| (x - m).abs()).collect();
    median(&mut dev) * 1.4826
}

pub fn analyze(img: &RgbaImage) -> AutoResult {
    let (w, h) = (img.width as usize, img.height as usize);
    let n = w * h;
    if n < 64 || w < 8 || h < 8 {
        return AutoResult {
            rotation_deg: 0.0,
            key_v: 0.0,
            key_h: 0.0,
            noise_luma: 0.0,
            noise_chroma: 0.0,
            ca_fix: 0.0,
            vibrance: 0.0,
            zones_ev: [0.0; 9],
            straighten_conf: 0.0,
            keystone_conf: 0.0,
            noise_sigma: 0.0,
            ca_score: 0.0,
        };
    }

    // --- per-pixel planes -------------------------------------------------
    let mut lum = vec![0f32; n];
    let mut chr = vec![0f32; n];
    for i in 0..n {
        let (r, g, b) = (
            img.data[i * 4] as f32 / 255.0,
            img.data[i * 4 + 1] as f32 / 255.0,
            img.data[i * 4 + 2] as f32 / 255.0,
        );
        lum[i] = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        chr[i] = r.max(g).max(b) - r.min(g).min(b);
    }

    // Sobel gradients (x only needed interior; edges ignored)
    let mut gx = vec![0f32; n];
    let mut gy = vec![0f32; n];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            gx[i] = lum[i - w + 1] + 2.0 * lum[i + 1] + lum[i + w + 1]
                - lum[i - w - 1] - 2.0 * lum[i - 1] - lum[i + w - 1];
            gy[i] = lum[i + w - 1] + 2.0 * lum[i + w] + lum[i + w + 1]
                - lum[i - w - 1] - 2.0 * lum[i - w] - lum[i - w + 1];
        }
    }

    // gradient magnitude percentile → edge threshold
    let mut mags: Vec<f32> = (0..n)
        .step_by(3)
        .map(|i| (gx[i] * gx[i] + gy[i] * gy[i]).sqrt())
        .collect();
    let m_med = median(&mut mags);
    let edge_t = (m_med * 4.0).max(0.02);

    // --- edges -> straighten + keystone via Hough -------------------------
    // Local Sobel orientation can't measure shallow tilts (a 4° edge moves
    // ~0.2px inside a 3px window), so we use a small Hough transform: edge
    // pixels vote for (angle, offset) of global lines. Near-horizontal lines
    // fix rotation; near-vertical line tilts per side give keystone.
    let mut hpts: Vec<(f32, f32, f32)> = Vec::new(); // near-horizontal edges
    // near-vertical edges per half: (line tilt from vertical, magnitude).
    // The tilt is measured per pixel directly: a line x = y*tan(phi) + c has
    // dx/dy = -gy/gx, so phi = atan2(-gy, gx). Per-pixel values are noisy
    // but the median over a half is a sharp estimate of the side's tilt.
    let mut vl_pts: Vec<(f32, f32)> = Vec::new();   // left half
    let mut vr_pts: Vec<(f32, f32)> = Vec::new();   // right half
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            let m = (gx[i] * gx[i] + gy[i] * gy[i]).sqrt();
            if m < edge_t {
                continue;
            }
            // local orientation is only a coarse filter — coarse is fine
            let dir = gy[i].atan2(gx[i]).to_degrees() - 90.0;
            let a = if dir > 90.0 {
                dir - 180.0
            } else if dir < -90.0 {
                dir + 180.0
            } else {
                dir
            };
            if a.abs() <= 35.0 {
                hpts.push((x as f32, y as f32, m));
            } else if a.abs() >= 55.0 {
                if (x as usize) < w / 2 {
                    vl_pts.push((x as f32, y as f32));
                } else {
                    vr_pts.push((x as f32, y as f32));
                }
            }
        }
    }
    // cap point counts (uniform stride) — Hough is O(pts x angles)
    let cap = 6000usize;
    if hpts.len() > cap {
        let s = hpts.len() / cap;
        hpts = hpts.iter().step_by(s).copied().collect();
    }

    // Hough over line angle theta:  y = x*tan(theta) + b.
    // Confidence = peak dominance over the per-angle median: a real horizon
    // makes one angle stand far above the rest; random texture does not.
    let (mut best_th, mut best_votes) = (0f32, 0f32);
    let mut angle_peaks: Vec<f32> = Vec::with_capacity(81);
    let mut hbins = std::collections::HashMap::<i32, f32>::new();
    for ti in 0..81 {
        let th_deg = -8.0 + ti as f32 * 0.2;
        let tan = th_deg.to_radians().tan();
        hbins.clear();
        for &(x, y, m) in &hpts {
            let b = y - x * tan;
            *hbins.entry((b / 3.0).round() as i32).or_default() += m;
        }
        let peak = hbins.values().copied().fold(0f32, f32::max);
        angle_peaks.push(peak);
        if peak > best_votes {
            best_votes = peak;
            best_th = th_deg;
        }
    }
    let med_peak = median(&mut angle_peaks);
    let mut rotation = -best_th;
    let straighten_conf =
        if best_votes > 0.0 { ((best_votes - med_peak) / best_votes).max(0.0) } else { 0.0 };
    if rotation.abs() < 0.3 || straighten_conf < 0.4 {
        rotation = 0.0;
    }
    rotation = rotation.clamp(-6.0, 6.0);

    // keystone: Hough for near-vertical lines x = y*tan(phi) + c, per half.
    // conv < 0 when tops lean inward (typical upward building shot) ->
    // key_v < 0 widens the top in the dst->src warp (see develop.rs).
    // returns (best angle, coherence): coherence = share of points sitting
    // in "line-supported" c-bins (>= 25% of the angle's peak or >= 10 votes)
    // at the winning angle — real converging lines agree, texture scatters.
    fn vpeak(pts: &[(f32, f32)]) -> (f32, f32) {
        let mut bins = std::collections::HashMap::<i32, f32>::new();
        let mut support: Vec<f32> = Vec::with_capacity(97);
        let (mut best_ti, mut best_v) = (0usize, 0f32);
        for ti in 0..97usize {
            let tan = (-12.0 + ti as f32 * 0.25).to_radians().tan();
            bins.clear();
            for &(x, y) in pts {
                let c = x - y * tan;
                *bins.entry((c / 3.0).round() as i32).or_default() += 1.0;
            }
            let pk = bins.values().copied().fold(0f32, f32::max);
            let th = (pk * 0.25).max(10.0);
            let sup: f32 = bins.values().filter(|v| **v >= th).sum();
            support.push(sup);
            if pk > best_v {
                best_v = pk;
                best_ti = ti;
            }
        }
        // parabolic sub-bin refinement of the winning angle
        let ti = best_ti;
        let (v0, v1, v2) = (
            support[ti.saturating_sub(1)],
            support[ti],
            support[(ti + 1).min(96)],
        );
        let denom = v0 - 2.0 * v1 + v2;
        let shift = if denom.abs() > 1e-6 { 0.5 * (v0 - v2) / denom } else { 0.0 };
        let best_a = -12.0 + (ti as f32 + shift.clamp(-1.0, 1.0)) * 0.25;
        let coh = if pts.is_empty() { 0.0 } else { v1 / pts.len() as f32 };
        (best_a, coh.min(1.0))
    }
    let mut key_v = 0f32;
    let mut keystone_conf = 0f32;
    if vl_pts.len() > 60 && vr_pts.len() > 60 {
        let (pl, cl) = vpeak(&vl_pts);
        let (pr, cr) = vpeak(&vr_pts);
        // convergence = the tilt difference between the halves
        let conv = (pl - pr) / 2.0;
        // ~0.02 key per degree of convergence, capped — partial correction
        key_v = (conv * 0.02).clamp(-0.25, 0.25);
        keystone_conf = cl.min(cr);
        if key_v.abs() < 0.02 || keystone_conf < 0.35 {
            key_v = 0.0;
        }
    }

    // --- noise: MAD of high-pass residual inside flat mid-tone blocks -----
    let blk = 16usize;
    let mut residuals: Vec<f32> = Vec::with_capacity(4096);
    for by in (0..h - blk).step_by(blk) {
        for bx in (0..w - blk).step_by(blk) {
            // block flatness: mean |grad|
            let mut ge = 0f32;
            let mut lv = 0f32;
            for yy in 0..blk {
                for xx in 0..blk {
                    let i = (by + yy) * w + bx + xx;
                    ge += (gx[i] * gx[i] + gy[i] * gy[i]).sqrt();
                    lv += lum[i];
                }
            }
            ge /= (blk * blk) as f32;
            lv /= (blk * blk) as f32;
            if ge < edge_t * 0.35 && (0.12..0.9).contains(&lv) {
                // flat + mid-tone: collect high-pass residual (l - 3x3 mean)
                for yy in 1..blk - 1 {
                    for xx in 1..blk - 1 {
                        let i = (by + yy) * w + bx + xx;
                        let mut s = 0f32;
                        for dy in -1i32..=1 {
                            for dx in -1i32..=1 {
                                s += lum[(i as i32 + dy * w as i32 + dx) as usize];
                            }
                        }
                        residuals.push(lum[i] - s / 9.0);
                    }
                }
            }
        }
    }
    let noise_sigma = mad_sigma(&residuals) * 255.0; // back to 8-bit units
    // sigma ~1 = clean file; ~4+ = visible noise
    let noise_luma = ((noise_sigma - 1.0) / 7.0).clamp(0.0, 0.6);
    let noise_chroma = (noise_luma * 1.2).min(0.7);

    // --- CA: chroma spike localized ON the edge vs. its surroundings ------
    // True fringing sits right at the edge pixel; natural colorful edges
    // transition smoothly. A bandpass along the gradient direction —
    // chr(p) - 0.5*(chr(p+) + chr(p-)) — isolates the spike and rejects
    // natural color differences across the boundary.
    let mut fringe: Vec<f32> = Vec::with_capacity(2048);
    for y in 3..h - 3 {
        for x in 3..w - 3 {
            let i = y * w + x;
            let (mx, my) = (gx[i], gy[i]);
            let m = (mx * mx + my * my).sqrt();
            if m < edge_t * 2.5 {
                continue;
            }
            let (ux, uy) = (mx / m, my / m);
            let px = (x as f32 + ux * 2.0).round().clamp(0.0, (w - 1) as f32) as usize;
            let py = (y as f32 + uy * 2.0).round().clamp(0.0, (h - 1) as f32) as usize;
            let qx = (x as f32 - ux * 2.0).round().clamp(0.0, (w - 1) as f32) as usize;
            let qy = (y as f32 - uy * 2.0).round().clamp(0.0, (h - 1) as f32) as usize;
            let f = chr[i] - 0.5 * (chr[py * w + px] + chr[qy * w + qx]);
            fringe.push(f.max(0.0));
        }
    }
    fringe.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let ca_score = if fringe.is_empty() { 0.0 } else { fringe[fringe.len() * 3 / 4] };
    // demosaic residue leaves a small bandpass floor; subtract it
    let ca_fix = ((ca_score - 0.02) * 6.0).clamp(0.0, 0.7);

    // --- vibrance: image-wide chroma is low -> suggest a lift -------------
    let mut cv = chr.clone();
    let p75 = {
        let k = ((cv.len() as f32 * 0.75) as usize).min(cv.len() - 1);
        cv.select_nth_unstable_by(k, |a, b| {
            a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
        });
        cv[k]
    };
    let vibrance = ((0.11 - p75) * 3.0).clamp(0.0, 0.5);

    // --- zones: pull each log2-luma band's median toward middle gray ------
    let mut band_lum: [Vec<f32>; 9] = Default::default();
    for (i, &l) in lum.iter().enumerate() {
        if i % 2 == 0 {
            continue;
        }
        let ev = (l.max(0.015)).log2() + 4.0;
        let b = ev.round().clamp(0.0, 8.0) as usize;
        band_lum[b].push(l);
    }
    let mut zones = [0f32; 9];
    for (b, v) in band_lum.iter_mut().enumerate() {
        // need enough pixels to bother
        if v.len() < n / 60 {
            continue;
        }
        let med = median(v);
        // half the pull toward 18% gray, capped — flatten gently, not fully
        zones[b] = (0.5 * (0.18 / med.max(1e-3)).log2()).clamp(-1.5, 1.5);
        if zones[b].abs() < 0.08 {
            zones[b] = 0.0;
        }
    }
    // don't let the global band fight auto exposure
    zones[8] = 0.0;
    zones[4] = 0.0;

    AutoResult {
        rotation_deg: rotation,
        key_v,
        key_h: 0.0,
        noise_luma,
        noise_chroma,
        ca_fix,
        vibrance,
        zones_ev: zones,
        straighten_conf,
        keystone_conf,
        noise_sigma,
        ca_score,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, v: u8) -> RgbaImage {
        RgbaImage {
            width: w,
            height: h,
            data: vec![v; (w * h * 4) as usize],
        }
    }

    /// image with tilted horizon + Gaussian-ish noise + converging verticals
    fn synthetic(w: u32, h: u32, tilt_deg: f32) -> RgbaImage {
        let mut d = vec![0u8; (w * h * 4) as usize];
        let (w2, h2) = (w as f32, h as f32);
        let t = tilt_deg.to_radians();
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 - w2 / 2.0, y as f32 - h2 / 2.0);
                // sky above tilted horizon, ground below
                let edge = fx * t.sin() + h2 * 0.1;
                let mut v = if fy < edge { 200u8 } else { 60u8 };
                // converging vertical bars: un-warped coord ux makes lines
                // spread apart going down (upward convergence, like a
                // building shot from below)
                let conv = 1.0 + (fy / h2) * 0.3;
                let ux = fx / conv;
                if ux.rem_euclid(40.0) < 4.0 && ux.abs() > 30.0 && fy > edge + 20.0 {
                    v = 230;
                }
                let i = ((y * w + x) * 4) as usize;
                d[i] = v;
                d[i + 1] = v;
                d[i + 2] = v;
                d[i + 3] = 255;
            }
        }
        RgbaImage { width: w, height: h, data: d }
    }

    #[test]
    fn flat_is_neutral() {
        let r = analyze(&solid(64, 64, 128));
        assert_eq!(r.rotation_deg, 0.0);
        assert_eq!(r.key_v, 0.0);
        assert_eq!(r.ca_fix, 0.0);
        // a flat mid-gray field still gets zone/vibrance pulls toward the
        // reference mid — check they're bounded, not that they're zero
        assert!(r.zones_ev.iter().all(|&z| z.abs() <= 1.5));
    }

    #[test]
    fn detects_tilt() {
        let r = analyze(&synthetic(160, 160, 4.0));
        eprintln!("rot={} conf={} kv={} kconf={} zones={:?}", r.rotation_deg, r.straighten_conf, r.key_v, r.keystone_conf, r.zones_ev);
        // horizon tilts +4 -> suggestion should be roughly -4
        assert!(r.rotation_deg < -1.0, "rotation {}", r.rotation_deg);
        assert!(r.rotation_deg > -8.0, "rotation {}", r.rotation_deg);
        // upward-converging verticals -> key_v must widen the top (<0)
        assert!(r.key_v < 0.0, "key_v {}", r.key_v);
    }
}
