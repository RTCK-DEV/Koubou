use serde::{Deserialize, Serialize};

/// White balance mode for development.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WbMode {
    /// camera as-shot multipliers
    AsShot,
    /// neutralize on scene average (grey world)
    Auto,
    /// eyedropper: neutralize the point in `wb_pick`
    Pick,
    /// explicit kelvin/tint offsets relative to as-shot
    Manual,
}

impl Default for WbMode {
    fn default() -> Self {
        WbMode::AsShot
    }
}

/// Non-destructive development recipe. All ranges are normalized:
/// tone sliders -1.0..1.0, exposure in EV stops.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Recipe {
    pub exposure: f32,
    pub contrast: f32,
    /// >0 recovers/compresses highlights, <0 boosts them
    pub highlights: f32,
    /// >0 lifts shadows
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub saturation: f32,
    pub vibrance: f32,
    /// relative WB shift, -1..1 (warm/cool)
    pub temperature: f32,
    /// relative WB shift, -1..1 (green/magenta)
    pub tint: f32,
    pub wb_mode: WbMode,
    /// tone curve control points (x,y in 0..1); empty = linear
    pub curve: Vec<[f32; 2]>,
    /// unsharp mask amount 0..1
    pub sharpen: f32,
    /// luminance noise reduction 0..1
    pub noise_luma: f32,
    /// straighten angle in degrees, -10..10
    pub rotation_deg: f32,
    /// mid-tone local contrast -1..1
    pub clarity: f32,
    /// corner darkening -1..1 (positive darkens)
    pub vignette: f32,
    /// film grain 0..1
    pub grain: f32,
    // ---- grading (DaVinci-style lift/gamma/gain + split tone + look) ----
    /// per-channel shadow offset, typically -0.25..0.25
    pub lift: [f32; 3],
    /// per-channel midtone exponent multiplier, typically 0.5..2
    pub gamma: [f32; 3],
    /// per-channel highlight multiplier, typically 0.5..2
    pub gain: [f32; 3],
    /// split tone: shadow hue (0..1) + amount (0..1)
    pub shadow_hue: f32,
    pub shadow_sat: f32,
    /// split tone: highlight hue + amount
    pub highlight_hue: f32,
    pub highlight_sat: f32,
    /// cinematic preset: "none"|"teal_orange"|"film_fade"|"bleach"|"noir"|"matte"
    pub look: String,
    // ---- automatic correction ----
    /// histogram-driven exposure compensation
    pub auto_exposure: bool,
    /// histogram percentile white/black point stretch
    pub auto_contrast: bool,
    // ---- retouch ----
    /// crop fractions [left, top, right, bottom] 0..0.9 (post-rotation frame)
    pub crop: [f32; 4],
    /// spot heal marks [cx, cy, radius, _] in normalized post-flip frame coords
    pub spots: Vec<[f32; 4]>,
    /// dodge/burn radial lights [cx, cy, radius, ev] in normalized frame coords
    pub lights: Vec<[f32; 4]>,
    // ---- DaVinci color page extras ----
    /// WB eyedropper point (frame-normalized); used when wb_mode="pick"
    pub wb_pick: [f32; 2],
    /// global offset wheel, -0.25..0.25
    pub offset: [f32; 3],
    /// 3-way midtone tint: hue (0..1) + amount
    pub midtone_hue: f32,
    pub midtone_sat: f32,
    /// per-channel custom curves (same [[x,y]] format as `curve`)
    pub curve_r: Vec<[f32; 2]>,
    pub curve_g: Vec<[f32; 2]>,
    pub curve_b: Vec<[f32; 2]>,
    /// hue-domain curves, x = input hue 0..1
    pub hue_hue: Vec<[f32; 2]>,  // y = output hue 0..1
    pub hue_sat: Vec<[f32; 2]>,  // y = sat gain (1 = neutral)
    pub hue_lum: Vec<[f32; 2]>,  // y = luma gain (1 = neutral)
    pub lum_sat: Vec<[f32; 2]>,  // y = sat gain vs input luma
    pub sat_sat: Vec<[f32; 2]>,  // y = output sat (remap)
    /// HSL qualifier (secondary): soft windows in each H/S/L channel
    pub qh: [f32; 3],            // hue [center, half_width, soft] 0..1 cycle
    pub qs: [f32; 3],            // sat [lo, hi, soft]
    pub ql: [f32; 3],            // lum [lo, hi, soft]
    /// inside-mask adjustments [hue_shift, sat_gain, lum_gain, temp]
    pub qadj: [f32; 4],
    pub q_invert: bool,
    /// matte finesse: [clean_black, clean_white] remap of the key 0..1
    pub q_clean: [f32; 2],
    /// matte finesse: blur radius (dilates HSL soft edges) 0..1
    pub q_blur: f32,
    /// highlight mode: preview the key — qualified in colour, rest grey
    pub q_show: bool,
    /// power windows: parametric spatial masks carrying local adjustments (max 4)
    pub windows: Vec<PowerWindow>,
    // ---- HDR wheels + raw gamma controls ----
    /// zone adjustments [hue_tint(0..1), tint_amt, ev, sat] applied per luma band
    pub z_dark: [f32; 4],
    pub z_shadow: [f32; 4],
    pub z_light: [f32; 4],
    pub z_global: [f32; 4],
    /// contrast pivot point, 0..1 (DaVinci pivot / raw midpoint)
    pub pivot: f32,
    /// highlight rolloff compression, 0..2 (1 = linear)
    pub highlight_rolloff: f32,
    /// shadow rolloff, 0..2 (1 = linear)
    pub shadow_rolloff: f32,
    // ---- channel mixer / clone stamp / beauty ----
    /// RGB mixer: 3x3 row-major (identity default)
    pub mixer: [f32; 9],
    /// monochrome conversion weights [r,g,b]; all-zero = off
    pub mono: [f32; 3],
    /// clone stamps [sx, sy, dx, dy, radius, _] frame-normalized (max 8)
    pub clones: Vec<[f32; 6]>,
    /// skin smoothing amount 0..1 (edge-aware, skin-hue weighted)
    pub beauty: f32,
    // ---- restoration + light fx ----
    /// chroma noise reduction 0..1 (spatial, pairs with noise_luma)
    pub noise_chroma: f32,
    /// chromatic aberration fix -0.5..0.5 (radial R/B scale)
    pub ca_fix: f32,
    /// debanding amount 0..1 (smooths quantized gradients)
    pub deband: f32,
    /// lens glow (bloom on highlights) 0..1
    pub glow: f32,
    /// lens flare [cx, cy, strength, hue] frame-normalized
    pub flare: [f32; 4],
    // ---- darktable tone equalizer + LUT import + keystone ----
    /// per-zone exposure in EV for log2-luma zones centered at -4..+4 EV
    /// (index 0 = deepest shadows, 8 = brightest highlights), -4..4 each
    pub zones_ev: [f32; 9],
    /// WB eyedropper sample radius as a fraction of the frame, 0.002..0.2
    pub wb_pick_size: f32,
    /// path to a .cube 3D LUT file (empty = none); applied display-referred
    /// at the end of the adjust chain
    pub lut_file: String,
    /// 0..1 blend amount of the LUT
    pub lut_amount: f32,
    /// keystone correction: vertical trapezoid warp -0.4..0.4
    /// (>0 widens the bottom — fixes converging verticals shot from below)
    pub key_v: f32,
    /// keystone correction: horizontal trapezoid warp -0.4..0.4
    pub key_h: f32,
}

/// parametric spatial mask + local adjustment (DaVinci power window).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PowerWindow {
    /// "circle" (p=[cx,cy,rx,ry,rot_deg,soft]) or "gradient" (p=[x1,y1,x2,y2,soft,0])
    pub kind: String,
    pub p: [f32; 6],
    pub ev: f32,             // exposure offset in EV, -4..4
    pub sat: f32,            // saturation offset -1..1
    pub temp: f32,           // warm(+)/cool(-) -1..1
    pub invert: bool,
    /// window on/off (DaVinci: per-window visibility eye)
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// adjustment strength 0..1 (DaVinci window opacity)
    #[serde(default = "default_one")]
    pub opacity: f32,
}

fn default_true() -> bool {
    true
}
fn default_one() -> f32 {
    1.0
}

impl Default for PowerWindow {
    fn default() -> Self {
        PowerWindow {
            kind: "circle".into(),
            p: [0.5, 0.5, 0.15, 0.15, 0.0, 0.2],
            ev: 0.0,
            sat: 0.0,
            temp: 0.0,
            invert: false,
            enabled: true,
            opacity: 1.0,
        }
    }
}

impl Default for Recipe {
    fn default() -> Self {
        Recipe {
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            saturation: 0.0,
            vibrance: 0.0,
            temperature: 0.0,
            tint: 0.0,
            wb_mode: WbMode::AsShot,
            curve: Vec::new(),
            sharpen: 0.0,
            noise_luma: 0.0,
            rotation_deg: 0.0,
            clarity: 0.0,
            vignette: 0.0,
            grain: 0.0,
            lift: [0.0; 3],
            gamma: [1.0; 3],
            gain: [1.0; 3],
            shadow_hue: 0.55,
            shadow_sat: 0.0,
            highlight_hue: 0.08,
            highlight_sat: 0.0,
            look: String::new(),
            auto_exposure: false,
            auto_contrast: false,
            crop: [0.0; 4],
            spots: Vec::new(),
            lights: Vec::new(),
            wb_pick: [0.5, 0.5],
            offset: [0.0; 3],
            midtone_hue: 0.33,
            midtone_sat: 0.0,
            curve_r: Vec::new(),
            curve_g: Vec::new(),
            curve_b: Vec::new(),
            hue_hue: Vec::new(),
            hue_sat: Vec::new(),
            hue_lum: Vec::new(),
            lum_sat: Vec::new(),
            sat_sat: Vec::new(),
            qh: [0.0, 0.0, 0.05],
            qs: [0.0, 1.0, 0.05],
            ql: [0.0, 1.0, 0.05],
            qadj: [0.0; 4],
            q_invert: false,
            q_clean: [0.0, 1.0],
            q_blur: 0.0,
            q_show: false,
            windows: Vec::new(),
            z_dark: [0.0; 4],
            z_shadow: [0.0; 4],
            z_light: [0.0; 4],
            z_global: [0.0; 4],
            pivot: 0.18,
            highlight_rolloff: 1.0,
            shadow_rolloff: 1.0,
            mixer: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            mono: [0.0; 3],
            clones: Vec::new(),
            beauty: 0.0,
            noise_chroma: 0.0,
            ca_fix: 0.0,
            deband: 0.0,
            glow: 0.0,
            flare: [0.0; 4],
            zones_ev: [0.0; 9],
            wb_pick_size: 0.025,
            lut_file: String::new(),
            lut_amount: 1.0,
            key_v: 0.0,
            key_h: 0.0,
        }
    }
}

impl Recipe {
    pub fn from_json(s: &str) -> Option<Recipe> {
        serde_json::from_str(s).ok()
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// A named snapshot of a recipe (DaVinci "grade version" / still).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GradeVersion {
    pub name: String,
    pub recipe: Recipe,
}

impl Default for GradeVersion {
    fn default() -> Self {
        GradeVersion {
            name: String::new(),
            recipe: Recipe::default(),
        }
    }
}

/// Sidecar file contents stored next to each asset as `<stem>.koubou.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Sidecar {
    pub version: u32,
    pub rating: i32,
    pub label: String,
    pub recipe: Recipe,
    /// saved grade versions (DaVinci stills/versions)
    pub versions: Vec<GradeVersion>,
}

impl Default for Sidecar {
    fn default() -> Self {
        Sidecar {
            version: 1,
            rating: 0,
            label: String::new(),
            recipe: Recipe::default(),
            versions: Vec::new(),
        }
    }
}

pub fn sidecar_path_for(asset: &std::path::Path) -> std::path::PathBuf {
    let mut p = asset.to_path_buf();
    let stem = asset
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("asset")
        .to_string();
    p.set_file_name(format!("{stem}.koubou.json"));
    p
}
