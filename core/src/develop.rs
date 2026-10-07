//! Development pipeline (CPU reference implementation).
//! Stages: normalize -> demosaic -> WB -> camera->sRGB matrix ->
//!         tone/color adjustments -> sRGB gamma -> orient.
//! The same math is mirrored by the wgpu compute pipeline in gpu.rs.
use crate::decode::{Decoded, Mosaic};
use crate::recipe::{Recipe, WbMode};

#[derive(Debug, Clone)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    /// rgba8
    pub data: Vec<u8>,
}

/// CIE XYZ(D65) -> linear sRGB (kept for future ICC/custom-matrix paths)
#[allow(dead_code)]
const XYZ_TO_SRGB: [[f32; 3]; 3] = [
    [3.2404542, -1.5371385, -0.4985314],
    [-0.9692660, 1.8760108, 0.0415560],
    [0.0556434, -0.2040259, 1.0572252],
];

/// Shared per-image derived parameters (identical math in gpu.rs).
#[derive(Debug, Clone)]
pub struct Params {
    pub wb: [f32; 3],
    /// camera -> linear sRGB matrix incl. WB
    pub m: [[f32; 3]; 3],
    pub lut: Vec<f32>,
    pub exposure_mul: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub saturation: f32,
    pub vibrance: f32,
    pub sharpen: f32,
    pub noise_luma: f32,
    pub rotation_deg: f32,
    pub clarity: f32,
    pub vignette: f32,
    pub grain: f32,
    // grading
    pub lift: [f32; 3],
    pub gamma: [f32; 3],
    pub gain: [f32; 3],
    pub shadow_col: [f32; 3],
    pub shadow_sat: f32,
    pub high_col: [f32; 3],
    pub high_sat: f32,
    /// auto-contrast remap points (identity when 0,1)
    pub black_pt: f32,
    pub white_pt: f32,
    // retouch (frame-normalized coords, converted to virtual-px inside)
    pub spots: [[f32; 4]; 8],
    pub n_spots: u32,
    pub lights: [[f32; 4]; 8],
    pub n_lights: u32,
    pub crop: [f32; 4],
    // DaVinci extras
    pub offset: [f32; 3],
    pub mid_col: [f32; 3],
    pub mid_sat: f32,
    pub pivot: f32,
    pub hl_roll: f32,
    pub sh_roll: f32,
    /// per-channel curve LUTs, 768 floats (r|g|b); empty = identity
    pub chan_luts: Vec<f32>,
    /// hue-domain curve LUTs, 1280 floats (hh|hs|hl|ls|ss); empty = identity
    pub hue_luts: Vec<f32>,
    pub qh: [f32; 3],
    pub qs: [f32; 3],
    pub ql: [f32; 3],
    pub qadj: [f32; 4],
    pub q_invert: bool,
    /// matte finesse [clean_black, clean_white]
    pub q_clean: [f32; 2],
    /// matte finesse blur → widens soft edges
    pub q_blur: f32,
    /// highlight/isolate preview of the key
    pub q_show: bool,
    pub has_qual: bool,
    /// power windows: packed [kind(+2=invert), p0,p1,p2,p3, p4(rot), p5(soft), ev, sat, temp, strength]
    pub wins: [[f32; 12]; 4],
    pub n_wins: u32,
    /// HDR zone wheels [hue, amt, ev, sat] for dark/shadow/light/global
    pub zones: [[f32; 4]; 4],
    pub mixer: [f32; 9],
    pub mono: [f32; 3],
    /// clone stamps in virtual-px [sx,sy,dx,dy,r,_] (max 8)
    pub clones: [[f32; 6]; 8],
    pub n_clones: u32,
    pub beauty: f32,
    pub noise_chroma: f32,
    pub ca_fix: f32,
    pub deband: f32,
    pub glow: f32,
    /// lens flare [cx, cy, strength, hue] in dst-normalized coords
    pub flare: [f32; 4],
    /// tone equalizer: EV per log2-luma zone (centers -4..+4 EV, 9 zones)
    pub zone_ev: [f32; 9],
    /// imported .cube 3D LUT (applied display-referred) + amount
    pub lut3d: Option<crate::lut::CubeLut>,
    pub lut_amt: f32,
    /// keystone trapezoid warps -0.4..0.4
    pub key_v: f32,
    pub key_h: f32,
}

/// statistics gathered by the sparse sampling pass (auto WB / exposure / contrast)
#[derive(Debug, Clone)]
pub struct Stats {
    pub means: [f32; 3],
    pub luma_mean: f32,
    pub luma_hist: [u32; 256],
    pub count: u64,
    /// mean raw level inside the WB-pick rectangle (camera space)
    pub pick_means: [f32; 3],
    pub pick_count: u64,
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            means: [0.0; 3],
            luma_mean: 0.0,
            luma_hist: [0; 256],
            count: 0,
            pick_means: [0.0; 3],
            pick_count: 0,
        }
    }
}

impl Stats {
    /// 256-bin luma histogram (linear 0..1 domain)
    pub fn needs(r: &Recipe) -> bool {
        r.wb_mode == WbMode::Auto
            || r.wb_mode == WbMode::Pick
            || r.auto_exposure
            || r.auto_contrast
    }
    fn luma_percentile(&self, p: f32) -> f32 {
        let target = (self.count as f64 * p as f64) as u64;
        let mut acc = 0u64;
        for (i, &b) in self.luma_hist.iter().enumerate() {
            acc += b as u64;
            if acc >= target.max(1) {
                return i as f32 / 255.0;
            }
        }
        1.0
    }
}

fn inv3(m: [[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let [a, b, c] = m[0];
    let [d, e, f] = m[1];
    let [g, h, i] = m[2];
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-8 {
        return None;
    }
    let id = 1.0 / det;
    Some([
        [
            (e * i - f * h) * id,
            (c * h - b * i) * id,
            (b * f - c * e) * id,
        ],
        [
            (f * g - d * i) * id,
            (a * i - c * g) * id,
            (c * d - a * f) * id,
        ],
        [
            (d * h - e * g) * id,
            (b * g - a * h) * id,
            (a * e - b * d) * id,
        ],
    ])
}

fn mat_mul(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut o = [[0.0f32; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            o[r][c] = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c];
        }
    }
    o
}

fn norm_rows(mut m: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    for r in 0..3 {
        let s: f32 = m[r].iter().sum();
        if s.abs() > 1e-6 {
            for c in 0..3 {
                m[r][c] /= s;
            }
        }
    }
    m
}

fn catmull_lut(points: &[[f32; 2]]) -> Vec<f32> {
    curve_lut(points, |x| x, 1.0)
}

/// catmull-rom LUT with caller-chosen endpoints/default and output ceiling.
/// `default` supplies the endpoint values (and the whole curve when `points`
/// is empty), so identity/gain/remap curves share this path.
fn curve_lut(points: &[[f32; 2]], default: impl Fn(f32) -> f32, maxy: f32) -> Vec<f32> {
    // no user points: the curve is exactly the default mapping. (Degenerate
    // endpoint-only Catmull-Rom is NOT the identity — it bows ~20% dark at
    // x=0.25 — so the empty case must not go through the spline.)
    if points.is_empty() {
        return (0..256)
            .map(|i| default(i as f32 / 255.0).clamp(0.0, maxy))
            .collect();
    }
    // build a sorted point list incl. endpoints
    let mut pts: Vec<[f32; 2]> = Vec::new();
    pts.push([0.0, default(0.0)]);
    let mut mid: Vec<[f32; 2]> = points
        .iter()
        .map(|p| [p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0)])
        .collect();
    mid.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap());
    mid.retain(|p| p[0] > 0.0 && p[0] < 1.0);
    pts.extend(mid);
    pts.push([1.0, default(1.0)]);
    let n = pts.len();
    let mut lut = Vec::with_capacity(256);
    for i in 0..256 {
        let x = i as f32 / 255.0;
        // find segment
        let mut k = 0;
        while k + 1 < n && pts[k + 1][0] < x {
            k += 1;
        }
        let p0 = pts[k.saturating_sub(1)];
        let p1 = pts[k];
        let p2 = pts[(k + 1).min(n - 1)];
        let p3 = pts[(k + 2).min(n - 1)];
        let dx = (p2[0] - p1[0]).max(1e-6);
        let t = ((x - p1[0]) / dx).clamp(0.0, 1.0);
        let t2 = t * t;
        let t3 = t2 * t;
        let y = 0.5
            * ((2.0 * p1[1])
                + (-p0[1] + p2[1]) * t
                + (2.0 * p0[1] - 5.0 * p1[1] + 4.0 * p2[1] - p3[1]) * t2
                + (-p0[1] + 3.0 * p1[1] - 3.0 * p2[1] + p3[1]) * t3);
        lut.push(y.clamp(0.0, maxy));
    }
    lut
}

/// linear-interp sample of a packed 256-entry lut
fn lut_at(lut: &[f32], x: f32) -> f32 {
    let idx = (x.clamp(0.0, 1.0) * 255.0) as usize;
    lut[idx.min(255)]
}

/// rgb -> (hue 0..1, sat, v=max) — mirrored in the WGSL adjust
fn rgb_to_hsv(x: [f32; 3]) -> (f32, f32, f32) {
    let mx = x[0].max(x[1]).max(x[2]);
    let mn = x[0].min(x[1]).min(x[2]);
    let d = mx - mn;
    let s = if mx > 1e-6 { d / mx } else { 0.0 };
    let h = if d < 1e-6 {
        0.0
    } else if mx == x[0] {
        (x[1] - x[2]) / d / 6.0
    } else if mx == x[1] {
        (2.0 + (x[2] - x[0]) / d) / 6.0
    } else {
        (4.0 + (x[0] - x[1]) / d) / 6.0
    };
    (h - h.floor(), s, mx)
}

/// (hue 0..1, sat, v) -> rgb — standard HSV sector interpolation
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h6 = (h - h.floor()) * 6.0;
    let i = h6 as u32 % 6;
    let f = h6 - h6.floor();
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// cyclic distance between two hues in 0..1
fn hue_dist(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() - (a - b).abs().floor();
    d.min(1.0 - d)
}

/// skin-tone proximity for the beauty/smoothing mask
fn skin_mask(x: [f32; 3]) -> f32 {
    let (h, s, v) = rgb_to_hsv(x);
    let hw = 1.0 - sstep(0.02, 0.12, hue_dist(h, 0.075));
    let sw = sstep(0.05, 0.25, s);
    let lw = sstep(0.15, 0.35, v);
    hw * sw * lw
}

/// hue (0..1, 0=red) -> rgb for split-toning targets
fn hue_to_rgb(h: f32) -> [f32; 3] {
    let h = (h - h.floor()) * 6.0;
    let i = h as usize % 6;
    let f = h - h.floor();
    let seg = [
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 1.0, 1.0],
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 1.0],
    ];
    let a = seg[i];
    let b = seg[(i + 1) % 6];
    [
        a[0] + (b[0] - a[0]) * f,
        a[1] + (b[1] - a[1]) * f,
        a[2] + (b[2] - a[2]) * f,
    ]
}

/// cinematic presets: additive tweaks on top of the user's params.
fn apply_look(p: &mut Params, look: &str) {
    match look {
        "teal_orange" => {
            p.shadow_col = hue_to_rgb(0.52);
            p.shadow_sat = (p.shadow_sat + 0.30).min(1.0);
            p.high_col = hue_to_rgb(0.08);
            p.high_sat = (p.high_sat + 0.25).min(1.0);
            p.contrast += 0.12;
        }
        "film_fade" => {
            for c in &mut p.lift {
                *c += 0.05;
            }
            p.contrast -= 0.18;
            p.saturation -= 0.20;
        }
        "bleach" => {
            p.saturation -= 0.45;
            p.contrast += 0.22;
            p.highlights += 0.35;
            for c in &mut p.lift {
                *c += 0.015;
            }
        }
        "noir" => {
            p.saturation = -1.0;
            p.contrast += 0.30;
            for c in &mut p.lift {
                *c += 0.01;
            }
        }
        "matte" => {
            for c in &mut p.lift {
                *c += 0.035;
            }
            p.contrast -= 0.22;
            p.saturation -= 0.12;
            p.high_col = hue_to_rgb(0.10);
            p.high_sat = (p.high_sat + 0.15).min(1.0);
        }
        _ => {}
    }
}

pub fn build_params(m: &Mosaic, r: &Recipe, stats: Option<&Stats>) -> Params {
    // as-shot WB: prefer libraw's effective pre_mul (dcraw's actual channel
    // scaling), normalized to green. Fall back to cam_mul when absent.
    let mut wb = {
        let pm = m.pre_mul;
        if pm[1] > 0.0 && pm[0] > 0.0 && pm[2] > 0.0 {
            [pm[0] / pm[1], 1.0, pm[2] / pm[1]]
        } else {
            let g = if m.cam_mul[1] > 0.0 { m.cam_mul[1] } else { 1.0 };
            [m.cam_mul[0] / g, 1.0, if m.cam_mul[2] > 0.0 { m.cam_mul[2] / g } else { 1.0 }]
        }
    };
    if r.wb_mode == WbMode::Auto {
        if let Some(s) = stats {
            let gm = s.means[1].max(1e-4);
            wb = [gm / s.means[0].max(1e-4), 1.0, gm / s.means[2].max(1e-4)];
        }
    }
    if r.wb_mode == WbMode::Pick {
        if let Some(s) = stats {
            if s.pick_count > 0 {
                let gm = s.pick_means[1].max(1e-4);
                wb = [
                    gm / s.pick_means[0].max(1e-4),
                    1.0,
                    gm / s.pick_means[2].max(1e-4),
                ];
            }
        }
    }
    // relative temperature/tint shift (v1 approximation)
    let t = r.temperature.clamp(-1.0, 1.0);
    let ti = r.tint.clamp(-1.0, 1.0);
    wb[0] *= 1.0 + t * 0.35 + ti * 0.12;
    wb[1] *= 1.0 - ti * 0.30;
    wb[2] *= 1.0 - t * 0.35 + ti * 0.12;

    // cam -> sRGB: use libraw's precomputed rgb_cam (cam->sRGB, row-normalized).
    // WB is applied separately with a post-WB clip so blown highlights stay
    // white instead of shifting magenta.
    let cam2srgb = [
        [m.rgb_cam[0][0], m.rgb_cam[0][1], m.rgb_cam[0][2]],
        [m.rgb_cam[1][0], m.rgb_cam[1][1], m.rgb_cam[1][2]],
        [m.rgb_cam[2][0], m.rgb_cam[2][1], m.rgb_cam[2][2]],
    ];

    // post-WB+matrix mean luma: luma/M/WB are all linear, so luma of the
    // mean equals the mean of luma — exact scene-mean in the working space.
    let post_mean_luma = |s: &Stats| -> f32 {
        let cm = [s.means[0] * wb[0], s.means[1] * wb[1], s.means[2] * wb[2]];
        let mut l = 0.0f32;
        for (row, wgt) in cam2srgb.iter().zip([0.2126f32, 0.7152, 0.0722]) {
            l += wgt * (row[0] * cm[0] + row[1] * cm[1] + row[2] * cm[2]);
        }
        l
    };
    // auto exposure: steer scene mean luma toward 18% gray, cap ±3EV
    let mut exposure_mul = (2.0f32).powf(r.exposure);
    if r.auto_exposure {
        if let Some(s) = stats {
            let l = post_mean_luma(s);
            if l > 1e-4 {
                exposure_mul *= (2.0f32).powf((0.18f32 / l).log2().clamp(-3.0, 3.0));
            }
        }
    }
    // auto contrast: p1/p99 stretch, expressed in the post-exposure working
    // space (raw percentiles scaled by scene gain * exposure_mul)
    let (mut black_pt, mut white_pt) = (0.0f32, 1.0f32);
    if r.auto_contrast {
        if let Some(s) = stats {
            let scene_gain = if s.luma_mean > 1e-4 {
                post_mean_luma(s) / s.luma_mean
            } else {
                1.0
            };
            let f = scene_gain * exposure_mul;
            black_pt = (s.luma_percentile(0.01) * f).clamp(0.0, 0.35);
            white_pt = (s.luma_percentile(0.99) * f)
                .clamp(black_pt + 0.02, 1.2);
        }
    }

    let mut spots = [[0.0; 4]; 8];
    for (i, s) in r.spots.iter().take(8).enumerate() {
        spots[i] = [s[0].clamp(0.0, 1.0), s[1].clamp(0.0, 1.0), s[2].clamp(0.001, 0.3), 0.0];
    }
    let mut lights = [[0.0; 4]; 8];
    for (i, l) in r.lights.iter().take(8).enumerate() {
        lights[i] = [
            l[0].clamp(0.0, 1.0),
            l[1].clamp(0.0, 1.0),
            l[2].clamp(0.01, 1.0),
            l[3].clamp(-4.0, 4.0),
        ];
    }
    let crop = [
        r.crop[0].clamp(0.0, 0.9),
        r.crop[1].clamp(0.0, 0.9),
        r.crop[2].clamp(0.0, 0.9),
        r.crop[3].clamp(0.0, 0.9),
    ];

    // per-channel custom curves: 768-float packed luts, empty when unused
    let chan_luts = if r.curve_r.is_empty() && r.curve_g.is_empty() && r.curve_b.is_empty() {
        Vec::new()
    } else {
        let mut v = curve_lut(&r.curve_r, |x| x, 1.0);
        v.extend(curve_lut(&r.curve_g, |x| x, 1.0));
        v.extend(curve_lut(&r.curve_b, |x| x, 1.0));
        v
    };
    // hue-domain curves: hh/ss are remaps (default x), hs/hl/ls are gains (1)
    let hue_luts = if r.hue_hue.is_empty()
        && r.hue_sat.is_empty()
        && r.hue_lum.is_empty()
        && r.lum_sat.is_empty()
        && r.sat_sat.is_empty()
    {
        Vec::new()
    } else {
        let mut v = curve_lut(&r.hue_hue, |x| x, 1.0);
        v.extend(curve_lut(&r.hue_sat, |_| 1.0, 2.0));
        v.extend(curve_lut(&r.hue_lum, |_| 1.0, 2.0));
        v.extend(curve_lut(&r.lum_sat, |_| 1.0, 2.0));
        v.extend(curve_lut(&r.sat_sat, |x| x, 1.0));
        v
    };

    // power windows → packed [kind, p0..p5, ev, sat, temp, strength]
    // strength = opacity × enabled (folded so disabled windows are free).
    // windows are stored frame-normalized (pre-crop, matching the UI markers);
    // window_mask runs in dst-normalized post-crop space, so translate here.
    let sw = (1.0 - r.crop[0] - r.crop[2]).max(0.01);
    let sh = (1.0 - r.crop[1] - r.crop[3]).max(0.01);
    let mut wins = [[0.0; 12]; 4];
    for (i, w) in r.windows.iter().take(4).enumerate() {
        let grad = w.kind == "gradient";
        let (p0, p1, p2, p3) = if grad {
            // [x1,y1,x2,y2]: both endpoints move through the same map
            (
                (w.p[0] - r.crop[0]) / sw,
                (w.p[1] - r.crop[1]) / sh,
                (w.p[2] - r.crop[0]) / sw,
                (w.p[3] - r.crop[1]) / sh,
            )
        } else {
            // circle/ellipse: centre + radii rescale into crop space
            (
                (w.p[0] - r.crop[0]) / sw,
                (w.p[1] - r.crop[1]) / sh,
                w.p[2] / sw,
                w.p[3] / sh,
            )
        };
        wins[i] = [
            (if grad { 1.0 } else { 0.0 }) + (if w.invert { 2.0 } else { 0.0 }),
            p0,
            p1,
            p2,
            p3,
            w.p[4],
            w.p[5],
            w.ev.clamp(-4.0, 4.0),
            w.sat.clamp(-1.0, 1.0),
            w.temp.clamp(-1.0, 1.0),
            if w.enabled { w.opacity.clamp(0.0, 1.0) } else { 0.0 },
            0.0,
        ];
    }

    let mut clones = [[0.0; 6]; 8];
    for (i, c) in r.clones.iter().take(8).enumerate() {
        clones[i] = [
            c[0].clamp(0.0, 1.0),
            c[1].clamp(0.0, 1.0),
            c[2].clamp(0.0, 1.0),
            c[3].clamp(0.0, 1.0),
            c[4].clamp(0.001, 0.3),
            0.0,
        ];
    }

    let zone_pack = |z: [f32; 4]| -> [f32; 4] {
        [
            z[0] - z[0].floor(),
            z[1].clamp(0.0, 1.0),
            z[2].clamp(-4.0, 4.0),
            z[3].clamp(-1.0, 1.0),
        ]
    };
    let mut mixer = r.mixer;
    for c in &mut mixer {
        *c = c.clamp(-2.0, 2.0);
    }

    let mut p = Params {
        wb,
        m: cam2srgb,
        lut: catmull_lut(&r.curve),
        exposure_mul,
        contrast: r.contrast,
        highlights: r.highlights,
        shadows: r.shadows,
        whites: r.whites,
        blacks: r.blacks,
        saturation: r.saturation,
        vibrance: r.vibrance,
        sharpen: r.sharpen,
        noise_luma: r.noise_luma,
        rotation_deg: r.rotation_deg.clamp(-45.0, 45.0),
        clarity: r.clarity.clamp(-1.0, 1.0),
        vignette: r.vignette.clamp(-1.0, 1.0),
        grain: r.grain.clamp(0.0, 1.0),
        lift: [
            r.lift[0].clamp(-0.5, 0.5),
            r.lift[1].clamp(-0.5, 0.5),
            r.lift[2].clamp(-0.5, 0.5),
        ],
        gamma: [
            r.gamma[0].clamp(0.2, 5.0),
            r.gamma[1].clamp(0.2, 5.0),
            r.gamma[2].clamp(0.2, 5.0),
        ],
        gain: [
            r.gain[0].clamp(0.0, 4.0),
            r.gain[1].clamp(0.0, 4.0),
            r.gain[2].clamp(0.0, 4.0),
        ],
        shadow_col: hue_to_rgb(r.shadow_hue),
        shadow_sat: r.shadow_sat.clamp(0.0, 1.0),
        high_col: hue_to_rgb(r.highlight_hue),
        high_sat: r.highlight_sat.clamp(0.0, 1.0),
        black_pt,
        white_pt,
        spots,
        n_spots: r.spots.len().min(8) as u32,
        lights,
        n_lights: r.lights.len().min(8) as u32,
        crop,
        offset: [
            r.offset[0].clamp(-0.25, 0.25),
            r.offset[1].clamp(-0.25, 0.25),
            r.offset[2].clamp(-0.25, 0.25),
        ],
        mid_col: hue_to_rgb(r.midtone_hue),
        mid_sat: r.midtone_sat.clamp(0.0, 1.0),
        pivot: r.pivot.clamp(0.02, 0.8),
        hl_roll: r.highlight_rolloff.clamp(0.0, 2.0),
        sh_roll: r.shadow_rolloff.clamp(0.0, 2.0),
        chan_luts,
        hue_luts,
        qh: [
            r.qh[0] - r.qh[0].floor(),
            r.qh[1].clamp(0.0, 0.5),
            r.qh[2].clamp(0.0, 0.5),
        ],
        qs: [
            r.qs[0].clamp(0.0, 1.0),
            r.qs[1].clamp(0.0, 1.0),
            r.qs[2].clamp(0.0, 0.5),
        ],
        ql: [
            r.ql[0].clamp(0.0, 1.0),
            r.ql[1].clamp(0.0, 1.0),
            r.ql[2].clamp(0.0, 0.5),
        ],
        qadj: [
            r.qadj[0].clamp(-0.5, 0.5),
            r.qadj[1].clamp(-1.0, 1.0),
            r.qadj[2].clamp(-1.0, 1.0),
            r.qadj[3].clamp(-1.0, 1.0),
        ],
        q_invert: r.q_invert,
        q_clean: [r.q_clean[0].clamp(0.0, 1.0), r.q_clean[1].clamp(0.0, 1.0)],
        q_blur: r.q_blur.clamp(0.0, 1.0),
        q_show: r.q_show,
        // show-key mode works even with no adjustment applied
        has_qual: r.qh[1] > 0.0,
        wins,
        n_wins: r.windows.len().min(4) as u32,
        zones: [
            zone_pack(r.z_dark),
            zone_pack(r.z_shadow),
            zone_pack(r.z_light),
            zone_pack(r.z_global),
        ],
        mixer,
        mono: r.mono,
        clones,
        n_clones: r.clones.len().min(8) as u32,
        beauty: r.beauty.clamp(0.0, 1.0),
        noise_chroma: r.noise_chroma.clamp(0.0, 1.0),
        ca_fix: r.ca_fix.clamp(-0.5, 0.5),
        deband: r.deband.clamp(0.0, 1.0),
        glow: r.glow.clamp(0.0, 1.0),
        flare: [
            r.flare[0].clamp(0.0, 1.0),
            r.flare[1].clamp(0.0, 1.0),
            r.flare[2].clamp(0.0, 1.0),
            r.flare[3] - r.flare[3].floor(),
        ],
        zone_ev: {
            let mut z = r.zones_ev;
            for v in &mut z {
                *v = v.clamp(-4.0, 4.0);
            }
            z
        },
        lut3d: if !r.lut_file.is_empty() && r.lut_amount > 0.0 {
            crate::lut::load(&r.lut_file)
        } else {
            None
        },
        lut_amt: r.lut_amount.clamp(0.0, 1.0),
        key_v: r.key_v.clamp(-0.4, 0.4),
        key_h: r.key_h.clamp(-0.4, 0.4),
    };
    apply_look(&mut p, &r.look);
    p
}

/// smoothstep(edge0, edge1, x) helper
fn sstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn srgb_encode(v: f32) -> f32 {
    let c = v.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn srgb_decode(v: f32) -> f32 {
    let c = v.clamp(0.0, 1.0);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// tone equalizer: interpolated EV for a pixel of linear luma `l`.
/// Zones are centered at log2 EV -4..+4 (9 zones); linear interp between
/// centers == unit-partitioned triangle weights.
fn zone_ev_at(l: f32, z: &[f32; 9]) -> f32 {
    let e = l.max(1e-6).log2();
    let t = (e + 4.0).clamp(0.0, 8.0);
    let i = (t as usize).min(7);
    let f = t - i as f32;
    z[i] * (1.0 - f) + z[i + 1] * f
}

/// apply tone+color adjustments on linear sRGB triple. Shared logic with gpu shader.
/// order: exposure -> auto-contrast remap -> lift/gamma/gain -> blacks/whites ->
///        contrast -> shadows/highlights -> saturation/vibrance -> split tone -> LUT
fn adjust(rgb: [f32; 3], p: &Params) -> [f32; 3] {
    let mut x = rgb;
    // exposure
    for c in &mut x {
        *c *= p.exposure_mul;
    }
    // auto-contrast percentile remap
    if p.white_pt - p.black_pt < 0.999 || p.black_pt > 0.001 {
        for c in &mut x {
            *c = (*c - p.black_pt) / (p.white_pt - p.black_pt).max(0.02);
        }
    }
    // tone equalizer: per-zone exposure on the post-exposure luma
    if p.zone_ev.iter().any(|&v| v != 0.0) {
        let l = 0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2];
        let evg = 2.0f32.powf(zone_ev_at(l, &p.zone_ev));
        for c in &mut x {
            *c *= evg;
        }
    }
    // lift/gamma/gain: out = gain * pow(x + lift, 1/gamma)
    for c in 0..3 {
        if p.lift[c] != 0.0 || p.gamma[c] != 1.0 || p.gain[c] != 1.0 {
            x[c] = p.gain[c] * (x[c] + p.lift[c]).max(0.0).powf(1.0 / p.gamma[c]);
        }
    }
    // global offset wheel
    for c in 0..3 {
        x[c] += p.offset[c];
    }
    // blacks / whites remap
    for c in &mut x {
        if p.blacks > 0.0 {
            *c += p.blacks * 0.15 * (1.0 - *c);
        } else {
            *c *= 1.0 + p.blacks * 0.20;
        }
        *c *= 1.0 + p.whites * 0.20;
    }
    // contrast around adjustable pivot
    for c in &mut x {
        *c = (*c - p.pivot) * (1.0 + p.contrast * 0.9) + p.pivot;
    }
    // shadows lift / highlights recovery in upper/lower bands
    for c in &mut x {
        if p.shadows != 0.0 {
            let w = (1.0 - sstep(0.0, 0.45, *c)) * sstep(0.0, 0.06, *c);
            *c += p.shadows * w * 0.30;
        }
        if p.highlights != 0.0 {
            let w = sstep(0.45, 1.2, *c);
            *c += -p.highlights * w * 0.5 * *c;
        }
    }
    // highlight/shadow rolloff shaping (camera-raw style)
    if p.hl_roll != 1.0 {
        for c in &mut x {
            if p.hl_roll > 1.0 {
                *c = *c / (1.0 + (p.hl_roll - 1.0) * sstep(0.8, 1.6, *c));
            } else {
                *c *= 1.0 + (1.0 - p.hl_roll) * 0.5 * sstep(0.8, 1.6, *c);
            }
        }
    }
    if p.sh_roll != 1.0 {
        for c in &mut x {
            let w = 1.0 - sstep(0.0, 0.35, *c);
            *c *= 1.0 + (1.0 - p.sh_roll) * 0.4 * w;
            *c *= 1.0 - (p.sh_roll - 1.0).max(0.0) * 0.3 * w;
        }
    }
    // saturation / vibrance
    if p.saturation != 0.0 || p.vibrance != 0.0 {
        let luma = 0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2];
        let mx = x[0].max(x[1]).max(x[2]);
        let mn = x[0].min(x[1]).min(x[2]);
        let sat_now = if mx > 1e-5 { (mx - mn) / mx } else { 0.0 };
        let vib = p.vibrance * (1.0 - sat_now);
        let s = 1.0 + p.saturation + vib;
        for c in &mut x {
            *c = luma + (*c - luma) * s.max(0.0);
        }
    }
    // split toning: pull shadows/midtones/highlights toward their tint colors
    if p.shadow_sat > 0.0 || p.high_sat > 0.0 || p.mid_sat > 0.0 {
        let luma = 0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2];
        let ws = (1.0 - sstep(0.0, 0.55, luma)) * p.shadow_sat;
        let wh = sstep(0.45, 1.0, luma) * p.high_sat;
        let wm = (1.0 - (luma - 0.5).abs() * 2.0).max(0.0) * p.mid_sat;
        for c in 0..3 {
            x[c] += ws * (p.shadow_col[c] - luma) * 0.5
                + wh * (p.high_col[c] - luma) * 0.5
                + wm * (p.mid_col[c] - luma) * 0.5;
        }
    }
    // HDR zone wheels: per-band [hue, amt, ev, sat]
    for (i, z) in p.zones.iter().enumerate() {
        if z[1] == 0.0 && z[2] == 0.0 && z[3] == 0.0 {
            continue;
        }
        let luma = (0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2]).clamp(0.0, 1.0);
        let w = match i {
            0 => 1.0 - sstep(0.0, 0.18, luma),
            1 => sstep(0.05, 0.25, luma) * (1.0 - sstep(0.25, 0.55, luma)),
            2 => sstep(0.45, 0.75, luma),
            _ => 1.0,
        };
        if w <= 0.0 {
            continue;
        }
        let zc = hue_to_rgb(z[0]);
        let evg = (2.0f32).powf(z[2] * w);
        for c in 0..3 {
            x[c] = x[c] * evg + w * z[1] * (zc[c] - luma) * 0.4;
        }
        if z[3] != 0.0 {
            let l2 = 0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2];
            for c in 0..3 {
                x[c] = l2 + (x[c] - l2) * (1.0 + z[3] * w);
            }
        }
    }
    // RGB channel mixer (3x3, row-major)
    if p.mixer != [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
        x = [
            p.mixer[0] * x[0] + p.mixer[1] * x[1] + p.mixer[2] * x[2],
            p.mixer[3] * x[0] + p.mixer[4] * x[1] + p.mixer[5] * x[2],
            p.mixer[6] * x[0] + p.mixer[7] * x[1] + p.mixer[8] * x[2],
        ];
    }
    // monochrome conversion (mixer-style luma weights)
    if p.mono[0] != 0.0 || p.mono[1] != 0.0 || p.mono[2] != 0.0 {
        let g = p.mono[0] * x[0] + p.mono[1] * x[1] + p.mono[2] * x[2];
        x = [g, g, g];
    }
    // tone curve lut
    for c in &mut x {
        let idx = (c.clamp(0.0, 1.0) * 255.0) as usize;
        *c = p.lut[idx.min(255)];
    }
    // per-channel custom curves
    if !p.chan_luts.is_empty() {
        for c in 0..3 {
            x[c] = lut_at(&p.chan_luts[c * 256..], x[c]);
        }
    }
    // hue-domain curves (DaVinci Hue-vs-* and Lum/Sat-vs-Sat)
    if !p.hue_luts.is_empty() {
        let (h, s, v) = rgb_to_hsv(x);
        let lum = 0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2];
        let h2 = lut_at(&p.hue_luts[0..], h);
        let mut s2 = s * lut_at(&p.hue_luts[256..], h) * lut_at(&p.hue_luts[768..], lum);
        s2 = lut_at(&p.hue_luts[1024..], s2.clamp(0.0, 1.0));
        let l2 = lum * lut_at(&p.hue_luts[512..], lum);
        let mut x2 = hsv_to_rgb(h2, s2.clamp(0.0, 1.0), v);
        let l3 = 0.2126 * x2[0] + 0.7152 * x2[1] + 0.0722 * x2[2];
        if l3 > 1e-5 {
            for c in 0..3 {
                x2[c] *= l2 / l3;
            }
        }
        x = x2;
    }
    // HSL qualifier: soft windows on hue/sat/lum, adjust inside the mask.
    // Also runs when only q_show is set so the matte can be previewed.
    if p.has_qual || p.q_show {
        let (h, s, _v) = rgb_to_hsv(x);
        let l = 0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2];
        // matte finesse "blur" ≈ dilate soft edges on every channel
        let qb = p.q_blur * 0.25;
        let mh = 1.0 - sstep(p.qh[1], p.qh[1] + (p.qh[2] + qb).max(1e-4), hue_dist(h, p.qh[0]));
        let qs2 = p.qs[2] + qb;
        let ms = sstep(p.qs[0] - qs2, p.qs[0] + qs2, s)
            * (1.0 - sstep(p.qs[1] - qs2, p.qs[1] + qs2, s));
        let ql2 = p.ql[2] + qb;
        let ml = sstep(p.ql[0] - ql2, p.ql[0] + ql2, l)
            * (1.0 - sstep(p.ql[1] - ql2, p.ql[1] + ql2, l));
        let mut mask = mh * ms * ml;
        // clean black/white: remap the matte to push mid-tones to 0/1
        let cb = p.q_clean[0];
        let cw = p.q_clean[1];
        if cb > 0.0 || cw < 1.0 {
            mask = ((mask - cb) / (cw - cb).max(1e-4)).clamp(0.0, 1.0);
        }
        if p.q_invert {
            mask = 1.0 - mask;
        }
        // highlight/isolate: desaturate everything outside the key
        if p.q_show {
            for c in 0..3 {
                x[c] = l + (x[c] - l) * mask;
            }
        }
        if mask > 0.001 && p.has_qual {
            let h2 = h + p.qadj[0];
            let s2 = (s * (1.0 + p.qadj[1])).clamp(0.0, 1.0);
            let mut xq = hsv_to_rgb(h2, s2, _v);
            let lq = 0.2126 * xq[0] + 0.7152 * xq[1] + 0.0722 * xq[2];
            if lq > 1e-5 {
                for c in 0..3 {
                    xq[c] *= (l * (1.0 + p.qadj[2])) / lq;
                }
            }
            xq[0] += p.qadj[3] * 0.06;
            xq[2] -= p.qadj[3] * 0.06;
            for c in 0..3 {
                x[c] += (xq[c] - x[c]) * mask;
            }
        }
    }
    // imported .cube LUT: run display-referred (encode -> lut -> decode back
    // to linear so grain/vignette/windows keep working in the linear domain)
    if let Some(cube) = &p.lut3d {
        let enc = [
            srgb_encode(x[0]),
            srgb_encode(x[1]),
            srgb_encode(x[2]),
        ];
        let v = cube.sample(enc[0], enc[1], enc[2]);
        for c in 0..3 {
            x[c] = srgb_decode(enc[c] + (v[c] - enc[c]) * p.lut_amt);
        }
    }
    x
}

/// generic same-colour-mean demosaic (works for bayer & xtrans).
/// Each virtual pixel covers a `stride`x`stride` block of sensor pixels at
/// (left + vx*stride, top + vy*stride). The CFA phase is computed on raw
/// sensor coordinates, which is what libraw's filters/xtrans refer to.
/// When a channel has no samples inside the block (small stride), the
/// window is widened until every channel is covered.
fn demosaic_pixel(
    m: &Mosaic,
    vx: usize,
    vy: usize,
    stride: usize,
    norm: &[f32; 4],
) -> [f32; 3] {
    let cfa = &m.cfa;
    let x0 = m.left + vx * stride;
    let y0 = m.top + vy * stride;
    let x_end = m.left + m.w;
    let y_end = m.top + m.h;
    let mut sums = [0.0f32; 3];
    let mut cnts = [0u32; 3];
    let mut pad = 0usize;
    for _ in 0..6 {
        sums = [0.0; 3];
        cnts = [0; 3];
        let ys = y0.saturating_sub(pad).max(m.top);
        let ye = (y0 + stride + pad).min(y_end);
        let xs = x0.saturating_sub(pad).max(m.left);
        let xe = (x0 + stride + pad).min(x_end);
        for sy in ys..ye {
            let row = sy * m.raw_w;
            for sx in xs..xe {
                let col = cfa.color_at(sx % cfa.w, sy % cfa.h) as usize;
                let cc = if col == 3 { 1 } else { col };
                let raw = m.data[row + sx] as f32;
                sums[cc] += (raw - m.black[col.min(3)]) * norm[col.min(3)];
                cnts[cc] += 1;
            }
        }
        if cnts[0] > 0 && cnts[1] > 0 && cnts[2] > 0 {
            break;
        }
        pad += 2;
    }
    let mut out = [0.0f32; 3];
    for ch in 0..3 {
        if cnts[ch] > 0 {
            out[ch] = sums[ch] / cnts[ch] as f32;
        }
    }
    out
}

/// black level + white point normalization factors per CFA colour index.
fn norm_factors(m: &Mosaic) -> [f32; 4] {
    let mut n = [0.0f32; 4];
    for c in 0..4 {
        let denom = (m.maximum as f32 - m.black[c]).max(1.0);
        n[c] = 1.0 / denom;
    }
    n
}

/// Full development: decoded -> rgba8 oriented, fit inside max_px (0 = full res).
pub fn develop_cpu(d: &Decoded, r: &Recipe, max_px: u32) -> RgbaImage {
    match d {
        Decoded::Mosaic(m) => develop_mosaic(m, r, max_px),
        Decoded::Raster {
            rgba, w, h, flip, ..
        } => develop_raster(rgba, *w, *h, *flip, r, max_px),
    }
}

fn develop_mosaic(m: &Mosaic, r: &Recipe, max_px: u32) -> RgbaImage {
    // virtual stride: subsample to roughly fit max_px, keeping CFA phase
    let period = m.cfa.h.max(1);
    let mut stride = 1usize;
    if max_px > 0 {
        let ratio = (m.w.max(m.h) as f32 / max_px as f32).max(1.0);
        stride = ((ratio / period as f32).floor() as usize).max(1) * period;
    }
    let vw = m.w / stride;
    let vh = m.h / stride;
    let norm = norm_factors(m);

    // sparse scene statistics for auto WB / auto exposure / auto contrast
    let stats = if Stats::needs(r) {
        let mut s = Stats::default();
        let mut acc = [0.0f64; 3];
        let mut luma = 0.0f64;
        let step = 4usize; // sparse sample
        // WB pick rectangle (virtual src px); empty when not picking
        let pr = if r.wb_mode == WbMode::Pick {
            pick_rect(r.wb_pick, vw, vh, m.info.flip, r.wb_pick_size)
        } else {
            [-1.0; 4]
        };
        let mut pacc = [0.0f64; 3];
        let mut pcnt = 0u64;
        for vy in (0..vh).step_by(step) {
            for vx in (0..vw).step_by(step) {
                let c = demosaic_pixel(m, vx, vy, stride, &norm);
                for ch in 0..3 {
                    acc[ch] += c[ch] as f64;
                }
                let l = (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]).clamp(0.0, 1.0);
                luma += l as f64;
                s.luma_hist[(l * 255.0) as usize] += 1;
                s.count += 1;
                if vx as f32 >= pr[0] && vx as f32 <= pr[2] && vy as f32 >= pr[1] && vy as f32 <= pr[3] {
                    for ch in 0..3 {
                        pacc[ch] += c[ch] as f64;
                    }
                    pcnt += 1;
                }
            }
        }
        if pcnt > 0 {
            s.pick_means = [
                (pacc[0] / pcnt as f64) as f32,
                (pacc[1] / pcnt as f64) as f32,
                (pacc[2] / pcnt as f64) as f32,
            ];
            s.pick_count = pcnt;
        }
        s.means = [
            (acc[0] / s.count as f64) as f32,
            (acc[1] / s.count as f64) as f32,
            (acc[2] / s.count as f64) as f32,
        ];
        s.luma_mean = (luma / s.count as f64) as f32;
        if std::env::var_os("KOU_STATS").is_some() {
            eprintln!("[stats] means={:?} luma_mean={:.4} count={}", s.means, s.luma_mean, s.count);
            eprintln!(
                "[stats] p1={:.4} p50={:.4} p99={:.4}",
                s.luma_percentile(0.01),
                s.luma_percentile(0.5),
                s.luma_percentile(0.99)
            );
        }
        Some(s)
    } else {
        None
    };
    let p = build_params(m, r, stats.as_ref());

    let mut lin = vec![[0.0f32; 3]; vw * vh];
    for vy in 0..vh {
        for vx in 0..vw {
            let cam = demosaic_pixel(m, vx, vy, stride, &norm);
            // WB in camera space, clip at sensor white -> neutral highlights
            let cw = [
                (cam[0] * p.wb[0]).clamp(0.0, 1.0),
                (cam[1] * p.wb[1]).clamp(0.0, 1.0),
                (cam[2] * p.wb[2]).clamp(0.0, 1.0),
            ];
            let mut out = [0.0f32; 3];
            for c in 0..3 {
                out[c] = p.m[c][0] * cw[0] + p.m[c][1] * cw[1] + p.m[c][2] * cw[2];
            }
            lin[vy * vw + vx] = out;
        }
    }

    finish_linear(lin, vw, vh, &p, m.info.flip, max_px)
}

fn develop_raster(
    rgba: &[u16],
    w: usize,
    h: usize,
    flip: i32,
    r: &Recipe,
    max_px: u32,
) -> RgbaImage {
    let fake = Mosaic {
        data: Vec::new(),
        raw_w: 0,
        raw_h: 0,
        left: 0,
        top: 0,
        w: 0,
        h: 0,
        black: [0.0; 4],
        maximum: 65535,
        cam_mul: [1.0; 4],
        cam_xyz: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0; 3]],
        rgb_cam: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
        pre_mul: [1.0; 4],
        cfa: crate::decode::CfaPattern {
            w: 2,
            h: 2,
            cells: vec![0, 1, 1, 2],
        },
        info: crate::decode::CameraInfo {
            make: String::new(),
            model: String::new(),
            lens: String::new(),
            iso: 0.0,
            shutter: 0.0,
            aperture: 0.0,
            focal: 0.0,
            timestamp: 0,
            flip,
        },
    };
    // raster: histogram on the decoded image feeds auto-exposure/contrast
    let stats = if Stats::needs(r) {
        let mut s = Stats::default();
        for px in rgba.chunks_exact(4).step_by(4) {
            let l = (0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32) / 65535.0;
            s.luma_hist[(l.clamp(0.0, 1.0) * 255.0) as usize] += 1;
            s.luma_mean += l;
            s.count += 1;
        }
        s.luma_mean /= (s.count.max(1)) as f32;
        s.means = [s.luma_mean; 3];
        Some(s)
    } else {
        None
    };
    let p = build_params(&fake, r, stats.as_ref());
    // raster is already sRGB-encoded u16: decode gamma to linear, adjust, re-encode
    let mut lin = vec![[0.0f32; 3]; w * h];
    for (i, px) in rgba.chunks_exact(4).enumerate() {
        for c in 0..3 {
            let v = px[c] as f32 / 65535.0;
            lin[i][c] = if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            };
        }
    }
    finish_linear(lin, w, h, &p, flip, max_px)
}

/// frame-pixel dims + crop rect + rotation pivot shared by CPU/GPU.
/// returns (fw, fh, crop l/t in px, crop w/h in px, dw, dh)
pub(crate) fn frame_geometry(
    w: usize,
    h: usize,
    flip: i32,
    crop: [f32; 4],
    max_px: u32,
) -> (usize, usize, f32, f32, f32, f32, usize, usize) {
    let (fw, fh) = match flip {
        5 | 6 => (h, w),
        _ => (w, h),
    };
    let cl = crop[0] * fw as f32;
    let ct = crop[1] * fh as f32;
    let ew = (fw as f32 * (1.0 - crop[0] - crop[2])).max(1.0);
    let eh = (fh as f32 * (1.0 - crop[1] - crop[3])).max(1.0);
    let (dw, dh) = if max_px > 0 {
        fit(ew as u32, eh as u32, max_px)
    } else {
        (ew.round().max(1.0) as u32, eh.round().max(1.0) as u32)
    };
    (fw, fh, cl, ct, ew, eh, dw.max(1) as usize, dh.max(1) as usize)
}

/// spot heal on the demosaiced buffer: replace each spot's interior with the
/// mean of a ring sampled just outside its radius (dust/blemish removal).
/// spots are [cx,cy,r] in normalized POST-FLIP frame coords.
/// convert a normalized post-flip-frame spot [cx,cy,r] to virtual-src px
/// (sx, sy, radius_px) — shared by CPU heal and the GPU uniform builder.
/// normalized post-flip frame point → virtual-src px (shared by spots/clones/pick)
pub fn frame_to_src(nx: f32, ny: f32, w: usize, h: usize, flip: i32) -> (f32, f32) {
    let (fw, fh) = match flip {
        5 | 6 => (h, w),
        _ => (w, h),
    };
    let fx = nx * fw as f32;
    let fy = ny * fh as f32;
    match flip {
        3 => (w as f32 - 1.0 - fx, h as f32 - 1.0 - fy),
        6 => (fy, h as f32 - 1.0 - fx),
        5 => (w as f32 - 1.0 - fy, fx),
        _ => (fx, fy),
    }
}

pub fn spot_to_src(s: [f32; 4], w: usize, h: usize, flip: i32) -> (f32, f32, f32) {
    let (sx, sy) = frame_to_src(s[0], s[1], w, h, flip);
    let (fw, fh) = match flip {
        5 | 6 => (h, w),
        _ => (w, h),
    };
    (sx, sy, (s[2] * fw.max(fh) as f32).max(2.0))
}

/// WB pick rectangle in virtual-src px: `size` is the half-width as a
/// fraction of the longer frame side (default ~0.025 = 5% box)
pub fn pick_rect(pick: [f32; 2], w: usize, h: usize, flip: i32, size: f32) -> [f32; 4] {
    let (cx, cy) = frame_to_src(pick[0], pick[1], w, h, flip);
    let (fw, fh) = match flip {
        5 | 6 => (h, w),
        _ => (w, h),
    };
    let r = size.clamp(0.002, 0.2) * fw.max(fh) as f32;
    [cx - r, cy - r, cx + r, cy + r]
}

/// clone stamps: copy a source patch into the destination circle (patch replacer)
fn clone_lin(lin: &mut [[f32; 3]], w: usize, h: usize, clones: &[[f32; 6]; 8], n: u32, flip: i32) {
    if n == 0 {
        return;
    }
    let orig = lin.to_vec();
    let sample = |x: f32, y: f32| -> [f32; 3] {
        let xi = (x.round() as i32).clamp(0, w as i32 - 1) as usize;
        let yi = (y.round() as i32).clamp(0, h as i32 - 1) as usize;
        orig[yi * w + xi]
    };
    for c in clones.iter().take(n as usize) {
        let (sx, sy) = frame_to_src(c[0], c[1], w, h, flip);
        let (dx, dy) = frame_to_src(c[2], c[3], w, h, flip);
        let (fw, fh) = match flip {
            5 | 6 => (h, w),
            _ => (w, h),
        };
        let r = (c[4] * fw.max(fh) as f32).max(2.0);
        let x0 = (dx - r).max(0.0) as i32;
        let x1 = (dx + r).min(w as f32 - 1.0) as i32;
        let y0 = (dy - r).max(0.0) as i32;
        let y1 = (dy + r).min(h as f32 - 1.0) as i32;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = ((x as f32 - dx).hypot(y as f32 - dy)) / r;
                if d < 1.0 {
                    let src = sample(x as f32 + sx - dx, y as f32 + sy - dy);
                    let blend = 1.0 - sstep(0.7, 1.0, d);
                    let i = y as usize * w + x as usize;
                    for ch in 0..3 {
                        lin[i][ch] = lin[i][ch] * (1.0 - blend) + src[ch] * blend;
                    }
                }
            }
        }
    }
}

fn heal_lin(lin: &mut [[f32; 3]], w: usize, h: usize, spots: &[[f32; 4]; 8], n: u32, flip: i32) {
    if n == 0 {
        return;
    }
    let orig = lin.to_vec();
    // clamped sampling (same as the GPU heal pass)
    let sample = |x: f32, y: f32| -> [f32; 3] {
        let xi = (x.round() as i32).clamp(0, w as i32 - 1) as usize;
        let yi = (y.round() as i32).clamp(0, h as i32 - 1) as usize;
        orig[yi * w + xi]
    };
    for s in spots.iter().take(n as usize) {
        let (sx, sy, r) = spot_to_src(*s, w, h, flip);
        // ring mean just outside the spot
        let mut ring = [0.0f32; 3];
        let ring_r = r * 1.4;
        for k in 0..8 {
            let a = k as f32 * std::f32::consts::TAU / 8.0;
            let v = sample(sx + ring_r * a.cos(), sy + ring_r * a.sin());
            for c in 0..3 {
                ring[c] += v[c] / 8.0;
            }
        }
        let x0 = (sx - r).max(0.0) as i32;
        let x1 = (sx + r).min(w as f32 - 1.0) as i32;
        let y0 = (sy - r).max(0.0) as i32;
        let y1 = (sy + r).min(h as f32 - 1.0) as i32;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = ((x as f32 - sx).hypot(y as f32 - sy)) / r;
                if d < 1.0 {
                    // soft edge + slight texture preservation via neighbor noise
                    let blend = 1.0 - sstep(0.7, 1.0, d);
                    let i = y as usize * w + x as usize;
                    for c in 0..3 {
                        lin[i][c] = lin[i][c] * (1.0 - blend) + ring[c] * blend;
                    }
                }
            }
        }
    }
}

/// shared tail: spatial ops -> straighten/flip/crop/fit-resize -> adjust ->
/// dodge/burn -> grain/vignette -> gamma. Same ordering as the gpu.rs finish pass.
fn finish_linear(
    mut lin: Vec<[f32; 3]>,
    w: usize,
    h: usize,
    p: &Params,
    flip: i32,
    max_px: u32,
) -> RgbaImage {
    heal_lin(&mut lin, w, h, &p.spots, p.n_spots, flip);
    clone_lin(&mut lin, w, h, &p.clones, p.n_clones, flip);
    let nr = p.noise_luma.max(p.noise_chroma);
    if nr > 0.0 {
        lin = chroma_smooth(&lin, w, h, nr);
    }
    if p.beauty > 0.0 || p.deband > 0.0 {
        lin = beauty_deband(&lin, w, h, p.beauty, p.deband);
    }
    if p.glow > 0.0 {
        lin = glow_lin(&lin, w, h, p.glow);
    }
    if p.sharpen > 0.0 || p.clarity != 0.0 {
        lin = sharpen_clarity(&lin, w, h, p.sharpen, p.clarity);
    }
    let (_fw, _fh, cl, ct, ew, eh, dw, dh) = frame_geometry(w, h, flip, p.crop, max_px);
    let cx = cl + ew * 0.5 - 0.5;
    let cy = ct + eh * 0.5 - 0.5;
    let sin = p.rotation_deg.to_radians().sin();
    let cos = p.rotation_deg.to_radians().cos();
    let mut out = vec![0u8; dw * dh * 4];
    for dy in 0..dh {
        for dx in 0..dw {
            // dst -> crop rect in post-flip frame, undo straighten, undo flip -> src
            let nx = (dx as f32 + 0.5) / dw as f32;
            let ny = (dy as f32 + 0.5) / dh as f32;
            let mut fx = cl + nx * ew - 0.5;
            let mut fy = ct + ny * eh - 0.5;
            // keystone: trapezoid warp about the crop centre
            if p.key_v != 0.0 || p.key_h != 0.0 {
                fx = cx + (fx - cx) * (1.0 + p.key_v * (ny * 2.0 - 1.0));
                fy = cy + (fy - cy) * (1.0 + p.key_h * (nx * 2.0 - 1.0));
            }
            let px = fx - cx;
            let py = fy - cy;
            fx = cx + px * cos + py * sin;
            fy = cy - px * sin + py * cos;
            let (sx, sy) = match flip {
                3 => (w as f32 - 1.0 - fx, h as f32 - 1.0 - fy),
                6 => (fy, h as f32 - 1.0 - fx),
                5 => (w as f32 - 1.0 - fy, fx),
                _ => (fx, fy),
            };
            // chromatic-aberration fix: radial per-channel sample offsets
            let col = if p.ca_fix != 0.0 {
                let wcx = w as f32 * 0.5;
                let wcy = h as f32 * 0.5;
                let rn = ((sx - wcx).hypot(sy - wcy) / w.max(h) as f32) * 2.0;
                let fr = 1.0 - p.ca_fix * 0.05 * rn;
                let fb = 1.0 + p.ca_fix * 0.05 * rn;
                [
                    bilin_ch(&lin, w, h, wcx + (sx - wcx) * fr, wcy + (sy - wcy) * fr, 0),
                    bilin_ch(&lin, w, h, sx, sy, 1),
                    bilin_ch(&lin, w, h, wcx + (sx - wcx) * fb, wcy + (sy - wcy) * fb, 2),
                ]
            } else {
                bilinear(&lin, w, h, sx, sy)
            };
            let mut adj = adjust(col, p);
            // dodge/burn radial lights (dst-normalized coords)
            for l in p.lights.iter().take(p.n_lights as usize) {
                let d = ((nx - l[0]).hypot(ny - l[1])) / l[2].max(1e-3);
                let f = (-d * d * 2.77).exp(); // gaussian falloff
                adj[0] *= 1.0 + l[3] * f * 0.5;
                adj[1] *= 1.0 + l[3] * f * 0.5;
                adj[2] *= 1.0 + l[3] * f * 0.5;
            }
            // power windows: local ev/sat/temp inside the mask
            for win in p.wins.iter().take(p.n_wins as usize) {
                let mask = window_mask(win, nx, ny) * win[10];
                if mask <= 0.001 {
                    continue;
                }
                let evg = (2.0f32).powf(win[7] * mask);
                let l = 0.2126 * adj[0] + 0.7152 * adj[1] + 0.0722 * adj[2];
                for c in 0..3 {
                    adj[c] = adj[c] * evg;
                    adj[c] = l + (adj[c] - l) * (1.0 + win[8] * mask);
                }
                adj[0] += win[9] * mask * 0.08;
                adj[2] -= win[9] * mask * 0.08;
            }
            // lens flare: core glow + horizontal streak + mirrored ghost ring
            if p.flare[2] > 0.0 {
                let (fx0, fy0, fs) = (p.flare[0], p.flare[1], p.flare[2]);
                let dx = nx - fx0;
                let dy = ny - fy0;
                let d2 = dx * dx + dy * dy;
                let core = (-d2 / 0.004).exp();
                let streak = (-dy * dy / (0.0004 + 0.02 * fs)).exp() * (-dx.abs() / 0.35).exp();
                let gx = 1.0 - fx0;
                let gy = 1.0 - fy0;
                let gd = ((nx - gx).hypot(ny - gy) - 0.10).abs();
                let ghost = (-gd * gd / 0.0008).exp();
                let colf = hue_to_rgb(p.flare[3]);
                let f = fs * (0.5 * core + 0.7 * streak + 0.35 * ghost);
                for c in 0..3 {
                    adj[c] += f * colf[c];
                }
            }
            if p.grain > 0.0 {
                for (c, seed) in adj.iter_mut().zip([0.0f32, 17.0, 43.0]) {
                    *c += (hash_px(dx, dy, seed) - 0.5) * p.grain * 0.12;
                }
            }
            if p.vignette != 0.0 {
                let vx = nx * 2.0 - 1.0;
                let vy = ny * 2.0 - 1.0;
                let d = (vx * vx + vy * vy).sqrt() * 0.7071;
                let f = 1.0 - p.vignette * sstep(0.35, 1.05, d) * 0.9;
                for c in &mut adj {
                    *c *= f;
                }
            }
            let o = (dy * dw + dx) * 4;
            for c in 0..3 {
                out[o + c] = (srgb_encode(adj[c].clamp(0.0, 1.0)) * 255.0 + 0.5) as u8;
            }
            out[o + 3] = 255;
        }
    }
    RgbaImage {
        width: dw as u32,
        height: dh as u32,
        data: out,
    }
}

/// power-window mask value at dst-normalized (nx,ny)
/// packed [kind(+2=invert), a,b,c,d, rot, soft, ev, sat, temp, strength]
fn window_mask(w: &[f32], nx: f32, ny: f32) -> f32 {
    let kind = w[0] as i32 % 2;
    let inv = w[0] >= 2.0;
    let mask = if kind == 1 {
        // gradient: [x1,y1,x2,y2,*,soft] — full cover before the p1..p2 span
        let (ax, ay, bx, by) = (w[1], w[2], w[3], w[4]);
        let soft = w[6].max(0.02);
        let dx = bx - ax;
        let dy = by - ay;
        let len2 = (dx * dx + dy * dy).max(1e-6);
        let t = ((nx - ax) * dx + (ny - ay) * dy) / len2;
        1.0 - sstep(0.5 - soft * 0.5, 0.5 + soft * 0.5, t)
    } else {
        // circle/ellipse: [cx,cy,rx,ry,rot,soft]
        let rot = w[5].to_radians();
        let dx = nx - w[1];
        let dy = ny - w[2];
        let rx = w[3].max(0.005);
        let ry = w[4].max(0.005);
        let ux = (dx * rot.cos() + dy * rot.sin()) / rx;
        let uy = (-dx * rot.sin() + dy * rot.cos()) / ry;
        let d = (ux * ux + uy * uy).sqrt();
        1.0 - sstep(1.0 - w[6].clamp(0.0, 0.95), 1.0, d)
    };
    if inv {
        1.0 - mask
    } else {
        mask
    }
}

/// single-channel bilinear sample (for CA-corrected per-channel taps)
fn bilin_ch(lin: &[[f32; 3]], w: usize, h: usize, sx: f32, sy: f32, ch: usize) -> f32 {
    let x0 = sx.floor() as i32;
    let y0 = sy.floor() as i32;
    if x0 < 0 || y0 < 0 || x0 + 1 >= w as i32 || y0 + 1 >= h as i32 {
        if x0 >= 0 && y0 >= 0 && (x0 as usize) < w && (y0 as usize) < h {
            return lin[y0 as usize * w + x0 as usize][ch];
        }
        return 0.0;
    }
    let tx = sx - x0 as f32;
    let ty = sy - y0 as f32;
    let (x0, y0) = (x0 as usize, y0 as usize);
    let a = lin[y0 * w + x0][ch] * (1.0 - tx) + lin[y0 * w + x0 + 1][ch] * tx;
    let b = lin[(y0 + 1) * w + x0][ch] * (1.0 - tx) + lin[(y0 + 1) * w + x0 + 1][ch] * tx;
    a * (1.0 - ty) + b * ty
}

/// beauty (skin-masked smoothing) + deband (flat-gradient smoothing)
fn beauty_deband(lin: &[[f32; 3]], w: usize, h: usize, beauty: f32, deband: f32) -> Vec<[f32; 3]> {
    let chans: Vec<Vec<f32>> = (0..3)
        .map(|c| lin.iter().map(|p| p[c]).collect())
        .collect();
    let mut out = vec![[0.0f32; 3]; lin.len()];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let c = lin[i];
            let m = [box3(&chans[0], w, h, x, y), box3(&chans[1], w, h, x, y), box3(&chans[2], w, h, x, y)];
            let skin = if beauty > 0.0 { skin_mask(c) } else { 0.0 };
            let flat = {
                let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
                let lm = 0.2126 * m[0] + 0.7152 * m[1] + 0.0722 * m[2];
                1.0 - sstep(0.004, 0.03, (l - lm).abs())
            };
            let wgt = (beauty * skin + deband * flat).min(1.0);
            for ch in 0..3 {
                out[i][ch] = c[ch] * (1.0 - wgt) + m[ch] * wgt;
            }
        }
    }
    out
}

/// lens glow: extract highlights, wide box blur, add back
fn glow_lin(lin: &[[f32; 3]], w: usize, h: usize, amt: f32) -> Vec<[f32; 3]> {
    let hi: Vec<f32> = lin
        .iter()
        .map(|c| {
            let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
            sstep(0.55, 0.9, l)
        })
        .collect();
    let mut out = Vec::with_capacity(lin.len());
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut m = [0.0f32; 3];
            let mut n = 0u32;
            for dy in -4i32..=4 {
                for dx in -4i32..=4 {
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                        let j = ny as usize * w + nx as usize;
                        for c in 0..3 {
                            m[c] += lin[j][c] * hi[j];
                        }
                        n += 1;
                    }
                }
            }
            let f = amt * 0.8 / n.max(1) as f32;
            out.push([
                lin[i][0] + m[0] * f,
                lin[i][1] + m[1] * f,
                lin[i][2] + m[2] * f,
            ]);
        }
    }
    out
}

fn bilinear(lin: &[[f32; 3]], w: usize, h: usize, sx: f32, sy: f32) -> [f32; 3] {
    let x0 = sx.floor() as i32;
    let y0 = sy.floor() as i32;
    if x0 < 0 || y0 < 0 || x0 + 1 >= w as i32 || y0 + 1 >= h as i32 {
        if x0 >= 0 && y0 >= 0 && (x0 as usize) < w && (y0 as usize) < h {
            return lin[y0 as usize * w + x0 as usize];
        }
        return [0.0; 3];
    }
    let tx = sx - x0 as f32;
    let ty = sy - y0 as f32;
    let (x0, y0) = (x0 as usize, y0 as usize);
    let at = |x: usize, y: usize| lin[y * w + x];
    let mut o = [0.0f32; 3];
    for c in 0..3 {
        let a = at(x0, y0)[c] * (1.0 - tx) + at(x0 + 1, y0)[c] * tx;
        let b = at(x0, y0 + 1)[c] * (1.0 - tx) + at(x0 + 1, y0 + 1)[c] * tx;
        o[c] = a * (1.0 - ty) + b * ty;
    }
    o
}

fn hash_px(x: usize, y: usize, seed: f32) -> f32 {
    // same integer hash as gpu.rs `hash` so grain is identical on both paths
    let mut h = (x as u32)
        .wrapping_mul(2654435761)
        .wrapping_add((y as u32).wrapping_mul(2246822519))
        .wrapping_add(((seed + 1.0) as u32).wrapping_mul(3266489917));
    h = (h ^ (h >> 13)).wrapping_mul(1103515245);
    h ^= h >> 16;
    h as f32 * (1.0 / 4294967296.0)
}

/// Video-style scopes computed from rendered rgba8.
/// `wave`: per-channel column histograms, wave[c*65536 + v*256 + col]
/// `vec`: YCbCr chroma plane, vec[cr*256 + cb]
/// `cie`: CIE xy chromaticity plane, cie[yi*256 + xi]
pub fn scopes(rgba8: &[u8], w: u32, h: u32, wave: &mut [u32], vec: &mut [u32], cie: &mut [u32]) {
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 {
        return;
    }
    // stride so we touch at most ~512K pixels regardless of input size
    let step = ((w * h) as f64 / 524288.0).sqrt().ceil().max(1.0) as usize;
    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            let i = (y * w + x) * 4;
            let (r, g, b) = (
                rgba8[i] as f32 / 255.0,
                rgba8[i + 1] as f32 / 255.0,
                rgba8[i + 2] as f32 / 255.0,
            );
            let col = x * 255 / w;
            for (c, v) in [r, g, b].iter().enumerate() {
                let bin = (v.clamp(0.0, 1.0) * 255.0) as usize;
                wave[c * 65536 + bin * 256 + col] += 1;
            }
            // BT.601 chroma for the vectorscope
            let cb = (-0.168736 * r - 0.331264 * g + 0.5 * b) * 255.0 + 128.0;
            let cr = (0.5 * r - 0.418688 * g - 0.081312 * b) * 255.0 + 128.0;
            let cbi = cb.clamp(0.0, 255.0) as usize;
            let cri = cr.clamp(0.0, 255.0) as usize;
            vec[cri * 256 + cbi] += 1;
            // sRGB -> CIE XYZ -> xy chromaticity
            let lr = if r <= 0.04045 { r / 12.92 } else { ((r + 0.055) / 1.055).powf(2.4) };
            let lg = if g <= 0.04045 { g / 12.92 } else { ((g + 0.055) / 1.055).powf(2.4) };
            let lb = if b <= 0.04045 { b / 12.92 } else { ((b + 0.055) / 1.055).powf(2.4) };
            let xx = 0.4124 * lr + 0.3576 * lg + 0.1805 * lb;
            let yy = 0.2126 * lr + 0.7152 * lg + 0.0722 * lb;
            let zz = 0.0193 * lr + 0.1192 * lg + 0.9505 * lb;
            let s = xx + yy + zz;
            if s > 1e-6 {
                let xi = (xx / s * 255.0).clamp(0.0, 255.0) as usize;
                let yi = (yy / s * 255.0).clamp(0.0, 255.0) as usize;
                cie[yi * 256 + xi] += 1;
            }
        }
    }
}

/// 256-bin histogram of rendered rgba8 pixels: [R,G,B,luma] * 256.
pub fn histogram(rgba8: &[u8]) -> [u32; 1024] {
    let mut h = [0u32; 1024];
    for px in rgba8.chunks_exact(4) {
        h[px[0] as usize] += 1;
        h[256 + px[1] as usize] += 1;
        h[512 + px[2] as usize] += 1;
        let l = (0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32) as usize;
        h[768 + l.min(255)] += 1;
    }
    h
}

fn fit(w: u32, h: u32, max_px: u32) -> (u32, u32) {
    // cap, not target: small inputs keep their size
    if w.max(h) <= max_px {
        return (w.max(1), h.max(1));
    }
    let s = max_px as f32 / w.max(h) as f32;
    (((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1))
}

fn box3(chan: &[f32], w: usize, h: usize, x: usize, y: usize) -> f32 {
    let mut s = 0.0f32;
    let mut n = 0u32;
    for dy in -1i32..=1 {
        for dx in -1i32..=1 {
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                s += chan[ny as usize * w + nx as usize];
                n += 1;
            }
        }
    }
    s / n as f32
}

/// simple chroma smoothing (colour NR): blur R-B residuals only.
fn chroma_smooth(lin: &[[f32; 3]], w: usize, h: usize, amount: f32) -> Vec<[f32; 3]> {
    let luma: Vec<f32> = lin
        .iter()
        .map(|c| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2])
        .collect();
    let mut cr = vec![0.0f32; lin.len()];
    let mut cb = vec![0.0f32; lin.len()];
    for (i, c) in lin.iter().enumerate() {
        cr[i] = c[0] - luma[i];
        cb[i] = c[2] - luma[i];
    }
    let mut out = Vec::with_capacity(lin.len());
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let br = box3(&cr, w, h, x, y);
            let bb = box3(&cb, w, h, x, y);
            let nr = cr[i] + (br - cr[i]) * amount;
            let nb = cb[i] + (bb - cb[i]) * amount;
            let l = luma[i];
            out.push([
                l + nr,
                l - 0.2126 / 0.7152 * nr - 0.0722 / 0.7152 * nb, // keep luma
                l + nb,
            ]);
        }
    }
    out
}

/// unsharp mask (3x3) + clarity (5x5 midtone-weighted local contrast).
fn sharpen_clarity(
    lin: &[[f32; 3]],
    w: usize,
    h: usize,
    sharpen: f32,
    clarity: f32,
) -> Vec<[f32; 3]> {
    let chans: Vec<Vec<f32>> = (0..3)
        .map(|c| lin.iter().map(|p| p[c]).collect())
        .collect();
    let boxn = |chan: &[f32], x: usize, y: usize, rad: i32| {
        let mut s = 0.0f32;
        let mut n = 0u32;
        for dy in -rad..=rad {
            for dx in -rad..=rad {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                    s += chan[ny as usize * w + nx as usize];
                    n += 1;
                }
            }
        }
        s / n as f32
    };
    let mut out = vec![[0.0f32; 3]; lin.len()];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let lum = (0.2126 * lin[i][0] + 0.7152 * lin[i][1] + 0.0722 * lin[i][2])
                .clamp(0.0, 1.0);
            let mid = 4.0 * lum * (1.0 - lum);
            for c in 0..3 {
                let mut v = lin[i][c];
                if sharpen != 0.0 {
                    v += sharpen * 0.8 * (v - boxn(&chans[c], x, y, 1));
                }
                if clarity != 0.0 {
                    v += clarity * 0.6 * mid * (lin[i][c] - boxn(&chans[c], x, y, 2));
                }
                out[i][c] = v;
            }
        }
    }
    out
}
