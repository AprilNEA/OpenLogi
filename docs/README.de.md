> [!WARNING]
> **OpenLogi befindet sich in aktiver Entwicklung** und ist noch nicht stabil —— Funktionen und Konfiguration können sich noch ändern. Gib dem Repo einen **Star** ⭐ und **beobachte** 👀 es, um sofort benachrichtigt zu werden, sobald ein Release erscheint.

<h4 align="right"><a href="../README.md">English</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.ja.md">日本語</a> | <strong>Deutsch</strong> | <a href="README.fr.md">Français</a> | <a href="README.ko.md">한국어</a></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-animated.svg" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Eine native, lokal-zuerst arbeitende Alternative zu Logitech Options+, geschrieben in Rust 🦀<br/>Tasten, DPI und SmartShift über HID++ neu belegen. Kein Konto, keine Telemetrie.</strong></p>


<div align="center">
    <a href="https://twitter.com/AprilNEA" target="_blank">
    <img alt="twitter" src="https://img.shields.io/badge/follow-AprilNEA-green?style=social&logo=Twitter"></a>
    <a href="https://t.me/+u8DfyLlIqPYxZjJh" target="_blank">
    <img alt="telegram" src="https://img.shields.io/badge/chat-telegram-blueviolet?style=flat&logo=Telegram"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/releases" target="_blank">
    <img alt="GitHub downloads" src="https://img.shields.io/github/downloads/AprilNEA/OpenLogi/total.svg?style=flat"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/commits" target="_blank">
    <img alt="GitHub commit" src="https://img.shields.io/github/commit-activity/m/AprilNEA/OpenLogi?style=flat"></a>
    <img alt="Hits" src="https://hits.aprilnea.com/hits?url=https://github.com/aprilnea/openlogi">
</div>

> **Genug von Options+? Probier OpenLogi.**

Belege Tasten neu, steuere DPI und SmartShift und wechsle Profile pro App —— ohne Logitech-Konto, ohne Telemetrie und ohne die offizielle Options+-Installation. Keine Cloud, schlichte TOML-Konfiguration; die einzigen Netzwerkzugriffe sind das Laden der Gerätebilder und eine optionale, standardmäßig deaktivierte Update-Prüfung.

---

## Was es ist

OpenLogi spricht mit Logitech-HID++-Mäusen über einen Logi-Bolt-Empfänger —— oder eine direkte Bluetooth- bzw. kabelgebundene Verbindung —— ohne Logi Options+ auszuführen. Es liefert zwei Binärdateien:

- **[OpenLogi GUI](../crates/openlogi-gui)** —— eine GPUI-Desktop-App: ein interaktives Maus-Diagramm mit anklickbaren Hotspots, ein Aktions-Auswahlmenü pro Taste (39 eingebaute Aktionen plus aufgezeichnete eigene Tastenkürzel), DPI-Presets, ein SmartShift-Schalter, anwendungsspezifische Profil-Overlays, ein Geräte-Karussell, das live zwischen gekoppelten Geräten wechselt, und ein Einstellungsfenster mit einer in sechs Sprachen lokalisierten Oberfläche.
- **[OpenLogi CLI](../crates/openlogi-cli)** —— ein CLI für die kopflose Inventarisierung (`list`) sowie Unterbefehle zur Asset-Synchronisierung und Geräte-Diagnose.

Alles bleibt lokal: Belegungen liegen in einer schlichten TOML-Datei, Tastendrücke werden über den OS-Event-Tap neu belegt, und DPI-/SmartShift-Änderungen werden direkt über HID++ auf das Gerät geschrieben.

macOS wird heute unterstützt; Linux und Windows folgen bald —— siehe [Roadmap](#roadmap).

## Roadmap

| Funktion | Status |
|---|---|
| Bolt-Empfänger erkennen + gekoppelte Geräte auflisten (CLI + GUI) | ✅ |
| Geräte per Bluetooth-Direktverbindung / Kabel (ohne Empfänger) | ✅ |
| Akkustand / Ladezustand | ✅ (Geräte online) |
| Interaktive GUI: Karussell, Maus-Diagramm, Aktions-Auswahl | ✅ macOS |
| Tastenneubelegung über den OS-Event-Tap (derzeit Seitentasten Back / Forward) | ✅ macOS |
| Katalog mit 39 Aktionen + aufgezeichnete eigene Tastenkürzel | ✅ macOS¹ |
| DPI-Steuerung + Presets + Aktionen „Durchschalten“ / „Preset setzen“ (HID++ `0x2201`) | ✅ macOS |
| SmartShift-Umschaltung des Radmodus (HID++ `0x2111`) | ✅ macOS |
| Anwendungsspezifische Profil-Overlays (automatischer Wechsel bei App-Fokus) | ✅ macOS |
| Einstellungsfenster: Start bei Anmeldung, Update-Prüfung, Menüleiste, Berechtigungen, Sprache | ✅ macOS |
| Oberflächen-Lokalisierung (6 Sprachen: en, ja, ru, zh-CN, zh-HK, zh-TW) | ✅ macOS |
| Richtungsbelegungen der Gestentaste | 🟡 konfigurierbar; Hardware-Erfassung ausstehend |
| Erfassung von Mittel- / Modus-Umschalt- / Daumenrad-Taste | 🟡 konfigurierbar; Hook belegt nur die Seitentasten |
| Linux-/Windows-Event-Hook | ❌ Stub (`Unsupported`) |
| Unifying-Empfänger | ❌ (noch nicht unterstützt) |

¹ Einige Aktionen (z. B. die Medientasten) protokollieren derzeit nur das beabsichtigte Ereignis, statt es tatsächlich auszulösen —— als Folgeaufgabe vermerkt.

## Installation

> [!IMPORTANT]
> Beende zuerst **Logi Options+** —— beide Programme konkurrieren um den HID++-Zugriff, und ein Empfänger kann jeweils nur von einem belegt werden.

Lade die signierte, notarisierte `.dmg` aus dem [neuesten Release](https://github.com/AprilNEA/OpenLogi/releases/latest) herunter und ziehe `OpenLogi.app` nach `/Applications`.

Oder installiere via [Homebrew](https://brew.sh):

```sh
brew install --cask openlogi
```

Zum Bauen aus dem Quellcode siehe [DEVELOPMENT.md](DEVELOPMENT.md).

## Verwendung (CLI)

Siehe [USAGE.md](USAGE.md).

## Konfiguration

Siehe [CONFIGURATION.md](CONFIGURATION.md).

## Entwicklung

Siehe [DEVELOPMENT.md](DEVELOPMENT.md).

## Danksagungen

- [`hidpp`](https://crates.io/crates/hidpp) von [@lus](https://github.com/lus)
- [Solaar](https://github.com/pwr-Solaar/Solaar)
- [Mouser](https://github.com/TomBadash/Mouser) von Tom Badash

## Lizenz

Dual-lizenziert unter wahlweise

- Apache License, Version 2.0 ([LICENSE-APACHE](../LICENSE-APACHE))
- MIT-Lizenz ([LICENSE-MIT](../LICENSE-MIT))

—— nach deiner Wahl.

---

**Nicht mit Logitech verbunden.** „Logitech“, „MX Master“ und „Options+“ sind Marken der Logitech International S.A.
