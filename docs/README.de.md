> [!WARNING]
> **OpenLogi befindet sich in aktiver Entwicklung** und ist noch nicht stabil — Funktionen und Konfiguration können sich noch ändern. Gib dem Repo einen **Star** ⭐ und **beobachte** 👀 es, um benachrichtigt zu werden, wenn ein neues Release erscheint.

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Eine native, local-first Alternative zu Logitech Options+, geschrieben in Rust 🦀<br/>Schöpfe das volle Potenzial von Logitech-Mäusen, -Tastaturen und -Webcams über HID++ und UVC aus</strong></p>


<div align="center">
    <a href="https://github.com/AprilNEA/OpenLogi/releases/latest" target="_blank">
    <img alt="GitHub release" src="https://img.shields.io/github/v/release/AprilNEA/OpenLogi?style=flat"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/releases" target="_blank">
    <img alt="GitHub downloads" src="https://img.shields.io/github/downloads/AprilNEA/OpenLogi/total.svg?style=flat"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/commits" target="_blank">
    <img alt="GitHub commit" src="https://img.shields.io/github/commit-activity/m/AprilNEA/OpenLogi?style=flat"></a>
    <a href="#lizenz">
    <img alt="License" src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue?style=flat"></a>
    <img alt="Hits" src="https://hits.aprilnea.com/hits?url=https://github.com/aprilnea/openlogi">
</div>

<div align="center">
    <a href="https://x.com/AprilNEA" target="_blank">
    <img alt="X" src="https://img.shields.io/badge/follow-AprilNEA-black?style=flat&logo=x"></a>
    <a href="https://t.me/+VDtkR5OSAT04NzVh" target="_blank">
    <img alt="telegram" src="https://img.shields.io/badge/chat-telegram-blueviolet?style=flat&logo=Telegram"></a>
</div>

<div align="center">
    <a href="../README.md">
    <img alt="English" src="https://img.shields.io/badge/English-lightgrey?style=flat"></a>
    <a href="README.zh-CN.md">
    <img alt="简体中文" src="https://img.shields.io/badge/%E7%AE%80%E4%BD%93%E4%B8%AD%E6%96%87-lightgrey?style=flat"></a>
    <a href="README.ja.md">
    <img alt="日本語" src="https://img.shields.io/badge/%E6%97%A5%E6%9C%AC%E8%AA%9E-lightgrey?style=flat"></a>
    <a href="README.de.md">
    <img alt="Deutsch" src="https://img.shields.io/badge/Deutsch-blue?style=flat"></a>
    <a href="README.fr.md">
    <img alt="Français" src="https://img.shields.io/badge/Fran%C3%A7ais-lightgrey?style=flat"></a>
    <a href="README.ko.md">
    <img alt="한국어" src="assets/badge-lang-ko.svg"></a>
    <a href="README.ru.md">
    <img alt="Русский" src="https://img.shields.io/badge/%D0%A0%D1%83%D1%81%D1%81%D0%BA%D0%B8%D0%B9-lightgrey?style=flat"></a>
    <a href="README.es.md">
    <img alt="Español" src="https://img.shields.io/badge/Espa%C3%B1ol-lightgrey?style=flat"></a>
    <a href="README.pt-BR.md">
    <img alt="Português" src="https://img.shields.io/badge/Portugu%C3%AAs-lightgrey?style=flat"></a>
    <a href="README.tr.md">
    <img alt="Türkçe" src="https://img.shields.io/badge/T%C3%BCrk%C3%A7e-lightgrey?style=flat"></a>
</div>

<p align="center">
    <a href="https://trendshift.io/repositories/42303" target="_blank">
    <img src="https://trendshift.io/api/badge/repositories/42303" alt="AprilNEA%2FOpenLogi | Trendshift" width="250" height="55"/></a>
    <a href="https://www.producthunt.com/products/openlogi?embed=true&amp;utm_source=badge-featured&amp;utm_medium=badge&amp;utm_campaign=badge-openlogi" target="_blank" rel="noopener noreferrer">
    <picture>
        <source media="(prefers-color-scheme: dark)" srcset="https://api.producthunt.com/widgets/embed-image/v1/top-post-badge.svg?post_id=openlogi&amp;theme=dark&amp;period=daily">
        <source media="(prefers-color-scheme: light)" srcset="https://api.producthunt.com/widgets/embed-image/v1/top-post-badge.svg?post_id=openlogi&amp;theme=light&amp;period=daily">
        <img alt="OpenLogi - A local-first alternative to Logitech Options+ | Product Hunt" width="250" height="54" src="https://api.producthunt.com/widgets/embed-image/v1/top-post-badge.svg?post_id=openlogi&amp;theme=light&amp;period=daily">
    </picture></a>
</p>

> **Genug von Options+? Probier OpenLogi.**

Läuft auf macOS, Linux und Windows.

---

## Mehr als Options+

Was OpenLogi kann und Options+ nicht:

- **Leichtgewichtig bleiben.** Natives Rust + GPUI.
- **Auf Linux laufen.** Linux ist in OpenLogi eine vollwertige Plattform.
- **Gesten auf unterstützten Tasten.** Weise unterstützten Bedienelementen Gestenaktionen zu — oder schalte Gesten ganz ab.
- **Konfiguration im Klartext.** Alles steckt in einer TOML-Datei, die sich beliebig zwischen Rechnern synchronisieren lässt.
- **Skriptbar.** Neben der GUI gibt es eine echte CLI: eine JSON-[Automatisierungs-API](USAGE.md#automation-api) für DPI, SmartShift, Fn-Sperre und Abfragerate, dazu Hardware-Diagnosen.

## Funktionen

<p align="center">
    <img src="https://assets.openlogi.org/screenshots/macos/light/devices-grid.png" width="860" alt="OpenLogi-Geräteliste mit sechs Logitech-Geräten: Mäuse, eine Tastatur und eine Litra-Leuchte">
</p>

### Geräte

- Logi-Bolt- und Unifying-Empfänger, Bluetooth und Kabelverbindungen, mit Akkustand und Ladezustand
- Hinweise bei niedrigem Akkustand am unteren Bildschirmrand, einmal pro Entladung, ohne Berechtigung für Systembenachrichtigungen
- Eine Bildschirmanzeige, wenn eine Tastatur Caps Lock ein- oder ausschaltet, unter Windows und Linux auch für Num Lock und Scroll Lock

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/easy-switch.png" width="400" alt="Easy-Switch-Karte mit drei gekoppelten Computern"></td>
        <td width="50%"><b>Easy-Switch</b><ul><li>Sieh, welcher Computer auf welchem Kanal gekoppelt ist, samt Betriebssystem und Verbindung</li><li>Benenne einen Kanal um, sofern das Gerät es erlaubt</li></ul></td>
    </tr>
</table>

### Tasten und Aktionen

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/buttons.png" width="400" alt="Tastenbelegung einer MX Master 4 mit geöffnetem Inspektor für die Gestentaste"></td>
        <td width="50%"><b>Jede Taste umbelegen</b><ul><li>Ein eingebauter Aktionskatalog, eigene Tastenkürzel, Texteingabe oder mehrstufige Workflows</li><li>Getrennte Aktionen für Tippen und Halten sowie Hold-to-Talk-Kombinationen, die mit der Taste losgelassen werden</li><li>Gesten auf Mittelklick, Zurück/Vorwärts, DPI/ModeShift, der Gestentaste und dem Haptik-Panel</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/actions-ring.png" width="400" alt="Actions-Ring-Editor mit acht Slots rund um den Cursor"></td>
        <td width="50%"><b>Actions Ring</b><ul><li>Acht Aktionen in einem Ring um den Cursor</li><li>Ein eigenes Layout pro Anwendung</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/keys.png" width="400" alt="MX Keys S mit drei Tasten, belegt mit Launchpad, Bildschirm sperren und Bildschirmfoto"></td>
        <td width="50%"><b>Tastaturtasten</b><ul><li>Medien-, Emoji-, Screenshot- und andere Sondertasten wie Maustasten belegen</li><li>Eine einzelne Taste auf die Gerätevorgabe zurücksetzen oder die ganze Reihe</li></ul></td>
    </tr>
</table>

Profile pro Anwendung wechseln mit der App im Fokus. Ein `[apps]`-Alias lässt ein Profil einer Anwendung plattformübergreifend folgen.

### Zeiger und Scrollen

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/dpi.png" width="400" alt="Karte zur Zeigerabstimmung mit DPI-Schieberegler"></td>
        <td width="50%"><b>Zeiger</b><ul><li>DPI-Voreinstellungen mit Cycle- und Set-Preset-Aktionen</li><li>Zeigergeschwindigkeit und Abfragerate</li><li>Präzisionsmodus: Eine Taste schaltet die Maus auf ihre langsamste Geschwindigkeit und wieder zurück</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/smartshift.png" width="400" alt="SmartShift-Karte mit Radmodus, Empfindlichkeit und Scrollkraft"></td>
        <td width="50%"><b>SmartShift</b><ul><li>Radmodus und Empfindlichkeit</li><li>Scrollkraft bei Mäusen mit SmartShift Enhanced</li><li>Ein Schalter für dauerhafte Rasterung</li></ul></td>
    </tr>
    <tr>
        <td colspan="2"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/scrolling.png" width="820" alt="Scroll-Karte mit Umkehr, Mausradauflösung und weichem Mausrad"><br><b>Scrollen</b><ul><li>Scroll-Umkehr pro Gerät, nativ oder per Software</li><li>Mausradauflösung und weiches Scrollen mit dem Mausrad</li><li>Zurück oder Vorwärts halten und das Rad drehen, um auf Mäusen ohne Daumenrad seitwärts zu scrollen</li></ul></td>
    </tr>
</table>

### Tastatur und Beleuchtung

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/keys-backlight.png" width="400" alt="Karten für Tasten und Hintergrundbeleuchtung einer MX Keys S"></td>
        <td width="50%"><b>Tastatur</b><ul><li>Fn Lock sowie Multi-Plattform-Tastaturen, die beim Layout des Host-Betriebssystems bleiben</li><li>Stufe der Hintergrundbeleuchtung, automatische Helligkeit, Energiesparen und wie lange die Tasten leuchten</li><li>Bildschirmanzeige der Beleuchtungsstufe, sobald sie sich ändert</li><li>Statisches RGB auf unterstützten Tastaturen</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/litra.png" width="400" alt="Litra-Glow-Beleuchtungspanel mit Helligkeit und Farbtemperatur"></td>
        <td width="50%"><b>Litra-Leuchten</b><ul><li>Ein/Aus, Helligkeit und Farbtemperatur</li><li>Schaltet sich auf Wunsch mit der Kamera ein</li><li>Übernimmt Änderungen, die direkt an der Leuchte vorgenommen werden</li></ul></td>
    </tr>
</table>

### Webcams

- Jede Logitech-UVC-Webcam (Brio, StreamCam, die C920-Serie, …), Plug and Play
- Live-Vorschau, die die Kamera nur belegt, solange du hinschaust; verlässt du sie, erlischt die LED
- Zoom, Schwenken, Neigen, Fokus, Belichtung, Weißabgleich und alle anderen UVC-Regler, direkt in die Hardware geschrieben, sodass Meet, Zoom und OBS die Änderung sehen
- Eingebaute Profile Standard / Streaming / Videoanruf plus eigene Schnappschüsse, pro Kamera gespeichert

Der Funktionsumfang hängt davon ab, was das jeweilige Gerät meldet; die App zeigt nur die Bedienelemente, die ein Gerät unterstützt.

### Plattformunterstützung

| Funktion | macOS | Windows | Linux |
| --- | --- | --- | --- |
| Umbelegung von Maustasten und Gesten | ✓ | ✓ | ✓ |
| Umbelegung der F-Tasten | ✓ | ✓ | — |
| Profile pro Anwendung | ✓ | ✓ | X11, wlroots (sway, Hyprland), GNOME mit der [Begleiterweiterung](../crates/openlogi-hook/gnome-shell-extension/README.md) |
| Mausprofil folgt dem Fenster unter dem Zeiger | ✓ | ✓ | Nur X11 |
| Medientasten | Nativ | Nativ | D-Bus MPRIS |
| Fenstermanager-Aktionen (Schreibtische, Mission Control, …) | Nativ | Native Entsprechungen | GNOME, KDE (KWin), Hyprland; einige reine macOS-Aktionen bewirken nichts |
| Symbol in Menüleiste / Infobereich | ✓ | ✓ | — |
| Updates in der App | Sparkle | Integriert | Integriert |

## Installation

> [!IMPORTANT]
> Beende zuerst **Logi Options+** (und unter Linux Solaar). Ein Empfänger kann immer nur einer Anwendung gleichzeitig gehören.

| Plattform | System | Architektur |
| --- | --- | --- |
| macOS | macOS 13 oder neuer | Apple Silicon, Intel |
| Linux | GLIBC 2.35 oder neuer | x86_64, arm64 |
| Windows | Getestet unter Windows 11 | x86_64, arm64 |

Jeder Download hat eine minisign-Signatur und einen Eintrag in `SHA256SUMS`. Zum Bauen aus dem Quellcode siehe [DEVELOPMENT.md](DEVELOPMENT.md).

### macOS

<p>
    <a href="https://openlogi.org/download/mac-arm64"><img src="https://assets.openlogi.org/brand/buttons/download-macos-arm64.png" width="260" alt="Für Mac herunterladen: Apple silicon (.dmg)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/mac-x64"><img src="https://assets.openlogi.org/brand/buttons/download-macos-x64.png" width="260" alt="Für Mac herunterladen: Intel (.dmg)"></a>
</p>

Oder per Homebrew:

```sh
brew install --cask openlogi
```

Öffne OpenLogi und erteile **Bedienungshilfen** und **Eingabeüberwachung**, wenn macOS danach fragt. Danach aktualisiert sich OpenLogi selbst über Sparkle.

### Linux

<p>
    <a href="https://openlogi.org/download/linux"><img src="https://assets.openlogi.org/brand/buttons/download-linux-deb.png" width="260" alt="Für Linux herunterladen: Debian, Ubuntu (.deb)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/linux-rpm"><img src="https://assets.openlogi.org/brand/buttons/download-linux-rpm.png" width="260" alt="Für Linux herunterladen: Fedora, SUSE (.rpm)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/linux-arch"><img src="https://assets.openlogi.org/brand/buttons/download-linux-arch.png" width="260" alt="Für Linux herunterladen: Arch (.pkg.tar.zst)"></a>
</p>

Oder installiere mit einem Befehl das passende Paket für deine Distribution. Das Skript prüft die minisign-Signatur und startet den Agent; installiere vorher `minisign`:

```sh
curl -fsSL https://github.com/AprilNEA/OpenLogi/releases/latest/download/install.sh | sh
```

Unter Arch Linux baut das AUR-Paket [`openlogi`](https://aur.archlinux.org/packages/openlogi) das Release aus dem Quellcode:

```sh
yay -S openlogi
```

Wie du das Skript vor dem Ausführen prüfst, sowie Hinweise zum AUR-Paket, Flatpak, NixOS und Distributionen ohne systemd findest du in [INSTALL-linux.md](INSTALL-linux.md).

### Windows

<p>
    <a href="https://openlogi.org/download/windows"><img src="https://assets.openlogi.org/brand/buttons/download-windows-msi.png" width="260" alt="Für Windows herunterladen: Installer (.msi)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/windows-zip"><img src="https://assets.openlogi.org/brand/buttons/download-windows-zip.png" width="260" alt="Für Windows herunterladen: portabel (.zip)"></a>
</p>

## Verwendung (CLI)

Siehe [USAGE.md](USAGE.md)

## Konfiguration

Einstellungen werden als reines TOML gespeichert; beim Speichern bleiben per Symlink eingebundene Konfigurationsdateien erhalten. Siehe [CONFIGURATION.md](CONFIGURATION.md).

## Entwicklung

Siehe [DEVELOPMENT.md](DEVELOPMENT.md), einschließlich [Sicherheit des macOS-Input-Hooks](DEVELOPMENT.md#macos-input-hook-safety).

## Danksagungen

- **Windows, Kameras und i18n** von [@davidbudnick](https://github.com/davidbudnick) — Tastatur-RGB, Windows-Unterstützung, Logitech-Webcam-Unterstützung
- **Linux-Portierung** von [@cserby](https://github.com/cserby) — Linux-Unterstützung
- [Solaar](https://github.com/pwr-Solaar/Solaar) von [@pwr](https://github.com/pwr) — quelloffene HID++-Implementierung
- [Mouser](https://github.com/TomBadash/Mouser) von [@TomBadash](https://github.com/TomBadash) — ein lokaler Options+-Ersatz ohne Konto

## Lizenz

Der Code in diesem Repository ist doppelt lizenziert, wahlweise unter

- Apache License, Version 2.0 ([LICENSE-APACHE](../LICENSE-APACHE))
- MIT-Lizenz ([LICENSE-MIT](../LICENSE-MIT))

### Code von Dritten

`crates/openlogi-hidpp` ist ein eingebundener Fork von [`hidpp`](https://crates.io/crates/hidpp)
von [@lus](https://github.com/lus), lizenziert unter 0BSD.

### Logo & Markenressourcen

Danke an [@kubai087](https://github.com/kubai087) für das Design des OpenLogi-Logos. Das OpenLogi-Logo und das App-Icon — die Markenressourcen unter [`design/`](../design/) — sind © 2026 AprilNEA, alle Rechte vorbehalten, und fallen nicht unter die obigen MIT-/Apache-Lizenzen; siehe [`design/LICENSE`](../design/LICENSE). Ein Fork des Codes gewährt kein Recht am Namen, Logo oder Icon von OpenLogi; bitte verwende sie nicht ohne vorherige schriftliche Erlaubnis für eigene Projekte, Forks oder Distributionen.

---

**Nicht mit Logitech verbunden.** „Logitech", „MX Master" und „Options+" sind Marken der Logitech International S.A.

## Repo-Aktivität

![Repobeats analytics image](https://repobeats.axiom.co/api/embed/4a0b576a03e9d528ad31ccf4797a1286c045d021.svg "Repobeats analytics image")
