> [!WARNING]
> **OpenLogi est en cours de développement actif** et n'est pas encore stable — les fonctionnalités et la configuration peuvent encore changer. Mettez une **Star** ⭐ au dépôt et **suivez-le** 👀 pour être averti dès qu'une version est publiée.

<h4 align="right"><a href="../README.md">English</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.ja.md">日本語</a> | <a href="README.de.md">Deutsch</a> | <strong>Français</strong> | <a href="README.ko.md">한국어</a></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Une alternative native et local-first à Logitech Options+, écrite en Rust 🦀<br/>Remappez boutons, DPI et SmartShift via HID++. Sans compte, sans télémétrie.</strong></p>


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

> **Assez d'Options+ ? Essayez OpenLogi.**

Remappez les boutons, pilotez le DPI et SmartShift, basculez de profil selon l'application — sans compte Logitech, sans télémétrie, sans installer l'Options+ officiel. Pas de cloud, une configuration en TOML brut ; les seuls appels réseau sont la récupération des images d'appareils et une vérification de mise à jour opt-in, désactivée par défaut.

---

## Présentation

OpenLogi dialogue avec les souris Logitech HID++ via un récepteur Logi Bolt — ou une connexion Bluetooth directe / filaire — sans exécuter Logi Options+. Il fournit deux binaires :

- **[OpenLogi GUI](../crates/openlogi-gui)** — une application de bureau GPUI : un schéma de souris interactif avec zones cliquables, un sélecteur d'action par bouton (41 actions intégrées plus des raccourcis personnalisés rédigés dans la configuration TOML), des préréglages DPI, un panneau SmartShift (mode de molette, sensibilité, cran permanent), des surcouches de profil par application, un carrousel d'appareils qui bascule en direct entre les appareils appairés, et une fenêtre de réglages dont l'interface est traduite en 20 langues.
- **[OpenLogi CLI](../crates/openlogi-cli)** — un outil en ligne de commande : inventaire headless (`list`), synchronisation des assets et sous-commandes de diagnostic des appareils.

Tout reste local : les affectations vivent dans un fichier TOML brut, les pressions de boutons sont remappées par le hook d'événements de l'OS, et les changements DPI / SmartShift sont écrits directement sur l'appareil via HID++.

macOS et Linux sont pris en charge. Windows est un aperçu précoce non testé — des builds signés accompagnent chaque release ; voir la [feuille de route](#feuille-de-route).

## Au-delà d'Options+

Ce qu'OpenLogi fait et qu'Options+ ne fait pas :

- **Tourner sous Linux.** Options+ n'existe que pour macOS et Windows. OpenLogi traite Linux en plateforme de premier rang : hook evdev/uinput, règles udev, unité utilisateur systemd et paquets `.deb` / `.rpm`.
- **Déplacer le bouton de gestes.** Choisissez quel bouton physique porte le rôle de gestes — pavé de pouce, bouton du milieu, précédent ou suivant — avec des affectations de glissement par direction, ou désactivez complètement les gestes. Options+ fige le rôle de gestes sur le pavé de pouce dédié.
- **Une configuration en texte brut.** Tout tient dans un fichier TOML que vous pouvez lire, diff-er, versionner et copier entre machines.
- **Scriptable.** Une vraie CLI : inventaire des appareils, préchargement des assets et diagnostics HID++ sur l'appareil (dump des features, allers-retours DPI / SmartShift).
- **Rester léger.** Des binaires natifs Rust + GPUI — pas de suite Electron, pas d'updaters résidents, pas de compte, pas de télémétrie.

## Feuille de route

| Capacité | État |
|---|---|
| Découverte des récepteurs Bolt + liste des appareils appairés (CLI + GUI) | ✅ |
| Récepteurs Unifying (protocole plus ancien, remplacé par Bolt) | ✅ |
| Appareils Bluetooth directs / filaires (sans récepteur) | ✅ |
| Pourcentage de batterie / état de charge | ✅ (appareils en ligne) |
| GUI interactive : carrousel, schéma de souris, sélecteur d'action | ✅ macOS + Linux |
| Remappage des boutons via le hook d'événements OS / evdev | ✅ macOS + Linux |
| Catalogue de 41 actions + raccourcis clavier personnalisés (rédigés en TOML) | ✅ macOS + Linux¹ |
| Contrôle DPI + préréglages + actions Cycle / Set-preset (HID++ `0x2201`) | ✅ |
| Molette SmartShift : mode + sensibilité + cran permanent (HID++ `0x2111`) | ✅ |
| Surcouches de profil par application (bascule automatique au focus) | ✅ macOS, 🟡 Linux (X11 uniquement) |
| Fenêtre de réglages : lancement à la connexion, mises à jour, barre de menus, permissions, langue | ✅ macOS + Linux |
| Interface localisée (20 langues : da, de, el, en, es, fi, fr, it, ja, ko, nb, nl, pl, pt-BR, pt-PT, ru, sv, zh-CN, zh-HK, zh-TW) | ✅ |
| Empaquetage Linux : règles udev, unité systemd, `.deb` / `.rpm` | ✅ Linux |
| Affectations par direction du bouton de gestes | 🟡 configurable ; capture matérielle en cours |
| Capture des boutons molette / mode-shift / molette de pouce | 🟡 configurable ; le hook ne gère que les boutons latéraux |
| Windows (agent, GUI, hook d'événements) | 🟡 aperçu non testé — `.exe` / `.msi` signés à chaque release |

¹ Sous Linux, les actions de touches multimédia passent par D-Bus MPRIS ; quelques actions propres à macOS (p. ex. Launchpad) n'ont pas d'équivalent Linux et sont sans effet.

## Installation

> [!IMPORTANT]
> Quittez d'abord **Logi Options+** — les deux applications se disputent l'accès HID++ et un récepteur ne peut appartenir qu'à une seule à la fois.

### macOS

Téléchargez le `.dmg` signé et notarié depuis la [dernière release](https://github.com/AprilNEA/OpenLogi/releases/latest) et glissez `OpenLogi.app` dans `/Applications`.

Ou installez via [Homebrew](https://brew.sh) :

```sh
brew install --cask openlogi
```

Le cask Homebrew officiel est la voie d'installation par défaut. Pour suivre explicitement la dernière release GitHub via `aprilnea/tap` :

```sh
brew tap aprilnea/tap
brew install --cask aprilnea/tap/openlogi@latest
```

`openlogi@latest` est maintenu par le workflow de release d'OpenLogi et peut être mis à jour avant l'autobump du cask officiel. Installez `openlogi` ou `openlogi@latest`, pas les deux.

### Linux

Téléchargez le `.deb` ou le `.rpm` depuis la [dernière release](https://github.com/AprilNEA/OpenLogi/releases/latest) :

```sh
# Debian / Ubuntu
sudo dpkg -i openlogi_*.deb

# Fedora / RHEL
sudo rpm -i openlogi-*.rpm
```

Les paquets sont publiés pour `x86_64`/`amd64` et `arm64`/`aarch64`.

Le paquet installe des règles udev qui donnent à votre utilisateur l'accès à `/dev/hidraw*` et `/dev/uinput` sans `sudo`. Après l'installation, activez l'agent d'arrière-plan pour votre utilisateur :

```sh
systemctl --user enable --now openlogi-agent.service
```

Pour les installations manuelles / depuis les sources et les distributions sans systemd, voir [INSTALL-linux.md](INSTALL-linux.md).

### Windows (aperçu)

Des installeurs signés `.exe` et `.msi` par utilisateur (x86_64 et arm64) accompagnent chaque release. La prise en charge de Windows est un aperçu précoce qui n'a pas encore été largement testé sur du matériel réel — attendez-vous à des aspérités et [signalez les problèmes](https://github.com/AprilNEA/OpenLogi/issues).

Pour compiler depuis les sources, voir [DEVELOPMENT.md](DEVELOPMENT.md).


## Utilisation (CLI)

Voir [USAGE.md](USAGE.md)

## Configuration

Voir [CONFIGURATION.md](CONFIGURATION.md)

## Développement

Voir [DEVELOPMENT.md](DEVELOPMENT.md)

## Remerciements

- [`hidpp`](https://crates.io/crates/hidpp) par [@lus](https://github.com/lus)
- [Solaar](https://github.com/pwr-Solaar/Solaar)
- [Mouser](https://github.com/TomBadash/Mouser) par Tom Badash

## Licence

Sous double licence, au choix :

- Apache License, version 2.0 ([LICENSE-APACHE](../LICENSE-APACHE))
- Licence MIT ([LICENSE-MIT](../LICENSE-MIT))

### Logo et ressources de marque

Le logo et l'icône d'application OpenLogi — les ressources de marque sous [`design/`](../design/) — sont © 2026 AprilNEA, tous droits réservés, et ne sont pas couverts par les licences MIT/Apache ci-dessus ; voir [`design/LICENSE`](../design/LICENSE). Forker le code ne confère aucun droit sur le nom, le logo ou l'icône d'OpenLogi ; merci de ne pas les utiliser pour représenter vos propres projets, forks ou distributions sans autorisation écrite préalable.

---

**Sans affiliation avec Logitech.** « Logitech », « MX Master » et « Options+ » sont des marques de Logitech International S.A.
