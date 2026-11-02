> [!WARNING]
> **OpenLogi hâlâ aktif geliştirme aşamasında** ve henüz kararlı değil — özellikler ve ayarlar değişebilir. Yeni bir sürüm çıktığında haberdar olmak için depoya **Star** ⭐ verin ve **Watch** 👀 edin.

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Rust 🦀 ile yazılmış, Logitech Options+'a yerel ve tamamen çevrimdışı çalışan bir alternatif<br/>Logitech fare, klavye ve web kameralarının tüm gücünü HID++ ve UVC üzerinden açığa çıkarır</strong></p>

<div align="center">
    <a href="https://github.com/AprilNEA/OpenLogi/releases/latest" target="_blank">
    <img alt="GitHub release" src="https://img.shields.io/github/v/release/AprilNEA/OpenLogi?style=flat"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/releases" target="_blank">
    <img alt="GitHub downloads" src="https://img.shields.io/github/downloads/AprilNEA/OpenLogi/total.svg?style=flat"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/commits" target="_blank">
    <img alt="GitHub commit" src="https://img.shields.io/github/commit-activity/m/AprilNEA/OpenLogi?style=flat"></a>
    <a href="#lisans">
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
    <img alt="日本語" src="https://img.shields.io/badge/%E6%97%A5%E6%9C%AC%E8%AA%9E-lightgrey?style=flat"></a>
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
    <img alt="Türkçe" src="https://img.shields.io/badge/T%C3%BCrk%C3%A7e-blue?style=flat"></a>
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

> **Options+'tan bıktınız mı? OpenLogi'yi deneyin.**

macOS, Linux ve Windows'ta çalışır.

---

## Options+'ın Ötesinde

OpenLogi'nin Options+'ta bulamayacağınız özellikleri:

- **Hafif kalır.** Native Rust + GPUI.
- **Linux'ta çalışır.** Linux, OpenLogi'de birinci sınıf bir platformdur.
- **Desteklenen tuşlarda jestler.** Desteklenen kontrollere jest eylemleri atayın — isterseniz jestleri tamamen kapatın.
- **Düz metin ayar dosyası.** Her şey tek bir TOML dosyasında; makineler arasında istediğiniz şekilde senkronize edin.
- **Betiklenebilir.** Arayüzün yanında gerçek bir CLI: DPI, SmartShift, Fn-lock ve raporlama hızı için JSON tabanlı bir [otomasyon API'si](USAGE.md#automation-api), ayrıca donanım tanılama araçları.

## Özellikler

<p align="center">
    <img src="https://assets.openlogi.org/screenshots/macos/light/devices-grid.png" width="860" alt="Altı Logitech cihazını gösteren OpenLogi cihaz listesi: fareler, bir klavye ve bir Litra ışığı">
</p>

### Cihazlar

- Logi Bolt ve Unifying alıcıları, Bluetooth ve kablolu bağlantılar; pil seviyesi ve şarj durumu dahil
- Ekranın altında düşük pil uyarıları; her deşarjda bir kez gösterilir ve işletim sisteminden bildirim izni gerektirmez
- Bir klavye Caps Lock'u açıp kapattığında ekranda bir gösterge; Windows ve Linux'ta Num Lock ve Scroll Lock için de

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/easy-switch.png" width="400" alt="Eşleştirilmiş üç bilgisayarı listeleyen Easy-Switch kartı"></td>
        <td width="50%"><b>Easy-Switch</b><ul><li>Her kanalda hangi bilgisayarın eşleştirildiğini, işletim sistemi ve bağlantısıyla birlikte görün</li><li>İzin veren cihazlarda bir kanalı yeniden adlandırın</li></ul></td>
    </tr>
</table>

### Tuşlar ve eylemler

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/buttons.png" width="400" alt="MX Master 4 için tuş eşleme ekranı, jest tuşu denetçisi açık"></td>
        <td width="50%"><b>Herhangi bir tuşu yeniden atayın</b><ul><li>Hazır eylem kataloğu, özel kısayollar, yazılı metin veya çok adımlı iş akışları</li><li>Ayrı dokunma ve basılı tutma eylemleri; tuş bırakılınca serbest kalan bas-konuş kombinasyonları</li><li>Orta Tık, Geri/İleri, DPI/ModeShift, jest tuşu ve dokunsal panelde jestler</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/actions-ring.png" width="400" alt="İmlecin etrafında sekiz yuvalı Actions Ring düzenleyicisi"></td>
        <td width="50%"><b>Actions Ring</b><ul><li>İmlecin etrafında halka şeklinde sekiz eylem</li><li>Her uygulama için ayrı bir düzen</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/keys.png" width="400" alt="Üç tuşu Launchpad, Ekranı Kilitle ve Ekran Görüntüsü eylemlerine atanmış MX Keys S"></td>
        <td width="50%"><b>Klavye tuşları</b><ul><li>Medya, emoji, ekran görüntüsü ve diğer özel tuşları fare tuşları gibi atayın</li><li>Tek bir tuşu cihaz varsayılanına döndürün veya tüm satırı sıfırlayın</li></ul></td>
    </tr>
</table>

Uygulama başına profiller, odaktaki uygulamayla birlikte değişir. Bir `[apps]` takma adı, tek bir profilin bir uygulamayı tüm platformlarda takip etmesini sağlar.

### İşaretçi ve kaydırma

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/dpi.png" width="400" alt="DPI kaydırıcısı içeren işaretçi ayar kartı"></td>
        <td width="50%"><b>İşaretçi</b><ul><li>Döngü ve Önayar-Belirle eylemleriyle DPI önayarları</li><li>İşaretçi hızı ve raporlama hızı</li><li>Hassas Mod: tek bir tuş fareyi en yavaş hızına alır ve geri döndürür</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/smartshift.png" width="400" alt="Tekerlek modu, hassasiyet ve kaydırma kuvveti içeren SmartShift kartı"></td>
        <td width="50%"><b>SmartShift</b><ul><li>Tekerlek modu ve hassasiyet</li><li>SmartShift Enhanced farelerde kaydırma kuvveti</li><li>Kalıcı kademeli mod anahtarı</li></ul></td>
    </tr>
    <tr>
        <td colspan="2"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/scrolling.png" width="820" alt="Ters çevirme, tekerlek çözünürlüğü ve yumuşak tekerlek içeren kaydırma kartı"><br><b>Kaydırma</b><ul><li>Cihaz başına kaydırma yönü ters çevirme, yerel olarak veya yazılımla</li><li>Tekerlek çözünürlüğü ve yumuşak tekerlek kaydırma</li><li>Parmak tekerleği olmayan farelerde Geri veya İleri'yi basılı tutup tekerleği çevirerek yatay kaydırma</li></ul></td>
    </tr>
</table>

### Klavye ve aydınlatma

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/keys-backlight.png" width="400" alt="MX Keys S için Tuşlar ve Arka aydınlatma kartları"></td>
        <td width="50%"><b>Klavye</b><ul><li>Fn Lock ve ana bilgisayarın işletim sistemi düzeninde kalan çok platformlu klavyeler</li><li>Arka aydınlatma seviyesi, otomatik parlaklık, pil tasarrufu ve tuşların ne kadar süre aydınlık kalacağı</li><li>Arka aydınlatma seviyesi değiştikçe ekranda gösterilir</li><li>Desteklenen klavyelerde statik RGB</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/litra.png" width="400" alt="Parlaklık ve renk sıcaklığı içeren Litra Glow aydınlatma paneli"></td>
        <td width="50%"><b>Litra ışıkları</b><ul><li>Güç, parlaklık ve renk sıcaklığı</li><li>İsterseniz kamerayla birlikte açılır</li><li>Işığın kendisi üzerinde yapılan değişikliği korur</li></ul></td>
    </tr>
</table>

### Web kameraları

- Herhangi bir Logitech UVC web kamerası (Brio, StreamCam, C920 serisi, …), tak-çalıştır
- Kamerayı yalnızca siz izlerken kullanan canlı önizleme; ekrandan çıktığınızda LED söner
- Yakınlaştırma, kaydırma, eğme, odak, pozlama, beyaz dengesi ve diğer tüm UVC kontrolleri; donanıma yazılır, böylece Meet, Zoom ve OBS değişikliği görür
- Yerleşik Varsayılan / Yayın / Görüntülü arama profilleri ile kendi anlık görüntüleriniz; kamera başına saklanır

Özellik kapsamı her cihazın bildirdiklerine bağlıdır; uygulama yalnızca cihazın desteklediği kontrolleri gösterir.

### Platform desteği

| Özellik | macOS | Windows | Linux |
| --- | --- | --- | --- |
| Fare tuşu yeniden atama ve jestler | ✓ | ✓ | ✓ |
| Klavye F-tuşu yeniden atama | ✓ | ✓ | — |
| Uygulama başına profiller | ✓ | ✓ | X11, wlroots (sway, Hyprland), [yardımcı eklentiyle](../crates/openlogi-hook/gnome-shell-extension/README.md) GNOME |
| Fare profili işaretçinin altındaki pencereyi takip eder | ✓ | ✓ | Yalnızca X11 |
| Medya tuşları | Yerel | Yerel | D-Bus MPRIS |
| Pencere yöneticisi eylemleri (masaüstleri, Mission Control, …) | Yerel | Yerel karşılıklar | GNOME, KDE (KWin), Hyprland; macOS'a özel bazı eylemler hiçbir şey yapmaz |
| Menü çubuğu / sistem tepsisi simgesi | ✓ | ✓ | — |
| Uygulama içi güncellemeler | Sparkle | Yerleşik | Yerleşik |

## Kurulum

> [!IMPORTANT]
> Önce **Logi Options+**'tan (Linux'ta Solaar'dan da) çıkın. Bir alıcıyı aynı anda yalnızca bir uygulama kullanabilir.
> macOS'ta **Karabiner-Elements** de değiştirdiği Bluetooth klavyeyi kilitler. Karabiner-Elements → Settings → Devices bölümünde o klavye için **Modify events** seçeneğini kapatın.

| Platform | Sistem | Mimari |
| --- | --- | --- |
| macOS | macOS 13 veya üzeri | Apple silicon, Intel |
| Linux | GLIBC 2.35 veya üzeri | x86_64, arm64 |
| Windows | Windows 11'de test edildi | x86_64, arm64 |

Her indirmenin bir minisign imzası ve `SHA256SUMS` içinde bir girdisi vardır. Kaynaktan derlemek için [DEVELOPMENT.md](DEVELOPMENT.md)'ye bakın.

### macOS

<p>
    <a href="https://openlogi.org/download/mac-arm64"><img src="https://assets.openlogi.org/brand/buttons/download-macos-arm64.png" width="260" alt="Mac için indir: Apple silicon (.dmg)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/mac-x64"><img src="https://assets.openlogi.org/brand/buttons/download-macos-x64.png" width="260" alt="Mac için indir: Intel (.dmg)"></a>
</p>

Veya Homebrew ile:

```sh
brew install --cask openlogi
```

OpenLogi'yi açın ve macOS sorduğunda **Erişilebilirlik** ve **Girdi İzleme** izinlerini verin. Ardından OpenLogi kendini Sparkle ile günceller.

### Linux

<p>
    <a href="https://openlogi.org/download/linux"><img src="https://assets.openlogi.org/brand/buttons/download-linux-deb.png" width="260" alt="Linux için indir: Debian, Ubuntu (.deb)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/linux-rpm"><img src="https://assets.openlogi.org/brand/buttons/download-linux-rpm.png" width="260" alt="Linux için indir: Fedora, SUSE (.rpm)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/linux-arch"><img src="https://assets.openlogi.org/brand/buttons/download-linux-arch.png" width="260" alt="Linux için indir: Arch (.pkg.tar.zst)"></a>
</p>

Veya dağıtımınıza uygun paketi tek bir komutla kurun. Komut minisign imzasını doğrular ve ajanı başlatır; önce `minisign`'ı kurun:

```sh
curl -fsSL https://github.com/AprilNEA/OpenLogi/releases/latest/download/install.sh | sh
```

Arch Linux'ta [`openlogi`](https://aur.archlinux.org/packages/openlogi) AUR paketi sürümü kaynak koddan derler:

```sh
yay -S openlogi
```

Betiği çalıştırmadan önce okumak ve AUR paketi, Flatpak, NixOS ile systemd olmayan dağıtımlar için [INSTALL-linux.md](INSTALL-linux.md)'ye bakın.

### Windows

<p>
    <a href="https://openlogi.org/download/windows"><img src="https://assets.openlogi.org/brand/buttons/download-windows-msi.png" width="260" alt="Windows için indir: kurulum dosyası (.msi)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/windows-zip"><img src="https://assets.openlogi.org/brand/buttons/download-windows-zip.png" width="260" alt="Windows için indir: taşınabilir (.zip)"></a>
</p>

## Kullanım (CLI)

[USAGE.md](USAGE.md)'ye bakın

## Yapılandırma

Ayarlar düz TOML kullanır; kaydetme işlemleri sembolik bağlı (symlink) ayar dosyalarını korur. [CONFIGURATION.md](CONFIGURATION.md)'ye bakın.

## Geliştirme

[DEVELOPMENT.md](DEVELOPMENT.md)'ye bakın, [macOS giriş-kancası güvenliği](DEVELOPMENT.md#macos-input-hook-safety) dahil.

## Teşekkürler

- **Windows, kameralar ve i18n**: [@davidbudnick](https://github.com/davidbudnick) — klavye RGB, Windows desteği, Logitech web kamerası desteği
- **Linux portu**: [@cserby](https://github.com/cserby) — Linux desteği
- [Solaar](https://github.com/pwr-Solaar/Solaar) ([@pwr](https://github.com/pwr)) — açık kaynak HID++ uygulaması
- [Mouser](https://github.com/TomBadash/Mouser) ([@TomBadash](https://github.com/TomBadash)) — hesap gerektirmeyen, yerel bir Options+ alternatifi

## Lisans

Bu depodaki kod, aşağıdakilerden birinin seçimine bağlı olarak çift lisanslıdır:

- Apache License, Version 2.0 ([LICENSE-APACHE](../LICENSE-APACHE))
- MIT lisansı ([LICENSE-MIT](../LICENSE-MIT))

### Üçüncü taraf kod

`crates/openlogi-hidpp`, [@lus](https://github.com/lus) tarafından yazılan
[`hidpp`](https://crates.io/crates/hidpp) paketinin 0BSD lisanslı, depoya
gömülü bir çatallanmasıdır (fork).

### Logo ve marka varlıkları

OpenLogi logosunu tasarlayan [@kubai087](https://github.com/kubai087)'e
teşekkürler. OpenLogi logosu ve uygulama simgesi ([`design/`](../design/)
altındaki marka varlıkları) © 2026 AprilNEA'ya aittir, tüm hakları saklıdır,
ve yukarıdaki MIT/Apache lisansları kapsamında değildir; bakınız
[`design/LICENSE`](../design/LICENSE). Kodu çatallamak, OpenLogi adı, logosu
veya simgesi üzerinde hak vermez; lütfen bunları kendi projelerinizi,
çatallarınızı veya dağıtımlarınızı temsil etmek için önceden yazılı izin
almadan kullanmayın.

---

**Logitech ile bağlantılı değildir.** "Logitech", "MX Master" ve "Options+", Logitech International S.A.'nın ticari markalarıdır.

## Depo etkinliği

![Repobeats analytics image](https://repobeats.com/AprilNEA/OpenLogi "Repobeats analytics image")
