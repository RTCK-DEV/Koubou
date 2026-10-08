use koubou_composer::blend::{blend_pixel, BlendMode};
use koubou_composer::doc::{Document, Fill, Layer, LayerKind, Mask, Shape, Stroke, TextContent};
use koubou_composer::Composer;
use koubou_core::Recipe;

fn px(v: f32) -> [f32; 3] {
    [v, v, v]
}

#[test]
fn blend_modes_match_reference() {
    // reference values from the W3C compositing spec worked examples
    let cases: &[(BlendMode, f32, f32, f32)] = &[
        (BlendMode::Multiply, 0.5, 0.5, 0.25),
        (BlendMode::Screen, 0.5, 0.5, 0.75),
        (BlendMode::Overlay, 0.25, 0.6, 0.3),
        (BlendMode::Overlay, 0.75, 0.6, 0.8),
        (BlendMode::Difference, 0.7, 0.3, 0.4),
        (BlendMode::Darken, 0.7, 0.3, 0.3),
        (BlendMode::Lighten, 0.7, 0.3, 0.7),
        (BlendMode::ColorDodge, 0.4, 0.5, 0.8),
        (BlendMode::LinearBurn, 0.4, 0.5, 0.0),
        (BlendMode::LinearDodge, 0.4, 0.5, 0.9),
        (BlendMode::Subtract, 0.7, 0.4, 0.3),
    ];
    for (m, cb, cs, want) in cases {
        let got = m.blend(px(*cb), px(*cs));
        assert!(
            (got[0] - want).abs() < 1e-3,
            "{m:?} cb={cb} cs={cs}: got {} want {want}",
            got[0]
        );
    }
}

#[test]
fn soft_light_matches_spec() {
    // cs <= 0.5 branch
    let got = BlendMode::SoftLight.blend(px(0.5), px(0.25));
    let want = 0.5 - (1.0 - 0.5) * 0.5 * 0.5;
    assert!((got[0] - want).abs() < 1e-3, "{:?}", got);
}

#[test]
fn alpha_compositing() {
    // 50% opaque red over opaque blue -> blended colour keeps dst alpha
    let mut dst = [0.0, 0.0, 1.0, 1.0];
    blend_pixel(&mut dst, [1.0, 0.0, 0.0, 0.5], BlendMode::Normal);
    assert_eq!(dst[3], 1.0);
    assert!((dst[0] - 0.5).abs() < 1e-3);
    assert!((dst[2] - 0.5).abs() < 1e-3);
}

#[test]
fn doc_json_roundtrip() {
    let mut d = Document::new("t", 64, 48);
    d.backdrop = [1.0, 1.0, 1.0, 1.0];
    let mut l = Layer::fill(
        "bg",
        Fill::Solid {
            color: [1.0, 0.0, 0.0, 1.0],
        },
    );
    l.mask = Some(Mask::full(4, 4));
    let id = d.add_layer(l);
    let mut t = Layer::text(
        "title",
        TextContent {
            text: "hello".into(),
            size: 20.0,
            ..Default::default()
        },
    );
    t.blend = BlendMode::Screen;
    d.add_layer(t);
    let s = d.to_json();
    let back = Document::from_json(&s).expect("roundtrip");
    assert_eq!(back.layers.len(), 2);
    assert_eq!(back.layers[0].id, id);
    assert!(matches!(back.layers[1].kind, LayerKind::Text { .. }));
    assert_eq!(back.layers[1].blend, BlendMode::Screen);
}

#[test]
fn composite_fill_plus_gradient() {
    let mut d = Document::new("t", 8, 8);
    d.add_layer(Layer::fill(
        "base",
        Fill::Solid {
            color: [0.0, 0.0, 1.0, 1.0],
        },
    ));
    let mut g = Layer::fill(
        "g",
        Fill::LinearGradient {
            line: [0.0, 0.0, 1.0, 0.0],
            stops: vec![[0.0, 1.0, 0.0, 0.0, 1.0], [1.0, 0.0, 1.0, 0.0, 1.0]],
        },
    );
    g.blend = BlendMode::Screen;
    d.add_layer(g);
    let mut c = Composer::new(d).unwrap();
    let img = c.render().unwrap();
    assert_eq!((img.width, img.height), (8, 8));
    // left edge: screen(blue, red) = red stays blue+red mix; right edge: screen(blue, green)
    let left = &img.data[0..4];
    assert!(left[0] > 200 && left[2] > 200, "left {left:?}");
    let ri = (7 * 4) as usize;
    let right = &img.data[ri..ri + 4];
    assert!(right[1] > 200 && right[2] > 200, "right {right:?}");
}

#[test]
fn adjustment_layer_darkens_below() {
    let mut d = Document::new("t", 8, 8);
    d.add_layer(Layer::fill(
        "base",
        Fill::Solid {
            color: [0.8, 0.8, 0.8, 1.0],
        },
    ));
    let mut r = Recipe::default();
    r.exposure = -1.0; // -1 EV
    d.add_layer(Layer::adjustment("ev-1", r));
    let mut c = Composer::new(d).unwrap();
    let img = c.render().unwrap();
    // should be ~0.4 of original after -1EV — check every pixel: a sanitize
    // bug once collapsed the adjusted output to 1×1, leaving all but the
    // first pixel untouched and still passing a single-pixel assertion.
    for (i, px) in img.data.chunks_exact(4).enumerate() {
        assert!(
            px[0] < 200 && px[0] > 60,
            "px {i} not adjusted: {px:?} (expected ~102)"
        );
    }
}

#[test]
fn mask_hides_area() {
    let mut d = Document::new("t", 4, 1);
    d.add_layer(Layer::fill(
        "base",
        Fill::Solid {
            color: [0.0, 0.0, 0.0, 1.0],
        },
    ));
    let mut top = Layer::fill(
        "top",
        Fill::Solid {
            color: [1.0, 1.0, 1.0, 1.0],
        },
    );
    let mut m = Mask::full(4, 1);
    m.data[0] = 0.0;
    m.data[1] = 0.0; // hide left half of top layer
    top.mask = Some(m);
    d.add_layer(top);
    let mut c = Composer::new(d).unwrap();
    let img = c.render().unwrap();
    assert_eq!(img.data[0], 0, "masked px should be black");
    assert_eq!(img.data[8], 255, "unmasked px should be white");
}

#[test]
fn shape_circle_renders() {
    let mut d = Document::new("t", 100, 100);
    d.add_layer(Layer::fill(
        "base",
        Fill::Solid {
            color: [0.0, 0.0, 0.0, 1.0],
        },
    ));
    let mut s = Layer::shape(
        "dot",
        vec![Shape {
            // circle r=30 centered 50,50 via two arcs -> approximate with bezier circle
            d: "M 80 50 C 80 66.6 66.6 80 50 80 C 33.4 80 20 66.6 20 50 C 20 33.4 33.4 20 50 20 C 66.6 20 80 33.4 80 50 Z".into(),
            fill: Some([1.0, 0.0, 0.0, 1.0]),
            stroke: Some(Stroke { color: [0.0, 1.0, 0.0, 1.0], width: 2.0, dash: None }),
        }],
    );
    s.gen = 1;
    d.add_layer(s);
    let mut c = Composer::new(d).unwrap();
    let img = c.render().unwrap();
    let center = &img.data[((50 * 100 + 50) * 4) as usize..][..4];
    assert!(center[0] > 200, "center should be red: {center:?}");
    let corner = &img.data[0..4];
    assert!(corner[0] < 30, "corner should be dark: {corner:?}");
}

#[test]
fn gen_paths_produce_shapes() {
    // vectorcraft share: gen names must emit a usable `d`
    let rect = koubou_composer::shape::gen_path(&serde_json::json!({
        "gen": "rect", "x": 10, "y": 10, "w": 40, "h": 30
    }));
    assert!(rect.unwrap().starts_with('M'));
    let star = koubou_composer::shape::gen_path(&serde_json::json!({
        "gen": "star", "cx": 50, "cy": 50, "r": 40, "points": 5, "innerRatio": 0.5
    }));
    assert!(star.unwrap().contains('L'));
    let bad = koubou_composer::shape::gen_path(&serde_json::json!({"gen": "nope"}));
    assert!(bad.is_none());
}

#[test]
fn dashed_stroke_renders() {
    // a dashed stroke must leave gaps — not crash, not a solid ring
    let mut d = Document::new("t", 100, 20);
    d.add_layer(Layer::fill(
        "base",
        Fill::Solid {
            color: [0.0, 0.0, 0.0, 1.0],
        },
    ));
    let s = Layer::shape(
        "dash",
        vec![Shape {
            d: "M 0 10 L 100 10".into(),
            fill: None,
            stroke: Some(Stroke {
                color: [1.0, 1.0, 1.0, 1.0],
                width: 2.0,
                dash: Some(vec![6.0, 6.0]),
            }),
        }],
    );
    d.add_layer(s);
    let mut c = Composer::new(d).unwrap();
    let img = c.render().unwrap();
    let lit = img.data.chunks(4).filter(|p| p[0] > 100).count();
    // solid stroke would paint ~all of 2000 px along the line; dashed ~half
    assert!(lit > 10 && lit < 1600, "dashed stroke lit px: {lit}");
}

#[test]
fn text_renders_nonempty() {
    let mut d = Document::new("t", 400, 120);
    d.add_layer(Layer::fill(
        "base",
        Fill::Solid {
            color: [0.0, 0.0, 0.0, 1.0],
        },
    ));
    let mut t = Layer::text(
        "title",
        TextContent {
            text: "Koubou".into(),
            size: 80.0,
            color: [1.0, 1.0, 1.0, 1.0],
            ..Default::default()
        },
    );
    t.x = 10;
    t.y = 10;
    d.add_layer(t);
    let mut c = Composer::new(d).unwrap();
    let img = c.render().unwrap();
    let lit = img.data.chunks(4).filter(|p| p[0] > 200).count();
    assert!(lit > 500, "text should have painted pixels: {lit}");
}

// ---- layer styles ----

fn styled_render(styles: serde_json::Value) -> koubou_core::develop::RgbaImage {
    let mut d = Document::new("t", 64, 48);
    // 16×16 opaque red square at (24,16) → occupies x 24..40, y 16..32
    let mut l = Layer::raster("sq", 16, 16, vec![255, 0, 0, 255].repeat(16 * 16));
    l.x = 24;
    l.y = 16;
    l.styles = serde_json::from_value(styles).expect("styles json");
    d.add_layer(l);
    Composer::new(d)
        .expect("composer")
        .render()
        .expect("render")
}

fn at(img: &koubou_core::develop::RgbaImage, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.width + x) * 4) as usize;
    [
        img.data[i],
        img.data[i + 1],
        img.data[i + 2],
        img.data[i + 3],
    ]
}

#[test]
fn style_drop_shadow_offsets_silhouette() {
    let img = styled_render(serde_json::json!({
        "dropShadow": {"dx": 8, "dy": 8, "blur": 1, "color": [0,0,0,1]}
    }));
    // shifted square covers (32..48, 24..40): (44,36) is shadow-only → dark
    let p = at(&img, 44, 36);
    assert!(p[3] > 200 && p[0] < 40, "shadow px: {p:?}");
    // the layer itself still red
    let c = at(&img, 32, 24);
    assert!(c[0] > 200 && c[1] < 40, "layer px: {c:?}");
}

#[test]
fn style_outer_glow_paints_beyond_silhouette() {
    let img = styled_render(serde_json::json!({
        "outerGlow": {"blur": 4, "blend": "normal", "color": [0,1,0,1]}
    }));
    let p = at(&img, 21, 24); // 3px left of the square
    assert!(p[3] > 30 && p[1] > 50, "glow px: {p:?}");
}

#[test]
fn style_inner_shadow_darkens_inner_edge() {
    let img = styled_render(serde_json::json!({
        "innerShadow": {"dx": 3, "dy": 3, "blur": 1, "color": [0,0,0,1]}
    }));
    // top-left interior: the offset darkness field covers it
    let p = at(&img, 25, 17);
    assert!(p[0] < 200, "inner shadow px: {p:?}");
    // bottom-right interior stays red
    let c = at(&img, 38, 30);
    assert!(c[0] > 200, "interior px: {c:?}");
}

#[test]
fn style_inner_glow_edge_lights_rim() {
    let img = styled_render(serde_json::json!({
        "innerGlow": {"source": "edge", "blur": 2, "blend": "normal",
                      "color": [0,0,1,1]}
    }));
    let edge = at(&img, 25, 24);
    let center = at(&img, 32, 24);
    assert!(
        edge[2] > center[2] + 20,
        "rim {edge:?} should be bluer than centre {center:?}"
    );
}

#[test]
fn style_bevel_light_and_shade() {
    let img = styled_render(serde_json::json!({
        "bevel": {"style": "innerBevel", "size": 2, "angle": 120,
                  "altitude": 45, "depth": 1.0,
                  "highlight": {"blend": "normal", "color": [1,1,1,1]},
                  "shadow": {"blend": "normal", "color": [0,0,0,1]}}
    }));
    // angle 120 lights from the upper-left: top edge brightens…
    let hi = at(&img, 30, 16);
    assert!(hi[1] > 100 && hi[2] > 100, "highlight px: {hi:?}");
    // …bottom-right edge darkens
    let sh = at(&img, 36, 31);
    assert!(sh[0] < 60, "shaded px: {sh:?}");
}

#[test]
fn style_color_overlay_replaces_fill() {
    let img = styled_render(serde_json::json!({
        "colorOverlay": {"color": [0,1,0,1]}
    }));
    let p = at(&img, 32, 24);
    assert!(p[1] > 200 && p[0] < 40, "overlay px: {p:?}");
}

#[test]
fn style_gradient_overlay_ramps_across() {
    let img = styled_render(serde_json::json!({
        "gradientOverlay": {"opacity": 1, "gradient": {
            "stops": [[0,0,0,0,1],[1,1,1,1,1]],
            "angle": 0, "style": "linear", "scale": 1}}
    }));
    let l = at(&img, 26, 24);
    let r = at(&img, 38, 24);
    assert!(r[0] > l[0] + 60, "left {l:?} vs right {r:?}");
}

#[test]
fn style_pattern_overlay_tiles() {
    let img = styled_render(serde_json::json!({
        "patternOverlay": {"opacity": 1, "pattern": {
            "kind": "builtin", "name": "checker", "size": 4,
            "fg": [0,0,0,1], "bg": [1,1,1,1]}}
    }));
    // checker cells alternate along x inside the square
    let a = at(&img, 26, 20);
    let b = at(&img, 30, 20);
    assert!(
        (a[0] as i32 - b[0] as i32).abs() > 100,
        "checker: {a:?} {b:?}"
    );
}

#[test]
fn style_stroke_outside_and_inside() {
    let img = styled_render(serde_json::json!({
        "stroke": {"size": 3, "position": "outside",
                   "fill": {"fill": "color", "color": [0,0,1,1]}}
    }));
    let p = at(&img, 22, 24); // 2px left of square
    assert!(p[2] > 150 && p[0] < 60, "outside stroke px: {p:?}");

    let img2 = styled_render(serde_json::json!({
        "stroke": {"size": 3, "position": "inside",
                   "fill": {"fill": "color", "color": [0,0,1,1]}}
    }));
    let p2 = at(&img2, 25, 24); // 1px inside left edge
    assert!(p2[2] > 150 && p2[0] < 60, "inside stroke px: {p2:?}");
}

#[test]
fn style_disabled_effect_renders_nothing() {
    let img = styled_render(serde_json::json!({
        "dropShadow": {"enabled": false, "dx": 8, "dy": 8, "color": [0,0,0,1]}
    }));
    let p = at(&img, 44, 36);
    assert_eq!(p[3], 0, "disabled shadow px: {p:?}");
}
