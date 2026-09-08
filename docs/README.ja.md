> [!WARNING]
> **OpenLogi は現在活発に開発中**であり、まだ安定していません —— 機能や設定は今後も変わる可能性があります。リポジトリに **Star** ⭐ と **Watch** 👀 を付けて、新しいリリースの通知を受け取りましょう。

<h4 align="right"><a href="../README.md">English</a> | <a href="README.zh-CN.md">简体中文</a> | <strong>日本語</strong> | <a href="README.de.md">Deutsch</a> | <a href="README.fr.md">Français</a> | <a href="README.ko.md">한국어</a></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Rust 製のネイティブでローカルファーストな Logitech Options+ 代替 🦀<br/>HID++ と UVC で Logitech のマウス・キーボード・ウェブカメラの能力を最大限に引き出す</strong></p>


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

macOS、Linux、Windows に対応。

---

## Options+ を超えて

OpenLogi にできて Options+ にできないこと：

- **軽量なまま。** ネイティブ Rust + GPUI。
- **Linux で動く。** Linux は OpenLogi のファーストクラスプラットフォームです。
- **ジェスチャーボタンを自由に。** どの物理ボタンにでもジェスチャー役を割り当てられ、ジェスチャーを完全にオフにもできます。
- **設定がプレーンテキスト。** すべてが 1 つの TOML ファイルに収まり、好きな方法でマシン間を同期できます。
- **スクリプトで叩ける。** GUI に加えて本物の CLI も。

## 機能一覧

- Logi Bolt / Unifying レシーバー、Bluetooth、有線で接続されたデバイスに対応し、バッテリー残量と充電状態を表示
- OS 入力フックによるボタン再マッピング：組み込みアクションカタログ + カスタムキーボードショートカット（TOML で作成）¹
- アプリごとのプロファイルオーバーレイ（フォーカスで自動切替；macOS + Windows、Linux は GNOME（同梱の Shell 拡張機能）、wlroots 系コンポジタ、または X11 / XWayland）
- Litra ライト：電源、明るさ、色温度。カメラの使用状況に連動した自動オン / オフも可能

**マウス**

- ミドル、モードシフト、サムホイールなどのボタンのキャプチャと再マッピング（ミドルは全プラットフォーム対応、その他はデバイス機能に依存）
- 方向別ジェスチャーバインディングとライブキャプチャ（対応する任意のボタンに設定可能）
- Actions Ring：カーソル中心の 8 スロットアクションオーバーレイ（`ShowActionsRing`）、アプリごとのレイアウトに対応
- DPI 制御：プリセット + サイクル / プリセット指定アクション（`0x2201`）
- SmartShift ホイール：モード切替、感度、永続ラチェットパネル（`0x2111`）
- デバイスごとのネイティブスクロール反転（`0x2121`、対応デバイス）

**キーボード**

- F キーのグローバル再マッピング：マウスと同じアクションカタログに加え、テキスト入力、キーコンボ、複数ステップのワークフローなどのパワーユーザーアクション（macOS + Windows）
- 静的 RGB ライティング（`0x8070` / `0x8080`、対応デバイス）

**カメラ**

- あらゆる Logitech UVC ウェブカメラ（Brio、StreamCam、C920 シリーズなど）にプラグアンドプレイで対応
- ライブプレビュー：見ているあいだだけカメラを起動し、離れると完全に解放されて LED も消灯
- 画質コントロールは UVC ハードウェアへ直接書き込み：ズーム、フォーカス、露出、明るさ、コントラスト、彩度、シャープネス、ホワイトバランス、色合い。フォーカス / 露出 / ホワイトバランスは自動モード切替付きで、Meet / Zoom / OBS などカメラを使うすべてのアプリに反映
- ワンクリックプロファイル：組み込みの「デフォルト / 配信 / ビデオ通話」に加えてカスタムスナップショットを保存可能。設定はカメラごとに保持され、次回表示時にハードウェアへ書き戻されます

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

# Arch Linux — AUR から（システムと一緒に更新されます）:
paru -S openlogi-bin   # or: yay -S openlogi-bin
# …またはリリースパッケージを直接インストール:
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

- **Windows・カメラ・i18n**: [@davidbudnick](https://github.com/davidbudnick) —— キーボード RGB 対応、Windows 対応、Logitech ウェブカメラ対応
- **Linux 移植**: [@cserby](https://github.com/cserby) —— Linux 対応
- [Solaar](https://github.com/pwr-Solaar/Solaar) by [@pwr](https://github.com/pwr) —— オープンソースの HID++ 実装
- [Mouser](https://github.com/TomBadash/Mouser) by [@TomBadash](https://github.com/TomBadash) —— ローカル完結・アカウント不要の Options+ 代替

## ライセンス

本リポジトリのコードは、以下のいずれかのライセンスを選択できます：

- Apache License 2.0（[LICENSE-APACHE](../LICENSE-APACHE)）
- MIT ライセンス（[LICENSE-MIT](../LICENSE-MIT)）

### サードパーティコード

`crates/openlogi-hidpp` は [`hidpp`](https://crates.io/crates/hidpp)（作者 [@lus](https://github.com/lus)）の vendored fork で、0BSD ライセンスです。

### ロゴとブランドアセット

OpenLogi のロゴをデザインしてくれた [@kubai087](https://github.com/kubai087) に感謝します。OpenLogi のロゴとアプリアイコン —— [`design/`](../design/) 配下のブランドアセット —— は © 2026 AprilNEA が全権利を留保しており、上記の MIT/Apache ライセンスの対象外です。[`design/LICENSE`](../design/LICENSE) を参照してください。コードをフォークしても OpenLogi の名称・ロゴ・アイコンの使用権は付与されません。事前の書面による許可なく、ご自身のプロジェクト、フォーク、配布物を表すために使用しないでください。

---

**Logitech とは無関係です。** 「Logitech」「MX Master」「Options+」は Logitech International S.A. の商標です。

## リポジトリの活動

![Repobeats analytics image](https://repobeats.axiom.co/api/embed/4a0b576a03e9d528ad31ccf4797a1286c045d021.svg "Repobeats analytics image")
