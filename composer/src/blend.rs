//! Photoshop-compatible blend modes (PDF/W3C compositing spec).
//! Colors are sRGB-encoded f32 in 0..1; alpha is linear 0..1.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlendMode {
    Normal,
    Dissolve,
    Darken,
    Multiply,
    ColorBurn,
    LinearBurn,
    DarkerColor,
    Lighten,
    Screen,
    ColorDodge,
    LinearDodge,
    LighterColor,
    Overlay,
    SoftLight,
    HardLight,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
    Difference,
    Exclusion,
    Subtract,
    Divide,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl Default for BlendMode {
    fn default() -> Self {
        BlendMode::Normal
    }
}

impl BlendMode {
    pub const ALL: &'static [BlendMode] = &[
        BlendMode::Normal,
        BlendMode::Dissolve,
        BlendMode::Darken,
        BlendMode::Multiply,
        BlendMode::ColorBurn,
        BlendMode::LinearBurn,
        BlendMode::DarkerColor,
        BlendMode::Lighten,
        BlendMode::Screen,
        BlendMode::ColorDodge,
        BlendMode::LinearDodge,
        BlendMode::LighterColor,
        BlendMode::Overlay,
        BlendMode::SoftLight,
        BlendMode::HardLight,
        BlendMode::VividLight,
        BlendMode::LinearLight,
        BlendMode::PinLight,
        BlendMode::HardMix,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Subtract,
        BlendMode::Divide,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];

    pub fn label(self) -> &'static str {
        use BlendMode::*;
        match self {
            Normal => "Normal",
            Dissolve => "Dissolve",
            Darken => "Darken",
            Multiply => "Multiply",
            ColorBurn => "Color Burn",
            LinearBurn => "Linear Burn",
            DarkerColor => "Darker Color",
            Lighten => "Lighten",
            Screen => "Screen",
            ColorDodge => "Color Dodge",
            LinearDodge => "Linear Dodge (Add)",
            LighterColor => "Lighter Color",
            Overlay => "Overlay",
            SoftLight => "Soft Light",
            HardLight => "Hard Light",
            VividLight => "Vivid Light",
            LinearLight => "Linear Light",
            PinLight => "Pin Light",
            HardMix => "Hard Mix",
            Difference => "Difference",
            Exclusion => "Exclusion",
            Subtract => "Subtract",
            Divide => "Divide",
            Hue => "Hue",
            Saturation => "Saturation",
            Color => "Color",
            Luminosity => "Luminosity",
        }
    }

    /// parse a Photoshop/psd blend-mode key or a friendly name
    pub fn parse(s: &str) -> Option<BlendMode> {
        use BlendMode::*;
        let k = s.trim().to_ascii_lowercase().replace([' ', '-', '_'], "");
        Some(match k.as_str() {
            "normal" | "norm" => Normal,
            "dissolve" | "dslv" => Dissolve,
            "darken" | "dark" => Darken,
            "multiply" | "mul" => Multiply,
            "colorburn" | "cbrn" => ColorBurn,
            "linearburn" | "lbrn" => LinearBurn,
            "darkercolor" | "dkcl" => DarkerColor,
            "lighten" | "lite" => Lighten,
            "screen" | "scrn" => Screen,
            "colordodge" | "div" => ColorDodge,
            "lineardodge" | "lddg" | "add" => LinearDodge,
            "lightercolor" | "ltcl" => LighterColor,
            "overlay" | "over" => Overlay,
            "softlight" | "slit" => SoftLight,
            "hardlight" | "hlit" => HardLight,
            "vividlight" | "vlit" => VividLight,
            "linearlight" | "llit" => LinearLight,
            "pinlight" | "plit" => PinLight,
            "hardmix" | "hmix" => HardMix,
            "difference" | "diff" => Difference,
            "exclusion" | "xclu" | "smud" => Exclusion,
            "subtract" | "fsub" => Subtract,
            "divide" | "fdiv" => Divide,
            "hue" | "hhue" => Hue,
            "saturation" | "lsat" => Saturation,
            "color" | "lclr" => Color,
            "luminosity" | "llum" => Luminosity,
            _ => return None,
        })
    }

    /// per-pixel blend of source rgb over backdrop rgb (both 0..1).
    /// Returns blended rgb *before* alpha compositing.
    pub fn blend(self, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
        use BlendMode::*;
        match self {
            Normal => cs,
            Dissolve => cs, // stochastic dissolve handled by caller as alpha jitter
            Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
            Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
            Color => set_lum(cs, lum(cb)),
            Luminosity => set_lum(cb, lum(cs)),
            DarkerColor => {
                if lum(cs) <= lum(cb) {
                    cs
                } else {
                    cb
                }
            }
            LighterColor => {
                if lum(cs) > lum(cb) {
                    cs
                } else {
                    cb
                }
            }
            _ => {
                let f = separable(self);
                [f(cb[0], cs[0]), f(cb[1], cs[1]), f(cb[2], cs[2])]
            }
        }
    }
}

type CFn = fn(f32, f32) -> f32;

fn separable(m: BlendMode) -> CFn {
    use BlendMode::*;
    match m {
        Darken => |cb, cs| cb.min(cs),
        Multiply => |cb, cs| cb * cs,
        ColorBurn => |cb, cs| {
            if cs <= 0.0 {
                0.0
            } else {
                1.0 - (1.0 - cb) / cs.min(1.0)
            }
            .max(0.0)
            .min(1.0)
        },
        LinearBurn => |cb, cs| (cb + cs - 1.0).max(0.0),
        Lighten => |cb, cs| cb.max(cs),
        Screen => |cb, cs| cb + cs - cb * cs,
        ColorDodge => |cb, cs| {
            if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        },
        LinearDodge => |cb, cs| (cb + cs).min(1.0),
        Overlay => |cb, cs| {
            if cb <= 0.5 {
                2.0 * cb * cs
            } else {
                1.0 - 2.0 * (1.0 - cb) * (1.0 - cs)
            }
        },
        SoftLight => |cb, cs| {
            if cs <= 0.5 {
                cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
            } else {
                let d = if cb <= 0.25 {
                    ((16.0 * cb - 12.0) * cb + 4.0) * cb
                } else {
                    cb.sqrt()
                };
                cb + (2.0 * cs - 1.0) * (d - cb)
            }
        },
        HardLight => |cb, cs| {
            if cs <= 0.5 {
                2.0 * cb * cs
            } else {
                1.0 - 2.0 * (1.0 - cb) * (1.0 - cs)
            }
        },
        VividLight => |cb, cs| {
            if cs <= 0.5 {
                // color burn with 2*cs
                let c = 2.0 * cs;
                if c <= 0.0 {
                    0.0
                } else {
                    (1.0 - (1.0 - cb) / c).max(0.0)
                }
            } else {
                // color dodge with 2*(cs-0.5)
                let c = 2.0 * (cs - 0.5);
                if c >= 1.0 {
                    1.0
                } else {
                    (cb / (1.0 - c)).min(1.0)
                }
            }
        },
        LinearLight => |cb, cs| (cb + 2.0 * cs - 1.0).clamp(0.0, 1.0),
        PinLight => |cb, cs| {
            if cs <= 0.5 {
                cb.min(2.0 * cs)
            } else {
                cb.max(2.0 * (cs - 0.5))
            }
        },
        HardMix => |cb, cs| {
            if separable(VividLight)(cb, cs) < 0.5 {
                0.0
            } else {
                1.0
            }
        },
        Difference => |cb, cs| (cb - cs).abs(),
        Exclusion => |cb, cs| cb + cs - 2.0 * cb * cs,
        Subtract => |cb, cs| (cb - cs).max(0.0),
        Divide => |cb, cs| {
            if cs <= 0.0 {
                1.0
            } else {
                (cb / cs).min(1.0)
            }
        },
        _ => |_cb, cs| cs,
    }
}

// ---- non-separable helpers (HSL-style, from the PDF spec) ----

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn clip(mut c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    if n < 0.0 {
        for v in c.iter_mut() {
            *v = l + (*v - l) * l / (l - n);
        }
    }
    if x > 1.0 {
        for v in c.iter_mut() {
            *v = l + (*v - l) * (1.0 - l) / (x - l);
        }
    }
    c
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip([c[0] + d, c[1] + d, c[2] + d])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|&a, &b| c[a].partial_cmp(&c[b]).unwrap_or(std::cmp::Ordering::Equal));
    let (lo, mid, hi) = (idx[0], idx[1], idx[2]);
    let mut out = [0.0f32; 3];
    if c[hi] > c[lo] {
        out[mid] = (c[mid] - c[lo]) * s / (c[hi] - c[lo]);
        out[hi] = s;
    } else {
        out[mid] = 0.0;
        out[hi] = 0.0;
    }
    out[lo] = 0.0;
    out
}

/// blend `src` over `dst` (both f32 rgba straight alpha, sRGB), in place.
pub fn blend_pixel(dst: &mut [f32; 4], src: [f32; 4], mode: BlendMode) {
    let ab = dst[3].clamp(0.0, 1.0);
    let a_s = src[3].clamp(0.0, 1.0);
    if a_s <= 0.0 {
        return;
    }
    let cb = [dst[0], dst[1], dst[2]];
    let cs = [src[0], src[1], src[2]];
    let b = mode.blend(cb, cs);
    let ao = a_s + ab * (1.0 - a_s);
    if ao <= 0.0 {
        *dst = [0.0; 4];
        return;
    }
    for ch in 0..3 {
        let co = a_s * ab * b[ch] + a_s * (1.0 - ab) * cs[ch] + ab * (1.0 - a_s) * cb[ch];
        dst[ch] = (co / ao).clamp(0.0, 1.0);
    }
    dst[3] = ao;
}
