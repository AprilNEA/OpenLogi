> [!WARNING]
> **OpenLogi 仍在积极开发中**，尚未稳定 —— 功能与配置仍可能变动。点个 **Star** ⭐ 并 **Watch** 👀 本仓库，第一时间获得发布通知。

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

> **被 Options+ 折腾够了？试试 OpenLogi。**

无需罗技账号、无遥测、无需安装官方 Options+，即可重映射按键、调节 DPI 与 SmartShift、按应用自动切换配置。没有云端，配置是纯 TOML 文件；唯一的网络请求是获取设备图片和默认关闭、需手动开启的更新检查。

---

## 这是什么

OpenLogi 通过 Logi Bolt 接收器 —— 或蓝牙直连 / 有线连接 —— 与罗技 HID++ 鼠标通信，完全不需要运行 Logi Options+。它提供两个可执行程序：

- **[OpenLogi GUI](../crates/openlogi-gui)** —— 基于 GPUI 的桌面应用：可点击热区的交互式鼠标示意图、逐按键动作选择器（41 个内置动作 + 在 TOML 配置中手写的自定义快捷键）、DPI 预设、SmartShift 面板（滚轮模式、灵敏度、永久棘轮）、按应用的配置叠加层、可在已配对设备间实时切换的设备轮播，以及一个界面已本地化为 20 种语言的设置窗口。
- **[OpenLogi CLI](../crates/openlogi-cli)** —— 命令行工具：无界面设备清单（`list`）、资产同步与设备诊断子命令。

一切都在本地完成：绑定保存在纯 TOML 文件中，按键通过操作系统事件钩子重映射，DPI / SmartShift 修改经由 HID++ 直接写入设备。

目前支持 macOS 与 Linux。Windows 处于早期预览阶段（未经充分测试）—— 每个 release 都会附带签名构建；详见[路线图](#路线图)。

## 超越 Options+

OpenLogi 能做、而 Options+ 做不到的事：

- **跑在 Linux 上。** Options+ 只有 macOS 和 Windows 版本。OpenLogi 把 Linux 当作一等公民：evdev/uinput 钩子、udev 规则、systemd 用户单元，以及 `.deb` / `.rpm` 安装包。
- **切换手势键。** 自由指定哪个物理按键承担手势角色 —— 拇指键、中键、后退或前进键 —— 支持按方向绑定滑动动作，也可以彻底关闭手势。Options+ 则把手势固定在专用拇指键上。
- **纯文本配置。** 全部设置就是一个 TOML 文件，可读、可 diff、可纳入版本管理、可在多台机器间复制。
- **可脚本化。** 真正的 CLI：设备清单、资产预取、设备端 HID++ 诊断（特性表转储、DPI / SmartShift 往返自检）。
- **保持轻量。** 原生 Rust + GPUI 二进制 —— 没有 Electron 全家桶、没有常驻更新器、无账号、无遥测。

## 路线图

| 能力 | 状态 |
|---|---|
| 发现 Bolt 接收器 + 列出已配对设备（CLI + GUI） | ✅ |
| Unifying 接收器（更早的协议，已被 Bolt 取代） | ✅ |
| 蓝牙直连 / 有线设备（无接收器） | ✅ |
| 电池电量 / 充电状态 | ✅（在线设备） |
| 交互式 GUI：轮播、鼠标示意图、动作选择器 | ✅ macOS + Linux |
| 经由 OS 事件钩子 / evdev 的按键重映射 | ✅ macOS + Linux |
| 41 个动作目录 + 自定义键盘快捷键（TOML 手写） | ✅ macOS + Linux¹ |
| DPI 控制 + 预设 + 循环 / 按预设设置动作（HID++ `0x2201`） | ✅ |
| SmartShift 滚轮：模式切换 + 灵敏度 + 永久棘轮面板（HID++ `0x2111`） | ✅ |
| 按应用的配置叠加层（应用获得焦点时自动切换） | ✅ macOS，🟡 Linux（仅 X11） |
| 设置窗口：开机启动、更新检查、菜单栏、权限、语言 | ✅ macOS + Linux |
| 界面本地化（20 种语言：da、de、el、en、es、fi、fr、it、ja、ko、nb、nl、pl、pt-BR、pt-PT、ru、sv、zh-CN、zh-HK、zh-TW） | ✅ |
| Linux 打包：udev 规则、systemd 单元、`.deb` / `.rpm` | ✅ Linux |
| 手势键按方向绑定 | 🟡 可配置；硬件捕获开发中 |
| 中键 / 模式切换键 / 拇指滚轮按键捕获 | 🟡 可配置；钩子目前只接管侧键 |
| Windows（agent、GUI、事件钩子） | 🟡 未经测试的预览 —— 每个 release 附带签名 `.exe` / `.msi` |

¹ Linux 上媒体键动作走 D-Bus MPRIS；少数 macOS 专属动作（如 Launchpad）在 Linux 上没有对应物，为空操作。

## 安装

> [!IMPORTANT]
> 请先退出 **Logi Options+** —— 两者会争夺 HID++ 访问权，同一个接收器同时只能由一方持有。

### macOS

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

从[最新 release](https://github.com/AprilNEA/OpenLogi/releases/latest) 下载 `.deb` 或 `.rpm`：

```sh
# Debian / Ubuntu
sudo dpkg -i openlogi_*.deb

# Fedora / RHEL
sudo rpm -i openlogi-*.rpm
```

安装包同时提供 `x86_64`/`amd64` 与 `arm64`/`aarch64` 两种架构。

安装包会写入 udev 规则，让你的用户无需 `sudo` 即可访问 `/dev/hidraw*` 和 `/dev/uinput`。装完后为当前用户启用后台 agent：

```sh
systemctl --user enable --now openlogi-agent.service
```

手动 / 源码安装以及无 systemd 的发行版，见 [INSTALL-linux.md](INSTALL-linux.md)。

### Windows（预览）

每个 release 都附带签名的 `.exe` 与按用户安装的 `.msi`（x86_64 与 arm64）。Windows 支持仍处于早期预览，尚未在真实硬件上充分测试 —— 请预期一些粗糙之处，并欢迎[反馈问题](https://github.com/AprilNEA/OpenLogi/issues)。

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

**与 Logitech 无关联。** "Logitech"、"MX Master" 与 "Options+" 是 Logitech International S.A. 的商标。
