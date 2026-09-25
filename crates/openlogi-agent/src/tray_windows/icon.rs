//! Owns the brand icon and recolors its alpha mask without replacing the logo.

use openlogi_core::battery::Severity;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, COLOR_WINDOWTEXT, DIB_RGB_COLORS, DeleteObject,
    GetDC, GetDIBits, GetObjectW, GetSysColor, HDC, ReleaseDC, SetDIBits,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconFromResourceEx, CreateIconIndirect, DestroyIcon, GetIconInfo, HICON, ICONINFO,
    IDI_APPLICATION, LR_DEFAULTCOLOR, LoadIconW,
};

pub(super) struct Icon {
    pub(super) handle: HICON,
    owned: bool,
}

impl Drop for Icon {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: this wrapper owns an icon created here, never a shared stock icon.
            unsafe {
                DestroyIcon(self.handle);
            }
        }
    }
}

impl Icon {
    pub(super) fn new(severity: Severity) -> Self {
        const BLACK: &[u8] = include_bytes!("../../assets/tray-icon@2x.png");
        const WHITE: &[u8] = include_bytes!("../../assets/tray-icon-white@2x.png");
        let png = if super::taskbar_is_light() {
            BLACK
        } else {
            WHITE
        };
        // SAFETY: valid embedded PNG; Windows copies the resource bytes.
        let handle = unsafe {
            CreateIconFromResourceEx(
                png.as_ptr(),
                u32::try_from(png.len()).unwrap_or(0),
                1,
                0x0003_0000,
                0,
                0,
                LR_DEFAULTCOLOR,
            )
        };
        if handle.is_null() {
            tracing::warn!("tray icon PNG rejected; falling back to stock icon");
            return Self {
                // SAFETY: loading a shared stock icon; owned remains false.
                handle: unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) },
                owned: false,
            };
        }
        let original = Self {
            handle,
            owned: true,
        };
        let color = if super::battery::high_contrast() {
            // SAFETY: reading a system-defined color needs no handles.
            let color = unsafe { GetSysColor(COLOR_WINDOWTEXT) };
            let [red, green, blue, _] = color.to_le_bytes();
            Some([red, green, blue])
        } else {
            match severity {
                Severity::Normal => None,
                Severity::Low => Some([230, 145, 0]),
                Severity::Critical => Some([220, 55, 55]),
            }
        };
        if let Some(color) = color {
            if let Some(tinted) = original.tinted(color) {
                return tinted;
            }
            tracing::warn!("could not recolor the tray icon; retaining its normal appearance");
        }
        original
    }

    fn tinted(&self, [red, green, blue]: [u8; 3]) -> Option<Self> {
        // SAFETY: GetIconInfo returns newly allocated bitmaps, held until after
        // CreateIconIndirect copies them. DC and bitmaps are cleaned up on every path.
        unsafe {
            let mut info = ICONINFO::default();
            if GetIconInfo(self.handle, &raw mut info) == 0 {
                return None;
            }
            let bitmaps = Bitmaps(info);
            let dc = GetDC(std::ptr::null_mut());
            if dc.is_null() {
                return None;
            }
            let result = tint_bitmap(dc, &bitmaps.0, [red, green, blue]);
            ReleaseDC(std::ptr::null_mut(), dc);
            result
        }
    }
}

struct Bitmaps(ICONINFO);
impl Drop for Bitmaps {
    fn drop(&mut self) {
        // SAFETY: GetIconInfo allocated these; neither was selected into a DC.
        unsafe {
            if !self.0.hbmColor.is_null() {
                DeleteObject(self.0.hbmColor);
            }
            if !self.0.hbmMask.is_null() {
                DeleteObject(self.0.hbmMask);
            }
        }
    }
}

unsafe fn tint_bitmap(dc: HDC, info: &ICONINFO, [red, green, blue]: [u8; 3]) -> Option<Icon> {
    // SAFETY: caller owns live DC and icon bitmaps. Checked dimensions bound the
    // pixel buffer passed to GetDIBits/SetDIBits; both use the same bitmap header.
    unsafe {
        let mut bitmap = BITMAP::default();
        if info.hbmColor.is_null()
            || GetObjectW(
                info.hbmColor,
                i32::try_from(size_of::<BITMAP>()).ok()?,
                (&raw mut bitmap).cast(),
            ) == 0
        {
            return None;
        }
        let width = usize::try_from(bitmap.bmWidth).ok()?;
        let height = usize::try_from(bitmap.bmHeight).ok()?;
        let count = width
            .checked_mul(height)
            .filter(|count| *count <= 256 * 256)?;
        let mut pixels = vec![[0u8; 4]; count];
        let lines = u32::try_from(height).ok()?;
        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).ok()?,
                biWidth: bitmap.bmWidth,
                biHeight: -bitmap.bmHeight,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..BITMAPINFOHEADER::default()
            },
            ..BITMAPINFO::default()
        };
        if GetDIBits(
            dc,
            info.hbmColor,
            0,
            lines,
            pixels.as_mut_ptr().cast(),
            &raw mut header,
            DIB_RGB_COLORS,
        ) != i32::try_from(lines).ok()?
        {
            return None;
        }
        for pixel in &mut pixels {
            let alpha = u16::from(pixel[3]);
            // Icon color bitmaps carry premultiplied BGRA. Preserve alpha and mask.
            let premultiply =
                |channel: u8| u8::try_from(u16::from(channel) * alpha / 255).unwrap_or(0);
            *pixel = [
                premultiply(blue),
                premultiply(green),
                premultiply(red),
                pixel[3],
            ];
        }
        if SetDIBits(
            dc,
            info.hbmColor,
            0,
            lines,
            pixels.as_ptr().cast(),
            &raw const header,
            DIB_RGB_COLORS,
        ) != i32::try_from(lines).ok()?
        {
            return None;
        }
        let handle = CreateIconIndirect(info);
        if handle.is_null() {
            tracing::warn!("could not tint the battery warning tray icon");
            return None;
        }
        Some(Icon {
            handle,
            owned: true,
        })
    }
}
