//! Vector shapes: SVG path data rasterized via tiny-skia.
//! Coordinates are document pixels (top-left origin).

use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke as TStroke, Transform};

use crate::doc::Shape;

/// Path generators → SVG `d` strings for the commands layer.
/// gen names: rect | roundRect | ellipse | star | line
pub fn gen_path(v: &serde_json::Value) -> Option<String> {
    let f = |k: &str, d: f32| {
        v.get(k)
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(d as f64) as f32
    };
    match v.get("gen").and_then(serde_json::Value::as_str)? {
        "rect" => {
            let (x, y, w, h) = (f("x", 0.0), f("y", 0.0), f("w", 100.0), f("h", 100.0));
            Some(format!("M{x} {y} H{} V{} H{} Z", x + w, y + h, x))
        }
        "roundRect" => {
            let (x, y, w, h) = (f("x", 0.0), f("y", 0.0), f("w", 100.0), f("h", 100.0));
            let r = f("r", 10.0).min(w * 0.5).min(h * 0.5);
            const K: f32 = 0.5523;
            let c = r * K;
            Some(format!(
                "M{} {} H{} C{} {} {} {} {} {} V{} C{} {} {} {} {} {} H{} C{} {} {} {} {} {} V{} C{} {} {} {} {} {} Z",
                x + r, y, x + w - r,
                x + w - r + c, y, x + w, y + r - c, x + w, y + r,
                y + h - r,
                x + w, y + h - r + c, x + w - r + c, y + h, x + w - r, y + h,
                x + r,
                x + r - c, y + h, x, y + h - r + c, x, y + h - r,
                y + r,
                x, y + r - c, x + r - c, y, x + r, y
            ))
        }
        "ellipse" => {
            let (cx, cy, rx, ry) = (f("cx", 50.0), f("cy", 50.0), f("rx", 50.0), f("ry", 50.0));
            const K: f32 = 0.5523;
            let (kx, ky) = (rx * K, ry * K);
            Some(format!(
                "M{} {} C{} {} {} {} {} {} C{} {} {} {} {} {} C{} {} {} {} {} {} C{} {} {} {} {} {} Z",
                cx + rx, cy,
                cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry,
                cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy,
                cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry,
                cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy
            ))
        }
        "star" => {
            let (cx, cy, r) = (f("cx", 50.0), f("cy", 50.0), f("r", 50.0));
            let n = (v
                .get("points")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(5))
            .max(2) as usize;
            let inner = r * f("innerRatio", 0.382);
            let mut d = String::from("M");
            for i in 0..n * 2 {
                let rad = if i % 2 == 0 { r } else { inner };
                let a = std::f32::consts::PI * i as f32 / n as f32 - std::f32::consts::FRAC_PI_2;
                let (x, y) = (cx + rad * a.cos(), cy + rad * a.sin());
                if i > 0 {
                    d.push_str(" L");
                }
                d.push_str(&format!("{x:.2} {y:.2}"));
            }
            d.push_str(" Z");
            Some(d)
        }
        "line" => Some(format!(
            "M{} {} L{} {}",
            f("x1", 0.0),
            f("y1", 0.0),
            f("x2", 100.0),
            f("y2", 100.0)
        )),
        _ => None,
    }
}

/// minimal SVG path parser: M L H V C S Q T Z (+ lowercase), with implicit
/// repeat-L after M like the spec. Numbers may be comma/space separated.
pub fn parse_path(d: &str) -> Option<tiny_skia::Path> {
    let b = d.as_bytes();
    let mut i = 0usize;
    let mut pb = PathBuilder::new();
    let mut cmd = 0u8;
    let (mut cx, mut cy, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let (mut px_ctrl, mut py_ctrl) = (0.0f32, 0.0f32);

    fn ws(b: &[u8], i: &mut usize) {
        while *i < b.len() && (b[*i] == b' ' || b[*i] == b',' || b[*i].is_ascii_whitespace()) {
            *i += 1;
        }
    }
    fn num(b: &[u8], i: &mut usize) -> Option<f32> {
        ws(b, i);
        let s = *i;
        if *i < b.len() && (b[*i] == b'-' || b[*i] == b'+') {
            *i += 1;
        }
        while *i < b.len() && (b[*i].is_ascii_digit() || b[*i] == b'.') {
            *i += 1;
        }
        if *i < b.len() && (b[*i] == b'e' || b[*i] == b'E') {
            *i += 1;
            if *i < b.len() && (b[*i] == b'-' || b[*i] == b'+') {
                *i += 1;
            }
            while *i < b.len() && b[*i].is_ascii_digit() {
                *i += 1;
            }
        }
        if s == *i {
            return None;
        }
        std::str::from_utf8(&b[s..*i]).ok()?.parse().ok()
    }
    fn peek_num(b: &[u8], i: usize) -> bool {
        let mut j = i;
        ws(b, &mut j);
        j < b.len() && (b[j].is_ascii_digit() || b[j] == b'-' || b[j] == b'+' || b[j] == b'.')
    }

    while i < b.len() {
        ws(b, &mut i);
        if i >= b.len() {
            break;
        }
        if b[i].is_ascii_alphabetic() {
            cmd = b[i];
            i += 1;
        } else if cmd == b'M' {
            cmd = b'L'; // implicit LineTo after MoveTo
        } else if cmd == b'm' {
            cmd = b'l';
        }
        if !peek_num(b, i) && !matches!(cmd, b'Z' | b'z') {
            return None;
        }
        match cmd {
            b'M' => {
                let (x, y) = (num(b, &mut i)?, num(b, &mut i)?);
                pb.move_to(x, y);
                cx = x;
                cy = y;
                sx = x;
                sy = y;
            }
            b'm' => {
                let (x, y) = (num(b, &mut i)?, num(b, &mut i)?);
                pb.move_to(cx + x, cy + y);
                cx += x;
                cy += y;
                sx = cx;
                sy = cy;
            }
            b'L' => {
                let (x, y) = (num(b, &mut i)?, num(b, &mut i)?);
                pb.line_to(x, y);
                cx = x;
                cy = y;
            }
            b'l' => {
                let (x, y) = (num(b, &mut i)?, num(b, &mut i)?);
                pb.line_to(cx + x, cy + y);
                cx += x;
                cy += y;
            }
            b'H' => {
                cx = num(b, &mut i)?;
                pb.line_to(cx, cy);
            }
            b'h' => {
                cx += num(b, &mut i)?;
                pb.line_to(cx, cy);
            }
            b'V' => {
                cy = num(b, &mut i)?;
                pb.line_to(cx, cy);
            }
            b'v' => {
                cy += num(b, &mut i)?;
                pb.line_to(cx, cy);
            }
            b'C' => {
                let (x1, y1, x2, y2, x, y) = (
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                );
                pb.cubic_to(x1, y1, x2, y2, x, y);
                px_ctrl = x2;
                py_ctrl = y2;
                cx = x;
                cy = y;
            }
            b'c' => {
                let (x1, y1, x2, y2, x, y) = (
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                );
                pb.cubic_to(cx + x1, cy + y1, cx + x2, cy + y2, cx + x, cy + y);
                px_ctrl = cx + x2;
                py_ctrl = cy + y2;
                cx += x;
                cy += y;
            }
            b'S' => {
                let (x2, y2, x, y) = (
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                );
                pb.cubic_to(2.0 * cx - px_ctrl, 2.0 * cy - py_ctrl, x2, y2, x, y);
                px_ctrl = x2;
                py_ctrl = y2;
                cx = x;
                cy = y;
            }
            b's' => {
                let (x2, y2, x, y) = (
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                );
                pb.cubic_to(
                    2.0 * cx - px_ctrl,
                    2.0 * cy - py_ctrl,
                    cx + x2,
                    cy + y2,
                    cx + x,
                    cy + y,
                );
                px_ctrl = cx + x2;
                py_ctrl = cy + y2;
                cx += x;
                cy += y;
            }
            b'Q' => {
                let (x1, y1, x, y) = (
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                );
                pb.quad_to(x1, y1, x, y);
                px_ctrl = x1;
                py_ctrl = y1;
                cx = x;
                cy = y;
            }
            b'q' => {
                let (x1, y1, x, y) = (
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                    num(b, &mut i)?,
                );
                pb.quad_to(cx + x1, cy + y1, cx + x, cy + y);
                px_ctrl = cx + x1;
                py_ctrl = cy + y1;
                cx += x;
                cy += y;
            }
            b'T' => {
                let (x, y) = (num(b, &mut i)?, num(b, &mut i)?);
                pb.quad_to(2.0 * cx - px_ctrl, 2.0 * cy - py_ctrl, x, y);
                px_ctrl = 2.0 * cx - px_ctrl;
                py_ctrl = 2.0 * cy - py_ctrl;
                cx = x;
                cy = y;
            }
            b't' => {
                let (x, y) = (num(b, &mut i)?, num(b, &mut i)?);
                pb.quad_to(2.0 * cx - px_ctrl, 2.0 * cy - py_ctrl, cx + x, cy + y);
                px_ctrl = 2.0 * cx - px_ctrl;
                py_ctrl = 2.0 * cy - py_ctrl;
                cx += x;
                cy += y;
            }
            b'Z' | b'z' => {
                pb.close();
                cx = sx;
                cy = sy;
            }
            _ => return None,
        }
    }
    pb.finish()
}

/// rasterize a list of shapes into a doc-sized rgba8 pixmap
pub fn rasterize(shapes: &[Shape], w: u32, h: u32) -> Option<Vec<u8>> {
    let mut pm = Pixmap::new(w, h)?;
    for s in shapes {
        let Some(path) = parse_path(&s.d) else {
            continue;
        };
        if let Some(f) = s.fill {
            let mut paint = Paint::default();
            paint.set_color_rgba8(
                (f[0] * 255.0) as u8,
                (f[1] * 255.0) as u8,
                (f[2] * 255.0) as u8,
                (f[3] * 255.0) as u8,
            );
            paint.anti_alias = true;
            pm.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
        if let Some(st) = &s.stroke {
            let mut paint = Paint::default();
            paint.set_color_rgba8(
                (st.color[0] * 255.0) as u8,
                (st.color[1] * 255.0) as u8,
                (st.color[2] * 255.0) as u8,
                (st.color[3] * 255.0) as u8,
            );
            paint.anti_alias = true;
            let mut stroke = TStroke {
                width: st.width.max(0.0),
                ..TStroke::default()
            };
            if let Some(dash) = &st.dash {
                if let Some(d) = tiny_skia::StrokeDash::new(dash.clone(), 0.0) {
                    stroke.dash = Some(d);
                }
            }
            pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    }
    Some(pm.data().to_vec())
}
