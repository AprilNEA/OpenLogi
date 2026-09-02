> [!WARNING]
> **OpenLogi는 활발히 개발 중**이며 아직 안정 단계가 아닙니다 — 기능과 설정이 변경될 수 있습니다. 저장소에 **Star** ⭐ 와 **Watch** 👀 를 눌러 두면 새 릴리스가 나올 때 알림을 받을 수 있습니다.

<h4 align="right"><a href="../README.md">English</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.ja.md">日本語</a> | <a href="README.de.md">Deutsch</a> | <a href="README.fr.md">Français</a> | <strong>한국어</strong></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Rust로 작성된 네이티브 로컬 우선 Logitech Options+ 대안 🦀<br/>HID++와 UVC로 Logitech 마우스·키보드·웹캠의 모든 기능을 끌어냅니다</strong></p>


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

> **Options+가 지긋지긋하다면? OpenLogi를 써 보세요.**

macOS, Linux, Windows를 지원합니다.

---

## Options+ 그 너머

OpenLogi는 되고 Options+는 안 되는 것들:

- **가볍게 유지.** 네이티브 Rust + GPUI.
- **Linux 지원.** Linux는 OpenLogi의 일급 플랫폼입니다.
- **제스처 버튼 자유 지정.** 아무 물리 버튼에나 제스처 역할을 맡기거나, 제스처를 아예 끌 수 있습니다.
- **순수 텍스트 설정.** 모든 설정이 TOML 파일 하나에 담겨 원하는 방법으로 기기 간 동기화할 수 있습니다.
- **스크립트 가능.** GUI 외에 진짜 CLI도 제공합니다.

## 기능 목록

- Logi Bolt 수신기, Unifying 수신기, Bluetooth, 유선으로 연결된 기기를 지원하며 배터리 잔량과 충전 상태 표시
- OS 입력 훅을 통한 버튼 리매핑: 내장 액션 카탈로그 + 사용자 지정 키보드 단축키(TOML 작성)¹
- 앱 포커스 시 자동 전환되는 앱별 프로필 오버레이(macOS + Windows; Linux는 GNOME(번들 Shell 확장), wlroots 컴포지터 또는 X11 / XWayland)
- Litra 조명: 전원, 밝기, 색온도 제어와 카메라 사용에 연동한 자동 켜기 / 끄기

**마우스**

- 가운데 버튼, 모드 시프트, 썸휠 버튼의 캡처와 리매핑(가운데 버튼은 모든 플랫폼, 나머지는 기기 기능에 따라 다름)
- 방향별 제스처 바인딩과 실시간 캡처, 지원되는 아무 버튼에나 설정 가능
- Actions Ring: 커서 중심의 8슬롯 액션 오버레이(`ShowActionsRing`), 앱별 레이아웃 지원
- DPI 제어: 프리셋 + 사이클 / 프리셋 지정 액션(`0x2201`)
- SmartShift 휠: 모드 전환, 감도, 영구 래칫 패널(`0x2111`)
- 기기별 네이티브 스크롤 반전(`0x2121`, 지원 기기)

**키보드**

- F 키 전역 리매핑: 마우스와 같은 액션 카탈로그에 더해 텍스트 입력, 키 조합, 다단계 워크플로 등 파워 유저 액션 제공(macOS + Windows)
- 정적 RGB 조명(`0x8070` / `0x8080`, 지원 기기)

**카메라**

- 모든 Logitech UVC 웹캠(Brio, StreamCam, C920 시리즈 등) 플러그 앤 플레이 지원
- 실시간 미리보기: 보고 있는 동안에만 카메라를 켜고, 벗어나면 완전히 해제되어 LED도 꺼집니다
- 화면 조절 값을 UVC 하드웨어에 직접 기록: 줌, 초점, 노출, 밝기, 대비, 채도, 선명도, 화이트 밸런스, 색조 — 초점 / 노출 / 화이트 밸런스는 자동 모드 전환 지원, Meet / Zoom / OBS 등 카메라를 쓰는 모든 앱에 적용
- 원클릭 프로필: 기본값 / 스트리밍 / 영상 통화 3종 내장 + 사용자 스냅숏 저장; 설정은 카메라별로 저장되며 다음에 볼 때 하드웨어에 다시 기록됩니다

¹ Linux의 미디어 키 액션은 D-Bus MPRIS를 사용합니다. 일부 macOS 전용 액션은 Linux에 범용 대응 기능이 없어 아무 동작도 하지 않습니다. Windows는 가능한 경우 플랫폼 액션을 네이티브 기능에 매핑합니다.

## 설치

> [!IMPORTANT]
> 먼저 **Logi Options+** 를 종료하세요 — 두 애플리케이션은 HID++ 접근을 두고 경합하며, 하나의 수신기는 한쪽만 소유할 수 있습니다.

### macOS

macOS 13 이상이 필요합니다.

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

# Arch Linux — AUR에서 설치(시스템과 함께 업데이트됨):
paru -S openlogi-bin   # or: yay -S openlogi-bin
# …또는 릴리스 패키지를 직접 설치:
sudo pacman -U openlogi-*.pkg.tar.zst
```

패키지는 `x86_64`/`amd64`와 `arm64`/`aarch64` 두 아키텍처로 제공됩니다.

패키지는 `sudo` 없이 `/dev/hidraw*`와 `/dev/uinput`에 접근할 수 있게 해 주는 udev 규칙을 설치합니다. 설치 후 사용자용 백그라운드 에이전트를 활성화하세요:

```sh
systemctl --user enable --now openlogi-agent.service
```

수동 / 소스 설치와 systemd가 없는 배포판은 [INSTALL-linux.md](INSTALL-linux.md)를 참고하세요.

### Windows

각 릴리스에는 서명된 휴대용 `.zip` 아카이브와 사용자별 `.msi` 설치 파일(x86_64 및 arm64)이 포함됩니다. 둘 다 GUI(`OpenLogi.exe`)와 모든 기기 I/O를 소유하는 백그라운드 agent(`openlogi-agent.exe`)를 함께 제공합니다. 휴대용 zip을 사용할 때 두 파일을 같은 위치에 두지 않으면 GUI가 연결할 대상이 없습니다.

Windows 지원은 정상 작동하며 유선 키보드와 Unifying 수신기 마우스를 사용해 MSI 설치, 인플레이스 업그레이드, 제거까지 Windows 11 실제 하드웨어에서 엔드투엔드 검증했습니다. macOS 포트보다 최신이므로 문제가 있으면 [제보](https://github.com/AprilNEA/OpenLogi/issues)해 주세요. agent는 시스템 트레이 아이콘(메인 창 표시 / 종료)을 표시하므로 메인 창을 닫은 뒤에도 앱에 접근할 수 있습니다. Windows에서 비활성화하려면 TOML `[app_settings]` 블록에 `show_in_menu_bar = false`를 설정하고 agent를 다시 시작하세요. GUI 토글은 현재 macOS 전용입니다.

소스에서 빌드하려면 [DEVELOPMENT.md](DEVELOPMENT.md)를 참고하세요.


## 사용법 (CLI)

[USAGE.md](USAGE.md) 참고

## 설정

[CONFIGURATION.md](CONFIGURATION.md) 참고

## 개발

[DEVELOPMENT.md](DEVELOPMENT.md) 참고

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
