// HTTP runtime support for Atomic language
// Uses system curl via std::process::Command for HTTP/HTTPS requests.
// From the Atomic language perspective, httpRequest() is a built-in primitive —
// the implementation details are the compiler's concern.
use std::ffi::CString;
use std::os::raw::c_char;
use std::process::Command;

// Preserve symbols from being optimized out by the linker.
// These are called by JIT-compiled Atomic code via dlsym.
#[used]
static ATOMIC_HTTP_REQUEST_PTR: unsafe extern "C" fn(*const c_char, *const c_char, *const c_char, *const c_char, i64) -> *mut c_char = atomic_http_request;
#[used]
static ATOMIC_HTTP_FREE_PTR: unsafe extern "C" fn(*mut c_char) = atomic_http_free;

// Simple test function to verify JIT FFI works
#[no_mangle]
pub extern "C" fn atomic_test_ping() -> i64 {
    42
}

#[used]
static ATOMIC_TEST_PING_PTR: unsafe extern "C" fn() -> i64 = atomic_test_ping;

/// Perform an HTTP request using system curl.
///
/// Parameters:
///   method    - HTTP method ("GET", "POST", "PUT", "DELETE", "PATCH")
///   url       - Full URL including https://
///   headers   - Headers as "Name: Value\n" separated lines
///   body      - Request body (null if no body)
///   body_len  - Length of body in bytes (0 if no body)
///
/// Returns a C string in format "STATUS_CODE\nRESPONSE_BODY"
/// On error, returns "0\nError message"
/// Caller must free with atomic_http_free()
#[no_mangle]
pub extern "C" fn atomic_http_request(
    method: *const c_char,
    url: *const c_char,
    headers: *const c_char,
    body: *const c_char,
    body_len: i64,
) -> *mut c_char {
    let method = unsafe { std::ffi::CStr::from_ptr(method) }
        .to_str()
        .unwrap_or("GET");
    let url = unsafe { std::ffi::CStr::from_ptr(url) }
        .to_str()
        .unwrap_or("");
    let headers_str = unsafe { std::ffi::CStr::from_ptr(headers) }
        .to_str()
        .unwrap_or("");

    let mut cmd = Command::new("curl");
    cmd.arg("-s")           // silent mode
        .arg("-i")          // include response headers
        .arg("--max-time").arg("120")  // timeout
        .arg("-X").arg(method)
        .arg(url);

    // Parse and add headers
    for h in headers_str.lines() {
        let h = h.trim();
        if !h.is_empty() {
            cmd.arg("-H").arg(h);
        }
    }

    // Add body if present
    if !body.is_null() && body_len > 0 {
        let body_bytes =
            unsafe { std::slice::from_raw_parts(body as *const u8, body_len as usize) };
        let body_str = std::str::from_utf8(body_bytes).unwrap_or("");
        cmd.arg("-d").arg(body_str);
    }

    match cmd.output() {
        Ok(output) => {
            let raw = String::from_utf8_lossy(&output.stdout);
            // Parse HTTP response: split headers from body
            let body_start = raw.find("\r\n\r\n").map(|i| i + 4)
                .or_else(|| raw.find("\n\n").map(|i| i + 2))
                .unwrap_or(0);

            let headers_part = &raw[..body_start.saturating_sub(2)];
            let response_body = &raw[body_start..];

            // Extract status code from first line "HTTP/1.1 200 OK"
            let status_code = headers_part
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(0);

            let result = format!("{}\n{}", status_code, response_body.trim_end());
            CString::new(result).unwrap_or_else(|_| CString::new("0\nEncoding error").unwrap()).into_raw()
        }
        Err(e) => {
            let err = format!("0\nHTTP request failed: {}", e);
            CString::new(err).unwrap_or_else(|_| CString::new("0\nError").unwrap()).into_raw()
        }
    }
}

/// Free a string returned by atomic_http_request
#[no_mangle]
pub extern "C" fn atomic_http_free(ptr: *mut c_char) {
    if !ptr.is_null() {
        unsafe {
            drop(CString::from_raw(ptr));
        }
    }
}
