//! Browser-only Tatara: the modeling core compiled to WebAssembly with a
//! plain C ABI (no wasm-bindgen). JavaScript copies a request into memory
//! from `tatara_alloc`, calls `tatara_request` with the method, the path
//! relative to `/api` and the body, then reads the response back. The
//! request is routed by the same `tatara::api::handle` the native server
//! uses, so both backends behave identically.

use std::cell::RefCell;

use tatara::{api, engine::Editor};

struct Last {
    body: Vec<u8>,
    content_type: &'static str,
    disposition: &'static str,
}

thread_local! {
    static EDITOR: RefCell<Editor> = RefCell::new(Editor::new());
    static LAST: RefCell<Last> = const { RefCell::new(Last { body: Vec::new(), content_type: "", disposition: "" }) };
}

/// Reserve `len` bytes for the caller to fill.
#[unsafe(no_mangle)]
pub extern "C" fn tatara_alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// Release memory from `tatara_alloc`.
///
/// # Safety
/// `ptr` and `len` must come from one `tatara_alloc` call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tatara_free(ptr: *mut u8, len: usize) {
    drop(unsafe { Vec::from_raw_parts(ptr, 0, len.max(1)) });
}

unsafe fn slice<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

/// Handle one API request and return its HTTP status. The response body
/// and content type stay available until the next request.
///
/// # Safety
/// Each pointer must reference `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tatara_request(
    method_ptr: *const u8,
    method_len: usize,
    path_ptr: *const u8,
    path_len: usize,
    body_ptr: *const u8,
    body_len: usize,
) -> u32 {
    let method = String::from_utf8_lossy(unsafe { slice(method_ptr, method_len) }).into_owned();
    let path = String::from_utf8_lossy(unsafe { slice(path_ptr, path_len) }).into_owned();
    let body = unsafe { slice(body_ptr, body_len) };
    let response = EDITOR.with_borrow_mut(|ed| api::handle(ed, &method, &path, body, false));
    let status = response.status as u32;
    LAST.with_borrow_mut(|last| {
        *last = Last {
            body: response.body,
            content_type: response.content_type,
            disposition: response.disposition.unwrap_or(""),
        }
    });
    status
}

#[unsafe(no_mangle)]
pub extern "C" fn tatara_response_ptr() -> *const u8 {
    LAST.with_borrow(|l| l.body.as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn tatara_response_len() -> usize {
    LAST.with_borrow(|l| l.body.len())
}

#[unsafe(no_mangle)]
pub extern "C" fn tatara_response_type_ptr() -> *const u8 {
    LAST.with_borrow(|l| l.content_type.as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn tatara_response_type_len() -> usize {
    LAST.with_borrow(|l| l.content_type.len())
}

#[unsafe(no_mangle)]
pub extern "C" fn tatara_response_disposition_ptr() -> *const u8 {
    LAST.with_borrow(|l| l.disposition.as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn tatara_response_disposition_len() -> usize {
    LAST.with_borrow(|l| l.disposition.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, path: &str, body: &str) -> (u32, String) {
        let status = unsafe {
            tatara_request(
                method.as_ptr(),
                method.len(),
                path.as_ptr(),
                path.len(),
                body.as_ptr(),
                body.len(),
            )
        };
        let out = unsafe { slice(tatara_response_ptr(), tatara_response_len()) };
        (status, String::from_utf8_lossy(out).into_owned())
    }

    #[test]
    fn round_trips_through_the_c_abi() {
        let (s, body) = request(
            "POST",
            "/commands",
            r#"{"commands":[{"op":"add","primitive":{"kind":"cube"}}]}"#,
        );
        assert_eq!(s, 200, "{body}");
        let (s, body) = request("GET", "/state", "");
        assert_eq!(s, 200);
        assert!(body.contains("\"Cube\""));
        let p = tatara_alloc(16);
        unsafe { tatara_free(p, 16) };
    }
}
