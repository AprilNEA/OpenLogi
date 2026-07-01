> [!WARNING]
> **OpenLogi is under active development** and not yet stable — features and config may still change. Give the repo a **Star** ⭐ and **Watch** 👀 it to get notified the moment a release lands.

<h4 align="right"><strong>English</strong> | <a href="docs/README.zh-CN.md">简体中文</a> | <a href="docs/README.ja.md">日本語</a> | <a href="docs/README.de.md">Deutsch</a> | <a href="docs/README.fr.md">Français</a> | <a href="docs/README.ko.md">한국어</a></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ A native, local-first alternative to Logitech Options+, written in Rust 🦀<br/>Remap buttons, DPI, and SmartShift over HID++. No account, no telemetry.</strong></p>


<div align="center">
    <a href="https://twitter.com/AprilNEA" target="_blank">
    <img alt="twitter" src="https://img.shields.io/badge/follow-AprilNEA-green?style=social&logo=Twitter"></a>
    <a href="https://t.me/+VDtkR5OSAT04NzVh" target="_blank">
    <img alt="telegram" src="https://img.shields.io/badge/chat-telegram-blueviolet?style=flat&logo=Telegram"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/releases" target="_blank">
    <img alt="GitHub downloads" src="https://img.shields.io/github/downloads/AprilNEA/OpenLogi/total.svg?style=flat"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/commits" target="_blank">
    <img alt="GitHub commit" src="https://img.shields.io/github/commit-activity/m/AprilNEA/OpenLogi?style=flat"></a>
    <img alt="Hits" src="https://hits.aprilnea.com/hits?url=https://github.com/aprilnea/openlogi">
</div>

<p align="center">
    <a href="https://trendshift.io/repositories/42303" target="_blank">
    <img src="https://trendshift.io/api/badge/trendshift/repositories/42303/daily?language=Rust" alt="AprilNEA%2FOpenLogi | Trendshift" width="250" height="55"/></a>
</p>

> **Options+ ? Try OpenLogi.**

Remap buttons, drive DPI and SmartShift, and switch profiles per app — without a Logitech account, telemetry, or the official Options+ install. No cloud, plain TOML config; the only network calls are device-image fetches and an opt-in, off-by-default update check.

---

## What it is

OpenLogi talks to Logitech HID++ mice over a Logi Bolt receiver — or a
Bluetooth-direct / wired connection — without running Logi Options+. It ships
two binaries:

- **[OpenLogi GUI](crates/openlogi-gui)** — a GPUI desktop app: an interactive mouse diagram with clickable hotspots, a per-button action picker (41 built-in actions plus custom keyboard shortcuts authored in the TOML config), DPI presets, a SmartShift panel (wheel mode, sensitivity, permanent ratchet), per-application profile overlays, a device carousel that switches between paired devices live, and a Settings window with a UI localized into 20 languages.
- **[OpenLogi CLI](crates/openlogi-cli)** — a CLI for headless inventory (`list`) plus asset-sync and on-device diagnostic subcommands.

Everything is local: bindings live in a plain TOML file, button presses are remapped through the OS event tap, and DPI / SmartShift changes are written straight to the device over HID++.

macOS and Linux are supported. Windows is an early, untested preview — signed
builds ship with each release; see [Roadmap](#roadmap).

## Beyond Options+

Things OpenLogi does that Options+ won't:

- **Run on Linux.** Options+ ships for macOS and Windows only. OpenLogi treats
  Linux as a first-class platform: evdev/uinput hook, udev rules, a systemd
  user unit, and `.deb` / `.rpm` packages.
- **Move the Gesture Button.** Pick which physical button owns the gesture
  role — the dedicated Gesture Button, middle, back, or forward — with per-direction swipe
  bindings, or turn gestures off entirely. Options+ pins the gesture role to
  the dedicated Gesture Button.
- **Keep config in plain text.** Everything is one TOML file you can read,
  diff, version-control, and copy between machines.
- **Script it.** A real CLI: device inventory, asset prefetch, and on-device
  HID++ diagnostics (feature dump, DPI / SmartShift round-trips).
- **Stay light.** Native Rust + GPUI binaries — no Electron suite, no resident
  updaters, no account, no telemetry.

## Roadmap

| Capability | State |
|---|---|
| Discover Bolt receivers + list paired devices (CLI + GUI) | ✅ |
| Unifying receivers (older protocol, replaced by Bolt) | ✅ |
| Bluetooth-direct / wired devices (no receiver) | ✅ |
| Battery percentage / charge state | ✅ (online devices) |
| Interactive GUI: carousel, mouse diagram, action picker | ✅ macOS + Linux |
| Button remapping via the OS event tap / evdev hook | ✅ macOS + Linux |
| 41-action catalog + custom keyboard shortcuts (TOML-authored) | ✅ macOS + Linux¹ |
| DPI control + presets + Cycle / Set-preset actions (HID++ `0x2201`) | ✅ |
| SmartShift wheel: mode toggle + sensitivity + permanent-ratchet panel (HID++ `0x2111`) | ✅ |
| Per-application profile overlays (auto-switch on app focus) | ✅ macOS, 🟡 Linux (X11 only) |
| Settings window: launch-at-login, update check, menu-bar, permissions, language | ✅ macOS + Linux |
| Interface localization (20 languages: da, de, el, en, es, fi, fr, it, ja, ko, nb, nl, pl, pt-BR, pt-PT, ru, sv, zh-CN, zh-HK, zh-TW) | ✅ |
| Linux packaging: udev rules, systemd unit, `.deb` / `.rpm` | ✅ Linux |
| Gesture-button per-direction bindings | 🟡 configurable; hardware capture pending |
| Middle / mode-shift / thumbwheel tap + rotation capture | 🟡 configurable; hook owns side buttons only |
| Windows (agent, GUI, event hook) | 🟡 untested preview — signed `.exe` / `.msi` ship per release |

¹ Media key actions use D-Bus MPRIS on Linux; a handful of macOS-specific actions (e.g. Launchpad) have no Linux equivalent and are no-ops.

## Install

> [!IMPORTANT]
> Quit **Logi Options+** first — the two applications fight over HID++ access and only one can own a given receiver at a time.

### macOS

Download the signed, notarized `.dmg` from the [latest release](https://github.com/AprilNEA/OpenLogi/releases/latest) and drag `OpenLogi.app` to `/Applications`.

Or install via [Homebrew](https://brew.sh):

```sh
brew install --cask openlogi
```

The official Homebrew cask is the default installation path. To explicitly
track the latest GitHub release from `aprilnea/tap` instead:

```sh
brew tap aprilnea/tap
brew install --cask aprilnea/tap/openlogi@latest
```

`openlogi@latest` is maintained by OpenLogi's release workflow and may update
before the official cask autobump lands. Install either `openlogi` or
`openlogi@latest`, not both.

### Linux

Download the `.deb` or `.rpm` from the [latest release](https://github.com/AprilNEA/OpenLogi/releases/latest):

```sh
# Debian / Ubuntu
sudo dpkg -i openlogi_*.deb

# Fedora / RHEL
sudo rpm -i openlogi-*.rpm
```

Packages are published for both `x86_64`/`amd64` and `arm64`/`aarch64`.

The package installs udev rules that grant your user access to
`/dev/hidraw*` and `/dev/uinput` without `sudo`. After installation,
enable the background agent for your user:

```sh
systemctl --user enable --now openlogi-agent.service
```

See [docs/INSTALL-linux.md](docs/INSTALL-linux.md) for manual / source installs
and distros without systemd.

### Windows

Signed portable `.zip` archives and per-user `.msi` installers (x86_64 and
arm64) are attached to each release. Both ship the GUI (`OpenLogi.exe`)
together with the background agent (`openlogi-agent.exe`), which owns all
device I/O — keep the two files side by side when using the portable zip, or
the GUI has nothing to connect to.

Windows support works and has been validated end-to-end on Windows 11 with
real hardware — a wired keyboard and a Unifying-receiver mouse, including
install, in-place upgrade, and uninstall of the MSI. It is newer than the
macOS build, so if you hit a rough edge please
[report it](https://github.com/AprilNEA/OpenLogi/issues). The agent shows a
system-tray icon (Show Main Window / Quit) so the app stays reachable after
the main window is closed; disable it with the same "show in menu bar"
setting macOS uses.

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

### Logo & brand assets

The OpenLogi logo and app icon — the brand assets under [`design/`](design/) —
are © 2026 AprilNEA, all rights reserved, and are not covered by the MIT/Apache
licenses above; see [`design/LICENSE`](design/LICENSE). Forking the code grants
no right to the OpenLogi name, logo, or icon; please don't use them to represent
your own projects, forks, or distributions without prior written permission.

---

**Not affiliated with Logitech.** "Logitech", "MX Master", and "Options+" are trademarks of Logitech International S.A.

## Repo activity

![Repobeats analytics image](https://repobeats.axiom.co/api/embed/4a0b576a03e9d528ad31ccf4797a1286c045d021.svg "Repobeats analytics image")
