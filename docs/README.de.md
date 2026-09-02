> [!WARNING]
> **OpenLogi befindet sich in aktiver Entwicklung** und ist noch nicht stabil — Funktionen und Konfiguration können sich noch ändern. Gib dem Repo einen **Star** ⭐ und **beobachte** 👀 es, um benachrichtigt zu werden, wenn ein neues Release erscheint.

<h4 align="right"><a href="../README.md">English</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.ja.md">日本語</a> | <strong>Deutsch</strong> | <a href="README.fr.md">Français</a> | <a href="README.ko.md">한국어</a></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Eine native, local-first Alternative zu Logitech Options+, geschrieben in Rust 🦀<br/>Schöpfe das volle Potenzial von Logitech-Mäusen, -Tastaturen und -Webcams über HID++ und UVC aus</strong></p>


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

Läuft auf macOS, Linux und Windows.

---

## Mehr als Options+

Was OpenLogi kann und Options+ nicht:

- **Leichtgewichtig bleiben.** Natives Rust + GPUI.
- **Auf Linux laufen.** Linux ist in OpenLogi eine vollwertige Plattform.
- **Die Gestentaste frei wählen.** Weise die Gestenrolle jeder beliebigen physischen Taste zu — oder schalte Gesten ganz ab.
- **Konfiguration im Klartext.** Alles steckt in einer TOML-Datei, die sich beliebig zwischen Rechnern synchronisieren lässt.
- **Skriptbar.** Neben der GUI gibt es eine echte CLI.

## Funktionen

- Geräte über Logi-Bolt-Empfänger, Unifying-Empfänger, Bluetooth oder Kabel, mit Akkustand und Ladezustand
- Tastenumbelegung über den OS-Input-Hook: Katalog eingebauter Aktionen plus eigene Tastenkürzel (in TOML angelegt)¹
- Profil-Overlays pro Anwendung mit Auto-Wechsel bei App-Fokus (macOS + Windows; Linux unter GNOME über die mitgelieferte Shell-Erweiterung, mit wlroots-Compositoren oder über X11 / XWayland)
- Litra-Leuchten: Ein/Aus, Helligkeit und Farbtemperatur, auf Wunsch automatisch an die Kameraaktivität gekoppelt

**Maus**

- Erfassung und Umbelegung von Mitteltaste, Mode-Shift und Daumenrad (Mitteltaste überall, der Rest, sofern das Gerät sie bereitstellt)
- Gestenbelegungen pro Richtung mit Live-Erfassung, auf jeder geeigneten Taste
- Actions Ring: ein cursorzentriertes Aktions-Overlay mit acht Slots (`ShowActionsRing`), mit Layouts pro Anwendung
- DPI-Steuerung mit Voreinstellungen und Cycle-/Set-Preset-Aktionen (`0x2201`)
- SmartShift-Rad: Modus, Empfindlichkeit und permanente Rasterung (`0x2111`)
- Native Scroll-Umkehr pro Gerät (`0x2121`, unterstützte Geräte)

**Tastatur**

- Globale F-Tasten-Umbelegung: derselbe Aktionskatalog wie bei der Maus, plus Power-User-Aktionen — Texteingabe, Tastenkombinationen, mehrstufige Workflows (macOS + Windows)
- Statische RGB-Beleuchtung (`0x8070` / `0x8080`, unterstützte Geräte)

**Kamera**

- Jede Logitech-UVC-Webcam (Brio, StreamCam, die C920-Serie, …), Plug and Play
- Live-Vorschau, die die Kamera nur öffnet, solange du hinschaust — beim Verlassen wird sie vollständig freigegeben und die LED erlischt
- Bildregler schreiben direkt in die UVC-Hardware — Zoom, Fokus, Belichtung, Helligkeit, Kontrast, Sättigung, Schärfe, Weißabgleich, Farbton, mit Automatik-Schaltern für Fokus / Belichtung / Weißabgleich — und wirken damit in Meet / Zoom / OBS und jeder anderen App, die die Kamera nutzt
- Ein-Klick-Profile: eingebaut Standard / Streaming / Videoanruf, dazu eigene Schnappschüsse; Einstellungen bleiben pro Kamera erhalten und werden beim nächsten Ansehen in die Hardware zurückgeschrieben

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

# Arch Linux — aus dem AUR (aktualisiert sich mit dem System):
paru -S openlogi-bin   # or: yay -S openlogi-bin
# …oder das Release-Paket direkt installieren:
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
