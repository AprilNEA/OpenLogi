//! Pointer motion for the Windows hook, read from raw input (`WM_INPUT`).
//!
//! Motion can't come from the hook itself: `WH_MOUSE_LL` carries the absolute
//! cursor point, which the OS clamps at the desktop edge, so differencing it
//! reports nothing for the rest of a swipe that runs into one. Raw input
//! reports the device's own relative counts, like macOS `MOUSE_EVENT_DELTA_X`
//! and Linux `REL_X`.

use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HID_USAGE_GENERIC_MOUSE, HID_USAGE_PAGE_GENERIC,
};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Input::{
    GetRawInputBuffer, GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE,
    RAWINPUTHEADER, RAWMOUSE, RID_INPUT, RIDEV_INPUTSINK, RIDEV_REMOVE, RIM_TYPEMOUSE,
    RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE};
use windows_sys::core::w;

use super::last_error;
use crate::{HookError, MouseEvent};

/// Set when the low-level hook sees a button press, cleared by the next drain,
/// which discards its motion — see [`invalidate`].
///
/// Written and read on the hook thread alone (`WH_MOUSE_LL` callbacks run on the
/// installing thread), so `Relaxed` is enough; it is a `static` only because the
/// hook procedure is a bare `extern "system"` function.
static DISCARD_QUEUED_MOTION: AtomicBool = AtomicBool::new(false);

/// Batch buffer size, in `u64`s — `Vec<u64>` because `GetRawInputBuffer`
/// requires QWORD alignment, which a `Vec<u8>` does not give.
///
/// 16 KiB holds ~340 mouse records, a third of a second of backlog at a 1 kHz
/// polling rate. If it ever does overflow the call fails and the records stay
/// queued, to be read one at a time by the per-message path — slower, not
/// broken.
const BATCH_QWORDS: usize = 2048;

/// Offset of a record's payload union from its start, in QWORDs — the step from
/// a record pointer to its `RAWMOUSE`.
const DATA_QWORDS: usize = {
    let offset = std::mem::offset_of!(RAWINPUT, data);
    assert!(
        offset.is_multiple_of(size_of::<u64>()),
        "RAWINPUT payload must be QWORD-aligned for the batch walk"
    );
    offset / size_of::<u64>()
};

/// QWORDs a record's header occupies — the minimum that must remain in the
/// batch buffer before one can be read.
const HEADER_QWORDS: usize = size_of::<RAWINPUTHEADER>().div_ceil(size_of::<u64>());

/// QWORDs from a record's start past the end of its `RAWMOUSE` payload.
const MOUSE_END_QWORDS: usize = DATA_QWORDS + size_of::<RAWMOUSE>().div_ceil(size_of::<u64>());

/// Subscribe to raw mouse input, delivered as `WM_INPUT` to this thread's
/// queue, and return the sink window to pass back to [`uninstall`].
///
/// `RIDEV_INPUTSINK` — raw input while unfocused, and the agent never has focus
/// — needs a target window, so this creates a message-only one of the predefined
/// `Static` class. Its window procedure is never relied on: the message loop
/// releases each `WM_INPUT` record by calling `DefWindowProcW` itself.
pub(super) fn install() -> Result<HWND, HookError> {
    // SAFETY: the class name is a live NUL-terminated UTF-16 literal naming a
    // predefined system class; HWND_MESSAGE as parent creates a message-only
    // window, for which null instance/menu/param and zero style and geometry
    // are valid. Returns null on failure, checked below.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            w!("Static"),
            std::ptr::null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(last_error("CreateWindowExW"));
    }

    let device = RAWINPUTDEVICE {
        usUsagePage: HID_USAGE_PAGE_GENERIC,
        usUsage: HID_USAGE_GENERIC_MOUSE,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    };
    // SAFETY: `device` is a live, fully initialized RAWINPUTDEVICE and the
    // count and struct size describe it exactly; `hwndTarget` is the window
    // just created on this thread, which outlives the registration.
    if unsafe { RegisterRawInputDevices(&device, 1, size_of::<RAWINPUTDEVICE>() as u32) } == 0 {
        // SAFETY: `hwnd` is the live window just created on this thread.
        unsafe { DestroyWindow(hwnd) };
        return Err(last_error("RegisterRawInputDevices"));
    }
    Ok(hwnd)
}

/// Drop the raw-input subscription and the sink window.
///
/// The registration is process-wide and outlives `hwnd`, so destroying the
/// window is not enough: without the explicit `RIDEV_REMOVE` a later reinstall
/// would re-register the same usage page against a new window with the stale
/// entry still in place.
///
/// Must run on the thread that called [`install`] — `DestroyWindow` only works
/// on the window's owning thread.
pub(super) fn uninstall(hwnd: HWND) {
    let device = RAWINPUTDEVICE {
        usUsagePage: HID_USAGE_PAGE_GENERIC,
        usUsage: HID_USAGE_GENERIC_MOUSE,
        dwFlags: RIDEV_REMOVE,
        // RIDEV_REMOVE requires a null target.
        hwndTarget: std::ptr::null_mut(),
    };
    // SAFETY: `device` is a live, fully initialized RAWINPUTDEVICE and the
    // count and struct size describe it exactly.
    if unsafe { RegisterRawInputDevices(&device, 1, size_of::<RAWINPUTDEVICE>() as u32) } == 0 {
        tracing::warn!(error = %last_error("RegisterRawInputDevices(RIDEV_REMOVE)"), "could not unregister raw mouse input");
    }
    // SAFETY: `hwnd` was returned by `install` on this thread and is destroyed
    // exactly once, here.
    unsafe { DestroyWindow(hwnd) };
}

/// Discard the motion already queued when a button transition arrives.
///
/// Buttons and motion reach the hook thread on two independent paths: a
/// `WH_MOUSE_LL` callback runs as soon as the thread waits for a message, ahead
/// of `WM_INPUT` records already sitting in the queue. Without this, a flick
/// followed a millisecond later by a gesture-button press would have its
/// *pre-press* travel summed into the fresh hold, committing a swipe the user
/// never made during it. Dropping the queued backlog costs at most the motion
/// between the press and the next drain — far inside the hold's swipe window.
pub(super) fn invalidate() {
    DISCARD_QUEUED_MOTION.store(true, Ordering::Relaxed);
}

/// Drain this thread's raw-input queue and return the total pointer motion in
/// it, starting from the record `current` announces.
///
/// The hook thread services `WH_MOUSE_LL` between `GetMessageW` calls, and
/// Windows drops hook events the thread doesn't answer within
/// `LowLevelHooksTimeout`. Reading one record per message put a 1 kHz mouse's
/// whole report stream in front of every click; `GetRawInputBuffer` takes the
/// backlog in one syscall instead.
///
/// Summing the batch is what [`crate::MouseEvent::Moved`]'s consumer does with
/// the deltas anyway — the accumulator adds them up and tests a threshold — so
/// batching only defers a commit to the end of the drain, which the thread
/// reaches as soon as the queue empties.
///
/// # Safety
/// `current` must be the `lParam` of a `WM_INPUT` message just retrieved from
/// this thread's queue, i.e. a live raw-input handle the OS has not yet
/// released.
pub(super) unsafe fn drain_motion(current: HRAWINPUT, batch: &mut Vec<u64>) -> Option<MouseEvent> {
    // Taken before the drain, not after: the hook procedure that sets it runs on
    // this thread, which is busy here, so nothing can arrive mid-drain.
    let discard = DISCARD_QUEUED_MOTION.swap(false, Ordering::Relaxed);

    // SAFETY: `current` is a live raw-input handle by this function's contract.
    let (mut dx, mut dy) = unsafe { read_one(current) }.unwrap_or((0, 0));

    if batch.is_empty() {
        batch.resize(BATCH_QWORDS, 0);
    }
    loop {
        let mut size = (batch.len() * size_of::<u64>()) as u32;
        // SAFETY: `batch` is a live allocation of `size` bytes, QWORD-aligned
        // because it is a `Vec<u64>`. Returns 0 when the queue is empty and
        // u32::MAX on failure; both stop the drain below.
        let count = unsafe {
            GetRawInputBuffer(
                batch.as_mut_ptr().cast(),
                &mut size,
                size_of::<RAWINPUTHEADER>() as u32,
            )
        };
        if count == 0 || count == u32::MAX {
            break;
        }

        // Walked in QWORDs rather than bytes so each record pointer inherits
        // the buffer's alignment instead of being cast up from a `*const u8`.
        let mut at = 0usize;
        for _ in 0..count {
            // Every read below is bounds-checked against what is left of the
            // buffer rather than trusted to `dwSize`: this is `unsafe`, so a
            // record whose size desynchronises the walk must stop it, not turn
            // into a read past the allocation.
            let Some(record) = batch.get(at..) else {
                break;
            };
            if record.len() < HEADER_QWORDS {
                break;
            }
            // SAFETY: the OS wrote `count` records into `batch` and at least a
            // header's worth of buffer remains. Read by value — records are
            // plain-old-data — from a pointer the slice keeps QWORD-aligned.
            let header = unsafe { std::ptr::read(record.as_ptr().cast::<RAWINPUTHEADER>()) };
            // Records are packed QWORD-aligned, not end to end.
            let stride = (header.dwSize as usize).div_ceil(size_of::<u64>());
            if stride == 0 {
                break;
            }
            if header.dwType == RIM_TYPEMOUSE && record.len() >= MOUSE_END_QWORDS {
                // SAFETY: a mouse record's payload is the `mouse` arm of the
                // union at `RAWINPUT`'s `data` offset, and the whole payload
                // lies inside the buffer per the length check above.
                let mouse =
                    unsafe { std::ptr::read(record.as_ptr().add(DATA_QWORDS).cast::<RAWMOUSE>()) };
                if let Some((x, y)) = motion(mouse) {
                    dx = dx.saturating_add(x);
                    dy = dy.saturating_add(y);
                }
            }
            at += stride;
        }
    }

    // The queue still had to be drained — the records are the OS's to reclaim —
    // but a button press just invalidated everything in it.
    if discard {
        return None;
    }

    (dx != 0 || dy != 0).then_some(MouseEvent::Moved {
        delta_x: dx,
        delta_y: dy,
    })
}

/// Read the single record behind a `WM_INPUT` handle.
///
/// # Safety
/// `handle` must be a live raw-input handle — see [`drain_motion`].
unsafe fn read_one(handle: HRAWINPUT) -> Option<(i32, i32)> {
    let mut raw = RAWINPUT::default();
    let mut size = size_of::<RAWINPUT>() as u32;
    // SAFETY: `handle` is a live raw-input handle by this function's contract;
    // `raw` is a live, owned RAWINPUT and `size` its byte length, so the call
    // writes at most that much. Returns u32::MAX on failure, checked below.
    let read = unsafe {
        GetRawInputData(
            handle,
            RID_INPUT,
            std::ptr::from_mut(&mut raw).cast(),
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if read == u32::MAX || raw.header.dwType != RIM_TYPEMOUSE {
        return None;
    }
    // SAFETY: the header says this record is a mouse, so the union holds
    // `mouse`.
    motion(unsafe { raw.data.mouse })
}

/// Pointer motion carried by a mouse record, or `None` if it isn't relative
/// movement. Raw input has no injected-input flag, so another process's
/// `SendInput` motion feeds a gesture hold here; our own injector never moves
/// the pointer.
fn motion(mouse: RAWMOUSE) -> Option<(i32, i32)> {
    // Absolute-mode devices (tablets, RDP, some VMs) report a screen position
    // in these fields rather than a delta — the very clamping this path exists
    // to avoid, so they contribute no gesture motion.
    if mouse.usFlags & MOUSE_MOVE_ABSOLUTE != 0 {
        return None;
    }
    // Button-only records repeat the last position with a zero delta.
    (mouse.lLastX != 0 || mouse.lLastY != 0).then_some((mouse.lLastX, mouse.lLastY))
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::UI::Input::{MOUSE_MOVE_RELATIVE, MOUSE_STATE};

    use super::*;

    /// Raw input is the only motion source, and it feeds the swipe accumulator:
    /// relative counts pass through untouched, while absolute-mode records (a
    /// screen position, already edge-clamped) and the zero-delta records that
    /// accompany button presses must not.
    #[test]
    fn motion_reports_relative_deltas_only() {
        let mouse = |flags: MOUSE_STATE, x, y| RAWMOUSE {
            usFlags: flags,
            lLastX: x,
            lLastY: y,
            ..RAWMOUSE::default()
        };

        assert_eq!(motion(mouse(MOUSE_MOVE_RELATIVE, 60, -5)), Some((60, -5)));
        assert!(
            motion(mouse(MOUSE_MOVE_ABSOLUTE, 60, -5)).is_none(),
            "absolute records carry a clamped position, not a delta"
        );
        assert!(
            motion(mouse(MOUSE_MOVE_RELATIVE, 0, 0)).is_none(),
            "button-only records repeat a zero delta"
        );
    }

    /// The batch walk steps `dwSize` rounded up to the next QWORD, not
    /// `dwSize` itself, and reaches the payload via [`DATA_QWORDS`]. Getting
    /// either wrong desynchronises the walk from the second record on, which
    /// would read neighbouring records as garbage deltas.
    #[test]
    fn batch_walk_matches_record_layout() {
        let record = size_of::<RAWINPUTHEADER>() + size_of::<RAWMOUSE>();
        let stride = record.next_multiple_of(size_of::<u64>());
        assert!(stride >= record, "a record is never truncated by alignment");
        assert!(
            stride.is_multiple_of(size_of::<u64>()),
            "the next record starts on a QWORD boundary"
        );
        assert!(
            DATA_QWORDS * size_of::<u64>() >= size_of::<RAWINPUTHEADER>(),
            "the payload follows the header, never overlaps it"
        );
        // The bounds guards must admit a well-formed record, or the walk would
        // silently skip every mouse payload instead of summing it.
        assert!(HEADER_QWORDS * size_of::<u64>() >= size_of::<RAWINPUTHEADER>());
        assert!(
            MOUSE_END_QWORDS * size_of::<u64>() >= record,
            "the payload bound covers a whole mouse record"
        );
        assert!(
            stride / size_of::<u64>() >= HEADER_QWORDS,
            "a record's stride always leaves room for the next header check"
        );
    }
}
