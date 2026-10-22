//! Native owner-drawn device rows. Windows still owns tracking, focus and navigation.

use std::cell::RefCell;
use std::rc::Rc;

use openlogi_core::battery::Severity;
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_MENU, COLOR_MENUTEXT, CreateFontIndirectW,
    CreatePen, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER,
    DeleteObject, DrawTextW, FillRect, GetDC, GetStockObject, GetSysColor, GetSysColorBrush,
    GetTextExtentPoint32W, HDC, HGDIOBJ, HOLLOW_BRUSH, LineTo, MoveToEx, PS_SOLID, Polyline,
    ReleaseDC, RestoreDC, RoundRect, SaveDC, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::UI::Accessibility::{
    HCF_HIGHCONTRASTON, HIGHCONTRASTW, MSAA_MENU_SIG, MSAAMENUINFO,
};
use windows_sys::Win32::UI::Controls::{DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_SELECTED, ODT_MENU};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, HMENU, InsertMenuItemW, MENUITEMINFOW, MF_SEPARATOR, MFT_OWNERDRAW, MIIM_DATA,
    MIIM_FTYPE, MIIM_ID, NONCLIENTMETRICSW, SPI_GETHIGHCONTRAST, SPI_GETNONCLIENTMETRICS,
    SystemParametersInfoW,
};

use crate::battery::DeviceBattery;

const FIRST_DEVICE: usize = 100;

thread_local! {
    static ACTIVE_MENU: RefCell<Option<Rc<DeviceMenu>>> = const { RefCell::new(None) };
}

/// A GDI object owned here; it must be deselected before the owner drops.
struct GdiObject(HGDIOBJ);

impl Drop for GdiObject {
    fn drop(&mut self) {
        // SAFETY: only freshly created GDI objects enter this owner; drawing
        // restores the saved DC before any selected object is dropped.
        unsafe {
            DeleteObject(self.0);
        }
    }
}

struct SavedDc(HDC, i32);

impl SavedDc {
    fn new(dc: HDC) -> Option<Self> {
        // SAFETY: callers supply a live drawing DC for the entire guard lifetime.
        let saved = unsafe { SaveDC(dc) };
        (saved != 0).then_some(Self(dc, saved))
    }
}

impl Drop for SavedDc {
    fn drop(&mut self) {
        // SAFETY: the paired SaveDC succeeded and the DC still belongs to the caller.
        unsafe {
            RestoreDC(self.0, self.1);
        }
    }
}

struct Row {
    device: DeviceBattery,
    name: Vec<u16>,
    // Windows reads this pointer through dwItemData for MSAA. Both allocations
    // remain stable until DestroyMenu has completed.
    accessible: Vec<u16>,
    msaa: Box<MSAAMENUINFO>,
}

impl Row {
    fn new(device: DeviceBattery) -> Self {
        let mut accessible = super::wide(&device.accessible_label());
        let msaa = Box::new(MSAAMENUINFO {
            dwMSAASignature: MSAA_MENU_SIG.cast_unsigned(),
            cchWText: u32::try_from(accessible.len().saturating_sub(1)).unwrap_or(u32::MAX),
            pszWText: accessible.as_mut_ptr(),
        });
        Self {
            name: super::wide(&device.name),
            device,
            accessible,
            msaa,
        }
    }
}

/// Owns every pointer used by the menu until native tracking and destruction finish.
pub(super) struct DeviceMenu {
    rows: Vec<Row>,
    font: GdiObject,
    dpi: i32,
    width: u32,
    height: u32,
}

impl DeviceMenu {
    pub(super) fn new(hwnd: HWND) -> Option<Rc<Self>> {
        // SAFETY: hwnd is the live tray window on its owning thread. The system
        // fills initialized structs; the DC is released before returning.
        unsafe {
            let dpi = GetDpiForWindow(hwnd).max(96);
            let mut metrics = NONCLIENTMETRICSW {
                cbSize: u32::try_from(size_of::<NONCLIENTMETRICSW>()).ok()?,
                ..NONCLIENTMETRICSW::default()
            };
            if SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS,
                metrics.cbSize,
                (&raw mut metrics).cast(),
                0,
                dpi,
            ) == 0
            {
                tracing::warn!("could not read the Windows menu font");
                return None;
            }
            let font = GdiObject(CreateFontIndirectW(&raw const metrics.lfMenuFont));
            if font.0.is_null() {
                return None;
            }
            let rows: Vec<_> = crate::battery::snapshot()
                .devices
                .into_iter()
                .map(Row::new)
                .collect();
            let dc = GetDC(hwnd);
            if dc.is_null() {
                return None;
            }
            let Some(saved) = SavedDc::new(dc) else {
                ReleaseDC(hwnd, dc);
                return None;
            };
            SelectObject(dc, font.0);
            let mut longest = 0;
            let mut text_height = 0;
            for row in &rows {
                let mut size = SIZE::default();
                if GetTextExtentPoint32W(dc, row.name.as_ptr(), text_len(&row.name), &raw mut size)
                    != 0
                {
                    longest = longest.max(size.cx);
                    text_height = text_height.max(size.cy);
                }
            }
            drop(saved);
            ReleaseDC(hwnd, dc);
            let dpi = i32::try_from(dpi).unwrap_or(96);
            let scale = |value: i32| value * dpi / 96;
            let result = Rc::new(Self {
                rows,
                font,
                dpi,
                width: u32::try_from(longest.min(scale(260)) + scale(116)).unwrap_or(400),
                height: u32::try_from(text_height.max(scale(20)) + scale(12)).unwrap_or(32),
            });
            ACTIVE_MENU.with_borrow_mut(|slot| *slot = Some(Rc::clone(&result)));
            Some(result)
        }
    }

    pub(super) fn append(&self, menu: HMENU) {
        if self.rows.is_empty() {
            return;
        }
        // SAFETY: menu is owned by show_menu. Row data and MSAA text remain
        // pinned by the Rc throughout tracking and until after DestroyMenu.
        unsafe {
            AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
            for (index, row) in self.rows.iter().enumerate() {
                debug_assert_eq!(row.accessible.last(), Some(&0));
                let mut item = MENUITEMINFOW {
                    cbSize: u32::try_from(size_of::<MENUITEMINFOW>()).unwrap_or(0),
                    fMask: MIIM_FTYPE | MIIM_ID | MIIM_DATA,
                    fType: MFT_OWNERDRAW,
                    wID: u32::try_from(FIRST_DEVICE + index).unwrap_or(0),
                    dwItemData: (&raw const *row.msaa) as usize,
                    ..MENUITEMINFOW::default()
                };
                if InsertMenuItemW(menu, u32::MAX, 1, &raw mut item) == 0 {
                    tracing::warn!("could not insert a battery menu row");
                }
            }
        }
    }

    pub(super) fn activate(&self, command: usize) {
        let Some(row) = command
            .checked_sub(FIRST_DEVICE)
            .and_then(|i| self.rows.get(i))
        else {
            return;
        };
        crate::device_selection::request(row.device.key.clone());
        super::open_or_focus_gui();
    }

    fn scale(&self, value: i32) -> i32 {
        value * self.dpi / 96
    }

    fn draw(&self, item: &DRAWITEMSTRUCT, row: &Row) {
        // SAFETY: Windows supplies this DC and rectangle during WM_DRAWITEM.
        // All selected objects are restored before their owners drop.
        unsafe {
            let Some(saved) = SavedDc::new(item.hDC) else {
                return;
            };
            let selected = item.itemState & ODS_SELECTED != 0;
            let foreground = GetSysColor(if selected {
                COLOR_HIGHLIGHTTEXT
            } else {
                COLOR_MENUTEXT
            });
            FillRect(
                item.hDC,
                &raw const item.rcItem,
                GetSysColorBrush(if selected {
                    COLOR_HIGHLIGHT
                } else {
                    COLOR_MENU
                }),
            );
            SelectObject(item.hDC, self.font.0);
            SetTextColor(item.hDC, foreground);
            SetBkMode(item.hDC, TRANSPARENT.cast_signed());
            let mut label_rect = item.rcItem;
            label_rect.left += self.scale(14);
            label_rect.right -= self.scale(94);
            DrawTextW(
                item.hDC,
                row.name.as_ptr(),
                text_len(&row.name),
                &raw mut label_rect,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
            let mut badge = item.rcItem;
            badge.right -= self.scale(17);
            badge.left = badge.right - self.scale(68);
            badge.top += (badge.bottom - badge.top - self.scale(22)) / 2;
            badge.bottom = badge.top + self.scale(22);
            self.draw_badge(item.hDC, badge, &row.device, foreground, selected);
            drop(saved);
        }
    }

    fn draw_badge(
        &self,
        dc: HDC,
        rect: RECT,
        device: &DeviceBattery,
        foreground: COLORREF,
        selected: bool,
    ) {
        let color = if high_contrast() || selected {
            foreground
        } else if device.charging {
            rgb(25, 130, 75)
        } else {
            match device.severity {
                Severity::Normal => foreground,
                Severity::Low => rgb(183, 100, 0),
                Severity::Critical => rgb(198, 40, 40),
            }
        };
        // SAFETY: objects are created here, selected only into the live drawing
        // DC, and deselected by saved before the GDI owners are dropped.
        unsafe {
            let pen = GdiObject(CreatePen(PS_SOLID, self.scale(2).max(1), color));
            if pen.0.is_null() {
                return;
            }
            let Some(saved) = SavedDc::new(dc) else {
                return;
            };
            SelectObject(dc, pen.0);
            SelectObject(dc, GetStockObject(HOLLOW_BRUSH));
            RoundRect(
                dc,
                rect.left,
                rect.top,
                rect.right,
                rect.bottom,
                self.scale(5),
                self.scale(5),
            );
            MoveToEx(
                dc,
                rect.right + self.scale(2),
                rect.top + self.scale(7),
                std::ptr::null_mut(),
            );
            LineTo(dc, rect.right + self.scale(2), rect.bottom - self.scale(7));
            SetTextColor(dc, color);
            let label = device
                .percentage
                .map_or_else(|| "?".to_owned(), |value| format!("{value}%"));
            let label = super::wide(&label);
            let mut text_rect = rect;
            if device.charging {
                text_rect.right -= self.scale(13);
            }
            DrawTextW(
                dc,
                label.as_ptr(),
                text_len(&label),
                &raw mut text_rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            if device.charging {
                // Draw the bolt as a shape, avoiding font-dependent emoji/color glyphs.
                let x = rect.right - self.scale(12);
                let y = rect.top + self.scale(4);
                let points = [
                    windows_sys::Win32::Foundation::POINT {
                        x: x + self.scale(5),
                        y,
                    },
                    windows_sys::Win32::Foundation::POINT {
                        x,
                        y: y + self.scale(7),
                    },
                    windows_sys::Win32::Foundation::POINT {
                        x: x + self.scale(4),
                        y: y + self.scale(7),
                    },
                    windows_sys::Win32::Foundation::POINT {
                        x,
                        y: y + self.scale(14),
                    },
                ];
                Polyline(dc, points.as_ptr(), 4);
            }
            drop(saved);
        }
    }
}

pub(super) fn is_open() -> bool {
    ACTIVE_MENU.with_borrow(Option::is_some)
}

pub(super) fn clear() {
    ACTIVE_MENU.with_borrow_mut(|slot| *slot = None);
}

pub(super) unsafe fn measure(lparam: LPARAM) -> bool {
    // SAFETY: called only for WM_MEASUREITEM. Windows owns the writable struct
    // for this callback; itemData is deliberately never dereferenced here.
    let item = unsafe { &mut *(lparam as *mut MEASUREITEMSTRUCT) };
    if item.CtlType != ODT_MENU {
        return false;
    }
    let menu = ACTIVE_MENU.with_borrow(Clone::clone);
    let Some(menu) = menu else {
        return false;
    };
    if usize::try_from(item.itemID)
        .ok()
        .and_then(|id| id.checked_sub(FIRST_DEVICE))
        .and_then(|index| menu.rows.get(index))
        .is_none()
    {
        return false;
    }
    item.itemWidth = menu.width;
    item.itemHeight = menu.height;
    true
}

pub(super) unsafe fn draw(lparam: LPARAM) -> bool {
    // SAFETY: called only for WM_DRAWITEM with the OS-owned struct. Resolve
    // rows by bounded command id, never by the untyped itemData pointer.
    let item = unsafe { &*(lparam as *const DRAWITEMSTRUCT) };
    if item.CtlType != ODT_MENU {
        return false;
    }
    let menu = ACTIVE_MENU.with_borrow(Clone::clone);
    let Some(menu) = menu else {
        return false;
    };
    let Some(row) = usize::try_from(item.itemID)
        .ok()
        .and_then(|id| id.checked_sub(FIRST_DEVICE))
        .and_then(|index| menu.rows.get(index))
    else {
        return false;
    };
    menu.draw(item, row);
    true
}

fn text_len(text: &[u16]) -> i32 {
    i32::try_from(text.len().saturating_sub(1)).unwrap_or(i32::MAX)
}

pub(super) const fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    u32::from_le_bytes([red, green, blue, 0])
}

pub(super) fn high_contrast() -> bool {
    // SAFETY: initialized struct and exact size for SPI_GETHIGHCONTRAST.
    unsafe {
        let mut setting = HIGHCONTRASTW {
            cbSize: u32::try_from(size_of::<HIGHCONTRASTW>()).unwrap_or(0),
            ..HIGHCONTRASTW::default()
        };
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            setting.cbSize,
            (&raw mut setting).cast(),
            0,
        ) != 0
            && setting.dwFlags & HCF_HIGHCONTRASTON != 0
    }
}
