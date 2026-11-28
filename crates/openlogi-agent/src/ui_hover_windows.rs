//! UI hover haptics (Windows): a light tick on the active mouse when the
//! pointer moves onto a new interactive UI element — a button, link, menu
//! item, tab, list item, check box and the like — found through UI Automation.
//!
//! A dedicated COM (MTA) thread samples the physical cursor position at a
//! bounded rate and asks UI Automation only after the pointer moved, so a
//! resting pointer costs one `GetPhysicalCursorPos` per period. Ticks go to a
//! latest-wins player of their own, one attempt each, whose acknowledgement is
//! awaited for only [`HOVER_REPLY_TIMEOUT`]: over a busy Bluetooth link replies
//! get dropped while the pointer moves, and the ring player's retries and
//! breaker stalled every later tick for seconds.

#![expect(
    unsafe_code,
    reason = "raw win32: COM UI Automation client calls and the runtime-id VARIANT — localized here"
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use openlogi_agent_core::orchestrator::{Orchestrator, SharedHandles};
use openlogi_hid::{DeviceRoute, HapticWaveform, HidppOperation, WriteError};
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

/// How long a hover play waits for its acknowledgement before the next tick
/// may go. Only the play is bounded by it: waiting for the device and locating
/// the haptic feature on a channel's first play keep their own limits, so a
/// slow link still finds and caches the feature. A reply that misses this is
/// left to the channel, which keeps the request's header reserved until it
/// lands, so it never answers a later play such as a ring confirmation.
const HOVER_REPLY_TIMEOUT: Duration = Duration::from_millis(250);

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

/// How long a device that refused a play for its haptic status is left
/// alone before the next hover re-reads that status.
const STATUS_RETRY: Duration = Duration::from_secs(5);

/// The running watcher; dropping it stops the thread, the route refresh and
/// the player.
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
    let refresh = tokio::spawn(refresh_route(
        move || {
            let orchestrator = Arc::clone(&orchestrator);
            async move { orchestrator.lock().await.ui_hover_haptic_route() }
        },
        Arc::clone(&route),
        ROUTE_REFRESH,
    ));
    let (ticks, rx) = tokio::sync::watch::channel(None::<DeviceRoute>);
    let player = tokio::spawn(play_ticks(rx, move |route, fresh_status| {
        let shared = shared.clone();
        async move {
            shared
                .device(&route)
                .run(HidppOperation::PlayHaptic, move |c| async move {
                    openlogi_hid::play_hover_haptic_on(
                        &c,
                        HapticWaveform::SubtleCollision,
                        fresh_status,
                        HOVER_REPLY_TIMEOUT,
                    )
                    .await
                })
                .await
        }
    }));
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

/// Publish the route `read` returns into `route` now, then once every
/// `period`. The first periodic read is one period out.
async fn refresh_route<F, Fut>(
    mut read: F,
    route: Arc<Mutex<Option<DeviceRoute>>>,
    period: Duration,
) where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<DeviceRoute>>,
{
    *lock(&route) = read().await;
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        *lock(&route) = read().await;
    }
}

/// Play the newest tick, one at a time; ticks queued behind a play collapse
/// into one. `play` sends one hover tick to a route, re-reading the haptic
/// status first when asked. After a
/// status refusal the device is left alone for [`STATUS_RETRY`], then the next
/// tick re-reads the status.
async fn play_ticks<P, Fut>(mut rx: tokio::sync::watch::Receiver<Option<DeviceRoute>>, mut play: P)
where
    P: FnMut(DeviceRoute, bool) -> Fut,
    Fut: Future<Output = Result<(), WriteError>>,
{
    let mut retry_at: Option<tokio::time::Instant> = None;
    while rx.changed().await.is_ok() {
        let Some(route) = rx.borrow_and_update().clone() else {
            continue;
        };
        let fresh_status = match retry_at {
            Some(at) if tokio::time::Instant::now() < at => continue,
            Some(_) => true,
            None => false,
        };
        match play(route, fresh_status).await {
            Ok(()) => retry_at = None,
            Err(WriteError::HapticsUnavailable { status }) => {
                debug!(status, "UI hover haptics refused by the device's status");
                retry_at = Some(tokio::time::Instant::now() + STATUS_RETRY);
            }
            Err(error) => debug!(%error, "UI hover haptic not sent"),
        }
    }
}

fn lock(route: &Mutex<Option<DeviceRoute>>) -> std::sync::MutexGuard<'_, Option<DeviceRoute>> {
    route.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the sampling loop remembers between samples.
#[derive(Debug, Default, PartialEq, Eq)]
enum Tracker {
    /// Nothing sampled since the start or the last reset.
    #[default]
    Unseen,
    /// The last sampled pointer position and the interactive element found
    /// there, if any.
    Sampled {
        point: (i32, i32),
        element: Option<Vec<i32>>,
    },
}

impl Tracker {
    /// Fold one pointer sample in. `lookup` finds the interactive element
    /// under the pointer; it runs only when the pointer moved and a route is
    /// set. Returns the route to tick when the pointer reached a new
    /// interactive element. A lookup the route changed during (setting turned
    /// off, another device selected) is discarded.
    fn step(
        &mut self,
        route: &Mutex<Option<DeviceRoute>>,
        point: (i32, i32),
        lookup: impl FnOnce() -> Option<Vec<i32>>,
    ) -> Option<DeviceRoute> {
        let Some(current) = lock(route).clone() else {
            *self = Self::default();
            return None;
        };
        let previous = match self {
            Self::Sampled {
                point: last,
                element,
            } if *last == point => return None,
            Self::Sampled { element, .. } => element.take(),
            Self::Unseen => None,
        };
        let element = lookup();
        if lock(route).as_ref() != Some(&current) {
            *self = Self::default();
            return None;
        }
        let tick = element.is_some() && element != previous;
        *self = Self::Sampled { point, element };
        tick.then_some(current)
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

    let mut tracker = Tracker::default();
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(PERIOD);
        let mut point = POINT::default();
        // SAFETY: `point` is a valid out-pointer for the call's duration.
        if unsafe { GetPhysicalCursorPos(&raw mut point) }.is_err() {
            continue;
        }
        let tick = tracker.step(route, (point.x, point.y), || {
            interactive_under(&automation, &walker, &cache, point).unwrap_or_else(|error| {
                // Elements vanish, and elevated windows refuse queries.
                debug!(%error, "UI Automation query failed");
                None
            })
        });
        if let Some(current) = tick
            && !stop.load(Ordering::Relaxed)
        {
            let _ = ticks.send(Some(current));
        }
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    fn mouse(product_id: u16) -> DeviceRoute {
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id,
        }
    }

    #[test]
    fn ticks_once_per_new_interactive_element() {
        let route = Mutex::new(Some(mouse(0xb042)));
        let mut tracker = Tracker::default();
        let button = || Some(vec![42, 1]);
        assert_eq!(tracker.step(&route, (1, 1), button), Some(mouse(0xb042)));
        assert_eq!(tracker.step(&route, (2, 1), button), None, "same element");
        assert_eq!(
            tracker.step(&route, (2, 1), || unreachable!()),
            None,
            "no move, no lookup"
        );
        assert_eq!(tracker.step(&route, (3, 1), || None), None, "plain text");
        assert_eq!(
            tracker.step(&route, (4, 1), button),
            Some(mouse(0xb042)),
            "back onto it"
        );
    }

    #[test]
    fn a_reset_or_discarded_lookup_leaves_no_element_behind() {
        let route = Mutex::new(Some(mouse(0xb042)));
        let mut tracker = Tracker::default();
        tracker.step(&route, (1, 1), || Some(vec![42]));
        assert_eq!(
            tracker,
            Tracker::Sampled {
                point: (1, 1),
                element: Some(vec![42])
            }
        );

        *lock(&route) = None;
        assert_eq!(tracker.step(&route, (2, 1), || unreachable!()), None);
        assert_eq!(
            tracker,
            Tracker::Unseen,
            "no route: the sample is forgotten"
        );

        *lock(&route) = Some(mouse(0xb042));
        tracker.step(&route, (3, 1), || {
            *lock(&route) = Some(mouse(0xb034));
            Some(vec![42])
        });
        assert_eq!(tracker, Tracker::Unseen, "a discarded lookup keeps nothing");
    }

    #[test]
    fn route_changes_during_hover_lookup() {
        let route = Mutex::new(Some(mouse(0xb042)));
        let mut tracker = Tracker::default();
        let tick = tracker.step(&route, (1, 1), || {
            // The setting is turned off while UI Automation answers.
            *lock(&route) = None;
            Some(vec![7])
        });
        assert_eq!(tick, None);

        *lock(&route) = Some(mouse(0xb042));
        let tick = tracker.step(&route, (1, 1), || {
            // Another device is selected while UI Automation answers.
            *lock(&route) = Some(mouse(0xb034));
            Some(vec![7])
        });
        assert_eq!(tick, None, "an old lookup does not tick the old mouse");
        assert_eq!(
            tracker.step(&route, (1, 1), || Some(vec![7])),
            Some(mouse(0xb034)),
            "the next sample looks up again for the new route"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn hover_route_refresh_schedule() {
        let reads = Arc::new(AtomicUsize::new(0));
        let route = Arc::new(Mutex::new(None));
        let counted = Arc::clone(&reads);
        let task = tokio::spawn(refresh_route(
            move || {
                counted.fetch_add(1, Ordering::SeqCst);
                async { Some(mouse(0xb042)) }
            },
            Arc::clone(&route),
            ROUTE_REFRESH,
        ));
        tokio::task::yield_now().await;
        assert_eq!(reads.load(Ordering::SeqCst), 1, "the initial snapshot");
        assert_eq!(*lock(&route), Some(mouse(0xb042)));

        tokio::time::advance(ROUTE_REFRESH.saturating_sub(Duration::from_millis(1))).await;
        tokio::task::yield_now().await;
        assert_eq!(
            reads.load(Ordering::SeqCst),
            1,
            "the first periodic read is one period out"
        );

        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(reads.load(Ordering::SeqCst), 2);

        tokio::time::advance(ROUTE_REFRESH).await;
        tokio::task::yield_now().await;
        assert_eq!(reads.load(Ordering::SeqCst), 3);
        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn a_status_refusal_rereads_the_status_after_the_retry_delay() {
        let (ticks, rx) = tokio::sync::watch::channel(None);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&calls);
        let player = tokio::spawn(play_ticks(rx, move |_, fresh_status| {
            let mut seen = seen.lock().expect("calls");
            seen.push(fresh_status);
            let refused = seen.len() == 1;
            async move {
                if refused {
                    Err(WriteError::HapticsUnavailable { status: 2 })
                } else {
                    Ok(())
                }
            }
        }));
        let tick = || async {
            ticks.send(Some(mouse(0xb042))).expect("player listens");
            tokio::task::yield_now().await;
        };

        tick().await;
        assert_eq!(*calls.lock().expect("calls"), vec![false]);
        tick().await;
        assert_eq!(
            *calls.lock().expect("calls"),
            vec![false],
            "a refused device is left alone until the retry delay passes"
        );
        tokio::time::advance(STATUS_RETRY).await;
        tick().await;
        assert_eq!(
            *calls.lock().expect("calls"),
            vec![false, true],
            "then the status is re-read"
        );
        tick().await;
        assert_eq!(
            *calls.lock().expect("calls"),
            vec![false, true, false],
            "and plays resume"
        );
        player.abort();
    }
}
