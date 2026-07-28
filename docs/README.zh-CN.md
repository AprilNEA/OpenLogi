> [!WARNING]
> **OpenLogi 仍在积极开发中**，尚未稳定 —— 功能与配置仍可能变动。点个 **Star** ⭐ 并 **Watch** 👀 本仓库，在新版本发布时获得通知。

<h4 align="right"><a href="../README.md">English</a> | <strong>简体中文</strong> | <a href="README.ja.md">日本語</a> | <a href="README.de.md">Deutsch</a> | <a href="README.fr.md">Français</a> | <a href="README.ko.md">한국어</a></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ 原生、本地优先的 Logitech Options+ 替代品，用 Rust 编写 🦀<br/>通过 HID++ 重映射按键、调节 DPI 与 SmartShift。无账号、无遥测。</strong></p>


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

无需 Logitech 账号、无遥测、无需安装官方 Options+，即可重映射按键、调节 DPI 与 SmartShift、按应用自动切换配置。没有云端，配置是纯 TOML 文件。默认情况下，唯一的自动联网行为是获取设备图片；只有在你主动请求或选择启用时，才会检查并下载更新。

---

## 这是什么

OpenLogi 通过 Logi Bolt 和 Unifying 接收器、蓝牙直连或 USB 线缆与 Logitech HID++ 外设通信，完全不需要运行 Logi Options+。它由三个组件组成：

- **[OpenLogi GUI](../crates/openlogi-gui)** —— 基于 GPUI 的桌面应用：可点击热区的交互式鼠标示意图、逐按键动作选择器（内置动作 + 在 TOML 配置中编写的自定义快捷键）、DPI 预设、SmartShift、按设备原生滚动反转、RGB 键盘灯光、按应用的配置叠加层、实时设备轮播，以及界面已本地化为 20 种语言的设置窗口。
- **[OpenLogi agent](../crates/openlogi-agent)** —— 拥有输入钩子和全部设备 I/O 的后台服务。GUI 是纯 IPC 客户端，并在需要时启动 agent。
- **[OpenLogi CLI](../crates/openlogi-cli)** —— 命令行工具：无界面设备清单（`list`）、资产同步与设备诊断子命令。

一切都在本地完成：绑定保存在纯 TOML 文件中，agent 通过操作系统输入钩子重映射按键，并经由 HID++ 将 DPI、SmartShift、滚动和灯光修改直接写入设备。

支持 macOS、Linux 和 Windows。Windows 是最新移植的平台：已在 Windows 11 实机上完成端到端验证，但可能仍比 macOS 和 Linux 版本更显粗糙；详见[路线图](#路线图)。

## 超越 Options+

OpenLogi 能做、而 Options+ 做不到的事：

- **跑在 Linux 上。** Options+ 只有 macOS 和 Windows 版本。OpenLogi 把 Linux 当作一等公民：evdev/uinput 钩子、udev 规则、systemd 用户单元，以及 `.deb` / `.rpm` / `.pkg.tar.zst` 安装包。
- **切换手势键。** 自由指定哪个物理按键承担手势角色 —— 专用手势键、中键、后退或前进键 —— 支持按方向绑定滑动动作，也可以彻底关闭手势。Options+ 则把手势固定在专用手势键上。
- **纯文本配置。** 全部设置就是一个 TOML 文件，可读、可 diff、可纳入版本管理、可在多台机器间复制。
- **可脚本化。** 真正的 CLI：设备清单、资产预取、设备端 HID++ 诊断（特性 / 控制转储、DPI / SmartShift 往返自检和键盘灯光检查）。
- **保持轻量。** 原生 Rust + GPUI 二进制 —— 没有 Electron 全家桶、没有常驻更新器、无账号、无遥测。

## 路线图

| 能力 | 状态 |
|---|---|
| 发现 Bolt 接收器 + 列出已配对设备（CLI + GUI） | ✅ |
| Unifying 接收器（更早的协议，已被 Bolt 取代） | ✅ |
| 蓝牙直连 / 有线设备（无接收器） | ✅ |
| 电池电量 / 充电状态 | ✅（在线设备） |
| 交互式 GUI：轮播、鼠标示意图、动作选择器 | ✅ macOS + Linux + Windows |
| 经由 OS 输入钩子的按键重映射 | ✅ macOS + Linux + Windows |
| 内置动作目录 + 自定义键盘快捷键（TOML 编写） | ✅ macOS + Linux + Windows¹ |
| DPI 控制 + 预设 + 循环 / 按预设设置动作（HID++ `0x2201`） | ✅ |
| SmartShift 滚轮：模式切换 + 灵敏度 + 永久棘轮面板（HID++ `0x2111`） | ✅ |
| 按设备原生滚动反转（HID++ `0x2121`） | ✅（受支持设备） |
| 静态 RGB 键盘灯光（HID++ `0x8070` / `0x8080`） | ✅（受支持设备） |
| 按应用的配置叠加层（应用获得焦点时自动切换） | ✅ macOS + Windows，🟡 Linux（仅 X11 / XWayland） |
| 设置窗口：登录时启动、更新、权限、语言、外观 | ✅ macOS + Linux + Windows |
| Agent 状态图标 | ✅ macOS 菜单栏 + Windows 系统托盘；不适用于 Linux |
| 界面本地化（20 种语言：da、de、el、en、es、fi、fr、it、ja、ko、nb、nl、pl、pt-BR、pt-PT、ru、sv、zh-CN、zh-HK、zh-TW） | ✅ |
| Linux 打包：udev 规则、systemd 单元、`.deb` / `.rpm` / `.pkg.tar.zst` | ✅ Linux |
| 手势键按方向绑定 + 实时捕获 | ✅（取决于设备能力） |
| 中键 / 模式切换键 / 拇指滚轮按键捕获 | ✅ 所有平台均支持中键；模式切换键 / 拇指滚轮取决于设备能力 |
| Windows（agent、GUI、事件钩子、安装程序） | ✅ 已在 Windows 11 实机验证；较新的移植版本，兼容性仍在持续打磨 |

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

# Arch Linux
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

- [`hidpp`](https://crates.io/crates/hidpp)，作者 [@lus](https://github.com/lus)
- [Solaar](https://github.com/pwr-Solaar/Solaar)
- [Mouser](https://github.com/TomBadash/Mouser)，作者 Tom Badash

## 许可证

以下两种许可证任选其一：

- Apache License 2.0（[LICENSE-APACHE](../LICENSE-APACHE)）
- MIT 许可证（[LICENSE-MIT](../LICENSE-MIT)）

### Logo 与品牌资产

OpenLogi 的 Logo 与应用图标 —— 即 [`design/`](../design/) 下的品牌资产 —— © 2026 AprilNEA 保留所有权利，不在上述 MIT/Apache 许可范围内；见 [`design/LICENSE`](../design/LICENSE)。Fork 代码并不授予 OpenLogi 名称、Logo 或图标的使用权；未经事先书面许可，请勿用它们代表你自己的项目、Fork 或分发版本。

---

**与 Logitech 无关联。** 「Logitech」、「MX Master」与「Options+」是 Logitech International S.A. 的商标。

## 仓库活跃度

![Repobeats analytics image](https://repobeats.axiom.co/api/embed/4a0b576a03e9d528ad31ccf4797a1286c045d021.svg "Repobeats analytics image")
