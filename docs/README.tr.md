> [!WARNING]
> **OpenLogi hâlâ aktif geliştirme aşamasında** ve henüz kararlı değil — özellikler ve ayarlar değişebilir. Yeni bir sürüm çıktığında haberdar olmak için depoya **Star** ⭐ verin ve **Watch** 👀 edin.

<h4 align="right"><a href="../README.md">English</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.ja.md">日本語</a> | <a href="README.de.md">Deutsch</a> | <a href="README.fr.md">Français</a> | <a href="README.ko.md">한국어</a> | <a href="README.ru.md">Русский</a> | <a href="README.es.md">Español</a> | <a href="README.pt-BR.md">Português</a> | <strong>Türkçe</strong></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Rust 🦀 ile yazılmış, Logitech Options+'a yerel ve tamamen çevrimdışı çalışan bir alternatif<br/>Logitech fare, klavye ve web kameralarının tüm gücünü HID++ ve UVC üzerinden açığa çıkarır</strong></p>

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
- **Betiklenebilir.** Arayüzün yanında gerçek bir CLI; DPI, imleç ölçekleme ve SmartShift için [donanım tanılama](USAGE.md) araçlarıyla birlikte.

## Özellikler

- Logi Bolt alıcıları, Unifying alıcıları, Bluetooth veya kablolu bağlantı üzerinden tanınan cihazlar; pil yüzdesi ve şarj durumu dahil
- İşletim sistemi giriş kancası üzerinden tuş yeniden atama: hazır eylem kataloğu ve TOML ayar dosyasında tanımlanan özel klavye kısayolları, bağımsız kısa/uzun basış eylemleri ve bas-konuş için basılı tutma kombinasyonları¹ dahil
- Uygulama odaklandığında otomatik geçiş yapan uygulama başına profil katmanları (macOS + Windows; Linux'ta sadece X11 / XWayland)
- Litra ışıkları: güç, parlaklık ve renk sıcaklığı; kamera etkinliğini takip eden isteğe bağlı otomatik güç özelliğiyle

**Fare**

- Orta tuş, mod değiştirme ve parmak tekerleği tuşlarını yakalayıp yeniden atama (orta tuş her yerde, diğerleri cihazın desteklediği yerlerde)
- Desteklenen tuşlarda canlı yakalamalı, yöne göre jest atamaları: Geri/İleri, DPI/ModeShift, özel jest tuşu ve dokunsal panel
  - DPI/ModeShift jestleri, cihazın yönlendirme (diversion) ve ham XY desteği bildirmesini gerektirir.
  - Birincil tıklamalara ve tekerlek kontrollerine yeni jest ataması yapılamaz; mevcut Orta Tık jest atamaları korunur.
- Actions Ring: imleç merkezli, sekiz yuvalı eylem overlay'i (`ShowActionsRing`), uygulama başına düzenlerle
- Önayarlı DPI kontrolü ve Döngü / Önayar-Belirle eylemleri (`0x2201`)
- SmartShift tekerleği: mod geçişi, hassasiyet ve kalıcı mandal (ratchet) paneli (`0x2111`)
- Cihaza özel yerel kaydırma yönü ters çevirme (`0x2121`, desteklenen cihazlarda)

**Klavye**

- Genel F-tuşu yeniden atama: farenin kullandığı aynı eylem kataloğu, ayrıca ileri düzey kullanıcı eylemleri — yazılı metin, tuş kombinasyonları, çok adımlı iş akışları (macOS + Windows)
- Statik RGB aydınlatma (`0x8070` / `0x8080`, desteklenen cihazlarda)

**Kamera**

- Herhangi bir Logitech UVC web kamerası (Brio, StreamCam, C920 serisi vb.), tak-çalıştır
- Sadece izlediğiniz sürece kamerayı açan canlı önizleme — pencereyi kapattığınızda kamera tamamen serbest kalır ve LED söner
- Doğrudan UVC donanımına yazılan görüntü kontrolleri — yakınlaştırma, kaydırma, eğme, odak, pozlama, parlaklık, kontrast, doygunluk, netlik, kazanç, arka ışık dengelemesi, beyaz dengesi, ton, flicker önleme ve düşük ışık dengelemesi; odak / pozlama / beyaz dengesi için otomatik mod anahtarlarıyla — değişiklikler Meet / Zoom / OBS ve kamerayı kullanan tüm diğer uygulamalarda hemen uygulanır
- Tek tıkla profiller: yerleşik Varsayılan / Yayın / Görüntülü arama profilleri ile özel anlık görüntüler; ayarlar kamera başına saklanır ve bir sonraki görüntülemede donanıma geri yazılır

¹ Medya tuşu eylemleri Linux'ta D-Bus MPRIS kullanır; bazı macOS'a özel eylemlerin Linux'ta evrensel bir karşılığı yoktur ve bu platformda hiçbir şey yapmaz. Windows, platform eylemlerini mevcut olduğunda yerel karşılıklarına eşler.

## Kurulum

> [!IMPORTANT]
> Önce **Logi Options+**'tan çıkın: iki uygulama da HID++ erişimi için rekabet eder ve bir alıcıyı aynı anda yalnızca biri kullanabilir.

### macOS

macOS 13 veya üzeri gerektirir.

İmzalı ve notarize edilmiş `.dmg` dosyasını [son sürümden](https://github.com/AprilNEA/OpenLogi/releases/latest) indirip `OpenLogi.app`'i `/Applications`'a sürükleyin.

Veya [Homebrew](https://brew.sh) ile kurun:

```sh
brew install --cask openlogi
```

Resmi Homebrew cask'i varsayılan kurulum yoludur. `aprilnea/tap`'ten en son
GitHub sürümünü doğrudan takip etmek isterseniz:

```sh
brew tap aprilnea/tap
brew install --cask aprilnea/tap/openlogi@latest
```

`openlogi@latest`, OpenLogi'nin release iş akışı tarafından yönetilir ve
resmi cask'in otomatik güncellemesinden önce güncellenebilir. `openlogi`
veya `openlogi@latest`'ten yalnızca birini kurun, ikisini birden değil.

### Linux

Kurulum betiğini HTTPS üzerinden indirin, inceleyin, sonra çalıştırın. Doğrudan
bir kabuğa (shell) pipe'lamayın:

```sh
curl --proto '=https' --proto-redir '=https' --tlsv1.2 \
  --fail --location --silent --show-error \
  --retry 3 --retry-connrefused \
  --output openlogi-install.sh \
  https://raw.githubusercontent.com/AprilNEA/OpenLogi/master/packaging/linux/install.sh
less openlogi-install.sh
sh openlogi-install.sh
rm openlogi-install.sh
```

Betik; apt, dnf, yum, zypper, rpm veya pacman'ı tespit eder, makineye uygun
`.deb`, `.rpm` ya da `.pkg.tar.zst` dosyasını seçer, OpenLogi'nin gömülü
minisign açık anahtarıyla ayrık imzayı doğrular ve paket yöneticisini
`sudo` ile çağırmadan önce dosyanın release `SHA256SUMS` listesindeki
girdiyle eşleştiğini kontrol eder. Önce dağıtımınızın paket yöneticisinden
`minisign`'ı kurun. Betiği normal kullanıcınızla çalıştırın, `sudo` ile değil.
Varsayılan olarak en son sürümü kurar. Gerektiğinde `--version`,
`--package-manager`, `--no-start` veya `--dry-run` seçeneklerini kullanın.

Paketler hem `x86_64`/`amd64` hem de `arm64`/`aarch64` için yayınlanır.
Hazır paketler GLIBC 2.35 veya daha yenisini gerektirir (Ubuntu 22.04 tabanı).

NixOS kullanıcıları, paketi ve udev kurallarını kuran ve grafik oturumuyla
birlikte ajanı başlatan deponun modülünü doğrudan içe aktarabilir:

```nix
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.openlogi = {
    url = "github:AprilNEA/OpenLogi";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { nixpkgs, openlogi, ... }: {
    nixosConfigurations.my-host = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux"; # veya aarch64-linux
      modules = [
        openlogi.nixosModules.default
        { programs.openlogi.enable = true; }
      ];
    };
  };
}
```

Tüm Linux paketleri, kullanıcınıza `sudo` gerekmeden `/dev/hidraw*`,
`/dev/uinput` ve Logitech farenizin `/dev/input/event*` düğümüne erişim
veren udev kuralları kurar. Kurulum betiği ve NixOS modülü ajanı otomatik
başlatır; elle paket kurulumundan sonra kendi kullanıcınız için etkinleştirin:

```sh
systemctl --user enable --now openlogi-agent.service
```

Sabit sürüm kurulumları, eksiksiz NixOS seçenekleri, kaynaktan kurulum ve
systemd olmayan dağıtımlar için [docs/INSTALL-linux.md](INSTALL-linux.md)'ye bakın.

### Windows

İmzalı taşınabilir `.zip` arşivleri ve kullanıcı başına `.msi` kurulum
dosyaları (x86_64 ve arm64) her sürüme eklenir. Her ikisi de arayüzü
(`OpenLogi.exe`) tüm cihaz G/Ç'sini üstlenen arka plan ajanıyla
(`openlogi-agent.exe`) birlikte içerir. Taşınabilir zip'i kullanırken iki
dosyayı aynı klasörde tutun, aksi halde arayüzün bağlanacağı bir şey olmaz.

Windows desteği, gerçek donanımla (kablolu bir klavye ve Unifying alıcılı
bir fare) Windows 11'de kurulum, yerinde güncelleme ve MSI kaldırma dahil
uçtan uca doğrulanmıştır. macOS yapısından daha yenidir; sorunla
karşılaşırsanız lütfen [bildirin](https://github.com/AprilNEA/OpenLogi/issues).
Ajan, ana pencere kapatıldıktan sonra uygulamaya erişimi sürdürmek için
sistem tepsisinde bir simge gösterir (Ana Pencereyi Göster / Çıkış).
Windows'ta bunu kapatmak için TOML dosyasındaki `[app_settings]` bloğunda
`show_in_menu_bar = false` ayarlayıp ajanı yeniden başlatın; arayüz anahtarı
şu an için yalnızca macOS'ta mevcuttur.

Kaynaktan derlemek için [DEVELOPMENT.md](DEVELOPMENT.md)'ye bakın.


## Kullanım (CLI)

[USAGE.md](USAGE.md)'ye bakın

## Yapılandırma

Ayarlar düz TOML kullanır; kaydetme işlemleri sembolik bağlı (symlink) ayar
dosyalarını korur. [CONFIGURATION.md](CONFIGURATION.md)'ye bakın.

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
