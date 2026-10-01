use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::OnceLock;

/// Resolve a symbol from ApplicationServices, caching the `dlopen`
/// handle for the process lifetime. Returns `None` if the framework or
/// symbol is unavailable on this macOS version.
pub(super) fn symbol(symbol: &CStr) -> Option<*mut c_void> {
    const RTLD_LAZY: c_int = 0x1;
    const APP_SERVICES: &CStr =
        c"/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices";
    static HANDLE: OnceLock<usize> = OnceLock::new();

    // SAFETY: `dlopen`/`dlsym` come from libSystem; APP_SERVICES and
    // `symbol` are valid C strings. The handle is cached and
    // intentionally never closed.
    let sym = unsafe {
        let handle = *HANDLE.get_or_init(|| dlopen(APP_SERVICES.as_ptr(), RTLD_LAZY) as usize);
        if handle == 0 {
            return None;
        }
        dlsym(handle as *mut c_void, symbol.as_ptr())
    };
    (!sym.is_null()).then_some(sym)
}

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
