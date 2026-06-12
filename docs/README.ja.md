> [!WARNING]
> **OpenLogi は現在活発に開発中**であり、まだ安定していません —— 機能や設定は今後も変わる可能性があります。リポジトリに **Star** ⭐ と **Watch** 👀 を付けて、リリースが出た瞬間に通知を受け取りましょう。

<h4 align="right"><a href="../README.md">English</a> | <a href="README.zh-CN.md">简体中文</a> | <strong>日本語</strong> | <a href="README.de.md">Deutsch</a> | <a href="README.fr.md">Français</a> | <a href="README.ko.md">한국어</a></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Rust 製のネイティブでローカルファーストな Logitech Options+ 代替 🦀<br/>HID++ でボタン・DPI・SmartShift を再マッピング。アカウント不要、テレメトリなし。</strong></p>


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

> **Options+ にうんざり？OpenLogi をどうぞ。**

Logitech アカウントもテレメトリも公式 Options+ のインストールも不要で、ボタンの再マッピング、DPI と SmartShift の制御、アプリごとのプロファイル切り替えができます。クラウドなし、設定はプレーンな TOML ファイル。ネットワーク通信はデバイス画像の取得と、デフォルトオフのオプトイン更新チェックだけです。

---

## 概要

OpenLogi は Logi Bolt レシーバー経由 —— あるいは Bluetooth 直結 / 有線接続 —— で Logitech の HID++ マウスと通信します。Logi Options+ を動かす必要はありません。2 つのバイナリを提供します：

- **[OpenLogi GUI](../crates/openlogi-gui)** —— GPUI 製デスクトップアプリ：クリック可能なホットスポット付きのインタラクティブなマウス図、ボタンごとのアクションピッカー（41 種の組み込みアクション + TOML 設定に手書きするカスタムショートカット）、DPI プリセット、SmartShift パネル（ホイールモード・感度・永続ラチェット）、アプリごとのプロファイルオーバーレイ、ペアリング済みデバイスをライブで切り替えるデバイスカルーセル、そして 20 言語にローカライズされた設定ウィンドウ。
- **[OpenLogi CLI](../crates/openlogi-cli)** —— ヘッドレスなデバイス一覧（`list`）、アセット同期、デバイス診断のサブコマンドを備えた CLI。

すべてはローカルで完結します：バインディングはプレーンな TOML ファイルに保存され、ボタン入力は OS のイベントフックで再マッピングされ、DPI / SmartShift の変更は HID++ で直接デバイスに書き込まれます。

macOS と Linux をサポートしています。Windows は未検証の早期プレビューで、各リリースに署名済みビルドが付属します —— [ロードマップ](#ロードマップ)を参照。

## Options+ を超えて

OpenLogi にできて Options+ にできないこと：

- **Linux で動く。** Options+ は macOS と Windows のみ。OpenLogi は Linux をファーストクラスで扱います：evdev/uinput フック、udev ルール、systemd ユーザーユニット、`.deb` / `.rpm` パッケージ。
- **ジェスチャーボタンを移せる。** どの物理ボタンがジェスチャー役を担うか —— サムパッド、ミドル、戻る、進む —— を選べ、方向ごとのスワイプバインディングを設定でき、ジェスチャーを完全にオフにもできます。Options+ はジェスチャーを専用サムパッドに固定しています。
- **設定がプレーンテキスト。** すべてが 1 つの TOML ファイル。読めて、diff できて、バージョン管理に入れられて、マシン間でコピーできます。
- **スクリプトで叩ける。** 本物の CLI：デバイス一覧、アセットのプリフェッチ、デバイス上での HID++ 診断（フィーチャーダンプ、DPI / SmartShift のラウンドトリップ検査）。
- **軽量なまま。** ネイティブ Rust + GPUI バイナリ —— Electron スイートも常駐アップデーターもアカウントもテレメトリもなし。

## ロードマップ

| 機能 | 状態 |
|---|---|
| Bolt レシーバーの発見 + ペアリング済みデバイスの一覧（CLI + GUI） | ✅ |
| Unifying レシーバー（Bolt に置き換えられた旧プロトコル） | ✅ |
| Bluetooth 直結 / 有線デバイス（レシーバーなし） | ✅ |
| バッテリー残量 / 充電状態 | ✅（オンラインのデバイス） |
| インタラクティブ GUI：カルーセル、マウス図、アクションピッカー | ✅ macOS + Linux |
| OS イベントフック / evdev によるボタン再マッピング | ✅ macOS + Linux |
| 41 アクションのカタログ + カスタムキーボードショートカット（TOML 手書き） | ✅ macOS + Linux¹ |
| DPI 制御 + プリセット + サイクル / プリセット指定アクション（HID++ `0x2201`） | ✅ |
| SmartShift ホイール：モード切替 + 感度 + 永続ラチェットパネル（HID++ `0x2111`） | ✅ |
| アプリごとのプロファイルオーバーレイ（フォーカスで自動切替） | ✅ macOS、🟡 Linux（X11 のみ） |
| 設定ウィンドウ：ログイン時起動、更新チェック、メニューバー、権限、言語 | ✅ macOS + Linux |
| UI のローカライズ（20 言語：da、de、el、en、es、fi、fr、it、ja、ko、nb、nl、pl、pt-BR、pt-PT、ru、sv、zh-CN、zh-HK、zh-TW） | ✅ |
| Linux パッケージング：udev ルール、systemd ユニット、`.deb` / `.rpm` | ✅ Linux |
| ジェスチャーボタンの方向別バインディング | 🟡 設定可能；ハードウェアキャプチャは開発中 |
| ミドル / モードシフト / サムホイールボタンのキャプチャ | 🟡 設定可能；フックが扱うのは現状サイドボタンのみ |
| Windows（agent、GUI、イベントフック） | 🟡 未検証プレビュー —— 各リリースに署名済み `.exe` / `.msi` が付属 |

¹ Linux のメディアキーアクションは D-Bus MPRIS を使います。macOS 固有の一部アクション（Launchpad など）は Linux に対応物がなく、no-op になります。

## インストール

> [!IMPORTANT]
> 先に **Logi Options+** を終了してください —— 両者は HID++ アクセスを奪い合い、1 つのレシーバーを同時に所有できるのは片方だけです。

### macOS

[最新リリース](https://github.com/AprilNEA/OpenLogi/releases/latest)から署名・公証済みの `.dmg` をダウンロードし、`OpenLogi.app` を `/Applications` にドラッグします。

または [Homebrew](https://brew.sh) で：

```sh
brew install --cask openlogi
```

公式 Homebrew cask が標準のインストール経路です。代わりに `aprilnea/tap` で GitHub の最新リリースを明示的に追うには：

```sh
brew tap aprilnea/tap
brew install --cask aprilnea/tap/openlogi@latest
```

`openlogi@latest` は OpenLogi のリリースワークフローが管理しており、公式 cask の autobump より先に更新されることがあります。`openlogi` か `openlogi@latest` のどちらか一方だけをインストールしてください。

### Linux

[最新リリース](https://github.com/AprilNEA/OpenLogi/releases/latest)から `.deb` または `.rpm` をダウンロード：

```sh
# Debian / Ubuntu
sudo dpkg -i openlogi_*.deb

# Fedora / RHEL
sudo rpm -i openlogi-*.rpm
```

パッケージは `x86_64`/`amd64` と `arm64`/`aarch64` の両方で公開されています。

パッケージは udev ルールをインストールし、`sudo` なしで `/dev/hidraw*` と `/dev/uinput` にアクセスできるようにします。インストール後、ユーザーのバックグラウンドエージェントを有効化してください：

```sh
systemctl --user enable --now openlogi-agent.service
```

手動 / ソースからのインストールや systemd のないディストリビューションは [INSTALL-linux.md](INSTALL-linux.md) を参照。

### Windows（プレビュー）

各リリースに署名済み `.exe` とユーザー単位の `.msi` インストーラー（x86_64 と arm64）が付属します。Windows サポートは実機での検証がまだ十分でない早期プレビューです —— 粗削りな部分はご容赦のうえ、[issue で報告](https://github.com/AprilNEA/OpenLogi/issues)してください。

ソースからのビルドは [DEVELOPMENT.md](DEVELOPMENT.md) を参照。


## 使い方（CLI）

[USAGE.md](USAGE.md) を参照

## 設定

[CONFIGURATION.md](CONFIGURATION.md) を参照

## 開発

[DEVELOPMENT.md](DEVELOPMENT.md) を参照

## 謝辞

- [`hidpp`](https://crates.io/crates/hidpp) by [@lus](https://github.com/lus)
- [Solaar](https://github.com/pwr-Solaar/Solaar)
- [Mouser](https://github.com/TomBadash/Mouser) by Tom Badash

## ライセンス

以下のいずれかを選択できます：

- Apache License 2.0（[LICENSE-APACHE](../LICENSE-APACHE)）
- MIT ライセンス（[LICENSE-MIT](../LICENSE-MIT)）

### ロゴとブランドアセット

OpenLogi のロゴとアプリアイコン —— [`design/`](../design/) 配下のブランドアセット —— は © 2026 AprilNEA が全権利を留保しており、上記の MIT/Apache ライセンスの対象外です。[`design/LICENSE`](../design/LICENSE) を参照してください。コードをフォークしても OpenLogi の名称・ロゴ・アイコンの使用権は付与されません。事前の書面による許可なく、ご自身のプロジェクト、フォーク、配布物を表すために使用しないでください。

---

**Logitech とは無関係です。** 「Logitech」「MX Master」「Options+」は Logitech International S.A. の商標です。
