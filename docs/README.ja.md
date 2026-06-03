> [!WARNING]
> **OpenLogi は現在活発に開発中**であり、まだ安定していません —— 機能や設定は今後も変わる可能性があります。リポジトリに **Star** ⭐ と **Watch** 👀 を付けて、リリースが出た瞬間に通知を受け取りましょう。

<h4 align="right"><a href="../README.md">English</a> | <a href="README.zh-CN.md">简体中文</a> | <strong>日本語</strong> | <a href="README.de.md">Deutsch</a> | <a href="README.fr.md">Français</a> | <a href="README.ko.md">한국어</a></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-animated.svg" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Rust 製の、ネイティブでローカルファーストな Logitech Options+ の代替 🦀<br/>HID++ 経由でボタン・DPI・SmartShift を再マッピング。アカウント不要、テレメトリなし。</strong></p>


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

> **Options+ にうんざり？ OpenLogi を試してみてください。**

ボタンの再マッピング、DPI と SmartShift の制御、アプリごとのプロファイル切り替えを —— Logitech アカウントもテレメトリも、公式 Options+ のインストールも不要で実現します。クラウドなし、設定はプレーンな TOML。ネットワーク通信はデバイス画像の取得と、オプトイン・既定でオフの更新チェックだけです。

---

## 概要

OpenLogi は Logi Bolt レシーバー —— あるいは Bluetooth 直結 / 有線接続 —— を通じて Logitech HID++ マウスと通信し、Logi Options+ を実行する必要はありません。2 つのバイナリを提供します：

- **[OpenLogi GUI](../crates/openlogi-gui)** —— GPUI 製のデスクトップアプリ：クリック可能なホットスポット付きのインタラクティブなマウス図、ボタンごとのアクションピッカー（39 個の組み込みアクション＋録音したカスタムショートカット）、DPI プリセット、SmartShift トグル、アプリ別のプロファイルオーバーレイ、ペアリング済みデバイスをライブで切り替えるデバイスカルーセル、そして UI が 6 言語にローカライズされた設定ウィンドウ。
- **[OpenLogi CLI](../crates/openlogi-cli)** —— ヘッドレスなインベントリ表示（`list`）に加え、アセット同期やデバイス診断のサブコマンドを備えた CLI。

すべてローカルで完結します：バインディングはプレーンな TOML ファイルに保存され、ボタン押下は OS のイベント tap を介して再マッピングされ、DPI / SmartShift の変更は HID++ 経由でデバイスに直接書き込まれます。

現在は macOS に対応しています。Linux と Windows は近日対応予定です —— [ロードマップ](#ロードマップ)を参照してください。

## ロードマップ

| 機能 | 状態 |
|---|---|
| Bolt レシーバーの検出とペアリング済みデバイスの一覧（CLI + GUI） | ✅ |
| Bluetooth 直結 / 有線デバイス（レシーバー不要） | ✅ |
| バッテリー残量 / 充電状態 | ✅（オンラインのデバイス） |
| インタラクティブ GUI：カルーセル、マウス図、アクションピッカー | ✅ macOS |
| OS イベント tap によるボタン再マッピング（現在はサイドボタンの Back / Forward） | ✅ macOS |
| 39 アクションのカタログ＋録音したカスタムキーボードショートカット | ✅ macOS¹ |
| DPI 制御＋プリセット＋循環 / プリセット指定アクション（HID++ `0x2201`） | ✅ macOS |
| SmartShift ホイールモードの切り替え（HID++ `0x2111`） | ✅ macOS |
| アプリ別プロファイルオーバーレイ（アプリのフォーカス時に自動切り替え） | ✅ macOS |
| 設定ウィンドウ：ログイン時起動、更新チェック、メニューバー、権限、言語 | ✅ macOS |
| インターフェースのローカライズ（6 言語：en、ja、ru、zh-CN、zh-HK、zh-TW） | ✅ macOS |
| ジェスチャーボタンの方向別バインディング | 🟡 設定可能；ハードウェアでの取得は未対応 |
| 中ボタン / モードシフト / サムホイールのボタン取得 | 🟡 設定可能；フックはサイドボタンのみを占有 |
| Linux / Windows のイベントフック | ❌ スタブ（`Unsupported`） |
| Unifying レシーバー | ❌（未対応） |

¹ 一部のアクション（例：メディアキー）は現在、想定するイベントを実際に送出せずログに記録するだけです —— フォローアップとして管理しています。

## インストール

> [!IMPORTANT]
> 先に **Logi Options+** を終了してください —— 両アプリは HID++ アクセスを奪い合い、1 つのレシーバーは同時に一方しか占有できません。

[最新リリース](https://github.com/AprilNEA/OpenLogi/releases/latest)から署名・公証済みの `.dmg` をダウンロードし、`OpenLogi.app` を `/Applications` にドラッグします。

または [Homebrew](https://brew.sh) でインストール：

```sh
brew install --cask openlogi
```

ソースからのビルドは [DEVELOPMENT.md](DEVELOPMENT.md) を参照してください。

## 使い方（CLI）

[USAGE.md](USAGE.md) を参照してください。

## 設定

[CONFIGURATION.md](CONFIGURATION.md) を参照してください。

## 開発

[DEVELOPMENT.md](DEVELOPMENT.md) を参照してください。

## 謝辞

- [`hidpp`](https://crates.io/crates/hidpp) by [@lus](https://github.com/lus)
- [Solaar](https://github.com/pwr-Solaar/Solaar)
- [Mouser](https://github.com/TomBadash/Mouser) by Tom Badash

## ライセンス

以下のいずれかのデュアルライセンスです：

- Apache License, Version 2.0 ([LICENSE-APACHE](../LICENSE-APACHE))
- MIT license ([LICENSE-MIT](../LICENSE-MIT))

お好きな方をお選びください。

---

**Logitech とは無関係です。** 「Logitech」「MX Master」「Options+」は Logitech International S.A. の商標です。
