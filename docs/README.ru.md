> [!WARNING]
> **OpenLogi активно разрабатывается** и ещё не стабилен — функции и конфигурация могут меняться. Поставьте репозиторию **Star** ⭐ и **Watch** 👀, чтобы узнать о новом релизе.

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Нативная local-first альтернатива Logitech Options+, написанная на Rust 🦀<br/>Полные возможности мышей, клавиатур и веб-камер Logitech по HID++ и UVC</strong></p>

<div align="center">
    <a href="https://github.com/AprilNEA/OpenLogi/releases/latest" target="_blank">
    <img alt="GitHub release" src="https://img.shields.io/github/v/release/AprilNEA/OpenLogi?style=flat"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/releases" target="_blank">
    <img alt="GitHub downloads" src="https://img.shields.io/github/downloads/AprilNEA/OpenLogi/total.svg?style=flat"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/commits" target="_blank">
    <img alt="GitHub commit" src="https://img.shields.io/github/commit-activity/m/AprilNEA/OpenLogi?style=flat"></a>
    <a href="#лицензия">
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
    <img alt="Français" src="https://img.shields.io/badge/Fran%C3%A7ais-lightgrey?style=flat"></a>
    <a href="README.ko.md">
    <img alt="한국어" src="assets/badge-lang-ko.svg"></a>
    <a href="README.ru.md">
    <img alt="Русский" src="https://img.shields.io/badge/%D0%A0%D1%83%D1%81%D1%81%D0%BA%D0%B8%D0%B9-blue?style=flat"></a>
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

> **Надоел Options+? Попробуйте OpenLogi.**

Работает на macOS, Linux и Windows.

---

## Больше, чем Options+

То, что OpenLogi умеет, а Options+ — нет:

- **Лёгкий.** Нативный Rust + GPUI.
- **Linux.** В OpenLogi Linux — платформа первого класса.
- **Жесты на поддерживаемых кнопках.** Назначайте действия жестам на поддерживаемых элементах управления — или отключите жесты совсем.
- **Конфиг обычным текстом.** Всё в одном TOML-файле, который можно синхронизировать между машинами как угодно.
- **Скрипты.** Настоящий CLI рядом с GUI: JSON-[API автоматизации](USAGE.md#automation-api) для DPI, SmartShift, Fn-lock и частоты опроса, плюс диагностика железа.

## Возможности

<p align="center">
    <img src="https://assets.openlogi.org/screenshots/macos/light/devices-grid.png" width="860" alt="Список устройств OpenLogi: шесть устройств Logitech — мыши, клавиатура и светильник Litra">
</p>

### Устройства

- Приёмники Logi Bolt и Unifying, Bluetooth и провод, с уровнем заряда батареи и состоянием зарядки
- Предупреждение о низком заряде внизу экрана — один раз за разряд и без системного разрешения на уведомления
- Экранный индикатор, когда клавиатура включает или выключает Caps Lock, а на Windows и Linux — ещё Num Lock и Scroll Lock

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/easy-switch.png" width="400" alt="Карточка Easy-Switch со списком трёх сопряжённых компьютеров"></td>
        <td width="50%"><b>Easy-Switch</b><ul><li>Какой компьютер сопряжён на каждом канале, с его ОС и типом подключения</li><li>Переименование канала на устройствах, которые это позволяют</li></ul></td>
    </tr>
</table>

### Кнопки и действия

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/buttons.png" width="400" alt="Назначение кнопок MX Master 4 с открытым инспектором кнопки жестов"></td>
        <td width="50%"><b>Переназначение любой кнопки</b><ul><li>Встроенный каталог действий, свои сочетания клавиш, набор текста или многошаговые сценарии</li><li>Отдельные действия на нажатие и удержание, а также аккорды push-to-talk, которые отпускаются вместе с кнопкой</li><li>Жесты на средней кнопке, Назад/Вперёд, DPI/ModeShift, кнопке жестов и тактильной панели</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/actions-ring.png" width="400" alt="Редактор Actions Ring с восемью слотами вокруг курсора"></td>
        <td width="50%"><b>Actions Ring</b><ul><li>Восемь действий кольцом вокруг курсора</li><li>Своя раскладка для каждого приложения</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/keys.png" width="400" alt="MX Keys S с тремя клавишами, назначенными на Launchpad, блокировку экрана и снимок экрана"></td>
        <td width="50%"><b>Клавиши клавиатуры</b><ul><li>Медиаклавиши, эмодзи, снимок экрана и другие выделенные клавиши назначаются так же, как кнопки мыши</li><li>Сброс одной клавиши к значению устройства по умолчанию или всего ряда разом</li></ul></td>
    </tr>
</table>

Профили по приложениям переключаются вместе с приложением в фокусе. Псевдоним в `[apps]` позволяет одному профилю следовать за приложением на разных платформах.

### Указатель и прокрутка

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/dpi.png" width="400" alt="Карточка настройки указателя с ползунком DPI"></td>
        <td width="50%"><b>Указатель</b><ul><li>Пресеты DPI и действия Cycle / Set-preset</li><li>Скорость указателя и частота опроса</li><li>Режим точности: одна кнопка переключает мышь на самую низкую скорость и обратно</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/smartshift.png" width="400" alt="Карточка SmartShift с режимом колеса, чувствительностью и усилием прокрутки"></td>
        <td width="50%"><b>SmartShift</b><ul><li>Режим колеса и чувствительность</li><li>Усилие прокрутки на мышах с SmartShift Enhanced</li><li>Переключатель постоянной трещотки</li></ul></td>
    </tr>
    <tr>
        <td colspan="2"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/scrolling.png" width="820" alt="Карточка прокрутки с инверсией, разрешением колеса и плавным колесом"><br><b>Прокрутка</b><ul><li>Инверсия прокрутки на устройство — нативно или программно</li><li>Разрешение колеса и плавная прокрутка колесом</li><li>Удерживайте Назад или Вперёд и вращайте колесо, чтобы прокручивать по горизонтали на мышах без колеса под большой палец</li></ul></td>
    </tr>
</table>

### Клавиатура и подсветка

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/keys-backlight.png" width="400" alt="Карточки «Клавиши» и «Подсветка» для MX Keys S"></td>
        <td width="50%"><b>Клавиатура</b><ul><li>Fn Lock и мультиплатформенные клавиатуры, которые остаются на раскладке ОС хоста</li><li>Уровень подсветки, автояркость, экономия заряда и время подсветки клавиш</li><li>Уровень подсветки показывается на экране при изменении</li><li>Статическая RGB-подсветка на поддерживаемых клавиатурах</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/litra.png" width="400" alt="Панель освещения Litra Glow с яркостью и цветовой температурой"></td>
        <td width="50%"><b>Светильники Litra</b><ul><li>Питание, яркость и цветовая температура</li><li>При желании включаются вместе с камерой</li><li>Изменения, сделанные на самом светильнике, сохраняются</li></ul></td>
    </tr>
</table>

### Веб-камеры

- Любая Logitech UVC-веб-камера (Brio, StreamCam, серия C920, …), plug and play
- Живой превью держит камеру только пока вы смотрите; закройте его — и LED погаснет
- Масштаб, панорама, наклон, фокус, экспозиция, баланс белого и все остальные UVC-параметры пишутся в железо, поэтому изменения видят Meet, Zoom и OBS
- Встроенные профили Default / Streaming / Video call плюс свои снимки, отдельно для каждой камеры

Набор функций зависит от того, что сообщает каждое устройство; приложение показывает только те элементы управления, которые устройство поддерживает.

### Поддержка платформ

| Функция | macOS | Windows | Linux |
| --- | --- | --- | --- |
| Переназначение кнопок мыши и жесты | ✓ | ✓ | ✓ |
| Переназначение F-клавиш клавиатуры | ✓ | ✓ | — |
| Профили по приложениям | ✓ | ✓ | X11, wlroots (sway, Hyprland), GNOME с [расширением-компаньоном](../crates/openlogi-hook/gnome-shell-extension/README.md) |
| Профиль мыши следует за окном под указателем | ✓ | ✓ | Только X11 |
| Медиаклавиши | Нативно | Нативно | D-Bus MPRIS |
| Действия оконного менеджера (рабочие столы, Mission Control, …) | Нативно | Нативные эквиваленты | GNOME, KDE (KWin), Hyprland; часть действий только для macOS ничего не делает |
| Иконка в строке меню / трее | ✓ | ✓ | — |
| Обновления в приложении | Sparkle | Встроенные | Встроенные |

## Установка

> [!IMPORTANT]
> Сначала закройте **Logi Options+** (а в Linux — и Solaar). Приёмник может принадлежать только одному приложению одновременно.

| Платформа | Система | Архитектура |
| --- | --- | --- |
| macOS | macOS 13 или новее | Apple silicon, Intel |
| Linux | GLIBC 2.35 или новее | x86_64, arm64 |
| Windows | Проверено на Windows 11 | x86_64, arm64 |

Каждый файл подписан minisign и указан в `SHA256SUMS`. Сборка из исходников: [DEVELOPMENT.md](DEVELOPMENT.md).

### macOS

<p>
    <a href="https://openlogi.org/download/mac-arm64"><img src="https://assets.openlogi.org/brand/buttons/download-macos-arm64.png" width="260" alt="Скачать для Mac: Apple silicon (.dmg)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/mac-x64"><img src="https://assets.openlogi.org/brand/buttons/download-macos-x64.png" width="260" alt="Скачать для Mac: Intel (.dmg)"></a>
</p>

Или через Homebrew:

```sh
brew install --cask openlogi
```

Откройте OpenLogi и выдайте разрешения **Универсальный доступ** и **Мониторинг ввода**, когда macOS их запросит. Дальше OpenLogi обновляется сам через Sparkle.

### Linux

<p>
    <a href="https://openlogi.org/download/linux"><img src="https://assets.openlogi.org/brand/buttons/download-linux-deb.png" width="260" alt="Скачать для Linux: Debian, Ubuntu (.deb)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/linux-rpm"><img src="https://assets.openlogi.org/brand/buttons/download-linux-rpm.png" width="260" alt="Скачать для Linux: Fedora, SUSE (.rpm)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/linux-arch"><img src="https://assets.openlogi.org/brand/buttons/download-linux-arch.png" width="260" alt="Скачать для Linux: Arch (.pkg.tar.zst)"></a>
</p>

Или установите подходящий для вашего дистрибутива пакет одной командой. Скрипт проверяет подпись minisign и запускает агент; сначала установите `minisign`:

```sh
curl -fsSL https://github.com/AprilNEA/OpenLogi/releases/latest/download/install.sh | sh
```

В Arch Linux пакет AUR [`openlogi`](https://aur.archlinux.org/packages/openlogi) собирает релиз из исходного кода:

```sh
yay -S openlogi
```

Как просмотреть скрипт перед запуском, а также пакет AUR, Flatpak, NixOS и дистрибутивы без systemd — в [INSTALL-linux.md](INSTALL-linux.md).

### Windows

<p>
    <a href="https://openlogi.org/download/windows"><img src="https://assets.openlogi.org/brand/buttons/download-windows-msi.png" width="260" alt="Скачать для Windows: установщик (.msi)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/windows-zip"><img src="https://assets.openlogi.org/brand/buttons/download-windows-zip.png" width="260" alt="Скачать для Windows: портативная версия (.zip)"></a>
</p>

## Использование (CLI)

См. [USAGE.md](USAGE.md)

## Конфигурация

Настройки хранятся в обычном TOML; при сохранении конфиги-симлинки остаются симлинками. См. [CONFIGURATION.md](CONFIGURATION.md).

## Разработка

См. [DEVELOPMENT.md](DEVELOPMENT.md), включая [безопасность input hook на macOS](DEVELOPMENT.md#macos-input-hook-safety).

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
