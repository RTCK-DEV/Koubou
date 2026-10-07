//! High-level engine API shared by the C FFI surface and the CLI.
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use serde_json::{json, Value};

use crate::auto;
use crate::catalog::{AssetEntry, Catalog};
use crate::decode::{self, Decoded};
use crate::develop::{self, RgbaImage};
use crate::gpu::Gpu;
use crate::recipe::{sidecar_path_for, Recipe, Sidecar};

struct State {
    path: Option<PathBuf>,
    /// (mtime_ns, byte_len) of the file when it was decoded — a file that
    /// changed on disk invalidates the cached decode.
    stamp: Option<(u128, u64)>,
    decoded: Option<Decoded>,
}

fn file_stamp(path: &Path) -> Option<(u128, u64)> {
    let m = std::fs::metadata(path).ok()?;
    let t = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((t, m.len()))
}

pub struct Engine {
    state: Mutex<State>,
    catalog: Catalog,
    /// dropped permanently after the first GPU failure (device may be poisoned)
    gpu: Mutex<Option<Gpu>>,
}

impl Engine {
    pub fn new() -> Result<Engine> {
        Ok(Engine {
            state: Mutex::new(State {
                path: None,
                stamp: None,
                decoded: None,
            }),
            catalog: Catalog::open()?,
            gpu: Mutex::new(Gpu::try_new()),
        })
    }

    #[cfg(test)]
    pub fn new_mem() -> Result<Engine> {
        Ok(Engine {
            state: Mutex::new(State {
                path: None,
                stamp: None,
                decoded: None,
            }),
            catalog: Catalog::open_mem()?,
            gpu: Mutex::new(Gpu::try_new()),
        })
    }

    fn with_decoded<R>(&self, path: &Path, f: impl FnOnce(&Decoded) -> Result<R>) -> Result<R> {
        let mut st = self.state.lock().unwrap();
        let stamp = file_stamp(path);
        if st.path.as_deref() != Some(path) || st.decoded.is_none() || st.stamp != stamp {
            st.decoded = Some(decode::decode(path)?);
            st.path = Some(path.to_path_buf());
            st.stamp = stamp;
        }
        f(st.decoded.as_ref().unwrap())
    }

    /// render with a recipe, fit inside max_px (0 = full res)
    pub fn render(&self, path: &Path, recipe: &Recipe, max_px: u32) -> Result<RgbaImage> {
        self.with_decoded(path, |d| {
            let mut gpu = self.gpu.lock().unwrap();
            if let (Decoded::Mosaic(m), Some(g)) = (d, gpu.as_ref()) {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    g.develop(m, recipe, max_px)
                }));
                match r {
                    Ok(Ok(img)) => return Ok(img),
                    Ok(Err(e)) => log::warn!("GPU develop failed, using CPU: {e:#}"),
                    Err(_) => log::warn!("GPU develop panicked, using CPU"),
                }
                // a failed device may be poisoned: drop it for good
                *gpu = None;
            }
            drop(gpu);
            Ok(develop::develop_cpu(d, recipe, max_px))
        })
    }

    /// export: full-res render (max_px = 0)
    pub fn export(&self, path: &Path, recipe: &Recipe) -> Result<RgbaImage> {
        self.render(path, recipe, 0)
    }

    /// auto-correction analysis: neutral 512px render -> suggested recipe
    /// values (see auto.rs). CPU stats on an sRGB preview.
    pub fn auto_analyze(&self, path: &Path) -> Result<auto::AutoResult> {
        let img = self.render(path, &Recipe::default(), 512)?;
        Ok(auto::analyze(&img))
    }

    /// fast preview: embedded thumbnail if any, else small develop.
    pub fn thumbnail(&self, path: &Path, max_px: u32) -> Result<RgbaImage> {
        if decode::is_raw(path) {
            if let Ok(Some(t)) = decode::embedded_thumb(path) {
                let img = image::RgbaImage::from_raw(t.w as u32, t.h as u32, t.rgba);
                if let Some(img) = img {
                    // embedded thumbs are stored unrotated — match the develop
                    // path's orientation (dcraw flip codes)
                    let img = match t.flip {
                        3 => image::imageops::rotate180(&img),
                        5 => image::imageops::rotate270(&img),
                        6 => image::imageops::rotate90(&img),
                        _ => img,
                    };
                    let out = if max_px > 0 && t.w.max(t.h) as u32 > max_px {
                        let s = max_px as f32 / t.w.max(t.h) as f32;
                        image::imageops::resize(
                            &img,
                            ((t.w as f32 * s) as u32).max(1),
                            ((t.h as f32 * s) as u32).max(1),
                            image::imageops::FilterType::Triangle,
                        )
                    } else {
                        img
                    };
                    return Ok(RgbaImage {
                        width: out.width(),
                        height: out.height(),
                        data: out.into_raw(),
                    });
                }
            }
        }
        self.render(path, &Recipe::default(), max_px.max(1))
    }

    pub fn scan(&self, folder: &Path) -> Result<Vec<AssetEntry>> {
        self.catalog.scan(folder)
    }

    pub fn metadata(&self, path: &Path) -> Result<Value> {
        let mut v = json!({
            "path": path.to_string_lossy(),
            "kind": if decode::is_raw(path) { "raw" } else { "raster" },
        });
        if decode::is_raster(path) {
            if let Ok((w, h)) = image::image_dimensions(path) {
                v["width"] = json!(w);
                v["height"] = json!(h);
            }
            if let Ok(f) = std::fs::File::open(path) {
                let ex = exif::Reader::new()
                    .read_from_container(&mut std::io::BufReader::new(f));
                if let Ok(ex) = ex {
                    let get = |t: exif::Tag| {
                        ex.get_field(t, exif::In::PRIMARY)
                            .map(|f| f.display_value().to_string())
                    };
                    v["make"] = json!(get(exif::Tag::Make).unwrap_or_default());
                    v["model"] = json!(get(exif::Tag::Model).unwrap_or_default());
                    v["lens"] = json!(get(exif::Tag::LensModel).unwrap_or_default());
                    v["iso"] = json!(get(exif::Tag::PhotographicSensitivity).unwrap_or_default());
                    v["shutter"] = json!(get(exif::Tag::ExposureTime).unwrap_or_default());
                    v["aperture"] = json!(get(exif::Tag::FNumber).unwrap_or_default());
                    v["focal"] = json!(get(exif::Tag::FocalLength).unwrap_or_default());
                }
            }
            return Ok(v);
        }
        // RAW: open via libraw just for info (decode fills nothing else)
        let d = decode::decode(path)?;
        match d {
            Decoded::Mosaic(m) => {
                v["make"] = json!(m.info.make);
                v["model"] = json!(m.info.model);
                v["lens"] = json!(m.info.lens);
                v["iso"] = json!(m.info.iso);
                v["shutter"] = json!(m.info.shutter);
                v["aperture"] = json!(m.info.aperture);
                v["focal"] = json!(m.info.focal);
                v["timestamp"] = json!(m.info.timestamp);
                v["width"] = json!(m.w);
                v["height"] = json!(m.h);
            }
            Decoded::Raster { w, h, .. } => {
                v["width"] = json!(w);
                v["height"] = json!(h);
            }
        }
        Ok(v)
    }

    pub fn read_sidecar(&self, asset: &Path) -> Result<Sidecar> {
        let sp = sidecar_path_for(asset);
        if sp.exists() {
            crate::catalog::read_sidecar(&sp)
        } else {
            Ok(Sidecar::default())
        }
    }

    pub fn write_sidecar(&self, asset: &Path, sc: &Sidecar) -> Result<()> {
        crate::catalog::write_sidecar(&sidecar_path_for(asset), sc)
    }

    pub fn set_rating(&self, asset: &Path, rating: i32) -> Result<()> {
        self.catalog.set_rating(asset, rating)
    }

    pub fn set_label(&self, asset: &Path, label: &str) -> Result<()> {
        self.catalog.set_label(asset, label)
    }
}
