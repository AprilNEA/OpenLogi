//! The action vocabulary a button can bind to, plus workflow steps.

use serde::{Deserialize, Serialize};

use super::application_target::ApplicationTarget;
use super::category::Category;
use super::key_combo::KeyCombo;

/// What pressing a [`ButtonId`](crate::binding::ButtonId) should do.
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
    /// Hold an arbitrary recorded key chord for the lifetime of its physical
    /// button press. This is the push-to-talk counterpart to
    /// [`Action::CustomShortcut`], which emits an immediate down/up pair.
    ///
    /// Lifecycle-aware runtimes emit the chord's down edge when the press
    /// starts and its up edge for every terminal outcome, including capture
    /// cancellation and shutdown. Dispatchers without a release context must
    /// degrade this action to a balanced tap rather than leave keys held.
    HoldShortcut(KeyCombo),
    /// Step to the next onboard DPI preset (G-series `0x8100` special).
    NextDpiPreset,
    /// Step to the previous onboard DPI preset.
    PrevDpiPreset,
    /// Cycle the mouse's onboard flash profiles.
    CycleOnboardProfile,
    /// Cycle OpenLogi application profiles (default, then each saved per-app
    /// profile) for the triggering device.
    CycleAppProfile,
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

/// X-macro table of every payload-free [`Action`] variant.
///
/// Each row is `Variant "Label" Category Icon`, optionally followed by
/// `not_pickable` for a row [`Action::catalog`] must omit. This is the single
/// place a plain action is declared; payload-carrying variants (`SetDpiPreset`,
/// `CustomShortcut`, …) build their label/category/icon from their payload and
/// keep hand-written arms alongside the generated ones instead.
///
/// `macro_rules!` can only emit items into the module it is invoked from, so
/// this table doesn't generate code itself — it forwards its rows verbatim to
/// a `$callback!` macro chosen by the caller. `action.rs` (below) uses it to
/// derive [`Action::label`], [`Action::category`], and [`Action::catalog`];
/// `action_ring::icon` uses it to derive [`ActionRingIcon::for_action`](
/// super::action_ring::ActionRingIcon::for_action). Row order is
/// [`Action::catalog`]'s output order, grouped by category to match the
/// popover section layout — edit rows here only, never in a callback's match.
macro_rules! for_each_unit_action {
    ($callback:ident) => {
        $callback! {
            // Mouse
            LeftClick "Left Click" Mouse Pointer,
            RightClick "Right Click" Mouse Pointer,
            MiddleClick "Middle Click" Mouse Mouse,
            MouseBack "Back (Button 4)" Mouse MouseBack,
            MouseForward "Forward (Button 5)" Mouse MouseForward,
            // Editing
            Copy "Copy" Editing Copy,
            Paste "Paste" Editing Paste,
            Cut "Cut" Editing Cut,
            Undo "Undo" Editing Undo,
            Redo "Redo" Editing Redo,
            SelectAll "Select All" Editing SelectAll,
            Find "Find" Editing Search,
            Save "Save" Editing Save,
            // Browser
            BrowserBack "Browser Back" Browser ArrowLeft,
            BrowserForward "Browser Forward" Browser ArrowRight,
            NewTab "New Tab" Browser NewTab,
            CloseTab "Close Tab" Browser CloseTab,
            ReopenTab "Reopen Tab" Browser ReopenTab,
            NextTab "Next Tab" Browser NextTab,
            PrevTab "Previous Tab" Browser PreviousTab,
            ReloadPage "Reload Page" Browser Reload,
            // Navigation
            MissionControl "Mission Control" Navigation Grid,
            AppExpose "App Exposé" Navigation Layers,
            PreviousDesktop "Previous Desktop" Navigation PreviousDesktop,
            NextDesktop "Next Desktop" Navigation NextDesktop,
            ShowDesktop "Show Desktop" Navigation Monitor,
            LaunchpadShow "Launchpad" Navigation Applications,
            // System
            None "Do Nothing" System Ban,
            LockScreen "Lock Screen" System Lock,
            Screenshot "Screenshot" System Camera,
            CaptureRegion "Capture Region" System Camera,
            Sleep "Sleep" System Monitor,
            ShowActionsRing "Actions Ring" System Grid not_pickable,
            // Media
            PlayPause "Play / Pause" Media Play,
            NextTrack "Next Track" Media NextTrack,
            PrevTrack "Previous Track" Media PreviousTrack,
            VolumeUp "Volume Up" Media Volume,
            VolumeDown "Volume Down" Media VolumeDown,
            MuteVolume "Mute" Media Mute,
            // DPI
            CycleDpiPresets "Cycle DPI Presets" Dpi Gauge,
            NextDpiPreset "DPI Up" Dpi Gauge,
            PrevDpiPreset "DPI Down" Dpi Gauge,
            CycleOnboardProfile "Cycle Onboard Profile" Dpi Refresh,
            CycleAppProfile "Cycle App Profile" Dpi Refresh,
            ToggleSmartShift "Toggle SmartShift" Dpi Refresh,
            // Scroll
            ScrollUp "Scroll Up" Scroll ArrowUp,
            ScrollDown "Scroll Down" Scroll ArrowDown,
            HorizontalScrollLeft "Scroll Left" Scroll ScrollLeft,
            HorizontalScrollRight "Scroll Right" Scroll ScrollRight,
        }
    };
}
pub(super) use for_each_unit_action;

/// Builds `label`, `category`, and `catalog` from [`for_each_unit_action!`]'s
/// rows, splicing in the hand-written arms for payload-carrying variants so
/// each generated `match` still covers every [`Action`] variant exhaustively.
macro_rules! derive_action_core {
    ( $( $variant:ident $label:literal $category:ident $icon:ident $( $tag:ident )? ),* $(,)? ) => {
        impl Action {
            /// Display label for the popover row.
            ///
            /// Returns `String` rather than `&str` so parameterized variants (e.g.
            /// `SetDpiPreset(i)`, `CustomShortcut(s)`) can build a label that
            /// includes their payload.
            #[must_use]
            pub fn label(&self) -> String {
                match self {
                    $( Action::$variant => $label.into(), )*
                    Action::SetDpiPreset(i) => format!("DPI Preset {}", i + 1),
                    Action::CustomShortcut(combo) => combo.rendered_label(),
                    Action::TypeText(s) => format!("Type \"{s}\""),
                    Action::RunAppleScript(_) => "Run AppleScript".into(),
                    Action::RunShellCommand(_) => "Run Command".into(),
                    Action::Workflow(steps) => format!("Workflow ({} steps)", steps.len()),
                    Action::OpenApplication(target) => format!("Open {}", target.display_name()),
                    Action::HoldShortcut(combo) => format!("Hold {}", combo.rendered_label()),
                }
            }

            /// Which [`Category`] this action belongs to, used for popover grouping.
            #[must_use]
            pub fn category(&self) -> Category {
                match self {
                    $( Action::$variant => Category::$category, )*
                    // CustomShortcut is assigned to Editing so it doesn't need a
                    // separate arm (it's not in the picker catalog).
                    Action::CustomShortcut(_)
                    | Action::TypeText(_)
                    | Action::RunAppleScript(_)
                    | Action::RunShellCommand(_)
                    | Action::Workflow(_)
                    | Action::HoldShortcut(_) => Category::Editing,
                    Action::SetDpiPreset(_) => Category::Dpi,
                    Action::OpenApplication(_) => Category::System,
                }
            }

            /// All pickable actions in a deterministic order.
            ///
            /// [`Action::CustomShortcut`] is intentionally excluded — it is opened via
            /// "Record shortcut…" (P1.3), not selected from the catalog. Table rows
            /// tagged `not_pickable` (currently only [`Action::ShowActionsRing`], the
            /// fixed default for [`ButtonId::HapticPanel`](super::ButtonId::HapticPanel))
            /// are excluded from the catalog the same way.
            #[must_use]
            pub fn catalog() -> Vec<Action> {
                [ $( derive_action_core!(@item $variant $( $tag )?) ),* ]
                    .into_iter()
                    .flatten()
                    .collect()
            }
        }
    };
    (@item $variant:ident) => {
        Some(Action::$variant)
    };
    (@item $variant:ident not_pickable) => {
        None
    };
}

for_each_unit_action!(derive_action_core);

impl Action {
    /// The chord whose output must remain down until the originating press
    /// ends, or `None` for an instantaneous action.
    #[must_use]
    pub fn held_combo(&self) -> Option<&KeyCombo> {
        match self {
            Self::HoldShortcut(combo) => Some(combo),
            _ => None,
        }
    }
}
