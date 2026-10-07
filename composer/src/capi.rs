//! C FFI for the document session: one entry point, `kou_dispatch`, which
//! routes a JSON command through the same dispatcher the CLI control channel
//! and MCP server use. Strings returned here are freed with
//! `koubou_free_string` (koubou-core).

use std::ffi::{c_char, CStr, CString};

use crate::commands::Session;

#[no_mangle]
pub extern "C" fn kou_session_new() -> *mut Session {
    match Session::new() {
        Ok(s) => Box::into_raw(Box::new(s)),
        Err(_) => std::ptr::null_mut(),
    }
}

/// # Safety
/// `s` must come from `kou_session_new` and be freed exactly once.
#[no_mangle]
pub unsafe extern "C" fn kou_session_free(s: *mut Session) {
    if !s.is_null() {
        drop(Box::from_raw(s));
    }
}

/// Dispatch a JSON command; returns a newly-allocated JSON response string
/// (never null — failures are `{"ok":false,"error":...}` JSON; a null return
/// means the response itself could not be encoded).
///
/// # Safety
/// `s` must be a live `Session*` from `kou_session_new`; `json` must be a
/// valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn kou_dispatch(s: *mut Session, json: *const c_char) -> *mut c_char {
    let resp = || -> String {
        let Some(session) = s.as_mut() else {
            return r#"{"ok":false,"error":"null session"}"#.into();
        };
        if json.is_null() {
            return r#"{"ok":false,"error":"null json"}"#.into();
        }
        let text = CStr::from_ptr(json).to_string_lossy();
        let req: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => return format!(r#"{{"ok":false,"error":"bad json: {e}"}}"#),
        };
        session.dispatch(&req).to_string()
    };
    match CString::new(resp()) {
        Ok(c) => c.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}
