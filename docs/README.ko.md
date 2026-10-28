> [!WARNING]
> **OpenLogi는 활발히 개발 중**이며 아직 안정 단계가 아닙니다 — 기능과 설정이 변경될 수 있습니다. 저장소에 **Star** ⭐ 와 **Watch** 👀 를 눌러 두면 새 릴리스가 나올 때 알림을 받을 수 있습니다.

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Rust로 작성된 네이티브 로컬 우선 Logitech Options+ 대안 🦀<br/>HID++와 UVC로 Logitech 마우스·키보드·웹캠의 모든 기능을 끌어냅니다</strong></p>


<div align="center">
    <a href="https://github.com/AprilNEA/OpenLogi/releases/latest" target="_blank">
    <img alt="GitHub release" src="https://img.shields.io/github/v/release/AprilNEA/OpenLogi?style=flat"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/releases" target="_blank">
    <img alt="GitHub downloads" src="https://img.shields.io/github/downloads/AprilNEA/OpenLogi/total.svg?style=flat"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/commits" target="_blank">
    <img alt="GitHub commit" src="https://img.shields.io/github/commit-activity/m/AprilNEA/OpenLogi?style=flat"></a>
    <a href="#라이선스">
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
    <img alt="한국어" src="assets/badge-lang-ko-current.svg"></a>
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

> **Options+가 지긋지긋하다면? OpenLogi를 써 보세요.**

macOS, Linux, Windows를 지원합니다.

---

## Options+ 그 너머

OpenLogi는 되고 Options+는 안 되는 것들:

- **가볍게 유지.** 네이티브 Rust + GPUI.
- **Linux 지원.** Linux는 OpenLogi의 일급 플랫폼입니다.
- **지원되는 버튼에서 제스처 사용.** 지원되는 컨트롤에 제스처 동작을 지정하거나, 제스처를 아예 끌 수 있습니다.
- **순수 텍스트 설정.** 모든 설정이 TOML 파일 하나에 담겨 원하는 방법으로 기기 간 동기화할 수 있습니다.
- **스크립트 가능.** GUI 외에 진짜 CLI도 제공합니다. DPI, SmartShift, Fn Lock, 보고율을 다루는 JSON [자동화 API](USAGE.md#automation-api)와 하드웨어 진단 기능을 갖추고 있습니다.

## 기능 목록

<p align="center">
    <img src="https://assets.openlogi.org/screenshots/macos/light/devices-grid.png" width="860" alt="마우스, 키보드, Litra 조명 등 Logitech 기기 6대가 표시된 OpenLogi 기기 목록">
</p>

### 기기

- Logi Bolt 수신기, Unifying 수신기, Bluetooth, 유선 연결을 지원하며 배터리 잔량과 충전 상태 표시
- 배터리 부족 알림을 화면 하단에 방전당 한 번만 표시하며, OS 알림 권한은 필요 없음
- 키보드에서 Caps Lock을 켜거나 끄면 화면에 표시기를 띄움. Windows와 Linux에서는 Num Lock과 Scroll Lock도 지원

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/easy-switch.png" width="400" alt="페어링된 컴퓨터 3대를 보여 주는 Easy-Switch 카드"></td>
        <td width="50%"><b>Easy-Switch</b><ul><li>채널마다 페어링된 컴퓨터를 OS, 연결 방식과 함께 확인</li><li>지원 기기에서는 채널 이름 변경 가능</li></ul></td>
    </tr>
</table>

### 버튼과 액션

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/buttons.png" width="400" alt="제스처 버튼 인스펙터가 열린 MX Master 4 버튼 매핑 화면"></td>
        <td width="50%"><b>모든 버튼 리매핑</b><ul><li>내장 액션 카탈로그, 사용자 지정 단축키, 텍스트 입력, 다단계 워크플로</li><li>탭과 길게 누르기에 각각 다른 액션, 버튼을 놓으면 해제되는 누르고 말하기용 키 조합</li><li>가운데 클릭, 뒤로/앞으로, DPI/모드 시프트, 제스처 버튼, 햅틱 패널에서 제스처 사용</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/actions-ring.png" width="400" alt="커서 주위에 슬롯 8개가 배치된 Actions Ring 편집기"></td>
        <td width="50%"><b>Actions Ring</b><ul><li>커서 주위에 링 형태로 배치되는 액션 8개</li><li>앱마다 별도 레이아웃</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/keys.png" width="400" alt="키 3개를 Launchpad, 화면 잠금, 스크린샷에 지정한 MX Keys S"></td>
        <td width="50%"><b>키보드 키</b><ul><li>미디어, 이모지, 스크린샷 등 전용 키를 마우스 버튼처럼 지정</li><li>키 하나만 기기 기본값으로 되돌리거나 행 전체를 재설정</li></ul></td>
    </tr>
</table>

앱별 프로필은 포커스된 앱에 따라 전환됩니다. `[apps]` 별칭을 쓰면 프로필 하나가 여러 플랫폼에서 같은 앱을 따라갑니다.

### 포인터와 스크롤

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/dpi.png" width="400" alt="DPI 슬라이더가 있는 포인터 조정 카드"></td>
        <td width="50%"><b>포인터</b><ul><li>DPI 프리셋과 사이클 / 프리셋 지정 액션</li><li>포인터 속도와 보고율</li><li>정밀 모드: 버튼 하나로 마우스를 가장 느린 속도로 바꾸고 다시 원래대로 복귀</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/smartshift.png" width="400" alt="휠 모드, 민감도, 스크롤 압력을 설정하는 SmartShift 카드"></td>
        <td width="50%"><b>SmartShift</b><ul><li>휠 모드와 민감도</li><li>SmartShift Enhanced 마우스의 스크롤 압력</li><li>래칫 고정 스위치</li></ul></td>
    </tr>
    <tr>
        <td colspan="2"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/scrolling.png" width="820" alt="스크롤 반전, 휠 해상도, 부드러운 휠을 설정하는 스크롤 카드"><br><b>스크롤</b><ul><li>기기별 스크롤 반전(네이티브 또는 소프트웨어)</li><li>휠 해상도와 부드러운 휠 스크롤</li><li>썸휠이 없는 마우스에서도 뒤로 또는 앞으로 버튼을 누른 채 휠을 굴리면 가로 스크롤</li></ul></td>
    </tr>
</table>

### 키보드와 조명

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/keys-backlight.png" width="400" alt="MX Keys S의 키 카드와 백라이트 카드"></td>
        <td width="50%"><b>키보드</b><ul><li>Fn Lock, 그리고 호스트 OS 레이아웃을 유지하는 멀티 플랫폼 키보드</li><li>백라이트 밝기, 자동 밝기, 배터리 절약, 키 조명 유지 시간</li><li>백라이트 밝기를 바꿀 때 화면에 표시</li><li>지원 키보드의 정적 RGB</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/litra.png" width="400" alt="밝기와 색온도를 설정하는 Litra Glow 조명 패널"></td>
        <td width="50%"><b>Litra 조명</b><ul><li>전원, 밝기, 색온도</li><li>원하면 카메라 사용에 맞춰 자동으로 켜기</li><li>조명 본체에서 바꾼 설정을 유지</li></ul></td>
    </tr>
</table>

### 웹캠

- 모든 Logitech UVC 웹캠(Brio, StreamCam, C920 시리즈 등) 플러그 앤 플레이 지원
- 실시간 미리보기는 보고 있는 동안에만 카메라를 사용하며, 벗어나면 LED도 꺼집니다
- 줌, 팬, 틸트, 초점, 노출, 화이트 밸런스 등 모든 UVC 컨트롤을 하드웨어에 기록하므로 Meet, Zoom, OBS에도 변경 사항이 적용됩니다
- 기본값 / 스트리밍 / 영상 통화 내장 프로필과 사용자 스냅숏을 카메라별로 저장

지원 기능은 각 기기가 보고하는 내용에 따라 다르며, 앱은 기기가 지원하는 컨트롤만 표시합니다.

### 플랫폼 지원

| 기능 | macOS | Windows | Linux |
| --- | --- | --- | --- |
| 마우스 버튼 리매핑과 제스처 | ✓ | ✓ | ✓ |
| 키보드 F 키 리매핑 | ✓ | ✓ | — |
| 앱별 프로필 | ✓ | ✓ | X11, wlroots(sway, Hyprland), GNOME([동반 확장](../crates/openlogi-hook/gnome-shell-extension/README.md) 필요) |
| 포인터 아래 창을 따라가는 마우스 프로필 | ✓ | ✓ | X11 전용 |
| 미디어 키 | 네이티브 | 네이티브 | D-Bus MPRIS |
| 창 관리자 액션(데스크톱, Mission Control 등) | 네이티브 | 네이티브 대응 기능 | GNOME, KDE (KWin), Hyprland; 일부 macOS 전용 액션은 동작하지 않음 |
| 메뉴 막대 / 트레이 아이콘 | ✓ | ✓ | — |
| 앱 내 업데이트 | Sparkle | 내장 | 내장 |

## 설치

> [!IMPORTANT]
> 먼저 **Logi Options+**(Linux에서는 Solaar도)를 종료하세요. 하나의 수신기는 한 번에 하나의 애플리케이션만 소유할 수 있습니다.

| 플랫폼 | 시스템 | 아키텍처 |
| --- | --- | --- |
| macOS | macOS 13 이상 | Apple 실리콘, Intel |
| Linux | GLIBC 2.35 이상 | x86_64, arm64 |
| Windows | Windows 11에서 테스트됨 | x86_64, arm64 |

모든 다운로드에는 minisign 서명과 `SHA256SUMS` 항목이 함께 제공됩니다. 소스에서 빌드하려면 [DEVELOPMENT.md](DEVELOPMENT.md)를 참고하세요.

### macOS

<p>
    <a href="https://openlogi.org/download/mac-arm64"><img src="https://assets.openlogi.org/brand/buttons/download-macos-arm64.png" width="260" alt="Mac용 다운로드: Apple silicon (.dmg)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/mac-x64"><img src="https://assets.openlogi.org/brand/buttons/download-macos-x64.png" width="260" alt="Mac용 다운로드: Intel (.dmg)"></a>
</p>

또는 Homebrew로 설치:

```sh
brew install --cask openlogi
```

OpenLogi를 열고 macOS가 요청하면 **손쉬운 사용**과 **입력 모니터링** 권한을 허용하세요. 이후 OpenLogi는 Sparkle로 스스로 업데이트합니다.

### Linux

<p>
    <a href="https://openlogi.org/download/linux"><img src="https://assets.openlogi.org/brand/buttons/download-linux-deb.png" width="260" alt="Linux용 다운로드: Debian, Ubuntu (.deb)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/linux-rpm"><img src="https://assets.openlogi.org/brand/buttons/download-linux-rpm.png" width="260" alt="Linux용 다운로드: Fedora, SUSE (.rpm)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/linux-arch"><img src="https://assets.openlogi.org/brand/buttons/download-linux-arch.png" width="260" alt="Linux용 다운로드: Arch (.pkg.tar.zst)"></a>
</p>

또는 명령 하나로 배포판에 맞는 패키지를 설치할 수 있습니다. 스크립트는 minisign 서명을 검증하고 agent를 시작합니다. 먼저 `minisign`을 설치하세요:

```sh
curl -fsSL https://github.com/AprilNEA/OpenLogi/releases/latest/download/install.sh | sh
```

Arch Linux에서는 AUR 패키지 [`openlogi`](https://aur.archlinux.org/packages/openlogi)가 릴리스를 소스에서 빌드합니다:

```sh
yay -S openlogi
```

실행 전에 스크립트를 확인하는 방법과 AUR 패키지, Flatpak, NixOS, systemd가 없는 배포판에 대해서는 [INSTALL-linux.md](INSTALL-linux.md)를 참고하세요.

### Windows

<p>
    <a href="https://openlogi.org/download/windows"><img src="https://assets.openlogi.org/brand/buttons/download-windows-msi.png" width="260" alt="Windows용 다운로드: 설치 프로그램 (.msi)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/windows-zip"><img src="https://assets.openlogi.org/brand/buttons/download-windows-zip.png" width="260" alt="Windows용 다운로드: 휴대용 (.zip)"></a>
</p>

## 사용법 (CLI)

[USAGE.md](USAGE.md) 참고

## 설정

설정은 순수 TOML을 사용하며, 저장할 때 심볼릭 링크된 설정 파일을 그대로 유지합니다. [CONFIGURATION.md](CONFIGURATION.md) 참고.

## 개발

[macOS 입력 훅 안전성](DEVELOPMENT.md#macos-input-hook-safety)을 포함해 [DEVELOPMENT.md](DEVELOPMENT.md) 참고.

## 감사의 말

- **Windows·카메라·i18n** — [@davidbudnick](https://github.com/davidbudnick): 키보드 RGB 지원, Windows 지원, Logitech 웹캠 지원
- **Linux 포팅** — [@cserby](https://github.com/cserby): Linux 지원
- [Solaar](https://github.com/pwr-Solaar/Solaar) — [@pwr](https://github.com/pwr): 오픈소스 HID++ 구현
- [Mouser](https://github.com/TomBadash/Mouser) — [@TomBadash](https://github.com/TomBadash): 로컬에서 동작하는 계정 없는 Options+ 대체제

## 라이선스

이 저장소의 코드는 다음 중 하나를 선택해 사용할 수 있습니다:

- Apache License 2.0 ([LICENSE-APACHE](../LICENSE-APACHE))
- MIT 라이선스 ([LICENSE-MIT](../LICENSE-MIT))

### 서드파티 코드

`crates/openlogi-hidpp`는 [`hidpp`](https://crates.io/crates/hidpp)([@lus](https://github.com/lus) 제작)의 vendored fork이며, 0BSD 라이선스를 따릅니다.

### 로고 및 브랜드 자산

OpenLogi 로고를 디자인해 준 [@kubai087](https://github.com/kubai087)에게 감사드립니다. OpenLogi 로고와 앱 아이콘 — [`design/`](../design/) 아래의 브랜드 자산 — 은 © 2026 AprilNEA가 모든 권리를 보유하며, 위 MIT/Apache 라이선스의 적용을 받지 않습니다. [`design/LICENSE`](../design/LICENSE)를 참고하세요. 코드를 포크해도 OpenLogi 이름·로고·아이콘에 대한 권리는 부여되지 않습니다. 사전 서면 허가 없이 자신의 프로젝트, 포크, 배포판을 나타내는 데 사용하지 마세요.

---

**Logitech과 무관합니다.** "Logitech", "MX Master", "Options+"는 Logitech International S.A.의 상표입니다.

## 저장소 활동

![Repobeats analytics image](https://repobeats.axiom.co/api/embed/4a0b576a03e9d528ad31ccf4797a1286c045d021.svg "Repobeats analytics image")
