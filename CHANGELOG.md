# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.2](https://github.com/AprilNEA/OpenLogi/compare/v0.6.1...v0.6.2) - 2026-06-08

### Added

- *(i18n)* integrate Crowdin localization workflow ([#174](https://github.com/AprilNEA/OpenLogi/pull/174))

### Other

- switch release notes generation to Codex ([#177](https://github.com/AprilNEA/OpenLogi/pull/177))
- add code of conduct

## [0.6.1](https://github.com/AprilNEA/OpenLogi/compare/openlogi-cli-v0.6.0...openlogi-cli-v0.6.1) - 2026-06-08

### Fixed

- *(cli)* diag selects a device that exposes the feature under test ([#150](https://github.com/AprilNEA/OpenLogi/pull/150))

## [0.6.0](https://github.com/AprilNEA/OpenLogi/compare/openlogi-core-v0.5.3...openlogi-core-v0.6.0) - 2026-06-07

### Added

- *(agent)* tarpc IPC server backed by the orchestrator + device I/O
- *(agent)* define tarpc IPC service contract + serde-derive wire types

### Fixed

- *(agent)* give the agent its own single-instance lock

### Other

- Merge origin/master into feat/agent-daemon-split

## [0.5.3](https://github.com/AprilNEA/OpenLogi/compare/openlogi-core-v0.5.2...openlogi-core-v0.5.3) - 2026-06-06

### Fixed

- *(gui)* prefer asset-registry kind + harden device-kind classification

### Other

- gate config panels on HID++ capabilities, not device kind

## [0.5.2](https://github.com/AprilNEA/OpenLogi/compare/openlogi-core-v0.5.1...openlogi-core-v0.5.2) - 2026-06-05

### Added

- *(core)* LockScreen and media actions via D-Bus on Linux
- *(core)* expose action_device_path for evtest attachment
- *(core)* implement Action::execute on Linux via uinput
- enable Thumb Wheel Up/Down mapping, "Do Nothing" action, and native scroll sensitivity ([#125](https://github.com/AprilNEA/OpenLogi/pull/125))

### Fixed

- *(core)* fmt + clarify mpris fallback log on the Linux D-Bus code
- *(core)* address PR #124 review comments
- *(core)* drop unused REL_X/REL_Y from the action uinput device
- *(core)* cover Action::None in execute_linux
- *(core)* address PR review comments
- *(core)* use enumerate_dev_nodes_blocking for correct event path
- *(core)* address code review findings

### Other

- run clippy on Windows instead of bare cargo check ([#146](https://github.com/AprilNEA/OpenLogi/pull/146))
- *(core)* simplify D-Bus helpers and add -v flag to inject_action
- *(core)* simplify inject_action parsing, guard --delay
- *(core)* extract KEY_CAPABILITIES const, drop too_many_lines allow
- *(core)* note LockScreen Linux limitation and D-Bus follow-up
- *(core)* note Ctrl+Shift+Z vs Ctrl+Y redo shortcut choice on Linux
- *(core)* clarify scroll unit difference between post_horizontal_scroll and HorizontalScroll* actions
- *(core)* simplify Linux execute helpers and doc fixes
- *(core)* add vk_mapping tests and inject_action example

## [0.5.1](https://github.com/AprilNEA/OpenLogi/compare/openlogi-assets-v0.5.0...openlogi-assets-v0.5.1) - 2026-06-05

### Fixed

- *(assets)* match devices against every model id a depot lists

### Other

- *(assets)* lock the index.json modelIds schema contract

## [0.5.0](https://github.com/AprilNEA/OpenLogi/compare/openlogi-core-v0.4.1...openlogi-core-v0.5.0) - 2026-06-05

### Added

- add wired G-series keyboard RGB control ([#29](https://github.com/AprilNEA/OpenLogi/pull/29))

## [0.4.1](https://github.com/AprilNEA/OpenLogi/compare/openlogi-v0.4.0...openlogi-v0.4.1) - 2026-06-03

### Added

- *(gui)* refine device gallery worktree changes
- *(nix)* wire passthru.updateScript for nix-update / autobump
- *(nix)* add nixpkgs package + flake; commit the prebuilt app icon

### Other

- route issue-chooser questions to GitHub Discussions
- update Telegram invite link to the new channel
- *(release)* disable homebrew-tap dispatch (openlogi moved to homebrew-cask) ([#105](https://github.com/AprilNEA/OpenLogi/pull/105))
- add GitHub issue form templates ([#102](https://github.com/AprilNEA/OpenLogi/pull/102))
- configure release-plz branch prefix

## [0.4.0](https://github.com/AprilNEA/OpenLogi/compare/openlogi-core-v0.3.4...openlogi-core-v0.4.0) - 2026-06-02

### Added

- *(i18n)* add zh-TW (Traditional Chinese, Taiwan) locale ([#57](https://github.com/AprilNEA/OpenLogi/pull/57))

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
