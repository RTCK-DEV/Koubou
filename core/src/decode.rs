//! RAW/raster decode: LibRaw CFA extraction + fallback raster loading.
use std::ffi::CString;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::ffi;

pub const RAW_EXTS: &[&str] = &[
    "arw", "cr2", "cr3", "crw", "nef", "nrw", "raf", "orf", "ori", "rw2", "dng",
    "pef", "srw", "x3f", "mrw", "erf", "raw", "rwl", "dcr", "kdc", "mos", "3fr",
    "fff", "iiq", "r3d", "gpr", "ari", "srf", "sr2",
];
pub const RASTER_EXTS: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff", "webp"];

pub fn is_raw(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| RAW_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

pub fn is_raster(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| RASTER_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[derive(Debug, Clone, Default)]
pub struct CameraInfo {
    pub make: String,
    pub model: String,
    pub lens: String,
    pub iso: f32,
    pub shutter: f32,
    pub aperture: f32,
    pub focal: f32,
    pub timestamp: i64,
    /// dcraw flip code: 0 none, 3 = 180deg, 5 = 90ccw, 6 = 90cw
    pub flip: i32,
}

#[derive(Debug, Clone)]
pub struct CfaPattern {
    /// 0 = bayer-like 2x2, 1 = xtrans 6x6 (mapped to dims below)
    pub w: usize,
    pub h: usize,
    /// color index per CFA cell: 0=R 1=G 2=B 3=G2/other
    pub cells: Vec<u8>,
}

impl CfaPattern {
    pub fn color_at(&self, x: usize, y: usize) -> u8 {
        self.cells[(y % self.h) * self.w + (x % self.w)]
    }
}

#[derive(Debug)]
pub struct Mosaic {
    /// full raw sensor area incl. margins, stride = raw_w
    pub data: Vec<u16>,
    pub raw_w: usize,
    pub raw_h: usize,
    pub left: usize,
    pub top: usize,
    /// visible frame
    pub w: usize,
    pub h: usize,
    pub black: [f32; 4],
    pub maximum: u32,
    /// as-shot white balance multipliers (R G1 B G2)
    pub cam_mul: [f32; 4],
    /// XYZ->camera matrix (dcraw cam_xyz semantics), 4x3
    pub cam_xyz: [[f32; 3]; 4],
    /// libraw-computed camera->sRGB matrix, 3x4 (3x3 used)
    pub rgb_cam: [[f32; 4]; 3],
    /// libraw effective channel multipliers
    pub pre_mul: [f32; 4],
    pub cfa: CfaPattern,
    pub info: CameraInfo,
}

#[derive(Debug)]
pub enum Decoded {
    Mosaic(Mosaic),
    /// already-RGB input (jpeg/png/tiff), rgba16
    Raster {
        rgba: Vec<u16>,
        w: usize,
        h: usize,
        info: CameraInfo,
        flip: i32,
    },
}

struct RawHandle {
    ptr: *mut ffi::KouRaw,
}
impl Drop for RawHandle {
    fn drop(&mut self) {
        unsafe { ffi::kou_raw_close(self.ptr) }
    }
}

fn open_raw(path: &Path) -> Result<(RawHandle, ffi::KouRawInfo)> {
    let c = CString::new(path.to_string_lossy().as_bytes())?;
    let mut info = unsafe { std::mem::zeroed::<ffi::KouRawInfo>() };
    let ptr = unsafe { ffi::kou_raw_open(c.as_ptr(), &mut info) };
    if ptr.is_null() {
        bail!("libraw cannot open {}", path.display());
    }
    Ok((RawHandle { ptr }, info))
}

fn info_of(i: &ffi::KouRawInfo) -> CameraInfo {
    CameraInfo {
        make: ffi::cstr_field(&i.make),
        model: ffi::cstr_field(&i.model),
        lens: ffi::cstr_field(&i.lens),
        iso: i.iso,
        shutter: i.shutter,
        aperture: i.aperture,
        focal: i.focal,
        timestamp: i.timestamp as i64,
        flip: i.flip,
    }
}

/// Decode a RAW file to its CFA mosaic, or a raster file to rgba16.
pub fn decode(path: &Path) -> Result<Decoded> {
    if is_raster(path) {
        return decode_raster(path);
    }
    let (h, mut info) = open_raw(path)?;
    if info.cfa_kind == 0 {
        // not a mosaic raw (linear dng / rgb) - use libraw's own pipeline
        let rgb = process8(&h)?;
        return Ok(rgb);
    }
    let rc = unsafe { ffi::kou_raw_unpack(h.ptr) };
    if rc != 0 {
        bail!("libraw_unpack failed ({rc}) for {}", path.display());
    }
    // black level / pre_mul / rgb_cam are only correct after unpack
    unsafe { ffi::kou_raw_refresh_info(h.ptr, &mut info) };
    let mut out: *mut u16 = std::ptr::null_mut();
    let mut count: i32 = 0;
    let rc = unsafe { ffi::kou_raw_cfa(h.ptr, &mut out, &mut count) };
    if rc != 0 || out.is_null() {
        bail!("kou_raw_cfa failed ({rc})");
    }
    let data = unsafe { Vec::from_raw_parts(out, count as usize, count as usize) };

    let (w, hgt) = if info.width > 0 && info.height > 0 {
        (info.width as usize, info.height as usize)
    } else {
        (info.raw_width as usize, info.raw_height as usize)
    };
    let cfa = CfaPattern {
        w: info.cfa_w as usize,
        h: info.cfa_h as usize,
        cells: info.cfa_pattern[..(info.cfa_w * info.cfa_h) as usize]
            .iter()
            .map(|&v| v as u8)
            .collect(),
    };
    let mut black = info.black;
    estimate_black_floor(
        &data,
        info.raw_width as usize,
        info.left_margin as usize,
        info.top_margin as usize,
        w,
        hgt,
        &cfa,
        &mut black,
        info.maximum,
    );
    Ok(Decoded::Mosaic(Mosaic {
        data,
        raw_w: info.raw_width as usize,
        raw_h: info.raw_height as usize,
        left: info.left_margin as usize,
        top: info.top_margin as usize,
        w,
        h: hgt,
        black,
        maximum: info.maximum.max(1),
        cam_mul: info.cam_mul,
        cam_xyz: info.cam_xyz,
        rgb_cam: info.rgb_cam,
        pre_mul: info.pre_mul,
        cfa,
        info: info_of(&info),
    }))
}

/// When libraw reports no black level (e.g. Fuji RAF), estimate it per CFA
/// channel from a low percentile of the visible sensor data.
fn estimate_black_floor(
    data: &[u16],
    raw_w: usize,
    left: usize,
    top: usize,
    w: usize,
    h: usize,
    cfa: &CfaPattern,
    black: &mut [f32; 4],
    maximum: u32,
) {
    if black.iter().all(|&b| b > 0.0) {
        return;
    }
    let mut samples: [Vec<u16>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    // step coprime with both 2 (bayer) and 6 (x-trans) so every cell is sampled
    let step = 7usize;
    for sy in (top..top + h).step_by(step) {
        let row = sy * raw_w;
        for sx in (left..left + w).step_by(step) {
            let c = cfa.color_at(sx % cfa.w, sy % cfa.h) as usize;
            samples[c.min(3)].push(data[row + sx]);
        }
    }
    for c in 0..4 {
        if black[c] > 0.0 || samples[c].is_empty() {
            continue;
        }
        let mut v = samples[c].clone();
        v.sort_unstable();
        let p1 = v[(v.len() / 100).min(v.len() - 1)] as f32;
        let cap = maximum as f32 * 0.25;
        if p1 > 0.0 && p1 < cap {
            black[c] = p1;
        }
    }
}

/// Header-only camera info without unpacking pixels — fast enough for
/// catalog scans. Rasters get EXIF make/model/lens.
pub fn probe(path: &Path) -> Option<CameraInfo> {
    if is_raw(path) {
        let (_h, info) = open_raw(path).ok()?;
        return Some(info_of(&info));
    }
    if is_raster(path) {
        let f = std::fs::File::open(path).ok()?;
        let ex = exif::Reader::new()
            .read_from_container(&mut std::io::BufReader::new(f))
            .ok()?;
        let get = |t: exif::Tag| {
            ex.get_field(t, exif::In::PRIMARY)
                .map(|f| f.display_value().to_string())
                .unwrap_or_default()
        };
        return Some(CameraInfo {
            make: get(exif::Tag::Make),
            model: get(exif::Tag::Model),
            lens: get(exif::Tag::LensModel),
            iso: 0.0,
            shutter: 0.0,
            aperture: 0.0,
            focal: 0.0,
            timestamp: 0,
            flip: 0,
        });
    }
    None
}

/// Reference render via libraw's own dcraw pipeline (rgb8).
pub fn reference_render(path: &Path) -> Result<(Vec<u8>, usize, usize)> {
    let (h, _info) = open_raw(path)?;
    process8(&h).map(|d| match d {
        Decoded::Raster { rgba, w, h, .. } => {
            let bytes: Vec<u8> = rgba
                .chunks_exact(4)
                .flat_map(|c| [(c[0] >> 8) as u8, (c[1] >> 8) as u8, (c[2] >> 8) as u8])
                .collect();
            (bytes, w, h)
        }
        Decoded::Mosaic(_) => unreachable!(),
    })
}

fn process8(h: &RawHandle) -> Result<Decoded> {
    let mut out: *mut u8 = std::ptr::null_mut();
    let (mut w, mut hgt) = (0i32, 0i32);
    let rc = unsafe { ffi::kou_process8(h.ptr, &mut out, &mut w, &mut hgt) };
    if rc != 0 || out.is_null() {
        bail!("libraw dcraw_process failed ({rc})");
    }
    let n = (w * hgt * 3) as usize;
    let rgb = unsafe { Vec::from_raw_parts(out, n, n) };
    let mut rgba = Vec::with_capacity(n / 3 * 4);
    for c in rgb.chunks_exact(3) {
        rgba.push((c[0] as u16) << 8);
        rgba.push((c[1] as u16) << 8);
        rgba.push((c[2] as u16) << 8);
        rgba.push(u16::MAX);
    }
    Ok(Decoded::Raster {
        rgba,
        w: w as usize,
        h: hgt as usize,
        info: CameraInfo {
            make: String::new(),
            model: String::new(),
            lens: String::new(),
            iso: 0.0,
            shutter: 0.0,
            aperture: 0.0,
            focal: 0.0,
            timestamp: 0,
            flip: 0,
        },
        flip: 0,
    })
}

fn decode_raster(path: &Path) -> Result<Decoded> {
    let img = image::open(path).with_context(|| format!("open {}", path.display()))?;
    // keep 16-bit sources at full depth (to_rgba16 expands 8-bit inputs
    // identically to the old (v<<8)|v path)
    let rgba16 = img.to_rgba16();
    let (w, h) = (rgba16.width() as usize, rgba16.height() as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    rgba.extend_from_slice(rgba16.as_raw());
    Ok(Decoded::Raster {
        rgba,
        w,
        h,
        info: CameraInfo {
            make: String::new(),
            model: String::new(),
            lens: String::new(),
            iso: 0.0,
            shutter: 0.0,
            aperture: 0.0,
            focal: 0.0,
            timestamp: 0,
            flip: 0,
        },
        flip: 0,
    })
}

/// Embedded JPEG/bitmap thumbnail from the RAW container.
pub struct Thumb {
    pub w: usize,
    pub h: usize,
    /// rgba8 pixels
    pub rgba: Vec<u8>,
    /// dcraw flip code for the source frame — most cameras embed the
    /// thumbnail unrotated, so the caller applies it
    pub flip: i32,
}

pub fn embedded_thumb(path: &Path) -> Result<Option<Thumb>> {
    let (h, info) = open_raw(path)?;
    let mut out: *mut u8 = std::ptr::null_mut();
    let (mut len, mut w, mut hgt, mut fmt) = (0i32, 0i32, 0i32, 0i32);
    let rc = unsafe { ffi::kou_thumb(h.ptr, &mut out, &mut len, &mut w, &mut hgt, &mut fmt) };
    if rc != 0 || out.is_null() || len <= 0 {
        return Ok(None);
    }
    let bytes = unsafe { Vec::from_raw_parts(out, len as usize, len as usize) };
    let rgba = match fmt {
        1 => {
            // jpeg
            let img = image::load_from_memory(&bytes).context("decode embedded jpeg")?;
            img.to_rgba8().into_raw()
        }
        2 => {
            // rgb8 bitmap
            let mut v = Vec::with_capacity(bytes.len() / 3 * 4);
            for c in bytes.chunks_exact(3) {
                v.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
            v
        }
        3 => {
            // rgb16 bitmap
            let mut v = Vec::with_capacity(bytes.len() / 6 * 4);
            for c in bytes.chunks_exact(6) {
                let r = u16::from_be_bytes([c[0], c[1]]);
                let g = u16::from_be_bytes([c[2], c[3]]);
                let b = u16::from_be_bytes([c[4], c[5]]);
                v.extend_from_slice(&[(r >> 8) as u8, (g >> 8) as u8, (b >> 8) as u8, 255]);
            }
            v
        }
        _ => return Ok(None),
    };
    Ok(Some(Thumb {
        w: w as usize,
        h: hgt as usize,
        rgba,
        flip: info.flip,
    }))
}
