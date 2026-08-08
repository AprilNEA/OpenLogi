//! Shared action-to-icon mapping used by the settings GUI and ring overlay.

use openlogi_core::binding::{Action, ActionRingIcon};

/// Embedded Lucide asset for an action.
pub(crate) fn action_icon_path(action: &Action) -> &'static str {
    match action {
        Action::None => "action-icons/ban.svg",
        Action::LeftClick | Action::RightClick => "action-icons/mouse-pointer-click.svg",
        Action::MiddleClick => "action-icons/mouse.svg",
        Action::MouseBack => "action-icons/circle-arrow-left.svg",
        Action::MouseForward => "action-icons/circle-arrow-right.svg",
        Action::Copy => "action-icons/copy.svg",
        Action::Paste => "action-icons/clipboard-paste.svg",
        Action::Cut => "action-icons/scissors.svg",
        Action::Undo => "action-icons/undo-2.svg",
        Action::Redo => "action-icons/redo-2.svg",
        Action::SelectAll => "action-icons/list-checks.svg",
        Action::Find => "action-icons/search.svg",
        Action::Save => "action-icons/save.svg",
        Action::BrowserBack => "action-icons/arrow-left.svg",
        Action::BrowserForward => "action-icons/arrow-right.svg",
        Action::NewTab => "action-icons/square-plus.svg",
        Action::CloseTab => "action-icons/square-x.svg",
        Action::ReopenTab => "action-icons/rotate-ccw.svg",
        Action::NextTab => "action-icons/chevron-right.svg",
        Action::PrevTab => "action-icons/chevron-left.svg",
        Action::ReloadPage => "action-icons/rotate-cw.svg",
        Action::MissionControl | Action::ShowActionsRing => "action-icons/layout-grid.svg",
        Action::AppExpose => "action-icons/layers.svg",
        Action::PreviousDesktop => "action-icons/square-arrow-left.svg",
        Action::NextDesktop => "action-icons/square-arrow-right.svg",
        Action::ShowDesktop => "action-icons/monitor.svg",
        Action::LaunchpadShow | Action::OpenApplication(_) => "action-icons/grid-3x3.svg",
        Action::LockScreen => "action-icons/lock.svg",
        Action::Screenshot | Action::CaptureRegion => "action-icons/camera.svg",
        Action::PlayPause => "action-icons/play.svg",
        Action::NextTrack => "action-icons/skip-forward.svg",
        Action::PrevTrack => "action-icons/skip-back.svg",
        Action::VolumeUp => "action-icons/volume-2.svg",
        Action::VolumeDown => "action-icons/volume-1.svg",
        Action::MuteVolume => "action-icons/volume-x.svg",
        Action::CycleDpiPresets | Action::SetDpiPreset(_) => "action-icons/gauge.svg",
        Action::ToggleSmartShift => "action-icons/refresh-cw.svg",
        Action::ScrollUp => "action-icons/chevrons-up.svg",
        Action::ScrollDown => "action-icons/chevrons-down.svg",
        Action::HorizontalScrollLeft => "action-icons/chevrons-left.svg",
        Action::HorizontalScrollRight => "action-icons/chevrons-right.svg",
        Action::CustomShortcut(_) => "action-icons/keyboard.svg",
    }
}

/// Embedded icon path for a user-selected Actions Ring glyph.
pub(crate) fn ring_icon_path(icon: ActionRingIcon) -> &'static str {
    match icon {
        ActionRingIcon::Pointer => "action-icons/mouse-pointer-click.svg",
        ActionRingIcon::Mouse => "action-icons/mouse.svg",
        ActionRingIcon::Copy => "action-icons/copy.svg",
        ActionRingIcon::Paste => "action-icons/clipboard-paste.svg",
        ActionRingIcon::Cut => "action-icons/scissors.svg",
        ActionRingIcon::Search => "action-icons/search.svg",
        ActionRingIcon::Save => "action-icons/save.svg",
        ActionRingIcon::Keyboard => "action-icons/keyboard.svg",
        ActionRingIcon::Applications => "action-icons/grid-3x3.svg",
        ActionRingIcon::Grid => "action-icons/layout-grid.svg",
        ActionRingIcon::Layers => "action-icons/layers.svg",
        ActionRingIcon::Monitor => "action-icons/monitor.svg",
        ActionRingIcon::Lock => "action-icons/lock.svg",
        ActionRingIcon::Camera => "action-icons/camera.svg",
        ActionRingIcon::Play => "action-icons/play.svg",
        ActionRingIcon::Volume => "action-icons/volume-2.svg",
        ActionRingIcon::Gauge => "action-icons/gauge.svg",
        ActionRingIcon::Refresh => "action-icons/refresh-cw.svg",
        ActionRingIcon::ArrowUp => "action-icons/chevrons-up.svg",
        ActionRingIcon::ArrowDown => "action-icons/chevrons-down.svg",
        ActionRingIcon::ArrowLeft => "action-icons/arrow-left.svg",
        ActionRingIcon::ArrowRight => "action-icons/arrow-right.svg",
        ActionRingIcon::Undo => "action-icons/undo-2.svg",
        ActionRingIcon::Redo => "action-icons/redo-2.svg",
        ActionRingIcon::SelectAll => "action-icons/list-checks.svg",
        ActionRingIcon::MouseBack => "action-icons/circle-arrow-left.svg",
        ActionRingIcon::MouseForward => "action-icons/circle-arrow-right.svg",
        ActionRingIcon::NewTab => "action-icons/square-plus.svg",
        ActionRingIcon::CloseTab => "action-icons/square-x.svg",
        ActionRingIcon::ReopenTab => "action-icons/rotate-ccw.svg",
        ActionRingIcon::NextTab => "action-icons/chevron-right.svg",
        ActionRingIcon::PreviousTab => "action-icons/chevron-left.svg",
        ActionRingIcon::Reload => "action-icons/rotate-cw.svg",
        ActionRingIcon::PreviousDesktop => "action-icons/square-arrow-left.svg",
        ActionRingIcon::NextDesktop => "action-icons/square-arrow-right.svg",
        ActionRingIcon::PreviousTrack => "action-icons/skip-back.svg",
        ActionRingIcon::NextTrack => "action-icons/skip-forward.svg",
        ActionRingIcon::VolumeDown => "action-icons/volume-1.svg",
        ActionRingIcon::Mute => "action-icons/volume-x.svg",
        ActionRingIcon::ScrollLeft => "action-icons/chevrons-left.svg",
        ActionRingIcon::ScrollRight => "action-icons/chevrons-right.svg",
        ActionRingIcon::Folder => "action-icons/folder.svg",
        ActionRingIcon::File => "action-icons/file.svg",
        ActionRingIcon::Globe => "action-icons/globe.svg",
        ActionRingIcon::Terminal => "action-icons/square-terminal.svg",
        ActionRingIcon::Settings => "action-icons/settings.svg",
        ActionRingIcon::Star => "action-icons/star.svg",
        ActionRingIcon::Heart => "action-icons/heart.svg",
        ActionRingIcon::Calendar => "action-icons/calendar.svg",
        ActionRingIcon::Bell => "action-icons/bell.svg",
        ActionRingIcon::User => "action-icons/user.svg",
        ActionRingIcon::Palette => "action-icons/palette.svg",
        ActionRingIcon::Book => "action-icons/book-open.svg",
    }
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource as _;

    use super::*;
    use crate::app_assets::AppAssets;

    #[test]
    fn every_ring_gallery_icon_is_embedded() {
        for icon in ActionRingIcon::ALL {
            let loaded = AppAssets.load(ring_icon_path(icon));
            assert!(
                matches!(loaded, Ok(Some(_))),
                "missing embedded asset for {icon:?}"
            );
        }
    }
}
