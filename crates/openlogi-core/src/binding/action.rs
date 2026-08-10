//! The action vocabulary a button can bind to, plus workflow steps.

use serde::{Deserialize, Serialize};

use super::application_target::ApplicationTarget;
use super::category::Category;
use super::key_combo::KeyCombo;

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
/// This type is pure config data: OS-level event synthesis for each variant
/// lives in the `openlogi-inject` crate (`openlogi_inject::execute`), keeping
/// this crate platform- and IO-free.
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
    /// Mouse "back" side button (extra button 4). Synthesizes the real mouse
    /// button event, which browsers and most apps interpret as "navigate back"
    /// natively — unlike [`Action::BrowserBack`], which sends ⌘[ and is ignored
    /// by many apps.
    MouseBack,
    /// Mouse "forward" side button (extra button 5). Native counterpart to
    /// [`Action::MouseBack`]; see [`Action::BrowserForward`] for the ⌘] form.
    MouseForward,

    // ── Editing ──────────────────────────────────────────────────────────────
    /// Copy the current selection (⌘C / Ctrl+C).
    Copy,
    /// Paste from the clipboard (⌘V / Ctrl+V).
    Paste,
    /// Cut the current selection (⌘X / Ctrl+X).
    Cut,
    /// Undo the last action (⌘Z / Ctrl+Z).
    Undo,
    /// Redo the last undone action (⌘⇧Z on macOS / Ctrl+Shift+Z on Linux).
    ///
    /// Note: Ctrl+Y is the dominant redo shortcut in LibreOffice and many GTK
    /// apps. Ctrl+Shift+Z is used here because it mirrors the macOS convention
    /// and works in GNOME text fields, browsers, and Electron apps. If Ctrl+Y
    /// coverage is needed, a `CustomShortcut` binding is the escape hatch.
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
    /// Lock the screen (⌘⌃Q on macOS).
    ///
    /// On Linux, calls `org.freedesktop.login1.Manager.LockSession($XDG_SESSION_ID)`
    /// on the system bus (current session only). Falls back to Super+L when
    /// `$XDG_SESSION_ID` is unset or on non-systemd systems.
    LockScreen,
    /// Capture a screenshot.
    Screenshot,
    /// Capture a selected screen region to the clipboard.
    ///
    /// macOS uses Cmd+Shift+Ctrl+4; Windows uses Win+Shift+S. Linux delegates
    /// to the desktop environment's screenshot handler via Print Screen.
    CaptureRegion,

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
    /// Holds the structured chord data so `openlogi_inject::execute` can post the
    /// real keystroke (macOS: CGEventPost with the encoded modifier flags).
    /// The `display` field is used by [`Action::label`] so the popover
    /// shows the user-friendly chord name.
    CustomShortcut(KeyCombo),

    // ── System (appended) ────────────────────────────────────────────────────
    /// Put the computer to sleep. Appended after `CustomShortcut` because the
    /// serde variant index is the wire format (see the stability contract
    /// above) — new variants only ever go at the end.
    Sleep,
    /// Type an arbitrary string by emitting unicode characters (macOS
    /// `CGEventKeyboardSetUnicodeString`). Used for macro text. Power-user
    /// escape hatch — excluded from the default catalog.
    TypeText(String),
    /// Run an AppleScript via `osascript -e <source>`. Power-user escape hatch.
    RunAppleScript(String),
    /// Run a shell command via `/bin/sh -c <command>`. Power-user escape hatch.
    RunShellCommand(String),
    /// Run a timed, ordered sequence of steps — the native, no-code version of
    /// "type 'bite me', wait 5s, press Enter, wait 5s, type more, Escape". Each
    /// step is one of the power-user actions or a `Delay`. The sequencer
    /// (`openlogi-inject`) runs them in order, awaiting `Delay`s. Power-user
    /// escape hatch — excluded from the default catalog.
    Workflow(Vec<WorkflowStep>),
    /// Open the configured Actions Ring at the current pointer position.
    /// The agent handles the ring session rather than the OS injector.
    ShowActionsRing,
    /// Open an application, folder, filesystem path, or platform URL.
    OpenApplication(ApplicationTarget),
}

/// One step in a [`Action::Workflow`]. A workflow is a `Vec<WorkflowStep>`
/// executed in order by the inject layer; `Delay` introduces a pause between
/// the surrounding steps.
///
/// `PressKey` reuses [`KeyCombo`] (the same model as [`Action::CustomShortcut`])
/// so a step can press a key chord. The other variants mirror their standalone
/// [`Action`] counterparts.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WorkflowStep {
    /// Type a unicode string (see [`Action::TypeText`]).
    TypeText(String),
    /// Press a key chord (see [`Action::CustomShortcut`] / [`KeyCombo`]).
    PressKey(KeyCombo),
    /// Wait `millis` milliseconds before the next step.
    Delay {
        /// Pause length in milliseconds.
        millis: u64,
    },
    /// Run an AppleScript (see [`Action::RunAppleScript`]).
    RunAppleScript(String),
    /// Run a shell command (see [`Action::RunShellCommand`]).
    RunShellCommand(String),
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
            Action::MouseBack => "Back (Button 4)".into(),
            Action::MouseForward => "Forward (Button 5)".into(),
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
            Action::CaptureRegion => "Capture Region".into(),
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
            Action::Sleep => "Sleep".into(),
            Action::TypeText(s) => format!("Type \"{s}\""),
            Action::RunAppleScript(_) => "Run AppleScript".into(),
            Action::RunShellCommand(_) => "Run Command".into(),
            Action::Workflow(steps) => format!("Workflow ({} steps)", steps.len()),
            Action::ShowActionsRing => "Actions Ring".into(),
            Action::OpenApplication(target) => format!("Open {}", target.display_name()),
        }
    }

    /// Which [`Category`] this action belongs to, used for popover grouping.
    #[must_use]
    pub fn category(&self) -> Category {
        match self {
            Action::LeftClick
            | Action::RightClick
            | Action::MiddleClick
            | Action::MouseBack
            | Action::MouseForward => Category::Mouse,
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
            | Action::CustomShortcut(_)
            | Action::TypeText(_)
            | Action::RunAppleScript(_)
            | Action::RunShellCommand(_)
            | Action::Workflow(_) => Category::Editing,
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
            Action::None
            | Action::LockScreen
            | Action::Screenshot
            | Action::CaptureRegion
            | Action::Sleep
            | Action::ShowActionsRing
            | Action::OpenApplication(_) => Category::System,
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
            Action::MouseBack,
            Action::MouseForward,
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
            Action::CaptureRegion,
            Action::Sleep,
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
}
