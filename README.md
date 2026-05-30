<h4 align="right"><strong>English</strong> | <a href="README_CN.md">简体中文</a></h4>

<p align="center">
    <img src="design/icon/openlogi.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ A native, local-first alternative to Logitech Options+, written in Rust 🦀 — remap buttons, DPI, and SmartShift over HID++. No account, no telemetry.</strong></p>


<div align="center">
    <a href="https://twitter.com/AprilNEA" target="_blank">
    <img alt="twitter" src="https://img.shields.io/badge/follow-AprilNEA-green?style=flat-square&logo=Twitter"></a>
    <a href="https://t.me/+pCVJtHAgI3hjYTkx" target="_blank">
    <img alt="telegram" src="https://img.shields.io/badge/chat-telegram-blueviolet?style=flat-square&logo=Telegram"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/releases" target="_blank">
    <img alt="GitHub downloads" src="https://img.shields.io/github/downloads/AprilNEA/OpenLogi/total.svg?style=flat-square"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/commits" target="_blank">
    <img alt="GitHub commit" src="https://img.shields.io/github/commit-activity/m/AprilNEA/OpenLogi?style=flat-square"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/issues?q=is%3Aissue+is%3Aclosed" target="_blank">
    <img alt="GitHub closed issues" src="https://img.shields.io/github/issues-closed/AprilNEA/OpenLogi.svg?style=flat-square"></a>
</div>

> **Options+ ? Try OpenLogi.**

Remap buttons, drive DPI and SmartShift, and switch profiles per app — without a
Logitech account, telemetry, or the official Options+ install. No cloud, plain
TOML config; the only network calls are device-image fetches and an opt-in,
off-by-default update check.

---

## What it is

OpenLogi talks to Logitech HID++ mice over a Logi Bolt receiver — or a
Bluetooth-direct / wired connection — without running Logi Options+. It ships
two binaries:

- **`openlogi-gui`** — a GPUI desktop app: an interactive mouse diagram with
  clickable hotspots, a per-button action picker (37 built-in actions plus
  recorded custom shortcuts), DPI presets, a SmartShift toggle, per-application
  profile overlays, and a device carousel that switches between paired devices
  live.
- **`openlogi`** — a CLI for headless inventory (`list`) plus asset-sync and
  on-device diagnostic subcommands.

Everything is local: bindings live in a plain TOML file, button presses are
remapped through the OS event tap, and DPI / SmartShift changes are written
straight to the device over HID++.

macOS is the supported platform today. Linux and Windows compile (HID
enumeration works), but the OS-level event hook is a stub — see
[Status](#status).

## What it is not

- **Not a headless daemon.** The remapping hook runs inside `openlogi-gui`
  while it's open (optionally launched at login). There is no separate
  background service.
- **Not a cloud or telemetry app.** No account, no telemetry, no auto-download.
  The only outbound traffic is (1) fetching your device's render image from
  `assets.openlogi.org` on first launch — avoidable entirely with a
  bundled-assets build — and (2) an **opt-in** update check, off by default,
  that makes a single HEAD request to the GitHub releases API and never
  downloads anything.
- **Not a drop-in for Options+ — yet.** Scroll-wheel rotation binding,
  gesture-button swipe *hardware* capture, scroll inversion, and Logitech Flow
  are not implemented. Side-button remapping, DPI, SmartShift, and per-app
  profiles are. See [Status](#status).
- **Not affiliated with Logitech.** "Logitech", "MX Master", and "Options+" are
  trademarks of Logitech International S.A.

## Status

Pre-alpha, macOS-first. The workspace builds on Linux and Windows (CI keeps them
green), but the interactive features below require the macOS event tap.

| Capability | State |
|---|---|
| Discover Bolt receivers + list paired devices (CLI + GUI) | ✅ |
| Bluetooth-direct / wired devices (no receiver) | ✅ |
| Battery percentage / charge state | ✅ (online devices) |
| Interactive GUI: carousel, mouse diagram, action picker | ✅ macOS |
| Button remapping via the OS event tap (side Back / Forward today) | ✅ macOS |
| 37-action catalog + recorded custom keyboard shortcuts | ✅ macOS¹ |
| DPI control + presets + Cycle / Set-preset actions (HID++ `0x2201`) | ✅ macOS |
| SmartShift wheel-mode toggle (HID++ `0x2111`) | ✅ macOS |
| Per-application profile overlays (auto-switch on app focus) | ✅ macOS |
| Launch-at-login + opt-in update check | ✅ (TOML only — no settings UI yet) |
| Gesture-button per-direction bindings | 🟡 configurable; hardware capture pending |
| Middle / mode-shift / thumbwheel button capture | 🟡 configurable; hook owns side buttons only |
| Linux / Windows event hook | ❌ stub (`Unsupported`) |
| Unifying receivers | ❌ (not yet in `hidpp 0.2`) |

¹ A few actions (e.g. the media keys) currently log their intended event rather
than posting it — tracked as a follow-up.

## Install

> [!IMPORTANT]
> Quit **Logi Options+** first — the two applications fight over HID++ access and only one can own a given receiver at a time.

Download the signed, notarized `.dmg` from the
[latest release](https://github.com/AprilNEA/OpenLogi/releases/latest) and drag
`OpenLogi.app` to `/Applications`.

Or install via [Homebrew](https://brew.sh):

```sh
brew install --cask aprilnea/tap/openlogi
```

To build from source, see [DEVELOPMENT.md](docs/DEVELOPMENT.md).

## Usage (CLI)

See [USAGE.md](docs/USAGE.md)

## Configuration

See [CONFIGURATION.md](docs/CONFIGURATION.md)

## Developing

See [DEVELOPMENT.md](docs/DEVELOPMENT.md)

## Acknowledgments

- [`hidpp`](https://crates.io/crates/hidpp) by [@lus](https://github.com/lus)
- [Solaar](https://github.com/pwr-Solaar/Solaar)
- [Mouser](https://github.com/TomBadash/Mouser) by Tom Badash

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
