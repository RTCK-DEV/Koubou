//! GPU development pipeline (wgpu/WGSL compute).
//! Mirrors the math in develop.rs: demosaic -> chroma NR -> sharpen/clarity ->
//! straighten/flip/fit-resize -> tone/color adjust -> grain/vignette -> gamma.
//! Intermediates are vec4<f32> storage buffers; the last pass writes packed
//! rgba8. The CPU path remains as fallback (KOU_DISABLE_GPU or no adapter).
use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};

use crate::decode::Mosaic;
use crate::develop::{build_params, frame_geometry, frame_to_src, pick_rect, spot_to_src, Params, RgbaImage, Stats};
use crate::recipe::Recipe;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uni {
    black: [f32; 4],
    norm: [f32; 4],
    wb: [f32; 4],
    m0: [f32; 4],
    m1: [f32; 4],
    m2: [f32; 4],
    /// exposure_mul, contrast, highlights, shadows
    a1: [f32; 4],
    /// whites, blacks, saturation, vibrance
    a2: [f32; 4],
    /// sharpen, noise_luma, clarity, vignette
    a3: [f32; 4],
    /// grain, rot_sin, rot_cos, unused
    a4: [f32; 4],
    /// raw_w, raw_h, left, top
    g0: [u32; 4],
    /// vis_w, vis_h, stride, flip_mode
    g1: [u32; 4],
    /// src_w, src_h (demosaiced), dst_w, dst_h
    g2: [u32; 4],
    /// cfa_w, cfa_h, samp_step, fw (post-flip frame width)
    g3: [u32; 4],
    /// fh (post-flip frame height), sensor visible w, sensor visible h
    g4: [u32; 4],
    /// lift, gamma, gain (grading; gamma/gain are multipliers)
    lgg0: [f32; 4],
    lgg1: [f32; 4],
    lgg2: [f32; 4],
    /// split tone: shadow rgb + sat
    st0: [f32; 4],
    /// split tone: highlight rgb + sat
    st1: [f32; 4],
    /// crop rect in frame px: left, top, w, h
    crop: [f32; 4],
    /// black_pt, white_pt (auto contrast), n_spots, n_lights
    misc: [f32; 4],
    /// heal spots in virtual-src px: x, y, radius
    heal: [[f32; 4]; 8],
    /// dodge/burn lights in dst-normalized coords: x, y, radius, ev
    lts: [[f32; 4]; 8],
    /// offset wheel rgb
    off0: [f32; 4],
    /// midtone split-tone rgb + sat
    mt0: [f32; 4],
    /// HDR zone wheels dark/shadow/light/global: [hue, amt, ev, sat]
    z0: [f32; 4],
    z1: [f32; 4],
    z2: [f32; 4],
    z3: [f32; 4],
    /// pivot, highlight_rolloff, shadow_rolloff, has_chan_luts
    pv: [f32; 4],
    /// qualifier hue [center,width,soft,0], sat [lo,hi,soft,0], lum [lo,hi,soft,0]
    qh: [f32; 4],
    qs: [f32; 4],
    ql: [f32; 4],
    /// qualifier adjustment [hue_shift, sat_gain, lum_gain, temp]
    qadj: [f32; 4],
    /// has_qual, q_invert, 0, 0
    qf: [f32; 4],
    /// RGB mixer rows
    mx0: [f32; 4],
    mx1: [f32; 4],
    mx2: [f32; 4],
    /// mono weights rgb + beauty
    mono0: [f32; 4],
    /// WB pick rect in virtual src px: x0,y0,x1,y1 (-1 = disabled)
    pick0: [f32; 4],
    /// n_wins, n_clones, has_hue_luts, deband
    misc2: [f32; 4],
    /// ca_fix, glow, noise_chroma, 0
    fx0: [f32; 4],
    /// lens flare: cx, cy, strength, hue
    flare: [f32; 4],
    /// power windows: [a,b,c,d]/[rot,soft,ev,sat]/[temp,kind(+2=invert),0,0]/pad
    wins: [[f32; 4]; 16],
    /// clone stamps: [sx,sy,r,0] / [dx,dy,0,0] per clone in virtual-src px
    clones: [[f32; 4]; 16],
    /// qualifier finesse: clean_black, clean_white, blur→soft dilation, highlight
    qf2: [f32; 4],
    /// tone equalizer zones: ze0 = EV for zones 0..3, ze1 = 4..7,
    /// ze2 = [zone 8, has_zones, key_v, key_h]
    ze0: [f32; 4],
    ze1: [f32; 4],
    ze2: [f32; 4],
}

const WGSL: &str = r#"
struct Uni {
    black: vec4<f32>,
    norm: vec4<f32>,
    wb: vec4<f32>,
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
    a1: vec4<f32>,
    a2: vec4<f32>,
    a3: vec4<f32>,
    a4: vec4<f32>,
    g0: vec4<u32>,
    g1: vec4<u32>,
    g2: vec4<u32>,
    g3: vec4<u32>,
    g4: vec4<u32>,
    lgg0: vec4<f32>,
    lgg1: vec4<f32>,
    lgg2: vec4<f32>,
    st0: vec4<f32>,
    st1: vec4<f32>,
    crop: vec4<f32>,
    misc: vec4<f32>,
    heal: array<vec4<f32>, 8>,
    lts: array<vec4<f32>, 8>,
    off0: vec4<f32>,
    mt0: vec4<f32>,
    z0: vec4<f32>,
    z1: vec4<f32>,
    z2: vec4<f32>,
    z3: vec4<f32>,
    pv: vec4<f32>,
    qh: vec4<f32>,
    qs: vec4<f32>,
    ql: vec4<f32>,
    qadj: vec4<f32>,
    qf: vec4<f32>,
    mx0: vec4<f32>,
    mx1: vec4<f32>,
    mx2: vec4<f32>,
    mono0: vec4<f32>,
    pick0: vec4<f32>,
    misc2: vec4<f32>,
    fx0: vec4<f32>,
    flare: vec4<f32>,
    wins: array<vec4<f32>, 16>,
    clones: array<vec4<f32>, 16>,
    qf2: vec4<f32>,
    ze0: vec4<f32>,
    ze1: vec4<f32>,
    ze2: vec4<f32>,
}
@group(0) @binding(0) var<uniform> u: Uni;
@group(0) @binding(1) var<storage, read> rawbuf: array<u32>;
@group(0) @binding(2) var<storage, read> cfa: array<u32>;
@group(0) @binding(3) var<storage, read_write> io_a: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> io_b: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> outb: array<u32>;
@group(0) @binding(6) var<storage, read> lut: array<f32>;
@group(0) @binding(7) var<storage, read_write> stats: array<atomic<u32>>;

fn cfa_col(sx: u32, sy: u32) -> u32 {
    return cfa[(sy % u.g3.y) * u.g3.x + (sx % u.g3.x)];
}

fn demosaic(vx: u32, vy: u32) -> vec3<f32> {
    let x0 = u.g0.z + vx * u.g1.z;
    let y0 = u.g0.w + vy * u.g1.z;
    let x_end = u.g0.z + u.g4.y;
    let y_end = u.g0.w + u.g4.z;
    var sums = vec3<f32>(0.0);
    var cnts = vec3<u32>(0u);
    var pad = 0u;
    for (var it = 0u; it < 6u; it = it + 1u) {
        sums = vec3<f32>(0.0);
        cnts = vec3<u32>(0u);
        var ys = y0;
        if (ys >= pad) { ys = ys - pad; } else { ys = 0u; }
        ys = max(ys, u.g0.w);
        var xs = x0;
        if (xs >= pad) { xs = xs - pad; } else { xs = 0u; }
        xs = max(xs, u.g0.z);
        let ye = min(y0 + u.g1.z + pad, y_end);
        let xe = min(x0 + u.g1.z + pad, x_end);
        for (var sy = ys; sy < ye; sy = sy + 1u) {
            let row = sy * u.g0.x;
            for (var sx = xs; sx < xe; sx = sx + 1u) {
                let col = cfa_col(sx, sy);
                let ci = min(col, 3u);
                let cc = select(ci, 1u, col == 3u);
                let v = (f32(rawbuf[row + sx]) - u.black[ci]) * u.norm[ci];
                sums[cc] = sums[cc] + v;
                cnts[cc] = cnts[cc] + 1u;
            }
        }
        if (cnts.x > 0u && cnts.y > 0u && cnts.z > 0u) { break; }
        pad = pad + 2u;
    }
    return sums / vec3<f32>(max(cnts, vec3<u32>(1u)));
}

fn wb_matrix(cam: vec3<f32>) -> vec3<f32> {
    let cw = clamp(cam * u.wb.xyz, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec3<f32>(
        dot(u.m0.xyz, cw),
        dot(u.m1.xyz, cw),
        dot(u.m2.xyz, cw),
    );
}

// x-dispatch is capped at 65535 workgroups; gid.y carries the overflow rows.
fn flat_index(gid: vec3<u32>, num: vec3<u32>) -> u32 {
    return gid.y * (num.x * 256u) + gid.x;
}

// sparse gray-world statistics for auto WB (sums * 1e6 as u32 + count)
@compute @workgroup_size(256)
fn stats_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) num: vec3<u32>) {
    let i = flat_index(gid, num);
    let step = u.g3.z;
    let nx = (u.g1.x + step - 1u) / step;
    let ny = (u.g1.y + step - 1u) / step;
    if (i >= nx * ny) { return; }
    let c = demosaic((i % nx) * step, (i / nx) * step);
    // u32 accumulators: scale x1000, capped at ~64K samples => sum <= ~2.6e8
    atomicAdd(&stats[0], u32(clamp(c.x, 0.0, 4.0) * 1000.0));
    atomicAdd(&stats[1], u32(clamp(c.y, 0.0, 4.0) * 1000.0));
    atomicAdd(&stats[2], u32(clamp(c.z, 0.0, 4.0) * 1000.0));
    atomicAdd(&stats[3], 1u);
    let l = clamp(luma(c), 0.0, 1.0);
    atomicAdd(&stats[4], u32(l * 1000.0));
    atomicAdd(&stats[5u + u32(l * 255.0)], 1u);
    // WB pick region accumulation (stats[261..264] = rgb sums, [264] = count)
    let px = f32((i % nx) * step);
    let py = f32((i / nx) * step);
    if (px >= u.pick0.x && px <= u.pick0.z && py >= u.pick0.y && py <= u.pick0.w) {
        atomicAdd(&stats[261u], u32(clamp(c.x, 0.0, 4.0) * 1000.0));
        atomicAdd(&stats[262u], u32(clamp(c.y, 0.0, 4.0) * 1000.0));
        atomicAdd(&stats[263u], u32(clamp(c.z, 0.0, 4.0) * 1000.0));
        atomicAdd(&stats[264u], 1u);
    }
}

@compute @workgroup_size(256)
fn demosaic_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) num: vec3<u32>) {
    let i = flat_index(gid, num);
    if (i >= u.g1.x * u.g1.y) { return; }
    let cam = demosaic(i % u.g1.x, i / u.g1.x);
    io_a[i] = vec4<f32>(wb_matrix(cam), 0.0);
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// chroma smoothing: blur R/B residuals vs luma
@compute @workgroup_size(256)
fn nr_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) num: vec3<u32>) {
    let i = flat_index(gid, num);
    let w = u.g2.x;
    let h = u.g2.y;
    if (i >= w * h) { return; }
    let x = i % w;
    let y = i / w;
    let c = io_a[i].xyz;
    let l = luma(c);
    let cr0 = c.x - l;
    let cb0 = c.z - l;
    var sr = 0.0;
    var sb = 0.0;
    var n = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let nx = i32(x) + dx;
            let ny = i32(y) + dy;
            if (nx >= 0 && ny >= 0 && nx < i32(w) && ny < i32(h)) {
                let cc = io_a[u32(ny) * w + u32(nx)].xyz;
                let ll = luma(cc);
                sr = sr + cc.x - ll;
                sb = sb + cc.z - ll;
                n = n + 1.0;
            }
        }
    }
    let amt = max(u.a3.y, u.fx0.z);
    let nr = mix(cr0, sr / n, amt);
    let nb = mix(cb0, sb / n, amt);
    io_b[i] = vec4<f32>(
        l + nr,
        l - 0.2126 / 0.7152 * nr - 0.0722 / 0.7152 * nb,
        l + nb,
        0.0,
    );
}

fn box_at(x: u32, y: u32, rad: i32) -> vec3<f32> {
    let w = u.g2.x;
    let h = u.g2.y;
    var s = vec3<f32>(0.0);
    var n = 0.0;
    for (var dy = -rad; dy <= rad; dy = dy + 1) {
        for (var dx = -rad; dx <= rad; dx = dx + 1) {
            let nx = i32(x) + dx;
            let ny = i32(y) + dy;
            if (nx >= 0 && ny >= 0 && nx < i32(w) && ny < i32(h)) {
                s = s + io_a[u32(ny) * w + u32(nx)].xyz;
                n = n + 1.0;
            }
        }
    }
    return s / n;
}

fn px_at(x: i32, y: i32) -> vec3<f32> {
    let cx = clamp(x, 0, i32(u.g2.x) - 1);
    let cy = clamp(y, 0, i32(u.g2.y) - 1);
    return io_a[u32(cy) * u.g2.x + u32(cx)].xyz;
}

// spot heal: interior pixels take the mean of a ring at 1.4x radius
@compute @workgroup_size(256)
fn heal_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) num: vec3<u32>) {
    let i = flat_index(gid, num);
    let w = u.g2.x;
    let h = u.g2.y;
    if (i >= w * h) { return; }
    var c = io_a[i].xyz;
    let n = u32(u.misc.z);
    for (var s = 0u; s < n; s = s + 1u) {
        let sp = u.heal[s];
        let d = distance(vec2<f32>(f32(i % w), f32(i / w)), sp.xy) / max(sp.z, 1.0);
        if (d < 1.0) {
            var ring = vec3<f32>(0.0);
            for (var k = 0u; k < 8u; k = k + 1u) {
                let a = f32(k) * 6.2832 / 8.0;
                ring = ring + px_at(
                    i32(sp.x + sp.z * 1.4 * cos(a) + 0.5),
                    i32(sp.y + sp.z * 1.4 * sin(a) + 0.5),
                );
            }
            ring = ring / 8.0;
            let blend = 1.0 - sstep(0.7, 1.0, d);
            c = mix(c, ring, blend);
        }
    }
    // clone stamps: copy the source patch into the destination circle
    let nc = u32(u.misc2.y);
    for (var s = 0u; s < nc; s = s + 1u) {
        let csrc = u.clones[s * 2u];
        let cdst = u.clones[s * 2u + 1u];
        let d = distance(vec2<f32>(f32(i % w), f32(i / w)), cdst.xy) / max(csrc.z, 1.0);
        if (d < 1.0) {
            let src = px_at(
                i32(f32(i % w) + csrc.x - cdst.x + 0.5),
                i32(f32(i / w) + csrc.y - cdst.y + 0.5),
            );
            let blend = 1.0 - sstep(0.7, 1.0, d);
            c = mix(c, src, blend);
        }
    }
    io_b[i] = vec4<f32>(c, 0.0);
}

fn hue_to_rgb(h: f32) -> vec3<f32> {
    let h6 = fract(h) * 6.0;
    let i = u32(h6) % 6u;
    let f = h6 - floor(h6);
    let seg_a = array<vec3<f32>, 6>(
        vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(1.0, 1.0, 0.0), vec3<f32>(0.0, 1.0, 0.0),
        vec3<f32>(0.0, 1.0, 1.0), vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(1.0, 0.0, 1.0),
    );
    let a = seg_a[i];
    let b = seg_a[(i + 1u) % 6u];
    return a + (b - a) * f;
}

fn rgb_to_hsv(x: vec3<f32>) -> vec3<f32> {
    let mx = max(x.x, max(x.y, x.z));
    let mn = min(x.x, min(x.y, x.z));
    let d = mx - mn;
    var s = 0.0;
    if (mx > 1e-6) { s = d / mx; }
    var h = 0.0;
    if (d >= 1e-6) {
        if (mx == x.x) { h = (x.y - x.z) / d / 6.0; }
        else if (mx == x.y) { h = (2.0 + (x.z - x.x) / d) / 6.0; }
        else { h = (4.0 + (x.x - x.y) / d) / 6.0; }
    }
    return vec3<f32>(h - floor(h), s, mx);
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> vec3<f32> {
    let h6 = fract(h) * 6.0;
    let i = u32(h6) % 6u;
    let f = h6 - floor(h6);
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    switch i {
        case 0u: { return vec3<f32>(v, t, p); }
        case 1u: { return vec3<f32>(q, v, p); }
        case 2u: { return vec3<f32>(p, v, t); }
        case 3u: { return vec3<f32>(p, q, v); }
        case 4u: { return vec3<f32>(t, p, v); }
        default: { return vec3<f32>(v, p, q); }
    }
}

fn hue_dist(a: f32, b: f32) -> f32 {
    let d = fract(abs(a - b));
    return min(d, 1.0 - d);
}

fn skin_mask(x: vec3<f32>) -> f32 {
    let hsv = rgb_to_hsv(x);
    let hw = 1.0 - sstep(0.02, 0.12, hue_dist(hsv.x, 0.075));
    let sw = sstep(0.05, 0.25, hsv.y);
    let lw = sstep(0.15, 0.35, hsv.z);
    return hw * sw * lw;
}

// beauty (skin-masked smoothing) + deband (flat-area smoothing)
@compute @workgroup_size(256)
fn soft_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) num: vec3<u32>) {
    let i = flat_index(gid, num);
    let w = u.g2.x;
    let h = u.g2.y;
    if (i >= w * h) { return; }
    let x = i % w;
    let y = i / w;
    let c = io_a[i].xyz;
    let m = box_at(x, y, 1);
    var wgt = 0.0;
    if (u.mono0.w > 0.0) { wgt = wgt + u.mono0.w * skin_mask(c); }
    if (u.misc2.w > 0.0) {
        let flat = 1.0 - sstep(0.004, 0.03, abs(luma(c) - luma(m)));
        wgt = wgt + u.misc2.w * flat;
    }
    wgt = min(wgt, 1.0);
    io_b[i] = vec4<f32>(mix(c, m, wgt), 0.0);
}

// lens glow: highlight-extract + 9x9 blur added back
@compute @workgroup_size(256)
fn glow_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) num: vec3<u32>) {
    let i = flat_index(gid, num);
    let w = u.g2.x;
    let h = u.g2.y;
    if (i >= w * h) { return; }
    let x = i % w;
    let y = i / w;
    var m = vec3<f32>(0.0);
    var n = 0.0;
    for (var dy = -4; dy <= 4; dy = dy + 1) {
        for (var dx = -4; dx <= 4; dx = dx + 1) {
            let nx = i32(x) + dx;
            let ny = i32(y) + dy;
            if (nx >= 0 && ny >= 0 && nx < i32(w) && ny < i32(h)) {
                let cc = io_a[u32(ny) * w + u32(nx)].xyz;
                m = m + cc * sstep(0.55, 0.9, luma(cc));
                n = n + 1.0;
            }
        }
    }
    io_b[i] = vec4<f32>(io_a[i].xyz + m * (u.fx0.y * 0.8 / n), 0.0);
}

// unsharp (3x3) + clarity (5x5 midtone-weighted local contrast)
@compute @workgroup_size(256)
fn sharpen_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) num: vec3<u32>) {
    let i = flat_index(gid, num);
    let w = u.g2.x;
    let h = u.g2.y;
    if (i >= w * h) { return; }
    let src = io_a[i].xyz;
    var out = src;
    if (u.a3.x != 0.0) {
        out = src + u.a3.x * 0.8 * (src - box_at(i % w, i / w, 1));
    }
    if (u.a3.z != 0.0) {
        let lum = clamp(luma(src), 0.0, 1.0);
        let mid = 4.0 * lum * (1.0 - lum);
        out = out + u.a3.z * 0.6 * mid * (src - box_at(i % w, i / w, 2));
    }
    io_b[i] = vec4<f32>(out, 0.0);
}

fn sstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn srgb_encode(v: f32) -> f32 {
    let c = clamp(v, 0.0, 1.0);
    if (c <= 0.0031308) {
        return c * 12.92;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

fn srgb_decode(v: f32) -> f32 {
    let c = clamp(v, 0.0, 1.0);
    if (c <= 0.04045) {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

// imported 3D LUT lives in `lut` past the curve region:
// [2304]=size (0=off) [2305]=amount [2306..9]=domain_min [2309..12]=domain_scale
// data from 2312, R fastest
fn l3at(n: u32, x: u32, y: u32, z: u32) -> vec3<f32> {
    let o = 2312u + ((z * n + y) * n + x) * 3u;
    return vec3<f32>(lut[o], lut[o + 1u], lut[o + 2u]);
}

fn lut3d_apply(x: vec3<f32>) -> vec3<f32> {
    let n = u32(lut[2304u]);
    if (n < 2u) { return x; }
    let amt = lut[2305u];
    let enc = vec3<f32>(
        srgb_encode(clamp(x.x, 0.0, 1.0)),
        srgb_encode(clamp(x.y, 0.0, 1.0)),
        srgb_encode(clamp(x.z, 0.0, 1.0)));
    let dmin = vec3<f32>(lut[2306u], lut[2307u], lut[2308u]);
    let dscl = vec3<f32>(lut[2309u], lut[2310u], lut[2311u]);
    let f = clamp((enc - dmin) * dscl, vec3<f32>(0.0), vec3<f32>(1.0)) * f32(n - 1u);
    let i0 = vec3<u32>(floor(f));
    let i1 = min(i0 + vec3<u32>(1u), vec3<u32>(n - 1u));
    let t = f - vec3<f32>(i0);
    let c000 = l3at(n, i0.x, i0.y, i0.z);
    let c100 = l3at(n, i1.x, i0.y, i0.z);
    let c010 = l3at(n, i0.x, i1.y, i0.z);
    let c110 = l3at(n, i1.x, i1.y, i0.z);
    let c001 = l3at(n, i0.x, i0.y, i1.z);
    let c101 = l3at(n, i1.x, i0.y, i1.z);
    let c011 = l3at(n, i0.x, i1.y, i1.z);
    let c111 = l3at(n, i1.x, i1.y, i1.z);
    let v0 = mix(mix(c000, c100, t.x), mix(c010, c110, t.x), t.y);
    let v1 = mix(mix(c001, c101, t.x), mix(c011, c111, t.x), t.y);
    let v = mix(v0, v1, t.z);
    let bl = mix(enc, v, vec3<f32>(amt));
    return vec3<f32>(srgb_decode(bl.x), srgb_decode(bl.y), srgb_decode(bl.z));
}

fn adjust(px: vec3<f32>) -> vec3<f32> {
    var x = px * u.a1.x;
    // auto-contrast percentile remap
    if (u.misc.y - u.misc.x < 0.999 || u.misc.x > 0.001) {
        x = (x - vec3<f32>(u.misc.x)) / max(u.misc.y - u.misc.x, 0.02);
    }
    // tone equalizer: per-zone EV (9 log2-luma zones, centers -4..+4)
    if (u.ze2.y > 0.5) {
        let zz = array<f32, 9>(
            u.ze0.x, u.ze0.y, u.ze0.z, u.ze0.w,
            u.ze1.x, u.ze1.y, u.ze1.z, u.ze1.w, u.ze2.x);
        let e = log2(max(luma(x), 1e-6));
        let t = clamp(e + 4.0, 0.0, 8.0);
        let zi = min(u32(t), 7u);
        x = x * pow(2.0, zz[zi] * (1.0 - fract(t)) + zz[zi + 1u] * fract(t));
    }
    // lift/gamma/gain
    x = u.lgg2.xyz * pow(max(x + u.lgg0.xyz, vec3<f32>(0.0)), 1.0 / u.lgg1.xyz);
    // offset wheel
    x = x + u.off0.xyz;
    if (u.a2.y > 0.0) {
        x = x + u.a2.y * 0.15 * (vec3<f32>(1.0) - x);
    } else {
        x = x * (1.0 + u.a2.y * 0.20);
    }
    x = x * (1.0 + u.a2.x * 0.20);
    x = (x - vec3<f32>(u.pv.x)) * (1.0 + u.a1.y * 0.9) + vec3<f32>(u.pv.x);
    if (u.a1.w != 0.0) {
        let w = (vec3<f32>(1.0) - vec3<f32>(
            sstep(0.0, 0.45, x.x), sstep(0.0, 0.45, x.y), sstep(0.0, 0.45, x.z),
        )) * vec3<f32>(
            sstep(0.0, 0.06, x.x), sstep(0.0, 0.06, x.y), sstep(0.0, 0.06, x.z),
        );
        x = x + u.a1.w * w * 0.30;
    }
    if (u.a1.z != 0.0) {
        let w = vec3<f32>(
            sstep(0.45, 1.2, x.x), sstep(0.45, 1.2, x.y), sstep(0.45, 1.2, x.z),
        );
        x = x - u.a1.z * w * 0.5 * x;
    }
    // highlight/shadow rolloff
    if (u.pv.y != 1.0) {
        let sw = vec3<f32>(
            sstep(0.8, 1.6, x.x), sstep(0.8, 1.6, x.y), sstep(0.8, 1.6, x.z),
        );
        if (u.pv.y > 1.0) {
            x = x / (vec3<f32>(1.0) + (u.pv.y - 1.0) * sw);
        } else {
            x = x * (vec3<f32>(1.0) + (1.0 - u.pv.y) * 0.5 * sw);
        }
    }
    if (u.pv.z != 1.0) {
        let wv = vec3<f32>(1.0) - vec3<f32>(
            sstep(0.0, 0.35, x.x), sstep(0.0, 0.35, x.y), sstep(0.0, 0.35, x.z),
        );
        x = x * (vec3<f32>(1.0) + (1.0 - u.pv.z) * 0.4 * wv);
        x = x * (vec3<f32>(1.0) - max(u.pv.z - 1.0, 0.0) * 0.3 * wv);
    }
    if (u.a2.z != 0.0 || u.a2.w != 0.0) {
        let luma2 = luma(x);
        let mx = max(x.x, max(x.y, x.z));
        let mn = min(x.x, min(x.y, x.z));
        var sat_now = 0.0;
        if (mx > 1e-5) { sat_now = (mx - mn) / mx; }
        let s = max(1.0 + u.a2.z + u.a2.w * (1.0 - sat_now), 0.0);
        x = vec3<f32>(luma2) + (x - vec3<f32>(luma2)) * s;
    }
    // split toning (shadow / midtone / highlight)
    if (u.st0.w > 0.0 || u.st1.w > 0.0 || u.mt0.w > 0.0) {
        let lum = luma(x);
        let ws = (1.0 - sstep(0.0, 0.55, lum)) * u.st0.w;
        let wh = sstep(0.45, 1.0, lum) * u.st1.w;
        let wm = max(1.0 - abs(lum - 0.5) * 2.0, 0.0) * u.mt0.w;
        x = x + ws * (u.st0.xyz - vec3<f32>(lum)) * 0.5
            + wh * (u.st1.xyz - vec3<f32>(lum)) * 0.5
            + wm * (u.mt0.xyz - vec3<f32>(lum)) * 0.5;
    }
    // HDR zone wheels
    let zones = array<vec4<f32>, 4>(u.z0, u.z1, u.z2, u.z3);
    for (var zi = 0u; zi < 4u; zi = zi + 1u) {
        let z = zones[zi];
        if (z.y == 0.0 && z.z == 0.0 && z.w == 0.0) { continue; }
        let lum = clamp(luma(x), 0.0, 1.0);
        var zw = 1.0;
        if (zi == 0u) { zw = 1.0 - sstep(0.0, 0.18, lum); }
        else if (zi == 1u) { zw = sstep(0.05, 0.25, lum) * (1.0 - sstep(0.25, 0.55, lum)); }
        else if (zi == 2u) { zw = sstep(0.45, 0.75, lum); }
        if (zw <= 0.0) { continue; }
        let zc = hue_to_rgb(z.x);
        x = x * pow(2.0, z.z * zw) + zw * z.y * (zc - vec3<f32>(lum)) * 0.4;
        if (z.w != 0.0) {
            let l2 = luma(x);
            x = vec3<f32>(l2) + (x - vec3<f32>(l2)) * (1.0 + z.w * zw);
        }
    }
    // RGB channel mixer
    if (u.qf.z > 0.5) {
        x = vec3<f32>(
            dot(u.mx0.xyz, x),
            dot(u.mx1.xyz, x),
            dot(u.mx2.xyz, x),
        );
    }
    // monochrome
    if (u.mono0.x != 0.0 || u.mono0.y != 0.0 || u.mono0.z != 0.0) {
        let g = dot(u.mono0.xyz, x);
        x = vec3<f32>(g);
    }
    x = vec3<f32>(
        lut[u32(clamp(x.x, 0.0, 1.0) * 255.0)],
        lut[u32(clamp(x.y, 0.0, 1.0) * 255.0)],
        lut[u32(clamp(x.z, 0.0, 1.0) * 255.0)],
    );
    // per-channel custom curves (lut offsets 256/512/768)
    if (u.fx0.w > 0.5) {
        x = vec3<f32>(
            lut[256u + u32(clamp(x.x, 0.0, 1.0) * 255.0)],
            lut[512u + u32(clamp(x.y, 0.0, 1.0) * 255.0)],
            lut[768u + u32(clamp(x.z, 0.0, 1.0) * 255.0)],
        );
    }
    // hue-domain curves (lut offsets 1024 hh | 1280 hs | 1536 hl | 1792 ls | 2048 ss)
    if (u.misc2.z > 0.5) {
        let hsv = rgb_to_hsv(x);
        let lum = luma(x);
        let h2 = lut[1024u + u32(clamp(hsv.x, 0.0, 1.0) * 255.0)];
        var s2 = hsv.y * lut[1280u + u32(clamp(hsv.x, 0.0, 1.0) * 255.0)]
            * lut[1792u + u32(clamp(lum, 0.0, 1.0) * 255.0)];
        s2 = lut[2048u + u32(clamp(s2, 0.0, 1.0) * 255.0)];
        let l2 = lum * lut[1536u + u32(clamp(lum, 0.0, 1.0) * 255.0)];
        var x2 = hsv_to_rgb(h2, clamp(s2, 0.0, 1.0), hsv.z);
        let l3 = luma(x2);
        if (l3 > 1e-5) { x2 = x2 * (l2 / l3); }
        x = x2;
    }
    // HSL qualifier (also runs for highlight-only preview when u.qf2.w is set)
    if (u.qf.x > 0.5 || u.qf2.w > 0.5) {
        let hsv = rgb_to_hsv(x);
        let l = luma(x);
        let qb = u.qf2.z * 0.25;
        let mh = 1.0 - sstep(u.qh.y, u.qh.y + max(u.qh.z + qb, 1e-4), hue_dist(hsv.x, u.qh.x));
        let qs2 = u.qs.z + qb;
        let ms = sstep(u.qs.x - qs2, u.qs.x + qs2, hsv.y)
            * (1.0 - sstep(u.qs.y - qs2, u.qs.y + qs2, hsv.y));
        let ql2 = u.ql.z + qb;
        let ml = sstep(u.ql.x - ql2, u.ql.x + ql2, l)
            * (1.0 - sstep(u.ql.y - ql2, u.ql.y + ql2, l));
        var mask = mh * ms * ml;
        if (u.qf2.x > 0.0 || u.qf2.y < 1.0) {
            mask = clamp((mask - u.qf2.x) / max(u.qf2.y - u.qf2.x, 1e-4), 0.0, 1.0);
        }
        if (u.qf.y > 0.5) { mask = 1.0 - mask; }
        if (u.qf2.w > 0.5) { x = vec3<f32>(l) + (x - vec3<f32>(l)) * mask; }
        if (mask > 0.001 && u.qf.x > 0.5) {
            var xq = hsv_to_rgb(hsv.x + u.qadj.x, clamp(hsv.y * (1.0 + u.qadj.y), 0.0, 1.0), hsv.z);
            let lq = luma(xq);
            if (lq > 1e-5) { xq = xq * ((l * (1.0 + u.qadj.z)) / lq); }
            xq = xq + vec3<f32>(u.qadj.w * 0.06, 0.0, -u.qadj.w * 0.06);
            x = mix(x, xq, mask);
        }
    }
    // imported .cube LUT (display-referred, last colour op)
    x = lut3d_apply(x);
    return x;
}

fn bil_ch(sx: f32, sy: f32, ch: u32) -> f32 {
    let sw = u.g2.x;
    let sh = u.g2.y;
    let x0 = i32(floor(sx));
    let y0 = i32(floor(sy));
    if (x0 < 0 || y0 < 0 || x0 + 1 >= i32(sw) || y0 + 1 >= i32(sh)) {
        if (x0 >= 0 && y0 >= 0 && x0 < i32(sw) && y0 < i32(sh)) {
            return io_a[u32(y0) * sw + u32(x0)][ch];
        }
        return 0.0;
    }
    let tx = sx - floor(sx);
    let ty = sy - floor(sy);
    let i00 = u32(y0) * sw + u32(x0);
    let a = mix(io_a[i00][ch], io_a[i00 + 1u][ch], tx);
    let b = mix(io_a[i00 + sw][ch], io_a[i00 + sw + 1u][ch], tx);
    return mix(a, b, ty);
}

fn hash(p: vec2<f32>, seed: f32) -> f32 {
    // integer hash: bit-identical between Metal and CPU f32 sin is not
    var h = u32(p.x) * 2654435761u + u32(p.y) * 2246822519u + u32(seed + 1.0) * 3266489917u;
    h = (h ^ (h >> 13u)) * 1103515245u;
    h = h ^ (h >> 16u);
    return f32(h) * (1.0 / 4294967296.0);
}

// finish: fit-resize + straighten + flip undo + adjust + grain + vignette + gamma
@compute @workgroup_size(256)
fn finish_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) num: vec3<u32>) {
    let i = flat_index(gid, num);
    let dw = u.g2.z;
    let dh = u.g2.w;
    if (i >= dw * dh) { return; }
    let dx = i % dw;
    let dy = i / dw;
    // dst -> crop rect in post-flip frame coords
    let nx = (f32(dx) + 0.5) / f32(dw);
    let ny = (f32(dy) + 0.5) / f32(dh);
    var fx = u.crop.x + nx * u.crop.z - 0.5;
    var fy = u.crop.y + ny * u.crop.w - 0.5;
    // undo straighten rotation about crop centre
    let cx = u.crop.x + u.crop.z * 0.5 - 0.5;
    let cy = u.crop.y + u.crop.w * 0.5 - 0.5;
    // keystone: trapezoid warp about the crop centre
    if (u.ze2.z != 0.0 || u.ze2.w != 0.0) {
        fx = cx + (fx - cx) * (1.0 + u.ze2.z * (ny * 2.0 - 1.0));
        fy = cy + (fy - cy) * (1.0 + u.ze2.w * (nx * 2.0 - 1.0));
    }
    let px = fx - cx;
    let py = fy - cy;
    fx = cx + px * u.a4.z + py * u.a4.y;
    fy = cy - px * u.a4.y + py * u.a4.z;
    // undo flip -> src coords (src_w x src_h)
    var sx = fx;
    var sy = fy;
    let sw = u.g2.x;
    let sh = u.g2.y;
    switch u.g1.w {
        case 3u: {
            sx = f32(sw) - 1.0 - fx;
            sy = f32(sh) - 1.0 - fy;
        }
        case 6u: {
            sx = fy;
            sy = f32(sh) - 1.0 - fx;
        }
        case 5u: {
            sx = f32(sw) - 1.0 - fy;
            sy = fx;
        }
        default: {}
    }
    // bilinear sample (black outside); ca_fix splits the R/B taps radially
    var col = vec3<f32>(0.0);
    if (u.fx0.x != 0.0) {
        let wcx = f32(sw) * 0.5;
        let wcy = f32(sh) * 0.5;
        let rn = distance(vec2<f32>(sx, sy), vec2<f32>(wcx, wcy)) / f32(max(sw, sh)) * 2.0;
        let fr = 1.0 - u.fx0.x * 0.05 * rn;
        let fb = 1.0 + u.fx0.x * 0.05 * rn;
        col = vec3<f32>(
            bil_ch(wcx + (sx - wcx) * fr, wcy + (sy - wcy) * fr, 0u),
            bil_ch(sx, sy, 1u),
            bil_ch(wcx + (sx - wcx) * fb, wcy + (sy - wcy) * fb, 2u),
        );
    } else {
        let x0 = i32(floor(sx));
        let y0 = i32(floor(sy));
        if (x0 >= 0 && y0 >= 0 && x0 + 1 < i32(sw) && y0 + 1 < i32(sh)) {
            let tx = sx - floor(sx);
            let ty = sy - floor(sy);
            let i00 = u32(y0) * sw + u32(x0);
            let c00 = io_a[i00].xyz;
            let c10 = io_a[i00 + 1u].xyz;
            let c01 = io_a[i00 + sw].xyz;
            let c11 = io_a[i00 + sw + 1u].xyz;
            col = mix(mix(c00, c10, tx), mix(c01, c11, tx), ty);
        } else if (x0 >= 0 && y0 >= 0 && x0 < i32(sw) && y0 < i32(sh)) {
            col = io_a[u32(y0) * sw + u32(x0)].xyz;
        }
    }
    var adj = adjust(col);
    // dodge/burn radial lights (dst-normalized coords)
    let nl = u32(u.misc.w);
    for (var li = 0u; li < nl; li = li + 1u) {
        let lt = u.lts[li];
        let d = distance(vec2<f32>(nx, ny), lt.xy) / max(lt.z, 1e-3);
        let f = exp(-d * d * 2.77);
        adj = adj * (1.0 + lt.w * f * 0.5);
    }
    // power windows
    let nw = u32(u.misc2.x);
    for (var wi = 0u; wi < nw; wi = wi + 1u) {
        let w0 = u.wins[wi * 4u];
        let w1 = u.wins[wi * 4u + 1u];
        let w2 = u.wins[wi * 4u + 2u];
        let kind = u32(w2.y) % 2u;
        var mask = 0.0;
        if (kind == 1u) {
            // gradient: full cover before the p1..p2 span, soft ramp across
            let dvec = w0.zw - w0.xy;
            let len2 = max(dot(dvec, dvec), 1e-6);
            let t = dot(vec2<f32>(nx, ny) - w0.xy, dvec) / len2;
            let soft = max(w1.y, 0.02);
            mask = 1.0 - sstep(0.5 - soft * 0.5, 0.5 + soft * 0.5, t);
        } else {
            // circle/ellipse
            let rot = w1.x * 0.0174533;
            let dd = vec2<f32>(nx, ny) - w0.xy;
            let rr = max(w0.zw, vec2<f32>(0.005));
            let ux = (dd.x * cos(rot) + dd.y * sin(rot)) / rr.x;
            let uy = (-dd.x * sin(rot) + dd.y * cos(rot)) / rr.y;
            let d = length(vec2<f32>(ux, uy));
            mask = 1.0 - sstep(1.0 - clamp(w1.y, 0.0, 0.95), 1.0, d);
        }
        if (w2.y >= 2.0) { mask = 1.0 - mask; }
        mask = mask * w2.z;
        if (mask > 0.001) {
            let evg = pow(2.0, w1.z * mask);
            let l = luma(adj);
            adj = adj * evg;
            adj = vec3<f32>(l) + (adj - vec3<f32>(l)) * (1.0 + w1.w * mask);
            adj = adj + vec3<f32>(w2.x * mask * 0.08, 0.0, -w2.x * mask * 0.08);
        }
    }
    // lens flare: core + horizontal streak + mirrored ghost ring
    if (u.flare.z > 0.0) {
        let dvec = vec2<f32>(nx, ny) - u.flare.xy;
        let core = exp(-dot(dvec, dvec) / 0.004);
        let streak = exp(-dvec.y * dvec.y / (0.0004 + 0.02 * u.flare.z)) * exp(-abs(dvec.x) / 0.35);
        let gd = abs(distance(vec2<f32>(nx, ny), vec2<f32>(1.0) - u.flare.xy) - 0.10);
        let ghost = exp(-gd * gd / 0.0008);
        adj = adj + hue_to_rgb(u.flare.w) * (u.flare.z * (0.5 * core + 0.7 * streak + 0.35 * ghost));
    }
    // film grain (pre-gamma, linear domain)
    if (u.a4.x > 0.0) {
        let p = vec2<f32>(f32(dx), f32(dy));
        adj = adj + vec3<f32>(
            hash(p, 0.0) - 0.5,
            hash(p, 17.0) - 0.5,
            hash(p, 43.0) - 0.5,
        ) * u.a4.x * 0.12;
    }
    // vignette (post-adjust, pre-gamma; positive darkens corners)
    if (u.a3.w != 0.0) {
        let vx = nx * 2.0 - 1.0;
        let vy = ny * 2.0 - 1.0;
        let d = length(vec2<f32>(vx, vy)) * 0.7071;
        adj = adj * (1.0 - u.a3.w * sstep(0.35, 1.05, d) * 0.9);
    }
    let enc = srgb_encode(clamp(adj.x, 0.0, 1.0)) * 255.0 + 0.5;
    let enc2 = srgb_encode(clamp(adj.y, 0.0, 1.0)) * 255.0 + 0.5;
    let enc3 = srgb_encode(clamp(adj.z, 0.0, 1.0)) * 255.0 + 0.5;
    outb[i] = min(u32(enc), 255u)
        | (min(u32(enc2), 255u) << 8u)
        | (min(u32(enc3), 255u) << 16u)
        | (255u << 24u);
}
"#;

struct Pipe {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    demosaic: Pipe,
    stats: Pipe,
    heal: Pipe,
    nr: Pipe,
    soft: Pipe,
    glow: Pipe,
    sharpen: Pipe,
    finish: Pipe,
}

fn norm_factors(m: &Mosaic) -> [f32; 4] {
    let mut n = [0.0f32; 4];
    for c in 0..4 {
        n[c] = 1.0 / (m.maximum as f32 - m.black[c]).max(1.0);
    }
    n
}


impl Gpu {
    pub fn try_new() -> Option<Gpu> {
        if std::env::var_os("KOU_DISABLE_GPU").is_some() {
            log::info!("GPU disabled via KOU_DISABLE_GPU");
            return None;
        }
        match Self::init() {
            Ok(g) => Some(g),
            Err(e) => {
                log::warn!("GPU init failed, falling back to CPU: {e:#}");
                None
            }
        }
    }

    fn init() -> Result<Gpu> {
        let inst = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let adapter = pollster::block_on(inst.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .context("no wgpu adapter")?;
        // full-res frames are large (vec4f per px); request the adapter's own
        // buffer limits rather than the 128MB default
        let mut limits = wgpu::Limits::default();
        let al = adapter.limits();
        limits.max_storage_buffer_binding_size = al.max_storage_buffer_binding_size;
        limits.max_buffer_size = al.max_buffer_size;
        limits.max_compute_invocations_per_workgroup = al.max_compute_invocations_per_workgroup;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("koubou"),
            required_limits: limits,
            ..Default::default()
        }))
        .context("wgpu device request")?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("develop"),
            source: wgpu::ShaderSource::Wgsl(WGSL.into()),
        });
        let mk = |entry: &str| -> Pipe {
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(entry),
                entries: &(0..8)
                    .map(|i| wgpu::BindGroupLayoutEntry {
                        binding: i,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: match i {
                                0 => wgpu::BufferBindingType::Uniform,
                                3 | 4 | 5 | 7 => {
                                    wgpu::BufferBindingType::Storage { read_only: false }
                                }
                                _ => wgpu::BufferBindingType::Storage { read_only: true },
                            },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    })
                    .collect::<Vec<_>>(),
            });
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(entry),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pl),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            });
            Pipe { pipeline, layout }
        };
        Ok(Gpu {
            demosaic: mk("demosaic_main"),
            stats: mk("stats_main"),
            heal: mk("heal_main"),
            nr: mk("nr_main"),
            soft: mk("soft_main"),
            glow: mk("glow_main"),
            sharpen: mk("sharpen_main"),
            finish: mk("finish_main"),
            device,
            queue,
        })
    }

    /// mosaic -> developed rgba8 (fit inside max_px, 0 = full res)
    pub fn develop(&self, m: &Mosaic, r: &Recipe, max_px: u32) -> Result<RgbaImage> {
        use wgpu::BufferUsages as U;
        let dev = &self.device;
        let period = m.cfa.h.max(1) as u32;
        let mut stride = 1u32;
        if max_px > 0 {
            let ratio = (m.w.max(m.h) as f32 / max_px as f32).max(1.0);
            stride = ((ratio / period as f32).floor() as u32).max(1) * period;
        }
        let vw = (m.w as u32 / stride).max(1);
        let vh = (m.h as u32 / stride).max(1);
        let n_px = (vw * vh) as u64;

        // uploads
        let raw32: Vec<u32> = m.data.iter().map(|&v| v as u32).collect();
        let cfa32: Vec<u32> = m.cfa.cells.iter().map(|&v| v as u32).collect();
        let mk_buf = |label: &str, bytes: &[u8], usage: wgpu::BufferUsages| {
            let b = dev.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes.len() as u64,
                usage,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(&b, 0, bytes);
            b
        };
        let storage_in = U::STORAGE | U::COPY_DST;
        let raw_b = mk_buf("raw", bytemuck::cast_slice(&raw32), storage_in);
        let cfa_b = mk_buf("cfa", bytemuck::cast_slice(&cfa32), storage_in);
        let io_a = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("io_a"),
            size: n_px * 16,
            usage: U::STORAGE | U::COPY_SRC | U::COPY_DST,
            mapped_at_creation: false,
        });
        // io_b is the scratch target for spatial passes; copied back into io_a after each.
        let io_b = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("io_b"),
            size: n_px * 16,
            usage: U::STORAGE | U::COPY_SRC,
            mapped_at_creation: false,
        });
        let (fw, fh, cl, ct, ew, eh, dw, dh) = frame_geometry(
            vw as usize,
            vh as usize,
            m.info.flip,
            r.crop,
            max_px,
        );
        let (fw, fh) = (fw as u32, fh as u32);
        let (dw, dh) = (dw as u32, dh as u32);
        let out_b = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("out"),
            size: (dw * dh) as u64 * 4,
            usage: U::STORAGE | U::COPY_SRC,
            mapped_at_creation: false,
        });
        // r,g,b,luma sums + count + 256-bin luma hist + pick rgb sums + pick cnt
        const STATS_N: u64 = 265;
        let stats_b = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stats"),
            size: STATS_N * 4,
            usage: U::STORAGE | U::COPY_DST | U::COPY_SRC,
            mapped_at_creation: false,
        });
        // master 256 + per-channel 768 + hue-curves 1280
        const LUT_N: u64 = 2304;
        // .cube 3D LUT (loaded once for sizing; build_params hits the cache)
        let lut3d = if !r.lut_file.is_empty() && r.lut_amount > 0.0 {
            crate::lut::load(&r.lut_file)
        } else {
            None
        };
        // header after the curve region: [size, amt, dmin*3, dscale*3] then data
        let lut_len = LUT_N + 8 + lut3d.as_ref().map(|c| c.data.len() as u64).unwrap_or(0);
        let lut_b = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lut"),
            size: lut_len * 4,
            usage: storage_in,
            mapped_at_creation: false,
        });
        // WB pick rect (virtual src px) for the stats pass
        let pick0 = if r.wb_mode == crate::recipe::WbMode::Pick {
            pick_rect(r.wb_pick, vw as usize, vh as usize, m.info.flip, r.wb_pick_size)
        } else {
            [-1.0; 4]
        };
        let uni_b = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uni"),
            size: std::mem::size_of::<Uni>() as u64,
            usage: U::UNIFORM | U::COPY_DST,
            mapped_at_creation: false,
        });

        let mk_uni = |p: &Params, samp_step: u32| Uni {
            black: m.black,
            norm: norm_factors(m),
            wb: [p.wb[0], p.wb[1], p.wb[2], 0.0],
            m0: [p.m[0][0], p.m[0][1], p.m[0][2], 0.0],
            m1: [p.m[1][0], p.m[1][1], p.m[1][2], 0.0],
            m2: [p.m[2][0], p.m[2][1], p.m[2][2], 0.0],
            a1: [p.exposure_mul, p.contrast, p.highlights, p.shadows],
            a2: [p.whites, p.blacks, p.saturation, p.vibrance],
            a3: [p.sharpen, p.noise_luma, p.clarity, p.vignette],
            a4: [
                p.grain,
                p.rotation_deg.to_radians().sin(),
                p.rotation_deg.to_radians().cos(),
                0.0,
            ],
            g0: [m.raw_w as u32, m.raw_h as u32, m.left as u32, m.top as u32],
            g1: [vw, vh, stride, m.info.flip as u32],
            g2: [vw, vh, dw, dh],
            g3: [m.cfa.w as u32, m.cfa.h as u32, samp_step, fw],
            g4: [fh, m.w as u32, m.h as u32, 0],
            lgg0: [p.lift[0], p.lift[1], p.lift[2], 0.0],
            lgg1: [p.gamma[0], p.gamma[1], p.gamma[2], 0.0],
            lgg2: [p.gain[0], p.gain[1], p.gain[2], 0.0],
            st0: [p.shadow_col[0], p.shadow_col[1], p.shadow_col[2], p.shadow_sat],
            st1: [p.high_col[0], p.high_col[1], p.high_col[2], p.high_sat],
            crop: [cl, ct, ew as f32, eh as f32],
            misc: [
                p.black_pt,
                p.white_pt,
                p.n_spots as f32,
                p.n_lights as f32,
            ],
            heal: {
                let mut a = [[0.0f32; 4]; 8];
                for (i, s) in p.spots.iter().enumerate() {
                    let (sx, sy, rad) = spot_to_src(*s, vw as usize, vh as usize, m.info.flip);
                    a[i] = [sx, sy, rad, 0.0];
                }
                a
            },
            lts: p.lights,
            off0: [p.offset[0], p.offset[1], p.offset[2], 0.0],
            mt0: [p.mid_col[0], p.mid_col[1], p.mid_col[2], p.mid_sat],
            z0: p.zones[0],
            z1: p.zones[1],
            z2: p.zones[2],
            z3: p.zones[3],
            pv: [
                p.pivot,
                p.hl_roll,
                p.sh_roll,
                if p.chan_luts.is_empty() { 0.0 } else { 1.0 },
            ],
            qh: [p.qh[0], p.qh[1], p.qh[2], 0.0],
            qs: [p.qs[0], p.qs[1], p.qs[2], 0.0],
            ql: [p.ql[0], p.ql[1], p.ql[2], 0.0],
            qadj: p.qadj,
            qf: [
                if p.has_qual { 1.0 } else { 0.0 },
                if p.q_invert { 1.0 } else { 0.0 },
                if p.mixer != [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] { 1.0 } else { 0.0 },
                0.0,
            ],
            mx0: [p.mixer[0], p.mixer[1], p.mixer[2], 0.0],
            mx1: [p.mixer[3], p.mixer[4], p.mixer[5], 0.0],
            mx2: [p.mixer[6], p.mixer[7], p.mixer[8], 0.0],
            mono0: [p.mono[0], p.mono[1], p.mono[2], p.beauty],
            pick0,
            misc2: [
                p.n_wins as f32,
                p.n_clones as f32,
                if p.hue_luts.is_empty() { 0.0 } else { 1.0 },
                p.deband,
            ],
            fx0: [p.ca_fix, p.glow, p.noise_chroma, if p.chan_luts.is_empty() { 0.0 } else { 1.0 }],
            flare: p.flare,
            wins: {
                let mut a = [[0.0f32; 4]; 16];
                for (i, w) in p.wins.iter().take(4).enumerate() {
                    a[i * 4] = [w[1], w[2], w[3], w[4]];
                    a[i * 4 + 1] = [w[5], w[6], w[7], w[8]];
                    a[i * 4 + 2] = [w[9], w[0], w[10], 0.0];
                }
                a
            },
            clones: {
                let mut a = [[0.0f32; 4]; 16];
                for (i, c) in p.clones.iter().take(8).enumerate() {
                    let (sx, sy) = frame_to_src(c[0], c[1], vw as usize, vh as usize, m.info.flip);
                    let (dx, dy) = frame_to_src(c[2], c[3], vw as usize, vh as usize, m.info.flip);
                    let (fw2, fh2) = match m.info.flip {
                        5 | 6 => (vh, vw),
                        _ => (vw, vh),
                    };
                    let rad = (c[4] * fw2.max(fh2) as f32).max(2.0);
                    a[i * 2] = [sx, sy, rad, 0.0];
                    a[i * 2 + 1] = [dx, dy, 0.0, 0.0];
                }
                a
            },
            qf2: [
                p.q_clean[0],
                p.q_clean[1],
                p.q_blur,
                if p.q_show { 1.0 } else { 0.0 },
            ],
            ze0: [p.zone_ev[0], p.zone_ev[1], p.zone_ev[2], p.zone_ev[3]],
            ze1: [p.zone_ev[4], p.zone_ev[5], p.zone_ev[6], p.zone_ev[7]],
            ze2: [
                p.zone_ev[8],
                if p.zone_ev.iter().any(|&v| v != 0.0) { 1.0 } else { 0.0 },
                p.key_v,
                p.key_h,
            ],
        };

        let bind = |pipe: &Pipe| {
            let entries: Vec<wgpu::BindGroupEntry> = [0, 1, 2, 3, 4, 5, 6, 7]
                .iter()
                .map(|&i| wgpu::BindGroupEntry {
                    binding: i,
                    resource: match i {
                        0 => uni_b.as_entire_binding(),
                        1 => raw_b.as_entire_binding(),
                        2 => cfa_b.as_entire_binding(),
                        3 => io_a.as_entire_binding(),
                        4 => io_b.as_entire_binding(),
                        5 => out_b.as_entire_binding(),
                        6 => lut_b.as_entire_binding(),
                        _ => stats_b.as_entire_binding(),
                    },
                })
                .collect();
            dev.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &pipe.layout,
                entries: &entries,
            })
        };
        let run = |enc: &mut wgpu::CommandEncoder, pipe: &Pipe, n: u64| {
            let bg = bind(pipe);
            let mut cp = enc.begin_compute_pass(&Default::default());
            cp.set_pipeline(&pipe.pipeline);
            cp.set_bind_group(0, &bg, &[]);
            let groups = n.div_ceil(256) as u32;
            let gx = groups.min(65535);
            cp.dispatch_workgroups(gx, groups.div_ceil(gx), 1);
        };

        // scene statistics (auto WB / auto exposure / auto contrast)
        let stats = if Stats::needs(r) {
            // keep total samples under ~64K so the u32 accumulators can't overflow
            let step = ((n_px as f64 / 65536.0).sqrt().ceil() as u32).max(2);
            self.queue.write_buffer(&stats_b, 0, &vec![0u8; STATS_N as usize * 4]);
            self.queue
                .write_buffer(&uni_b, 0, bytemuck::bytes_of(&mk_uni(&build_params(m, r, None), step)));
            let mut enc = dev.create_command_encoder(&Default::default());
            run(&mut enc, &self.stats, (vw.div_ceil(step) * vh.div_ceil(step)) as u64);
            let stg = dev.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stg_stats"),
                size: STATS_N * 4,
                usage: U::COPY_DST | U::MAP_READ,
                mapped_at_creation: false,
            });
            enc.copy_buffer_to_buffer(&stats_b, 0, &stg, 0, STATS_N * 4);
            self.queue.submit([enc.finish()]);
            let s = readback(dev, &stg, STATS_N as usize * 4)?;
            let raw: &[u32] = bytemuck::cast_slice(&s);
            let cnt = raw[3].max(1) as f64;
            let pcnt = raw[264] as f64;
            let mut st = Stats {
                means: [
                    (raw[0] as f64 / 1e3 / cnt) as f32,
                    (raw[1] as f64 / 1e3 / cnt) as f32,
                    (raw[2] as f64 / 1e3 / cnt) as f32,
                ],
                luma_mean: (raw[4] as f64 / 1e3 / cnt) as f32,
                luma_hist: [0u32; 256],
                count: raw[3] as u64,
                pick_means: if pcnt > 0.0 {
                    [
                        (raw[261] as f64 / 1e3 / pcnt) as f32,
                        (raw[262] as f64 / 1e3 / pcnt) as f32,
                        (raw[263] as f64 / 1e3 / pcnt) as f32,
                    ]
                } else {
                    [0.0; 3]
                },
                pick_count: raw[264] as u64,
            };
            st.luma_hist.copy_from_slice(&raw[5..261]);
            Some(st)
        } else {
            None
        };

        let p = build_params(m, r, stats.as_ref());
        // packed lut buffer: master | chan r,g,b | hue hh,hs,hl,ls,ss
        let mut lut_buf = vec![0.0f32; LUT_N as usize];
        lut_buf[..256].copy_from_slice(&p.lut);
        for c in 0..3 {
            let off = 256 + c * 256;
            if p.chan_luts.is_empty() {
                for i in 0..256 {
                    lut_buf[off + i] = i as f32 / 255.0;
                }
            } else {
                lut_buf[off..off + 256].copy_from_slice(&p.chan_luts[c * 256..(c + 1) * 256]);
            }
        }
        let hue_defaults: [fn(f32) -> f32; 5] = [
            |x| x,
            |_| 1.0,
            |_| 1.0,
            |_| 1.0,
            |x| x,
        ];
        for k in 0..5 {
            let off = 1024 + k * 256;
            if p.hue_luts.is_empty() {
                for i in 0..256 {
                    lut_buf[off + i] = hue_defaults[k](i as f32 / 255.0);
                }
            } else {
                lut_buf[off..off + 256].copy_from_slice(&p.hue_luts[k * 256..(k + 1) * 256]);
            }
        }
        // 3D LUT header + data (shader: size==0 => passthrough)
        lut_buf.resize(lut_len as usize, 0.0);
        if let Some(c) = &p.lut3d {
            lut_buf[LUT_N as usize] = c.size as f32;
            lut_buf[LUT_N as usize + 1] = p.lut_amt;
            lut_buf[LUT_N as usize + 2..LUT_N as usize + 5].copy_from_slice(&c.dmin);
            lut_buf[LUT_N as usize + 5..LUT_N as usize + 8].copy_from_slice(&c.dscale);
            lut_buf[LUT_N as usize + 8..LUT_N as usize + 8 + c.data.len()]
                .copy_from_slice(&c.data);
        }
        self.queue.write_buffer(&lut_b, 0, bytemuck::cast_slice(&lut_buf));
        self.queue
            .write_buffer(&uni_b, 0, bytemuck::bytes_of(&mk_uni(&p, 0)));

        let mut enc = dev.create_command_encoder(&Default::default());
        run(&mut enc, &self.demosaic, n_px);
        // Each spatial stage reads io_a and writes io_b; copy back between stages.
        if p.n_spots > 0 || p.n_clones > 0 {
            run(&mut enc, &self.heal, n_px);
            enc.copy_buffer_to_buffer(&io_b, 0, &io_a, 0, n_px * 16);
        }
        if p.noise_luma > 0.0 || p.noise_chroma > 0.0 {
            run(&mut enc, &self.nr, n_px);
            enc.copy_buffer_to_buffer(&io_b, 0, &io_a, 0, n_px * 16);
        }
        if p.beauty > 0.0 || p.deband > 0.0 {
            run(&mut enc, &self.soft, n_px);
            enc.copy_buffer_to_buffer(&io_b, 0, &io_a, 0, n_px * 16);
        }
        if p.glow > 0.0 {
            run(&mut enc, &self.glow, n_px);
            enc.copy_buffer_to_buffer(&io_b, 0, &io_a, 0, n_px * 16);
        }
        if p.sharpen > 0.0 || p.clarity != 0.0 {
            run(&mut enc, &self.sharpen, n_px);
            enc.copy_buffer_to_buffer(&io_b, 0, &io_a, 0, n_px * 16);
        }
        run(&mut enc, &self.finish, (dw * dh) as u64);
        let out_bytes = (dw * dh * 4) as u64;
        let stg = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stg_out"),
            size: out_bytes,
            usage: U::COPY_DST | U::MAP_READ,
            mapped_at_creation: false,
        });
        enc.copy_buffer_to_buffer(&out_b, 0, &stg, 0, out_bytes);
        self.queue.submit([enc.finish()]);
        let data = readback(dev, &stg, out_bytes as usize)?;
        Ok(RgbaImage {
            width: dw,
            height: dh,
            data,
        })
    }
}

fn readback(dev: &wgpu::Device, buf: &wgpu::Buffer, size: usize) -> Result<Vec<u8>> {
    let slice = buf.slice(..size as u64);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = dev.poll(wgpu::PollType::Wait);
    let data = slice.get_mapped_range().to_vec();
    buf.unmap();
    Ok(data)
}
