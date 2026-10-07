//! .cube (IRIDAS / DaVinci-style) 3D LUT loading + trilinear sampling.
//! Parsed luts are cached by (path, mtime, len) so re-rendering a recipe
//! doesn't re-read the file.
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct CubeLut {
    /// cube edge size N (data is N^3 * 3, R fastest then G then B)
    pub size: usize,
    pub data: Vec<f32>,
    /// input domain scale/offset (DOMAIN_MIN/MAX), almost always 0..1
    /// (pub for the GPU upload path)
    pub dmin: [f32; 3],
    pub dscale: [f32; 3],
}

impl CubeLut {
    /// trilinear sample; inputs are display-domain (sRGB-encoded) 0..1
    pub fn sample(&self, r: f32, g: f32, b: f32) -> [f32; 3] {
        let n = self.size as f32;
        let norm = |v: f32, c: usize| {
            ((v * self.dscale[c] + self.dmin[c]).clamp(0.0, 1.0) * (n - 1.0))
        };
        let fx = norm(r, 0);
        let fy = norm(g, 1);
        let fz = norm(b, 2);
        let (x0, y0, z0) = (fx.floor() as usize, fy.floor() as usize, fz.floor() as usize);
        let (x1, y1, z1) = (
            (x0 + 1).min(self.size - 1),
            (y0 + 1).min(self.size - 1),
            (z0 + 1).min(self.size - 1),
        );
        let (tx, ty, tz) = (fx - x0 as f32, fy - y0 as f32, fz - z0 as f32);
        let at = |x: usize, y: usize, z: usize, c: usize| -> f32 {
            self.data[((z * self.size + y) * self.size + x) * 3 + c]
        };
        let mut out = [0.0f32; 3];
        for c in 0..3 {
            let v00 = at(x0, y0, z0, c) + (at(x1, y0, z0, c) - at(x0, y0, z0, c)) * tx;
            let v10 = at(x0, y1, z0, c) + (at(x1, y1, z0, c) - at(x0, y1, z0, c)) * tx;
            let v01 = at(x0, y0, z1, c) + (at(x1, y0, z1, c) - at(x0, y0, z1, c)) * tx;
            let v11 = at(x0, y1, z1, c) + (at(x1, y1, z1, c) - at(x0, y1, z1, c)) * tx;
            let v0 = v00 + (v10 - v00) * ty;
            let v1 = v01 + (v11 - v01) * ty;
            out[c] = v0 + (v1 - v0) * tz;
        }
        out
    }
}

fn parse_cube(text: &str) -> Option<CubeLut> {
    let mut size = 0usize;
    let mut dmin = [0.0f32; 3];
    let mut dmax = [1.0f32; 3];
    let mut data: Vec<f32> = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let up = line.to_ascii_uppercase();
        if up.starts_with("LUT_3D_SIZE") {
            size = line.split_whitespace().nth(1)?.parse().ok()?;
            continue;
        }
        if up.starts_with("LUT_1D_SIZE") || up.starts_with("TITLE")
            || up.starts_with("COMMENT") || up.starts_with("LUT_")
        {
            continue;
        }
        if up.starts_with("DOMAIN_MIN") {
            let v: Vec<f32> = line.split_whitespace().skip(1)
                .filter_map(|s| s.parse().ok()).collect();
            if v.len() == 3 {
                dmin = [v[0], v[1], v[2]];
            }
            continue;
        }
        if up.starts_with("DOMAIN_MAX") {
            let v: Vec<f32> = line.split_whitespace().skip(1)
                .filter_map(|s| s.parse().ok()).collect();
            if v.len() == 3 {
                dmax = [v[0], v[1], v[2]];
            }
            continue;
        }
        // data row: 3 floats
        let v: Vec<f32> = line
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        if v.len() == 3 {
            data.extend_from_slice(&v);
        }
        if size > 0 && data.len() >= size * size * size * 3 {
            break;
        }
    }
    if size < 2 || data.len() < size * size * size * 3 {
        return None;
    }
    data.truncate(size * size * size * 3);
    // floats may be >1 or <0 in exotic files; clamp into display range
    for v in &mut data {
        *v = v.clamp(0.0, 1.0);
    }
    let mut dscale = [1.0f32; 3];
    for c in 0..3 {
        let span = dmax[c] - dmin[c];
        dscale[c] = if span.abs() > 1e-6 { 1.0 / span } else { 1.0 };
    }
    Some(CubeLut {
        size,
        data,
        dmin,
        dscale,
    })
}

/// Cache: path -> ((mtime_ns, len), lut). Bounded — users own a handful of
/// LUT files at most.
static CACHE: Mutex<Option<HashMap<String, ((u128, u64), CubeLut)>>> = Mutex::new(None);

pub fn load(path: &str) -> Option<CubeLut> {
    let stamp = {
        let m = std::fs::metadata(path).ok()?;
        let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
        (t, m.len())
    };
    {
        let guard = CACHE.lock().unwrap();
        if let Some(map) = guard.as_ref() {
            if let Some((s, lut)) = map.get(path) {
                if *s == stamp {
                    return Some(lut.clone());
                }
            }
        }
    }
    let text = std::fs::read_to_string(path).ok()?;
    let lut = parse_cube(&text)?;
    let mut guard = CACHE.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() > 64 {
        map.clear();
    }
    map.insert(path.to_string(), (stamp, lut.clone()));
    Some(lut)
}
