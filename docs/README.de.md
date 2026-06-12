> [!WARNING]
> **OpenLogi befindet sich in aktiver Entwicklung** und ist noch nicht stabil — Funktionen und Konfiguration können sich noch ändern. Gib dem Repo einen **Star** ⭐ und **beobachte** 👀 es, um sofort benachrichtigt zu werden, sobald ein Release erscheint.

<h4 align="right"><a href="../README.md">English</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.ja.md">日本語</a> | <strong>Deutsch</strong> | <a href="README.fr.md">Français</a> | <a href="README.ko.md">한국어</a></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Eine native, local-first Alternative zu Logitech Options+, geschrieben in Rust 🦀<br/>Tasten, DPI und SmartShift über HID++ neu belegen. Kein Konto, keine Telemetrie.</strong></p>


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

> **Genug von Options+? Probier OpenLogi.**

Tasten neu belegen, DPI und SmartShift steuern, Profile pro App umschalten — ohne Logitech-Konto, ohne Telemetrie, ohne das offizielle Options+. Keine Cloud, Konfiguration als einfaches TOML; die einzigen Netzwerkzugriffe sind Geräte-Bilder und eine standardmäßig deaktivierte Opt-in-Updateprüfung.

---

## Was es ist

OpenLogi spricht mit Logitech-HID++-Mäusen über einen Logi-Bolt-Empfänger — oder per Bluetooth-Direktverbindung / Kabel — ganz ohne Logi Options+. Es liefert zwei Programme:

- **[OpenLogi GUI](../crates/openlogi-gui)** — eine GPUI-Desktop-App: ein interaktives Mausdiagramm mit klickbaren Hotspots, ein Aktions-Picker pro Taste (41 eingebaute Aktionen plus eigene Tastenkürzel, von Hand in der TOML-Konfiguration angelegt), DPI-Voreinstellungen, ein SmartShift-Panel (Radmodus, Empfindlichkeit, permanente Rasterung), Profil-Overlays pro Anwendung, ein Geräte-Karussell, das live zwischen gekoppelten Geräten wechselt, und ein Einstellungsfenster mit einer in 20 Sprachen lokalisierten Oberfläche.
- **[OpenLogi CLI](../crates/openlogi-cli)** — ein Kommandozeilenwerkzeug für headless Inventar (`list`) sowie Asset-Sync- und Geräte-Diagnose-Unterbefehle.

Alles bleibt lokal: Belegungen liegen in einer einfachen TOML-Datei, Tastendrücke werden über den OS-Event-Hook umgeleitet, und DPI-/SmartShift-Änderungen werden per HID++ direkt aufs Gerät geschrieben.

macOS und Linux werden unterstützt. Windows ist eine frühe, ungetestete Vorschau — signierte Builds liegen jedem Release bei; siehe [Roadmap](#roadmap).

## Mehr als Options+

Was OpenLogi kann und Options+ nicht:

- **Auf Linux laufen.** Options+ gibt es nur für macOS und Windows. OpenLogi behandelt Linux als vollwertige Plattform: evdev/uinput-Hook, udev-Regeln, eine systemd-User-Unit und `.deb`-/`.rpm`-Pakete.
- **Die Gestentaste verschieben.** Wähle, welche physische Taste die Gestenrolle übernimmt — Daumenfläche, Mitteltaste, Zurück oder Vor — mit Wischbelegungen pro Richtung, oder schalte Gesten ganz ab. Options+ nagelt die Gestenrolle auf die dedizierte Daumenfläche fest.
- **Konfiguration im Klartext.** Alles steckt in einer TOML-Datei, die du lesen, diffen, versionieren und zwischen Rechnern kopieren kannst.
- **Skriptbar.** Eine echte CLI: Geräteinventar, Asset-Prefetch und HID++-Diagnosen am Gerät (Feature-Dump, DPI-/SmartShift-Roundtrips).
- **Leichtgewichtig bleiben.** Native Rust-+-GPUI-Binaries — keine Electron-Suite, keine residenten Updater, kein Konto, keine Telemetrie.

## Roadmap

| Fähigkeit | Status |
|---|---|
| Bolt-Empfänger finden + gekoppelte Geräte auflisten (CLI + GUI) | ✅ |
| Unifying-Empfänger (älteres Protokoll, von Bolt abgelöst) | ✅ |
| Bluetooth-Direkt- / Kabelgeräte (ohne Empfänger) | ✅ |
| Akkustand / Ladezustand | ✅ (Geräte online) |
| Interaktive GUI: Karussell, Mausdiagramm, Aktions-Picker | ✅ macOS + Linux |
| Tastenumbelegung über OS-Event-Hook / evdev | ✅ macOS + Linux |
| Katalog mit 41 Aktionen + eigene Tastenkürzel (TOML-handgepflegt) | ✅ macOS + Linux¹ |
| DPI-Steuerung + Voreinstellungen + Cycle-/Set-Preset-Aktionen (HID++ `0x2201`) | ✅ |
| SmartShift-Rad: Modus + Empfindlichkeit + permanente Rasterung (HID++ `0x2111`) | ✅ |
| Profil-Overlays pro Anwendung (Auto-Wechsel bei App-Fokus) | ✅ macOS, 🟡 Linux (nur X11) |
| Einstellungsfenster: Autostart, Updateprüfung, Menüleiste, Berechtigungen, Sprache | ✅ macOS + Linux |
| Lokalisierte Oberfläche (20 Sprachen: da, de, el, en, es, fi, fr, it, ja, ko, nb, nl, pl, pt-BR, pt-PT, ru, sv, zh-CN, zh-HK, zh-TW) | ✅ |
| Linux-Paketierung: udev-Regeln, systemd-Unit, `.deb` / `.rpm` | ✅ Linux |
| Gestentaste: Belegungen pro Richtung | 🟡 konfigurierbar; Hardware-Erfassung in Arbeit |
| Erfassung von Mittel-/Mode-Shift-/Daumenrad-Taste | 🟡 konfigurierbar; der Hook übernimmt bislang nur die Seitentasten |
| Windows (Agent, GUI, Event-Hook) | 🟡 ungetestete Vorschau — signierte `.exe` / `.msi` liegen jedem Release bei |

¹ Medientasten-Aktionen nutzen unter Linux D-Bus MPRIS; einige macOS-spezifische Aktionen (z. B. Launchpad) haben unter Linux kein Gegenstück und sind No-ops.

## Installation

> [!IMPORTANT]
> Beende zuerst **Logi Options+** — die beiden Anwendungen streiten sich um den HID++-Zugriff, und ein Empfänger kann immer nur einem gehören.

### macOS

Lade das signierte, notarisierte `.dmg` vom [neuesten Release](https://github.com/AprilNEA/OpenLogi/releases/latest) und ziehe `OpenLogi.app` nach `/Applications`.

Oder per [Homebrew](https://brew.sh):

```sh
brew install --cask openlogi
```

Der offizielle Homebrew-Cask ist der Standardweg. Um stattdessen explizit das neueste GitHub-Release über `aprilnea/tap` zu verfolgen:

```sh
brew tap aprilnea/tap
brew install --cask aprilnea/tap/openlogi@latest
```

`openlogi@latest` wird vom Release-Workflow von OpenLogi gepflegt und kann aktualisiert sein, bevor der Autobump des offiziellen Casks greift. Installiere entweder `openlogi` oder `openlogi@latest`, nicht beide.

### Linux

Lade das `.deb` oder `.rpm` vom [neuesten Release](https://github.com/AprilNEA/OpenLogi/releases/latest):

```sh
# Debian / Ubuntu
sudo dpkg -i openlogi_*.deb

# Fedora / RHEL
sudo rpm -i openlogi-*.rpm
```

Pakete erscheinen für `x86_64`/`amd64` und `arm64`/`aarch64`.

Das Paket installiert udev-Regeln, die deinem Benutzer Zugriff auf `/dev/hidraw*` und `/dev/uinput` ohne `sudo` geben. Aktiviere nach der Installation den Hintergrund-Agent für deinen Benutzer:

```sh
systemctl --user enable --now openlogi-agent.service
```

Für manuelle / Quellcode-Installationen und Distributionen ohne systemd siehe [INSTALL-linux.md](INSTALL-linux.md).

### Windows (Vorschau)

Jedem Release liegen signierte `.exe`- und Per-User-`.msi`-Installer (x86_64 und arm64) bei. Die Windows-Unterstützung ist eine frühe Vorschau, die auf echter Hardware noch nicht breit getestet wurde — rechne mit Ecken und Kanten und [melde Probleme](https://github.com/AprilNEA/OpenLogi/issues).

Zum Bauen aus dem Quellcode siehe [DEVELOPMENT.md](DEVELOPMENT.md).


## Verwendung (CLI)

Siehe [USAGE.md](USAGE.md)

## Konfiguration

Siehe [CONFIGURATION.md](CONFIGURATION.md)

## Entwicklung

Siehe [DEVELOPMENT.md](DEVELOPMENT.md)

## Danksagungen

- [`hidpp`](https://crates.io/crates/hidpp) von [@lus](https://github.com/lus)
- [Solaar](https://github.com/pwr-Solaar/Solaar)
- [Mouser](https://github.com/TomBadash/Mouser) von Tom Badash

## Lizenz

Doppelt lizenziert, wahlweise unter

- Apache License, Version 2.0 ([LICENSE-APACHE](../LICENSE-APACHE))
- MIT-Lizenz ([LICENSE-MIT](../LICENSE-MIT))

### Logo & Markenressourcen

Das OpenLogi-Logo und das App-Icon — die Markenressourcen unter [`design/`](../design/) — sind © 2026 AprilNEA, alle Rechte vorbehalten, und fallen nicht unter die obigen MIT-/Apache-Lizenzen; siehe [`design/LICENSE`](../design/LICENSE). Ein Fork des Codes gewährt kein Recht am Namen, Logo oder Icon von OpenLogi; bitte verwende sie nicht ohne vorherige schriftliche Erlaubnis für eigene Projekte, Forks oder Distributionen.

---

**Nicht mit Logitech verbunden.** „Logitech", „MX Master" und „Options+" sind Marken der Logitech International S.A.
