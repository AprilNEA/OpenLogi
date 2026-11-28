/// Serializes tests that read or mutate `rust_i18n`'s process-global locale.
pub(crate) static LOCALE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct LocaleGuard<'a> {
    _lock: std::sync::MutexGuard<'a, ()>,
}

impl LocaleGuard<'_> {
    fn set(locale: &str) -> Self {
        let lock = LOCALE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        rust_i18n::set_locale(locale);
        Self { _lock: lock }
    }
}

impl Drop for LocaleGuard<'_> {
    fn drop(&mut self) {
        rust_i18n::set_locale("en");
    }
}

/// The catalog this binary translates against lives in `openlogi-ui` and is
/// reached by the relative path in the `i18n!` in `main.rs`. A wrong path does
/// **not** fail the build: `rust_i18n` compiles it to an empty catalog, and
/// every ring label renders as its semantic key. Resolve one action key so that
/// breakage is loud without pinning any translation wording.
#[test]
fn the_shared_catalog_is_wired_up() {
    const KEY: &str = "actions.left_click";
    assert_ne!(rust_i18n::t!(KEY), KEY);
}

/// A notice names a device that has no name by its kind. A kind whose key is
/// missing from the catalog would show the raw key on screen, so resolve each
/// one. The other locales are held to the same keys by `catalog_parity`.
#[test]
fn every_device_kind_has_a_name_to_fall_back_to() {
    use openlogi_core::device::DeviceKind;

    const KINDS: [DeviceKind; 14] = [
        DeviceKind::Mouse,
        DeviceKind::Keyboard,
        DeviceKind::Numpad,
        DeviceKind::Presenter,
        DeviceKind::Remote,
        DeviceKind::Trackball,
        DeviceKind::Touchpad,
        DeviceKind::Tablet,
        DeviceKind::Gamepad,
        DeviceKind::Joystick,
        DeviceKind::Headset,
        DeviceKind::Camera,
        DeviceKind::Unknown,
        DeviceKind::Light,
    ];
    // Exhaustive on purpose: a new kind stops this compiling until it is
    // listed above.
    let _: fn(DeviceKind) = |kind| match kind {
        DeviceKind::Mouse
        | DeviceKind::Keyboard
        | DeviceKind::Numpad
        | DeviceKind::Presenter
        | DeviceKind::Remote
        | DeviceKind::Trackball
        | DeviceKind::Touchpad
        | DeviceKind::Tablet
        | DeviceKind::Gamepad
        | DeviceKind::Joystick
        | DeviceKind::Headset
        | DeviceKind::Camera
        | DeviceKind::Unknown
        | DeviceKind::Light => {}
    };
    for kind in KINDS {
        let key = kind.translation_key();
        assert_ne!(
            rust_i18n::t!(key, locale = "en"),
            key,
            "{kind:?} has no name"
        );
    }
}

#[test]
fn overlay_window_titles_keep_brand_and_translate_in_every_locale() {
    const KEYS: [&str; 4] = [
        "overlay.actions_ring_window_title",
        "overlay.backlight_window_title",
        "overlay.lock_keys_window_title",
        "overlay.notice_window_title",
    ];
    for (code, _) in openlogi_core::locale::SUPPORTED {
        for key in KEYS {
            let localized = rust_i18n::t!(key, locale = code);
            assert_ne!(localized, key, "{code} is missing {key}");
            assert!(
                localized.contains("OpenLogi"),
                "{code} {key} dropped brand name: {localized}"
            );
            if *code != "en" {
                let english = rust_i18n::t!(key, locale = "en");
                assert_ne!(
                    localized, english,
                    "{code} {key} stayed untranslated: {localized}"
                );
            }
        }
    }
}

fn close_all_windows(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        for handle in cx.windows() {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    });
    cx.run_until_parked();
}

fn test_invocation() -> openlogi_ipc::ActionRingInvocation {
    openlogi_ipc::ActionRingInvocation {
        session_id: 7,
        slots: [(
            openlogi_core::binding::ActionRingSlot::Top,
            openlogi_ipc::ActionRingPresentation {
                label: "Copy".to_string(),
                literal: true,
                icon: openlogi_core::binding::ActionRingIcon::Copy,
            },
        )]
        .into(),
        language: None,
        size: openlogi_core::binding::ActionRingSize::Medium,
    }
}

fn open_window_title(
    cx: &mut gpui::TestAppContext,
    handle: gpui::AnyWindowHandle,
) -> Option<String> {
    gpui::VisualTestContext::from_window(handle, cx).window_title()
}

fn open_all_four_and_read_titles(
    cx: &mut gpui::TestAppContext,
    backlight: &openlogi_ipc::BacklightObservation,
    lock_change: openlogi_ipc::LockKeyChange,
    notice_cards: &[openlogi_ipc::Notice],
) -> (
    [Option<String>; 4],
    gpui::AnyWindowHandle,
    gpui::AnyWindowHandle,
) {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    let live_session = Arc::new(crate::session::ClickAwaySession::new());
    let (commands, _rx) = tokio::sync::mpsc::unbounded_channel();
    let invocation = test_invocation();
    let shelf = Rc::new(RefCell::new(crate::notice::NoticeShelf::default()));

    let ring_handle = cx.update(|cx| {
        crate::ring::open_windows(
            cx,
            vec![vec![(
                crate::ring::ring_window_options(),
                crate::ring::Placement::Window,
            )]],
            &invocation,
            &commands,
            &live_session,
        )
        .expect("ring window should open")
    });
    cx.update(|cx| crate::backlight::present(cx, backlight));
    cx.update(|cx| crate::lock_key::present(cx, Some(lock_change)));
    let notice_handle = cx.update(|cx| {
        crate::notice::open_window(
            cx,
            vec![crate::notice::notice_window_options()],
            &shelf,
            notice_cards,
        )
        .expect("notice window should open")
    });

    let backlight_handle = cx.update(|cx| {
        cx.windows()
            .into_iter()
            .find(|h| h.downcast::<crate::backlight::BacklightView>().is_some())
            .expect("backlight window should be open")
    });
    let lock_handle = cx.update(|cx| {
        cx.windows()
            .into_iter()
            .find(|h| h.downcast::<crate::lock_key::LockKeyView>().is_some())
            .expect("lock-key window should be open")
    });

    let titles = [
        open_window_title(cx, ring_handle.into()),
        open_window_title(cx, backlight_handle),
        open_window_title(cx, lock_handle),
        open_window_title(cx, notice_handle.into()),
    ];
    (titles, backlight_handle, lock_handle)
}

#[gpui::test]
fn all_four_overlay_windows_are_named_in_the_active_language(cx: &mut gpui::TestAppContext) {
    use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus, DeviceKind, LockKey};
    use openlogi_ipc::{BacklightObservation, LockKeyChange, Notice, NoticeKind};

    let backlight = BacklightObservation {
        generation: 1,
        current_level: 3,
        levels: 7,
        visible: true,
    };
    let lock_change = LockKeyChange {
        key: LockKey::CapsLock,
        engaged: true,
    };
    let notice_cards = [Notice {
        id: 1,
        kind: NoticeKind::LowBattery {
            name: Some("MX Master 3S".to_string()),
            kind: DeviceKind::Mouse,
            battery: BatteryInfo {
                percentage: Some(15),
                level: BatteryLevel::Low,
                status: BatteryStatus::Discharging,
            },
        },
    }];

    for (code, ring_expected, backlight_expected, lock_expected, notice_expected) in [
        (
            "en",
            "OpenLogi Actions Ring",
            "OpenLogi Backlight",
            "OpenLogi Lock Keys",
            "OpenLogi Notice",
        ),
        (
            "zh-CN",
            "OpenLogi 操作环",
            "OpenLogi 背光",
            "OpenLogi 锁定键",
            "OpenLogi 提示",
        ),
        (
            "de",
            "OpenLogi Aktionsring",
            "OpenLogi Hintergrundbeleuchtung",
            "OpenLogi Sperrtasten",
            "OpenLogi Hinweis",
        ),
    ] {
        let _locale = LocaleGuard::set(code);
        let (titles, backlight_handle, lock_handle) =
            open_all_four_and_read_titles(cx, &backlight, lock_change, &notice_cards);

        assert_eq!(
            titles,
            [
                Some(ring_expected.to_string()),
                Some(backlight_expected.to_string()),
                Some(lock_expected.to_string()),
                Some(notice_expected.to_string()),
            ],
            "wrong window titles for locale {code}"
        );

        // Verify live re-titling when updating already-open indicator windows
        // after switching locale.
        if code == "en" {
            rust_i18n::set_locale("zh-CN");
            cx.update(|cx| crate::backlight::present(cx, &backlight));
            cx.update(|cx| crate::lock_key::present(cx, Some(lock_change)));
            assert_eq!(
                open_window_title(cx, backlight_handle),
                Some("OpenLogi 背光".to_string())
            );
            assert_eq!(
                open_window_title(cx, lock_handle),
                Some("OpenLogi 锁定键".to_string())
            );
        }

        close_all_windows(cx);
    }
}

#[gpui::test]
fn multi_display_ring_opening_names_every_window(cx: &mut gpui::TestAppContext) {
    use std::sync::Arc;

    let _locale = LocaleGuard::set("en");
    let live_session = Arc::new(crate::session::ClickAwaySession::new());
    let (commands, _rx) = tokio::sync::mpsc::unbounded_channel();
    let invocation = test_invocation();

    cx.update(|cx| {
        crate::ring::open_windows(
            cx,
            vec![vec![
                (
                    crate::ring::ring_window_options(),
                    crate::ring::Placement::AtPointer,
                ),
                (
                    crate::ring::ring_window_options(),
                    crate::ring::Placement::AtPointer,
                ),
            ]],
            &invocation,
            &commands,
            &live_session,
        )
        .expect("primary ring window should open");
    });

    let ring_handles: Vec<_> = cx.update(|cx| {
        cx.windows()
            .into_iter()
            .filter(|h| h.downcast::<crate::ring::RingView>().is_some())
            .collect()
    });
    assert_eq!(ring_handles.len(), 2);
    let titles: Vec<_> = ring_handles
        .into_iter()
        .map(|h| open_window_title(cx, h))
        .collect();
    assert_eq!(
        titles,
        vec![
            Some("OpenLogi Actions Ring".to_string()),
            Some("OpenLogi Actions Ring".to_string()),
        ]
    );

    close_all_windows(cx);
}
