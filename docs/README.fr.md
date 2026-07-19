> [!WARNING]
> **OpenLogi est en cours de développement actif** et n'est pas encore stable — les fonctionnalités et la configuration peuvent encore changer. Mettez une **Star** ⭐ au dépôt et **suivez-le** 👀 pour être averti dès qu'une nouvelle version est publiée.

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

<p align="center">
    <a href="https://trendshift.io/repositories/42303" target="_blank">
    <img src="https://trendshift.io/api/badge/trendshift/repositories/42303/daily?language=Rust" alt="AprilNEA%2FOpenLogi | Trendshift" width="250" height="55"/></a>
</p>

> **Assez d'Options+ ? Essayez OpenLogi.**

Remappez les boutons, pilotez le DPI et SmartShift, basculez de profil selon l'application — sans compte Logitech, sans télémétrie, sans installer l'Options+ officiel. Pas de cloud, une configuration en TOML brut. Par défaut, l'application ne se connecte automatiquement que pour récupérer les images d'appareils ; la vérification et le téléchargement des mises à jour ne se font qu'à votre demande ou après opt-in.

---

## Présentation

OpenLogi dialogue avec les périphériques Logitech HID++ via des récepteurs Logi Bolt et Unifying, une connexion Bluetooth directe ou un câble USB — sans exécuter Logi Options+. Il se compose de trois éléments :

- **[OpenLogi GUI](../crates/openlogi-gui)** — une application de bureau GPUI : schéma de souris interactif avec zones cliquables, sélecteur d'action par bouton (actions intégrées et raccourcis personnalisés rédigés dans la configuration TOML), préréglages DPI, SmartShift, inversion native du défilement par appareil, éclairage RGB des claviers, profils par application, carrousel d'appareils en direct et fenêtre de réglages traduite en 20 langues.
- **[OpenLogi agent](../crates/openlogi-agent)** — le service d'arrière-plan qui possède le hook d'entrée et toutes les E/S des appareils. La GUI est un pur client IPC et démarre l'agent au besoin.
- **[OpenLogi CLI](../crates/openlogi-cli)** — un outil en ligne de commande : inventaire headless (`list`), synchronisation des assets et sous-commandes de diagnostic des appareils.

Tout reste local : les affectations vivent dans un fichier TOML brut, l'agent remappe les pressions de boutons par le hook d'entrée de l'OS et écrit directement sur l'appareil via HID++ les changements de DPI, SmartShift, défilement et éclairage.

macOS, Linux et Windows sont pris en charge. Windows est le portage le plus récent : il a été validé de bout en bout sur du matériel Windows 11, mais peut rester moins poli que les builds macOS et Linux ; voir la [feuille de route](#feuille-de-route).

## Au-delà d'Options+

Ce qu'OpenLogi fait et qu'Options+ ne fait pas :

- **Tourner sous Linux.** Options+ n'existe que pour macOS et Windows. OpenLogi traite Linux en plateforme de premier rang : hook evdev/uinput, règles udev, unité utilisateur systemd et paquets `.deb` / `.rpm` / `.pkg.tar.zst`.
- **Déplacer le bouton de gestes.** Choisissez quel bouton physique porte le rôle de gestes — bouton de gestes dédié, bouton du milieu, précédent ou suivant — avec des affectations de glissement par direction, ou désactivez complètement les gestes. Options+ fige ce rôle sur le bouton de gestes dédié.
- **Une configuration en texte brut.** Tout tient dans un fichier TOML que vous pouvez lire, diff-er, versionner et copier entre machines.
- **Scriptable.** Une vraie CLI : inventaire des appareils, préchargement des assets et diagnostics HID++ sur l'appareil (dumps des features / contrôles, allers-retours DPI / SmartShift et vérification de l'éclairage du clavier).
- **Rester léger.** Des binaires natifs Rust + GPUI — pas de suite Electron, pas d'updaters résidents, pas de compte, pas de télémétrie.

## Feuille de route

| Capacité | État |
|---|---|
| Découverte des récepteurs Bolt + liste des appareils appairés (CLI + GUI) | ✅ |
| Récepteurs Unifying (protocole plus ancien, remplacé par Bolt) | ✅ |
| Appareils Bluetooth directs / filaires (sans récepteur) | ✅ |
| Pourcentage de batterie / état de charge | ✅ (appareils en ligne) |
| GUI interactive : carrousel, schéma de souris, sélecteur d'action | ✅ macOS + Linux + Windows |
| Remappage des boutons via le hook d'entrée de l'OS | ✅ macOS + Linux + Windows |
| Catalogue d'actions intégrées + raccourcis clavier personnalisés (rédigés en TOML) | ✅ macOS + Linux + Windows¹ |
| Contrôle DPI + préréglages + actions Cycle / Set-preset (HID++ `0x2201`) | ✅ |
| Molette SmartShift : mode + sensibilité + cran permanent (HID++ `0x2111`) | ✅ |
| Inversion native du défilement par appareil (HID++ `0x2121`) | ✅ (appareils compatibles) |
| Éclairage RGB statique des claviers (HID++ `0x8070` / `0x8080`) | ✅ (appareils compatibles) |
| Surcouches de profil par application (bascule automatique au focus) | ✅ macOS + Windows, 🟡 Linux (X11 / XWayland uniquement) |
| Fenêtre de réglages : lancement à la connexion, mises à jour, permissions, langue, apparence | ✅ macOS + Linux + Windows |
| Icône d'état de l'agent | ✅ barre des menus macOS + zone de notification Windows ; sans objet sous Linux |
| Interface localisée (20 langues : da, de, el, en, es, fi, fr, it, ja, ko, nb, nl, pl, pt-BR, pt-PT, ru, sv, zh-CN, zh-HK, zh-TW) | ✅ |
| Empaquetage Linux : règles udev, unité systemd, `.deb` / `.rpm` / `.pkg.tar.zst` | ✅ Linux |
| Affectations par direction du bouton de gestes + capture en direct | ✅ (selon les capacités de l'appareil) |
| Capture des boutons du milieu / mode-shift / molette de pouce | ✅ milieu sur toutes les plateformes ; mode-shift / molette selon l'appareil |
| Windows (agent, GUI, hook d'événements, installateur) | ✅ validé sur du matériel Windows 11 ; portage récent dont la compatibilité continue d'être peaufinée |

¹ Sous Linux, les actions de touches multimédia passent par D-Bus MPRIS ; quelques actions propres à macOS n'ont pas d'équivalent Linux universel et sont sans effet. Windows associe les actions de plateforme à leurs équivalents natifs lorsqu'ils existent.

## Installation

> [!IMPORTANT]
> Quittez d'abord **Logi Options+** — les deux applications se disputent l'accès HID++ et un récepteur ne peut appartenir qu'à une seule à la fois.

### macOS

Nécessite macOS 13 ou une version ultérieure.

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

# Arch Linux
sudo pacman -U openlogi-*.pkg.tar.zst
```

Les paquets sont publiés pour `x86_64`/`amd64` et `arm64`/`aarch64`.

Le paquet installe des règles udev qui donnent à votre utilisateur l'accès à `/dev/hidraw*` et `/dev/uinput` sans `sudo`. Après l'installation, activez l'agent d'arrière-plan pour votre utilisateur :

```sh
systemctl --user enable --now openlogi-agent.service
```

Pour les installations manuelles / depuis les sources et les distributions sans systemd, voir [INSTALL-linux.md](INSTALL-linux.md).

### Windows

Des archives portables `.zip` signées et des installateurs `.msi` par utilisateur (x86_64 et arm64) accompagnent chaque release. Tous deux contiennent la GUI (`OpenLogi.exe`) et l'agent d'arrière-plan (`openlogi-agent.exe`), qui possède toutes les E/S des appareils. Avec le ZIP portable, gardez les deux fichiers côte à côte, sinon la GUI n'aura rien auquel se connecter.

La prise en charge de Windows fonctionne et a été validée de bout en bout sur du matériel Windows 11 réel — un clavier filaire et une souris sur récepteur Unifying, y compris l'installation, la mise à niveau sur place et la désinstallation du MSI. Ce portage est plus récent que celui de macOS ; [signalez](https://github.com/AprilNEA/OpenLogi/issues) toute aspérité. L'agent affiche une icône dans la zone de notification (Afficher la fenêtre principale / Quitter), afin que l'application reste accessible après la fermeture de sa fenêtre principale. Pour la désactiver sous Windows, définissez `show_in_menu_bar = false` dans le bloc TOML `[app_settings]`, puis redémarrez l'agent ; l'option de la GUI est actuellement réservée à macOS.

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

## Activité du dépôt

![Repobeats analytics image](https://repobeats.axiom.co/api/embed/4a0b576a03e9d528ad31ccf4797a1286c045d021.svg "Repobeats analytics image")
