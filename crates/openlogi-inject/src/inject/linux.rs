//! Linux helpers for synthesising OS-level input events via a shared `uinput`
//! virtual device.
//!
//! The device is created lazily on first use. If `/dev/uinput` is inaccessible
//! (missing group membership or udev rule) every call logs a `warn` and returns
//! without panicking.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    LazyLock, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode};
use zbus::blocking::Connection as DbusConn;

use openlogi_core::binding::{
    Action, Effect, KeyCombo, MediaKey, MouseButton, NativeAction, Shortcut,
};
use openlogi_core::scroll::ScrollDelta;

use super::{HeldKey, HeldOutput, KeyPhase, QuantizedScroll, ScrollQuantizer};

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
        Effect::Script(script) => super::dispatch_script(script),
        Effect::Text(text) => type_text(text),
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
    super::parse_shortcut(text)
}

/// Press an already-resolved chord: a table lookup from [`combo`] or a
/// user-recorded [`Action::CustomShortcut`]/`WorkflowStep::PressKey`.
pub(super) fn press_combo(combo: &KeyCombo) {
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
    if hyprland_session() {
        tracing::debug!(
            desktop = std::env::var("XDG_CURRENT_DESKTOP")
                .ok()
                .as_deref()
                .unwrap_or(""),
            "Hyprland native table selected"
        );
        dispatch_hyprland_native(action, native);
        return;
    }
    dispatch_generic_linux_native(action, native);
}

fn dispatch_generic_linux_native(action: &Action, native: NativeAction) {
    match native {
        // On KDE, KWin's Overview effect via its global-shortcut component.
        // GNOME's overlay key toggles the Activities overview on a bare tap;
        // no other tested desktop binds Super alone the same way (KDE opens
        // KRunner), so the key only fires under GNOME. Activities shows every
        // window across every app, not just the frontmost app's — that is
        // MissionControl's contract, not AppExpose's, so AppExpose must not
        // share this arm without its own promised single-app scope.
        NativeAction::MissionControl => {
            if is_kde() {
                kwin_invoke_shortcut(action, "Overview");
            } else {
                gnome_key_or_skip(action, &[], KeyCode::KEY_LEFTMETA);
            }
        }
        // KWin's "Expose windows of current class" effect keeps the
        // single-app scope. GNOME has no stock single-app window-exposé
        // binding (Activities shows every app), so everywhere else skips
        // rather than substitute the wrong scope.
        NativeAction::AppExpose => {
            if is_kde() {
                kwin_invoke_shortcut(action, "ExposeClass");
            } else {
                tracing::debug!(
                    action = action.label(),
                    "no Linux equivalent — action skipped"
                );
            }
        }
        // KWin's show-desktop toggle on KDE; GNOME's default "Show desktop"
        // keybinding otherwise.
        NativeAction::ShowDesktop => {
            if is_kde() {
                kwin_toggle_show_desktop(action);
            } else {
                gnome_key_or_skip(action, &[KeyCode::KEY_LEFTMETA], KeyCode::KEY_D);
            }
        }
        // GNOME's default "Show Applications" keybinding.
        NativeAction::LaunchpadShow => {
            gnome_key_or_skip(action, &[KeyCode::KEY_LEFTMETA], KeyCode::KEY_A);
        }
        // On KDE, drive KWin's D-Bus API directly; elsewhere press the
        // desktop's own workspace chord (see `desktop_switch_mods`), which is
        // also what KDE falls back to when the KWin call is not handled.
        NativeAction::PreviousDesktop => {
            kwin_switch_desktop("previousDesktop").unwrap_or_else(|| {
                press_key(
                    desktop_switch_mods(current_desktop().as_deref()),
                    KeyCode::KEY_LEFT,
                );
            });
        }
        NativeAction::NextDesktop => {
            kwin_switch_desktop("nextDesktop").unwrap_or_else(|| {
                press_key(
                    desktop_switch_mods(current_desktop().as_deref()),
                    KeyCode::KEY_RIGHT,
                );
            });
        }
        // logind LockSession() via the system bus; falls back to Super+L.
        NativeAction::LockScreen => lock_screen(),
        NativeAction::Screenshot => press_key(&[], KeyCode::KEY_SYSRQ),
        // Print alone only opens the region selector on GNOME >= 42; other
        // desktops need an extra chord (see `capture_region_mods`).
        NativeAction::CaptureRegion => {
            press_key(
                &capture_region_mods(current_desktop().as_deref()),
                KeyCode::KEY_SYSRQ,
            );
        }
        // logind Suspend() via the system bus.
        NativeAction::Sleep => sleep_system(),
    }
}

fn dispatch_hyprland_native(action: &Action, native: NativeAction) {
    match native {
        NativeAction::MissionControl | NativeAction::AppExpose | NativeAction::ShowDesktop => {
            skip_unsupported_native(action);
        }
        NativeAction::LaunchpadShow | NativeAction::PreviousDesktop | NativeAction::NextDesktop => {
            if let Some((helper, fallback)) = hyprland_helper(native) {
                run_helper_or_chord(helper, fallback);
            }
        }
        NativeAction::LockScreen => lock_screen_hyprland(),
        NativeAction::Screenshot | NativeAction::CaptureRegion => {
            press_key(&[], KeyCode::KEY_SYSRQ);
        }
        NativeAction::Sleep => sleep_system(),
    }
}

fn skip_unsupported_native(action: &Action) {
    tracing::debug!(
        action = action.label(),
        "no Linux equivalent — action skipped"
    );
}

/// Hyprland compositor session. Gate is the instance signature only —
/// `XDG_CURRENT_DESKTOP` is logged for diagnostics and must not select the table.
fn hyprland_session() -> bool {
    hyprland_session_from_signature(std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok().as_deref())
}

fn hyprland_session_from_signature(signature: Option<&str>) -> bool {
    signature.is_some_and(|value| !value.is_empty())
}

const HELPER_PREFIXES: [&str; 2] = ["/usr/bin", "/usr/local/bin"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HelperSpec {
    program: &'static str,
    args: &'static [&'static str],
    /// Arguments to run the same program with once more when `args` exits
    /// unsuccessfully, before the keyboard fallback.
    retry_args: Option<&'static [&'static str]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChordSpec {
    modifiers: &'static [KeyCode],
    key: KeyCode,
}

fn hyprland_helper(native: NativeAction) -> Option<(HelperSpec, ChordSpec)> {
    match native {
        NativeAction::LaunchpadShow => Some((
            HelperSpec {
                program: "omarchy-menu",
                args: &["toggle"],
                retry_args: None,
            },
            ChordSpec {
                modifiers: &[KeyCode::KEY_LEFTMETA],
                key: KeyCode::KEY_SPACE,
            },
        )),
        NativeAction::NextDesktop => Some((
            HelperSpec {
                program: "hyprctl",
                args: &["dispatch", "workspace", "e+1"],
                // A Lua config evaluates `dispatch`'s argument as Lua, so the
                // hyprlang form above fails there.
                retry_args: Some(&["dispatch", r#"hl.dsp.focus({ workspace = "e+1" })"#]),
            },
            ChordSpec {
                modifiers: &[KeyCode::KEY_LEFTMETA],
                key: KeyCode::KEY_TAB,
            },
        )),
        NativeAction::PreviousDesktop => Some((
            HelperSpec {
                program: "hyprctl",
                args: &["dispatch", "workspace", "e-1"],
                // A Lua config evaluates `dispatch`'s argument as Lua, so the
                // hyprlang form above fails there.
                retry_args: Some(&["dispatch", r#"hl.dsp.focus({ workspace = "e-1" })"#]),
            },
            ChordSpec {
                modifiers: &[KeyCode::KEY_LEFTMETA, KeyCode::KEY_LEFTSHIFT],
                key: KeyCode::KEY_TAB,
            },
        )),
        _ => None,
    }
}

fn hyprland_lock_helper() -> HelperSpec {
    HelperSpec {
        program: "omarchy-system-lock",
        args: &[],
        retry_args: None,
    }
}

fn hyprland_lock_fallback_chord() -> ChordSpec {
    ChordSpec {
        modifiers: &[KeyCode::KEY_LEFTMETA, KeyCode::KEY_LEFTCTRL],
        key: KeyCode::KEY_L,
    }
}

fn generic_linux_lock_fallback_chord() -> ChordSpec {
    ChordSpec {
        modifiers: &[KeyCode::KEY_LEFTMETA],
        key: KeyCode::KEY_L,
    }
}

fn run_helper_or_chord(helper: HelperSpec, fallback: ChordSpec) {
    if run_fixed_argv(helper, fallback) {
        return;
    }
    press_key(fallback.modifiers, fallback.key);
}

/// Spawn one allowlisted absolute helper. Never searches `PATH` and never
/// shells out through `/bin/sh -c` (that path is only for user
/// `RunShellCommand`). A concurrent request uses its keyboard fallback instead
/// of creating another process.
fn run_fixed_argv(helper: HelperSpec, fallback: ChordSpec) -> bool {
    let program = helper.program;
    let Some(path) = resolve_allowlisted(program) else {
        tracing::debug!(
            program,
            "Hyprland helper not in an allowlisted directory — using chord fallback"
        );
        return false;
    };
    if HELPER_IN_FLIGHT.swap(true, Ordering::AcqRel) {
        tracing::debug!(
            program,
            "Hyprland helper already in flight — using chord fallback"
        );
        return false;
    }
    match Command::new(&path).args(helper.args).spawn() {
        Ok(child) => {
            std::thread::spawn(move || {
                if !wait_with_retry(child, &path, helper.retry_args) {
                    press_key(fallback.modifiers, fallback.key);
                }
                HELPER_IN_FLIGHT.store(false, Ordering::Release);
            });
            true
        }
        Err(error) => {
            HELPER_IN_FLIGHT.store(false, Ordering::Release);
            tracing::warn!(
                program = %path.display(),
                %error,
                "failed to spawn Hyprland helper — using chord fallback"
            );
            false
        }
    }
}

const HELPER_TIMEOUT: Duration = Duration::from_secs(2);
static HELPER_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Wait for `child`; when it fails, run `path` once more with `retry_args`.
/// `true` when either run succeeded.
fn wait_with_retry(child: Child, path: &Path, retry_args: Option<&[&str]>) -> bool {
    wait_for_helper(child)
        || retry_args.is_some_and(|args| {
            Command::new(path)
                .args(args)
                .spawn()
                .is_ok_and(wait_for_helper)
        })
}

/// Wait for one helper run; `false` when it failed, timed out, or could not be
/// waited on.
fn wait_for_helper(mut child: Child) -> bool {
    let deadline = Instant::now() + HELPER_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break true,
            Ok(Some(status)) => {
                tracing::debug!(
                    %status,
                    "Hyprland helper exited unsuccessfully"
                );
                break false;
            }
            Ok(None) if Instant::now() >= deadline => {
                tracing::warn!("Hyprland helper timed out");
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "failed while waiting for Hyprland helper"
                );
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    }
}

fn resolve_allowlisted(name: &str) -> Option<PathBuf> {
    resolve_allowlisted_under(name, &HELPER_PREFIXES)
}

fn resolve_allowlisted_under(name: &str, prefixes: &[&str]) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return None;
    }
    prefixes
        .iter()
        .map(|prefix| Path::new(prefix).join(name))
        .find(|path| path.is_file())
}

/// Press `mods`+`key` under GNOME; log the existing "no Linux equivalent"
/// skip everywhere else, since Super-based bindings vary by desktop.
fn gnome_key_or_skip(action: &Action, mods: &[KeyCode], key: KeyCode) {
    if is_gnome() {
        press_key(mods, key);
    } else {
        tracing::debug!(
            action = action.label(),
            "no Linux equivalent — action skipped"
        );
    }
}

fn is_gnome() -> bool {
    desktop_is_gnome(std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
}

/// The pure half of [`is_gnome`], taking the desktop string directly instead
/// of reading it from the process environment — so tests can drive it
/// without `std::env::set_var`, which cannot be scoped to just this test:
/// the process environment is process-wide, and a private test-only mutex
/// only serializes callers that take it, not every other reader (including
/// `is_gnome` itself, called from unrelated code running concurrently).
fn desktop_is_gnome(current_desktop: impl AsRef<str>) -> bool {
    current_desktop.as_ref().to_lowercase().contains("gnome")
}

/// Synthesise one scroll tick in direction `(dx, dy)`. Unit direction
/// (-1/0/1) scaled by the fixed relative-axis magnitude the four
/// `Scroll*`/`HorizontalScroll*` actions have always used.
fn dispatch_scroll(dx: i8, dy: i8) {
    post_scroll(action_scroll(dx, dy));
}

fn action_scroll(dx: i8, dy: i8) -> ScrollDelta {
    ScrollDelta::wheel_ticks(f64::from(dx) * 3.0, f64::from(dy) * 3.0)
}

/// Not implemented yet: unicode text has no uinput encoding without a keymap.
pub(super) fn type_text(text: &str) {
    tracing::warn!(
        chars = text.chars().count(),
        "TypeText injection is not implemented on Linux yet"
    );
}

pub(super) fn run_apple_script(_src: &str) {
    tracing::warn!("RunAppleScript is only supported on macOS");
}

pub(super) fn run_shell_command(cmd: &str) {
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", cmd]);
    super::run_to_end(command, "shell command");
}

/// Settle time after creating the uinput device, before its first emit.
///
/// `/dev/uinput` creation is synchronous, but udev, libinput, and the display
/// server noticing the new event node and opening it are not — and an evdev
/// reader never sees events written before it opens the device. Emitting the
/// very first action right after creation races that pickup and is silently
/// dropped, with every later action working once the display server has
/// caught up (#1800).
const UINPUT_SETTLE: std::time::Duration = std::time::Duration::from_millis(250);

static VIRTUAL_INPUT: LazyLock<Option<Mutex<VirtualDevice>>> = LazyLock::new(|| {
    let device = build()
        .map_err(|e| tracing::warn!("failed to create uinput action device: {e}"))
        .ok()?;
    std::thread::sleep(UINPUT_SETTLE);
    Some(Mutex::new(device))
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
    KeyCode::KEY_ESC,   KeyCode::KEY_SPACE, KeyCode::KEY_INSERT,
    // Keypad
    KeyCode::KEY_KP0, KeyCode::KEY_KP1, KeyCode::KEY_KP2, KeyCode::KEY_KP3,
    KeyCode::KEY_KP4, KeyCode::KEY_KP5, KeyCode::KEY_KP6, KeyCode::KEY_KP7,
    KeyCode::KEY_KP8, KeyCode::KEY_KP9,
    KeyCode::KEY_KPSLASH, KeyCode::KEY_KPASTERISK, KeyCode::KEY_KPMINUS, KeyCode::KEY_KPPLUS,
    KeyCode::KEY_KPENTER, KeyCode::KEY_KPDOT,      KeyCode::KEY_KPEQUAL,
    // Modifiers (KEY_LEFTMETA used by LockScreen Super+L and Hyprland Super+Ctrl+L)
    KeyCode::KEY_LEFTCTRL, KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_LEFTALT, KeyCode::KEY_LEFTMETA,
    // Function keys
    KeyCode::KEY_F1,  KeyCode::KEY_F2,  KeyCode::KEY_F3,  KeyCode::KEY_F4,
    KeyCode::KEY_F5,  KeyCode::KEY_F6,  KeyCode::KEY_F7,  KeyCode::KEY_F8,
    KeyCode::KEY_F9,  KeyCode::KEY_F10, KeyCode::KEY_F11, KeyCode::KEY_F12,
    KeyCode::KEY_F13, KeyCode::KEY_F14, KeyCode::KEY_F15, KeyCode::KEY_F16,
    KeyCode::KEY_F17, KeyCode::KEY_F18, KeyCode::KEY_F19, KeyCode::KEY_F20,
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

    crate::uinput::builder(crate::uinput::Device::ActionInjector)?
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
///
/// A modifier a held chord keeps down on the device gets neither edge. The
/// kernel drops the repeated press, but the release would lift the held
/// chord's modifier while that chord is still held (#2413).
fn press_key(mods: &[KeyCode], key: KeyCode) {
    super::with_held_output(|held| {
        let mods = unheld_modifiers(mods, held);
        emit(&key_phase_events(&mods, key, KeyPhase::Down));
        emit(&key_phase_events(&mods, key, KeyPhase::Up));
    });
}

/// The keys of `mods` no held chord keeps down on the device.
fn unheld_modifiers(mods: &[KeyCode], held: &HeldOutput) -> Vec<KeyCode> {
    let held: Vec<KeyCode> = [
        HeldKey::Control,
        HeldKey::Shift,
        HeldKey::Alt,
        HeldKey::Super,
    ]
    .into_iter()
    .filter(|&key| held.owns(key))
    .filter_map(held_keycode)
    .collect();
    mods.iter()
        .copied()
        .filter(|code| !held.contains(code))
        .collect()
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

pub(super) fn post_scroll(delta: ScrollDelta) {
    let ScrollDelta::WheelTicks { .. } = delta else {
        tracing::debug!("pixel scroll output is unsupported on Linux");
        return;
    };
    let Ok(mut output) = SCROLL_OUTPUT.lock() else {
        tracing::warn!("Linux scroll quantizer mutex poisoned");
        return;
    };
    let events = scroll_events(&mut output, delta);
    drop(output);
    if !events.is_empty() {
        emit(&events);
    }
}

/// One `SYN_REPORT` frame carrying `delta` on the hi-res and legacy axes, or
/// nothing while the quantizers hold less than one unit.
fn scroll_events(output: &mut ScrollOutput, delta: ScrollDelta) -> Vec<InputEvent> {
    let high_resolution = output
        .high_resolution
        .quantize(delta, HIGH_RES_UNITS_PER_TICK);
    let legacy = output.legacy.quantize(delta, 1.0);
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
    }
    events
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
    if combo.has_super() {
        modifiers.push(KeyCode::KEY_LEFTMETA);
    }
    modifiers
}

fn held_keycode(key: HeldKey) -> Option<KeyCode> {
    match key {
        HeldKey::Control => Some(KeyCode::KEY_LEFTCTRL),
        HeldKey::Shift => Some(KeyCode::KEY_LEFTSHIFT),
        HeldKey::Alt => Some(KeyCode::KEY_LEFTALT),
        HeldKey::Super => Some(KeyCode::KEY_LEFTMETA),
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
        0x46 => Some(KeyCode::KEY_SYSRQ),
        0x49 => Some(KeyCode::KEY_INSERT),
        0x4a => Some(KeyCode::KEY_HOME),
        0x4b => Some(KeyCode::KEY_PAGEUP),
        0x4c => Some(KeyCode::KEY_DELETE),
        0x4d => Some(KeyCode::KEY_END),
        0x4e => Some(KeyCode::KEY_PAGEDOWN),
        0x4f => Some(KeyCode::KEY_RIGHT),
        0x50 => Some(KeyCode::KEY_LEFT),
        0x51 => Some(KeyCode::KEY_DOWN),
        0x52 => Some(KeyCode::KEY_UP),
        _ => hid_keypad_usage_to_linux(usage),
    }
}

/// Map a keypad USB HID usage to evdev.
fn hid_keypad_usage_to_linux(usage: u8) -> Option<KeyCode> {
    const KEYPAD_DIGITS: [KeyCode; 10] = [
        KeyCode::KEY_KP1,
        KeyCode::KEY_KP2,
        KeyCode::KEY_KP3,
        KeyCode::KEY_KP4,
        KeyCode::KEY_KP5,
        KeyCode::KEY_KP6,
        KeyCode::KEY_KP7,
        KeyCode::KEY_KP8,
        KeyCode::KEY_KP9,
        KeyCode::KEY_KP0,
    ];
    match usage {
        0x54 => Some(KeyCode::KEY_KPSLASH),
        0x55 => Some(KeyCode::KEY_KPASTERISK),
        0x56 => Some(KeyCode::KEY_KPMINUS),
        0x57 => Some(KeyCode::KEY_KPPLUS),
        0x58 => Some(KeyCode::KEY_KPENTER),
        0x59..=0x62 => KEYPAD_DIGITS.get(usize::from(usage - 0x59)).copied(),
        0x63 => Some(KeyCode::KEY_KPDOT),
        0x67 => Some(KeyCode::KEY_KPEQUAL),
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

/// Lock the session identified by `$XDG_SESSION_ID` via logind. Returns
/// `true` when the D-Bus call succeeded. Skips the bus entirely when the
/// session id is unset so we never lock every session on the machine.
fn try_logind_lock() -> bool {
    let (Some(conn), Ok(id)) = (SYSTEM_BUS.as_ref(), std::env::var("XDG_SESSION_ID")) else {
        return false;
    };
    match conn.call_method(
        Some("org.freedesktop.login1"),
        "/org/freedesktop/login1",
        Some("org.freedesktop.login1.Manager"),
        "LockSession",
        &(id.as_str(),),
    ) {
        Ok(_) => {
            tracing::debug!("LockScreen via logind");
            true
        }
        Err(e) => {
            tracing::warn!("logind LockSession failed: {e}");
            false
        }
    }
}

/// Lock the screen via logind `LockSession($XDG_SESSION_ID)` on the system
/// bus, falling back to Super+L.
///
/// Super+L covers non-systemd systems and the no-session-id case on GNOME/KDE.
fn lock_screen() {
    if try_logind_lock() {
        return;
    }
    // Super+L is the standard lock shortcut on GNOME and KDE.
    tracing::debug!("LockScreen via Super+L key combo");
    let chord = generic_linux_lock_fallback_chord();
    press_key(chord.modifiers, chord.key);
}

/// Hyprland lock: `omarchy-system-lock`, then Super+Ctrl+L.
///
/// Do not call logind `LockSession` here. A successful D-Bus reply only
/// signals the session; Hyprland does not listen, so treating OK as locked
/// skips the Omarchy helper and leaves the screen unlocked. Never Super+L —
/// that chord toggles workspace layout on Omarchy.
fn lock_screen_hyprland() {
    let helper = hyprland_lock_helper();
    run_helper_or_chord(helper, hyprland_lock_fallback_chord());
}

/// `$XDG_CURRENT_DESKTOP`, lowercased, or `None` if unset.
fn current_desktop() -> Option<String> {
    std::env::var("XDG_CURRENT_DESKTOP")
        .ok()
        .map(|d| d.to_lowercase())
}

/// The extra modifiers Print needs to open the region selector instead of
/// taking a full-screen shot, chosen from the (colon-separated)
/// `$XDG_CURRENT_DESKTOP` value.
///
/// GNOME >= 42 already opens the interactive selector on a bare Print, and
/// that is what an unset or unrecognised desktop keeps. Cinnamon, MATE and
/// XFCE bind region capture to Shift+Print; KDE Plasma binds it to
/// Meta+Shift+Print.
fn capture_region_mods(desktop: Option<&str>) -> Vec<KeyCode> {
    let Some(desktop) = desktop else {
        return Vec::new();
    };
    let mut desktops = desktop.split(':');
    if desktops.clone().any(|d| d == "kde") {
        vec![KeyCode::KEY_LEFTMETA, KeyCode::KEY_LEFTSHIFT]
    } else if desktops.any(|d| matches!(d, "x-cinnamon" | "cinnamon" | "mate" | "xfce")) {
        vec![KeyCode::KEY_LEFTSHIFT]
    } else {
        Vec::new()
    }
}

/// The modifiers that turn ←/→ into the previous/next-workspace chord, chosen
/// from the (colon-separated) `$XDG_CURRENT_DESKTOP` value.
///
/// Ctrl+Alt is GNOME's stock binding, and what an unset or unrecognised
/// desktop keeps. COSMIC and KDE Plasma (KWin's "Switch One Desktop to the
/// Left/Right") bind Super+Ctrl+←/→ instead and leave Ctrl+Alt+←/→ unbound.
fn desktop_switch_mods(desktop: Option<&str>) -> &'static [KeyCode] {
    if desktop.is_some_and(|desktop| desktop.split(':').any(|d| matches!(d, "cosmic" | "kde"))) {
        &[KeyCode::KEY_LEFTMETA, KeyCode::KEY_LEFTCTRL]
    } else {
        &[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_LEFTALT]
    }
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

fn is_kde() -> bool {
    current_desktop().is_some_and(|d| d.contains("kde"))
}

/// The KWin session bus, but only on KDE. KDE is identified by the XDG_CURRENT_DESKTOP
/// environment variable
fn kwin_bus() -> Option<&'static DbusConn> {
    if !is_kde() {
        return None;
    }
    SESSION_BUS.as_ref().or_else(|| {
        tracing::warn!("KDE session but no D-Bus session bus — KWin action skipped");
        None
    })
}

/// Trigger a named KWin effect (`Overview`, `ExposeClass`, …) through the
/// KDE global-shortcut component. No-op with a debug log off KDE.
fn kwin_invoke_shortcut(action: &Action, effect: &str) {
    let Some(conn) = kwin_bus() else {
        tracing::debug!(
            action = action.label(),
            "only implemented for KDE — action skipped"
        );
        return;
    };
    match conn.call_method(
        Some("org.kde.kglobalaccel"),
        "/component/kwin",
        Some("org.kde.kglobalaccel.Component"),
        "invokeShortcut",
        &(effect,),
    ) {
        Ok(_) => tracing::debug!(effect, "invoked KWin shortcut"),
        Err(e) => tracing::warn!("KWin invokeShortcut({effect}) failed: {e}"),
    }
}

/// Held across the read-then-write in [`kwin_toggle_show_desktop`]: a button
/// press and an Actions Ring selection run on different threads, and two
/// interleaved toggles would otherwise both read the same state and collapse
/// into one.
static SHOW_DESKTOP_TOGGLE: Mutex<()> = Mutex::new(());

/// Toggle KWin's "show desktop" state, reading the current value first so the
/// action mirrors the native toggle. No-op with a debug log off KDE.
fn kwin_toggle_show_desktop(action: &Action) {
    let Some(conn) = kwin_bus() else {
        tracing::debug!(
            action = action.label(),
            "only implemented for KDE — action skipped"
        );
        return;
    };
    let _toggle = SHOW_DESKTOP_TOGGLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let showing = match conn.call_method(
        Some("org.kde.KWin"),
        "/KWin",
        Some("org.freedesktop.DBus.Properties"),
        "Get",
        &("org.kde.KWin", "showingDesktop"),
    ) {
        Ok(reply) => match reply.body().deserialize::<zbus::zvariant::Value<'_>>() {
            Ok(value) => match value.downcast::<bool>() {
                Ok(showing) => showing,
                Err(e) => {
                    tracing::warn!("KWin showingDesktop decode failed: {e}");
                    return;
                }
            },
            Err(e) => {
                tracing::warn!("KWin showingDesktop decode failed: {e}");
                return;
            }
        },
        Err(e) => {
            tracing::warn!("KWin showingDesktop query failed: {e}");
            return;
        }
    };
    match conn.call_method(
        Some("org.kde.KWin"),
        "/KWin",
        Some("org.kde.KWin"),
        "showDesktop",
        &(!showing,),
    ) {
        Ok(_) => tracing::debug!(showing = !showing, "toggled KWin show-desktop"),
        Err(e) => tracing::warn!("KWin showDesktop failed: {e}"),
    }
}

/// Call a KWin virtual-desktop navigation method (`previousDesktop` /
/// `nextDesktop`). Returns `None` when the action was not handled — off KDE,
/// or when the D-Bus call failed — so the caller can fall back to a key combo.
fn kwin_switch_desktop(method: &str) -> Option<()> {
    let conn = kwin_bus()?;
    match conn.call_method(
        Some("org.kde.KWin"),
        "/KWin",
        Some("org.kde.KWin"),
        method,
        &(),
    ) {
        Ok(_) => {
            tracing::debug!(method, "switched virtual desktop via KWin");
            Some(())
        }
        Err(e) => {
            tracing::warn!("KWin {method} failed: {e}");
            None
        }
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
    /// Hand it to the desktop opener (`xdg-open`): URLs, folders, and
    /// documents.
    Opener,
    /// Activate a desktop entry: a `.desktop` file, or the entry an
    /// application name such as `Dolphin` names. `xdg-open` cannot be trusted
    /// with these: KDE opens a `.desktop` file as text (#2525).
    Entry(PathBuf),
    /// Run it as a program: an executable file path, or a bare command name
    /// resolved on `PATH`.
    Program(PathBuf),
}

/// Decide how `target` should be launched.
///
/// `is_executable`, `on_path` and `find_entry` are injected so the decision
/// table is testable without touching the real filesystem or `PATH`.
fn classify(
    target: &str,
    is_executable: &dyn Fn(&Path) -> bool,
    on_path: &dyn Fn(&str) -> Option<PathBuf>,
    find_entry: &dyn Fn(&str) -> Option<PathBuf>,
) -> Launch {
    if target.contains("://") {
        return Launch::Opener;
    }
    // A desktop entry is activated, not spawned: activation reads Exec= and
    // applies the entry's Terminal / StartupNotify keys.
    if Path::new(target)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("desktop"))
    {
        return Launch::Entry(PathBuf::from(target));
    }
    if target.contains('/') {
        let path = Path::new(target);
        return if is_executable(path) {
            Launch::Program(path.to_path_buf())
        } else {
            Launch::Opener
        };
    }
    // A bare name: a command on `PATH`, else the application it names, as
    // the GUI's free-text field invites ("Dolphin" for `dolphin`).
    on_path(target)
        .map(Launch::Program)
        .or_else(|| find_entry(target).map(Launch::Entry))
        .unwrap_or(Launch::Opener)
}

/// The `applications` directories desktop entries live in, most preferred
/// first: `$XDG_DATA_HOME` (default `~/.local/share`), then each of
/// `$XDG_DATA_DIRS` (default `/usr/local/share:/usr/share`).
fn application_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/share")));
    let data_dirs = std::env::var_os("XDG_DATA_DIRS")
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    home.into_iter()
        .chain(std::env::split_paths(&data_dirs))
        .map(|dir| dir.join("applications"))
        .collect()
}

/// The desktop entry an application `name` refers to, searching `dirs` in
/// order. An entry matches, ignoring case, by its file name
/// (`dolphin.desktop`), the last part of a reverse-DNS one
/// (`org.kde.dolphin.desktop`), or its untranslated `Name=` (`Dolphin`).
fn find_entry_in(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    dirs.iter().find_map(|dir| {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "desktop"))
            .collect();
        // read_dir order is arbitrary; sort so the same name always picks
        // the same entry.
        entries.sort();
        entries.into_iter().find(|path| entry_matches(path, name))
    })
}

/// Whether the desktop entry at `path` is the application `name`.
fn entry_matches(path: &Path, name: &str) -> bool {
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    let last = stem.rsplit('.').next().unwrap_or(stem);
    if stem.eq_ignore_ascii_case(name) || last.eq_ignore_ascii_case(name) {
        return true;
    }
    std::fs::read_to_string(path).is_ok_and(|text| {
        text.lines()
            .find_map(|line| line.strip_prefix("Name="))
            .is_some_and(|entry_name| entry_name.trim().eq_ignore_ascii_case(name.trim()))
    })
}

/// The desktop entry an application `name` refers to on this system.
fn find_entry(name: &str) -> Option<PathBuf> {
    find_entry_in(&application_dirs(), name)
}

/// Activate the desktop entry at `entry` with the first launcher installed:
/// GLib's `gio launch`, then KDE's `kioclient exec` (`kioclient5` on Plasma
/// 5). `false` when none is, so the caller falls back to the opener.
fn launch_entry(entry: &Path) -> bool {
    launch_entry_with(entry, on_path)
}

/// [`launch_entry`] with the launcher lookup injected, so tests can point it
/// at stand-in launchers.
fn launch_entry_with(entry: &Path, find: impl Fn(&str) -> Option<PathBuf>) -> bool {
    const LAUNCHERS: &[(&str, &str)] = &[
        ("gio", "launch"),
        ("kioclient", "exec"),
        ("kioclient5", "exec"),
    ];
    for (launcher, verb) in LAUNCHERS {
        let Some(program) = find(launcher) else {
            continue;
        };
        match std::process::Command::new(&program)
            .arg(verb)
            .arg(entry)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return true;
            }
            Err(error) => tracing::warn!(
                %error,
                launcher = %program.display(),
                entry = %entry.display(),
                "could not start the desktop-entry launcher"
            ),
        }
    }
    false
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

/// Every executable named `name` on `PATH`, in `PATH` order.
fn path_matches(name: &str) -> Vec<PathBuf> {
    std::env::var_os("PATH").map_or_else(Vec::new, |paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .filter(|candidate| is_executable(candidate))
            .collect()
    })
}

/// First executable named `name` on `PATH`.
fn on_path(name: &str) -> Option<PathBuf> {
    path_matches(name).into_iter().next()
}

/// Outcome of trying to spawn a program from a list of candidates.
#[derive(Debug, PartialEq, Eq)]
enum Spawn {
    Started(PathBuf),
    /// Every candidate was refused for permissions.
    PermissionDenied,
    /// No candidate started and at least one was refused for a reason other
    /// than permissions.
    NotRunnable,
}

/// Spawn the first candidate the kernel will run, moving past any it refuses
/// — the mode check behind [`is_executable`] cannot tell a file executable
/// only by another user, or a script whose interpreter is missing, from one
/// this user can run, so an earlier `PATH` match must not shadow a runnable
/// later one.
fn spawn_first(candidates: impl IntoIterator<Item = PathBuf>) -> Spawn {
    let mut outcome = Spawn::PermissionDenied;
    for program in candidates {
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
                return Spawn::Started(program);
            }
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                tracing::warn!(
                    %error,
                    program = %program.display(),
                    "the configured application is not executable by this user"
                );
            }
            // Any other refusal means the mode bit was misleading — a data
            // file with the bit set, a script with no interpreter, a path
            // that vanished. If no later candidate starts, those are the
            // desktop opener's business after all.
            Err(error) => {
                tracing::warn!(
                    %error,
                    program = %program.display(),
                    "not a runnable program"
                );
                outcome = Spawn::NotRunnable;
            }
        }
    }
    outcome
}

/// Run an "Open application" target as a program when that is what it is,
/// reporting whether it was handled here.
///
/// `xdg-open` — what `opener` calls on Linux — *opens* a file: handed
/// `/usr/bin/nautilus` it looks for something claiming
/// `application/x-executable` and, finding nothing, does nothing at all
/// (#775). A path to an executable, or a bare command name on `PATH`, has to
/// be spawned instead. A `.desktop` file, or an application name such as
/// `Dolphin` that matches one, is activated through a desktop-entry launcher:
/// KDE's `xdg-open` opens the file as text and sends a bare name to the
/// browser (#2525). URLs, folders and documents return `false` and stay with
/// the opener.
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
    let program = match classify(target, &is_executable, &on_path, &find_entry) {
        Launch::Opener => return false,
        Launch::Entry(entry) => return launch_entry(&entry),
        Launch::Program(program) => program,
    };
    let candidates = if target.contains('/') {
        vec![program]
    } else {
        path_matches(target)
    };
    // Not executable *by this user*: the opener cannot run it either, and
    // handing a binary to `xdg-open` risks it landing in a text editor.
    // Report it handled so nothing else is tried.
    !matches!(spawn_first(candidates), Spawn::NotRunnable)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use evdev::{KeyCode, RelativeAxisCode};
    use openlogi_core::binding::{HeldKeys, KeyCombo, KeyboardUsage, NativeAction, Shortcut};

    use std::path::PathBuf;
    use std::process::Stdio;

    use std::os::unix::fs::PermissionsExt as _;

    use super::{
        HELPER_PREFIXES, KEY_CAPABILITIES, Launch, ScrollOutput, Spawn, action_scroll,
        capture_region_mods, classify, combo, desktop_switch_mods, find_entry_in,
        generic_linux_lock_fallback_chord, hid_usage_to_linux, hyprland_helper,
        hyprland_lock_fallback_chord, hyprland_lock_helper, hyprland_session_from_signature,
        is_executable, key_ev, key_phase_events, launch_entry_with, launch_program,
        modifiers_to_keycodes, on_path, rel_ev, resolve_allowlisted_under, scroll_events,
        spawn_first, syn, unheld_modifiers, wait_with_retry,
    };
    use crate::inject::{HeldChordKind, HeldOutput, KeyPhase};

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

    /// A one-shot chord leaves out a modifier a held chord keeps down on the
    /// shared device: its release would lift that modifier while the chord is
    /// still held (#2413). Cmd shares Control's key, so a held Cmd chord
    /// keeps the tapped chord's Ctrl out too.
    #[test]
    fn one_shot_chord_leaves_held_chord_modifiers_alone() {
        let mods = [KeyCode::KEY_LEFTCTRL, KeyCode::KEY_LEFTSHIFT];
        assert_eq!(unheld_modifiers(&mods, &HeldOutput::default()), mods);

        let mut output = HeldOutput::default();
        let chord: KeyCombo = "Cmd+X".parse().expect("test chord must be valid");
        let chord = HeldChordKind::Chord(chord, HeldKeys::ModifiersOnly);
        let _ = output.transition(None, Some(&chord));
        assert_eq!(
            unheld_modifiers(&mods, &output),
            [KeyCode::KEY_LEFTSHIFT],
            "the held chord's Ctrl must get neither edge"
        );

        let _ = output.transition(Some(&chord), None);
        assert_eq!(
            unheld_modifiers(&mods, &output),
            mods,
            "a released chord no longer keeps its Ctrl out"
        );
    }

    /// #775: configuring `/usr/bin/nautilus` (or bare `nautilus`) did
    /// nothing, because `xdg-open` opens files rather than running them.
    #[test]
    fn executables_are_run_and_everything_else_goes_to_the_opener() {
        let executable = |p: &Path| p == Path::new("/usr/bin/nautilus");
        let on_path = |name: &str| (name == "nautilus").then(|| PathBuf::from("/usr/bin/nautilus"));
        let no_entry = |_: &str| None;

        assert_eq!(
            classify("/usr/bin/nautilus", &executable, &on_path, &no_entry),
            Launch::Program(PathBuf::from("/usr/bin/nautilus")),
            "an executable path is a program to run"
        );
        assert_eq!(
            classify("nautilus", &executable, &on_path, &no_entry),
            Launch::Program(PathBuf::from("/usr/bin/nautilus")),
            "a bare command name resolves through PATH"
        );

        for opener in [
            "https://example.com",
            "/home/u/Documents",
            "/home/u/notes.txt",
            "definitely-not-on-path",
        ] {
            assert_eq!(
                classify(opener, &executable, &on_path, &no_entry),
                Launch::Opener,
                "{opener} belongs to the desktop opener"
            );
        }
    }

    /// libinput reads only the hi-res axes of a device that has them, so a
    /// scroll action without them does nothing.
    #[test]
    fn scroll_actions_send_hi_res_and_legacy_axes_in_one_frame() {
        let (wheel, wheel_hi) = (
            RelativeAxisCode::REL_WHEEL,
            RelativeAxisCode::REL_WHEEL_HI_RES,
        );
        let (hwheel, hwheel_hi) = (
            RelativeAxisCode::REL_HWHEEL,
            RelativeAxisCode::REL_HWHEEL_HI_RES,
        );
        for (dx, dy, hi_res, legacy, sign) in [
            (0, 1, wheel_hi, wheel, 1),
            (0, -1, wheel_hi, wheel, -1),
            (1, 0, hwheel_hi, hwheel, 1),
            (-1, 0, hwheel_hi, hwheel, -1),
        ] {
            assert_eq!(
                scroll_events(&mut ScrollOutput::default(), action_scroll(dx, dy)),
                vec![rel_ev(hi_res, sign * 360), rel_ev(legacy, sign * 3), syn()],
                "scroll ({dx}, {dy})"
            );
        }
    }

    /// A path that classifies as a program but cannot be exec'd must report
    /// itself unhandled, so the caller still hands it to the desktop opener.
    /// A regular file with no execute bit for *this* user is the case the
    /// mode-based classification cannot see (#839 review).
    #[test]
    fn a_refused_spawn_is_reported_as_unhandled() {
        let dir = TempDir::new("launch");
        let path = dir.0.join("not-a-binary");
        std::fs::write(&path, b"\x7fELF this is not a loadable binary").expect("write file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("set mode");

        let target = path.to_str().expect("utf-8 temp path");
        assert_eq!(
            classify(target, &is_executable, &on_path, &|_: &str| None),
            Launch::Program(path.clone()),
            "the mode check classifies it as a program"
        );
        assert!(
            !launch_program(target),
            "an exec the kernel refuses must fall through to the opener"
        );
    }

    /// A file whose execute bit belongs to someone else classifies as a
    /// program but cannot be run here — and the desktop opener cannot run it
    /// either, so falling through to it would only repeat the failure with a
    /// binary in a text editor as the best case. Report it handled instead.
    #[test]
    fn an_unexecutable_program_is_not_passed_to_the_opener() {
        let dir = TempDir::new("eacces");
        let path = dir.0.join("no-permission");
        std::fs::write(&path, b"#!/bin/sh\ntrue\n").expect("write file");
        // Executable for group and other, never for the owner running this.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o011)).expect("set mode");

        if !exec_denied(&path) {
            return;
        }

        let target = path.to_str().expect("utf-8 temp path");
        assert_eq!(
            classify(target, &is_executable, &on_path, &|_: &str| None),
            Launch::Program(path.clone()),
            "the mode check still sees an execute bit"
        );
        assert!(
            launch_program(target),
            "an exec refused for permissions must not fall through to the opener"
        );
    }

    /// An earlier `PATH` match this user cannot run must not shadow a
    /// runnable later one.
    #[test]
    fn a_permission_refusal_moves_on_to_the_next_candidate() {
        let dir = TempDir::new("shadow");
        let denied = dir.0.join("true");
        std::fs::write(&denied, b"#!/bin/sh\ntrue\n").expect("write file");
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o011))
            .expect("set mode");
        if !exec_denied(&denied) {
            return;
        }
        let runnable = on_path("true").expect("true is on PATH");

        assert_eq!(
            spawn_first([denied.clone(), runnable.clone()]),
            Spawn::Started(runnable)
        );
        assert_eq!(spawn_first([denied]), Spawn::PermissionDenied);
    }

    /// An earlier `PATH` match whose interpreter is missing must not shadow
    /// a runnable later one; with nothing after it, it still falls back to
    /// the opener.
    #[test]
    fn a_missing_interpreter_moves_on_to_the_next_candidate() {
        let dir = TempDir::new("interp");
        let broken = dir.0.join("true");
        std::fs::write(&broken, b"#!/nonexistent/openlogi-interpreter\n").expect("write file");
        std::fs::set_permissions(&broken, std::fs::Permissions::from_mode(0o755))
            .expect("set mode");
        let runnable = on_path("true").expect("true is on PATH");

        assert_eq!(
            spawn_first([broken.clone(), runnable.clone()]),
            Spawn::Started(runnable)
        );
        assert_eq!(spawn_first([broken]), Spawn::NotRunnable);
    }

    /// Whether the kernel refuses to exec `path` for permissions. Root (or
    /// any process with `CAP_DAC_OVERRIDE`) can run a file with only group
    /// and other execute bits, so permission-denial tests skip there.
    fn exec_denied(path: &Path) -> bool {
        match std::process::Command::new(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                let _ = child.wait();
                false
            }
            Err(error) => error.kind() == std::io::ErrorKind::PermissionDenied,
        }
    }

    /// A fresh `0o700` directory, created exclusively so a pre-planted path or
    /// symlink fails the test instead of redirecting it; removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            use std::os::unix::fs::DirBuilderExt as _;
            use std::sync::atomic::{AtomicU32, Ordering};

            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos());
            let dir = std::env::temp_dir().join(format!(
                "openlogi-{tag}-{}-{}-{nanos}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed),
            ));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&dir)
                .expect("create temp dir");
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
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
            classify("sh", &is_executable, &on_path, &|_: &str| None),
            Launch::Program(resolved)
        );
        assert_eq!(
            classify("/", &is_executable, &on_path, &|_: &str| None),
            Launch::Opener
        );
    }

    /// A `.desktop` entry is executable often enough that the extension check
    /// has to come first: activating the entry reads its `Exec=` and honours
    /// `Terminal=` and `StartupNotify=`, which spawning the file itself would
    /// not.
    #[test]
    fn an_executable_desktop_entry_is_activated_not_spawned() {
        assert_eq!(
            classify(
                "/usr/share/applications/foo.desktop",
                &|_: &Path| true,
                &|_: &str| None,
                &|_: &str| None
            ),
            Launch::Entry(PathBuf::from("/usr/share/applications/foo.desktop"))
        );
    }

    /// #2525: the GUI saves what the user typed, often the application's
    /// display name. A name not on `PATH` resolves to its desktop entry.
    #[test]
    fn an_application_name_resolves_to_its_desktop_entry() {
        let entry = PathBuf::from("/usr/share/applications/org.kde.dolphin.desktop");
        let find_entry = |name: &str| (name == "Dolphin").then(|| entry.clone());
        assert_eq!(
            classify("Dolphin", &|_: &Path| false, &|_: &str| None, &find_entry),
            Launch::Entry(entry.clone())
        );
        assert_eq!(
            classify("Nothing", &|_: &Path| false, &|_: &str| None, &find_entry),
            Launch::Opener
        );
    }

    /// An entry matches by file name, by the last part of a reverse-DNS file
    /// name, or by its untranslated `Name=`, ignoring case; earlier
    /// directories win.
    #[test]
    fn desktop_entries_match_by_file_name_or_name_key() {
        let user = TempDir::new("apps-user");
        let system = TempDir::new("apps-system");
        let write = |dir: &Path, file: &str, name: &str| {
            let path = dir.join(file);
            std::fs::write(
                &path,
                format!("[Desktop Entry]\nType=Application\nName={name}\nName[de]=Anders\n"),
            )
            .expect("write entry");
            path
        };
        let dolphin = write(&system.0, "org.kde.dolphin.desktop", "Dolphin");
        let editor = write(&system.0, "org.example.Writer.desktop", "Text Editor");
        let shadow = write(&user.0, "dolphin.desktop", "My Dolphin");
        let dirs = [user.0.clone(), system.0.clone()];
        let only_system = [system.0.clone()];

        assert_eq!(
            find_entry_in(&only_system, "dolphin"),
            Some(dolphin.clone())
        );
        assert_eq!(find_entry_in(&only_system, "Dolphin"), Some(dolphin));
        assert_eq!(find_entry_in(&only_system, "text editor"), Some(editor));
        assert_eq!(
            find_entry_in(&only_system, "Anders"),
            None,
            "translations are ignored"
        );
        assert_eq!(
            find_entry_in(&dirs, "Dolphin"),
            Some(shadow),
            "the user's entry wins"
        );
        assert_eq!(find_entry_in(&dirs, "konsole"), None);
    }

    /// A desktop entry is handed to the first launcher installed — `gio
    /// launch`, else `kioclient exec` — and with none the caller is told to
    /// fall back to the opener.
    #[test]
    fn a_desktop_entry_goes_to_the_first_installed_launcher() {
        let bin = TempDir::new("launchers");
        let log = bin.0.join("ran");
        for name in ["gio", "kioclient"] {
            let path = bin.0.join(name);
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\nprintf '%s %s %s' \"$(basename \"$0\")\" \"$1\" \"$2\" > {}\n",
                    log.display()
                ),
            )
            .expect("write launcher");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("make launcher executable");
        }
        let entry = Path::new("/usr/share/applications/org.kde.dolphin.desktop");
        let ran = || {
            for _ in 0..100 {
                if let Ok(text) = std::fs::read_to_string(&log)
                    && !text.is_empty()
                {
                    std::fs::remove_file(&log).ok();
                    return text;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            panic!("no launcher ran");
        };

        let installed = |names: &'static [&'static str]| {
            let bin = bin.0.clone();
            move |name: &str| names.contains(&name).then(|| bin.join(name))
        };
        assert!(launch_entry_with(entry, installed(&["gio", "kioclient"])));
        assert_eq!(ran(), format!("gio launch {}", entry.display()));
        assert!(launch_entry_with(entry, installed(&["kioclient"])));
        assert_eq!(ran(), format!("kioclient exec {}", entry.display()));
        assert!(
            !launch_entry_with(entry, installed(&[])),
            "no launcher sends the entry to the opener"
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
        assert_eq!(hid_usage_to_linux(0x46), Some(KeyCode::KEY_SYSRQ));
        assert_eq!(hid_usage_to_linux(0x49), Some(KeyCode::KEY_INSERT));
        assert_eq!(hid_usage_to_linux(0x56), Some(KeyCode::KEY_KPMINUS));
        assert_eq!(hid_usage_to_linux(0x57), Some(KeyCode::KEY_KPPLUS));
        assert_eq!(hid_usage_to_linux(0x58), Some(KeyCode::KEY_KPENTER));
        assert_eq!(hid_usage_to_linux(0x59), Some(KeyCode::KEY_KP1));
        assert_eq!(hid_usage_to_linux(0x62), Some(KeyCode::KEY_KP0));
        assert_eq!(hid_usage_to_linux(0x67), Some(KeyCode::KEY_KPEQUAL));
        assert_eq!(hid_usage_to_linux(0xff), None);
    }

    /// Every shortcut usage must map to a key the uinput device registered,
    /// or the kernel drops the event and the shortcut silently does nothing.
    #[test]
    fn every_shortcut_usage_maps_to_a_registered_key() {
        for code in 0..=u8::MAX {
            if KeyboardUsage::try_new(code).is_err() {
                continue;
            }
            let key = hid_usage_to_linux(code);
            assert!(key.is_some(), "usage {code:#04x} has no evdev code");
            assert!(
                key.is_some_and(|key| KEY_CAPABILITIES.contains(&key)),
                "usage {code:#04x} maps to {key:?}, which the uinput device does not register"
            );
        }
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

    #[test]
    fn hyprland_gate_is_signature_only() {
        assert!(!hyprland_session_from_signature(None));
        assert!(!hyprland_session_from_signature(Some("")));
        assert!(hyprland_session_from_signature(Some(
            "wayland-1_1234567890_1111111111"
        )));
    }

    #[test]
    fn hyprland_launchpad_maps_to_absolute_omarchy_menu_toggle() {
        let (helper, fallback) = hyprland_helper(NativeAction::LaunchpadShow)
            .expect("LaunchpadShow must have a Hyprland helper");
        assert_eq!(helper.program, "omarchy-menu");
        assert_eq!(helper.args, ["toggle"]);
        assert_eq!(HELPER_PREFIXES, ["/usr/bin", "/usr/local/bin"]);
        assert_eq!(fallback.modifiers, [KeyCode::KEY_LEFTMETA]);
        assert_eq!(fallback.key, KeyCode::KEY_SPACE);
    }

    #[test]
    fn hyprland_desktops_map_to_hyprctl_workspace() {
        let (next, next_fallback) = hyprland_helper(NativeAction::NextDesktop)
            .expect("NextDesktop must have a Hyprland helper");
        assert_eq!(next.program, "hyprctl");
        assert_eq!(next.args, ["dispatch", "workspace", "e+1"]);
        assert_eq!(
            next.retry_args,
            Some(&["dispatch", r#"hl.dsp.focus({ workspace = "e+1" })"#][..])
        );
        assert_eq!(next_fallback.modifiers, [KeyCode::KEY_LEFTMETA]);
        assert_eq!(next_fallback.key, KeyCode::KEY_TAB);

        let (prev, prev_fallback) = hyprland_helper(NativeAction::PreviousDesktop)
            .expect("PreviousDesktop must have a Hyprland helper");
        assert_eq!(prev.program, "hyprctl");
        assert_eq!(prev.args, ["dispatch", "workspace", "e-1"]);
        assert_eq!(
            prev.retry_args,
            Some(&["dispatch", r#"hl.dsp.focus({ workspace = "e-1" })"#][..])
        );
        assert_eq!(
            prev_fallback.modifiers,
            [KeyCode::KEY_LEFTMETA, KeyCode::KEY_LEFTSHIFT]
        );
        assert_eq!(prev_fallback.key, KeyCode::KEY_TAB);
    }

    #[test]
    fn a_failed_helper_run_is_retried_once_with_its_retry_args() {
        let sh = Path::new("/bin/sh");
        let spawn = |code: &str| {
            Command::new(sh)
                .args(["-c", &format!("exit {code}")])
                .spawn()
                .expect("spawn /bin/sh")
        };
        assert!(wait_with_retry(spawn("0"), sh, None));
        // A successful first run must not run the retry: here it would hang
        // until the helper timeout, so returning at once proves it was skipped.
        let started = std::time::Instant::now();
        assert!(wait_with_retry(spawn("0"), sh, Some(&["-c", "sleep 10"])));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(!wait_with_retry(spawn("7"), sh, None));
        assert!(wait_with_retry(spawn("7"), sh, Some(&["-c", "exit 0"])));
        assert!(!wait_with_retry(spawn("7"), sh, Some(&["-c", "exit 7"])));
    }

    #[test]
    fn hyprland_mac_only_natives_stay_skipped() {
        assert_eq!(hyprland_helper(NativeAction::MissionControl), None);
        assert_eq!(hyprland_helper(NativeAction::AppExpose), None);
        assert_eq!(hyprland_helper(NativeAction::ShowDesktop), None);
        assert_eq!(hyprland_helper(NativeAction::LockScreen), None);
        assert_eq!(hyprland_helper(NativeAction::Screenshot), None);
    }

    #[test]
    fn hyprland_lock_uses_omarchy_helper_not_logind() {
        let helper = hyprland_lock_helper();
        assert_eq!(helper.program, "omarchy-system-lock");
        assert_eq!(helper.args, [] as [&str; 0]);
        assert_eq!(HELPER_PREFIXES, ["/usr/bin", "/usr/local/bin"]);
    }

    #[test]
    fn hyprland_lock_fallback_is_never_super_l() {
        let hyprland = hyprland_lock_fallback_chord();
        assert_eq!(
            hyprland.modifiers,
            [KeyCode::KEY_LEFTMETA, KeyCode::KEY_LEFTCTRL]
        );
        assert_eq!(hyprland.key, KeyCode::KEY_L);

        let generic = generic_linux_lock_fallback_chord();
        assert_eq!(generic.modifiers, [KeyCode::KEY_LEFTMETA]);
        assert_eq!(generic.key, KeyCode::KEY_L);
        assert_ne!(hyprland.modifiers, generic.modifiers);
    }

    #[test]
    fn allowlisted_helpers_are_absolute_and_reject_path_escape() {
        let dir =
            std::env::temp_dir().join(format!("openlogi-inject-allowlist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp allowlist dir");
        let helper = dir.join("omarchy-menu");
        std::fs::write(&helper, []).expect("placeholder helper file");

        let prefix = dir.to_str().expect("utf-8 temp path");
        let resolved = resolve_allowlisted_under("omarchy-menu", &[prefix]);
        assert_eq!(resolved.as_deref(), Some(helper.as_path()));
        assert!(resolved.expect("resolved helper").is_absolute());
        assert_eq!(
            resolve_allowlisted_under("../omarchy-menu", &[prefix]),
            None
        );
        assert_eq!(
            resolve_allowlisted_under("omarchy-menu/../../bin/sh", &[prefix]),
            None
        );
        assert_eq!(
            resolve_allowlisted_under("omarchy-menu", &["/no/such/openlogi-prefix"]),
            None
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gnome_detection_matches_on_current_desktop_case_insensitively() {
        for (value, expected) in [
            ("GNOME", true),
            ("ubuntu:GNOME", true),
            ("gnome-classic", true),
            ("KDE", false),
            ("", false),
        ] {
            assert_eq!(
                super::desktop_is_gnome(value),
                expected,
                "XDG_CURRENT_DESKTOP={value:?}"
            );
        }
    }

    #[test]
    fn capture_region_mods_picks_the_desktops_chord() {
        assert_eq!(capture_region_mods(None), vec![]);
        assert_eq!(capture_region_mods(Some("gnome")), vec![]);
        assert_eq!(capture_region_mods(Some("unity")), vec![]);
        assert_eq!(
            capture_region_mods(Some("x-cinnamon")),
            vec![KeyCode::KEY_LEFTSHIFT]
        );
        assert_eq!(
            capture_region_mods(Some("mate")),
            vec![KeyCode::KEY_LEFTSHIFT]
        );
        assert_eq!(
            capture_region_mods(Some("xfce")),
            vec![KeyCode::KEY_LEFTSHIFT]
        );
        assert_eq!(
            capture_region_mods(Some("kde")),
            vec![KeyCode::KEY_LEFTMETA, KeyCode::KEY_LEFTSHIFT]
        );
        // GNOME-on-Ubuntu reports "ubuntu:gnome" — the compound string must
        // still resolve to the GNOME (no-op) branch, not fall through.
        assert_eq!(capture_region_mods(Some("ubuntu:gnome")), vec![]);
    }

    #[test]
    fn desktop_switch_mods_picks_the_desktops_chord() {
        let ctrl_alt: &[KeyCode] = &[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_LEFTALT];
        let super_ctrl: &[KeyCode] = &[KeyCode::KEY_LEFTMETA, KeyCode::KEY_LEFTCTRL];
        for (desktop, expected) in [
            (None, ctrl_alt),
            (Some("gnome"), ctrl_alt),
            (Some("ubuntu:gnome"), ctrl_alt),
            (Some("cosmic"), super_ctrl),
            // Any entry of the colon-separated list selects COSMIC's chord...
            (Some("pop:cosmic"), super_ctrl),
            // ...but only a whole entry, not a substring of another name.
            (Some("cosmic-like"), ctrl_alt),
            // KWin binds Ctrl+Meta+←/→, not GNOME's Ctrl+Alt (#2487). This is
            // the chord KDE presses when the KWin D-Bus call is not handled.
            (Some("kde"), super_ctrl),
            (Some("kde-like"), ctrl_alt),
        ] {
            assert_eq!(
                desktop_switch_mods(desktop),
                expected,
                "XDG_CURRENT_DESKTOP={desktop:?}"
            );
        }
    }
}
