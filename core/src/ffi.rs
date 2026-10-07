//! FFI bindings to the kou_shim C++ wrapper around LibRaw.
#![allow(non_snake_case, non_camel_case_types, dead_code)]

use std::os::raw::{c_char, c_int, c_long, c_uchar, c_uint, c_ushort, c_void};

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KouRawInfo {
    pub make: [c_char; 64],
    pub model: [c_char; 64],
    pub lens: [c_char; 128],
    pub raw_width: c_int,
    pub raw_height: c_int,
    pub width: c_int,
    pub height: c_int,
    pub top_margin: c_int,
    pub left_margin: c_int,
    pub flip: c_int,
    pub black: [f32; 4],
    pub maximum: c_uint,
    pub cam_mul: [f32; 4],
    pub pre_mul: [f32; 4],
    pub cam_xyz: [[f32; 3]; 4],
    pub rgb_cam: [[f32; 4]; 3],
    pub cfa_kind: c_int,
    pub cfa_pattern: [c_int; 36],
    pub cfa_w: c_int,
    pub cfa_h: c_int,
    pub colors: c_int,
    pub daylight_mul_valid: c_int,
    pub iso: f32,
    pub shutter: f32,
    pub aperture: f32,
    pub focal: f32,
    pub timestamp: c_long,
}

pub enum KouRaw {}

extern "C" {
    pub fn kou_raw_open(path: *const c_char, info: *mut KouRawInfo) -> *mut KouRaw;
    pub fn kou_raw_unpack(r: *mut KouRaw) -> c_int;
    pub fn kou_raw_refresh_info(r: *mut KouRaw, info: *mut KouRawInfo);
    pub fn kou_raw_cfa(r: *mut KouRaw, out: *mut *mut c_ushort, count: *mut c_int) -> c_int;
    pub fn kou_thumb(
        r: *mut KouRaw,
        out: *mut *mut c_uchar,
        len: *mut c_int,
        w: *mut c_int,
        h: *mut c_int,
        format: *mut c_int,
    ) -> c_int;
    pub fn kou_process8(
        r: *mut KouRaw,
        out: *mut *mut c_uchar,
        w: *mut c_int,
        h: *mut c_int,
    ) -> c_int;
    pub fn kou_raw_close(r: *mut KouRaw);
    pub fn kou_free(p: *mut c_void);
}

pub fn cstr_field(buf: &[c_char]) -> String {
    unsafe {
        let p = buf.as_ptr();
        if p.is_null() {
            return String::new();
        }
        std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}
