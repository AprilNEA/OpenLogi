//! Windows helpers for synthesising OS-level input events via `SendInput`.
#![expect(
    unsafe_code,
    reason = "SendInput and the low-level keyboard hook are the Win32 APIs for synthetic input and for watching it"
)]

use std::mem::size_of;
use std::os::windows::process::CommandExt;
use std::sync::{LazyLock, Mutex};

use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{XBUTTON1, XBUTTON2};

use openlogi_core::binding::{
    Action, Effect, KeyCombo, KeyboardUsage, MediaKey, MouseButton, NativeAction, Shortcut,
};
use openlogi_core::scroll::ScrollDelta;

use super::{HeldKey, KeyPhase, ScrollQuantizer};

mod keyboard_layout;

use keyboard_layout::ForegroundLayout;

const WHEEL_DELTA: i32 = 120;
const WHEEL_DELTA_F64: f64 = 120.0;

static SCROLL_QUANTIZER: LazyLock<Mutex<ScrollQuantizer>> =
    LazyLock::new(|| Mutex::new(ScrollQuantizer::default()));

const VK_D: u16 = 0x44;
const VK_L: u16 = 0x4C;
const VK_S: u16 = 0x53;
const VK_TAB: u16 = 0x09;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;
const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;
const VK_LWIN: u16 = 0x5B;
const VK_SNAPSHOT: u16 = 0x2C;
const VK_BROWSER_BACK: u16 = 0xA6;
const VK_BROWSER_FORWARD: u16 = 0xA7;
const VK_VOLUME_MUTE: u16 = 0xAD;
const VK_VOLUME_DOWN: u16 = 0xAE;
const VK_VOLUME_UP: u16 = 0xAF;
const VK_MEDIA_NEXT_TRACK: u16 = 0xB0;
const VK_MEDIA_PREV_TRACK: u16 = 0xB1;
const VK_MEDIA_PLAY_PAUSE: u16 = 0xB3;

/// The rest of the extended keys this backend can produce, from
/// WinUser.h. The file codes virtual keys as plain numbers, like the
/// `VK_*` constants above.
const VK_PRIOR: u16 = 0x21;
const VK_NEXT: u16 = 0x22;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_INSERT: u16 = 0x2D;
const VK_DELETE: u16 = 0x2E;

/// One key as [`key_input`] posts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    vk: u16,
    /// The hardware scan code a physical press of the key carries, or 0 to
    /// post the virtual key alone.
    scan: u16,
}

impl Key {
    /// A virtual key posted alone, with scan code 0.
    ///
    /// Only for a key that is not extended: [`key_input`] derives the extended
    /// flag from the virtual key alone, so an extended key built this way would
    /// go out flagged with no scan code, a form no keyboard sends. Extended keys
    /// come from [`Self::for_vk`] or [`Self::for_usage`], which look their scan
    /// code up.
    fn plain(vk: u16) -> Self {
        debug_assert!(
            !is_extended_key(vk),
            "an extended key needs its scan code: build it with Key::for_vk or Key::for_usage"
        );
        Self { vk, scan: 0 }
    }

    /// The key for `usage` under `layout`, or `None` when the usage has no
    /// Windows virtual key.
    ///
    /// A key a layout can move ([`KeyboardUsage::ascii_char`]) carries the
    /// scan code of the key `layout` produces its virtual key with, as a
    /// physical press does (see [`keyboard_layout`]). An extended key needs one
    /// for the same reason (see [`is_extended_key`]). Every other key keeps
    /// scan code 0, as it always has.
    fn for_usage(usage: KeyboardUsage, layout: ForegroundLayout) -> Option<Self> {
        let vk = super::hid_usage_to_windows(usage.code())?;
        if usage.ascii_char().is_none() && !is_extended_key(vk) {
            return Some(Self::plain(vk));
        }
        Some(Self {
            vk,
            scan: layout.scan_code(vk),
        })
    }

    /// The key for a bare virtual key, as `post_vk` and `held_key` pass it.
    ///
    /// Extended keys get the scan code `layout` gives them; every other key is
    /// posted alone, as before.
    fn for_vk(vk: u16, layout: ForegroundLayout) -> Self {
        if is_extended_key(vk) {
            return Self {
                vk,
                scan: layout.scan_code(vk),
            };
        }
        Self::plain(vk)
    }
}

/// Whether `vk` needs the extended-key flag and a scan code.
///
/// The dedicated arrow and editing clusters share their scan codes with the
/// numeric keypad, so a scan code on its own is ambiguous: `0x4d` is both the
/// dedicated Right arrow and the keypad's 6. A physical keyboard tells the two
/// apart with the extended prefix, so an injected key has to carry the prefix
/// too.
///
/// The set is the extended keys this backend can actually produce: the
/// navigation and editing cluster (the HID usage table maps `0x49..=0x52` onto
/// them) and the left Windows key (`NativeAction::LaunchpadShow`, and the
/// `Super` modifier of a custom shortcut). Keys the backend cannot reach are
/// left out on purpose — flagging a key nobody injects only widens the blast
/// radius.
///
/// `VK_SNAPSHOT` is deliberately absent. A physical Print Screen key sends
/// `E0 37`, but `MapVirtualKeyW` answers `0x54` for it, a scan code no keyboard
/// sends; injecting that would put a code into the system that the hardware
/// never produces. Num Lock only remaps the keypad keys that double as
/// navigation keys, and Print Screen is not one of them, so the problem from
/// #1895 does not apply. It keeps the plain virtual-key path that
/// `NativeAction::Screenshot` has always used.
///
/// The `match` keeps the set in one place: adding a virtual key to the Windows
/// backend means adding an arm here, and a renamed constant breaks the build
/// instead of silently dropping the flag.
fn is_extended_key(vk: u16) -> bool {
    matches!(
        vk,
        VK_LEFT
            | VK_UP
            | VK_RIGHT
            | VK_DOWN
            | VK_HOME
            | VK_END
            | VK_PRIOR
            | VK_NEXT
            | VK_INSERT
            | VK_DELETE
            | VK_LWIN
    )
}

/// Windows implementation: classify `action` into an [`Effect`] and
/// synthesise events via `SendInput`. macOS window-manager actions map to
/// their Windows equivalents; `CustomShortcut` maps macOS `kVK_*` codes to
/// Windows virtual-key codes (Cmd → Ctrl).
pub(super) fn execute(action: &Action) {
    match action.effect() {
        Effect::None => {}
        Effect::Click(button) => post_click(button),
        Effect::Shortcut(shortcut) => press_shortcut(shortcut),
        Effect::Key(combo) | Effect::HeldKey(combo) => press_combo(combo),
        Effect::Scroll { dx, dy } => dispatch_scroll(dx, dy),
        Effect::Media(key) => dispatch_media(key),
        Effect::Native(native) => dispatch_native(native),
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

/// The Windows chord for a named [`Shortcut`], or the raw virtual key to
/// post with no modifier when Windows has no chord-shaped representation
/// for it at all.
///
/// `BrowserBack`/`BrowserForward` are Windows' one exception: they fire a
/// dedicated virtual key (`VK_BROWSER_BACK`/`_FORWARD`) with no modifier,
/// which isn't a USB HID keyboard usage and so has no [`KeyCombo`]
/// representation — unlike on macOS/Linux, where the same shortcuts are
/// ordinary modifier+key chords. [`press_shortcut`] posts the `Err` case
/// directly instead of routing it through [`press_combo`].
fn combo(shortcut: Shortcut) -> Result<KeyCombo, u16> {
    let text = match shortcut {
        Shortcut::BrowserBack => return Err(VK_BROWSER_BACK),
        Shortcut::BrowserForward => return Err(VK_BROWSER_FORWARD),
        Shortcut::Copy => "Ctrl+C",
        Shortcut::Paste => "Ctrl+V",
        Shortcut::Cut => "Ctrl+X",
        Shortcut::Undo => "Ctrl+Z",
        // Ctrl+Y, not Ctrl+Shift+Z: matches the dominant Windows convention
        // (Office, most Win32/UWP apps) rather than the macOS ⌘⇧Z one.
        Shortcut::Redo => "Ctrl+Y",
        Shortcut::SelectAll => "Ctrl+A",
        Shortcut::Find => "Ctrl+F",
        Shortcut::Save => "Ctrl+S",
        Shortcut::NewTab => "Ctrl+T",
        Shortcut::CloseTab => "Ctrl+W",
        Shortcut::ReopenTab => "Ctrl+Shift+T",
        Shortcut::NextTab => "Ctrl+Tab",
        Shortcut::PrevTab => "Ctrl+Shift+Tab",
        Shortcut::ReloadPage => "Ctrl+R",
    };
    Ok(super::parse_shortcut(text))
}

fn press_shortcut(shortcut: Shortcut) {
    match combo(shortcut) {
        Ok(combo) => press_combo(&combo),
        Err(vk) => post_vk(vk, &[]),
    }
}

/// Dispatch a window-manager or power [`NativeAction`]. macOS window-manager
/// concepts map to their nearest Windows shortcut; `Sleep` has no clean
/// synthesis (see the comment below) and is skipped.
fn dispatch_native(native: NativeAction) {
    match native {
        NativeAction::MissionControl | NativeAction::AppExpose => post_vk(VK_TAB, &[VK_LWIN]),
        NativeAction::PreviousDesktop => post_vk(VK_LEFT, &[VK_LWIN, VK_CONTROL]),
        NativeAction::NextDesktop => post_vk(VK_RIGHT, &[VK_LWIN, VK_CONTROL]),
        NativeAction::ShowDesktop => post_vk(VK_D, &[VK_LWIN]),
        NativeAction::LaunchpadShow => post_vk(VK_LWIN, &[]),
        NativeAction::LockScreen => post_vk(VK_L, &[VK_LWIN]),
        // Win+PrtScn saves a full-screen capture straight to
        // `Pictures\Screenshots` with no further click, like ⌘⇧3 on macOS.
        NativeAction::Screenshot => post_vk(VK_SNAPSHOT, &[VK_LWIN]),
        // Win+Shift+S opens the snip overlay, which waits for a region. It
        // used to serve `Screenshot` too, so the two actions behaved the
        // same (#1864).
        NativeAction::CaptureRegion => post_vk(VK_S, &[VK_LWIN, VK_SHIFT]),
        // Suspending reliably needs `SetSuspendState` (powrprof.dll), which
        // hibernates instead when hibernation is enabled — no clean win from
        // a background agent, so the action is skipped on Windows for now.
        NativeAction::Sleep => {
            tracing::debug!("Sleep has no Windows synthesis yet — action skipped");
        }
    }
}

fn dispatch_media(key: MediaKey) {
    match key {
        MediaKey::PlayPause => post_vk(VK_MEDIA_PLAY_PAUSE, &[]),
        MediaKey::NextTrack => post_vk(VK_MEDIA_NEXT_TRACK, &[]),
        MediaKey::PrevTrack => post_vk(VK_MEDIA_PREV_TRACK, &[]),
        MediaKey::VolumeUp => post_vk(VK_VOLUME_UP, &[]),
        MediaKey::VolumeDown => post_vk(VK_VOLUME_DOWN, &[]),
        MediaKey::Mute => post_vk(VK_VOLUME_MUTE, &[]),
    }
}

/// Not implemented yet: SendInput's KEYEVENTF_UNICODE path is unwired.
pub(super) fn type_text(text: &str) {
    tracing::warn!(
        chars = text.chars().count(),
        "TypeText injection is not implemented on Windows yet"
    );
}

pub(super) fn run_apple_script(_src: &str) {
    tracing::warn!("RunAppleScript is only supported on macOS");
}

pub(super) fn run_shell_command(cmd: &str) {
    // Without this flag `cmd.exe` allocates and briefly flashes a visible
    // console window, since the agent itself is a console-less GUI/background
    // process and the child would otherwise get one of its own.
    let _ = std::process::Command::new("cmd")
        .args(["/C", cmd])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}

fn post_click(button: MouseButton) {
    let (down, up, data) = match button {
        MouseButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
        MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
        MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
        // Extra buttons share the X flag pair; mouseData carries which one.
        MouseButton::Back => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, i32::from(XBUTTON1)),
        MouseButton::Forward => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, i32::from(XBUTTON2)),
    };
    send_inputs(&[mouse_input(down, data), mouse_input(up, data)]);
}

/// Post the already-resolved key `key` with `modifiers` held around it.
fn post_key(key: Key, modifiers: &[u16]) {
    post_key_in(key, modifiers, ForegroundLayout::current());
}

/// Post the bare virtual key `vk` with `modifiers` held around it.
///
/// The key goes through [`Key::for_vk`] for the same reason the modifiers do:
/// an extended key carries its hardware scan code, as a physical press does
/// (see [`is_extended_key`]).
fn post_vk(vk: u16, modifiers: &[u16]) {
    let layout = ForegroundLayout::current();
    post_key_in(Key::for_vk(vk, layout), modifiers, layout);
}

fn post_key_in(key: Key, modifiers: &[u16], layout: ForegroundLayout) {
    let mut inputs = Vec::with_capacity(modifiers.len() * 2 + 2);
    for modifier in modifiers {
        inputs.push(key_input(Key::for_vk(*modifier, layout), false));
    }
    inputs.push(key_input(key, false));
    inputs.push(key_input(key, true));
    for modifier in modifiers.iter().rev() {
        inputs.push(key_input(Key::for_vk(*modifier, layout), true));
    }
    send_inputs(&inputs);
}

/// Synthesise one scroll tick in direction `(dx, dy)`. Unit direction
/// (-1/0/1) scaled by `WHEEL_DELTA`, the fixed magnitude the four
/// `Scroll*`/`HorizontalScroll*` actions have always used.
fn dispatch_scroll(dx: i8, dy: i8) {
    let mut inputs = Vec::with_capacity(2);
    if dy != 0 {
        inputs.push(mouse_input(MOUSEEVENTF_WHEEL, i32::from(dy) * WHEEL_DELTA));
    }
    if dx != 0 {
        inputs.push(mouse_input(MOUSEEVENTF_HWHEEL, i32::from(dx) * WHEEL_DELTA));
    }
    if !inputs.is_empty() {
        send_inputs(&inputs);
    }
}

pub(super) fn post_scroll(delta: ScrollDelta) {
    let ScrollDelta::WheelTicks { .. } = delta else {
        tracing::debug!("pixel scroll output is unsupported on Windows");
        return;
    };
    let Ok(mut quantizer) = SCROLL_QUANTIZER.lock() else {
        tracing::warn!("Windows scroll quantizer mutex poisoned");
        return;
    };
    let delta = quantizer.quantize(delta, WHEEL_DELTA_F64);
    drop(quantizer);

    let mut inputs = Vec::with_capacity(2);
    if delta.y != 0 {
        inputs.push(mouse_input(MOUSEEVENTF_WHEEL, delta.y));
    }
    if delta.x != 0 {
        inputs.push(mouse_input(MOUSEEVENTF_HWHEEL, delta.x));
    }
    if !inputs.is_empty() {
        send_inputs(&inputs);
    }
}

pub(super) fn press_combo(combo: &KeyCombo) {
    let Some(key) = Key::for_usage(combo.key(), ForegroundLayout::current()) else {
        tracing::warn!(
            usage = combo.key().code(),
            chord = %combo.rendered_label(),
            "CustomShortcut key has no Windows mapping yet; press ignored"
        );
        return;
    };

    post_key(key, &combo_modifiers(combo));
}

fn combo_modifiers(combo: &KeyCombo) -> Vec<u16> {
    let mut modifiers = Vec::new();
    if combo.has_command() {
        modifiers.push(VK_CONTROL);
    }
    if combo.has_shift() {
        modifiers.push(VK_SHIFT);
    }
    if combo.has_control() && !modifiers.contains(&VK_CONTROL) {
        modifiers.push(VK_CONTROL);
    }
    if combo.has_option() {
        modifiers.push(VK_MENU);
    }
    if combo.has_super() {
        modifiers.push(VK_LWIN);
    }
    modifiers
}

/// Emit one edge for the physical keys whose ownership changed.
pub(super) fn hold_keys(keys: &[HeldKey], phase: KeyPhase) {
    let layout = ForegroundLayout::current();
    let keys: Vec<_> = keys
        .iter()
        .filter_map(|key| held_key(*key, layout))
        .collect();
    let key_up = phase == KeyPhase::Up;
    let mut inputs: Vec<_> = keys.iter().map(|key| key_input(*key, key_up)).collect();
    if key_up {
        inputs.reverse();
    }
    send_inputs(&inputs);
}

/// The key a held chord posts for `key` under `layout`.
///
/// The release looks the scan code up again instead of reusing the press's.
/// The two differ only if the layout changes during the hold, and the
/// virtual key, which the system's key state is indexed by, stays the same.
fn held_key(key: HeldKey, layout: ForegroundLayout) -> Option<Key> {
    match key {
        HeldKey::Control => Some(Key::for_vk(VK_CONTROL, layout)),
        HeldKey::Shift => Some(Key::for_vk(VK_SHIFT, layout)),
        HeldKey::Alt => Some(Key::for_vk(VK_MENU, layout)),
        HeldKey::Super => Some(Key::for_vk(VK_LWIN, layout)),
        HeldKey::Key(usage) => {
            let key = Key::for_usage(usage, layout);
            if key.is_none() {
                tracing::warn!(
                    usage = usage.code(),
                    "held shortcut usage has no Windows mapping — edge ignored"
                );
            }
            key
        }
    }
}

fn send_inputs(inputs: &[INPUT]) {
    let Ok(input_count) = u32::try_from(inputs.len()) else {
        tracing::warn!(
            requested = inputs.len(),
            "too many SendInput events requested"
        );
        return;
    };
    let Ok(input_size) = i32::try_from(size_of::<INPUT>()) else {
        tracing::warn!("INPUT size does not fit the Win32 SendInput contract");
        return;
    };
    // SAFETY: inputs.as_ptr()/input_count describe a valid initialized INPUT slice; SendInput copies it and returns the count injected.
    let sent = unsafe { SendInput(input_count, inputs.as_ptr(), input_size) };
    if sent != input_count {
        tracing::warn!(
            requested = inputs.len(),
            sent,
            "SendInput accepted fewer events than requested"
        );
    }
}

fn key_input(key: Key, key_up: bool) -> INPUT {
    let mut flags = 0;
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }
    if is_extended_key(key.vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    // Without `KEYEVENTF_SCANCODE`, Windows posts `wVk` and hands `wScan` to
    // the receiving window in the message's `lParam`, so the scan code
    // changes only what an app that reads it sees.
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key.vk,
                wScan: key.scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_input(flags: u32, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: u32::from_ne_bytes(data.to_ne_bytes()),
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use openlogi_core::binding::{KeyCombo, KeyboardUsage, Shortcut};
    use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_EXTENDED,
        LLKHF_INJECTED, MSG, PM_REMOVE, PeekMessageW, SetWindowsHookExW, TranslateMessage,
        UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
    };

    use super::{
        ForegroundLayout, HeldKey, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
        KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, Key, VK_BROWSER_BACK, VK_BROWSER_FORWARD,
        VK_DELETE, VK_DOWN, VK_END, VK_HOME, VK_INSERT, VK_LEFT, VK_LWIN, VK_NEXT, VK_PRIOR,
        VK_RIGHT, VK_SHIFT, VK_UP, combo, held_key, is_extended_key, key_input, send_inputs,
    };

    fn usage(label: &str) -> KeyboardUsage {
        label
            .parse::<KeyCombo>()
            .expect("test key must be valid")
            .key()
    }

    /// A key a layout can move must carry the scan code of the key that
    /// produces its virtual key under the active layout. Without it, an app
    /// that matches shortcuts by scan code under a non-Latin layout ignores
    /// the key, although a physical press of it works (#2026).
    ///
    /// The check is the inverse lookup, so it holds on any host layout.
    /// Letters and digits have a virtual key on every layout Windows ships;
    /// some punctuation keys do not, so they are left out.
    #[test]
    fn letter_and_digit_keys_carry_the_layouts_scan_code() {
        let layout = ForegroundLayout::current();
        for label in ('A'..='Z').chain('0'..='9') {
            let key = Key::for_usage(usage(&label.to_string()), layout)
                .expect("letters and digits have a Windows virtual key");
            assert_ne!(key.scan, 0, "{label} must carry a scan code");
            assert_eq!(
                layout.virtual_key(key.scan),
                key.vk,
                "{label} must carry the scan code of the key that produces it"
            );
        }
    }

    /// Keys a layout never moves are posted as their virtual key alone, as
    /// they were before #2026.
    #[test]
    fn fixed_keys_post_the_virtual_key_alone() {
        let layout = ForegroundLayout::current();
        for label in ["Enter", "Escape", "Tab", "Space", "F5"] {
            let key = Key::for_usage(usage(label), layout)
                .expect("fixed keys have a Windows virtual key");
            assert_eq!(key.scan, 0, "{label} must not carry a scan code");
        }
    }

    /// `key_input` posts the scan code beside the virtual key on both edges.
    /// Dropping it is the bug in #2026.
    #[test]
    fn key_input_posts_the_scan_code_beside_the_virtual_key() {
        let key = Key {
            vk: u16::from(b'V'),
            scan: 0x2F,
        };
        // SAFETY: `key_input` initialises the `ki` arm of the union it
        // returns, so reading that arm is well-defined.
        let (down, up) = unsafe {
            (
                key_input(key, false).Anonymous.ki,
                key_input(key, true).Anonymous.ki,
            )
        };
        assert_eq!((down.wVk, down.wScan, down.dwFlags), (key.vk, key.scan, 0));
        assert_eq!(
            (up.wVk, up.wScan, up.dwFlags),
            (key.vk, key.scan, KEYEVENTF_KEYUP)
        );
    }

    /// Pin a handful of representative `Shortcut -> KeyCombo` rows so an
    /// edit to the table can't silently change what Ctrl+C sends.
    /// `Redo` differs from macOS/Linux by design (see the module doc on
    /// `combo`), and `BrowserBack`/`BrowserForward` have no `KeyCombo`
    /// representation on Windows at all — see `combo`'s doc.
    #[test]
    fn combo_table_pins_representative_shortcuts() {
        assert_eq!(
            combo(Shortcut::Copy)
                .unwrap_or_else(|vk| panic!("Copy should be a chord, not raw vk {vk:#x}"))
                .rendered_label(),
            "Ctrl+C"
        );
        assert_eq!(
            combo(Shortcut::Redo)
                .unwrap_or_else(|vk| panic!("Redo should be a chord, not raw vk {vk:#x}"))
                .rendered_label(),
            "Ctrl+Y"
        );
        assert_eq!(
            combo(Shortcut::NextTab)
                .unwrap_or_else(|vk| panic!("NextTab should be a chord, not raw vk {vk:#x}"))
                .rendered_label(),
            "Ctrl+Tab"
        );
        assert_eq!(combo(Shortcut::BrowserBack), Err(VK_BROWSER_BACK));
        assert_eq!(combo(Shortcut::BrowserForward), Err(VK_BROWSER_FORWARD));
        // Every chord-shaped row must actually resolve through
        // hid_usage_to_windows, or a `Shortcut` silently no-ops instead of
        // pressing anything (see `press_combo`'s warn-and-drop
        // path). Iterates `Shortcut::ALL` rather than a hand-copied list,
        // so a newly added `Shortcut` variant is checked here
        // automatically instead of depending on someone remembering to
        // extend a second, independent list. The two raw-vk exceptions
        // (pinned individually above) are skipped here, not silently
        // missed: `combo`'s own match already forces every `Shortcut`
        // variant to be classified as one or the other.
        for &shortcut in Shortcut::ALL {
            let Ok(chord) = combo(shortcut) else {
                continue;
            };
            let key = chord.key().code();
            assert!(
                super::super::hid_usage_to_windows(key).is_some(),
                "{shortcut:?} table entry has no Windows virtual-key mapping"
            );
        }
    }

    /// The extended keys this backend injects, with the scan code each one
    /// has to carry.
    ///
    /// The flag only means something next to a scan code, and the scan code is
    /// what tells the dedicated cluster apart from the keypad keys that share
    /// its codes. The expected codes are the hardware ones (scancode set 1),
    /// and they are what `MapVirtualKeyW` answers on every layout this backend
    /// can run on: measured identical on `00000804` (zh-CN) and `00000409`
    /// (en-US).
    const EXTENDED_KEYS_AND_THEIR_SCAN_CODES: &[(u16, u16)] = &[
        (VK_LEFT, 0x4B),
        (VK_UP, 0x48),
        (VK_RIGHT, 0x4D),
        (VK_DOWN, 0x50),
        (VK_HOME, 0x47),
        (VK_END, 0x4F),
        (VK_PRIOR, 0x49),
        (VK_NEXT, 0x51),
        (VK_INSERT, 0x52),
        (VK_DELETE, 0x53),
        (VK_LWIN, 0x5B),
    ];

    /// Every extended key has to be flagged and to carry the scan code of the
    /// hardware key it stands for. A key missing from [`is_extended_key`] loses
    /// its Shift whenever Num Lock is on (#1895); a key carrying the wrong scan
    /// code is read as a different key entirely.
    #[test]
    fn extended_keys_carry_their_hardware_scan_code() {
        let layout = ForegroundLayout::current();
        for &(vk, expected_scan) in EXTENDED_KEYS_AND_THEIR_SCAN_CODES {
            assert!(
                is_extended_key(vk),
                "virtual key {vk:#x} has to be treated as an extended key"
            );
            assert_eq!(
                layout.scan_code(vk),
                expected_scan,
                "virtual key {vk:#x} has to resolve to its hardware scan code"
            );
            // SAFETY: `key_input` fully initialises the `INPUT` it returns, so
            // reading the `ki` arm of the union is well-defined.
            let ki = unsafe { key_input(Key::for_vk(vk, layout), false).Anonymous.ki };
            assert_ne!(
                ki.dwFlags & KEYEVENTF_EXTENDEDKEY,
                0,
                "virtual key {vk:#x} must set KEYEVENTF_EXTENDEDKEY"
            );
            assert_eq!(
                ki.wScan, expected_scan,
                "virtual key {vk:#x} must carry its scan code"
            );
        }
    }

    /// The extended flag has to be set on both edges of the key press, and the
    /// key-up edge has to carry the key-up flag as well.
    #[test]
    fn key_input_sets_extended_flag_and_scan_code() {
        let key = Key::for_vk(VK_RIGHT, ForegroundLayout::current());
        // SAFETY: `key_input` fully initialises the `INPUT` it returns, so
        // reading the `ki` arm of the union is well-defined.
        let (down_ki, up_ki) = unsafe {
            let down = key_input(key, false);
            let up = key_input(key, true);
            (down.Anonymous.ki, up.Anonymous.ki)
        };
        assert_eq!(down_ki.wVk, VK_RIGHT);
        assert_ne!(
            down_ki.dwFlags & KEYEVENTF_EXTENDEDKEY,
            0,
            "extended key press must set KEYEVENTF_EXTENDEDKEY"
        );
        assert_eq!(down_ki.dwFlags & KEYEVENTF_KEYUP, 0);
        assert_eq!(down_ki.wScan, 0x4D);

        assert_ne!(
            up_ki.dwFlags & KEYEVENTF_EXTENDEDKEY,
            0,
            "extended key release must set KEYEVENTF_EXTENDEDKEY"
        );
        assert_ne!(up_ki.dwFlags & KEYEVENTF_KEYUP, 0);
        assert_eq!(up_ki.wScan, down_ki.wScan);
    }

    /// The navigation keys a shortcut can name, with the virtual key each one
    /// resolves to.
    const SHORTCUT_NAVIGATION_KEYS: &[(&str, u16)] = &[
        ("PageUp", VK_PRIOR),
        ("PageDown", VK_NEXT),
        ("Home", VK_HOME),
        ("End", VK_END),
        ("Insert", VK_INSERT),
        ("Delete", VK_DELETE),
        ("Left", VK_LEFT),
        ("Up", VK_UP),
        ("Right", VK_RIGHT),
        ("Down", VK_DOWN),
    ];

    /// A `CustomShortcut` key resolves through [`Key::for_usage`] and a
    /// `HoldShortcut` key through [`held_key`], not through [`Key::for_vk`],
    /// so the navigation keys need their flag and scan code on those paths
    /// too. A game that reads keys by scan code ignored a PageUp bound to a
    /// mouse button while it went out with scan code 0, although letter keys
    /// worked (#2170).
    #[test]
    fn shortcut_navigation_keys_carry_their_hardware_scan_code() {
        let layout = ForegroundLayout::current();
        for &(label, vk) in SHORTCUT_NAVIGATION_KEYS {
            let (_, scan) = EXTENDED_KEYS_AND_THEIR_SCAN_CODES
                .iter()
                .copied()
                .find(|&(extended, _)| extended == vk)
                .expect("every navigation key is an extended key");
            let expected = Key { vk, scan };
            let key = Key::for_usage(usage(label), layout)
                .expect("navigation keys have a Windows virtual key");
            assert_eq!(
                key, expected,
                "{label} in a custom shortcut must carry its hardware scan code"
            );
            assert_eq!(
                held_key(HeldKey::Key(usage(label)), layout),
                Some(expected),
                "{label} in a held shortcut must carry its hardware scan code"
            );
            // SAFETY: `key_input` fully initialises the `INPUT` it returns, so
            // reading the `ki` arm of the union is well-defined.
            let ki = unsafe { key_input(key, false).Anonymous.ki };
            assert_ne!(
                ki.dwFlags & KEYEVENTF_EXTENDEDKEY,
                0,
                "{label} must set KEYEVENTF_EXTENDEDKEY"
            );
        }
    }

    /// A plain key must not be flagged and must not carry a scan code: marking
    /// `B` extended would make Windows read it as a keypad key.
    #[test]
    fn key_input_leaves_plain_keys_alone() {
        // SAFETY: `key_input` fully initialises the `INPUT` it returns.
        let ki = unsafe { key_input(Key::plain(u16::from(b'B')), false).Anonymous.ki };
        assert_eq!(ki.wVk, u16::from(b'B'));
        assert_eq!(ki.wScan, 0);
        assert_eq!(ki.dwFlags & KEYEVENTF_EXTENDEDKEY, 0);
    }

    /// Every production path that posts an extended key has to build it with
    /// [`Key::for_vk`] or [`Key::for_usage`], because [`key_input`] takes the
    /// flag from the virtual key but the scan code from the `Key`. Otherwise
    /// the key goes out flagged with no scan code, a form no keyboard sends.
    ///
    /// `Key::plain` refuses that shape in a debug build, so a call site that
    /// skips `for_vk` trips this assertion before it can ship. It is a
    /// `debug_assert!`, so the test is gated on `debug_assertions` too: under
    /// `cargo test --release` the assertion is compiled out and there is
    /// nothing to panic.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "needs its scan code")]
    fn plain_key_refuses_an_extended_virtual_key() {
        let _ = Key::plain(VK_RIGHT);
    }

    /// The control of the #1895 regression test has to be the pre-fix form: the
    /// virtual key with no scan code and no extended flag. `key_input` cannot
    /// produce it, so the control builds its own `INPUT`; if that ever starts
    /// carrying the flag, the control stops proving anything and the guard
    /// below goes quiet.
    #[test]
    fn pre_fix_control_carries_no_flag_and_no_scan_code() {
        // SAFETY: `pre_fix_input` fully initialises the `INPUT` it returns.
        let down = unsafe { pre_fix_input(VK_RIGHT, false).Anonymous.ki };
        assert_eq!(down.wVk, VK_RIGHT);
        assert_eq!(down.wScan, 0);
        assert_eq!(down.dwFlags, 0, "the control must not be flagged");
        // SAFETY: as above.
        let up = unsafe { pre_fix_input(VK_RIGHT, true).Anonymous.ki };
        assert_eq!(up.dwFlags, KEYEVENTF_KEYUP);
    }

    /// One key event as the system-wide keyboard hook saw it.
    #[derive(Clone, Copy, Debug)]
    struct Observed {
        vk: u32,
        scan: u32,
        flags: u32,
        up: bool,
    }

    /// The key events the hook saw, in arrival order.
    static OBSERVED: Mutex<Vec<Observed>> = Mutex::new(Vec::new());

    /// The Shift keys as the keyboard hook reports them: Windows splits the
    /// generic `VK_SHIFT` into the left and the right one, and an injected
    /// generic Shift arrives as the left one.
    const VK_LSHIFT: u16 = 0xA0;
    const VK_RSHIFT: u16 = 0xA1;

    /// Num Lock, from WinUser.h. The backend never injects it in production;
    /// the regression test needs it to put the machine in the state the bug
    /// needs.
    const VK_NUMLOCK: u16 = 0x90;

    /// Windows' low-level keyboard hook: the only way to watch an injected key
    /// without an application window in the way.
    unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION.cast_signed()
            && (wparam == WM_KEYDOWN as WPARAM || wparam == WM_KEYUP as WPARAM)
        {
            // SAFETY: for a keyboard hook with `HC_ACTION`, `lparam` points at a
            // `KBDLLHOOKSTRUCT` that stays valid for the duration of the call.
            let data = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            let observed = Observed {
                vk: data.vkCode,
                // The hook encodes the `0xe0` prefix in bit `0x200` of the scan
                // code; only the scan code itself is compared here.
                scan: data.scanCode & 0xFF,
                flags: data.flags,
                up: wparam == WM_KEYUP as WPARAM,
            };
            if let Ok(mut events) = OBSERVED.lock() {
                events.push(observed);
            }
        }
        // SAFETY: a null hook handle is what the API expects for a low-level
        // hook.
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }

    /// Drain the message queue so the hook callback runs.
    fn pump_messages() {
        let mut msg = MSG::default();
        // SAFETY: `PeekMessageW` fills `msg` when it returns true.
        while unsafe { PeekMessageW(&raw mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
            // SAFETY: both calls take the message `PeekMessageW` just filled in.
            unsafe {
                TranslateMessage(&raw const msg);
                DispatchMessageW(&raw const msg);
            }
        }
    }

    /// Whether the event is one of the Shift keys.
    ///
    /// Windows splits the generic `VK_SHIFT` into the left and the right one,
    /// and an injected generic Shift arrives as the left one, so the check has
    /// to cover all three. The hook reports `vkCode` as a `u32`, which is why
    /// the codes are spelled here and not taken from the `u16` constants.
    fn is_shift(vk: u32) -> bool {
        vk == u32::from(VK_SHIFT) || vk == u32::from(VK_LSHIFT) || vk == u32::from(VK_RSHIFT)
    }

    /// Whether the Shift modifier was down when `events[index]` arrived.
    ///
    /// The state comes from the observed key events, not from `GetKeyState`:
    /// the high-order bit of that call reports the calling thread's input
    /// queue, and the test thread owns no focused window, so it does not follow
    /// an injected Shift.
    fn shift_down_at(events: &[Observed], index: usize) -> bool {
        let mut downs = 0usize;
        let mut ups = 0usize;
        for event in &events[..index] {
            if is_shift(event.vk) {
                if event.up {
                    ups += 1;
                } else {
                    downs += 1;
                }
            }
        }
        downs > ups
    }

    /// Whether Num Lock is on — the state the "Shift cancels Num Lock"
    /// behaviour depends on.
    ///
    /// The low-order bit of `GetKeyState` is the toggle state, which the system
    /// keeps globally; the high-order bit is the calling thread's input queue,
    /// which is why it cannot be used to read the Shift modifier.
    fn num_lock_on() -> bool {
        // SAFETY: `GetKeyState` takes a virtual-key code and has no other
        // precondition.
        (unsafe { GetKeyState(i32::from(VK_NUMLOCK)) }.cast_unsigned() & 1) != 0
    }

    /// Put Num Lock where the test needs it and hand the original state back,
    /// so the test leaves the machine the way it found it.
    fn set_num_lock(on: bool) -> bool {
        let original = num_lock_on();
        if original == on {
            return original;
        }
        send_inputs(&[
            key_input(Key::plain(VK_NUMLOCK), false),
            key_input(Key::plain(VK_NUMLOCK), true),
        ]);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(num_lock_on(), on, "could not switch Num Lock");
        original
    }

    /// The pre-fix form: the keyboard `INPUT` this backend built before #1895 —
    /// the virtual key, scan code 0, and no extended flag.
    ///
    /// `key_input` cannot produce it, because it derives the extended flag from
    /// the virtual key alone. The regression test injects this form as the
    /// control, so the assertion below is not vacuous.
    fn pre_fix_input(vk: u16, key_up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if key_up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    /// Inject `Shift+Right` with the given input builder and return the key
    /// events the hook saw.
    fn inject_shift_right(build: impl Fn(u16, bool) -> INPUT) -> Vec<Observed> {
        OBSERVED.lock().expect("event log poisoned").clear();
        send_inputs(&[build(VK_SHIFT, false), build(VK_RIGHT, false)]);
        for _ in 0..200 {
            pump_messages();
            let seen = OBSERVED.lock().expect("event log poisoned");
            if seen
                .iter()
                .any(|event| event.vk == u32::from(VK_RIGHT) && !event.up)
            {
                break;
            }
            drop(seen);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Let the modifiers go again.
        send_inputs(&[build(VK_RIGHT, true), build(VK_SHIFT, true)]);
        OBSERVED.lock().expect("event log poisoned").clone()
    }

    /// The index of the injected arrow's key-down in `events`.
    fn arrow_index(events: &[Observed]) -> usize {
        events
            .iter()
            .position(|event| event.vk == u32::from(VK_RIGHT) && !event.up)
            .expect("the injected arrow is not in the key event log")
    }

    /// The end-to-end guard for #1895: an injected `Shift+Right` has to reach
    /// the application with the Shift modifier still down.
    ///
    /// Windows releases the Shift around a key it cannot identify by scan code —
    /// the "Shift temporarily cancels Num Lock" behaviour — so the arrow has to
    /// carry the extended prefix the dedicated arrow cluster sends. The test
    /// drives the crate's own injection path and watches the system-wide
    /// keyboard hook, which sees injected keys whether or not an application
    /// window has focus.
    ///
    /// The pre-fix injection runs first, as the control: that form has to lose
    /// the Shift, which is what gives the assertion below its meaning. If
    /// Windows ever stops cancelling the Shift around an unidentified key, the
    /// control fails and the guard needs a rethink.
    ///
    /// The test switches Num Lock on (the behaviour only shows with Num Lock on)
    /// and puts it back afterwards; the injected keys reach whichever window has
    /// focus.
    #[test]
    #[ignore = "sends real keystrokes to the focused window and toggles Num Lock; requires explicit interactive execution"]
    fn extended_key_keeps_the_shift_modifier() {
        let original_num_lock = set_num_lock(true);

        // SAFETY: `SetWindowsHookExW` with a null module handle installs a
        // low-level hook whose callback lives in this process.
        let hook = unsafe {
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), std::ptr::null_mut(), 0)
        };
        assert!(!hook.is_null(), "could not install the keyboard hook");

        let layout = ForegroundLayout::current();
        let control = inject_shift_right(pre_fix_input);
        let fixed = inject_shift_right(|vk, up| key_input(Key::for_vk(vk, layout), up));

        // Drop the hook.
        // SAFETY: `hook` came from `SetWindowsHookExW` and is removed once.
        unsafe { UnhookWindowsHookEx(hook) };
        set_num_lock(original_num_lock);

        // The control: the pre-fix injection loses the Shift around the arrow.
        let control_arrow = arrow_index(&control);
        assert!(
            !shift_down_at(&control, control_arrow),
            "the pre-fix injection is expected to lose the shift modifier; if \
             Windows no longer cancels the shift around a key it cannot \
             identify, this control has to go: {control:?}"
        );

        // The guard: the shipped injection keeps it. This is the assertion that
        // fails when the extended flag goes away.
        let fixed_arrow = arrow_index(&fixed);
        assert!(
            shift_down_at(&fixed, fixed_arrow),
            "the shift modifier has to still be down when the arrow arrives: {fixed:?}"
        );
        let arrow = fixed[fixed_arrow];
        assert_ne!(
            arrow.flags & LLKHF_INJECTED,
            0,
            "the arrow has to be the injected one"
        );
        assert_ne!(
            arrow.flags & LLKHF_EXTENDED,
            0,
            "the arrow has to carry the extended bit, or Windows reads it as a keypad key"
        );
        assert_eq!(
            arrow.scan,
            u32::from(layout.scan_code(VK_RIGHT)),
            "the arrow has to carry its scan code"
        );
    }
}
