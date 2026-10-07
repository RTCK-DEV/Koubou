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
    let mut l = Layer::fill("bg", Fill::Solid { color: [1.0, 0.0, 0.0, 1.0] });
    l.mask = Some(Mask::full(4, 4));
    let id = d.add_layer(l);
    let mut t = Layer::text("title", TextContent {
        text: "hello".into(),
        size: 20.0,
        ..Default::default()
    });
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
    d.add_layer(Layer::fill("base", Fill::Solid { color: [0.0, 0.0, 1.0, 1.0] }));
    let mut g = Layer::fill(
        "g",
        Fill::LinearGradient {
            line: [0.0, 0.0, 1.0, 0.0],
            stops: vec![
                [0.0, 1.0, 0.0, 0.0, 1.0],
                [1.0, 0.0, 1.0, 0.0, 1.0],
            ],
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
    d.add_layer(Layer::fill("base", Fill::Solid { color: [0.8, 0.8, 0.8, 1.0] }));
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
    d.add_layer(Layer::fill("base", Fill::Solid { color: [0.0, 0.0, 0.0, 1.0] }));
    let mut top = Layer::fill("top", Fill::Solid { color: [1.0, 1.0, 1.0, 1.0] });
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
    d.add_layer(Layer::fill("base", Fill::Solid { color: [0.0, 0.0, 0.0, 1.0] }));
    let mut s = Layer::shape(
        "dot",
        vec![Shape {
            // circle r=30 centered 50,50 via two arcs -> approximate with bezier circle
            d: "M 80 50 C 80 66.6 66.6 80 50 80 C 33.4 80 20 66.6 20 50 C 20 33.4 33.4 20 50 20 C 66.6 20 80 33.4 80 50 Z".into(),
            fill: Some([1.0, 0.0, 0.0, 1.0]),
            stroke: Some(Stroke { color: [0.0, 1.0, 0.0, 1.0], width: 2.0 }),
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
fn text_renders_nonempty() {
    let mut d = Document::new("t", 400, 120);
    d.add_layer(Layer::fill("base", Fill::Solid { color: [0.0, 0.0, 0.0, 1.0] }));
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
