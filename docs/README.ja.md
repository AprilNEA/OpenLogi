> [!WARNING]
> **OpenLogi は現在活発に開発中**であり、まだ安定していません —— 機能や設定は今後も変わる可能性があります。リポジトリに **Star** ⭐ と **Watch** 👀 を付けて、新しいリリースの通知を受け取りましょう。

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Rust 製のネイティブでローカルファーストな Logitech Options+ 代替 🦀<br/>HID++ と UVC で Logitech のマウス・キーボード・ウェブカメラの能力を最大限に引き出す</strong></p>


<div align="center">
    <a href="https://github.com/AprilNEA/OpenLogi/releases/latest" target="_blank">
    <img alt="GitHub release" src="https://img.shields.io/github/v/release/AprilNEA/OpenLogi?style=flat"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/releases" target="_blank">
    <img alt="GitHub downloads" src="https://img.shields.io/github/downloads/AprilNEA/OpenLogi/total.svg?style=flat"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/commits" target="_blank">
    <img alt="GitHub commit" src="https://img.shields.io/github/commit-activity/m/AprilNEA/OpenLogi?style=flat"></a>
    <a href="#ライセンス">
    <img alt="License" src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue?style=flat"></a>
    <img alt="Hits" src="https://hits.aprilnea.com/hits?url=https://github.com/aprilnea/openlogi">
</div>

<div align="center">
    <a href="https://x.com/AprilNEA" target="_blank">
    <img alt="X" src="https://img.shields.io/badge/follow-AprilNEA-black?style=flat&logo=x"></a>
    <a href="https://t.me/+VDtkR5OSAT04NzVh" target="_blank">
    <img alt="telegram" src="https://img.shields.io/badge/chat-telegram-blueviolet?style=flat&logo=Telegram"></a>
</div>

<div align="center">
    <a href="../README.md">
    <img alt="English" src="https://img.shields.io/badge/English-lightgrey?style=flat"></a>
    <a href="README.zh-CN.md">
    <img alt="简体中文" src="https://img.shields.io/badge/%E7%AE%80%E4%BD%93%E4%B8%AD%E6%96%87-lightgrey?style=flat"></a>
    <a href="README.ja.md">
    <img alt="日本語" src="https://img.shields.io/badge/%E6%97%A5%E6%9C%AC%E8%AA%9E-blue?style=flat"></a>
    <a href="README.de.md">
    <img alt="Deutsch" src="https://img.shields.io/badge/Deutsch-lightgrey?style=flat"></a>
    <a href="README.fr.md">
    <img alt="Français" src="https://img.shields.io/badge/Fran%C3%A7ais-lightgrey?style=flat"></a>
    <a href="README.ko.md">
    <img alt="한국어" src="assets/badge-lang-ko.svg"></a>
    <a href="README.ru.md">
    <img alt="Русский" src="https://img.shields.io/badge/%D0%A0%D1%83%D1%81%D1%81%D0%BA%D0%B8%D0%B9-lightgrey?style=flat"></a>
    <a href="README.es.md">
    <img alt="Español" src="https://img.shields.io/badge/Espa%C3%B1ol-lightgrey?style=flat"></a>
    <a href="README.pt-BR.md">
    <img alt="Português" src="https://img.shields.io/badge/Portugu%C3%AAs-lightgrey?style=flat"></a>
    <a href="README.tr.md">
    <img alt="Türkçe" src="https://img.shields.io/badge/T%C3%BCrk%C3%A7e-lightgrey?style=flat"></a>
</div>

<p align="center">
    <a href="https://trendshift.io/repositories/42303" target="_blank">
    <img src="https://trendshift.io/api/badge/repositories/42303" alt="AprilNEA%2FOpenLogi | Trendshift" width="250" height="55"/></a>
    <a href="https://www.producthunt.com/products/openlogi?embed=true&amp;utm_source=badge-featured&amp;utm_medium=badge&amp;utm_campaign=badge-openlogi" target="_blank" rel="noopener noreferrer">
    <picture>
        <source media="(prefers-color-scheme: dark)" srcset="https://api.producthunt.com/widgets/embed-image/v1/top-post-badge.svg?post_id=openlogi&amp;theme=dark&amp;period=daily">
        <source media="(prefers-color-scheme: light)" srcset="https://api.producthunt.com/widgets/embed-image/v1/top-post-badge.svg?post_id=openlogi&amp;theme=light&amp;period=daily">
        <img alt="OpenLogi - A local-first alternative to Logitech Options+ | Product Hunt" width="250" height="54" src="https://api.producthunt.com/widgets/embed-image/v1/top-post-badge.svg?post_id=openlogi&amp;theme=light&amp;period=daily">
    </picture></a>
</p>

> **Options+ にうんざり？OpenLogi をどうぞ。**

macOS、Linux、Windows に対応。

---

## Options+ を超えて

OpenLogi にできて Options+ にできないこと：

- **軽量なまま。** ネイティブ Rust + GPUI。
- **Linux で動く。** Linux は OpenLogi のファーストクラスプラットフォームです。
- **対応ボタンでジェスチャー。** 対応するコントロールにジェスチャー操作を割り当てられ、ジェスチャーを完全にオフにもできます。
- **設定がプレーンテキスト。** すべてが 1 つの TOML ファイルに収まり、好きな方法でマシン間を同期できます。
- **スクリプトで叩ける。** GUI に加えて本物の CLI も。DPI、SmartShift、Fn Lock、レポートレートを操作する JSON の[自動化 API](USAGE.md#automation-api) と、ハードウェア診断を備えています。

## 機能一覧

<p align="center">
    <img src="https://assets.openlogi.org/screenshots/macos/light/devices-grid.png" width="860" alt="6 台の Logitech デバイス（マウス、キーボード、Litra ライト）が並ぶ OpenLogi のデバイス一覧">
</p>

### デバイス

- Logi Bolt / Unifying レシーバー、Bluetooth、有線接続に対応し、バッテリー残量と充電状態を表示
- バッテリー低下を画面下部に通知。放電ごとに 1 回だけで、OS の通知権限は不要
- キーボードで Caps Lock をオン / オフすると画面上にインジケーターを表示。Windows と Linux では Num Lock と Scroll Lock にも対応

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/easy-switch.png" width="400" alt="ペアリング済みのコンピューター 3 台を一覧表示する Easy-Switch カード"></td>
        <td width="50%"><b>Easy-Switch</b><ul><li>各チャンネルにペアリングされたコンピューターを、OS と接続方式つきで確認</li><li>対応デバイスではチャンネル名を変更可能</li></ul></td>
    </tr>
</table>

### ボタンとアクション

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/buttons.png" width="400" alt="MX Master 4 のボタン割り当て画面。ジェスチャーボタンのインスペクターを表示中"></td>
        <td width="50%"><b>あらゆるボタンを再マッピング</b><ul><li>組み込みアクションカタログ、カスタムショートカット、テキスト入力、複数ステップのワークフロー</li><li>タップと長押しで別々のアクション、ボタンを離すと解除される長押しトーク用のキーコンボ</li><li>ミドルクリック、戻る / 進む、DPI / モードシフト、ジェスチャーボタン、触覚パネルでのジェスチャー</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/actions-ring.png" width="400" alt="カーソルを囲む 8 スロットの Actions Ring エディター"></td>
        <td width="50%"><b>Actions Ring</b><ul><li>カーソルの周りにリング状に並ぶ 8 つのアクション</li><li>アプリごとに独自のレイアウト</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/keys.png" width="400" alt="3 つのキーを Launchpad、画面ロック、スクリーンショットに割り当てた MX Keys S"></td>
        <td width="50%"><b>キーボードのキー</b><ul><li>メディア、絵文字、スクリーンショットなどの専用キーをマウスボタンと同じように割り当て</li><li>キーを 1 つだけデバイスの既定値に戻すことも、列全体をリセットすることも可能</li></ul></td>
    </tr>
</table>

アプリごとのプロファイルは、フォーカス中のアプリに合わせて切り替わります。`[apps]` エイリアスを使えば、1 つのプロファイルをプラットフォームをまたいで同じアプリに適用できます。

### ポインターとスクロール

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/dpi.png" width="400" alt="DPI スライダーのあるポインター調整カード"></td>
        <td width="50%"><b>ポインター</b><ul><li>DPI プリセットと、サイクル / プリセット指定アクション</li><li>ポインタ速度とレポートレート</li><li>精密モード：ボタン 1 つでマウスを最低速度に切り替え、もう一度押すと元に戻る</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/smartshift.png" width="400" alt="ホイールモード、切り替え感度、スクロールの重さを設定する SmartShift カード"></td>
        <td width="50%"><b>SmartShift</b><ul><li>ホイールモードと切り替え感度</li><li>SmartShift Enhanced 対応マウスではスクロールの重さも調整</li><li>常時ラチェットのスイッチ</li></ul></td>
    </tr>
    <tr>
        <td colspan="2"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/scrolling.png" width="820" alt="スクロール反転、ホイール解像度、スムーズホイールを設定するスクロールカード"><br><b>スクロール</b><ul><li>デバイスごとのスクロール反転（ネイティブまたはソフトウェア）</li><li>ホイール解像度とスムーズホイールスクロール</li><li>サムホイールのないマウスでも、戻る / 進むを押しながらホイールを回すと横スクロール</li></ul></td>
    </tr>
</table>

### キーボードとライト

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/keys-backlight.png" width="400" alt="MX Keys S のキーとバックライトのカード"></td>
        <td width="50%"><b>キーボード</b><ul><li>Fn Lock と、ホスト OS のレイアウトを維持するマルチプラットフォームキーボード</li><li>バックライトの明るさ、明るさの自動調整、省電力、キーの点灯時間</li><li>バックライトの明るさを変更時に画面に表示</li><li>対応キーボードでの静的 RGB</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/litra.png" width="400" alt="明るさと色温度を設定する Litra Glow のライトパネル"></td>
        <td width="50%"><b>Litra ライト</b><ul><li>電源、明るさ、色温度</li><li>必要ならカメラの使用に連動して点灯</li><li>ライト本体で行った変更を保持</li></ul></td>
    </tr>
</table>

### ウェブカメラ

- あらゆる Logitech UVC ウェブカメラ（Brio、StreamCam、C920 シリーズなど）にプラグアンドプレイで対応
- ライブプレビューは見ているあいだだけカメラを使い、離れると LED も消灯
- ズーム、パン、チルト、フォーカス、露出、ホワイトバランスなどあらゆる UVC コントロールをハードウェアに書き込むため、Meet、Zoom、OBS にも変更が反映
- 組み込みの「デフォルト / 配信 / ビデオ通話」プロファイルに加えて独自のスナップショットを保存でき、カメラごとに保持

対応機能は各デバイスが報告する内容によって異なります。アプリには、そのデバイスが対応するコントロールだけが表示されます。

### プラットフォーム対応状況

| 機能 | macOS | Windows | Linux |
| --- | --- | --- | --- |
| マウスボタンの再マッピングとジェスチャー | ✓ | ✓ | ✓ |
| キーボードの F キー再マッピング | ✓ | ✓ | — |
| アプリごとのプロファイル | ✓ | ✓ | X11、wlroots（sway、Hyprland）、GNOME（[コンパニオン拡張機能](../crates/openlogi-hook/gnome-shell-extension/README.md)が必要） |
| マウスのプロファイルがポインター下のウィンドウに追従 | ✓ | ✓ | X11 のみ |
| メディアキー | ネイティブ | ネイティブ | D-Bus MPRIS |
| ウィンドウマネージャー操作（デスクトップ、Mission Control など） | ネイティブ | ネイティブの同等機能 | GNOME、KDE (KWin)、Hyprland。macOS 専用の一部操作は何もしない |
| メニューバー / トレイアイコン | ✓ | ✓ | — |
| アプリ内アップデート | Sparkle | 組み込み | 組み込み |

## インストール

> [!IMPORTANT]
> 先に **Logi Options+**（Linux では Solaar も）を終了してください。1 つのレシーバーを同時に所有できるアプリケーションは 1 つだけです。

| プラットフォーム | システム | アーキテクチャ |
| --- | --- | --- |
| macOS | macOS 13 以降 | Apple シリコン、Intel |
| Linux | GLIBC 2.35 以降 | x86_64, arm64 |
| Windows | Windows 11 で動作確認済み | x86_64, arm64 |

すべてのダウンロードには minisign 署名と `SHA256SUMS` のエントリが付属します。ソースからのビルドは [DEVELOPMENT.md](DEVELOPMENT.md) を参照してください。

### macOS

<p>
    <a href="https://openlogi.org/download/mac-arm64"><img src="https://assets.openlogi.org/brand/buttons/download-macos-arm64.png" width="260" alt="Mac 版をダウンロード：Apple silicon（.dmg）"></a>
    &nbsp;
    <a href="https://openlogi.org/download/mac-x64"><img src="https://assets.openlogi.org/brand/buttons/download-macos-x64.png" width="260" alt="Mac 版をダウンロード：Intel（.dmg）"></a>
</p>

または Homebrew で：

```sh
brew install --cask openlogi
```

OpenLogi を開き、macOS に求められたら **アクセシビリティ** と **入力監視** を許可してください。以降、OpenLogi は Sparkle で自動的に更新されます。

### Linux

<p>
    <a href="https://openlogi.org/download/linux"><img src="https://assets.openlogi.org/brand/buttons/download-linux-deb.png" width="260" alt="Linux 版をダウンロード：Debian、Ubuntu（.deb）"></a>
    &nbsp;
    <a href="https://openlogi.org/download/linux-rpm"><img src="https://assets.openlogi.org/brand/buttons/download-linux-rpm.png" width="260" alt="Linux 版をダウンロード：Fedora、SUSE（.rpm）"></a>
    &nbsp;
    <a href="https://openlogi.org/download/linux-arch"><img src="https://assets.openlogi.org/brand/buttons/download-linux-arch.png" width="260" alt="Linux 版をダウンロード：Arch（.pkg.tar.zst）"></a>
</p>

または、1 つのコマンドでディストリビューションに合ったパッケージをインストールできます。スクリプトは minisign 署名を検証し、agent を起動します。事前に `minisign` をインストールしてください：

```sh
curl -fsSL https://github.com/AprilNEA/OpenLogi/releases/latest/download/install.sh | sh
```

Arch Linux では、AUR パッケージ [`openlogi`](https://aur.archlinux.org/packages/openlogi) がリリースをソースからビルドします:

```sh
yay -S openlogi
```

実行前にスクリプトを確認する方法や、AUR パッケージ、Flatpak、NixOS、systemd のないディストリビューションについては [INSTALL-linux.md](INSTALL-linux.md) を参照してください。

### Windows

<p>
    <a href="https://openlogi.org/download/windows"><img src="https://assets.openlogi.org/brand/buttons/download-windows-msi.png" width="260" alt="Windows 版をダウンロード：インストーラー（.msi）"></a>
    &nbsp;
    <a href="https://openlogi.org/download/windows-zip"><img src="https://assets.openlogi.org/brand/buttons/download-windows-zip.png" width="260" alt="Windows 版をダウンロード：ポータブル（.zip）"></a>
</p>

## 使い方（CLI）

[USAGE.md](USAGE.md) を参照

## 設定

設定はプレーンな TOML を使い、保存時もシンボリックリンクされた設定ファイルを維持します。[CONFIGURATION.md](CONFIGURATION.md) を参照。

## 開発

[DEVELOPMENT.md](DEVELOPMENT.md) を参照。[macOS 入力フックの安全性](DEVELOPMENT.md#macos-input-hook-safety)についても記載しています。

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
