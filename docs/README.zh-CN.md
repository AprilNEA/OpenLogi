> [!WARNING]
> **OpenLogi 仍在积极开发中**，尚未稳定 —— 功能与配置仍可能变动。点个 **Star** ⭐ 并 **Watch** 👀 本仓库，在新版本发布时获得通知。

<h4 align="right"><a href="../README.md">English</a> | <strong>简体中文</strong> | <a href="README.ja.md">日本語</a> | <a href="README.de.md">Deutsch</a> | <a href="README.fr.md">Français</a> | <a href="README.ko.md">한국어</a></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ 本地优先的 Logitech Options+ 平替 <br/>通过 HID++ 与 UVC 协议解锁 Logitech 鼠标、键盘与摄像头的完整能力</strong></p>


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

> **被 Options+ 折腾够了？试试 OpenLogi。**

支持 macOS、Linux 和 Windows。

---

## 超越 Options+

OpenLogi 能做、而 Options+ 做不到的事：

- **轻量化** 原生 Rust + GPUI。
- **支持 Linux** Linux 是 OpenLogi 的一等公民。
- **自定义手势键** 可自由指定任一物理按键承担手势角色，也可以彻底关闭手势。
- **纯文本配置** 通过一个 TOML 文件完成，可通过多种方法在多台机器之间同步。
- **可脚本化** 除了 GUI 以外还支持 CLI。

## 功能列表

- 支持通过 Logi Bolt、Unifying 无线接收器、蓝牙或者有线连接的设备，并显示电池电量与充电状态
- 经由 OS 输入钩子的按键重映射：内置动作目录 + 自定义键盘快捷键（TOML 编写）¹
- 按应用的配置叠加层，应用获得焦点时自动切换（macOS + Windows；Linux 支持 GNOME（随附 Shell 扩展）、wlroots 合成器或 X11 / XWayland）
- Litra 补光灯：开关、亮度、色温，还可跟随摄像头活动自动开关

**鼠标**

- 中键、模式切换键、拇指滚轮等按键的捕获与重映射（中键全平台可用，其余取决于设备能力）
- 按方向的手势绑定与实时捕获，可放在任意支持的按键上
- Actions Ring：以光标为中心的八槽位动作环（`ShowActionsRing`），支持按应用的布局
- DPI 控制：预设 + 循环 / 按预设设置动作（`0x2201`）
- SmartShift 滚轮：模式切换、灵敏度和永久棘轮面板（`0x2111`）
- 按设备原生滚动反转（`0x2121`，受支持设备）

**键盘**

- F 键全局重映射：与鼠标共用同一动作目录，并提供文本输入、组合键、多步工作流等进阶动作（macOS + Windows）
- 静态 RGB 灯光（`0x8070` / `0x8080`，受支持设备）

**摄像头**

- 支持任何 Logitech UVC 摄像头（Brio、StreamCam、C920 系列等），即插即用
- 实时预览：只在查看时开启摄像头，切走即完全释放，指示灯同步熄灭
- 画面控制直写 UVC 硬件：变焦、对焦、曝光、亮度、对比度、饱和度、锐度、白平衡、色调，其中对焦 / 曝光 / 白平衡带自动模式开关；对 Meet / Zoom / OBS 等所有使用该摄像头的应用生效
- 一键配置档：内置「默认 / 直播 / 视频通话」三档，另可保存自定义快照；设置按摄像头持久化，下次查看时自动写回硬件

¹ Linux 上媒体键动作走 D-Bus MPRIS；少数 macOS 专属动作在 Linux 上没有通用对应功能，因此为空操作。Windows 会在可用时将平台动作映射到原生对应功能。

## 安装

> [!IMPORTANT]
> 请先退出 **Logi Options+** —— 两者会争夺 HID++ 访问权，同一个接收器同时只能由一方持有。

### macOS

需要 macOS 13 或更高版本。

从[最新 release](https://github.com/AprilNEA/OpenLogi/releases/latest) 下载已签名、已公证的 `.dmg`，把 `OpenLogi.app` 拖入 `/Applications`。

或通过 [Homebrew](https://brew.sh) 安装：

```sh
brew install --cask openlogi
```

官方 Homebrew cask 是默认安装途径。如需改用 `aprilnea/tap` 显式跟踪 GitHub 最新 release：

```sh
brew tap aprilnea/tap
brew install --cask aprilnea/tap/openlogi@latest
```

`openlogi@latest` 由 OpenLogi 的发布工作流维护，可能比官方 cask 的自动更新先一步。`openlogi` 和 `openlogi@latest` 二选一安装，不要同时装。

### Linux

从[最新 release](https://github.com/AprilNEA/OpenLogi/releases/latest) 下载适用于你的发行版的安装包：

```sh
# Debian / Ubuntu
sudo dpkg -i openlogi_*.deb

# Fedora / RHEL
sudo rpm -i openlogi-*.rpm

# Arch Linux — 从 AUR 安装（随系统一同更新）：
paru -S openlogi-bin   # or: yay -S openlogi-bin
# …或直接安装发布包：
sudo pacman -U openlogi-*.pkg.tar.zst
```

安装包同时提供 `x86_64`/`amd64` 与 `arm64`/`aarch64` 两种架构。

安装包会写入 udev 规则，让你的用户无需 `sudo` 即可访问 `/dev/hidraw*` 和 `/dev/uinput`。装完后为当前用户启用后台 agent：

```sh
systemctl --user enable --now openlogi-agent.service
```

手动 / 源码安装以及无 systemd 的发行版，见 [INSTALL-linux.md](INSTALL-linux.md)。

### Windows

每个 release 都附带签名的便携式 `.zip` 压缩包和按用户安装的 `.msi` 安装程序（x86_64 与 arm64）。两者均同时包含 GUI（`OpenLogi.exe`）和拥有全部设备 I/O 的后台 agent（`openlogi-agent.exe`）。使用便携式 zip 时，请把这两个文件放在同一目录，否则 GUI 将无法连接。

Windows 支持可正常工作，并已在 Windows 11 实机上完成端到端验证：包括有线键盘、使用 Unifying 接收器的鼠标，以及 MSI 的安装、原位升级和卸载。它比 macOS 版本更新，如遇到粗糙之处，请[反馈问题](https://github.com/AprilNEA/OpenLogi/issues)。agent 会显示系统托盘图标（「显示主窗口」/「退出」），因此关闭主窗口后仍可打开应用。如需在 Windows 上禁用该图标，请在 TOML 的 `[app_settings]` 块中设置 `show_in_menu_bar = false`，然后重启 agent；GUI 开关目前仅适用于 macOS。

从源码构建见 [DEVELOPMENT.md](DEVELOPMENT.md)。


## 使用（CLI）

见 [USAGE.md](USAGE.md)

## 配置

见 [CONFIGURATION.md](CONFIGURATION.md)

## 开发

见 [DEVELOPMENT.md](DEVELOPMENT.md)

## 致谢

- **Windows、摄像头与 i18n**：[@davidbudnick](https://github.com/davidbudnick) —— 键盘 RGB 支持、Windows 支持、Logitech 摄像头支持
- **Linux 移植**：[@cserby](https://github.com/cserby) —— Linux 支持
- [Solaar](https://github.com/pwr-Solaar/Solaar)，作者 [@pwr](https://github.com/pwr) —— 开源 HID++ 实现
- [Mouser](https://github.com/TomBadash/Mouser)，作者 [@TomBadash](https://github.com/TomBadash) —— 本地、无需账号的 Options+ 替代品

## 许可证

本仓库代码可选择以下任一许可证：

- Apache License 2.0（[LICENSE-APACHE](../LICENSE-APACHE)）
- MIT 许可证（[LICENSE-MIT](../LICENSE-MIT)）

### 第三方代码

`crates/openlogi-hidpp` 是 [`hidpp`](https://crates.io/crates/hidpp)（作者 [@lus](https://github.com/lus)）的 vendored fork，采用 0BSD 许可证。

### Logo 与品牌资产

感谢 [@kubai087](https://github.com/kubai087) 为 OpenLogi 设计的 Logo，该 Logo —— 即 [`design/`](../design/) 下的品牌资产 —— © 2026 AprilNEA 保留所有权利，不在上述 MIT/Apache 许可范围内，许可证详见 [`design/LICENSE`](../design/LICENSE)。
Fork 代码并不授予 OpenLogi 名称、Logo 或图标的使用权，未经事先书面许可，请勿用它们代表你自己的项目、Fork 或分发版本。

---

**与 Logitech 无关联。** 「Logitech」、「MX Master」与「Options+」是 Logitech International S.A. 的商标。

## 仓库活跃度

![Repobeats analytics image](https://repobeats.axiom.co/api/embed/4a0b576a03e9d528ad31ccf4797a1286c045d021.svg "Repobeats analytics image")
