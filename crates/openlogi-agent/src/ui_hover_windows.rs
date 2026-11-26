//! UI hover haptics (Windows): a light tick on the active mouse when the
//! pointer moves onto a new interactive UI element — a button, link, menu
//! item, tab or list item — found through UI Automation.
//!
//! A dedicated COM (MTA) thread samples the physical cursor position at a
//! bounded rate and asks UI Automation only after the pointer moved, so a
//! resting pointer costs one `GetPhysicalCursorPos` per period. Ticks go to a
//! latest-wins player that sends each play without waiting for the device's
//! acknowledgement: over a busy Bluetooth link replies get dropped while the
//! pointer moves, and waiting on them stalled every later tick for seconds.

#![expect(
    unsafe_code,
    reason = "raw win32: COM UI Automation client calls and the runtime-id VARIANT — localized here"
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use openlogi_agent_core::orchestrator::{Orchestrator, SharedHandles};
use openlogi_hid::{DeviceRoute, HapticWaveform, HidppOperation};
use tracing::{debug, warn};
use windows::Win32::Foundation::POINT;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Variant::{VARENUM, VT_ARRAY, VT_I4, VariantClear};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCacheRequest, IUIAutomationElement,
    IUIAutomationTreeWalker, UIA_ButtonControlTypeId, UIA_CONTROLTYPE_ID,
    UIA_CheckBoxControlTypeId, UIA_ComboBoxControlTypeId, UIA_ControlTypePropertyId,
    UIA_DataItemControlTypeId, UIA_HyperlinkControlTypeId, UIA_ListItemControlTypeId,
    UIA_MenuItemControlTypeId, UIA_RadioButtonControlTypeId, UIA_RuntimeIdPropertyId,
    UIA_SplitButtonControlTypeId, UIA_TabItemControlTypeId, UIA_TreeItemControlTypeId,
};
use windows::Win32::UI::WindowsAndMessaging::GetPhysicalCursorPos;

/// How often the pointer position is sampled: at most ~30 UI Automation
/// queries a second while the pointer moves.
const PERIOD: Duration = Duration::from_millis(33);

/// How often the route of the device to tick is re-read from the
/// orchestrator. A setting or device change takes effect within this.
const ROUTE_REFRESH: Duration = Duration::from_secs(1);

/// How long one play may take. Only the first play on a channel waits on the
/// device (to locate the haptic feature); later ones just write a report.
const PLAY_BUDGET: Duration = Duration::from_millis(500);

/// How many ancestors to search for the interactive element: the element
/// under the pointer is often the text or image inside a button.
const PARENT_DEPTH: usize = 3;

/// The control types that tick.
const INTERACTIVE: [UIA_CONTROLTYPE_ID; 11] = [
    UIA_ButtonControlTypeId,
    UIA_SplitButtonControlTypeId,
    UIA_HyperlinkControlTypeId,
    UIA_MenuItemControlTypeId,
    UIA_TabItemControlTypeId,
    UIA_ListItemControlTypeId,
    UIA_CheckBoxControlTypeId,
    UIA_RadioButtonControlTypeId,
    UIA_ComboBoxControlTypeId,
    UIA_TreeItemControlTypeId,
    UIA_DataItemControlTypeId,
];

/// The running watcher; dropping it stops the thread and the route refresh.
pub(crate) struct UiHover {
    stop: Arc<AtomicBool>,
    refresh: tokio::task::JoinHandle<()>,
    player: tokio::task::JoinHandle<()>,
}

impl Drop for UiHover {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.refresh.abort();
        self.player.abort();
    }
}

/// Start the watcher. Must be called from a tokio runtime.
pub(crate) fn spawn(
    orchestrator: Arc<tokio::sync::Mutex<Orchestrator>>,
    shared: SharedHandles,
) -> UiHover {
    let route = Arc::new(Mutex::new(None::<DeviceRoute>));
    let stop = Arc::new(AtomicBool::new(false));
    let refresh = {
        let route = Arc::clone(&route);
        tokio::spawn(async move {
            loop {
                let next = orchestrator.lock().await.ui_hover_haptic_route();
                *route.lock().unwrap_or_else(PoisonError::into_inner) = next;
                tokio::time::sleep(ROUTE_REFRESH).await;
            }
        })
    };
    let (ticks, mut rx) = tokio::sync::watch::channel(None::<DeviceRoute>);
    let player = tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let Some(route) = rx.borrow_and_update().clone() else {
                continue;
            };
            let play = shared
                .device(&route)
                .run(HidppOperation::PlayHaptic, |c| async move {
                    openlogi_hid::play_haptic_without_reply_on(&c, HapticWaveform::SubtleCollision)
                        .await
                });
            match tokio::time::timeout(PLAY_BUDGET, play).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => debug!(%error, "UI hover haptic not sent"),
                Err(_elapsed) => debug!("UI hover haptic ran out of time"),
            }
        }
    });
    let thread_stop = Arc::clone(&stop);
    let spawned = thread::Builder::new()
        .name("openlogi-ui-hover".into())
        .spawn(move || watch(&route, &ticks, &thread_stop));
    if let Err(error) = spawned {
        warn!(%error, "could not start the UI hover watcher — no UI hover haptics");
    }
    UiHover {
        stop,
        refresh,
        player,
    }
}

fn watch(
    route: &Mutex<Option<DeviceRoute>>,
    ticks: &tokio::sync::watch::Sender<Option<DeviceRoute>>,
    stop: &AtomicBool,
) {
    // SAFETY: called once on this thread before any COM use, balanced by
    // `CoUninitialize` below when it succeeded.
    if let Err(error) = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok() {
        warn!(%error, "COM initialization failed — no UI hover haptics");
        return;
    }
    if let Err(error) = run(route, ticks, stop) {
        warn!(%error, "UI Automation unavailable — no UI hover haptics");
    }
    // SAFETY: balances the successful `CoInitializeEx` above; every COM
    // object `run` created has been dropped by now.
    unsafe { CoUninitialize() };
}

fn run(
    route: &Mutex<Option<DeviceRoute>>,
    ticks: &tokio::sync::watch::Sender<Option<DeviceRoute>>,
    stop: &AtomicBool,
) -> windows::core::Result<()> {
    // SAFETY: COM is initialized on this thread; the class and interface are
    // the documented UI Automation client pair.
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }?;
    // SAFETY: plain COM calls on a live `IUIAutomation`.
    let walker = unsafe { automation.ControlViewWalker() }?;
    // SAFETY: as above.
    let cache = unsafe { automation.CreateCacheRequest() }?;
    for property in [UIA_ControlTypePropertyId, UIA_RuntimeIdPropertyId] {
        // SAFETY: plain COM call on a live cache request.
        unsafe { cache.AddProperty(property) }?;
    }

    let mut last_point: Option<(i32, i32)> = None;
    let mut last_element: Option<Vec<i32>> = None;
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(PERIOD);
        let current = route.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let Some(current) = current else {
            last_point = None;
            last_element = None;
            continue;
        };
        let mut point = POINT::default();
        // SAFETY: `point` is a valid out-pointer for the call's duration.
        if unsafe { GetPhysicalCursorPos(&raw mut point) }.is_err()
            || last_point == Some((point.x, point.y))
        {
            continue;
        }
        last_point = Some((point.x, point.y));
        let element = match interactive_under(&automation, &walker, &cache, point) {
            Ok(element) => element,
            Err(error) => {
                // Elements vanish, and elevated windows refuse queries.
                debug!(%error, "UI Automation query failed");
                None
            }
        };
        if element.is_some() && element != last_element {
            let _ = ticks.send(Some(current));
        }
        last_element = element;
    }
    Ok(())
}

/// The runtime id of the interactive element at `point`, searching up to
/// [`PARENT_DEPTH`] ancestors, or `None` when there is none.
fn interactive_under(
    automation: &IUIAutomation,
    walker: &IUIAutomationTreeWalker,
    cache: &IUIAutomationCacheRequest,
    point: POINT,
) -> windows::core::Result<Option<Vec<i32>>> {
    // SAFETY: plain COM call; `point` is in physical screen coordinates as
    // UI Automation expects.
    let mut element: IUIAutomationElement =
        unsafe { automation.ElementFromPointBuildCache(point, cache) }?;
    for depth in 0..=PARENT_DEPTH {
        // SAFETY: the control type was cached by the request above.
        let control_type = unsafe { element.CachedControlType() }?;
        if INTERACTIVE.contains(&control_type) {
            return runtime_id(&element);
        }
        if depth == PARENT_DEPTH {
            break;
        }
        // SAFETY: plain COM call on live objects.
        element = unsafe { walker.GetParentElementBuildCache(&element, cache) }?;
    }
    Ok(None)
}

/// The element's cached runtime id: an array of `i32` identifying it while it
/// lives.
fn runtime_id(element: &IUIAutomationElement) -> windows::core::Result<Option<Vec<i32>>> {
    // SAFETY: the runtime id was cached by the request that found `element`.
    let mut value = unsafe { element.GetCachedPropertyValue(UIA_RuntimeIdPropertyId) }?;
    // SAFETY: the union is read only as the variant type `vt` declares, and
    // an `VT_ARRAY | VT_I4` SAFEARRAY holds `cElements` contiguous `i32`s.
    let ids = unsafe {
        let inner = &value.Anonymous.Anonymous;
        let array = inner.Anonymous.parray;
        if inner.vt == VARENUM(VT_ARRAY.0 | VT_I4.0) && !array.is_null() {
            let len = (*array).rgsabound[0].cElements as usize;
            Some(std::slice::from_raw_parts((*array).pvData.cast::<i32>(), len).to_vec())
        } else {
            None
        }
    };
    // SAFETY: `value` came from UI Automation and is cleared exactly once.
    unsafe { VariantClear(&raw mut value) }?;
    Ok(ids)
}
