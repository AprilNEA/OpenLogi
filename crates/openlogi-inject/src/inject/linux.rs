//! Linux helpers for synthesising OS-level input events via a shared `uinput`
//! virtual device.
//!
//! The device is created lazily on first use. If `/dev/uinput` is inaccessible
//! (missing group membership or udev rule) every call logs a `warn` and returns
//! without panicking.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode};
use zbus::blocking::Connection as DbusConn;

use openlogi_core::binding::{
    Action, Effect, KeyCombo, MediaKey, MouseButton, NativeAction, Script, Shortcut, WorkflowStep,
};
use openlogi_core::scroll::ScrollDelta;

use super::{HeldKey, KeyPhase, QuantizedScroll, ScrollQuantizer};

const HIGH_RES_UNITS_PER_TICK: f64 = 120.0;

#[derive(Default)]
struct ScrollOutput {
    high_resolution: ScrollQuantizer,
    legacy: ScrollQuantizer,
}

static SCROLL_OUTPUT: LazyLock<Mutex<ScrollOutput>> =
    LazyLock::new(|| Mutex::new(ScrollOutput::default()));

/// Linux implementation: classify `action` into an [`Effect`] and inject the
/// resulting events via a shared `uinput` virtual device.
pub(super) fn execute(action: &Action) {
    match action.effect() {
        Effect::None => {}
        // Extra mouse buttons: BTN_SIDE/BTN_EXTRA are the evdev side
        // buttons ("back"/"forward") browsers handle natively.
        Effect::Click(button) => click(mouse_button_code(button)),
        Effect::Shortcut(shortcut) => press_combo(&combo(shortcut)),
        Effect::Key(combo) | Effect::HeldKey(combo) => press_combo(combo),
        Effect::Scroll { dx, dy } => dispatch_scroll(dx, dy),
        Effect::Media(key) => dispatch_media(key),
        Effect::Native(native) => dispatch_native(action, native),
        Effect::Script(script) => dispatch_script(script),
        Effect::Text(text) => {
            tracing::warn!(
                chars = text.chars().count(),
                "TypeText injection is not implemented on Linux yet"
            );
        }
        Effect::AgentSide => {
            tracing::debug!(
                action = action.label(),
                "device action handled by hook/HID layer"
            );
        }
    }
}

fn mouse_button_code(button: MouseButton) -> KeyCode {
    match button {
        MouseButton::Left => KeyCode::BTN_LEFT,
        MouseButton::Right => KeyCode::BTN_RIGHT,
        MouseButton::Middle => KeyCode::BTN_MIDDLE,
        MouseButton::Back => KeyCode::BTN_SIDE,
        MouseButton::Forward => KeyCode::BTN_EXTRA,
    }
}

/// The Linux chord for each named [`Shortcut`].
///
/// Parsed through [`KeyCombo`]'s existing, tested `FromStr` rather than
/// hand-built keycode lists — the table stays a flat, auditable list of
/// chord strings instead of a second modifier-encoding call site.
fn combo(shortcut: Shortcut) -> KeyCombo {
    let text = match shortcut {
        Shortcut::Copy => "Ctrl+C",
        Shortcut::Paste => "Ctrl+V",
        Shortcut::Cut => "Ctrl+X",
        Shortcut::Undo => "Ctrl+Z",
        // Ctrl+Shift+Z matches the macOS ⌘⇧Z convention (see `Shortcut::Redo`
        // doc on `Action`); Ctrl+Y is the GTK/LibreOffice convention and is
        // left to a `CustomShortcut` binding.
        Shortcut::Redo => "Ctrl+Shift+Z",
        Shortcut::SelectAll => "Ctrl+A",
        Shortcut::Find => "Ctrl+F",
        Shortcut::Save => "Ctrl+S",
        Shortcut::BrowserBack => "Alt+Left",
        Shortcut::BrowserForward => "Alt+Right",
        Shortcut::NewTab => "Ctrl+T",
        Shortcut::CloseTab => "Ctrl+W",
        Shortcut::ReopenTab => "Ctrl+Shift+T",
        Shortcut::NextTab => "Ctrl+Tab",
        Shortcut::PrevTab => "Ctrl+Shift+Tab",
        Shortcut::ReloadPage => "Ctrl+R",
    };
    parse_shortcut(text)
}

fn parse_shortcut(text: &str) -> KeyCombo {
    text.parse()
        .unwrap_or_else(|error| unreachable!("hardcoded shortcut table entry {text:?}: {error}"))
}

/// Press an already-resolved chord: a table lookup from [`combo`] or a
/// user-recorded [`Action::CustomShortcut`]/`WorkflowStep::PressKey`.
fn press_combo(combo: &KeyCombo) {
    let Some(key) = hid_usage_to_linux(combo.key().code()) else {
        tracing::warn!(
            usage = combo.key().code(),
            "shortcut usage has no Linux mapping — press ignored"
        );
        return;
    };
    press_key(&modifiers_to_keycodes(combo), key);
}

/// Emit one edge for the physical keys whose ownership changed.
pub(super) fn hold_keys(keys: &[HeldKey], phase: KeyPhase) {
    let keys: Vec<_> = keys.iter().filter_map(|key| held_keycode(*key)).collect();
    if !keys.is_empty() {
        emit(&held_key_events(&keys, phase));
    }
}

/// MPRIS targets the running media player; XF86 volume keys go to the
/// system mixer (PulseAudio/PipeWire) which is what users expect.
fn dispatch_media(key: MediaKey) {
    match key {
        MediaKey::PlayPause => mpris_command("PlayPause"),
        MediaKey::NextTrack => mpris_command("Next"),
        MediaKey::PrevTrack => mpris_command("Previous"),
        MediaKey::VolumeUp => press_key(&[], KeyCode::KEY_VOLUMEUP),
        MediaKey::VolumeDown => press_key(&[], KeyCode::KEY_VOLUMEDOWN),
        MediaKey::Mute => press_key(&[], KeyCode::KEY_MUTE),
    }
}

/// Dispatch a window-manager or power [`NativeAction`]. `action` is only
/// used for its label in the "no Linux equivalent" debug log.
fn dispatch_native(action: &Action, native: NativeAction) {
    let ctrl = KeyCode::KEY_LEFTCTRL;
    let alt = KeyCode::KEY_LEFTALT;
    match native {
        // No universal Linux equivalent; the compositor shortcut varies.
        NativeAction::MissionControl
        | NativeAction::AppExpose
        | NativeAction::ShowDesktop
        | NativeAction::LaunchpadShow => {
            tracing::debug!(
                action = action.label(),
                "no Linux equivalent — action skipped"
            );
        }
        // Ctrl+Alt+←/→ is the default in GNOME and KDE.
        NativeAction::PreviousDesktop => press_key(&[ctrl, alt], KeyCode::KEY_LEFT),
        NativeAction::NextDesktop => press_key(&[ctrl, alt], KeyCode::KEY_RIGHT),
        // logind LockSession() via the system bus; falls back to Super+L.
        NativeAction::LockScreen => lock_screen(),
        // Region vs full-screen capture depends on the desktop environment's
        // screenshot handler for Print Screen, so both map to the same key.
        NativeAction::Screenshot | NativeAction::CaptureRegion => {
            press_key(&[], KeyCode::KEY_SYSRQ);
        }
        // logind Suspend() via the system bus.
        NativeAction::Sleep => sleep_system(),
    }
}

fn dispatch_script(script: Script<'_>) {
    match script {
        Script::AppleScript(_) => {
            tracing::warn!("RunAppleScript is only supported on macOS");
        }
        Script::ShellCommand(cmd) => run_shell_command_async(cmd.to_string()),
        Script::Workflow(steps) => run_workflow_async(steps.to_vec()),
    }
}

/// Synthesise one scroll tick in direction `(dx, dy)`. Unit direction
/// (-1/0/1) scaled by the fixed relative-axis magnitude the four
/// `Scroll*`/`HorizontalScroll*` actions have always used.
fn dispatch_scroll(dx: i8, dy: i8) {
    if dy != 0 {
        scroll(RelativeAxisCode::REL_WHEEL, i32::from(dy) * 3);
    }
    if dx != 0 {
        scroll(RelativeAxisCode::REL_HWHEEL, i32::from(dx) * 3);
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
    for a in [
        RelativeAxisCode::REL_WHEEL,
        RelativeAxisCode::REL_HWHEEL,
        RelativeAxisCode::REL_WHEEL_HI_RES,
        RelativeAxisCode::REL_HWHEEL_HI_RES,
    ] {
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
    emit(&key_phase_events(mods, key, KeyPhase::Down));
    emit(&key_phase_events(mods, key, KeyPhase::Up));
}

/// Build one `SYN_REPORT` frame. Down order is modifiers then key; up order
/// is the exact reverse so the ordinary key never escapes as an unmodified
/// release.
fn key_phase_events(mods: &[KeyCode], key: KeyCode, phase: KeyPhase) -> Vec<InputEvent> {
    let mut keys = Vec::with_capacity(mods.len() + 1);
    keys.extend_from_slice(mods);
    keys.push(key);
    held_key_events(&keys, phase)
}

fn held_key_events(keys: &[KeyCode], phase: KeyPhase) -> Vec<InputEvent> {
    let mut events = Vec::with_capacity(keys.len() + 1);
    match phase {
        KeyPhase::Down => {
            events.extend(keys.iter().map(|key| key_ev(*key, 1)));
        }
        KeyPhase::Up => {
            events.extend(keys.iter().rev().map(|key| key_ev(*key, 0)));
        }
    }
    events.push(syn());
    events
}

/// Inject a button-down in one SYN frame and button-up in a second.
fn click(button: KeyCode) {
    emit(&[key_ev(button, 1), syn()]);
    emit(&[key_ev(button, 0), syn()]);
}

/// Inject a single relative-axis delta followed by `SYN_REPORT`.
fn scroll(axis: RelativeAxisCode, value: i32) {
    emit(&[rel_ev(axis, value), syn()]);
}

pub(super) fn post_scroll(delta: ScrollDelta) {
    let ScrollDelta::WheelTicks { .. } = delta else {
        tracing::debug!("pixel scroll output is unsupported on Linux");
        return;
    };
    let Ok(mut output) = SCROLL_OUTPUT.lock() else {
        tracing::warn!("Linux scroll quantizer mutex poisoned");
        return;
    };
    let high_resolution = output
        .high_resolution
        .quantize(delta, HIGH_RES_UNITS_PER_TICK);
    let legacy = output.legacy.quantize(delta, 1.0);
    drop(output);

    let mut events = Vec::with_capacity(5);
    push_scroll_axes(
        &mut events,
        high_resolution,
        RelativeAxisCode::REL_HWHEEL_HI_RES,
        RelativeAxisCode::REL_WHEEL_HI_RES,
    );
    push_scroll_axes(
        &mut events,
        legacy,
        RelativeAxisCode::REL_HWHEEL,
        RelativeAxisCode::REL_WHEEL,
    );
    if !events.is_empty() {
        events.push(syn());
        emit(&events);
    }
}

fn push_scroll_axes(
    events: &mut Vec<InputEvent>,
    delta: QuantizedScroll,
    horizontal: RelativeAxisCode,
    vertical: RelativeAxisCode,
) {
    if delta.x != 0 {
        events.push(rel_ev(horizontal, delta.x));
    }
    if delta.y != 0 {
        events.push(rel_ev(vertical, delta.y));
    }
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

/// Convert a [`KeyCombo`] modifier bitmask
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

fn held_keycode(key: HeldKey) -> Option<KeyCode> {
    match key {
        HeldKey::Control => Some(KeyCode::KEY_LEFTCTRL),
        HeldKey::Shift => Some(KeyCode::KEY_LEFTSHIFT),
        HeldKey::Alt => Some(KeyCode::KEY_LEFTALT),
        HeldKey::Key(usage) => {
            let key = hid_usage_to_linux(usage.code());
            if key.is_none() {
                tracing::warn!(
                    usage = usage.code(),
                    "held shortcut usage has no Linux mapping — edge ignored"
                );
            }
            key
        }
    }
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

/// What an "Open application" target should do on Linux.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Launch {
    /// Hand it to the desktop opener (`xdg-open`): URLs, folders, documents,
    /// and `.desktop` entries — which xdg-open activates properly.
    Opener,
    /// Run it as a program: an executable file path, or a bare command name
    /// resolved on `PATH`.
    Program(PathBuf),
}

/// Decide how `target` should be launched.
///
/// `is_executable` and `on_path` are injected so the decision table is
/// testable without touching the real filesystem or `PATH`.
fn classify(
    target: &str,
    is_executable: &dyn Fn(&Path) -> bool,
    on_path: &dyn Fn(&str) -> Option<PathBuf>,
) -> Launch {
    if target.contains("://") {
        return Launch::Opener;
    }
    // A desktop entry is the one "application" xdg-open launches correctly —
    // it reads Exec= and applies the entry's Terminal / StartupNotify keys,
    // which spawning the file directly would not.
    if Path::new(target)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("desktop"))
    {
        return Launch::Opener;
    }
    if target.contains('/') {
        let path = Path::new(target);
        return if is_executable(path) {
            Launch::Program(path.to_path_buf())
        } else {
            Launch::Opener
        };
    }
    on_path(target).map_or(Launch::Opener, Launch::Program)
}

/// Whether `path` is a regular file with any execute bit set.
///
/// A mode check, not an effective-permission one: a file executable only by
/// another user still answers `true` here. That is deliberate — this decides
/// *classification*, and the exec itself is the authority on whether the
/// program can actually run, so [`launch_program`] falls back to the opener
/// when the spawn is refused.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// First executable named `name` on `PATH`.
fn on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable(candidate))
    })
}

/// Run an "Open application" target as a program when that is what it is,
/// reporting whether it was handled here.
///
/// `xdg-open` — what `opener` calls on Linux — *opens* a file: handed
/// `/usr/bin/nautilus` it looks for something claiming
/// `application/x-executable` and, finding nothing, does nothing at all
/// (#775). A path to an executable, or a bare command name on `PATH`, has to
/// be spawned instead. Everything the desktop genuinely knows how to open —
/// URLs, folders, documents, `.desktop` entries — returns `false` and stays
/// with the opener.
///
/// The spawn runs on the calling thread so its result decides the return
/// value; only the wait is detached, because a long-lived GUI app would
/// otherwise linger as a zombie for the agent's whole lifetime.
///
/// `Command::spawn` reports a failed `exec` — the target is not executable by
/// this user, is not a valid binary, or vanished between the check and the
/// call — rather than succeeding and failing later, so a refused spawn is
/// reported as unhandled here and the caller falls through to the opener.
/// Without that, a file whose execute bit belongs to another user classified
/// as a program, failed to start, and activating the slot did nothing at all.
pub(super) fn launch_program(target: &str) -> bool {
    let Launch::Program(program) = classify(target, &is_executable, &on_path) else {
        return false;
    };
    match std::process::Command::new(&program)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            true
        }
        // Not executable *by this user*: the opener cannot run it either, and
        // handing a binary to `xdg-open` risks it landing in a text editor.
        // Report it handled so nothing else is tried, and say why.
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            tracing::warn!(
                %error,
                program = %program.display(),
                "the configured application is not executable by this user — nothing to run"
            );
            true
        }
        // Any other refusal means the mode bit was misleading — a data file
        // with the bit set, a script with no interpreter, a path that vanished.
        // Those are the desktop opener's business after all.
        Err(error) => {
            tracing::warn!(
                %error,
                program = %program.display(),
                "not a runnable program — handing it to the desktop opener"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use evdev::KeyCode;
    use openlogi_core::binding::{KeyCombo, Shortcut};

    use std::path::{Path, PathBuf};

    use std::os::unix::fs::PermissionsExt as _;

    use super::{
        KeyPhase, Launch, classify, combo, hid_usage_to_linux, is_executable, key_ev,
        key_phase_events, launch_program, modifiers_to_keycodes, on_path, syn,
    };

    #[test]
    fn held_chord_edges_use_inverse_key_order() {
        let modifiers = [KeyCode::KEY_LEFTCTRL, KeyCode::KEY_LEFTSHIFT];
        assert_eq!(
            key_phase_events(&modifiers, KeyCode::KEY_P, KeyPhase::Down),
            vec![
                key_ev(KeyCode::KEY_LEFTCTRL, 1),
                key_ev(KeyCode::KEY_LEFTSHIFT, 1),
                key_ev(KeyCode::KEY_P, 1),
                syn(),
            ]
        );
        assert_eq!(
            key_phase_events(&modifiers, KeyCode::KEY_P, KeyPhase::Up),
            vec![
                key_ev(KeyCode::KEY_P, 0),
                key_ev(KeyCode::KEY_LEFTSHIFT, 0),
                key_ev(KeyCode::KEY_LEFTCTRL, 0),
                syn(),
            ]
        );
    }

    /// #775: configuring `/usr/bin/nautilus` (or bare `nautilus`) did
    /// nothing, because `xdg-open` opens files rather than running them.
    #[test]
    fn executables_are_run_and_everything_else_goes_to_the_opener() {
        let executable = |p: &Path| p == Path::new("/usr/bin/nautilus");
        let on_path = |name: &str| (name == "nautilus").then(|| PathBuf::from("/usr/bin/nautilus"));

        assert_eq!(
            classify("/usr/bin/nautilus", &executable, &on_path),
            Launch::Program(PathBuf::from("/usr/bin/nautilus")),
            "an executable path is a program to run"
        );
        assert_eq!(
            classify("nautilus", &executable, &on_path),
            Launch::Program(PathBuf::from("/usr/bin/nautilus")),
            "a bare command name resolves through PATH"
        );

        for opener in [
            "https://example.com",
            "/home/u/Documents",
            "/home/u/notes.txt",
            "/usr/share/applications/org.gnome.Nautilus.desktop",
            "definitely-not-on-path",
        ] {
            assert_eq!(
                classify(opener, &executable, &on_path),
                Launch::Opener,
                "{opener} belongs to the desktop opener"
            );
        }
    }

    /// A path that classifies as a program but cannot be exec'd must report
    /// itself unhandled, so the caller still hands it to the desktop opener.
    /// A regular file with no execute bit for *this* user is the case the
    /// mode-based classification cannot see (#839 review).
    #[test]
    fn a_refused_spawn_is_reported_as_unhandled() {
        let dir = std::env::temp_dir().join(format!("openlogi-launch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("not-a-binary");
        std::fs::write(&path, b"\x7fELF this is not a loadable binary").expect("write file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("set mode");

        let target = path.to_str().expect("utf-8 temp path");
        assert_eq!(
            classify(target, &is_executable, &on_path),
            Launch::Program(path.clone()),
            "the mode check classifies it as a program"
        );
        assert!(
            !launch_program(target),
            "an exec the kernel refuses must fall through to the opener"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A file whose execute bit belongs to someone else classifies as a
    /// program but cannot be run here — and the desktop opener cannot run it
    /// either, so falling through to it would only repeat the failure with a
    /// binary in a text editor as the best case. Report it handled instead.
    #[test]
    fn an_unexecutable_program_is_not_passed_to_the_opener() {
        let dir = std::env::temp_dir().join(format!("openlogi-eacces-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("no-permission");
        std::fs::write(&path, b"#!/bin/sh\ntrue\n").expect("write file");
        // Executable for group and other, never for the owner running this.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o011)).expect("set mode");

        let target = path.to_str().expect("utf-8 temp path");
        assert_eq!(
            classify(target, &is_executable, &on_path),
            Launch::Program(path.clone()),
            "the mode check still sees an execute bit"
        );
        assert!(
            launch_program(target),
            "an exec refused for permissions must not fall through to the opener"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The fakes above pin the decision table; this pins the two real probes
    /// they stand in for against the filesystem. `sh` is on `PATH` and
    /// executable on every Linux host.
    #[test]
    fn the_real_probes_resolve_a_shell_on_path() {
        let resolved = on_path("sh").expect("sh is on PATH");
        assert!(
            is_executable(&resolved),
            "{} is executable",
            resolved.display()
        );
        assert_eq!(
            classify("sh", &is_executable, &on_path),
            Launch::Program(resolved)
        );
        assert_eq!(classify("/", &is_executable, &on_path), Launch::Opener);
    }

    /// A `.desktop` entry is executable often enough that the extension check
    /// has to come first: xdg-open reads its `Exec=` and honours `Terminal=`
    /// and `StartupNotify=`, which spawning the file itself would not.
    #[test]
    fn an_executable_desktop_entry_still_goes_to_the_opener() {
        assert_eq!(
            classify(
                "/usr/share/applications/foo.desktop",
                &|_: &Path| true,
                &|_: &str| None
            ),
            Launch::Opener
        );
    }

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

    /// Pin a handful of representative `Shortcut -> KeyCombo` rows so an
    /// edit to the table can't silently change what Ctrl+C sends.
    /// `BrowserBack` and `Redo` differ from macOS/Windows by design (see
    /// the module doc on `combo`), so each backend pins its own rows.
    #[test]
    fn combo_table_pins_representative_shortcuts() {
        assert_eq!(combo(Shortcut::Copy).rendered_label(), "Ctrl+C");
        assert_eq!(combo(Shortcut::Redo).rendered_label(), "Ctrl+Shift+Z");
        assert_eq!(combo(Shortcut::BrowserBack).rendered_label(), "Alt+Left");
        assert_eq!(combo(Shortcut::NextTab).rendered_label(), "Ctrl+Tab");
        // hid_usage_to_linux must actually resolve every table entry, or a
        // `Shortcut` silently no-ops instead of pressing anything (see
        // `press_combo`'s warn-and-drop path). Iterates `Shortcut::ALL`
        // rather than a hand-copied list, so a newly added `Shortcut`
        // variant is checked here automatically instead of depending on
        // someone remembering to extend a second, independent list.
        for &shortcut in Shortcut::ALL {
            let key = combo(shortcut).key().code();
            assert!(
                hid_usage_to_linux(key).is_some(),
                "{shortcut:?} table entry has no Linux keycode mapping"
            );
        }
    }
}
