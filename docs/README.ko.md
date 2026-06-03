> [!WARNING]
> **OpenLogi는 활발히 개발 중**이며 아직 안정 단계가 아닙니다 —— 기능과 설정이 변경될 수 있습니다. 저장소에 **Star** ⭐ 와 **Watch** 👀 를 눌러 두면 릴리스가 나오는 즉시 알림을 받을 수 있습니다.

<h4 align="right"><a href="../README.md">English</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.ja.md">日本語</a> | <a href="README.de.md">Deutsch</a> | <a href="README.fr.md">Français</a> | <strong>한국어</strong></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-animated.svg" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Rust로 작성된, 네이티브하고 로컬 우선의 Logitech Options+ 대체 도구 🦀<br/>HID++로 버튼, DPI, SmartShift를 재매핑하세요. 계정 불필요, 텔레메트리 없음.</strong></p>


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

> **Options+에 지치셨나요? OpenLogi를 사용해 보세요.**

Logitech 계정도, 텔레메트리도, 공식 Options+ 설치도 없이 버튼을 재매핑하고 DPI와 SmartShift를 제어하며 앱별로 프로필을 전환하세요. 클라우드 없이 단순한 TOML 설정만 사용하며, 네트워크 통신은 기기 이미지 가져오기와 옵트인 방식의 기본 비활성화 업데이트 확인뿐입니다.

---

## 소개

OpenLogi는 Logi Bolt 수신기 —— 또는 Bluetooth 직접 연결 / 유선 연결 —— 를 통해 Logitech HID++ 마우스와 통신하며, Logi Options+를 실행할 필요가 없습니다. 두 개의 바이너리를 제공합니다:

- **[OpenLogi GUI](../crates/openlogi-gui)** —— GPUI 데스크톱 앱: 클릭 가능한 핫스폿이 있는 인터랙티브 마우스 다이어그램, 버튼별 동작 선택기(39개의 기본 제공 동작과 녹화된 사용자 지정 단축키), DPI 프리셋, SmartShift 토글, 애플리케이션별 프로필 오버레이, 페어링된 기기 간을 실시간으로 전환하는 기기 캐러셀, 그리고 UI가 6개 언어로 현지화된 설정 창을 제공합니다.
- **[OpenLogi CLI](../crates/openlogi-cli)** —— 헤드리스 인벤토리 조회(`list`)와 에셋 동기화 및 기기 진단 하위 명령을 제공하는 CLI.

모든 것이 로컬에서 이루어집니다: 바인딩은 단순한 TOML 파일에 저장되고, 버튼 입력은 OS 이벤트 탭을 통해 재매핑되며, DPI / SmartShift 변경은 HID++로 기기에 직접 기록됩니다.

현재 macOS를 지원하며, Linux와 Windows도 곧 지원될 예정입니다 —— [로드맵](#로드맵)을 참조하세요.

## 로드맵

| 기능 | 상태 |
|---|---|
| Bolt 수신기 검색 + 페어링된 기기 목록(CLI + GUI) | ✅ |
| Bluetooth 직접 연결 / 유선 기기(수신기 불필요) | ✅ |
| 배터리 잔량 / 충전 상태 | ✅(온라인 기기) |
| 인터랙티브 GUI: 캐러셀, 마우스 다이어그램, 동작 선택기 | ✅ macOS |
| OS 이벤트 탭을 통한 버튼 재매핑(현재는 측면 Back / Forward) | ✅ macOS |
| 39개 동작 카탈로그 + 녹화된 사용자 지정 키보드 단축키 | ✅ macOS¹ |
| DPI 제어 + 프리셋 + 순환 / 프리셋 지정 동작(HID++ `0x2201`) | ✅ macOS |
| SmartShift 휠 모드 전환(HID++ `0x2111`) | ✅ macOS |
| 애플리케이션별 프로필 오버레이(앱 포커스 시 자동 전환) | ✅ macOS |
| 설정 창: 로그인 시 실행, 업데이트 확인, 메뉴 막대, 권한, 언어 | ✅ macOS |
| 인터페이스 현지화(6개 언어: en, ja, ru, zh-CN, zh-HK, zh-TW) | ✅ macOS |
| 제스처 버튼의 방향별 바인딩 | 🟡 설정 가능; 하드웨어 캡처는 대기 중 |
| 가운데 / 모드 전환 / 엄지 휠 버튼 캡처 | 🟡 설정 가능; 훅은 측면 버튼만 점유 |
| Linux / Windows 이벤트 훅 | ❌ 스텁(`Unsupported`) |
| Unifying 수신기 | ❌(아직 미지원) |

¹ 일부 동작(예: 미디어 키)은 현재 의도한 이벤트를 실제로 전송하지 않고 로그로만 기록합니다 —— 후속 작업으로 관리 중입니다.

## 설치

> [!IMPORTANT]
> 먼저 **Logi Options+**를 종료하세요 —— 두 애플리케이션은 HID++ 접근 권한을 두고 경쟁하며, 하나의 수신기는 한 번에 한쪽만 점유할 수 있습니다.

[최신 릴리스](https://github.com/AprilNEA/OpenLogi/releases/latest)에서 서명 및 공증된 `.dmg`를 내려받아 `OpenLogi.app`을 `/Applications`로 드래그하세요.

또는 [Homebrew](https://brew.sh)로 설치하세요:

```sh
brew install --cask openlogi
```

소스에서 빌드하려면 [DEVELOPMENT.md](DEVELOPMENT.md)를 참조하세요.

## 사용법(CLI)

[USAGE.md](USAGE.md)를 참조하세요.

## 설정

[CONFIGURATION.md](CONFIGURATION.md)를 참조하세요.

## 개발

[DEVELOPMENT.md](DEVELOPMENT.md)를 참조하세요.

## 감사의 말

- [`hidpp`](https://crates.io/crates/hidpp) — [@lus](https://github.com/lus)
- [Solaar](https://github.com/pwr-Solaar/Solaar)
- [Mouser](https://github.com/TomBadash/Mouser) — Tom Badash

## 라이선스

다음 중 하나로 이중 라이선스됩니다:

- Apache License, Version 2.0 ([LICENSE-APACHE](../LICENSE-APACHE))
- MIT 라이선스 ([LICENSE-MIT](../LICENSE-MIT))

원하는 쪽을 선택하시면 됩니다.

---

**Logitech과 무관합니다.** "Logitech", "MX Master", "Options+"는 Logitech International S.A.의 상표입니다.
