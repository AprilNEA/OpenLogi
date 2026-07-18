//! Linux helpers for synthesising OS-level input events via a shared `uinput`
//! virtual device.
//!
//! The device is created lazily on first use. If `/dev/uinput` is inaccessible
//! (missing group membership or udev rule) every call logs a `warn` and returns
//! without panicking.

use std::io;
use std::sync::{LazyLock, Mutex};

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode};
use zbus::blocking::Connection as DbusConn;

use openlogi_core::binding::Action;

/// Linux implementation: inject events via a shared `uinput` virtual device.
pub(super) fn execute(action: &Action) {
    let ctrl = KeyCode::KEY_LEFTCTRL;
    let shift = KeyCode::KEY_LEFTSHIFT;
    let alt = KeyCode::KEY_LEFTALT;
    match action {
        // ── Mouse clicks ──────────────────────────────────────────────────
        Action::LeftClick => click(KeyCode::BTN_LEFT),
        Action::RightClick => click(KeyCode::BTN_RIGHT),
        Action::MiddleClick => click(KeyCode::BTN_MIDDLE),
        // Extra mouse buttons: BTN_SIDE/BTN_EXTRA are the evdev side
        // buttons ("back"/"forward") browsers handle natively.
        Action::MouseBack => click(KeyCode::BTN_SIDE),
        Action::MouseForward => click(KeyCode::BTN_EXTRA),
        // ── Editing ───────────────────────────────────────────────────────
        Action::Copy => press_key(&[ctrl], KeyCode::KEY_C),
        Action::Paste => press_key(&[ctrl], KeyCode::KEY_V),
        Action::Cut => press_key(&[ctrl], KeyCode::KEY_X),
        Action::Undo => press_key(&[ctrl], KeyCode::KEY_Z),
        // Redo is Ctrl+Shift+Z on Linux (matches macOS ⌘⇧Z convention).
        Action::Redo => press_key(&[ctrl, shift], KeyCode::KEY_Z),
        Action::SelectAll => press_key(&[ctrl], KeyCode::KEY_A),
        Action::Find => press_key(&[ctrl], KeyCode::KEY_F),
        Action::Save => press_key(&[ctrl], KeyCode::KEY_S),
        // ── Browser / Navigation ──────────────────────────────────────────
        Action::BrowserBack => press_key(&[alt], KeyCode::KEY_LEFT),
        Action::BrowserForward => press_key(&[alt], KeyCode::KEY_RIGHT),
        Action::NewTab => press_key(&[ctrl], KeyCode::KEY_T),
        Action::CloseTab => press_key(&[ctrl], KeyCode::KEY_W),
        Action::ReopenTab => press_key(&[ctrl, shift], KeyCode::KEY_T),
        Action::NextTab => press_key(&[ctrl], KeyCode::KEY_TAB),
        Action::PrevTab => press_key(&[ctrl, shift], KeyCode::KEY_TAB),
        Action::ReloadPage => press_key(&[ctrl], KeyCode::KEY_R),
        // ── Navigation — macOS-specific ───────────────────────────────────
        // No universal Linux equivalent; the compositor shortcut varies.
        Action::MissionControl
        | Action::AppExpose
        | Action::ShowDesktop
        | Action::LaunchpadShow => {
            tracing::debug!(
                action = action.label(),
                "no Linux equivalent — action skipped"
            );
        }
        // Ctrl+Alt+←/→ is the default in GNOME and KDE.
        Action::PreviousDesktop => press_key(&[ctrl, alt], KeyCode::KEY_LEFT),
        Action::NextDesktop => press_key(&[ctrl, alt], KeyCode::KEY_RIGHT),
        // ── System ────────────────────────────────────────────────────────
        // logind LockSessions() via the system bus; falls back to Super+L.
        Action::LockScreen => lock_screen(),
        // Region vs full-screen capture depends on the desktop environment's
        // screenshot handler for Print Screen, so both map to the same key.
        Action::Screenshot | Action::CaptureRegion => press_key(&[], KeyCode::KEY_SYSRQ),
        // ── Media ─────────────────────────────────────────────────────────
        // MPRIS targets the running media player; XF86 volume keys go to the
        // system mixer (PulseAudio/PipeWire) which is what users expect.
        Action::PlayPause => mpris_command("PlayPause"),
        Action::NextTrack => mpris_command("Next"),
        Action::PrevTrack => mpris_command("Previous"),
        Action::VolumeUp => press_key(&[], KeyCode::KEY_VOLUMEUP),
        Action::VolumeDown => press_key(&[], KeyCode::KEY_VOLUMEDOWN),
        Action::MuteVolume => press_key(&[], KeyCode::KEY_MUTE),
        // ── DPI / SmartShift: handled at hook/HID layer ───────────────────
        Action::CycleDpiPresets | Action::SetDpiPreset(_) | Action::ToggleSmartShift => {
            tracing::debug!(
                action = action.label(),
                "device action handled by hook/HID layer"
            );
        }
        // ── Scroll ────────────────────────────────────────────────────────
        Action::ScrollUp => scroll(RelativeAxisCode::REL_WHEEL, 3),
        Action::ScrollDown => scroll(RelativeAxisCode::REL_WHEEL, -3),
        Action::HorizontalScrollLeft => scroll(RelativeAxisCode::REL_HWHEEL, -3),
        Action::HorizontalScrollRight => scroll(RelativeAxisCode::REL_HWHEEL, 3),
        // ── No-op ─────────────────────────────────────────────────────────
        Action::None => {}
        // ── Custom shortcut ───────────────────────────────────────────────
        Action::CustomShortcut(combo) => {
            if combo.key_code == 0 {
                tracing::warn!(
                    chord = %combo.rendered_label(),
                    "CustomShortcut with no key code — press ignored"
                );
                return;
            }
            let Some(key) = macos_vk_to_linux(combo.key_code) else {
                tracing::warn!(
                    key_code = combo.key_code,
                    "CustomShortcut key code has no Linux mapping — press ignored"
                );
                return;
            };
            press_key(&modifiers_to_keycodes(combo.modifiers), key);
        }
    }
}

const DEVICE_NAME: &str = "OpenLogi action injector";

static VIRTUAL_INPUT: LazyLock<Option<Mutex<VirtualDevice>>> = LazyLock::new(|| {
    build()
        .map(Mutex::new)
        .map_err(|e| tracing::warn!("failed to create uinput action device: {e}"))
        .ok()
});

#[rustfmt::skip]
const KEY_CAPABILITIES: &[KeyCode] = &[
    // Letters
    KeyCode::KEY_A, KeyCode::KEY_B, KeyCode::KEY_C, KeyCode::KEY_D,
    KeyCode::KEY_E, KeyCode::KEY_F, KeyCode::KEY_G, KeyCode::KEY_H,
    KeyCode::KEY_I, KeyCode::KEY_J, KeyCode::KEY_K, KeyCode::KEY_L,
    KeyCode::KEY_M, KeyCode::KEY_N, KeyCode::KEY_O, KeyCode::KEY_P,
    KeyCode::KEY_Q, KeyCode::KEY_R, KeyCode::KEY_S, KeyCode::KEY_T,
    KeyCode::KEY_U, KeyCode::KEY_V, KeyCode::KEY_W, KeyCode::KEY_X,
    KeyCode::KEY_Y, KeyCode::KEY_Z,
    // Digits
    KeyCode::KEY_0, KeyCode::KEY_1, KeyCode::KEY_2, KeyCode::KEY_3,
    KeyCode::KEY_4, KeyCode::KEY_5, KeyCode::KEY_6, KeyCode::KEY_7,
    KeyCode::KEY_8, KeyCode::KEY_9,
    // Punctuation / symbols
    KeyCode::KEY_MINUS,      KeyCode::KEY_EQUAL,   KeyCode::KEY_LEFTBRACE,
    KeyCode::KEY_RIGHTBRACE, KeyCode::KEY_BACKSLASH, KeyCode::KEY_SEMICOLON,
    KeyCode::KEY_APOSTROPHE, KeyCode::KEY_GRAVE,   KeyCode::KEY_COMMA,
    KeyCode::KEY_DOT,        KeyCode::KEY_SLASH,
    // Navigation / editing
    KeyCode::KEY_LEFT,  KeyCode::KEY_RIGHT, KeyCode::KEY_UP,       KeyCode::KEY_DOWN,
    KeyCode::KEY_HOME,  KeyCode::KEY_END,   KeyCode::KEY_PAGEUP,   KeyCode::KEY_PAGEDOWN,
    KeyCode::KEY_TAB,   KeyCode::KEY_ENTER, KeyCode::KEY_BACKSPACE, KeyCode::KEY_DELETE,
    KeyCode::KEY_ESC,   KeyCode::KEY_SPACE,
    // Modifiers (KEY_LEFTMETA used by the LockScreen Super+L fallback)
    KeyCode::KEY_LEFTCTRL, KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_LEFTALT, KeyCode::KEY_LEFTMETA,
    // Function keys
    KeyCode::KEY_F1,  KeyCode::KEY_F2,  KeyCode::KEY_F3,  KeyCode::KEY_F4,
    KeyCode::KEY_F5,  KeyCode::KEY_F6,  KeyCode::KEY_F7,  KeyCode::KEY_F8,
    KeyCode::KEY_F9,  KeyCode::KEY_F10, KeyCode::KEY_F11, KeyCode::KEY_F12,
    // System
    KeyCode::KEY_SYSRQ,
    // Multimedia
    KeyCode::KEY_PLAYPAUSE, KeyCode::KEY_NEXTSONG, KeyCode::KEY_PREVIOUSSONG,
    KeyCode::KEY_VOLUMEUP,  KeyCode::KEY_VOLUMEDOWN, KeyCode::KEY_MUTE,
    // Mouse buttons (injected as EV_KEY with BTN_* codes). The side pair
    // must be registered here or the kernel silently drops their events.
    KeyCode::BTN_LEFT, KeyCode::BTN_RIGHT, KeyCode::BTN_MIDDLE,
    KeyCode::BTN_SIDE, KeyCode::BTN_EXTRA,
];

fn build() -> io::Result<VirtualDevice> {
    let mut keys = AttributeSet::<KeyCode>::default();
    for &k in KEY_CAPABILITIES {
        keys.insert(k);
    }

    // Only scroll axes: the device never emits cursor movement, so leaving
    // out REL_X/REL_Y keeps libinput from classifying it as a pointer —
    // which can otherwise cause injected key/wheel events to be grabbed by
    // pointer-grabbing X11 clients or routed oddly by some Wayland compositors.
    let mut axes = AttributeSet::<RelativeAxisCode>::default();
    for a in [RelativeAxisCode::REL_WHEEL, RelativeAxisCode::REL_HWHEEL] {
        axes.insert(a);
    }

    VirtualDevice::builder()?
        .name(DEVICE_NAME)
        .with_keys(&keys)?
        .with_relative_axes(&axes)?
        .build()
}

fn emit(events: &[InputEvent]) {
    if let Some(m) = &*VIRTUAL_INPUT {
        if let Ok(mut guard) = m.lock() {
            if let Err(e) = guard.emit(events) {
                tracing::warn!("uinput action emit failed: {e}");
            }
        } else {
            tracing::warn!("uinput action device mutex poisoned");
        }
    } else {
        // Device creation failed at init; already logged once in LazyLock.
        tracing::debug!("uinput action device unavailable — action skipped");
    }
}

fn syn() -> InputEvent {
    InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0)
}

fn key_ev(code: KeyCode, value: i32) -> InputEvent {
    InputEvent::new(EventType::KEY.0, code.0, value)
}

fn rel_ev(axis: RelativeAxisCode, value: i32) -> InputEvent {
    InputEvent::new(EventType::RELATIVE.0, axis.0, value)
}

/// Inject modifier-down + key-down in one SYN frame, then key-up +
/// modifier-up in a second SYN frame.
///
/// Two separate frames give the kernel distinct timestamps for press and
/// release, which matches what the kernel `uinput` docs show and avoids
/// toolkits treating a zero-duration event as invalid.
fn press_key(mods: &[KeyCode], key: KeyCode) {
    // Down phase.
    let mut down: Vec<InputEvent> = Vec::with_capacity(mods.len() + 2);
    for &m in mods {
        down.push(key_ev(m, 1));
    }
    down.push(key_ev(key, 1));
    down.push(syn());
    emit(&down);

    // Up phase.
    let mut up: Vec<InputEvent> = Vec::with_capacity(mods.len() + 2);
    up.push(key_ev(key, 0));
    for &m in mods.iter().rev() {
        up.push(key_ev(m, 0));
    }
    up.push(syn());
    emit(&up);
}

/// Inject a button-down in one SYN frame and button-up in a second.
fn click(button: KeyCode) {
    emit(&[key_ev(button, 1), syn()]);
    emit(&[key_ev(button, 0), syn()]);
}

/// Inject a single relative-axis delta followed by `SYN_REPORT`.
pub(super) fn scroll(axis: RelativeAxisCode, value: i32) {
    emit(&[rel_ev(axis, value), syn()]);
}

/// Force the virtual device to initialise (if it hasn't already) and return
/// its `/dev/input/eventN` node path.
///
/// Uses `VirtualDevice::enumerate_dev_nodes()` which returns the correct
/// `/dev/input/eventN` path directly. Returns `None` if the device couldn't
/// be created or if the node hasn't appeared yet (udev typically creates it
/// within a few milliseconds of the `ioctl`).
pub(super) fn device_node() -> Option<std::path::PathBuf> {
    // Touch the LazyLock to force initialisation.
    let _ = &*VIRTUAL_INPUT;
    // Give udev a moment to create the /dev node.
    std::thread::sleep(std::time::Duration::from_millis(150));
    if let Some(m) = &*VIRTUAL_INPUT
        && let Ok(mut guard) = m.lock()
    {
        return guard.enumerate_dev_nodes_blocking().ok()?.flatten().next();
    }
    None
}

/// Convert a [`KeyCombo`](openlogi_core::binding::KeyCombo) modifier bitmask
/// to the evdev keys to hold.
///
/// macOS Cmd (`MOD_CMD`) and Ctrl (`MOD_CTRL`) both map to `KEY_LEFTCTRL`;
/// the bitwise-OR check deduplicates them so at most one Ctrl is pushed.
/// Order is canonical: Ctrl → Shift → Alt.
fn modifiers_to_keycodes(modifiers: u8) -> Vec<KeyCode> {
    use openlogi_core::binding::KeyCombo;
    let mut mods = Vec::new();
    if modifiers & (KeyCombo::MOD_CMD | KeyCombo::MOD_CTRL) != 0 {
        mods.push(KeyCode::KEY_LEFTCTRL);
    }
    if modifiers & KeyCombo::MOD_SHIFT != 0 {
        mods.push(KeyCode::KEY_LEFTSHIFT);
    }
    if modifiers & KeyCombo::MOD_OPTION != 0 {
        mods.push(KeyCode::KEY_LEFTALT);
    }
    mods
}

/// Map a macOS `kVK_*` virtual key code to the corresponding Linux `KeyCode`.
///
/// Source: `HIToolbox/Events.h` (macOS side) and
/// `linux/input-event-codes.h` (Linux side). Only the codes the recorder UI
/// is likely to produce are mapped; unknown codes return `None`.
fn macos_vk_to_linux(vk: u16) -> Option<KeyCode> {
    Some(match vk {
        0x00 => KeyCode::KEY_A,          // kVK_ANSI_A
        0x01 => KeyCode::KEY_S,          // kVK_ANSI_S
        0x02 => KeyCode::KEY_D,          // kVK_ANSI_D
        0x03 => KeyCode::KEY_F,          // kVK_ANSI_F
        0x04 => KeyCode::KEY_H,          // kVK_ANSI_H
        0x05 => KeyCode::KEY_G,          // kVK_ANSI_G
        0x06 => KeyCode::KEY_Z,          // kVK_ANSI_Z
        0x07 => KeyCode::KEY_X,          // kVK_ANSI_X
        0x08 => KeyCode::KEY_C,          // kVK_ANSI_C
        0x09 => KeyCode::KEY_V,          // kVK_ANSI_V
        0x0B => KeyCode::KEY_B,          // kVK_ANSI_B
        0x0C => KeyCode::KEY_Q,          // kVK_ANSI_Q
        0x0D => KeyCode::KEY_W,          // kVK_ANSI_W
        0x0E => KeyCode::KEY_E,          // kVK_ANSI_E
        0x0F => KeyCode::KEY_R,          // kVK_ANSI_R
        0x10 => KeyCode::KEY_Y,          // kVK_ANSI_Y
        0x11 => KeyCode::KEY_T,          // kVK_ANSI_T
        0x12 => KeyCode::KEY_1,          // kVK_ANSI_1
        0x13 => KeyCode::KEY_2,          // kVK_ANSI_2
        0x14 => KeyCode::KEY_3,          // kVK_ANSI_3
        0x15 => KeyCode::KEY_4,          // kVK_ANSI_4
        0x16 => KeyCode::KEY_6,          // kVK_ANSI_6
        0x17 => KeyCode::KEY_5,          // kVK_ANSI_5
        0x18 => KeyCode::KEY_EQUAL,      // kVK_ANSI_Equal
        0x19 => KeyCode::KEY_9,          // kVK_ANSI_9
        0x1A => KeyCode::KEY_7,          // kVK_ANSI_7
        0x1B => KeyCode::KEY_MINUS,      // kVK_ANSI_Minus
        0x1C => KeyCode::KEY_8,          // kVK_ANSI_8
        0x1D => KeyCode::KEY_0,          // kVK_ANSI_0
        0x1E => KeyCode::KEY_RIGHTBRACE, // kVK_ANSI_RightBracket
        0x1F => KeyCode::KEY_O,          // kVK_ANSI_O
        0x20 => KeyCode::KEY_U,          // kVK_ANSI_U
        0x21 => KeyCode::KEY_LEFTBRACE,  // kVK_ANSI_LeftBracket
        0x22 => KeyCode::KEY_I,          // kVK_ANSI_I
        0x23 => KeyCode::KEY_P,          // kVK_ANSI_P
        0x24 => KeyCode::KEY_ENTER,      // kVK_Return
        0x25 => KeyCode::KEY_L,          // kVK_ANSI_L
        0x26 => KeyCode::KEY_J,          // kVK_ANSI_J
        0x27 => KeyCode::KEY_APOSTROPHE, // kVK_ANSI_Quote
        0x28 => KeyCode::KEY_K,          // kVK_ANSI_K
        0x29 => KeyCode::KEY_SEMICOLON,  // kVK_ANSI_Semicolon
        0x2A => KeyCode::KEY_BACKSLASH,  // kVK_ANSI_Backslash
        0x2B => KeyCode::KEY_COMMA,      // kVK_ANSI_Comma
        0x2C => KeyCode::KEY_SLASH,      // kVK_ANSI_Slash
        0x2D => KeyCode::KEY_N,          // kVK_ANSI_N
        0x2E => KeyCode::KEY_M,          // kVK_ANSI_M
        0x2F => KeyCode::KEY_DOT,        // kVK_ANSI_Period
        0x30 => KeyCode::KEY_TAB,        // kVK_Tab
        0x31 => KeyCode::KEY_SPACE,      // kVK_Space
        0x32 => KeyCode::KEY_GRAVE,      // kVK_ANSI_Grave
        0x33 => KeyCode::KEY_BACKSPACE,  // kVK_Delete (= Backspace on macOS)
        0x35 => KeyCode::KEY_ESC,        // kVK_Escape
        0x60 => KeyCode::KEY_F5,         // kVK_F5
        0x61 => KeyCode::KEY_F6,         // kVK_F6
        0x62 => KeyCode::KEY_F7,         // kVK_F7
        0x63 => KeyCode::KEY_F3,         // kVK_F3
        0x64 => KeyCode::KEY_F8,         // kVK_F8
        0x65 => KeyCode::KEY_F9,         // kVK_F9
        0x67 => KeyCode::KEY_F11,        // kVK_F11
        0x6D => KeyCode::KEY_F10,        // kVK_F10
        0x6F => KeyCode::KEY_F12,        // kVK_F12
        0x76 => KeyCode::KEY_F4,         // kVK_F4
        0x78 => KeyCode::KEY_F2,         // kVK_F2
        0x7A => KeyCode::KEY_F1,         // kVK_F1
        0x73 => KeyCode::KEY_HOME,       // kVK_Home
        0x77 => KeyCode::KEY_END,        // kVK_End
        0x74 => KeyCode::KEY_PAGEUP,     // kVK_PageUp
        0x79 => KeyCode::KEY_PAGEDOWN,   // kVK_PageDown
        0x75 => KeyCode::KEY_DELETE,     // kVK_ForwardDelete
        0x7B => KeyCode::KEY_LEFT,       // kVK_LeftArrow
        0x7C => KeyCode::KEY_RIGHT,      // kVK_RightArrow
        0x7D => KeyCode::KEY_DOWN,       // kVK_DownArrow
        0x7E => KeyCode::KEY_UP,         // kVK_UpArrow
        _ => return None,
    })
}

// ── D-Bus helpers ────────────────────────────────────────────────────────

static SESSION_BUS: LazyLock<Option<DbusConn>> = LazyLock::new(|| {
    DbusConn::session()
        .map_err(|e| tracing::warn!("D-Bus session bus unavailable: {e}"))
        .ok()
});

static SYSTEM_BUS: LazyLock<Option<DbusConn>> = LazyLock::new(|| {
    DbusConn::system()
        .map_err(|e| tracing::warn!("D-Bus system bus unavailable: {e}"))
        .ok()
});

/// Lock the screen via logind `LockSession($XDG_SESSION_ID)` on the system
/// bus, falling back to Super+L.
///
/// Only the session identified by `$XDG_SESSION_ID` is locked; if the
/// variable is unset the D-Bus path is skipped entirely to avoid locking
/// all sessions on the machine. Super+L covers non-systemd systems and the
/// no-session-id case.
fn lock_screen() {
    if let (Some(conn), Ok(id)) = (SYSTEM_BUS.as_ref(), std::env::var("XDG_SESSION_ID")) {
        match conn.call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "LockSession",
            &(id.as_str(),),
        ) {
            Ok(_) => {
                tracing::debug!("LockScreen via logind");
                return;
            }
            Err(e) => tracing::warn!("logind LockSession failed: {e}"),
        }
    }
    // Super+L is the standard lock shortcut on GNOME and KDE.
    tracing::debug!("LockScreen via Super+L key combo");
    press_key(&[KeyCode::KEY_LEFTMETA], KeyCode::KEY_L);
}

/// Send `command` to the first MPRIS-capable media player on the session bus,
/// falling back to the corresponding XF86 multimedia key only if no MPRIS
/// player is found. When a player is found but the call fails, the fallback
/// is suppressed to avoid double-toggling (the player likely handles the
/// XF86 key too).
fn mpris_command(command: &str) {
    if try_mpris_command(command).is_none() {
        let fallback = match command {
            "PlayPause" => KeyCode::KEY_PLAYPAUSE,
            "Next" => KeyCode::KEY_NEXTSONG,
            "Previous" => KeyCode::KEY_PREVIOUSSONG,
            _ => return,
        };
        press_key(&[], fallback);
    }
}

fn try_mpris_command(command: &str) -> Option<()> {
    let conn = SESSION_BUS.as_ref()?;
    let reply = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "ListNames",
            &(),
        )
        .ok()?;
    let names = reply.body().deserialize::<Vec<String>>().ok()?;
    let Some(player) = names
        .iter()
        .find(|n| n.starts_with("org.mpris.MediaPlayer2."))
    else {
        tracing::debug!("no MPRIS player found — {command} via XF86 key fallback");
        return None;
    };
    match conn.call_method(
        Some(player.as_str()),
        "/org/mpris/MediaPlayer2",
        Some("org.mpris.MediaPlayer2.Player"),
        command,
        &(),
    ) {
        Ok(_) => {
            tracing::debug!("MPRIS {command} via {player}");
            Some(())
        }
        Err(e) => {
            // Player was identified — suppress XF86 fallback to avoid
            // double-toggling if the player also handles multimedia keys.
            tracing::warn!("MPRIS {command} on {player} failed: {e}");
            Some(())
        }
    }
}

#[cfg(test)]
mod tests {
    // ── modifiers_to_keycodes ─────────────────────────────────────────────

    mod modifier_mapping {
        use evdev::KeyCode;

        use super::super::modifiers_to_keycodes;
        use openlogi_core::binding::KeyCombo;

        #[test]
        fn mod_cmd_alone_maps_to_ctrl() {
            assert_eq!(
                modifiers_to_keycodes(KeyCombo::MOD_CMD),
                vec![KeyCode::KEY_LEFTCTRL]
            );
        }

        #[test]
        fn mod_ctrl_alone_maps_to_ctrl() {
            assert_eq!(
                modifiers_to_keycodes(KeyCombo::MOD_CTRL),
                vec![KeyCode::KEY_LEFTCTRL]
            );
        }

        #[test]
        fn mod_cmd_and_ctrl_together_produce_single_ctrl() {
            // Both bits set must not push KEY_LEFTCTRL twice.
            assert_eq!(
                modifiers_to_keycodes(KeyCombo::MOD_CMD | KeyCombo::MOD_CTRL),
                vec![KeyCode::KEY_LEFTCTRL]
            );
        }

        #[test]
        fn all_modifiers_produce_canonical_order() {
            let mods = modifiers_to_keycodes(
                KeyCombo::MOD_CMD | KeyCombo::MOD_SHIFT | KeyCombo::MOD_OPTION,
            );
            assert_eq!(
                mods,
                vec![
                    KeyCode::KEY_LEFTCTRL,
                    KeyCode::KEY_LEFTSHIFT,
                    KeyCode::KEY_LEFTALT
                ]
            );
        }

        #[test]
        fn no_modifiers_produces_empty_vec() {
            assert!(modifiers_to_keycodes(0).is_empty());
        }
    }

    // ── macos_vk_to_linux ─────────────────────────────────────────────────

    mod vk_mapping {
        use evdev::KeyCode;

        use super::super::macos_vk_to_linux;

        #[test]
        fn common_letters_map_correctly() {
            assert_eq!(macos_vk_to_linux(0x08), Some(KeyCode::KEY_C)); // kVK_ANSI_C
            assert_eq!(macos_vk_to_linux(0x09), Some(KeyCode::KEY_V)); // kVK_ANSI_V
            assert_eq!(macos_vk_to_linux(0x07), Some(KeyCode::KEY_X)); // kVK_ANSI_X
            assert_eq!(macos_vk_to_linux(0x00), Some(KeyCode::KEY_A)); // kVK_ANSI_A
            assert_eq!(macos_vk_to_linux(0x06), Some(KeyCode::KEY_Z)); // kVK_ANSI_Z
            assert_eq!(macos_vk_to_linux(0x0D), Some(KeyCode::KEY_W)); // kVK_ANSI_W
        }

        #[test]
        fn digits_map_correctly() {
            assert_eq!(macos_vk_to_linux(0x12), Some(KeyCode::KEY_1)); // kVK_ANSI_1
            assert_eq!(macos_vk_to_linux(0x1D), Some(KeyCode::KEY_0)); // kVK_ANSI_0
        }

        #[test]
        fn arrow_keys_map_correctly() {
            assert_eq!(macos_vk_to_linux(0x7B), Some(KeyCode::KEY_LEFT));
            assert_eq!(macos_vk_to_linux(0x7C), Some(KeyCode::KEY_RIGHT));
            assert_eq!(macos_vk_to_linux(0x7D), Some(KeyCode::KEY_DOWN));
            assert_eq!(macos_vk_to_linux(0x7E), Some(KeyCode::KEY_UP));
        }

        #[test]
        fn function_keys_map_correctly() {
            assert_eq!(macos_vk_to_linux(0x7A), Some(KeyCode::KEY_F1)); // kVK_F1
            assert_eq!(macos_vk_to_linux(0x78), Some(KeyCode::KEY_F2)); // kVK_F2
            assert_eq!(macos_vk_to_linux(0x76), Some(KeyCode::KEY_F4)); // kVK_F4
            assert_eq!(macos_vk_to_linux(0x60), Some(KeyCode::KEY_F5)); // kVK_F5
            assert_eq!(macos_vk_to_linux(0x6F), Some(KeyCode::KEY_F12)); // kVK_F12
        }

        #[test]
        fn nav_keys_map_correctly() {
            assert_eq!(macos_vk_to_linux(0x73), Some(KeyCode::KEY_HOME));
            assert_eq!(macos_vk_to_linux(0x77), Some(KeyCode::KEY_END));
            assert_eq!(macos_vk_to_linux(0x74), Some(KeyCode::KEY_PAGEUP));
            assert_eq!(macos_vk_to_linux(0x79), Some(KeyCode::KEY_PAGEDOWN));
            assert_eq!(macos_vk_to_linux(0x75), Some(KeyCode::KEY_DELETE));
        }

        #[test]
        fn brackets_follow_ansi_layout() {
            // kVK_ANSI_LeftBracket=0x21 → KEY_LEFTBRACE, RightBracket=0x1E → KEY_RIGHTBRACE
            assert_eq!(macos_vk_to_linux(0x21), Some(KeyCode::KEY_LEFTBRACE));
            assert_eq!(macos_vk_to_linux(0x1E), Some(KeyCode::KEY_RIGHTBRACE));
        }

        #[test]
        fn unmapped_code_returns_none() {
            assert_eq!(macos_vk_to_linux(0xFF), None);
            assert_eq!(macos_vk_to_linux(0x34), None); // gap in the kVK table
        }
    }
}
