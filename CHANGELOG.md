# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.4](https://github.com/AprilNEA/OpenLogi/compare/openlogi-hidpp-v0.3.3...openlogi-hidpp-v0.3.4) - 2026-06-01

### Added

- *(openlogi-hidpp)* vendor the hidpp 0.3 fork from lus/logy

### Fixed

- address /code-review findings (write timeouts, scanning fallback, asset sync, CoreBluetooth safety)

### Other

- *(hidpp)* up-convert short→long inside the channel for long-only BLE

## [0.3.3](https://github.com/AprilNEA/OpenLogi/compare/openlogi-assets-v0.3.2...openlogi-assets-v0.3.3) - 2026-06-01

### Fixed

- *(assets)* match devices by displayName when no PID lookup hits

## [0.3.2](https://github.com/AprilNEA/OpenLogi/compare/v0.3.1...v0.3.2) - 2026-06-01

### Other

- simplify format

## [0.3.1](https://github.com/AprilNEA/OpenLogi/compare/v0.3.0...v0.3.1) - 2026-06-01

### Added

- *(updater)* use static R2 manifest ([#43](https://github.com/AprilNEA/OpenLogi/pull/43))

## [0.3.0](https://github.com/AprilNEA/OpenLogi/compare/openlogi-core-v0.2.0...openlogi-core-v0.3.0) - 2026-06-01

### Added

- *(openlogi-gui)* add Russian localization and language select ([#38](https://github.com/AprilNEA/OpenLogi/pull/38))

### Fixed

- *(gui)* stabilize device tab ordering ([#37](https://github.com/AprilNEA/OpenLogi/pull/37))

## [0.2.0](https://github.com/AprilNEA/OpenLogi/compare/openlogi-hid-v0.1.4...openlogi-hid-v0.2.0) - 2026-05-31

### Added

- *(openlogi-hid)* route HID++ writes to directly-attached devices ([#5](https://github.com/AprilNEA/OpenLogi/pull/5))

## [0.1.4](https://github.com/AprilNEA/OpenLogi/compare/v0.1.3...v0.1.4) - 2026-05-31

### Other

- update workflow actions for Node 24
- *(release-plz)* fail loudly when a release silently stalls

## [0.1.3](https://github.com/AprilNEA/OpenLogi/compare/v0.1.2...v0.1.3) - 2026-05-31

### Added

- macOS menu-bar (tray) app: lives in the menu bar with the interactive mouse diagram, a mappable gesture-button hotspot, and live Open / Quit
- Dynamic Dock + menu-bar presence — full window with the app menu when open, tray-only once the window is closed; optional silent start-minimized on login
- "Show in menu bar" setting to keep OpenLogi in the menu bar, or run it as an ordinary Dock app instead
- ⌘W closes the focused window

### Fixed

- Use the real Xcode toolchain for GUI builds and build the installer DMG correctly

## [0.1.2](https://github.com/AprilNEA/OpenLogi/compare/v0.1.1...v0.1.2) - 2026-05-31

### Added

- Check for Updates in the About window, backed by the gpui-updater crate
- One opt-in update check on launch, with a first-run prompt to enable it
- Live download progress, and a clickable version that links to its GitHub release

## [0.1.1](https://github.com/AprilNEA/OpenLogi/compare/v0.1.0...v0.1.1) - 2026-05-30

### Other

- *(release-plz)* write a single root changelog, not one per crate
- *(release-plz)* load CARGO_REGISTRY_TOKEN from 1Password
