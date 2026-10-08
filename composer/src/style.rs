//! Layer styles — Photoshop-style effects driven by a layer's alpha
//! silhouette. Every effect is optional (`Option`), carries an `enabled`
//! flag (the PS eye toggle — the params stay when off) and its own blend
//! mode. All geometry params are in placed-layer pixels and scale with
//! `layer.scale`, like Photoshop's "Scale Effects" applied to a transform.
//!
//! Render order in the compositor (matching Photoshop):
//!   beneath: stroke(outside) → outerGlow → dropShadow → bevel(outside)
//!   layer
//!   inside:  innerShadow → innerGlow → satin → bevel(inside)
//!            → colorOverlay → gradientOverlay → patternOverlay
//!            → stroke(inside/center)
//!
//! Styles follow the layer's *unmasked* silhouette — Photoshop's default
//! ("Layer Mask Hides Effects" off). Masks clip the layer pixels, not the
//! effect boundary.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::blend::BlendMode;

fn on() -> bool {
    true
}
fn zero() -> f32 {
    0.0
}
fn one_f() -> f32 {
    1.0
}
fn blend_multiply() -> BlendMode {
    BlendMode::Multiply
}
fn blend_screen() -> BlendMode {
    BlendMode::Screen
}

/// Photoshop-style layer effects; all absent/default = no styling.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerStyles {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop_shadow: Option<DropShadow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_shadow: Option<InnerShadow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outer_glow: Option<OuterGlow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_glow: Option<InnerGlow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bevel: Option<BevelEmboss>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub satin: Option<Satin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_overlay: Option<ColorOverlay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gradient_overlay: Option<GradientOverlay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_overlay: Option<PatternOverlay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<LayerStroke>,
}

impl LayerStyles {
    /// valid `effect` names for doc.styleSet/styleClear (camelCase keys)
    pub const EFFECTS: &'static [&'static str] = &[
        "dropShadow",
        "innerShadow",
        "outerGlow",
        "innerGlow",
        "bevel",
        "satin",
        "colorOverlay",
        "gradientOverlay",
        "patternOverlay",
        "stroke",
    ];

    /// any effect present *and* enabled (PS eye on)
    pub fn any_active(&self) -> bool {
        self.drop_shadow.as_ref().is_some_and(|e| e.enabled)
            || self.inner_shadow.as_ref().is_some_and(|e| e.enabled)
            || self.outer_glow.as_ref().is_some_and(|e| e.enabled)
            || self.inner_glow.as_ref().is_some_and(|e| e.enabled)
            || self.bevel.as_ref().is_some_and(|e| e.enabled)
            || self.satin.as_ref().is_some_and(|e| e.enabled)
            || self.color_overlay.as_ref().is_some_and(|e| e.enabled)
            || self.gradient_overlay.as_ref().is_some_and(|e| e.enabled)
            || self.pattern_overlay.as_ref().is_some_and(|e| e.enabled)
            || self.stroke.as_ref().is_some_and(|e| e.enabled)
    }

    /// PS "Scale Effects": multiply every pixel-dimension parameter by `s`.
    /// Ratios (spread/choke/depth), angles and opacities stay.
    pub fn scale(&mut self, s: f32) {
        let s = s.clamp(0.0, 64.0);
        if let Some(e) = &mut self.drop_shadow {
            e.dx *= s;
            e.dy *= s;
            e.blur *= s;
        }
        if let Some(e) = &mut self.inner_shadow {
            e.dx *= s;
            e.dy *= s;
            e.blur *= s;
        }
        if let Some(e) = &mut self.outer_glow {
            e.blur *= s;
        }
        if let Some(e) = &mut self.inner_glow {
            e.blur *= s;
        }
        if let Some(e) = &mut self.bevel {
            e.size *= s;
            e.soften *= s;
        }
        if let Some(e) = &mut self.satin {
            e.distance *= s;
            e.size *= s;
        }
        if let Some(e) = &mut self.gradient_overlay {
            e.gradient.scale *= s;
        }
        if let Some(e) = &mut self.pattern_overlay {
            e.scale *= s;
        }
        if let Some(e) = &mut self.stroke {
            e.size *= s;
            if let StrokeFill::Gradient { gradient } = &mut e.fill {
                gradient.scale *= s;
            }
        }
    }

    /// PS-style merge: overlay `params` onto the effect's current JSON (or
    /// the defaults when the effect is absent) and store the result.
    /// `{"enabled":false}` alone turns an effect off without losing params.
    pub fn set_effect(&mut self, name: &str, params: &serde_json::Value) -> anyhow::Result<()> {
        use serde::de::DeserializeOwned;
        fn merge<T: Serialize + DeserializeOwned>(
            slot: &mut Option<T>,
            params: &serde_json::Value,
        ) -> anyhow::Result<()> {
            let mut cur = slot
                .as_ref()
                .and_then(|e| serde_json::to_value(e).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            if let (Some(a), Some(b)) = (cur.as_object_mut(), params.as_object()) {
                for (k, v) in b {
                    a.insert(k.clone(), v.clone());
                }
            } else if !params.is_null() {
                cur = params.clone();
            }
            *slot = Some(serde_json::from_value(cur)?);
            Ok(())
        }
        match name {
            "dropShadow" => merge(&mut self.drop_shadow, params),
            "innerShadow" => merge(&mut self.inner_shadow, params),
            "outerGlow" => merge(&mut self.outer_glow, params),
            "innerGlow" => merge(&mut self.inner_glow, params),
            "bevel" => merge(&mut self.bevel, params),
            "satin" => merge(&mut self.satin, params),
            "colorOverlay" => merge(&mut self.color_overlay, params),
            "gradientOverlay" => merge(&mut self.gradient_overlay, params),
            "patternOverlay" => merge(&mut self.pattern_overlay, params),
            "stroke" => merge(&mut self.stroke, params),
            _ => anyhow::bail!(
                "unknown effect '{name}' — one of {}",
                Self::EFFECTS.join(", ")
            ),
        }
    }

    /// remove one effect's params entirely; `None` clears all styles
    pub fn clear_effect(&mut self, name: Option<&str>) -> anyhow::Result<()> {
        match name {
            None => {
                *self = LayerStyles::default();
                Ok(())
            }
            Some(n) => {
                let cleared = match n {
                    "dropShadow" => self.drop_shadow.take().is_some(),
                    "innerShadow" => self.inner_shadow.take().is_some(),
                    "outerGlow" => self.outer_glow.take().is_some(),
                    "innerGlow" => self.inner_glow.take().is_some(),
                    "bevel" => self.bevel.take().is_some(),
                    "satin" => self.satin.take().is_some(),
                    "colorOverlay" => self.color_overlay.take().is_some(),
                    "gradientOverlay" => self.gradient_overlay.take().is_some(),
                    "patternOverlay" => self.pattern_overlay.take().is_some(),
                    "stroke" => self.stroke.take().is_some(),
                    _ => {
                        anyhow::bail!("unknown effect '{n}' — one of {}", Self::EFFECTS.join(", "))
                    }
                };
                let _ = cleared;
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DropShadow {
    #[serde(default = "on")]
    pub enabled: bool,
    #[serde(default = "blend_multiply")]
    pub blend: BlendMode,
    /// offset in document pixels
    #[serde(default = "d8")]
    pub dx: f32,
    #[serde(default = "d8")]
    pub dy: f32,
    /// blur radius in px
    #[serde(default = "d12")]
    pub blur: f32,
    /// spread: expands the silhouette before blurring, 0..1
    #[serde(default)]
    pub spread: f32,
    /// [r,g,b,a] 0..1 sRGB; alpha = PS opacity
    #[serde(default = "shadow_color")]
    pub color: [f32; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InnerShadow {
    #[serde(default = "on")]
    pub enabled: bool,
    #[serde(default = "blend_multiply")]
    pub blend: BlendMode,
    #[serde(default = "d4")]
    pub dx: f32,
    #[serde(default = "d4")]
    pub dy: f32,
    #[serde(default = "d8")]
    pub blur: f32,
    /// choke: contracts the shadow matte before blurring, 0..1
    #[serde(default)]
    pub choke: f32,
    #[serde(default = "shadow_color")]
    pub color: [f32; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OuterGlow {
    #[serde(default = "on")]
    pub enabled: bool,
    #[serde(default = "blend_screen")]
    pub blend: BlendMode,
    /// size of the glow falloff, px
    #[serde(default = "d16")]
    pub blur: f32,
    /// expands the silhouette before blurring, 0..1
    #[serde(default)]
    pub spread: f32,
    /// [r,g,b,a] 0..1 sRGB; alpha = PS opacity (PS default: pale yellow 75%)
    #[serde(default = "glow_color")]
    pub color: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InnerGlowSource {
    /// glow creeps inward from the silhouette edge (PS default)
    Edge,
    /// glow radiates outward from the silhouette's interior
    Center,
}
impl Default for InnerGlowSource {
    fn default() -> Self {
        InnerGlowSource::Edge
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InnerGlow {
    #[serde(default = "on")]
    pub enabled: bool,
    #[serde(default = "blend_screen")]
    pub blend: BlendMode,
    #[serde(default = "d8")]
    pub blur: f32,
    /// contracts the glow matte before blurring, 0..1
    #[serde(default)]
    pub choke: f32,
    #[serde(default)]
    pub source: InnerGlowSource,
    #[serde(default = "glow_color")]
    pub color: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BevelStyle {
    /// bevelled ridge *inside* the silhouette (PS default)
    InnerBevel,
    /// bevelled ridge *outside* the silhouette, on the backdrop
    OuterBevel,
    /// both: inside highlight/shadow plus outside shading (raised button)
    Emboss,
    /// both sides with the outside shading inverted (stamped pillow)
    PillowEmboss,
    /// bevel clipped to the layer's stroke ring (needs stroke enabled)
    StrokeEmboss,
}
impl Default for BevelStyle {
    fn default() -> Self {
        BevelStyle::InnerBevel
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BevelDirection {
    Up,
    Down,
}
impl Default for BevelDirection {
    fn default() -> Self {
        BevelDirection::Up
    }
}

/// one half of a bevel: the highlight or the shadow side
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shade {
    #[serde(default = "blend_screen")]
    pub blend: BlendMode,
    /// [r,g,b,a] 0..1 sRGB; alpha = PS opacity
    #[serde(default = "one4")]
    pub color: [f32; 4],
}

fn highlight_shade() -> Shade {
    Shade {
        blend: BlendMode::Screen,
        color: [1.0, 1.0, 1.0, 0.75],
    }
}
fn shadow_shade() -> Shade {
    Shade {
        blend: BlendMode::Multiply,
        color: [0.0, 0.0, 0.0, 0.5],
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BevelEmboss {
    #[serde(default = "on")]
    pub enabled: bool,
    #[serde(default)]
    pub style: BevelStyle,
    #[serde(default)]
    pub direction: BevelDirection,
    /// ridge width, px
    #[serde(default = "d8")]
    pub size: f32,
    /// post-blur of the ridge shading, px
    #[serde(default)]
    pub soften: f32,
    /// light direction, degrees (PS default 120 — from top-left)
    #[serde(default = "d120")]
    pub angle: f32,
    /// light height, degrees 0..90 — higher = harder edge rolloff
    #[serde(default = "d30")]
    pub altitude: f32,
    /// shading strength multiplier (PS 0..1000% → here 0..10)
    #[serde(default = "one_f")]
    pub depth: f32,
    #[serde(default = "highlight_shade")]
    pub highlight: Shade,
    #[serde(default = "shadow_shade")]
    pub shadow: Shade,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Satin {
    #[serde(default = "on")]
    pub enabled: bool,
    #[serde(default = "blend_multiply")]
    pub blend: BlendMode,
    /// [r,g,b,a] 0..1 sRGB
    #[serde(default = "satin_color")]
    pub color: [f32; 4],
    /// fold direction, degrees
    #[serde(default = "d19")]
    pub angle: f32,
    /// fold offset, px
    #[serde(default = "d11")]
    pub distance: f32,
    /// fold blur, px
    #[serde(default = "d14")]
    pub size: f32,
    /// invert the folds (dark↔light placement)
    #[serde(default)]
    pub invert: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorOverlay {
    #[serde(default = "on")]
    pub enabled: bool,
    #[serde(default)]
    pub blend: BlendMode,
    /// [r,g,b,a] 0..1 sRGB; alpha = PS opacity
    #[serde(default = "one4")]
    pub color: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GradientStyle {
    Linear,
    Radial,
    /// conic sweep around the box centre
    Angle,
    /// linear, mirrored at the centre line
    Reflected,
    Diamond,
}
impl Default for GradientStyle {
    fn default() -> Self {
        GradientStyle::Linear
    }
}

/// a Photoshop-gradient: colour stops plus a mapping from pixel to t 0..1.
/// Shared by gradientOverlay, gradient stroke fill and pattern scales.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GradientSpec {
    /// [pos, r,g,b,a] sorted by pos — like Fill::LinearGradient
    #[serde(default = "bw_stops")]
    pub stops: Vec<[f32; 5]>,
    /// gradient axis direction, degrees
    #[serde(default = "d90")]
    pub angle: f32,
    /// 1 = spans the alignment box
    #[serde(default = "one_f")]
    pub scale: f32,
    #[serde(default)]
    pub style: GradientStyle,
    #[serde(default)]
    pub reverse: bool,
    /// true = map across the layer's placed bounds (PS "Align with Layer");
    /// false = map across the document
    #[serde(default = "on")]
    pub align_layer: bool,
}
impl Default for GradientSpec {
    fn default() -> Self {
        GradientSpec {
            stops: bw_stops(),
            angle: d90(),
            scale: 1.0,
            style: GradientStyle::Linear,
            reverse: false,
            align_layer: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GradientOverlay {
    #[serde(default = "on")]
    pub enabled: bool,
    #[serde(default)]
    pub blend: BlendMode,
    /// overall strength multiplier, 0..1
    #[serde(default = "one_f")]
    pub opacity: f32,
    #[serde(default)]
    pub gradient: GradientSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BuiltinPattern {
    Checker,
    Dots,
    Stripes,
    Grid,
    Crosshatch,
    Noise,
}
impl Default for BuiltinPattern {
    fn default() -> Self {
        BuiltinPattern::Checker
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PatternSpec {
    /// procedural tile — no assets needed
    Builtin {
        #[serde(default)]
        name: BuiltinPattern,
        /// tile size in px
        #[serde(default = "d16")]
        size: f32,
        /// [r,g,b,a] fore/background
        #[serde(default = "zero4")]
        fg: [f32; 4],
        #[serde(default = "one4")]
        bg: [f32; 4],
    },
    /// external image, tiled
    File { path: PathBuf },
}
impl Default for PatternSpec {
    fn default() -> Self {
        PatternSpec::Builtin {
            name: BuiltinPattern::Checker,
            size: d16(),
            fg: zero4(),
            bg: one4(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternOverlay {
    #[serde(default = "on")]
    pub enabled: bool,
    #[serde(default)]
    pub blend: BlendMode,
    #[serde(default = "one_f")]
    pub opacity: f32,
    /// pattern tile scale multiplier
    #[serde(default = "one_f")]
    pub scale: f32,
    /// true = pattern origin follows the layer's position (PS "Link with Layer")
    #[serde(default = "on")]
    pub link: bool,
    #[serde(default)]
    pub pattern: PatternSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StrokePosition {
    Outside,
    Inside,
    Center,
}
impl Default for StrokePosition {
    fn default() -> Self {
        StrokePosition::Outside
    }
}

/// what fills the stroke ring — flat colour or a gradient over the bounds
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "fill", rename_all = "camelCase")]
pub enum StrokeFill {
    Color { color: [f32; 4] },
    Gradient { gradient: GradientSpec },
}
impl Default for StrokeFill {
    fn default() -> Self {
        StrokeFill::Color {
            color: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerStroke {
    #[serde(default = "on")]
    pub enabled: bool,
    #[serde(default)]
    pub blend: BlendMode,
    /// ring width in px
    #[serde(default = "d3")]
    pub size: f32,
    #[serde(default)]
    pub position: StrokePosition,
    #[serde(default)]
    pub fill: StrokeFill,
}

fn d3() -> f32 {
    3.0
}
fn d4() -> f32 {
    4.0
}
fn d8() -> f32 {
    8.0
}
fn d11() -> f32 {
    11.0
}
fn d12() -> f32 {
    12.0
}
fn d14() -> f32 {
    14.0
}
fn d16() -> f32 {
    16.0
}
fn d19() -> f32 {
    19.0
}
fn d30() -> f32 {
    30.0
}
fn d90() -> f32 {
    90.0
}
fn d120() -> f32 {
    120.0
}
fn zero4() -> [f32; 4] {
    [0.0; 4]
}
fn one4() -> [f32; 4] {
    [1.0; 4]
}
fn shadow_color() -> [f32; 4] {
    [0.0, 0.0, 0.0, 0.5]
}
fn glow_color() -> [f32; 4] {
    [1.0, 1.0, 0.75, 0.75]
}
fn satin_color() -> [f32; 4] {
    [0.0, 0.0, 0.0, 0.4]
}
fn bw_stops() -> Vec<[f32; 5]> {
    vec![[0.0, 0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0, 1.0]]
}

/// `zero` is referenced as a serde default name candidate — keep a symbol.
const _: fn() -> f32 = zero;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse_empty_objects() {
        // {"dropShadow":{}} must deserialize — PS-parity defaults kick in
        let s: LayerStyles = serde_json::from_str(r#"{"dropShadow":{}}"#).unwrap();
        let ds = s.drop_shadow.unwrap();
        assert!(ds.enabled);
        assert_eq!(ds.blend, BlendMode::Multiply);
        assert!((ds.color[3] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn all_effects_roundtrip() {
        let s: LayerStyles = serde_json::from_str(
            r#"{"dropShadow":{"dx":5},"innerShadow":{},"outerGlow":{},
               "innerGlow":{"source":"center"},"bevel":{"style":"pillowEmboss"},
               "satin":{},"colorOverlay":{"color":[1,0,0,0.8]},
               "gradientOverlay":{"gradient":{"style":"radial"}},
               "patternOverlay":{"pattern":{"kind":"builtin","name":"dots"}},
               "stroke":{"size":4,"position":"inside","fill":{"fill":"color","color":[0,1,0,1]}}}"#,
        )
        .unwrap();
        assert!(s.any_active());
        let js = serde_json::to_value(&s).unwrap();
        for k in LayerStyles::EFFECTS {
            assert!(js.get(k).is_some(), "{k} missing after roundtrip");
        }
        let back: LayerStyles = serde_json::from_value(js).unwrap();
        assert_eq!(back.bevel.as_ref().unwrap().style, BevelStyle::PillowEmboss);
        assert_eq!(
            back.stroke.as_ref().unwrap().position,
            StrokePosition::Inside
        );
    }

    #[test]
    fn disabled_effect_is_not_active() {
        let s: LayerStyles = serde_json::from_str(r#"{"outerGlow":{"enabled":false}}"#).unwrap();
        assert!(!s.any_active());
    }

    #[test]
    fn scale_effects() {
        let mut s: LayerStyles = serde_json::from_str(
            r#"{"dropShadow":{"dx":4,"blur":10,"spread":0.5},
                "stroke":{"size":6}}"#,
        )
        .unwrap();
        s.scale(2.0);
        let ds = s.drop_shadow.as_ref().unwrap();
        assert_eq!(ds.dx, 8.0);
        assert_eq!(ds.blur, 20.0);
        assert_eq!(ds.spread, 0.5, "spread is a ratio — not scaled");
        assert_eq!(s.stroke.as_ref().unwrap().size, 12.0);
    }
}
