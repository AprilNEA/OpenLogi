> [!WARNING]
> **OpenLogi は現在活発に開発中**であり、まだ安定していません —— 機能や設定は今後も変わる可能性があります。リポジトリに **Star** ⭐ と **Watch** 👀 を付けて、新しいリリースの通知を受け取りましょう。

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

<p align="center">
    <a href="https://trendshift.io/repositories/42303" target="_blank">
    <img src="https://trendshift.io/api/badge/trendshift/repositories/42303/daily?language=Rust" alt="AprilNEA%2FOpenLogi | Trendshift" width="250" height="55"/></a>
</p>

> **Options+ にうんざり？OpenLogi をどうぞ。**

Logitech アカウントもテレメトリも公式 Options+ のインストールも不要で、ボタンの再マッピング、DPI と SmartShift の制御、アプリごとのプロファイル切り替えができます。クラウドなし、設定はプレーンな TOML ファイル。デフォルトでは自動接続はデバイス画像の取得だけで、更新の確認とダウンロードは要求時またはオプトイン時にのみ実行されます。

---

## 概要

OpenLogi は Logi Bolt および Unifying レシーバー、Bluetooth 直結、USB ケーブル経由で Logitech の HID++ 周辺機器と通信します。Logi Options+ を動かす必要はありません。3 つのコンポーネントで構成されます：

- **[OpenLogi GUI](../crates/openlogi-gui)** —— GPUI 製デスクトップアプリ：クリック可能なホットスポット付きのインタラクティブなマウス図、ボタンごとのアクションピッカー（組み込みアクション + TOML 設定で作成するカスタムショートカット）、DPI プリセット、SmartShift、デバイスごとのスクロール反転、RGB キーボード照明、アプリごとのプロファイル、ライブデバイスカルーセル、20 言語にローカライズされた設定ウィンドウ。
- **[OpenLogi agent](../crates/openlogi-agent)** —— 入力フックとすべてのデバイス I/O を所有するバックグラウンドサービス。GUI は純粋な IPC クライアントで、必要時に agent を起動します。
- **[OpenLogi CLI](../crates/openlogi-cli)** —— ヘッドレスなデバイス一覧（`list`）、アセット同期、デバイス診断のサブコマンドを備えた CLI。

すべてはローカルで完結します：バインディングはプレーンな TOML ファイルに保存され、agent が OS の入力フックでボタン入力を再マッピングし、DPI、SmartShift、スクロール、照明の変更を HID++ で直接デバイスに書き込みます。

macOS、Linux、Windows をサポートしています。Windows は最新の移植で、Windows 11 実機上でエンドツーエンド検証済みですが、macOS / Linux ビルドより粗削りな部分が残る可能性があります。[ロードマップ](#ロードマップ)を参照してください。

## Options+ を超えて

OpenLogi にできて Options+ にできないこと：

- **Linux で動く。** Options+ は macOS と Windows のみ。OpenLogi は Linux をファーストクラスで扱います：evdev/uinput フック、udev ルール、systemd ユーザーユニット、`.deb` / `.rpm` / `.pkg.tar.zst` パッケージ。
- **ジェスチャーボタンを移せる。** どの物理ボタンがジェスチャー役を担うか —— 専用ジェスチャーボタン、ミドル、戻る、進む —— を選べ、方向ごとのスワイプバインディングを設定でき、ジェスチャーを完全にオフにもできます。Options+ はジェスチャーを専用ジェスチャーボタンに固定しています。
- **設定がプレーンテキスト。** すべてが 1 つの TOML ファイル。読めて、diff できて、バージョン管理に入れられて、マシン間でコピーできます。
- **スクリプトで叩ける。** 本物の CLI：デバイス一覧、アセットのプリフェッチ、デバイス上での HID++ 診断（フィーチャー / コントロールダンプ、DPI / SmartShift のラウンドトリップ検査、キーボード照明チェック）。
- **軽量なまま。** ネイティブ Rust + GPUI バイナリ —— Electron スイートも常駐アップデーターもアカウントもテレメトリもなし。

## ロードマップ

| 機能 | 状態 |
|---|---|
| Bolt レシーバーの発見 + ペアリング済みデバイスの一覧（CLI + GUI） | ✅ |
| Unifying レシーバー（Bolt に置き換えられた旧プロトコル） | ✅ |
| Bluetooth 直結 / 有線デバイス（レシーバーなし） | ✅ |
| バッテリー残量 / 充電状態 | ✅（オンラインのデバイス） |
| インタラクティブ GUI：カルーセル、マウス図、アクションピッカー | ✅ macOS + Linux + Windows |
| OS 入力フックによるボタン再マッピング | ✅ macOS + Linux + Windows |
| 組み込みアクションカタログ + カスタムキーボードショートカット（TOML で作成） | ✅ macOS + Linux + Windows¹ |
| DPI 制御 + プリセット + サイクル / プリセット指定アクション（HID++ `0x2201`） | ✅ |
| SmartShift ホイール：モード切替 + 感度 + 永続ラチェットパネル（HID++ `0x2111`） | ✅ |
| デバイスごとのネイティブスクロール反転（HID++ `0x2121`） | ✅（対応デバイス） |
| 静的 RGB キーボード照明（HID++ `0x8070` / `0x8080`） | ✅（対応デバイス） |
| アプリごとのプロファイルオーバーレイ（フォーカスで自動切替） | ✅ macOS + Windows、🟡 Linux（X11 / XWayland のみ） |
| 設定ウィンドウ：ログイン時起動、更新、権限、言語、外観 | ✅ macOS + Linux + Windows |
| Agent ステータスアイコン | ✅ macOS メニューバー + Windows トレイ；Linux には非該当 |
| UI のローカライズ（20 言語：da、de、el、en、es、fi、fr、it、ja、ko、nb、nl、pl、pt-BR、pt-PT、ru、sv、zh-CN、zh-HK、zh-TW） | ✅ |
| Linux パッケージング：udev ルール、systemd ユニット、`.deb` / `.rpm` / `.pkg.tar.zst` | ✅ Linux |
| ジェスチャーボタンの方向別バインディング + ライブキャプチャ | ✅（デバイス機能に依存） |
| ミドル / モードシフト / サムホイールボタンのキャプチャ | ✅ ミドルは全プラットフォーム；モードシフト / サムホイールはデバイス機能に依存 |
| Windows（agent、GUI、イベントフック、インストーラー） | ✅ Windows 11 実機で検証済み；新しい移植のため互換性を継続改善中 |

¹ Linux のメディアキーアクションは D-Bus MPRIS を使います。少数の macOS 固有アクションには Linux で汎用的な対応物がなく、no-op になります。Windows では利用可能なプラットフォームアクションをネイティブの対応機能に割り当てます。

## インストール

> [!IMPORTANT]
> 先に **Logi Options+** を終了してください —— 両者は HID++ アクセスを奪い合い、1 つのレシーバーを同時に所有できるのは片方だけです。

### macOS

macOS 13 以降が必要です。

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

# Arch Linux
sudo pacman -U openlogi-*.pkg.tar.zst
```

パッケージは `x86_64`/`amd64` と `arm64`/`aarch64` の両方で公開されています。

パッケージは udev ルールをインストールし、`sudo` なしで `/dev/hidraw*` と `/dev/uinput` にアクセスできるようにします。インストール後、ユーザーのバックグラウンドエージェントを有効化してください：

```sh
systemctl --user enable --now openlogi-agent.service
```

手動 / ソースからのインストールや systemd のないディストリビューションは [INSTALL-linux.md](INSTALL-linux.md) を参照。

### Windows

各リリースには署名済みポータブル `.zip` とユーザー単位の `.msi` インストーラー（x86_64 / arm64）が付属します。どちらも GUI（`OpenLogi.exe`）と、すべてのデバイス I/O を所有するバックグラウンド agent（`openlogi-agent.exe`）を同梱します。ポータブル zip では 2 ファイルを同じ場所に置いてください。そうしないと GUI は接続先を失います。

Windows サポートは動作しており、有線キーボードと Unifying レシーバー接続のマウスを使い、MSI のインストール、インプレースアップグレード、アンインストールを含めて Windows 11 実機でエンドツーエンド検証済みです。macOS 版より新しいため、問題があれば[報告](https://github.com/AprilNEA/OpenLogi/issues)してください。agent はシステムトレイアイコン（メインウィンドウを表示 / 終了）を表示し、メインウィンドウを閉じてもアプリを開けます。Windows で無効にするには TOML の `[app_settings]` ブロックで `show_in_menu_bar = false` を設定し、agent を再起動してください。GUI の切り替えは現在 macOS 専用です。

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

## リポジトリの活動

![Repobeats analytics image](https://repobeats.axiom.co/api/embed/4a0b576a03e9d528ad31ccf4797a1286c045d021.svg "Repobeats analytics image")
