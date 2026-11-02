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
