//! Quiet, informational Windows app notifications from the background agent.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use openlogi_agent_core::dpi_notifications::{DpiNotice, DpiNotifications};
use openlogi_agent_core::orchestrator::Orchestrator;
use openlogi_core::brand::APP_ID;
use openlogi_hid::{DIRECT_DEVICE_INDEX, DeviceRoute, Dpi};
use rust_i18n::t;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{Mutex, broadcast};
use tracing::{debug, warn};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::{DateTime, IReference, PropertyValue};
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager, ToastNotifier};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
use windows::core::{HSTRING, Interface as _, Result as WinResult};

const TOAST_LIFETIME: Duration = Duration::from_secs(8);
const WINDOWS_EPOCH_TICKS: i64 = 116_444_736_000_000_000;

pub(super) fn start(
    notifications: &Arc<DpiNotifications>,
    orchestrator: &Arc<Mutex<Orchestrator>>,
) {
    let mut changes = notifications.subscribe();
    let notifications = Arc::downgrade(notifications);
    let orchestrator = Arc::downgrade(orchestrator);
    // Subscribe at startup so enabling the preference later works. Wait on the
    // agent runtime until there is work for a native thread.
    tokio::spawn(async move {
        let first = loop {
            match changes.recv().await {
                Ok(change) => {
                    let Some(source) = notifications.upgrade() else {
                        return;
                    };
                    if should_show(&source, &change) {
                        break change;
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return,
            }
        };
        if let Err(error) = std::thread::Builder::new()
            .name("dpi-notifications".into())
            .spawn(move || run_worker(first, changes, &notifications, &orchestrator))
        {
            warn!(%error, "could not start Windows DPI notifications");
        }
    });
}

fn run_worker(
    first: DpiNotice,
    mut pending: broadcast::Receiver<DpiNotice>,
    notifications: &Weak<DpiNotifications>,
    orchestrator: &Weak<Mutex<Orchestrator>>,
) {
    let mut state = ToastState::Uninitialized;
    let remaining = std::iter::from_fn(|| {
        loop {
            match pending.blocking_recv() {
                Ok(change) => return Some(change),
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return None,
            }
        }
    });
    for change in std::iter::once(first).chain(remaining) {
        let Some(notifications) = notifications.upgrade() else {
            break;
        };
        if !should_show(&notifications, &change) {
            continue;
        }
        let Some(orchestrator) = orchestrator.upgrade() else {
            break;
        };
        let name = {
            let orchestrator = orchestrator.blocking_lock();
            orchestrator.dpi_notification_label(&change.route, |slot| {
                if change.route.device_index() == DIRECT_DEVICE_INDEX {
                    t!("device.mouse").into_owned()
                } else {
                    t!("device.receiver_slot_number", number => slot).into_owned()
                }
            })
        };
        let Some(name) = name else {
            continue;
        };
        let (next, result) = state.deliver(
            Apartment::initialize,
            |_| ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID)),
            |notifier| show(notifier, &notifications, &change, &name),
        );
        state = next;
        if let Err(error) = result {
            if matches!(state, ToastState::Uninitialized) {
                warn!(%error, "could not initialize Windows DPI notifications");
            } else {
                debug!(%error, "could not show Windows DPI notification");
            }
        }
    }
}

enum ToastState<A, N> {
    Uninitialized,
    Initialized(A),
    Ready(ReadyToast<A, N>),
}

// Keep the apartment alive for cached WinRT factories. Field order releases
// the notifier before uninitializing the apartment, including on unwind.
struct ReadyToast<A, N> {
    notifier: N,
    apartment: A,
}

impl<A, N> ToastState<A, N> {
    fn deliver<E>(
        self,
        initialize: impl FnOnce() -> Result<A, E>,
        create: impl FnOnce(&A) -> Result<N, E>,
        send: impl FnOnce(&N) -> Result<(), E>,
    ) -> (Self, Result<(), E>) {
        let (notifier, apartment) = match self {
            Self::Ready(ReadyToast {
                notifier,
                apartment,
            }) => (Some(notifier), apartment),
            Self::Initialized(apartment) => (None, apartment),
            Self::Uninitialized => match initialize() {
                Ok(apartment) => (None, apartment),
                Err(error) => return (Self::Uninitialized, Err(error)),
            },
        };
        let notifier = match notifier {
            Some(notifier) => notifier,
            None => match create(&apartment) {
                Ok(notifier) => notifier,
                Err(error) => return (Self::Initialized(apartment), Err(error)),
            },
        };
        let ready = ReadyToast {
            notifier,
            apartment,
        };
        if let Err(error) = send(&ready.notifier) {
            let ReadyToast {
                notifier,
                apartment,
            } = ready;
            drop(notifier);
            (Self::Initialized(apartment), Err(error))
        } else {
            (Self::Ready(ready), Ok(()))
        }
    }
}

fn should_show(notifications: &DpiNotifications, change: &DpiNotice) -> bool {
    notifications.enabled() && change.is_current(Instant::now())
}

fn show(
    notifier: &ToastNotifier,
    notifications: &DpiNotifications,
    change: &DpiNotice,
    name: &str,
) -> WinResult<()> {
    let toast = build_toast(&change.route, name, change.dpi)?;
    if should_show(notifications, change) {
        // Setting() can return ERROR_NOT_FOUND before the first Show().
        // Windows applies its notification and Do Not Disturb settings.
        notifier.Show(&toast)?;
    }
    Ok(())
}

fn build_toast(route: &DeviceRoute, name: &str, dpi: Dpi) -> WinResult<ToastNotification> {
    let xml = XmlDocument::new()?;
    xml.LoadXml(&HSTRING::from(
        r#"<toast><visual><binding template="ToastGeneric"><text/><text/></binding></visual><audio silent="true"/></toast>"#
    ))?;
    let text = xml.GetElementsByTagName(&HSTRING::from("text"))?;
    text.Item(0)?.SetInnerText(&HSTRING::from(name))?;
    text.Item(1)?
        .SetInnerText(&HSTRING::from(format!("{dpi} DPI")))?;

    let toast = ToastNotification::CreateToastNotification(&xml)?;
    let mut hasher = DefaultHasher::new();
    route.hash(&mut hasher);
    toast.SetTag(&HSTRING::from(format!("{:016x}", hasher.finish())))?;
    toast.SetGroup(&HSTRING::from("dpi"))?;
    let valid_until = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        + TOAST_LIFETIME;
    let ticks = i64::try_from(valid_until.as_nanos() / 100).unwrap_or(i64::MAX);
    let expiry: IReference<DateTime> = PropertyValue::CreateDateTime(DateTime {
        UniversalTime: WINDOWS_EPOCH_TICKS.saturating_add(ticks),
    })?
    .cast()?;
    toast.SetExpirationTime(&expiry)?;
    Ok(toast)
}

// Kept on the thread that initialized WinRT and dropped after all WinRT objects.
struct Apartment(std::marker::PhantomData<std::rc::Rc<()>>);

impl Apartment {
    #[expect(unsafe_code, reason = "initialize WinRT on the current thread")]
    fn initialize() -> WinResult<Self> {
        // SAFETY: the notification worker uses the MTA and balances each
        // successful initialization (including S_FALSE) on the same thread.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }?;
        Ok(Self(std::marker::PhantomData))
    }
}

impl Drop for Apartment {
    #[expect(unsafe_code, reason = "balance this thread's WinRT initialization")]
    fn drop(&mut self) {
        // SAFETY: this !Send guard exists only after a successful initialization.
        unsafe { RoUninitialize() };
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn dpi_notifier_requires_a_successful_apartment_initialization() {
        let state: ToastState<(), ()> = ToastState::Uninitialized;
        let (state, result) = state.deliver(
            || Err("initialization failed"),
            |()| panic!("cannot create a notifier before initializing the apartment"),
            |()| panic!("cannot send without a notifier"),
        );
        assert_eq!(result, Err("initialization failed"));
        assert!(matches!(state, ToastState::Uninitialized));
        let (state, result) = state.deliver(
            || Ok(()),
            |()| Err("creation failed"),
            |()| panic!("cannot send without a notifier"),
        );
        assert_eq!(result, Err("creation failed"));
        assert!(matches!(state, ToastState::Initialized(())));
    }

    #[test]
    fn dpi_notifier_retry_and_shutdown_keep_the_apartment_alive() {
        struct OnDrop<'a>(&'a RefCell<Vec<&'static str>>, &'static str);

        impl Drop for OnDrop<'_> {
            fn drop(&mut self) {
                self.0.borrow_mut().push(self.1);
            }
        }

        let events = RefCell::new(Vec::new());
        let initialize_again = || panic!("must reuse the initialized apartment");
        let create = |_: &OnDrop<'_>| Ok(OnDrop(&events, "notifier dropped"));
        let state = ToastState::Initialized(OnDrop(&events, "apartment dropped"));
        let (state, result) = state.deliver(initialize_again, create, |_| Err("send failed"));
        assert_eq!(result, Err("send failed"));
        assert_eq!(*events.borrow(), ["notifier dropped"]);

        let (state, result) = state.deliver(initialize_again, create, |_| Ok::<_, &str>(()));
        result.unwrap();
        let (state, result) = state.deliver(
            initialize_again,
            |_| panic!("must reuse the ready notifier"),
            |_| Ok::<_, &str>(()),
        );
        result.unwrap();
        drop(state);
        assert_eq!(
            *events.borrow(),
            ["notifier dropped", "notifier dropped", "apartment dropped"]
        );
    }

    #[test]
    fn windows_dpi_toast_contains_safe_text_and_reuses_the_mouse_tag() -> WinResult<()> {
        let _apartment = Apartment::initialize()?;
        let route = DeviceRoute::Bolt {
            receiver_uid: "test".into(),
            slot: 1,
        };
        let other = DeviceRoute::Bolt {
            receiver_uid: "test".into(),
            slot: 2,
        };
        let first = build_toast(&route, "Mouse <A> & B", Dpi::new(800))?;
        let second = build_toast(&route, "Mouse <A> & B", Dpi::new(1600))?;
        let other = build_toast(&other, "Mouse <A> & B", Dpi::new(2400))?;
        assert_eq!(first.Tag()?, second.Tag()?);
        assert_ne!(first.Tag()?, other.Tag()?);
        assert_eq!(first.Tag()?.len(), 16);
        assert_eq!(first.Group()?, second.Group()?);
        let content = second.Content()?;
        let text = content.GetElementsByTagName(&HSTRING::from("text"))?;
        assert_eq!(text.Item(0)?.InnerText()?, HSTRING::from("Mouse <A> & B"));
        assert_eq!(text.Item(1)?.InnerText()?, HSTRING::from("1600 DPI"));
        let audio = content.SelectSingleNode(&HSTRING::from("/toast/audio/@silent"))?;
        assert_eq!(audio.InnerText()?, HSTRING::from("true"));
        assert!(second.ExpirationTime()?.Value()?.UniversalTime > WINDOWS_EPOCH_TICKS);
        Ok(())
    }

    #[test]
    fn windows_dpi_toast_uses_the_installed_shortcut_identity() {
        let msi = include_str!("../../../packaging/windows/OpenLogi.wxs");
        assert!(msi.contains(&format!(
            r#"<ShortcutProperty Key="System.AppUserModel.ID" Value="{APP_ID}" />"#
        )));
    }
}
