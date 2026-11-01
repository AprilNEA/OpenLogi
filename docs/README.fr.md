> [!WARNING]
> **OpenLogi est en cours de développement actif** et n'est pas encore stable — les fonctionnalités et la configuration peuvent encore changer. Mettez une **Star** ⭐ au dépôt et **suivez-le** 👀 pour être averti dès qu'une nouvelle version est publiée.

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Une alternative native et local-first à Logitech Options+, écrite en Rust 🦀<br/>Libérez tout le potentiel des souris, claviers et webcams Logitech via HID++ et UVC</strong></p>


<div align="center">
    <a href="https://github.com/AprilNEA/OpenLogi/releases/latest" target="_blank">
    <img alt="GitHub release" src="https://img.shields.io/github/v/release/AprilNEA/OpenLogi?style=flat"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/releases" target="_blank">
    <img alt="GitHub downloads" src="https://img.shields.io/github/downloads/AprilNEA/OpenLogi/total.svg?style=flat"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/commits" target="_blank">
    <img alt="GitHub commit" src="https://img.shields.io/github/commit-activity/m/AprilNEA/OpenLogi?style=flat"></a>
    <a href="#licence">
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
    <img alt="Deutsch" src="https://img.shields.io/badge/Deutsch-lightgrey?style=flat"></a>
    <a href="README.fr.md">
    <img alt="Français" src="https://img.shields.io/badge/Fran%C3%A7ais-blue?style=flat"></a>
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

> **Assez d'Options+ ? Essayez OpenLogi.**

Fonctionne sous macOS, Linux et Windows.

---

## Au-delà d'Options+

Ce qu'OpenLogi fait et qu'Options+ ne fait pas :

- **Rester léger.** Du Rust natif + GPUI.
- **Tourner sous Linux.** Linux est une plateforme de premier rang pour OpenLogi.
- **Des gestes sur les boutons pris en charge.** Affectez des actions gestuelles aux commandes prises en charge — ou désactivez complètement les gestes.
- **Une configuration en texte brut.** Tout tient dans un fichier TOML, synchronisable entre machines comme vous voulez.
- **Scriptable.** Une vraie CLI en plus de la GUI : une [API d'automatisation](USAGE.md#automation-api) JSON pour le DPI, SmartShift, le verrouillage Fn et le taux de rapport, plus des diagnostics matériels.

## Fonctionnalités

<p align="center">
    <img src="https://assets.openlogi.org/screenshots/macos/light/devices-grid.png" width="860" alt="Liste des appareils OpenLogi avec six appareils Logitech : des souris, un clavier et une lampe Litra">
</p>

### Appareils

- Récepteurs Logi Bolt et Unifying, Bluetooth et connexions filaires, avec niveau de batterie et état de charge
- Alertes de batterie faible en bas de l'écran, une fois par décharge, sans autorisation de notifications système
- Un indicateur à l'écran quand un clavier active ou désactive Caps Lock, ainsi que Num Lock et Scroll Lock sous Windows et Linux

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/easy-switch.png" width="400" alt="Carte Easy-Switch listant trois ordinateurs appairés"></td>
        <td width="50%"><b>Easy-Switch</b><ul><li>Voyez quel ordinateur est appairé sur chaque canal, avec son système et sa connexion</li><li>Renommez un canal sur les appareils qui le permettent</li></ul></td>
    </tr>
</table>

### Boutons et actions

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/buttons.png" width="400" alt="Affectation des boutons d'une MX Master 4, avec l'inspecteur du bouton de gestes ouvert"></td>
        <td width="50%"><b>Remappez n'importe quel bouton</b><ul><li>Un catalogue d'actions intégré, des raccourcis personnalisés, du texte saisi ou des workflows multi-étapes</li><li>Des actions distinctes pour l'appui court et l'appui long, et des combinaisons push-to-talk relâchées avec le bouton</li><li>Des gestes sur le clic du milieu, Précédent/Suivant, DPI/ModeShift, le bouton de gestes et le panneau haptique</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/actions-ring.png" width="400" alt="Éditeur de l'Actions Ring avec huit emplacements autour du curseur"></td>
        <td width="50%"><b>Actions Ring</b><ul><li>Huit actions en anneau autour du curseur</li><li>Une disposition propre à chaque application</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/keys.png" width="400" alt="MX Keys S avec trois touches affectées à Launchpad, Verrouiller l'écran et Capture d'écran"></td>
        <td width="50%"><b>Touches du clavier</b><ul><li>Affectez les touches multimédia, emoji, capture d'écran et autres touches dédiées comme des boutons de souris</li><li>Rétablissez une touche à sa valeur par défaut de l'appareil, ou réinitialisez toute la rangée</li></ul></td>
    </tr>
</table>

Les profils par application basculent avec l'application au premier plan. Un alias `[apps]` permet à un profil de suivre une application d'une plateforme à l'autre.

### Pointeur et défilement

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/dpi.png" width="400" alt="Carte de réglage du pointeur avec un curseur DPI"></td>
        <td width="50%"><b>Pointeur</b><ul><li>Préréglages DPI avec actions Cycle et Set-preset</li><li>Vitesse du pointeur et taux de rapport</li><li>Mode Précision : un bouton fait passer la souris à sa vitesse la plus lente, puis la rétablit</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/smartshift.png" width="400" alt="Carte SmartShift avec mode de molette, sensibilité et force de défilement"></td>
        <td width="50%"><b>SmartShift</b><ul><li>Mode de molette et sensibilité</li><li>Force de défilement sur les souris SmartShift Enhanced</li><li>Un interrupteur de crantage permanent</li></ul></td>
    </tr>
    <tr>
        <td colspan="2"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/scrolling.png" width="820" alt="Carte de défilement avec inversion, résolution de la molette et molette fluide"><br><b>Défilement</b><ul><li>Inversion du défilement par appareil, nativement ou par logiciel</li><li>Résolution de la molette et défilement fluide à la molette</li><li>Maintenez Précédent ou Suivant et tournez la molette pour défiler horizontalement sur les souris sans molette latérale</li></ul></td>
    </tr>
</table>

### Clavier et éclairage

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/keys-backlight.png" width="400" alt="Cartes Touches et Rétroéclairage d'un MX Keys S"></td>
        <td width="50%"><b>Clavier</b><ul><li>Fn Lock, et claviers multiplateformes qui restent sur la disposition du système hôte</li><li>Niveau de rétroéclairage, luminosité automatique, économie de batterie et durée d'éclairage des touches</li><li>Le niveau de rétroéclairage affiché à l'écran quand il change</li><li>RGB statique sur les claviers compatibles</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/litra.png" width="400" alt="Panneau d'éclairage Litra Glow avec luminosité et température de couleur"></td>
        <td width="50%"><b>Lampes Litra</b><ul><li>Alimentation, luminosité et température de couleur</li><li>S'allume avec la caméra si vous le souhaitez</li><li>Conserve un réglage effectué sur la lampe elle-même</li></ul></td>
    </tr>
</table>

### Webcams

- N'importe quelle webcam UVC Logitech (Brio, StreamCam, série C920, …), prête à l'emploi
- Aperçu en direct qui n'occupe la caméra que pendant que vous regardez ; quittez-le et la LED s'éteint
- Zoom, panoramique, inclinaison, mise au point, exposition, balance des blancs et toutes les autres commandes UVC, écrits dans le matériel pour que Meet, Zoom et OBS voient le changement
- Profils intégrés Par défaut / Streaming / Appel vidéo, plus vos propres instantanés, conservés par caméra

Les fonctionnalités disponibles dépendent de ce que chaque appareil signale ; l'application n'affiche que les commandes qu'un appareil prend en charge.

### Plateformes prises en charge

| Fonctionnalité | macOS | Windows | Linux |
| --- | --- | --- | --- |
| Remappage des boutons de souris et gestes | ✓ | ✓ | ✓ |
| Remappage des touches F du clavier | ✓ | ✓ | — |
| Profils par application | ✓ | ✓ | X11, wlroots (sway, Hyprland), GNOME avec l'[extension compagnon](../crates/openlogi-hook/gnome-shell-extension/README.md) |
| Le profil de la souris suit la fenêtre sous le pointeur | ✓ | ✓ | X11 uniquement |
| Touches multimédia | Natif | Natif | D-Bus MPRIS |
| Actions du gestionnaire de fenêtres (bureaux, Mission Control, …) | Natif | Équivalents natifs | GNOME, KDE (KWin), Hyprland ; certaines actions propres à macOS sont sans effet |
| Icône de barre des menus / zone de notification | ✓ | ✓ | — |
| Mises à jour intégrées | Sparkle | Intégré | Intégré |

## Installation

> [!IMPORTANT]
> Quittez d'abord **Logi Options+** (et Solaar sous Linux). Un récepteur ne peut appartenir qu'à une seule application à la fois.
> Sous macOS, **Karabiner-Elements** verrouille aussi un clavier Bluetooth qu'il modifie. Désactivez **Modify events** pour ce clavier dans Karabiner-Elements → Settings → Devices.

| Plateforme | Système | Architecture |
| --- | --- | --- |
| macOS | macOS 13 ou ultérieur | Apple silicon, Intel |
| Linux | GLIBC 2.35 ou plus récent | x86_64, arm64 |
| Windows | Testé sous Windows 11 | x86_64, arm64 |

Chaque téléchargement est accompagné d'une signature minisign et d'une entrée dans `SHA256SUMS`. Pour compiler depuis les sources, voir [DEVELOPMENT.md](DEVELOPMENT.md).

### macOS

<p>
    <a href="https://openlogi.org/download/mac-arm64"><img src="https://assets.openlogi.org/brand/buttons/download-macos-arm64.png" width="260" alt="Télécharger pour Mac : Apple silicon (.dmg)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/mac-x64"><img src="https://assets.openlogi.org/brand/buttons/download-macos-x64.png" width="260" alt="Télécharger pour Mac : Intel (.dmg)"></a>
</p>

Ou avec Homebrew :

```sh
brew install --cask openlogi
```

Ouvrez OpenLogi et accordez **Accessibilité** et **Surveillance de la saisie** lorsque macOS le demande. OpenLogi se met ensuite à jour lui-même via Sparkle.

### Linux

<p>
    <a href="https://openlogi.org/download/linux"><img src="https://assets.openlogi.org/brand/buttons/download-linux-deb.png" width="260" alt="Télécharger pour Linux : Debian, Ubuntu (.deb)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/linux-rpm"><img src="https://assets.openlogi.org/brand/buttons/download-linux-rpm.png" width="260" alt="Télécharger pour Linux : Fedora, SUSE (.rpm)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/linux-arch"><img src="https://assets.openlogi.org/brand/buttons/download-linux-arch.png" width="260" alt="Télécharger pour Linux : Arch (.pkg.tar.zst)"></a>
</p>

Ou installez le paquet adapté à votre distribution en une seule commande. Le script vérifie la signature minisign et démarre l'agent ; installez d'abord `minisign` :

```sh
curl -fsSL https://github.com/AprilNEA/OpenLogi/releases/latest/download/install.sh | sh
```

Sous Arch Linux, le paquet AUR [`openlogi`](https://aur.archlinux.org/packages/openlogi) compile la version depuis les sources :

```sh
yay -S openlogi
```

Pour examiner le script avant de l'exécuter, ainsi que pour le paquet AUR, Flatpak, NixOS et les distributions sans systemd, voir [INSTALL-linux.md](INSTALL-linux.md).

### Windows

<p>
    <a href="https://openlogi.org/download/windows"><img src="https://assets.openlogi.org/brand/buttons/download-windows-msi.png" width="260" alt="Télécharger pour Windows : installateur (.msi)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/windows-zip"><img src="https://assets.openlogi.org/brand/buttons/download-windows-zip.png" width="260" alt="Télécharger pour Windows : portable (.zip)"></a>
</p>

## Utilisation (CLI)

Voir [USAGE.md](USAGE.md)

## Configuration

Les réglages sont stockés en TOML brut ; l'enregistrement préserve les fichiers de configuration liés par lien symbolique. Voir [CONFIGURATION.md](CONFIGURATION.md).

## Développement

Voir [DEVELOPMENT.md](DEVELOPMENT.md), notamment la [sécurité du hook d'entrée macOS](DEVELOPMENT.md#macos-input-hook-safety).

## Remerciements

- **Windows, caméras et i18n** par [@davidbudnick](https://github.com/davidbudnick) — RGB clavier, prise en charge de Windows, prise en charge des webcams Logitech
- **Portage Linux** par [@cserby](https://github.com/cserby) — prise en charge de Linux
- [Solaar](https://github.com/pwr-Solaar/Solaar) par [@pwr](https://github.com/pwr) — implémentation open source de HID++
- [Mouser](https://github.com/TomBadash/Mouser) par [@TomBadash](https://github.com/TomBadash) — un remplacement d'Options+ local et sans compte

## Licence

Le code de ce dépôt est sous double licence, au choix :

- Apache License, version 2.0 ([LICENSE-APACHE](../LICENSE-APACHE))
- Licence MIT ([LICENSE-MIT](../LICENSE-MIT))

### Code tiers

`crates/openlogi-hidpp` est un fork intégré de [`hidpp`](https://crates.io/crates/hidpp)
par [@lus](https://github.com/lus), sous licence 0BSD.

### Logo et ressources de marque

Merci à [@kubai087](https://github.com/kubai087) pour la conception du logo OpenLogi. Le logo et l'icône d'application OpenLogi — les ressources de marque sous [`design/`](../design/) — sont © 2026 AprilNEA, tous droits réservés, et ne sont pas couverts par les licences MIT/Apache ci-dessus ; voir [`design/LICENSE`](../design/LICENSE). Forker le code ne confère aucun droit sur le nom, le logo ou l'icône d'OpenLogi ; merci de ne pas les utiliser pour représenter vos propres projets, forks ou distributions sans autorisation écrite préalable.

---

**Sans affiliation avec Logitech.** « Logitech », « MX Master » et « Options+ » sont des marques de Logitech International S.A.

## Activité du dépôt

![Repobeats analytics image](https://repobeats.com/AprilNEA/OpenLogi "Repobeats analytics image")
