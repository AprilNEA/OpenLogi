> [!WARNING]
> **OpenLogi активно разрабатывается** и ещё не стабилен — функции и конфигурация могут меняться. Поставьте репозиторию **Star** ⭐ и **Watch** 👀, чтобы узнать о новом релизе.

<h4 align="right"><a href="../README.md">English</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.ja.md">日本語</a> | <a href="README.de.md">Deutsch</a> | <a href="README.fr.md">Français</a> | <a href="README.ko.md">한국어</a> | <strong>Русский</strong></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Нативная local-first альтернатива Logitech Options+, написанная на Rust 🦀<br/>Полные возможности мышей, клавиатур и веб-камер Logitech по HID++ и UVC</strong></p>

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
    <img src="https://trendshift.io/api/badge/repositories/42303" alt="AprilNEA%2FOpenLogi | Trendshift" width="250" height="55"/></a>
    <a href="https://www.producthunt.com/products/openlogi?embed=true&amp;utm_source=badge-featured&amp;utm_medium=badge&amp;utm_campaign=badge-openlogi" target="_blank" rel="noopener noreferrer">
    <picture>
        <source media="(prefers-color-scheme: dark)" srcset="https://api.producthunt.com/widgets/embed-image/v1/top-post-badge.svg?post_id=openlogi&amp;theme=dark&amp;period=daily">
        <source media="(prefers-color-scheme: light)" srcset="https://api.producthunt.com/widgets/embed-image/v1/top-post-badge.svg?post_id=openlogi&amp;theme=light&amp;period=daily">
        <img alt="OpenLogi - A local-first alternative to Logitech Options+ | Product Hunt" width="250" height="54" src="https://api.producthunt.com/widgets/embed-image/v1/top-post-badge.svg?post_id=openlogi&amp;theme=light&amp;period=daily">
    </picture></a>
</p>

> **Надоел Options+? Попробуйте OpenLogi.**

Работает на macOS, Linux и Windows.

---

## Больше, чем Options+

То, что OpenLogi умеет, а Options+ — нет:

- **Лёгкий.** Нативный Rust + GPUI.
- **Linux.** Linux — платформа первого класса.
- **Жесты на любой кнопке.** Назначьте роль жеста любой физической кнопке — или отключите жесты совсем.
- **Конфиг обычным текстом.** Всё в одном TOML-файле, который можно синхронизировать между машинами как угодно.
- **Скрипты.** Настоящий CLI рядом с GUI.

## Возможности

- Устройства через приёмники Logi Bolt, Unifying, Bluetooth или провод, с процентом батареи и состоянием зарядки
- Переназначение кнопок через OS input hook: встроенный каталог действий плюс свои сочетания клавиш в TOML, включая независимые действия на короткое/длинное нажатие и аккорды hold-until-release для push-to-talk¹
- Оверлеи профилей по приложениям с автопереключением по фокусу (macOS + Windows; Linux только на X11 / XWayland)
- Подсветка Litra: питание, яркость и цветовая температура, опционально автопитание по активности камеры

**Мышь**

- Захват и переназначение средней кнопки, mode-shift и thumbwheel (средняя везде, остальные — где устройство их отдаёт)
- Жесты по направлениям с live capture на любой подходящей кнопке
- Actions Ring: оверлей из восьми слотов вокруг курсора (`ShowActionsRing`), с раскладками по приложениям
- DPI: пресеты и действия Cycle / Set-preset (`0x2201`)
- Колесо SmartShift: переключение режима, чувствительность и панель постоянного храповика (`0x2111`)
- Нативная инверсия прокрутки на устройство (`0x2121`, поддерживаемые устройства)

**Клавиатура**

- Глобальное переназначение F-клавиш: тот же каталог действий, что у мыши, плюс действия для продвинутых пользователей — набор текста, комбинации клавиш, многошаговые сценарии (macOS + Windows)
- Статическая RGB-подсветка (`0x8070` / `0x8080`, поддерживаемые устройства)

**Камера**

- Любая Logitech UVC-веб-камера (Brio, StreamCam, серия C920, …), plug and play
- Живой превью открывает камеру только пока вы смотрите — выход полностью освобождает камеру, LED гаснет
- Настройки изображения пишутся сразу в UVC-железо — zoom, focus, exposure, brightness, contrast, saturation, sharpness, white balance, tint, anti-flicker и low-light compensation, с авто-режимами focus / exposure / white balance — поэтому изменения видны в Meet / Zoom / OBS и любом другом приложении, использующем камеру
- Профили в один клик: встроенные Default / Streaming / Video call плюс свои снимки; настройки хранятся на камеру и записываются обратно в железо при следующем просмотре

¹ Действия медиаклавиш на Linux идут через D-Bus MPRIS; часть macOS-специфичных действий не имеет универсального Linux-аналога и становится no-op. Windows сопоставляет платформенные действия с нативными эквивалентами, где это возможно.

## Установка

> [!IMPORTANT]
> Сначала закройте **Logi Options+**: два приложения борются за доступ к HID++, и приёмник может принадлежать только одному из них.

### macOS

Нужен macOS 13 или новее.

Скачайте подписанный и нотаризованный `.dmg` из [последнего релиза](https://github.com/AprilNEA/OpenLogi/releases/latest) и перетащите `OpenLogi.app` в `/Applications`.

Или установите через [Homebrew](https://brew.sh):

```sh
brew install --cask openlogi
```

Официальный cask Homebrew — путь установки по умолчанию. Чтобы явно следить за последним GitHub-релизом из `aprilnea/tap`:

```sh
brew tap aprilnea/tap
brew install --cask aprilnea/tap/openlogi@latest
```

`openlogi@latest` поддерживается workflow релизов OpenLogi и может обновиться раньше официального autobump cask. Ставьте либо `openlogi`, либо `openlogi@latest`, не оба сразу.

### Linux

Скачайте пакет для своего дистрибутива из
[последнего релиза](https://github.com/AprilNEA/OpenLogi/releases/latest):

```sh
# Debian / Ubuntu
sudo dpkg -i openlogi_*.deb

# Fedora / RHEL
sudo rpm -i openlogi-*.rpm

# Arch Linux
sudo pacman -U openlogi-*.pkg.tar.zst
```

Пакеты публикуются для `x86_64`/`amd64` и `arm64`/`aarch64`.
Готовые пакеты требуют GLIBC 2.35 или новее (базовая линия Ubuntu 22.04).

Пользователи NixOS могут подключить модуль репозитория: он ставит пакет и правила udev и запускает агент вместе с графической сессией:

```nix
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.openlogi = {
    url = "github:AprilNEA/OpenLogi";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { nixpkgs, openlogi, ... }: {
    nixosConfigurations.my-host = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux"; # or aarch64-linux
      modules = [
        openlogi.nixosModules.default
        { programs.openlogi.enable = true; }
      ];
    };
  };
}
```

Все Linux-пакеты ставят правила udev, которые дают вашему пользователю доступ к
`/dev/hidraw*`, `/dev/uinput` и узлу `/dev/input/event*` мыши Logitech
без `sudo`. Модуль NixOS запускает агент сам; после установки `.deb`, `.rpm`
или `.pkg.tar.zst` включите его для своего пользователя:

```sh
systemctl --user enable --now openlogi-agent.service
```

Полные опции NixOS, ручная / source-установка и дистрибутивы без systemd —
в [INSTALL-linux.md](INSTALL-linux.md).

### Windows

К каждому релизу прилагаются подписанные портативные `.zip` и пользовательские
установщики `.msi` (x86_64 и arm64). И то и другое включает GUI (`OpenLogi.exe`)
и фоновый агент (`openlogi-agent.exe`), который владеет всем I/O устройств.
В портативном zip держите оба файла рядом, иначе GUI не к чему подключаться.

Поддержка Windows проверена end-to-end на Windows 11 с живым железом
(проводная клавиатура и мышь на Unifying-приёмнике), включая установку,
in-place обновление и удаление MSI. Сборка новее macOS, поэтому если
натолкнётесь на шероховатость — [сообщите](https://github.com/AprilNEA/OpenLogi/issues).
Агент показывает иконку в системном трее (Show Main Window / Quit), чтобы
приложение оставалось доступным после закрытия главного окна. Чтобы отключить
её на Windows, задайте `show_in_menu_bar = false` в блоке TOML `[app_settings]`
и перезапустите агент; переключатель в GUI пока только на macOS.

Сборка из исходников: [DEVELOPMENT.md](DEVELOPMENT.md).


## Использование (CLI)

См. [USAGE.md](USAGE.md)

## Конфигурация

См. [CONFIGURATION.md](CONFIGURATION.md)

## Разработка

См. [DEVELOPMENT.md](DEVELOPMENT.md)

## Благодарности

- **Windows, камеры и i18n** — [@davidbudnick](https://github.com/davidbudnick): RGB клавиатуры, поддержка Windows, веб-камеры Logitech
- **Порт на Linux** — [@cserby](https://github.com/cserby)
- [Solaar](https://github.com/pwr-Solaar/Solaar) от [@pwr](https://github.com/pwr) — открытая реализация HID++
- [Mouser](https://github.com/TomBadash/Mouser) от [@TomBadash](https://github.com/TomBadash) — локальная замена Options+ без аккаунта

## Лицензия

Код в этом репозитории распространяется по двойной лицензии — на выбор:

- Apache License, Version 2.0 ([LICENSE-APACHE](../LICENSE-APACHE))
- MIT license ([LICENSE-MIT](../LICENSE-MIT))

### Сторонний код

`crates/openlogi-hidpp` — вендорский форк [`hidpp`](https://crates.io/crates/hidpp)
от [@lus](https://github.com/lus), лицензия 0BSD.

### Логотип и бренд

Спасибо [@kubai087](https://github.com/kubai087) за дизайн логотипа OpenLogi.
Логотип и иконка приложения (бренд-ассеты в
[`design/`](../design/)) © 2026 AprilNEA, все права защищены, и не покрываются
лицензиями MIT/Apache выше; см. [`design/LICENSE`](../design/LICENSE). Форк кода
не даёт прав на имя, логотип или иконку OpenLogi; не используйте их для своих
проектов, форков и дистрибутивов без предварительного письменного разрешения.

---

**Не связан с Logitech.** «Logitech», «MX Master» и «Options+» — товарные знаки Logitech International S.A.

## Активность репозитория

![Repobeats analytics image](https://repobeats.axiom.co/api/embed/4a0b576a03e9d528ad31ccf4797a1286c045d021.svg "Repobeats analytics image")
