> [!WARNING]
> **OpenLogi는 활발히 개발 중**이며 아직 안정 단계가 아닙니다 — 기능과 설정이 변경될 수 있습니다. 저장소에 **Star** ⭐ 와 **Watch** 👀 를 눌러 두면 릴리스가 나오는 즉시 알림을 받을 수 있습니다.

<h4 align="right"><a href="../README.md">English</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.ja.md">日本語</a> | <a href="README.de.md">Deutsch</a> | <a href="README.fr.md">Français</a> | <strong>한국어</strong></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Rust로 작성된 네이티브 로컬 우선 Logitech Options+ 대안 🦀<br/>HID++로 버튼·DPI·SmartShift를 리매핑하세요. 계정도, 텔레메트리도 없습니다.</strong></p>


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

> **Options+가 지긋지긋하다면? OpenLogi를 써 보세요.**

Logitech 계정도, 텔레메트리도, 공식 Options+ 설치도 없이 버튼을 리매핑하고 DPI와 SmartShift를 제어하며 앱별 프로필을 전환할 수 있습니다. 클라우드 없이 순수 TOML 설정 파일만 사용합니다. 네트워크 요청은 기기 이미지 다운로드와 기본 비활성화된 옵트인 업데이트 확인뿐입니다.

---

## 소개

OpenLogi는 Logi Bolt 수신기 — 또는 Bluetooth 직접 연결 / 유선 연결 — 을 통해 Logitech HID++ 마우스와 통신하며, Logi Options+를 실행할 필요가 없습니다. 두 개의 실행 파일을 제공합니다:

- **[OpenLogi GUI](../crates/openlogi-gui)** — GPUI 데스크톱 앱: 클릭 가능한 핫스팟이 있는 인터랙티브 마우스 다이어그램, 버튼별 액션 선택기(내장 액션 41개 + TOML 설정에 직접 작성하는 사용자 지정 단축키), DPI 프리셋, SmartShift 패널(휠 모드·감도·영구 래칫), 앱별 프로필 오버레이, 페어링된 기기를 실시간 전환하는 기기 캐러셀, 그리고 20개 언어로 현지화된 설정 창.
- **[OpenLogi CLI](../crates/openlogi-cli)** — 헤드리스 기기 목록(`list`), 에셋 동기화, 기기 진단 하위 명령을 갖춘 CLI.

모든 것이 로컬에서 이루어집니다: 바인딩은 순수 TOML 파일에 저장되고, 버튼 입력은 OS 이벤트 훅으로 리매핑되며, DPI / SmartShift 변경은 HID++를 통해 기기에 직접 기록됩니다.

macOS와 Linux를 지원합니다. Windows는 검증되지 않은 초기 프리뷰로, 각 릴리스에 서명된 빌드가 포함됩니다 — [로드맵](#로드맵)을 참고하세요.

## Options+ 그 너머

OpenLogi는 되고 Options+는 안 되는 것들:

- **Linux에서 실행.** Options+는 macOS와 Windows 전용입니다. OpenLogi는 Linux를 일급 플랫폼으로 다룹니다: evdev/uinput 훅, udev 규칙, systemd 사용자 유닛, `.deb` / `.rpm` 패키지.
- **제스처 버튼 이동.** 어떤 물리 버튼이 제스처 역할을 맡을지 — 썸 패드, 가운데, 뒤로, 앞으로 — 직접 고를 수 있고, 방향별 스와이프 바인딩을 설정하거나 제스처를 아예 끌 수도 있습니다. Options+는 제스처 역할을 전용 썸 패드에 고정합니다.
- **순수 텍스트 설정.** 모든 설정이 TOML 파일 하나에 들어 있어 읽고, diff하고, 버전 관리하고, 다른 기기로 복사할 수 있습니다.
- **스크립트 가능.** 진짜 CLI: 기기 목록, 에셋 프리페치, 기기 내 HID++ 진단(피처 덤프, DPI / SmartShift 왕복 검사).
- **가볍게 유지.** 네이티브 Rust + GPUI 바이너리 — Electron 스위트도, 상주 업데이터도, 계정도, 텔레메트리도 없습니다.

## 로드맵

| 기능 | 상태 |
|---|---|
| Bolt 수신기 탐색 + 페어링된 기기 목록(CLI + GUI) | ✅ |
| Unifying 수신기(Bolt로 대체된 구형 프로토콜) | ✅ |
| Bluetooth 직접 연결 / 유선 기기(수신기 없음) | ✅ |
| 배터리 잔량 / 충전 상태 | ✅ (온라인 기기) |
| 인터랙티브 GUI: 캐러셀, 마우스 다이어그램, 액션 선택기 | ✅ macOS + Linux |
| OS 이벤트 훅 / evdev를 통한 버튼 리매핑 | ✅ macOS + Linux |
| 41개 액션 카탈로그 + 사용자 지정 키보드 단축키(TOML 직접 작성) | ✅ macOS + Linux¹ |
| DPI 제어 + 프리셋 + 사이클 / 프리셋 지정 액션(HID++ `0x2201`) | ✅ |
| SmartShift 휠: 모드 전환 + 감도 + 영구 래칫 패널(HID++ `0x2111`) | ✅ |
| 앱별 프로필 오버레이(앱 포커스 시 자동 전환) | ✅ macOS, 🟡 Linux (X11 전용) |
| 설정 창: 로그인 시 실행, 업데이트 확인, 메뉴 막대, 권한, 언어 | ✅ macOS + Linux |
| 인터페이스 현지화(20개 언어: da, de, el, en, es, fi, fr, it, ja, ko, nb, nl, pl, pt-BR, pt-PT, ru, sv, zh-CN, zh-HK, zh-TW) | ✅ |
| Linux 패키징: udev 규칙, systemd 유닛, `.deb` / `.rpm` | ✅ Linux |
| 제스처 버튼 방향별 바인딩 | 🟡 설정 가능; 하드웨어 캡처는 개발 중 |
| 가운데 / 모드 시프트 / 썸휠 버튼 캡처 | 🟡 설정 가능; 훅은 현재 사이드 버튼만 담당 |
| Windows(에이전트, GUI, 이벤트 훅) | 🟡 미검증 프리뷰 — 각 릴리스에 서명된 `.exe` / `.msi` 포함 |

¹ Linux의 미디어 키 액션은 D-Bus MPRIS를 사용합니다. 일부 macOS 전용 액션(예: Launchpad)은 Linux에 대응 기능이 없어 아무 동작도 하지 않습니다.

## 설치

> [!IMPORTANT]
> 먼저 **Logi Options+** 를 종료하세요 — 두 애플리케이션은 HID++ 접근을 두고 경합하며, 하나의 수신기는 한쪽만 소유할 수 있습니다.

### macOS

[최신 릴리스](https://github.com/AprilNEA/OpenLogi/releases/latest)에서 서명·공증된 `.dmg`를 내려받아 `OpenLogi.app`을 `/Applications`로 드래그하세요.

또는 [Homebrew](https://brew.sh)로 설치:

```sh
brew install --cask openlogi
```

공식 Homebrew cask가 기본 설치 경로입니다. 대신 `aprilnea/tap`으로 GitHub 최신 릴리스를 명시적으로 따라가려면:

```sh
brew tap aprilnea/tap
brew install --cask aprilnea/tap/openlogi@latest
```

`openlogi@latest`는 OpenLogi 릴리스 워크플로가 관리하며 공식 cask의 autobump보다 먼저 갱신될 수 있습니다. `openlogi`와 `openlogi@latest` 중 하나만 설치하세요.

### Linux

[최신 릴리스](https://github.com/AprilNEA/OpenLogi/releases/latest)에서 `.deb` 또는 `.rpm`을 내려받으세요:

```sh
# Debian / Ubuntu
sudo dpkg -i openlogi_*.deb

# Fedora / RHEL
sudo rpm -i openlogi-*.rpm
```

패키지는 `x86_64`/`amd64`와 `arm64`/`aarch64` 두 아키텍처로 제공됩니다.

패키지는 `sudo` 없이 `/dev/hidraw*`와 `/dev/uinput`에 접근할 수 있게 해 주는 udev 규칙을 설치합니다. 설치 후 사용자용 백그라운드 에이전트를 활성화하세요:

```sh
systemctl --user enable --now openlogi-agent.service
```

수동 / 소스 설치와 systemd가 없는 배포판은 [INSTALL-linux.md](INSTALL-linux.md)를 참고하세요.

### Windows (프리뷰)

각 릴리스에 서명된 `.exe`와 사용자별 `.msi` 설치 파일(x86_64 및 arm64)이 포함됩니다. Windows 지원은 실제 하드웨어에서 충분히 검증되지 않은 초기 프리뷰입니다 — 다듬어지지 않은 부분이 있을 수 있으니 [이슈로 제보](https://github.com/AprilNEA/OpenLogi/issues)해 주세요.

소스에서 빌드하려면 [DEVELOPMENT.md](DEVELOPMENT.md)를 참고하세요.


## 사용법 (CLI)

[USAGE.md](USAGE.md) 참고

## 설정

[CONFIGURATION.md](CONFIGURATION.md) 참고

## 개발

[DEVELOPMENT.md](DEVELOPMENT.md) 참고

## 감사의 말

- [`hidpp`](https://crates.io/crates/hidpp) — [@lus](https://github.com/lus)
- [Solaar](https://github.com/pwr-Solaar/Solaar)
- [Mouser](https://github.com/TomBadash/Mouser) — Tom Badash

## 라이선스

다음 중 하나를 선택해 사용할 수 있습니다:

- Apache License 2.0 ([LICENSE-APACHE](../LICENSE-APACHE))
- MIT 라이선스 ([LICENSE-MIT](../LICENSE-MIT))

### 로고 및 브랜드 자산

OpenLogi 로고와 앱 아이콘 — [`design/`](../design/) 아래의 브랜드 자산 — 은 © 2026 AprilNEA가 모든 권리를 보유하며, 위 MIT/Apache 라이선스의 적용을 받지 않습니다. [`design/LICENSE`](../design/LICENSE)를 참고하세요. 코드를 포크해도 OpenLogi 이름·로고·아이콘에 대한 권리는 부여되지 않습니다. 사전 서면 허가 없이 자신의 프로젝트, 포크, 배포판을 나타내는 데 사용하지 마세요.

---

**Logitech과 무관합니다.** "Logitech", "MX Master", "Options+"는 Logitech International S.A.의 상표입니다.
