> [!WARNING]
> **OpenLogi 仍在积极开发中**，尚未稳定 —— 功能与配置仍可能变动。点个 **Star** ⭐ 并 **Watch** 👀 本仓库，第一时间获得发布通知。

<h4 align="right"><a href="README.md">English</a> | <strong>简体中文</strong></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-animated.svg" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ 原生、本地优先的 Logitech Options+ 替代品，用 Rust 编写 🦀<br/>通过 HID++ 重映射按键、调节 DPI 与 SmartShift。无账号、无遥测。</strong></p>


<div align="center">
    <a href="https://twitter.com/AprilNEA" target="_blank">
    <img alt="twitter" src="https://img.shields.io/badge/follow-AprilNEA-green?style=flat-square&logo=Twitter"></a>
    <a href="https://t.me/+u8DfyLlIqPYxZjJh" target="_blank">
    <img alt="telegram" src="https://img.shields.io/badge/chat-telegram-blueviolet?style=flat-square&logo=Telegram"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/releases" target="_blank">
    <img alt="GitHub downloads" src="https://img.shields.io/github/downloads/AprilNEA/OpenLogi/total.svg?style=flat-square"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/commits" target="_blank">
    <img alt="GitHub commit" src="https://img.shields.io/github/commit-activity/m/AprilNEA/OpenLogi?style=flat-square"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/issues?q=is%3Aissue+is%3Aclosed" target="_blank">
    <img alt="GitHub closed issues" src="https://img.shields.io/github/issues-closed/AprilNEA/OpenLogi.svg?style=flat-square"></a>
</div>

> **被 Options+ 折腾够了？试试 OpenLogi。**

无需罗技账号、无遥测、无需安装官方 Options+，即可重映射按键、调节 DPI 与
SmartShift、按应用切换配置。完全本地化，纯 TOML 配置；

---

## 是什么

OpenLogi 通过 Logi Bolt 接收器 —— 或蓝牙直连 / 有线连接 —— 与罗技 HID++ 鼠标通信，无需运行 Logi Options+。它包含两个二进制：

- **[OpenLogi GUI](crates/openlogi-gui)** —— 基于 GPUI 的桌面应用：交互式鼠标示意图（按钮可点击）、按钮动作选择器（37 个内置动作 + 录制的自定义快捷键）、DPI 预设、SmartShift 开关、按应用的配置叠加层，以及可在配对设备间实时切换的设备轮播。
- **[OpenLogi CLI](crates/openlogi-cli)** —— 用于无头清单查看（`list`）以及资源同步与设备诊断的命令行工具。

所有数据都在本地：绑定保存在普通 TOML 文件里，按键按下经由系统事件 tap 重映射，DPI / SmartShift 变更通过 HID++ 直接写入设备。

目前仅支持 macOS，很快将会支持 Linux 和 Windows。

## 路线图

| 能力 | 状态 |
|---|---|
| 发现 Bolt 接收器并列出配对设备（CLI + GUI） | ✅ |
| 蓝牙直连 / 有线设备（无接收器） | ✅ |
| 电量百分比 / 充电状态 | ✅（在线设备） |
| 交互式 GUI：轮播、鼠标示意图、动作选择器 | ✅ macOS |
| 经由 OS 事件 tap 的按键重映射（目前为侧键 Back / Forward） | ✅ macOS |
| 37 个动作目录 + 录制的自定义键盘快捷键 | ✅ macOS¹ |
| DPI 控制 + 预设 + 循环 / 按预设设置动作（HID++ `0x2201`） | ✅ macOS |
| SmartShift 滚轮模式切换（HID++ `0x2111`） | ✅ macOS |
| 按应用的配置叠加层（应用获得焦点时自动切换） | ✅ macOS |
| 开机启动 + 可选更新检查 | ✅（仅 TOML —— 暂无设置 UI） |
| 手势按键的方向绑定 | 🟡 可配置；硬件捕获待办 |
| 中键 / 模式切换键 / 拇指轮的按键捕获 | 🟡 可配置；钩子目前只占用侧键 |
| Linux / Windows 事件钩子 | ❌ 占位（`Unsupported`） |
| Unifying 接收器 | ❌（`hidpp 0.2` 暂未实现） |

¹ 少数动作（例如媒体键）目前只记录预期事件而没有真正发送 —— 已列入待办。

## 安装

> [!IMPORTANT]
> 请先退出 **Logi Options+** —— 两者会争抢 HID++ 访问权，同一时刻一个接收器只能由一方占用。

从 [最新 Release](https://github.com/AprilNEA/OpenLogi/releases/latest) 下载的 `.dmg`，把 `OpenLogi.app` 拖到 `/Applications`。

或通过 [Homebrew](https://brew.sh) 安装：

```sh
brew install --cask openlogi
```

需要从源码构建请看 [DEVELOPMENT.md](docs/DEVELOPMENT.md)。

## 用法（CLI）

查看 [USAGE.md](docs/USAGE.md)

## 配置

查看 [CONFIGURATION.md](docs/CONFIGURATION.md)

## 开发

查看 [DEVELOPMENT.md](docs/DEVELOPMENT.md)

## 致谢

- [`hidpp`](https://crates.io/crates/hidpp) 由 [@lus](https://github.com/lus)
- [Solaar](https://github.com/pwr-Solaar/Solaar)
- [Mouser](https://github.com/TomBadash/Mouser) 由 Tom Badash

## 许可证

双重许可，任选其一：

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

---

**与罗技无关。** "Logitech"、"MX Master" 与 "Options+" 是 Logitech International S.A. 的商标。
