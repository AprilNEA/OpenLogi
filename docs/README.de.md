> [!WARNING]
> **OpenLogi befindet sich in aktiver Entwicklung** und ist noch nicht stabil — Funktionen und Konfiguration können sich noch ändern. Gib dem Repo einen **Star** ⭐ und **beobachte** 👀 es, um benachrichtigt zu werden, wenn ein neues Release erscheint.

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

<p align="center">
    <a href="https://trendshift.io/repositories/42303" target="_blank">
    <img src="https://trendshift.io/api/badge/trendshift/repositories/42303/daily?language=Rust" alt="AprilNEA%2FOpenLogi | Trendshift" width="250" height="55"/></a>
</p>

> **Genug von Options+? Probier OpenLogi.**

Tasten neu belegen, DPI und SmartShift steuern, Profile pro App umschalten — ohne Logitech-Konto, ohne Telemetrie, ohne das offizielle Options+. Keine Cloud, Konfiguration als einfaches TOML. Standardmäßig verbindet sich die App nur zum Abruf von Gerätebildern automatisch; Updateprüfungen und Downloads laufen nur auf Anfrage oder nach Opt-in.

---

## Was es ist

OpenLogi spricht mit Logitech-HID++-Peripheriegeräten über Logi-Bolt- und Unifying-Empfänger, Bluetooth-Direktverbindungen oder USB-Kabel — ganz ohne Logi Options+. Es besteht aus drei Komponenten:

- **[OpenLogi GUI](../crates/openlogi-gui)** — eine GPUI-Desktop-App: interaktives Mausdiagramm mit klickbaren Hotspots, Aktions-Picker pro Taste (eingebaute Aktionen plus eigene Tastenkürzel aus der TOML-Konfiguration), DPI-Voreinstellungen, SmartShift, native Scroll-Umkehr pro Gerät, RGB-Tastaturbeleuchtung, Profile pro Anwendung, Live-Gerätekarussell und ein in 20 Sprachen lokalisiertes Einstellungsfenster.
- **[OpenLogi agent](../crates/openlogi-agent)** — der Hintergrunddienst, dem der Input-Hook und sämtliche Geräte-I/O gehören. Die GUI ist ein reiner IPC-Client und startet den Agent bei Bedarf.
- **[OpenLogi CLI](../crates/openlogi-cli)** — ein Kommandozeilenwerkzeug für headless Inventar (`list`) sowie Asset-Sync- und Geräte-Diagnose-Unterbefehle.

Alles bleibt lokal: Belegungen liegen in einer einfachen TOML-Datei, der Agent leitet Tastendrücke über den OS-Input-Hook um und schreibt DPI-, SmartShift-, Scroll- und Beleuchtungsänderungen per HID++ direkt aufs Gerät.

macOS, Linux und Windows werden unterstützt. Windows ist der neueste Port: Er wurde auf Windows 11-Hardware vollständig validiert, kann aber noch mehr Ecken und Kanten als die macOS- und Linux-Builds haben; siehe [Roadmap](#roadmap).

## Mehr als Options+

Was OpenLogi kann und Options+ nicht:

- **Auf Linux laufen.** Options+ gibt es nur für macOS und Windows. OpenLogi behandelt Linux als vollwertige Plattform: evdev/uinput-Hook, udev-Regeln, eine systemd-User-Unit und `.deb`-/`.rpm`-/`.pkg.tar.zst`-Pakete.
- **Die Gestentaste verschieben.** Wähle, welche physische Taste die Gestenrolle übernimmt — dedizierte Gestentaste, Mitteltaste, Zurück oder Vor — mit Wischbelegungen pro Richtung, oder schalte Gesten ganz ab. Options+ nagelt die Gestenrolle auf die dedizierte Gestentaste fest.
- **Konfiguration im Klartext.** Alles steckt in einer TOML-Datei, die du lesen, diffen, versionieren und zwischen Rechnern kopieren kannst.
- **Skriptbar.** Eine echte CLI: Geräteinventar, Asset-Prefetch und HID++-Diagnosen am Gerät (Feature-/Control-Dumps, DPI-/SmartShift-Roundtrips und Prüfungen der Tastaturbeleuchtung).
- **Leichtgewichtig bleiben.** Native Rust-+-GPUI-Binaries — keine Electron-Suite, keine residenten Updater, kein Konto, keine Telemetrie.

## Roadmap

| Fähigkeit | Status |
|---|---|
| Bolt-Empfänger finden + gekoppelte Geräte auflisten (CLI + GUI) | ✅ |
| Unifying-Empfänger (älteres Protokoll, von Bolt abgelöst) | ✅ |
| Bluetooth-Direkt- / Kabelgeräte (ohne Empfänger) | ✅ |
| Akkustand / Ladezustand | ✅ (Geräte online) |
| Interaktive GUI: Karussell, Mausdiagramm, Aktions-Picker | ✅ macOS + Linux + Windows |
| Tastenumbelegung über den OS-Input-Hook | ✅ macOS + Linux + Windows |
| Katalog eingebauter Aktionen + eigene Tastenkürzel (in TOML angelegt) | ✅ macOS + Linux + Windows¹ |
| DPI-Steuerung + Voreinstellungen + Cycle-/Set-Preset-Aktionen (HID++ `0x2201`) | ✅ |
| SmartShift-Rad: Modus + Empfindlichkeit + permanente Rasterung (HID++ `0x2111`) | ✅ |
| Native Scroll-Umkehr pro Gerät (HID++ `0x2121`) | ✅ (unterstützte Geräte) |
| Statische RGB-Tastaturbeleuchtung (HID++ `0x8070` / `0x8080`) | ✅ (unterstützte Geräte) |
| Profil-Overlays pro Anwendung (Auto-Wechsel bei App-Fokus) | ✅ macOS + Windows, 🟡 Linux (nur X11 / XWayland) |
| Einstellungsfenster: Autostart, Updates, Berechtigungen, Sprache, Erscheinungsbild | ✅ macOS + Linux + Windows |
| Agent-Statussymbol | ✅ macOS-Menüleiste + Windows-Infobereich; unter Linux nicht anwendbar |
| Lokalisierte Oberfläche (20 Sprachen: da, de, el, en, es, fi, fr, it, ja, ko, nb, nl, pl, pt-BR, pt-PT, ru, sv, zh-CN, zh-HK, zh-TW) | ✅ |
| Linux-Paketierung: udev-Regeln, systemd-Unit, `.deb` / `.rpm` / `.pkg.tar.zst` | ✅ Linux |
| Gestentaste: Belegungen pro Richtung + Live-Erfassung | ✅ (abhängig von Gerätefähigkeiten) |
| Erfassung von Mittel-/Mode-Shift-/Daumenrad-Taste | ✅ Mitteltaste auf allen Plattformen; Mode-Shift / Daumenrad geräteabhängig |
| Windows (Agent, GUI, Event-Hook, Installer) | ✅ auf Windows 11-Hardware validiert; neuerer Port mit laufender Kompatibilitätsverbesserung |

¹ Medientasten-Aktionen nutzen unter Linux D-Bus MPRIS; einige macOS-spezifische Aktionen haben unter Linux kein universelles Gegenstück und sind No-ops. Windows bildet Plattformaktionen, wo verfügbar, auf native Entsprechungen ab.

## Installation

> [!IMPORTANT]
> Beende zuerst **Logi Options+** — die beiden Anwendungen streiten sich um den HID++-Zugriff, und ein Empfänger kann immer nur einem gehören.

### macOS

Erfordert macOS 13 oder neuer.

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

# Arch Linux
sudo pacman -U openlogi-*.pkg.tar.zst
```

Pakete erscheinen für `x86_64`/`amd64` und `arm64`/`aarch64`.

Das Paket installiert udev-Regeln, die deinem Benutzer Zugriff auf `/dev/hidraw*` und `/dev/uinput` ohne `sudo` geben. Aktiviere nach der Installation den Hintergrund-Agent für deinen Benutzer:

```sh
systemctl --user enable --now openlogi-agent.service
```

Für manuelle / Quellcode-Installationen und Distributionen ohne systemd siehe [INSTALL-linux.md](INSTALL-linux.md).

### Windows

Jedem Release liegen signierte portable `.zip`-Archive und Per-User-`.msi`-Installer (x86_64 und arm64) bei. Beide enthalten die GUI (`OpenLogi.exe`) zusammen mit dem Hintergrund-Agent (`openlogi-agent.exe`), dem sämtliche Geräte-I/O gehören. Halte bei der portablen ZIP beide Dateien nebeneinander, sonst hat die GUI keine Gegenstelle.

Windows funktioniert und wurde auf echter Windows 11-Hardware vollständig validiert — mit einer kabelgebundenen Tastatur und einer Maus am Unifying-Empfänger, einschließlich Installation, In-Place-Upgrade und Deinstallation des MSI. Der Port ist neuer als die macOS-Version; [melde](https://github.com/AprilNEA/OpenLogi/issues) bitte Ecken und Kanten. Der Agent zeigt ein Symbol im Infobereich (Hauptfenster anzeigen / Beenden), damit die App nach dem Schließen des Hauptfensters erreichbar bleibt. Setze zum Deaktivieren unter Windows `show_in_menu_bar = false` im TOML-Block `[app_settings]` und starte den Agent neu; der GUI-Schalter ist derzeit nur unter macOS verfügbar.

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

## Repo-Aktivität

![Repobeats analytics image](https://repobeats.axiom.co/api/embed/4a0b576a03e9d528ad31ccf4797a1286c045d021.svg "Repobeats analytics image")
