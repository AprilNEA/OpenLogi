> [!WARNING]
> **OpenLogi est en cours de développement actif** et n'est pas encore stable —— les fonctionnalités et la configuration peuvent encore changer. Mettez une **Star** ⭐ au dépôt et **suivez-le** 👀 pour être averti dès qu'une version est publiée.

<h4 align="right"><a href="../README.md">English</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.ja.md">日本語</a> | <a href="README.de.md">Deutsch</a> | <strong>Français</strong> | <a href="README.ko.md">한국어</a></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-animated.svg" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Une alternative native et locale d'abord à Logitech Options+, écrite en Rust 🦀<br/>Réaffectez les boutons, le DPI et SmartShift via HID++. Sans compte, sans télémétrie.</strong></p>


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

> **Marre d'Options+ ? Essayez OpenLogi.**

Réaffectez les boutons, pilotez le DPI et SmartShift, et changez de profil selon l'application —— sans compte Logitech, sans télémétrie et sans l'installation officielle d'Options+. Aucun cloud, configuration en simple TOML ; les seuls accès réseau sont le téléchargement des images d'appareils et une vérification de mise à jour optionnelle, désactivée par défaut.

---

## Présentation

OpenLogi communique avec les souris Logitech HID++ via un récepteur Logi Bolt —— ou une connexion Bluetooth directe / filaire —— sans exécuter Logi Options+. Il fournit deux binaires :

- **[OpenLogi GUI](../crates/openlogi-gui)** —— une application de bureau GPUI : un schéma de souris interactif avec des zones cliquables, un sélecteur d'action par bouton (39 actions intégrées plus des raccourcis personnalisés enregistrés), des préréglages DPI, un interrupteur SmartShift, des surcouches de profil par application, un carrousel d'appareils qui bascule en direct entre les appareils appairés, et une fenêtre de réglages dont l'interface est localisée en six langues.
- **[OpenLogi CLI](../crates/openlogi-cli)** —— une interface en ligne de commande pour l'inventaire sans interface graphique (`list`), ainsi que des sous-commandes de synchronisation des ressources et de diagnostic des appareils.

Tout reste local : les affectations vivent dans un simple fichier TOML, les appuis sur les boutons sont réaffectés via l'event tap du système, et les changements de DPI / SmartShift sont écrits directement sur l'appareil via HID++.

macOS est pris en charge aujourd'hui ; Linux et Windows arrivent bientôt —— voir la [Feuille de route](#feuille-de-route).

## Feuille de route

| Fonctionnalité | État |
|---|---|
| Détecter les récepteurs Bolt + lister les appareils appairés (CLI + GUI) | ✅ |
| Appareils Bluetooth direct / filaires (sans récepteur) | ✅ |
| Pourcentage de batterie / état de charge | ✅ (appareils en ligne) |
| GUI interactive : carrousel, schéma de souris, sélecteur d'action | ✅ macOS |
| Réaffectation des boutons via l'event tap du système (boutons latéraux Back / Forward pour l'instant) | ✅ macOS |
| Catalogue de 39 actions + raccourcis clavier personnalisés enregistrés | ✅ macOS¹ |
| Contrôle du DPI + préréglages + actions Cycler / Définir un préréglage (HID++ `0x2201`) | ✅ macOS |
| Bascule du mode molette SmartShift (HID++ `0x2111`) | ✅ macOS |
| Surcouches de profil par application (bascule auto au focus de l'app) | ✅ macOS |
| Fenêtre de réglages : lancement à l'ouverture de session, vérification des mises à jour, barre de menus, autorisations, langue | ✅ macOS |
| Localisation de l'interface (6 langues : en, ja, ru, zh-CN, zh-HK, zh-TW) | ✅ macOS |
| Affectations par direction du bouton gestuel | 🟡 configurable ; capture matérielle à venir |
| Capture des boutons central / changement de mode / molette du pouce | 🟡 configurable ; le hook ne gère que les boutons latéraux |
| Hook d'événements Linux / Windows | ❌ stub (`Unsupported`) |
| Récepteurs Unifying | ❌ (pas encore pris en charge) |

¹ Quelques actions (p. ex. les touches multimédias) se contentent pour l'instant de journaliser l'événement prévu au lieu de l'émettre réellement —— suivi en tant que tâche ultérieure.

## Installation

> [!IMPORTANT]
> Quittez d'abord **Logi Options+** —— les deux applications se disputent l'accès HID++, et un récepteur ne peut être détenu que par une seule à la fois.

Téléchargez le `.dmg` signé et notarisé depuis la [dernière version](https://github.com/AprilNEA/OpenLogi/releases/latest) et glissez `OpenLogi.app` dans `/Applications`.

Ou installez via [Homebrew](https://brew.sh) :

```sh
brew install --cask openlogi
```

Pour compiler depuis les sources, voir [DEVELOPMENT.md](DEVELOPMENT.md).

## Utilisation (CLI)

Voir [USAGE.md](USAGE.md).

## Configuration

Voir [CONFIGURATION.md](CONFIGURATION.md).

## Développement

Voir [DEVELOPMENT.md](DEVELOPMENT.md).

## Remerciements

- [`hidpp`](https://crates.io/crates/hidpp) par [@lus](https://github.com/lus)
- [Solaar](https://github.com/pwr-Solaar/Solaar)
- [Mouser](https://github.com/TomBadash/Mouser) par Tom Badash

## Licence

Sous double licence, au choix :

- Apache License, Version 2.0 ([LICENSE-APACHE](../LICENSE-APACHE))
- Licence MIT ([LICENSE-MIT](../LICENSE-MIT))

à votre convenance.

---

**Sans affiliation avec Logitech.** « Logitech », « MX Master » et « Options+ » sont des marques de Logitech International S.A.
