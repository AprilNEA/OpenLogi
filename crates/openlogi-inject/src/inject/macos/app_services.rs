use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::OnceLock;

/// Resolve a symbol from ApplicationServices, caching the `dlopen`
/// handle for the process lifetime. Returns `None` if the framework or
/// symbol is unavailable on this macOS version.
pub(super) fn symbol(symbol: &CStr) -> Option<*mut c_void> {
    const APP_SERVICES: &CStr =
        c"/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices";
    static HANDLE: OnceLock<usize> = OnceLock::new();
    resolve(APP_SERVICES, &HANDLE, symbol)
}

/// Read-only Space SPI lives in SkyLight; resolve it without a strong link.
pub(super) fn sky_light_symbol(symbol: &CStr) -> Option<*mut c_void> {
    const SKY_LIGHT: &CStr = c"/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight";
    static HANDLE: OnceLock<usize> = OnceLock::new();
    resolve(SKY_LIGHT, &HANDLE, symbol)
}

fn resolve(framework: &CStr, handle: &OnceLock<usize>, symbol: &CStr) -> Option<*mut c_void> {
    const RTLD_LAZY: c_int = 0x1;

    let handle = *handle.get_or_init(|| {
        // SAFETY: framework is a valid NUL-terminated path. The cached handle
        // deliberately lives for the process lifetime and is never closed.
        unsafe { dlopen(framework.as_ptr(), RTLD_LAZY) as usize }
    });
    if handle == 0 {
        tracing::warn!(?framework, "private macOS framework unavailable");
        return None;
    }
    // SAFETY: handle is an open framework and symbol is a valid C string.
    let sym = unsafe { dlsym(handle as *mut c_void, symbol.as_ptr()) };
    if sym.is_null() {
        tracing::warn!(?framework, ?symbol, "private macOS symbol unavailable");
    }
    (!sym.is_null()).then_some(sym)
}

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
