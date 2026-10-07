//! C ABI surface consumed by the SwiftUI app and other frontends.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;
use std::sync::Mutex;

use crate::develop::RgbaImage;
use crate::engine::Engine;
use crate::recipe::{Recipe, Sidecar};

#[repr(C)]
pub struct KouImage {
    pub data: *mut u8,
    pub len: usize,
    pub width: u32,
    pub height: u32,
}

/// 256-bin x 4-channel (R,G,B,luma) histogram of the rendered output.
#[repr(C)]
pub struct KouHistogram {
    pub bins: [u32; 1024],
}

thread_local! {
    static LAST_ERROR: Mutex<CString> = Mutex::new(CString::new("").unwrap());
}

fn set_err(e: &anyhow::Error) {
    LAST_ERROR.with(|s| {
        *s.lock().unwrap() = CString::new(e.to_string()).unwrap_or_default();
    });
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn into_raw_string(s: String) -> *mut c_char {
    CString::new(s).map(CString::into_raw).unwrap_or(std::ptr::null_mut())
}

fn into_raw_image(img: RgbaImage) -> KouImage {
    let mut v = img.data.into_boxed_slice();
    let out = KouImage {
        data: v.as_mut_ptr(),
        len: v.len(),
        width: img.width,
        height: img.height,
    };
    std::mem::forget(v);
    out
}

fn null_image() -> KouImage {
    KouImage {
        data: std::ptr::null_mut(),
        len: 0,
        width: 0,
        height: 0,
    }
}

fn engine<'a>(e: *mut c_void) -> Option<&'a Engine> {
    if e.is_null() {
        None
    } else {
        Some(unsafe { &*(e as *const Engine) })
    }
}

#[no_mangle]
pub extern "C" fn koubou_init() -> *mut c_void {
    match Engine::new() {
        Ok(e) => Box::into_raw(Box::new(e)) as *mut c_void,
        Err(err) => {
            set_err(&err);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn koubou_free_engine(e: *mut c_void) {
    if !e.is_null() {
        drop(Box::from_raw(e as *mut Engine));
    }
}

#[no_mangle]
pub extern "C" fn koubou_last_error() -> *mut c_char {
    LAST_ERROR.with(|s| {
        let guard = s.lock().unwrap();
        CString::new(guard.as_bytes()).map(CString::into_raw).unwrap_or(std::ptr::null_mut())
    })
}

#[no_mangle]
pub unsafe extern "C" fn koubou_free_string(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

#[no_mangle]
pub unsafe extern "C" fn koubou_free_image(img: KouImage) {
    if !img.data.is_null() && img.len > 0 {
        drop(Box::from_raw(std::slice::from_raw_parts_mut(img.data, img.len) as *mut [u8]));
    }
}

/// returns JSON array of assets
#[no_mangle]
pub unsafe extern "C" fn koubou_scan_folder(e: *mut c_void, folder: *const c_char) -> *mut c_char {
    let Some(eng) = engine(e) else { return std::ptr::null_mut() };
    match eng.scan(Path::new(&cstr(folder))) {
        Ok(list) => into_raw_string(serde_json::to_string(&list).unwrap_or_else(|_| "[]".into())),
        Err(err) => {
            set_err(&err);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn koubou_thumbnail(
    e: *mut c_void,
    path: *const c_char,
    max_px: u32,
) -> KouImage {
    let Some(eng) = engine(e) else { return null_image() };
    match eng.thumbnail(Path::new(&cstr(path)), max_px) {
        Ok(img) => into_raw_image(img),
        Err(err) => {
            set_err(&err);
            null_image()
        }
    }
}

/// recipe_json: serialized Recipe; empty/null = defaults
#[no_mangle]
pub unsafe extern "C" fn koubou_render(
    e: *mut c_void,
    path: *const c_char,
    recipe_json: *const c_char,
    max_px: u32,
) -> KouImage {
    let Some(eng) = engine(e) else { return null_image() };
    let recipe = cstr(recipe_json);
    let recipe = if recipe.is_empty() {
        Recipe::default()
    } else {
        Recipe::from_json(&recipe).unwrap_or_default()
    };
    match eng.render(Path::new(&cstr(path)), &recipe, max_px) {
        Ok(img) => into_raw_image(img),
        Err(err) => {
            set_err(&err);
            null_image()
        }
    }
}

/// like koubou_render, and additionally fills `hist` (nullable) with the
/// output-image histogram.
#[no_mangle]
pub unsafe extern "C" fn koubou_render_h(
    e: *mut c_void,
    path: *const c_char,
    recipe_json: *const c_char,
    max_px: u32,
    hist: *mut KouHistogram,
) -> KouImage {
    let img = koubou_render(e, path, recipe_json, max_px);
    if !hist.is_null() {
        let bins = if img.data.is_null() {
            [0u32; 1024]
        } else {
            let data = std::slice::from_raw_parts(img.data as *const u8, img.len);
            crate::develop::histogram(data)
        };
        (*hist).bins = bins;
    }
    img
}

/// render preview + fill caller buffers: wave 3*256*256, vec 256*256,
/// cie 256*256, hist 1024 (each nullable, skipped when null)
#[no_mangle]
pub unsafe extern "C" fn koubou_scopes(
    e: *mut c_void,
    path: *const c_char,
    recipe_json: *const c_char,
    max_px: u32,
    wave: *mut u32,
    vec: *mut u32,
    cie: *mut u32,
    hist: *mut u32,
) -> KouImage {
    let img = koubou_render(e, path, recipe_json, max_px);
    if !img.data.is_null() {
        let data = std::slice::from_raw_parts(img.data as *const u8, img.len);
        // heap: these exceed a dispatch-queue worker's ~512KB stack
        let mut wv = vec![0u32; 196608];
        let mut vc = vec![0u32; 65536];
        let mut ce = vec![0u32; 65536];
        crate::develop::scopes(data, img.width, img.height, &mut wv, &mut vc, &mut ce);
        if !wave.is_null() {
            std::ptr::copy_nonoverlapping(wv.as_ptr(), wave, wv.len());
        }
        if !vec.is_null() {
            std::ptr::copy_nonoverlapping(vc.as_ptr(), vec, vc.len());
        }
        if !cie.is_null() {
            std::ptr::copy_nonoverlapping(ce.as_ptr(), cie, ce.len());
        }
        if !hist.is_null() {
            std::ptr::copy_nonoverlapping(
                crate::develop::histogram(data).as_ptr(),
                hist,
                1024,
            );
        }
    }
    img
}

/// full-res render for export; RGBA8 out
#[no_mangle]
pub unsafe extern "C" fn koubou_export(
    e: *mut c_void,
    path: *const c_char,
    recipe_json: *const c_char,
) -> KouImage {
    koubou_render(e, path, recipe_json, 0)
}

#[no_mangle]
pub unsafe extern "C" fn koubou_metadata(e: *mut c_void, path: *const c_char) -> *mut c_char {
    let Some(eng) = engine(e) else { return std::ptr::null_mut() };
    match eng.metadata(Path::new(&cstr(path))) {
        Ok(v) => into_raw_string(v.to_string()),
        Err(err) => {
            set_err(&err);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn koubou_sidecar_read(path: *const c_char) -> *mut c_char {
    let pstr = cstr(path);
    let p = Path::new(&pstr);
    let sp = crate::recipe::sidecar_path_for(p);
    let sc = if sp.exists() {
        crate::catalog::read_sidecar(&sp).unwrap_or_default()
    } else {
        Sidecar::default()
    };
    into_raw_string(serde_json::to_string(&sc).unwrap_or_else(|_| "{}".into()))
}

/// json: serialized Sidecar
#[no_mangle]
pub unsafe extern "C" fn koubou_sidecar_write(path: *const c_char, json: *const c_char) -> i32 {
    let pstr = cstr(path);
    let p = Path::new(&pstr);
    let sc: Sidecar = match serde_json::from_str(&cstr(json)) {
        Ok(s) => s,
        Err(err) => {
            set_err(&err.into());
            return -1;
        }
    };
    match crate::catalog::write_sidecar(&crate::recipe::sidecar_path_for(p), &sc) {
        Ok(()) => 0,
        Err(err) => {
            set_err(&err);
            -2
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn koubou_set_rating(
    e: *mut c_void,
    path: *const c_char,
    rating: i32,
) -> i32 {
    let Some(eng) = engine(e) else { return -1 };
    match eng.set_rating(Path::new(&cstr(path)), rating) {
        Ok(()) => 0,
        Err(err) => {
            set_err(&err);
            -2
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn koubou_set_label(
    e: *mut c_void,
    path: *const c_char,
    label: *const c_char,
) -> i32 {
    let Some(eng) = engine(e) else { return -1 };
    match eng.set_label(Path::new(&cstr(path)), &cstr(label)) {
        Ok(()) => 0,
        Err(err) => {
            set_err(&err);
            -2
        }
    }
}

/// Auto-correction analysis -> JSON with suggested recipe values.
#[no_mangle]
pub unsafe extern "C" fn koubou_auto_analyze(e: *mut c_void, path: *const c_char) -> *mut c_char {
    let eng = &*(e as *const Engine);
    match eng.auto_analyze(Path::new(&cstr(path))) {
        Ok(r) => match serde_json::to_string(&r) {
            Ok(j) => into_raw_string(j),
            Err(e) => {
                set_err(&e.into());
                std::ptr::null_mut()
            }
        },
        Err(e) => {
            set_err(&e);
            std::ptr::null_mut()
        }
    }
}

/// libraw reference render (sanity check / debugging)
#[no_mangle]
pub unsafe extern "C" fn koubou_reference(path: *const c_char) -> KouImage {
    match crate::decode::reference_render(Path::new(&cstr(path))) {
        Ok((rgb, w, h)) => {
            let mut rgba = Vec::with_capacity(rgb.len() / 3 * 4);
            for c in rgb.chunks_exact(3) {
                rgba.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
            into_raw_image(RgbaImage {
                width: w as u32,
                height: h as u32,
                data: rgba,
            })
        }
        Err(err) => {
            set_err(&err);
            null_image()
        }
    }
}
