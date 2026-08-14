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

use openlogi_core::binding::{Action, WorkflowStep};

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
        // logind Suspend() via the system bus.
        Action::Sleep => sleep_system(),
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
        Action::CycleDpiPresets
        | Action::SetDpiPreset(_)
        | Action::ToggleSmartShift
        | Action::ShowActionsRing
        | Action::OpenApplication(_) => {
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
            let Some(key) = hid_usage_to_linux(combo.key().code()) else {
                tracing::warn!(
                    usage = combo.key().code(),
                    "CustomShortcut usage has no Linux mapping — press ignored"
                );
                return;
            };
            press_key(&modifiers_to_keycodes(combo), key);
        }
        Action::TypeText(text) => {
            tracing::warn!(
                chars = text.chars().count(),
                "TypeText injection is not implemented on Linux yet"
            );
        }
        Action::RunAppleScript(_) => {
            tracing::warn!("RunAppleScript is only supported on macOS");
        }
        Action::RunShellCommand(cmd) => run_shell_command_async(cmd.clone()),
        Action::Workflow(steps) => run_workflow_async(steps.clone()),
    }
}

fn run_shell_command_async(cmd: String) {
    std::thread::spawn(move || run_shell_command(&cmd));
}

fn run_workflow_async(steps: Vec<WorkflowStep>) {
    std::thread::spawn(move || run_workflow(&steps));
}

fn run_workflow(steps: &[WorkflowStep]) {
    for step in steps {
        match step {
            WorkflowStep::TypeText(text) => {
                tracing::warn!(
                    chars = text.chars().count(),
                    "workflow TypeText injection is not implemented on Linux yet"
                );
            }
            WorkflowStep::PressKey(combo) => {
                let Some(key) = hid_usage_to_linux(combo.key().code()) else {
                    tracing::warn!(
                        usage = combo.key().code(),
                        "workflow PressKey usage has no Linux mapping; step ignored"
                    );
                    continue;
                };
                press_key(&modifiers_to_keycodes(combo), key);
            }
            WorkflowStep::Delay { millis } => {
                std::thread::sleep(std::time::Duration::from_millis(*millis));
            }
            WorkflowStep::RunAppleScript(_) => {
                tracing::warn!("workflow RunAppleScript is only supported on macOS");
            }
            WorkflowStep::RunShellCommand(cmd) => run_shell_command(cmd),
        }
    }
}

fn run_shell_command(cmd: &str) {
    let _ = std::process::Command::new("/bin/sh")
        .args(["-c", cmd])
        .output();
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
fn modifiers_to_keycodes(combo: &openlogi_core::binding::KeyCombo) -> Vec<KeyCode> {
    let mut modifiers = Vec::new();
    if combo.has_command() || combo.has_control() {
        modifiers.push(KeyCode::KEY_LEFTCTRL);
    }
    if combo.has_shift() {
        modifiers.push(KeyCode::KEY_LEFTSHIFT);
    }
    if combo.has_option() {
        modifiers.push(KeyCode::KEY_LEFTALT);
    }
    modifiers
}

/// Map a platform-neutral USB HID keyboard usage to evdev.
fn hid_usage_to_linux(usage: u8) -> Option<KeyCode> {
    const LETTERS: [KeyCode; 26] = [
        KeyCode::KEY_A,
        KeyCode::KEY_B,
        KeyCode::KEY_C,
        KeyCode::KEY_D,
        KeyCode::KEY_E,
        KeyCode::KEY_F,
        KeyCode::KEY_G,
        KeyCode::KEY_H,
        KeyCode::KEY_I,
        KeyCode::KEY_J,
        KeyCode::KEY_K,
        KeyCode::KEY_L,
        KeyCode::KEY_M,
        KeyCode::KEY_N,
        KeyCode::KEY_O,
        KeyCode::KEY_P,
        KeyCode::KEY_Q,
        KeyCode::KEY_R,
        KeyCode::KEY_S,
        KeyCode::KEY_T,
        KeyCode::KEY_U,
        KeyCode::KEY_V,
        KeyCode::KEY_W,
        KeyCode::KEY_X,
        KeyCode::KEY_Y,
        KeyCode::KEY_Z,
    ];
    const DIGITS: [KeyCode; 10] = [
        KeyCode::KEY_1,
        KeyCode::KEY_2,
        KeyCode::KEY_3,
        KeyCode::KEY_4,
        KeyCode::KEY_5,
        KeyCode::KEY_6,
        KeyCode::KEY_7,
        KeyCode::KEY_8,
        KeyCode::KEY_9,
        KeyCode::KEY_0,
    ];
    const FUNCTIONS: [KeyCode; 20] = [
        KeyCode::KEY_F1,
        KeyCode::KEY_F2,
        KeyCode::KEY_F3,
        KeyCode::KEY_F4,
        KeyCode::KEY_F5,
        KeyCode::KEY_F6,
        KeyCode::KEY_F7,
        KeyCode::KEY_F8,
        KeyCode::KEY_F9,
        KeyCode::KEY_F10,
        KeyCode::KEY_F11,
        KeyCode::KEY_F12,
        KeyCode::KEY_F13,
        KeyCode::KEY_F14,
        KeyCode::KEY_F15,
        KeyCode::KEY_F16,
        KeyCode::KEY_F17,
        KeyCode::KEY_F18,
        KeyCode::KEY_F19,
        KeyCode::KEY_F20,
    ];
    match usage {
        0x04..=0x1d => LETTERS.get(usize::from(usage - 0x04)).copied(),
        0x1e..=0x27 => DIGITS.get(usize::from(usage - 0x1e)).copied(),
        0x3a..=0x45 => FUNCTIONS.get(usize::from(usage - 0x3a)).copied(),
        0x68..=0x6f => FUNCTIONS.get(usize::from(usage - 0x68 + 12)).copied(),
        0x28 => Some(KeyCode::KEY_ENTER),
        0x29 => Some(KeyCode::KEY_ESC),
        0x2a => Some(KeyCode::KEY_BACKSPACE),
        0x2b => Some(KeyCode::KEY_TAB),
        0x2c => Some(KeyCode::KEY_SPACE),
        0x2d => Some(KeyCode::KEY_MINUS),
        0x2e => Some(KeyCode::KEY_EQUAL),
        0x2f => Some(KeyCode::KEY_LEFTBRACE),
        0x30 => Some(KeyCode::KEY_RIGHTBRACE),
        0x31 => Some(KeyCode::KEY_BACKSLASH),
        0x33 => Some(KeyCode::KEY_SEMICOLON),
        0x34 => Some(KeyCode::KEY_APOSTROPHE),
        0x35 => Some(KeyCode::KEY_GRAVE),
        0x36 => Some(KeyCode::KEY_COMMA),
        0x37 => Some(KeyCode::KEY_DOT),
        0x38 => Some(KeyCode::KEY_SLASH),
        0x4a => Some(KeyCode::KEY_HOME),
        0x4b => Some(KeyCode::KEY_PAGEUP),
        0x4c => Some(KeyCode::KEY_DELETE),
        0x4d => Some(KeyCode::KEY_END),
        0x4e => Some(KeyCode::KEY_PAGEDOWN),
        0x4f => Some(KeyCode::KEY_RIGHT),
        0x50 => Some(KeyCode::KEY_LEFT),
        0x51 => Some(KeyCode::KEY_DOWN),
        0x52 => Some(KeyCode::KEY_UP),
        _ => None,
    }
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

/// Suspend the system via logind's `Suspend()` on the system bus. The
/// `false` argument declines the "interactive" polkit prompt — if the
/// session isn't allowed to suspend, the call fails and is logged rather
/// than popping an authentication dialog from a background agent.
fn sleep_system() {
    let Some(conn) = SYSTEM_BUS.as_ref() else {
        tracing::warn!("no system bus — Sleep skipped");
        return;
    };
    match conn.call_method(
        Some("org.freedesktop.login1"),
        "/org/freedesktop/login1",
        Some("org.freedesktop.login1.Manager"),
        "Suspend",
        &(false,),
    ) {
        Ok(_) => tracing::debug!("Sleep via logind Suspend"),
        Err(e) => tracing::warn!("logind Suspend failed: {e}"),
    }
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
#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
mod tests {
    use evdev::KeyCode;
    use openlogi_core::binding::KeyCombo;

    use super::{hid_usage_to_linux, modifiers_to_keycodes};

    #[test]
    fn modifiers_map_to_linux_without_duplicate_control() {
        let combo = "Cmd+Ctrl+Shift+Alt+A"
            .parse::<KeyCombo>()
            .expect("a valid shortcut must parse");
        assert_eq!(
            modifiers_to_keycodes(&combo),
            vec![
                KeyCode::KEY_LEFTCTRL,
                KeyCode::KEY_LEFTSHIFT,
                KeyCode::KEY_LEFTALT
            ]
        );
    }

    #[test]
    fn hid_usages_map_letters_navigation_and_function_keys() {
        assert_eq!(hid_usage_to_linux(0x04), Some(KeyCode::KEY_A));
        assert_eq!(hid_usage_to_linux(0x50), Some(KeyCode::KEY_LEFT));
        assert_eq!(hid_usage_to_linux(0x3a), Some(KeyCode::KEY_F1));
        assert_eq!(hid_usage_to_linux(0x6f), Some(KeyCode::KEY_F20));
        assert_eq!(hid_usage_to_linux(0xff), None);
    }
}
