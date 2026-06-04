//! Logical mouse button identifiers and the action vocabulary each one can
//! bind to. Lives in `openlogi-core` because the [`config`](crate::config)
//! schema serializes these directly — the GUI re-exports them.
//!
//! When [`Action`] gains new variants, keep the existing variant names stable:
//! the TOML config keys/values use the enum variant identifiers verbatim, so
//! renames are migration events.

use std::fmt;

use serde::{Deserialize, Serialize};

/// One of the user-rebindable hotspots on a Logi mouse. The order matches the
/// physical layout from front to side; [`ButtonId::ALL`] is consumed by the
/// default-binding generator and the popover trigger list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ButtonId {
    LeftClick,
    RightClick,
    MiddleClick,
    Back,
    Forward,
    /// The "ModeShift" button under the wheel — typically used for SmartShift /
    /// DPI cycle. Named `DpiToggle` for historical reasons.
    DpiToggle,
    /// The horizontal thumb wheel's click. Kept in [`ButtonId::ALL`] so its
    /// default still seeds and dispatches when the wheel is diverted, even
    /// though the mouse model surfaces the two rotation directions instead of
    /// the click (see `mouse_model::geometry`).
    Thumbwheel,
    /// Rotating the thumb wheel "up" (positive rotation). Bound, by default, to
    /// continuous horizontal scroll; see [`crate::watchers`]-side dispatch.
    ThumbwheelScrollUp,
    /// Rotating the thumb wheel "down" (negative rotation).
    ThumbwheelScrollDown,
    /// The thumb-pad gesture button on MX-line devices. The press itself
    /// fires the bound action; swipe directions are P1.5 territory.
    GestureButton,
}

impl ButtonId {
    pub const ALL: [ButtonId; 10] = [
        ButtonId::LeftClick,
        ButtonId::RightClick,
        ButtonId::MiddleClick,
        ButtonId::Back,
        ButtonId::Forward,
        ButtonId::DpiToggle,
        ButtonId::Thumbwheel,
        ButtonId::ThumbwheelScrollUp,
        ButtonId::ThumbwheelScrollDown,
        ButtonId::GestureButton,
    ];

    /// Human-readable label for popovers and tooltips.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ButtonId::LeftClick => "Left Click",
            ButtonId::RightClick => "Right Click",
            ButtonId::MiddleClick => "Middle Click",
            ButtonId::Back => "Back",
            ButtonId::Forward => "Forward",
            ButtonId::DpiToggle => "DPI Toggle",
            ButtonId::Thumbwheel => "Thumb Wheel",
            ButtonId::ThumbwheelScrollUp => "Thumb Wheel Up",
            ButtonId::ThumbwheelScrollDown => "Thumb Wheel Down",
            ButtonId::GestureButton => "Gesture Button",
        }
    }
}

impl fmt::Display for ButtonId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One of the five sub-bindings on the gesture button: hold + swipe up/down/
/// left/right or a plain click without movement. Logi ships these as
/// independent assignments (`SLOT_NAME_GESTURE_*_BUTTON` in the
/// `device_gesture_buttons_image` metadata block) — OpenLogi mirrors the
/// same shape.
///
/// Variant identifiers are TOML-stable: renames are migration events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum GestureDirection {
    Up,
    Down,
    Left,
    Right,
    Click,
}

impl GestureDirection {
    pub const ALL: [GestureDirection; 5] = [
        GestureDirection::Up,
        GestureDirection::Down,
        GestureDirection::Left,
        GestureDirection::Right,
        GestureDirection::Click,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            GestureDirection::Up => "Up",
            GestureDirection::Down => "Down",
            GestureDirection::Left => "Left",
            GestureDirection::Right => "Right",
            GestureDirection::Click => "Click",
        }
    }

    /// Arrow glyph for compact list rendering.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            GestureDirection::Up => "↑",
            GestureDirection::Down => "↓",
            GestureDirection::Left => "←",
            GestureDirection::Right => "→",
            GestureDirection::Click => "·",
        }
    }
}

impl fmt::Display for GestureDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Minimum dominant-axis travel (raw-XY units) before a held gesture commits to
/// a direction. Tuned to match Logitech Options+'s responsiveness.
pub const GESTURE_SWIPE_THRESHOLD: i32 = 50;
/// Maximum cross-axis travel allowed at the threshold, so only a reasonably
/// straight swipe commits. Grows with the dominant axis (`max(deadzone, 35%)`).
pub const GESTURE_SWIPE_DEADZONE: i32 = 40;

/// Classify the *running* raw-XY travel of a held gesture button into a
/// directional swipe, the instant it commits — or `None` while it's still too
/// short or too diagonal.
///
/// The dominant axis must pass [`GESTURE_SWIPE_THRESHOLD`] while the cross axis
/// stays within `max(`[`GESTURE_SWIPE_DEADZONE`]`, 35% of dominant)`. Callers
/// fire the bound action the moment this returns `Some` — mid-swipe, like
/// Options+ — rather than waiting for the button release; a press that never
/// commits a direction is treated as [`GestureDirection::Click`] on release.
///
/// Coordinates follow the device's raw-XY convention (`+x` = right, `+y` =
/// down), so an upward swipe (negative `dy`) maps to [`GestureDirection::Up`].
#[must_use]
pub fn detect_swipe(dx: i32, dy: i32) -> Option<GestureDirection> {
    let (abs_x, abs_y) = (dx.abs(), dy.abs());
    let dominant = abs_x.max(abs_y);
    if dominant < GESTURE_SWIPE_THRESHOLD {
        return None;
    }
    let cross_limit = GESTURE_SWIPE_DEADZONE.max(dominant * 35 / 100);
    if abs_x > abs_y {
        if abs_y > cross_limit {
            return None;
        }
        Some(if dx > 0 {
            GestureDirection::Right
        } else {
            GestureDirection::Left
        })
    } else {
        if abs_x > cross_limit {
            return None;
        }
        Some(if dy > 0 {
            GestureDirection::Down
        } else {
            GestureDirection::Up
        })
    }
}

/// Grouping for popover section headers.
///
/// Used by [`Action::category`] and rendered as a small muted label above
/// each group in the action picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    /// Cut, copy, paste, undo, redo, select-all, find, save.
    Editing,
    /// Browser navigation: tabs, page reload, back/forward.
    Browser,
    /// Playback and volume controls.
    Media,
    /// Physical mouse clicks.
    Mouse,
    /// DPI cycle and SmartShift.
    Dpi,
    /// Scroll direction shortcuts.
    Scroll,
    /// Window/app navigation: Mission Control, Launchpad, etc.
    Navigation,
    /// Lock screen, show desktop, system-level actions.
    System,
}

impl Category {
    /// Short label for popover section headers (already uppercase so callers
    /// don't have to transform it).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Category::Editing => "EDITING",
            Category::Browser => "BROWSER",
            Category::Media => "MEDIA",
            Category::Mouse => "MOUSE",
            Category::Dpi => "DPI",
            Category::Scroll => "SCROLL",
            Category::Navigation => "NAVIGATION",
            Category::System => "SYSTEM",
        }
    }
}

/// What pressing a [`ButtonId`] should do.
///
/// Serialization uses serde's default external tagging: unit variants
/// serialize as a bare string (`"BrowserBack"`) and the tuple variant
/// serializes as a single-key table (`{ CustomShortcut = "my chord" }`).
///
/// **Stability contract:** existing variant *names* are frozen — they form the
/// on-disk `config.toml` schema. New variants may be appended freely; removing
/// or renaming a variant requires a `schema_version` bump and a migration.
///
/// `Action::execute` synthesizes the OS-level event for each variant.
/// On macOS it posts the event via `CGEventPost(kCGHIDEventTap, …)`.
/// On other platforms it logs a warning and returns immediately — the binary
/// compiles on all targets.
///
/// # Manual verification
///
/// `execute` is intentionally excluded from the automated test suite because
/// it would need to intercept the OS event queue. Smoke-test it manually:
/// bind a button to any action in the GUI and confirm the expected system event
/// fires when the button is pressed.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Action {
    // ── System ───────────────────────────────────────────────────────────────
    /// Suppress the input entirely — the button or wheel direction is captured
    /// but no OS event is synthesised, so the physical input does nothing.
    None,

    // ── Mouse ────────────────────────────────────────────────────────────────
    /// Primary mouse button.
    LeftClick,
    /// Secondary mouse button.
    RightClick,
    /// Middle mouse button (wheel click).
    MiddleClick,

    // ── Editing ──────────────────────────────────────────────────────────────
    /// Copy the current selection (⌘C / Ctrl+C).
    Copy,
    /// Paste from the clipboard (⌘V / Ctrl+V).
    Paste,
    /// Cut the current selection (⌘X / Ctrl+X).
    Cut,
    /// Undo the last action (⌘Z / Ctrl+Z).
    Undo,
    /// Redo the last undone action (⌘⇧Z / Ctrl+Y).
    Redo,
    /// Select all content (⌘A / Ctrl+A).
    SelectAll,
    /// Open the find / search bar (⌘F / Ctrl+F).
    Find,
    /// Save the current document (⌘S / Ctrl+S).
    Save,

    // ── Browser / Navigation ──────────────────────────────────────────────────
    /// Navigate backward in browser history.
    BrowserBack,
    /// Navigate forward in browser history.
    BrowserForward,
    /// Open a new tab (⌘T / Ctrl+T).
    NewTab,
    /// Close the current tab (⌘W / Ctrl+W).
    CloseTab,
    /// Reopen the last closed tab (⌘⇧T / Ctrl+Shift+T).
    ReopenTab,
    /// Switch to the next tab (⌃⇥ / Ctrl+Tab).
    NextTab,
    /// Switch to the previous tab (⌃⇧⇥ / Ctrl+Shift+Tab).
    PrevTab,
    /// Reload the current page (⌘R / Ctrl+R).
    ReloadPage,

    // ── Navigation / Window ───────────────────────────────────────────────────
    /// macOS Mission Control (⌃↑).
    MissionControl,
    /// macOS App Exposé — all windows for the current app (⌃↓).
    AppExpose,
    /// Switch to the previous desktop / Space.
    PreviousDesktop,
    /// Switch to the next desktop / Space.
    NextDesktop,
    /// Show the desktop (hide all windows).
    ShowDesktop,
    /// Open Launchpad.
    LaunchpadShow,

    // ── System ────────────────────────────────────────────────────────────────
    /// Lock the screen.
    LockScreen,
    /// Capture a screenshot.
    Screenshot,

    // ── Media ────────────────────────────────────────────────────────────────
    /// Toggle media play/pause.
    PlayPause,
    /// Skip to the next track.
    NextTrack,
    /// Go back to the previous track.
    PrevTrack,
    /// Increase system volume.
    VolumeUp,
    /// Decrease system volume.
    VolumeDown,
    /// Toggle system mute.
    MuteVolume,

    // ── DPI ──────────────────────────────────────────────────────────────────
    /// Step through the configured DPI preset list (P1.7).
    CycleDpiPresets,
    /// Jump to a specific zero-based preset in the device's DPI preset list.
    /// Out-of-range indices clamp to the list length at fire time (P1.7).
    SetDpiPreset(u8),
    /// Toggle the HID++ SmartShift ratchet/free-spin wheel mode (P1.1).
    ToggleSmartShift,

    // ── Scroll ───────────────────────────────────────────────────────────────
    /// Synthesise a vertical scroll-up tick.
    ScrollUp,
    /// Synthesise a vertical scroll-down tick.
    ScrollDown,
    /// Synthesise a horizontal scroll-left tick.
    HorizontalScrollLeft,
    /// Synthesise a horizontal scroll-right tick.
    HorizontalScrollRight,

    // ── Custom ───────────────────────────────────────────────────────────────
    /// Replay an arbitrary recorded key chord (P1.3).
    ///
    /// Holds the structured chord data so `execute` can post the real
    /// keystroke (macOS: CGEventPost with the encoded modifier flags).
    /// The `display` field is used by [`Action::label`] so the popover
    /// shows the user-friendly chord name.
    CustomShortcut(KeyCombo),
}

/// A modifier + virtual-key keystroke captured by the P1.3 recorder UI or
/// hand-authored in `config.toml`.
///
/// `modifiers` is a bitmask of [`KeyCombo::MOD_CMD`] etc. so the wire format
/// is a compact integer, not a string. `key_code` is the macOS virtual key
/// (`kVK_*`); on Linux, `Action::execute` maps it to an evdev `KeyCode` via
/// `linux::macos_vk_to_linux`.
///
/// `display` is purely for rendering — e.g. `"⌘⇧P"`. Callers regenerate it
/// from the captured chord; we keep it in the struct so older configs
/// continue to render the same label without re-deriving on every load.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyCombo {
    /// Bitmask of [`Self::MOD_CMD`] etc.
    pub modifiers: u8,
    /// macOS virtual key code (`kVK_*`). 0 means "no key" — useful for
    /// modifier-only placeholders that the recorder UI rejects. On Linux,
    /// `Action::execute` translates this to an evdev `KeyCode`.
    pub key_code: u16,
    /// Pre-rendered chord label, e.g. `"⌘⇧P"`. Empty falls through to a
    /// generated label at runtime.
    #[serde(default)]
    pub display: String,
}

impl KeyCombo {
    pub const MOD_CMD: u8 = 1 << 0;
    pub const MOD_SHIFT: u8 = 1 << 1;
    pub const MOD_CTRL: u8 = 1 << 2;
    pub const MOD_OPTION: u8 = 1 << 3;

    /// Build the human-readable label from the modifier bitmask + key code.
    /// Falls back to `"⌘key 0xNN"` when the key code isn't one of the
    /// commonly-recognised letters; the recorder UI usually overrides this
    /// with its own derivation.
    #[must_use]
    pub fn rendered_label(&self) -> String {
        if !self.display.is_empty() {
            return self.display.clone();
        }
        let mut out = String::new();
        if self.modifiers & Self::MOD_CTRL != 0 {
            out.push('⌃');
        }
        if self.modifiers & Self::MOD_OPTION != 0 {
            out.push('⌥');
        }
        if self.modifiers & Self::MOD_SHIFT != 0 {
            out.push('⇧');
        }
        if self.modifiers & Self::MOD_CMD != 0 {
            out.push('⌘');
        }
        match self.key_code {
            0x00 => out.push('A'),
            0x01 => out.push('S'),
            0x02 => out.push('D'),
            0x03 => out.push('F'),
            0x06 => out.push('Z'),
            0x07 => out.push('X'),
            0x08 => out.push('C'),
            0x09 => out.push('V'),
            0x0B => out.push('B'),
            0x0C => out.push('Q'),
            0x0D => out.push('W'),
            0x0E => out.push('E'),
            0x0F => out.push('R'),
            0x10 => out.push('Y'),
            0x11 => out.push('T'),
            0x20 => out.push('U'),
            0x22 => out.push('I'),
            0x1F => out.push('O'),
            0x23 => out.push('P'),
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "key 0x{:02X}", self.key_code);
            }
        }
        out
    }
}

impl Action {
    /// Display label for the popover row.
    ///
    /// Returns `String` rather than `&str` so parameterized variants (e.g.
    /// `SetDpiPreset(i)`, `CustomShortcut(s)`) can build a label that
    /// includes their payload.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Action::None => "Do Nothing".into(),
            Action::LeftClick => "Left Click".into(),
            Action::RightClick => "Right Click".into(),
            Action::MiddleClick => "Middle Click".into(),
            Action::Copy => "Copy".into(),
            Action::Paste => "Paste".into(),
            Action::Cut => "Cut".into(),
            Action::Undo => "Undo".into(),
            Action::Redo => "Redo".into(),
            Action::SelectAll => "Select All".into(),
            Action::Find => "Find".into(),
            Action::Save => "Save".into(),
            Action::BrowserBack => "Browser Back".into(),
            Action::BrowserForward => "Browser Forward".into(),
            Action::NewTab => "New Tab".into(),
            Action::CloseTab => "Close Tab".into(),
            Action::ReopenTab => "Reopen Tab".into(),
            Action::NextTab => "Next Tab".into(),
            Action::PrevTab => "Previous Tab".into(),
            Action::ReloadPage => "Reload Page".into(),
            Action::MissionControl => "Mission Control".into(),
            Action::AppExpose => "App Exposé".into(),
            Action::PreviousDesktop => "Previous Desktop".into(),
            Action::NextDesktop => "Next Desktop".into(),
            Action::ShowDesktop => "Show Desktop".into(),
            Action::LaunchpadShow => "Launchpad".into(),
            Action::LockScreen => "Lock Screen".into(),
            Action::Screenshot => "Screenshot".into(),
            Action::PlayPause => "Play / Pause".into(),
            Action::NextTrack => "Next Track".into(),
            Action::PrevTrack => "Previous Track".into(),
            Action::VolumeUp => "Volume Up".into(),
            Action::VolumeDown => "Volume Down".into(),
            Action::MuteVolume => "Mute".into(),
            Action::CycleDpiPresets => "Cycle DPI Presets".into(),
            Action::SetDpiPreset(i) => format!("DPI Preset {}", i + 1),
            Action::ToggleSmartShift => "Toggle SmartShift".into(),
            Action::ScrollUp => "Scroll Up".into(),
            Action::ScrollDown => "Scroll Down".into(),
            Action::HorizontalScrollLeft => "Scroll Left".into(),
            Action::HorizontalScrollRight => "Scroll Right".into(),
            Action::CustomShortcut(combo) => combo.rendered_label(),
        }
    }

    /// Which [`Category`] this action belongs to, used for popover grouping.
    #[must_use]
    pub fn category(&self) -> Category {
        match self {
            Action::LeftClick | Action::RightClick | Action::MiddleClick => Category::Mouse,
            // CustomShortcut is assigned to Editing so it doesn't need a
            // separate arm (it's not in the picker catalog).
            Action::Copy
            | Action::Paste
            | Action::Cut
            | Action::Undo
            | Action::Redo
            | Action::SelectAll
            | Action::Find
            | Action::Save
            | Action::CustomShortcut(_) => Category::Editing,
            Action::BrowserBack
            | Action::BrowserForward
            | Action::NewTab
            | Action::CloseTab
            | Action::ReopenTab
            | Action::NextTab
            | Action::PrevTab
            | Action::ReloadPage => Category::Browser,
            Action::MissionControl
            | Action::AppExpose
            | Action::PreviousDesktop
            | Action::NextDesktop
            | Action::ShowDesktop
            | Action::LaunchpadShow => Category::Navigation,
            Action::None | Action::LockScreen | Action::Screenshot => Category::System,
            Action::PlayPause
            | Action::NextTrack
            | Action::PrevTrack
            | Action::VolumeUp
            | Action::VolumeDown
            | Action::MuteVolume => Category::Media,
            Action::CycleDpiPresets | Action::SetDpiPreset(_) | Action::ToggleSmartShift => {
                Category::Dpi
            }
            Action::ScrollUp
            | Action::ScrollDown
            | Action::HorizontalScrollLeft
            | Action::HorizontalScrollRight => Category::Scroll,
        }
    }

    /// All pickable actions in a deterministic order.
    ///
    /// [`Action::CustomShortcut`] is intentionally excluded — it is opened via
    /// "Record shortcut…" (P1.3), not selected from the catalog.
    #[must_use]
    pub fn catalog() -> Vec<Action> {
        vec![
            // Mouse
            Action::LeftClick,
            Action::RightClick,
            Action::MiddleClick,
            // Editing
            Action::Copy,
            Action::Paste,
            Action::Cut,
            Action::Undo,
            Action::Redo,
            Action::SelectAll,
            Action::Find,
            Action::Save,
            // Browser
            Action::BrowserBack,
            Action::BrowserForward,
            Action::NewTab,
            Action::CloseTab,
            Action::ReopenTab,
            Action::NextTab,
            Action::PrevTab,
            Action::ReloadPage,
            // Navigation
            Action::MissionControl,
            Action::AppExpose,
            Action::PreviousDesktop,
            Action::NextDesktop,
            Action::ShowDesktop,
            Action::LaunchpadShow,
            // System
            Action::None,
            Action::LockScreen,
            Action::Screenshot,
            // Media
            Action::PlayPause,
            Action::NextTrack,
            Action::PrevTrack,
            Action::VolumeUp,
            Action::VolumeDown,
            Action::MuteVolume,
            // DPI
            Action::CycleDpiPresets,
            Action::ToggleSmartShift,
            // Scroll
            Action::ScrollUp,
            Action::ScrollDown,
            Action::HorizontalScrollLeft,
            Action::HorizontalScrollRight,
        ]
    }

    /// Synthesise the OS-level event for this action.
    ///
    /// On macOS, key events are posted via `CGEventPost(kCGHIDEventTap, …)`
    /// using virtual key codes from the standard US keyboard layout, and the
    /// `LeftClick`/`RightClick`/`MiddleClick` variants synthesise a mouse click
    /// at the current cursor location. The WindowServer actions (`MissionControl`,
    /// `AppExpose`, `ShowDesktop`, `LaunchpadShow`) are posted straight to the
    /// Dock via `CoreDockSendNotification`. Device-side actions (`CycleDpiPresets`,
    /// `SetDpiPreset`, `ToggleSmartShift`) have no CGEvent equivalent and are
    /// handled at the hook/HID layer, logging a trace here.
    ///
    /// On Linux, key and scroll events are injected via a lazily-created `uinput`
    /// virtual device. Mouse clicks inject `BTN_*` events. macOS-only window
    /// manager actions (`MissionControl`, `AppExpose`, `ShowDesktop`,
    /// `LaunchpadShow`) have no universal Linux equivalent and are silently
    /// skipped (debug-logged). `CustomShortcut` maps macOS `kVK_*` codes to
    /// Linux key codes; macOS Cmd maps to Ctrl.
    ///
    /// On other platforms a warning is logged and the function returns
    /// immediately — the binary compiles clean on all targets.
    pub fn execute(&self) {
        #[cfg(target_os = "macos")]
        self.execute_macos();

        #[cfg(target_os = "linux")]
        self.execute_linux();

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            tracing::warn!(
                action = self.label(),
                "Action::execute unsupported on this platform"
            );
        }
    }

    /// Linux implementation: inject events via a shared `uinput` virtual device.
    #[cfg(target_os = "linux")]
    fn execute_linux(&self) {
        use evdev::{KeyCode, RelativeAxisCode};
        let ctrl = KeyCode::KEY_LEFTCTRL;
        let shift = KeyCode::KEY_LEFTSHIFT;
        let alt = KeyCode::KEY_LEFTALT;
        match self {
            // ── Mouse clicks ──────────────────────────────────────────────────
            Action::LeftClick => linux::click(KeyCode::BTN_LEFT),
            Action::RightClick => linux::click(KeyCode::BTN_RIGHT),
            Action::MiddleClick => linux::click(KeyCode::BTN_MIDDLE),
            // ── Editing ───────────────────────────────────────────────────────
            Action::Copy => linux::press_key(&[ctrl], KeyCode::KEY_C),
            Action::Paste => linux::press_key(&[ctrl], KeyCode::KEY_V),
            Action::Cut => linux::press_key(&[ctrl], KeyCode::KEY_X),
            Action::Undo => linux::press_key(&[ctrl], KeyCode::KEY_Z),
            // Redo is Ctrl+Shift+Z on Linux (matches macOS ⌘⇧Z convention).
            Action::Redo => linux::press_key(&[ctrl, shift], KeyCode::KEY_Z),
            Action::SelectAll => linux::press_key(&[ctrl], KeyCode::KEY_A),
            Action::Find => linux::press_key(&[ctrl], KeyCode::KEY_F),
            Action::Save => linux::press_key(&[ctrl], KeyCode::KEY_S),
            // ── Browser / Navigation ──────────────────────────────────────────
            Action::BrowserBack => linux::press_key(&[alt], KeyCode::KEY_LEFT),
            Action::BrowserForward => linux::press_key(&[alt], KeyCode::KEY_RIGHT),
            Action::NewTab => linux::press_key(&[ctrl], KeyCode::KEY_T),
            Action::CloseTab => linux::press_key(&[ctrl], KeyCode::KEY_W),
            Action::ReopenTab => linux::press_key(&[ctrl, shift], KeyCode::KEY_T),
            Action::NextTab => linux::press_key(&[ctrl], KeyCode::KEY_TAB),
            Action::PrevTab => linux::press_key(&[ctrl, shift], KeyCode::KEY_TAB),
            Action::ReloadPage => linux::press_key(&[ctrl], KeyCode::KEY_R),
            // ── Navigation — macOS-specific ───────────────────────────────────
            // No universal Linux equivalent; the compositor shortcut varies.
            Action::MissionControl
            | Action::AppExpose
            | Action::ShowDesktop
            | Action::LaunchpadShow => {
                tracing::debug!(
                    action = self.label(),
                    "no Linux equivalent — action skipped"
                );
            }
            // Ctrl+Alt+←/→ is the default in GNOME and KDE.
            Action::PreviousDesktop => linux::press_key(&[ctrl, alt], KeyCode::KEY_LEFT),
            Action::NextDesktop => linux::press_key(&[ctrl, alt], KeyCode::KEY_RIGHT),
            // ── System ────────────────────────────────────────────────────────
            // Ctrl+Alt+L is the standard lock shortcut in GNOME and KDE.
            Action::LockScreen => linux::press_key(&[ctrl, alt], KeyCode::KEY_L),
            Action::Screenshot => linux::press_key(&[], KeyCode::KEY_SYSRQ),
            // ── Media ─────────────────────────────────────────────────────────
            Action::PlayPause => linux::press_key(&[], KeyCode::KEY_PLAYPAUSE),
            Action::NextTrack => linux::press_key(&[], KeyCode::KEY_NEXTSONG),
            Action::PrevTrack => linux::press_key(&[], KeyCode::KEY_PREVIOUSSONG),
            Action::VolumeUp => linux::press_key(&[], KeyCode::KEY_VOLUMEUP),
            Action::VolumeDown => linux::press_key(&[], KeyCode::KEY_VOLUMEDOWN),
            Action::MuteVolume => linux::press_key(&[], KeyCode::KEY_MUTE),
            // ── DPI / SmartShift: handled at hook/HID layer ───────────────────
            Action::CycleDpiPresets | Action::SetDpiPreset(_) | Action::ToggleSmartShift => {
                tracing::debug!(
                    action = self.label(),
                    "device action handled by hook/HID layer"
                );
            }
            // ── Scroll ────────────────────────────────────────────────────────
            Action::ScrollUp => linux::scroll(RelativeAxisCode::REL_WHEEL, 3),
            Action::ScrollDown => linux::scroll(RelativeAxisCode::REL_WHEEL, -3),
            Action::HorizontalScrollLeft => linux::scroll(RelativeAxisCode::REL_HWHEEL, -3),
            Action::HorizontalScrollRight => linux::scroll(RelativeAxisCode::REL_HWHEEL, 3),
            // ── Custom shortcut ───────────────────────────────────────────────
            Action::CustomShortcut(combo) => {
                if combo.key_code == 0 {
                    tracing::warn!(
                        chord = %combo.rendered_label(),
                        "CustomShortcut with no key code — press ignored"
                    );
                    return;
                }
                let Some(key) = linux::macos_vk_to_linux(combo.key_code) else {
                    tracing::warn!(
                        key_code = combo.key_code,
                        "CustomShortcut key code has no Linux mapping — press ignored"
                    );
                    return;
                };
                linux::press_key(&linux::modifiers_to_keycodes(combo.modifiers), key);
            }
        }
    }

    /// macOS implementation: dispatch to the appropriate event helper.
    #[cfg(target_os = "macos")]
    fn execute_macos(&self) {
        use core_graphics::event::{CGEventFlags, CGMouseButton};

        // Modifier bit shorthands.
        let cmd = CGEventFlags::CGEventFlagCommand;
        let shift = CGEventFlags::CGEventFlagShift;
        let ctrl = CGEventFlags::CGEventFlagControl;
        let none = CGEventFlags::CGEventFlagNull;

        match self {
            // Suppressed input: captured but deliberately produces no event.
            Action::None => {}
            // ── Mouse clicks: synthesise a click at the cursor ────────────────
            // Remapping a *different* button to a click lands here (e.g. Back →
            // MiddleClick). A button left on its own native click never reaches
            // this — the hook passes it straight through to the OS.
            Action::LeftClick => macos::post_click(CGMouseButton::Left),
            Action::RightClick => macos::post_click(CGMouseButton::Right),
            Action::MiddleClick => macos::post_click(CGMouseButton::Center),
            // ── Editing ───────────────────────────────────────────────────────
            Action::Copy => macos::post_key(VK_C, cmd),
            Action::Paste => macos::post_key(VK_V, cmd),
            Action::Cut => macos::post_key(VK_X, cmd),
            Action::Undo => macos::post_key(VK_Z, cmd),
            Action::Redo => macos::post_key(VK_Z, cmd | shift),
            Action::SelectAll => macos::post_key(VK_A, cmd),
            Action::Find => macos::post_key(VK_F, cmd),
            Action::Save => macos::post_key(VK_S, cmd),
            // ── Browser / Navigation ──────────────────────────────────────────
            // BrowserBack/Forward: Cmd+[ / Cmd+] as keyboard fallback; hook
            // layer handles the physical mouse buttons directly.
            // kVK_ANSI_LeftBracket = 0x21, kVK_ANSI_RightBracket = 0x1E
            Action::BrowserBack => macos::post_key(0x21, cmd),
            Action::BrowserForward => macos::post_key(0x1E, cmd),
            Action::NewTab => macos::post_key(VK_T, cmd),
            Action::CloseTab => macos::post_key(VK_W, cmd),
            Action::ReopenTab => macos::post_key(VK_T, cmd | shift),
            Action::NextTab => macos::post_key(VK_TAB, ctrl),
            Action::PrevTab => macos::post_key(VK_TAB, ctrl | shift),
            Action::ReloadPage => macos::post_key(VK_R, cmd),
            // ── Navigation / Window: posted straight to the Dock ──────────────
            // Synthesising these shortcuts is unreliable — the WindowServer
            // matcher needs the exact configured key (incl. the Fn flag) and
            // Show Desktop ignores synthetic events entirely — so they go to the
            // Dock via `CoreDockSendNotification`, which fires regardless of the
            // user's keyboard settings.
            Action::MissionControl => macos::mission_control(),
            Action::AppExpose => macos::app_expose(),
            Action::PreviousDesktop => macos::previous_desktop(),
            Action::NextDesktop => macos::next_desktop(),
            Action::ShowDesktop => macos::show_desktop(),
            Action::LaunchpadShow => macos::launchpad(),
            // ── System ────────────────────────────────────────────────────────
            // Lock screen = Cmd+Ctrl+Q (kVK_ANSI_Q = 0x0C)
            Action::LockScreen => macos::post_key(0x0C, cmd | ctrl),
            // Screenshot = Cmd+Shift+3 (kVK_ANSI_3 = 0x14)
            Action::Screenshot => macos::post_key(0x14, cmd | shift),
            // ── Media ─────────────────────────────────────────────────────────
            // NX_KEYTYPE_PLAY=16, NEXT=17, PREVIOUS=18 via NSSystemDefined stub.
            Action::PlayPause => macos::post_media_key(0),
            Action::NextTrack => macos::post_media_key(1),
            Action::PrevTrack => macos::post_media_key(2),
            // kVK_VolumeUp/Down/Mute = 0x48/0x49/0x4A (ADB codes)
            Action::VolumeUp => macos::post_key(0x48, none),
            Action::VolumeDown => macos::post_key(0x49, none),
            Action::MuteVolume => macos::post_key(0x4A, none),
            // ── DPI / SmartShift: handled at hook/HID layer ───────────────────
            Action::CycleDpiPresets | Action::SetDpiPreset(_) | Action::ToggleSmartShift => {
                tracing::debug!(
                    action = self.label(),
                    "device action handled by hook/HID layer"
                );
            }
            // ── Scroll ────────────────────────────────────────────────────────
            Action::ScrollUp
            | Action::ScrollDown
            | Action::HorizontalScrollLeft
            | Action::HorizontalScrollRight => macos::post_scroll(self),
            // ── Custom ────────────────────────────────────────────────────────
            Action::CustomShortcut(combo) => {
                // P1.3: post the recorded chord. `key_code == 0` is the
                // "modifier-only placeholder" the recorder UI rejects;
                // skip it here too so a malformed config doesn't fire
                // bare modifier presses.
                if combo.key_code == 0 {
                    tracing::warn!(
                        chord = %combo.rendered_label(),
                        "CustomShortcut with no key code — press ignored"
                    );
                    return;
                }
                let mut flags = CGEventFlags::CGEventFlagNull;
                if combo.modifiers & KeyCombo::MOD_CMD != 0 {
                    flags |= CGEventFlags::CGEventFlagCommand;
                }
                if combo.modifiers & KeyCombo::MOD_SHIFT != 0 {
                    flags |= CGEventFlags::CGEventFlagShift;
                }
                if combo.modifiers & KeyCombo::MOD_CTRL != 0 {
                    flags |= CGEventFlags::CGEventFlagControl;
                }
                if combo.modifiers & KeyCombo::MOD_OPTION != 0 {
                    flags |= CGEventFlags::CGEventFlagAlternate;
                }
                macos::post_key(combo.key_code, flags);
            }
        }
    }
}

/// Synthesise a horizontal scroll of `delta` wheel lines at the current focus.
///
/// Used by the gesture/thumbwheel capture watcher to re-inject the MX thumb
/// wheel's scrolling after the wheel has been diverted over HID++ to capture its
/// click. `delta` is the device's raw rotation; its sign follows the wheel's
/// rotation convention and its magnitude (one line per rotation increment) may
/// need tuning per device, since the diverted resolution differs from native.
///
/// No-op (logs nothing) on platforms without a supported injection mechanism.
pub fn post_horizontal_scroll(delta: i32) {
    #[cfg(target_os = "macos")]
    macos::post_horizontal_scroll(delta);

    #[cfg(target_os = "linux")]
    linux::scroll(evdev::RelativeAxisCode::REL_HWHEEL, delta);

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = delta;
}

/// Return the `/dev/input/eventN` node for the action-injector uinput device,
/// initialising it if needed.
///
/// Intended for debugging and manual smoke-testing (e.g. attaching `evtest`
/// before firing `Action::execute`). Returns `None` on non-Linux platforms or
/// when the device could not be created (e.g. `/dev/uinput` not writable).
#[cfg(target_os = "linux")]
#[must_use]
pub fn action_device_path() -> Option<std::path::PathBuf> {
    linux::device_node()
}

// ── macOS virtual key codes ────────────────────────────────────────────────
// Source: <HIToolbox/Events.h> kVK_* constants. Values are layout-independent
// for the US ANSI keyboard.
#[cfg(target_os = "macos")]
const VK_A: u16 = 0x00;
#[cfg(target_os = "macos")]
const VK_C: u16 = 0x08;
#[cfg(target_os = "macos")]
const VK_F: u16 = 0x03;
#[cfg(target_os = "macos")]
const VK_R: u16 = 0x0F;
#[cfg(target_os = "macos")]
const VK_S: u16 = 0x01;
#[cfg(target_os = "macos")]
const VK_T: u16 = 0x11;
#[cfg(target_os = "macos")]
const VK_V: u16 = 0x09;
#[cfg(target_os = "macos")]
const VK_W: u16 = 0x0D;
#[cfg(target_os = "macos")]
const VK_X: u16 = 0x07;
#[cfg(target_os = "macos")]
const VK_Z: u16 = 0x06;
#[cfg(target_os = "macos")]
const VK_TAB: u16 = 0x30;

/// Platform helpers for synthesising OS-level input events on macOS.
#[cfg(target_os = "macos")]
mod macos {
    use core_graphics::event::{
        CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, ScrollEventUnit,
    };
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::CGPoint;

    use crate::binding::Action;

    /// Post a mouse-down + mouse-up pair for `button` at the cursor's current
    /// location.
    ///
    /// Posted at the HID tap location, so OpenLogi's own event tap sees the
    /// synthetic click too: a `LeftClick`/`RightClick` flows straight through
    /// (the tap never owns the primary buttons), and a `MiddleClick` is left
    /// alone unless the user has *also* remapped the middle button.
    pub(super) fn post_click(button: CGMouseButton) {
        let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
            tracing::warn!("CGEventSource::new failed for click");
            return;
        };
        // A fresh event reports the current pointer location; mouse events need
        // an explicit position or they land at (0, 0).
        let location = CGEvent::new(src.clone()).map_or(CGPoint::new(0., 0.), |e| e.location());
        let (down, up) = match button {
            CGMouseButton::Left => (CGEventType::LeftMouseDown, CGEventType::LeftMouseUp),
            CGMouseButton::Right => (CGEventType::RightMouseDown, CGEventType::RightMouseUp),
            CGMouseButton::Center => (CGEventType::OtherMouseDown, CGEventType::OtherMouseUp),
        };
        for (kind, phase) in [(down, "down"), (up, "up")] {
            if let Ok(ev) = CGEvent::new_mouse_event(src.clone(), kind, location, button) {
                ev.post(CGEventTapLocation::HID);
            } else {
                tracing::warn!(phase, "CGEvent::new_mouse_event failed");
            }
        }
    }

    /// Post a key-down + key-up pair for `vk` with `flags` set.
    pub(super) fn post_key(vk: u16, flags: CGEventFlags) {
        let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
            tracing::warn!("CGEventSource::new failed");
            return;
        };
        let Ok(down) = CGEvent::new_keyboard_event(src.clone(), vk, true) else {
            tracing::warn!("CGEvent::new_keyboard_event(down) failed");
            return;
        };
        down.set_flags(flags);
        down.post(CGEventTapLocation::HID);
        let Ok(up) = CGEvent::new_keyboard_event(src, vk, false) else {
            tracing::warn!("CGEvent::new_keyboard_event(up) failed");
            return;
        };
        up.set_flags(flags);
        up.post(CGEventTapLocation::HID);
    }

    /// Post a media key event (Play/Pause, Next, Previous).
    ///
    /// `kind`: 0 = play/pause, 1 = next track, 2 = previous track.
    ///
    /// The proper implementation uses an `NSSystemDefined` event (type 14,
    /// subtype 8) which requires AppKit bindings. Until those land this
    /// function logs a debug trace so manual smoke tests can confirm the
    /// correct execution path.
    pub(super) fn post_media_key(kind: i32) {
        // NX_KEYTYPE_PLAY=16, NX_KEYTYPE_NEXT=17, NX_KEYTYPE_PREVIOUS=18.
        let nx_key: i64 = match kind {
            0 => 16,
            1 => 17,
            _ => 18,
        };
        tracing::debug!(
            nx_key,
            "media key event: NSSystemDefined stub — full AppKit impl tracked in P1.x"
        );
    }

    /// Post a synthetic scroll event for `action` (one of the `Scroll*` variants).
    pub(super) fn post_scroll(action: &Action) {
        let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
            tracing::warn!("CGEventSource::new failed for scroll");
            return;
        };
        let (v, h): (i32, i32) = match action {
            Action::ScrollUp => (3, 0),
            Action::ScrollDown => (-3, 0),
            Action::HorizontalScrollLeft => (0, -3),
            Action::HorizontalScrollRight => (0, 3),
            _ => return,
        };
        let Ok(ev) = CGEvent::new_scroll_event(src, ScrollEventUnit::PIXEL, 2, v, h, 0) else {
            tracing::warn!("CGEvent::new_scroll_event failed");
            return;
        };
        ev.post(CGEventTapLocation::HID);
    }

    /// Post a horizontal scroll of `delta` lines (wheel2 axis). Line units suit
    /// the thumb wheel's ratchet-like increments better than pixels.
    pub(super) fn post_horizontal_scroll(delta: i32) {
        let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
            tracing::warn!("CGEventSource::new failed for thumbwheel scroll");
            return;
        };
        let Ok(ev) = CGEvent::new_scroll_event(src, ScrollEventUnit::LINE, 2, 0, delta, 0) else {
            tracing::warn!("CGEvent::new_scroll_event failed for thumbwheel");
            return;
        };
        ev.post(CGEventTapLocation::HID);
    }

    pub(super) use dock::{app_expose, launchpad, mission_control, show_desktop};
    pub(super) use symbolic_hotkey::{next_desktop, previous_desktop};

    use app_services::symbol as app_services_symbol;

    /// Shared resolver for private ApplicationServices SPI used by the Dock and
    /// symbolic-hotkey helpers.
    #[allow(
        unsafe_code,
        reason = "private ApplicationServices SPI symbols are resolved via dlopen/dlsym FFI"
    )]
    mod app_services {
        use std::ffi::{CStr, c_char, c_int, c_void};
        use std::sync::OnceLock;

        /// Resolve a symbol from ApplicationServices, caching the `dlopen`
        /// handle for the process lifetime. Returns `None` if the framework or
        /// symbol is unavailable on this macOS version.
        pub(super) fn symbol(symbol: &CStr) -> Option<*mut c_void> {
            const RTLD_LAZY: c_int = 0x1;
            const APP_SERVICES: &CStr =
                c"/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices";
            static HANDLE: OnceLock<usize> = OnceLock::new();

            // SAFETY: `dlopen`/`dlsym` come from libSystem; APP_SERVICES and
            // `symbol` are valid C strings. The handle is cached and
            // intentionally never closed.
            let sym = unsafe {
                let handle =
                    *HANDLE.get_or_init(|| dlopen(APP_SERVICES.as_ptr(), RTLD_LAZY) as usize);
                if handle == 0 {
                    return None;
                }
                dlsym(handle as *mut c_void, symbol.as_ptr())
            };
            (!sym.is_null()).then_some(sym)
        }

        unsafe extern "C" {
            fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
            fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        }
    }

    /// WindowServer window/space actions (Mission Control, App Exposé, Show
    /// Desktop, Launchpad).
    ///
    /// These are driven by the Dock, and synthesising their keyboard shortcut is
    /// unreliable — the WindowServer matcher needs the exact configured key
    /// (incl. the Fn flag) and Show Desktop's in particular doesn't respond. So
    /// we post the action straight to the Dock via the private
    /// `CoreDockSendNotification` SPI, which fires it regardless of the user's
    /// Keyboard settings.
    ///
    /// Isolated in its own submodule so the `unsafe` the `dlopen`/`dlsym` FFI
    /// needs is scoped here rather than spread across the platform helpers.
    #[allow(
        unsafe_code,
        reason = "the private CoreDockSendNotification SPI is only reachable via dlopen/dlsym FFI"
    )]
    mod dock {
        use std::ffi::{c_int, c_void};

        use core_foundation::base::TCFType;
        use core_foundation::string::CFString;

        use super::app_services_symbol;

        /// Show all windows across spaces (Mission Control).
        pub(crate) fn mission_control() {
            send("com.apple.expose.awake");
        }

        /// Show the front app's windows (App Exposé).
        pub(crate) fn app_expose() {
            send("com.apple.expose.front.awake");
        }

        /// Move all windows aside to reveal the desktop.
        pub(crate) fn show_desktop() {
            send("com.apple.showdesktop.awake");
        }

        /// Toggle Launchpad. A no-op on macOS 26, which removed Launchpad.
        pub(crate) fn launchpad() {
            send("com.apple.launchpad.toggle");
        }

        /// Post `notification` to the Dock. Logs and returns on any failure.
        fn send(notification: &str) {
            let Some(core_dock_send) = core_dock_send_notification() else {
                tracing::warn!(notification, "CoreDockSendNotification unavailable");
                return;
            };
            let name = CFString::new(notification);
            // SAFETY: resolved AppServices symbol called with its documented
            // signature; `name` is a live CFString for the call's duration.
            let err = unsafe { core_dock_send(name.as_concrete_TypeRef().cast(), 0) };
            if err != 0 {
                tracing::warn!(notification, err, "CoreDockSendNotification failed");
            }
        }

        type CoreDockSendNotificationFn = unsafe extern "C" fn(*const c_void, c_int) -> c_int;

        /// Resolve `CoreDockSendNotification` from `ApplicationServices`, caching
        /// the `dlopen` handle for the process lifetime. `None` if unavailable.
        fn core_dock_send_notification() -> Option<CoreDockSendNotificationFn> {
            let sym = app_services_symbol(c"CoreDockSendNotification")?;
            // SAFETY: the symbol, when present, has the documented signature.
            Some(unsafe { std::mem::transmute::<*mut c_void, CoreDockSendNotificationFn>(sym) })
        }
    }

    /// macOS Space switching actions.
    ///
    /// Use the system symbolic hotkey records for "Move left a space" (79) and
    /// "Move right a space" (81). That respects the user's configured shortcut
    /// instead of assuming Ctrl+Left/Right, and temporarily enables the symbolic
    /// hotkey when the user has disabled it.
    #[allow(
        unsafe_code,
        reason = "CGS symbolic hotkey SPI is only reachable via dlopen/dlsym FFI"
    )]
    mod symbolic_hotkey {
        use std::ffi::{c_int, c_uint, c_ushort, c_void};

        use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
        use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

        use super::app_services_symbol;

        const SPACE_LEFT: u32 = 79;
        const SPACE_RIGHT: u32 = 81;

        /// Switch to the previous desktop / Space.
        pub(crate) fn previous_desktop() {
            post_symbolic_hotkey(SPACE_LEFT);
        }

        /// Switch to the next desktop / Space.
        pub(crate) fn next_desktop() {
            post_symbolic_hotkey(SPACE_RIGHT);
        }

        fn post_symbolic_hotkey(hotkey: u32) {
            let Some(cgs) = cgs_hotkey_api() else {
                tracing::warn!(hotkey, "CGS symbolic hotkey API unavailable");
                return;
            };

            let mut key_equivalent = 0_u16;
            let mut virtual_key = 0_u16;
            let mut modifiers = 0_u32;

            // SAFETY: resolved AppServices symbols are called with their
            // expected signatures and valid out-parameters.
            let err = unsafe {
                (cgs.get_value)(
                    hotkey,
                    &raw mut key_equivalent,
                    &raw mut virtual_key,
                    &raw mut modifiers,
                )
            };
            if err != 0 {
                tracing::warn!(hotkey, err, "CGSGetSymbolicHotKeyValue failed");
                return;
            }

            // SAFETY: resolved AppServices symbol called with its expected
            // signature.
            let was_enabled = unsafe { (cgs.is_enabled)(hotkey) };
            if !was_enabled {
                // SAFETY: resolved AppServices symbol called with its expected
                // signature.
                let err = unsafe { (cgs.set_enabled)(hotkey, true) };
                if err != 0 {
                    tracing::warn!(hotkey, err, "CGSSetSymbolicHotKeyEnabled(true) failed");
                }
            }

            post_key(virtual_key, modifiers);

            if !was_enabled {
                // SAFETY: resolved AppServices symbol called with its expected
                // signature.
                let err = unsafe { (cgs.set_enabled)(hotkey, false) };
                if err != 0 {
                    tracing::warn!(hotkey, err, "CGSSetSymbolicHotKeyEnabled(false) failed");
                }
            }
        }

        fn post_key(vk: u16, modifiers: u32) {
            let Ok(src) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
                tracing::warn!("CGEventSource::new failed for symbolic hotkey");
                return;
            };
            let Ok(down) = CGEvent::new_keyboard_event(src.clone(), vk, true) else {
                tracing::warn!(vk, "CGEvent::new_keyboard_event(down) failed");
                return;
            };
            let flags = CGEventFlags::from_bits_truncate(u64::from(modifiers));
            down.set_flags(flags);
            down.post(CGEventTapLocation::Session);

            let Ok(up) = CGEvent::new_keyboard_event(src, vk, false) else {
                tracing::warn!(vk, "CGEvent::new_keyboard_event(up) failed");
                return;
            };
            up.set_flags(flags);
            up.post(CGEventTapLocation::Session);
        }

        #[derive(Clone, Copy)]
        struct CgsHotkeyApi {
            get_value: CgsGetSymbolicHotKeyValueFn,
            is_enabled: CgsIsSymbolicHotKeyEnabledFn,
            set_enabled: CgsSetSymbolicHotKeyEnabledFn,
        }

        type CgsGetSymbolicHotKeyValueFn =
            unsafe extern "C" fn(c_uint, *mut c_ushort, *mut c_ushort, *mut c_uint) -> c_int;
        type CgsIsSymbolicHotKeyEnabledFn = unsafe extern "C" fn(c_uint) -> bool;
        type CgsSetSymbolicHotKeyEnabledFn = unsafe extern "C" fn(c_uint, bool) -> c_int;

        fn cgs_hotkey_api() -> Option<CgsHotkeyApi> {
            let get_value = app_services_symbol(c"CGSGetSymbolicHotKeyValue")?;
            let is_enabled = app_services_symbol(c"CGSIsSymbolicHotKeyEnabled")?;
            let set_enabled = app_services_symbol(c"CGSSetSymbolicHotKeyEnabled")?;

            // SAFETY: the symbols, when present, have the private SPI
            // signatures declared above.
            Some(unsafe {
                CgsHotkeyApi {
                    get_value: std::mem::transmute::<*mut c_void, CgsGetSymbolicHotKeyValueFn>(
                        get_value,
                    ),
                    is_enabled: std::mem::transmute::<*mut c_void, CgsIsSymbolicHotKeyEnabledFn>(
                        is_enabled,
                    ),
                    set_enabled: std::mem::transmute::<*mut c_void, CgsSetSymbolicHotKeyEnabledFn>(
                        set_enabled,
                    ),
                }
            })
        }
    }
}

/// Sensible defaults for a fresh device so the panel isn't empty on first run.
///
/// Thumbwheel / GestureButton defaults match what Logi Options+ ships for
/// MX-line devices: thumb wheel click → App Exposé, gesture button →
/// Mission Control. The thumb wheel isn't captured yet; the gesture button is
/// (per-direction, see [`default_gesture_binding`]). The bindings persist
/// regardless so the user only configures once.
///
/// `GestureButton`'s entry here is the legacy single-binding placeholder;
/// the per-direction sub-bindings live in [`default_gesture_binding`] and
/// are what the UI now edits.
#[must_use]
pub fn default_binding(button: ButtonId) -> Action {
    match button {
        ButtonId::LeftClick => Action::LeftClick,
        ButtonId::RightClick => Action::RightClick,
        ButtonId::MiddleClick => Action::MiddleClick,
        ButtonId::Back => Action::BrowserBack,
        ButtonId::Forward => Action::BrowserForward,
        ButtonId::DpiToggle => Action::CycleDpiPresets,
        ButtonId::Thumbwheel => Action::AppExpose,
        // The thumb wheel scrolls horizontally by default: rotating it produces
        // continuous horizontal scroll, with "up" → right and "down" → left.
        // The wheel watcher renders these two actions as smooth, sensitivity-
        // scaled scrolling rather than the discrete per-press burst a button
        // would get (see `watchers::gesture`).
        ButtonId::ThumbwheelScrollUp => Action::HorizontalScrollRight,
        ButtonId::ThumbwheelScrollDown => Action::HorizontalScrollLeft,
        ButtonId::GestureButton => Action::MissionControl,
    }
}

/// Per-direction defaults for the gesture button. These are captured live over
/// HID++ `0x1b04` (raw-XY diversion) and dispatched like any other binding; the
/// defaults give the picker something sensible to show on first run.
#[must_use]
pub fn default_gesture_binding(direction: GestureDirection) -> Action {
    match direction {
        GestureDirection::Up => Action::MissionControl,
        GestureDirection::Down => Action::ShowDesktop,
        GestureDirection::Left => Action::PrevTab,
        GestureDirection::Right => Action::NextTab,
        GestureDirection::Click => Action::AppExpose,
    }
}

/// Linux helpers for synthesising OS-level input events via a shared `uinput`
/// virtual device.
///
/// The device is created lazily on first use. If `/dev/uinput` is inaccessible
/// (missing group membership or udev rule) every call logs a `warn` and returns
/// without panicking.
#[cfg(target_os = "linux")]
mod linux {
    use std::io;
    use std::sync::{LazyLock, Mutex};

    use evdev::uinput::VirtualDevice;
    use evdev::{AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode};

    const DEVICE_NAME: &str = "OpenLogi action injector";

    static VIRTUAL_INPUT: LazyLock<Option<Mutex<VirtualDevice>>> = LazyLock::new(|| {
        build()
            .map(Mutex::new)
            .map_err(|e| tracing::warn!("failed to create uinput action device: {e}"))
            .ok()
    });

    #[allow(clippy::too_many_lines)] // key-capability table — inherently long
    fn build() -> io::Result<VirtualDevice> {
        let mut keys = AttributeSet::<KeyCode>::default();
        for k in [
            // Letters
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
            // Digits
            KeyCode::KEY_0,
            KeyCode::KEY_1,
            KeyCode::KEY_2,
            KeyCode::KEY_3,
            KeyCode::KEY_4,
            KeyCode::KEY_5,
            KeyCode::KEY_6,
            KeyCode::KEY_7,
            KeyCode::KEY_8,
            KeyCode::KEY_9,
            // Punctuation / symbols
            KeyCode::KEY_MINUS,
            KeyCode::KEY_EQUAL,
            KeyCode::KEY_LEFTBRACE,
            KeyCode::KEY_RIGHTBRACE,
            KeyCode::KEY_BACKSLASH,
            KeyCode::KEY_SEMICOLON,
            KeyCode::KEY_APOSTROPHE,
            KeyCode::KEY_GRAVE,
            KeyCode::KEY_COMMA,
            KeyCode::KEY_DOT,
            KeyCode::KEY_SLASH,
            // Navigation / editing
            KeyCode::KEY_LEFT,
            KeyCode::KEY_RIGHT,
            KeyCode::KEY_UP,
            KeyCode::KEY_DOWN,
            KeyCode::KEY_HOME,
            KeyCode::KEY_END,
            KeyCode::KEY_PAGEUP,
            KeyCode::KEY_PAGEDOWN,
            KeyCode::KEY_TAB,
            KeyCode::KEY_ENTER,
            KeyCode::KEY_BACKSPACE,
            KeyCode::KEY_DELETE,
            KeyCode::KEY_ESC,
            KeyCode::KEY_SPACE,
            // Modifiers
            KeyCode::KEY_LEFTCTRL,
            KeyCode::KEY_LEFTSHIFT,
            KeyCode::KEY_LEFTALT,
            KeyCode::KEY_LEFTMETA,
            // Function keys
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
            // System
            KeyCode::KEY_SYSRQ,
            // Multimedia
            KeyCode::KEY_PLAYPAUSE,
            KeyCode::KEY_NEXTSONG,
            KeyCode::KEY_PREVIOUSSONG,
            KeyCode::KEY_VOLUMEUP,
            KeyCode::KEY_VOLUMEDOWN,
            KeyCode::KEY_MUTE,
            // Mouse buttons (injected as EV_KEY with BTN_* codes)
            KeyCode::BTN_LEFT,
            KeyCode::BTN_RIGHT,
            KeyCode::BTN_MIDDLE,
        ] {
            keys.insert(k);
        }

        let mut axes = AttributeSet::<RelativeAxisCode>::default();
        for a in [
            RelativeAxisCode::REL_X,
            RelativeAxisCode::REL_Y,
            RelativeAxisCode::REL_WHEEL,
            RelativeAxisCode::REL_HWHEEL,
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

    /// Inject modifier-down, key-down, key-up, modifier-up (in reverse order),
    /// followed by `SYN_REPORT`.
    pub(super) fn press_key(mods: &[KeyCode], key: KeyCode) {
        let mut events: Vec<InputEvent> = Vec::with_capacity(mods.len() * 2 + 3);
        for &m in mods {
            events.push(key_ev(m, 1));
        }
        events.push(key_ev(key, 1));
        events.push(key_ev(key, 0));
        for &m in mods.iter().rev() {
            events.push(key_ev(m, 0));
        }
        events.push(syn());
        emit(&events);
    }

    /// Inject a button-down + button-up pair followed by `SYN_REPORT`.
    pub(super) fn click(button: KeyCode) {
        emit(&[key_ev(button, 1), key_ev(button, 0), syn()]);
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
        if let Some(m) = &*VIRTUAL_INPUT {
            if let Ok(mut guard) = m.lock() {
                return guard.enumerate_dev_nodes_blocking().ok()?.flatten().next();
            }
        }
        None
    }


    /// Convert a [`KeyCombo`] modifier bitmask to the evdev keys to hold.
    ///
    /// macOS Cmd (`MOD_CMD`) and Ctrl (`MOD_CTRL`) both map to `KEY_LEFTCTRL`;
    /// the bitwise-OR check deduplicates them so at most one Ctrl is pushed.
    /// Order is canonical: Ctrl → Shift → Alt.
    pub(super) fn modifiers_to_keycodes(modifiers: u8) -> Vec<KeyCode> {
        use crate::binding::KeyCombo;
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
    pub(super) fn macos_vk_to_linux(vk: u16) -> Option<KeyCode> {
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
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
mod tests {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Serialize};

    use super::*;

    // ── Roundtrip wrapper: defined here so it precedes any `let` statements ──

    /// Minimal TOML-serializable wrapper used by `roundtrip`.
    /// Defined at module scope to satisfy `clippy::items_after_statements`.
    #[derive(Serialize, Deserialize)]
    struct RoundtripWrapper {
        binding: BTreeMap<ButtonId, Action>,
    }

    // ── Catalog tests ─────────────────────────────────────────────────────────

    #[test]
    fn catalog_has_at_least_29_entries() {
        let catalog = Action::catalog();
        assert!(
            catalog.len() >= 29,
            "catalog has {} entries, need ≥ 29",
            catalog.len()
        );
    }

    #[test]
    fn catalog_excludes_custom_shortcut() {
        let catalog = Action::catalog();
        for action in &catalog {
            assert!(
                !matches!(action, Action::CustomShortcut(_)),
                "catalog must not contain CustomShortcut"
            );
        }
    }

    // ── Gesture classification ────────────────────────────────────────────────

    #[test]
    fn detect_swipe_below_threshold_keeps_accumulating() {
        // Too little travel to commit — caller keeps summing raw-XY.
        assert_eq!(detect_swipe(40, 5), None);
        assert_eq!(detect_swipe(0, 0), None);
    }

    #[test]
    fn detect_swipe_commits_clean_direction() {
        assert_eq!(detect_swipe(120, 5), Some(GestureDirection::Right));
        assert_eq!(detect_swipe(-120, 5), Some(GestureDirection::Left));
        assert_eq!(detect_swipe(5, 120), Some(GestureDirection::Down));
        assert_eq!(detect_swipe(5, -120), Some(GestureDirection::Up));
    }

    #[test]
    fn detect_swipe_rejects_diagonal() {
        // Past the threshold but too diagonal (cross axis beyond the band).
        assert_eq!(detect_swipe(60, 60), None);
        assert_eq!(detect_swipe(-60, -60), None);
    }

    // ── TOML roundtrip ────────────────────────────────────────────────────────

    /// Serialize then deserialize `action` through TOML, using a wrapper
    /// struct because TOML requires a top-level table.
    fn roundtrip(action: &Action) -> Action {
        let mut map: BTreeMap<ButtonId, Action> = BTreeMap::new();
        map.insert(ButtonId::Back, action.clone());
        let w = RoundtripWrapper { binding: map };
        let s = toml::to_string(&w).expect("serialize");
        let back: RoundtripWrapper = toml::from_str(&s).expect("deserialize");
        back.binding
            .into_values()
            .next()
            .expect("binding present after roundtrip")
    }

    #[test]
    fn all_catalog_variants_roundtrip_toml() {
        for action in Action::catalog() {
            let back = roundtrip(&action);
            assert_eq!(action, back, "TOML roundtrip failed for {action:?}");
        }
    }

    #[test]
    fn custom_shortcut_roundtrips_toml() {
        let action = Action::CustomShortcut(KeyCombo {
            modifiers: KeyCombo::MOD_CMD | KeyCombo::MOD_SHIFT,
            key_code: 0x23, // kVK_ANSI_P
            display: "⌘⇧P".into(),
        });
        assert_eq!(roundtrip(&action), action);
    }

    #[test]
    fn key_combo_rendered_label_uses_display_when_set() {
        let combo = KeyCombo {
            modifiers: 0,
            key_code: 0,
            display: "preset".into(),
        };
        assert_eq!(combo.rendered_label(), "preset");
    }

    #[test]
    fn key_combo_rendered_label_falls_back_to_modifiers_plus_key() {
        let combo = KeyCombo {
            modifiers: KeyCombo::MOD_CMD | KeyCombo::MOD_SHIFT,
            key_code: 0x23, // P
            display: String::new(),
        };
        assert_eq!(combo.rendered_label(), "⇧⌘P");
    }

    // ── Category tests ────────────────────────────────────────────────────────

    #[test]
    fn category_editing_variants() {
        assert_eq!(Action::Copy.category(), Category::Editing);
        assert_eq!(Action::Undo.category(), Category::Editing);
        assert_eq!(Action::SelectAll.category(), Category::Editing);
        assert_eq!(Action::Find.category(), Category::Editing);
        assert_eq!(Action::Save.category(), Category::Editing);
        assert_eq!(Action::Cut.category(), Category::Editing);
        assert_eq!(Action::Redo.category(), Category::Editing);
        assert_eq!(Action::Paste.category(), Category::Editing);
    }

    #[test]
    fn category_browser_variants() {
        assert_eq!(Action::BrowserBack.category(), Category::Browser);
        assert_eq!(Action::BrowserForward.category(), Category::Browser);
        assert_eq!(Action::NewTab.category(), Category::Browser);
        assert_eq!(Action::CloseTab.category(), Category::Browser);
        assert_eq!(Action::ReopenTab.category(), Category::Browser);
        assert_eq!(Action::NextTab.category(), Category::Browser);
        assert_eq!(Action::PrevTab.category(), Category::Browser);
        assert_eq!(Action::ReloadPage.category(), Category::Browser);
    }

    #[test]
    fn category_media_variants() {
        assert_eq!(Action::PlayPause.category(), Category::Media);
        assert_eq!(Action::NextTrack.category(), Category::Media);
        assert_eq!(Action::PrevTrack.category(), Category::Media);
        assert_eq!(Action::VolumeUp.category(), Category::Media);
        assert_eq!(Action::VolumeDown.category(), Category::Media);
        assert_eq!(Action::MuteVolume.category(), Category::Media);
    }

    #[test]
    fn category_mouse_variants() {
        assert_eq!(Action::LeftClick.category(), Category::Mouse);
        assert_eq!(Action::RightClick.category(), Category::Mouse);
        assert_eq!(Action::MiddleClick.category(), Category::Mouse);
    }

    #[test]
    fn category_dpi_variants() {
        assert_eq!(Action::CycleDpiPresets.category(), Category::Dpi);
        assert_eq!(Action::ToggleSmartShift.category(), Category::Dpi);
    }

    #[test]
    fn category_scroll_variants() {
        assert_eq!(Action::ScrollUp.category(), Category::Scroll);
        assert_eq!(Action::ScrollDown.category(), Category::Scroll);
        assert_eq!(Action::HorizontalScrollLeft.category(), Category::Scroll);
        assert_eq!(Action::HorizontalScrollRight.category(), Category::Scroll);
    }

    #[test]
    fn category_navigation_variants() {
        assert_eq!(Action::MissionControl.category(), Category::Navigation);
        assert_eq!(Action::AppExpose.category(), Category::Navigation);
        assert_eq!(Action::PreviousDesktop.category(), Category::Navigation);
        assert_eq!(Action::NextDesktop.category(), Category::Navigation);
        assert_eq!(Action::ShowDesktop.category(), Category::Navigation);
        assert_eq!(Action::LaunchpadShow.category(), Category::Navigation);
    }

    #[test]
    fn category_system_variants() {
        assert_eq!(Action::LockScreen.category(), Category::System);
        assert_eq!(Action::Screenshot.category(), Category::System);
    }

    // ── Category label smoke test ─────────────────────────────────────────────

    #[test]
    fn category_labels_are_nonempty() {
        let categories = [
            Category::Editing,
            Category::Browser,
            Category::Media,
            Category::Mouse,
            Category::Dpi,
            Category::Scroll,
            Category::Navigation,
            Category::System,
        ];
        for cat in categories {
            assert!(!cat.label().is_empty(), "label empty for {cat:?}");
        }
    }

    // ── Default binding ───────────────────────────────────────────────────────

    #[test]
    fn dpi_toggle_default_is_cycle_dpi_presets() {
        assert_eq!(
            default_binding(ButtonId::DpiToggle),
            Action::CycleDpiPresets
        );
    }

    // ── modifiers_to_keycodes ─────────────────────────────────────────────────

    #[cfg(target_os = "linux")]
    mod modifier_mapping {
        use evdev::KeyCode;

        use crate::binding::{KeyCombo, linux::modifiers_to_keycodes};

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

    // ── macos_vk_to_linux ────────────────────────────────────────────────────

    #[cfg(target_os = "linux")]
    mod vk_mapping {
        use evdev::KeyCode;

        use crate::binding::linux::macos_vk_to_linux;

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
