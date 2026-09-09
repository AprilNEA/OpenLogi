> [!WARNING]
> **OpenLogi está en desarrollo activo** y todavía no es estable: las funciones y la configuración aún pueden cambiar. Dale una **Star** ⭐ al repositorio y ponlo en **Watch** 👀 para enterarte en cuanto salga una versión nueva.

<h4 align="right"><a href="../README.md">English</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.ja.md">日本語</a> | <a href="README.de.md">Deutsch</a> | <a href="README.fr.md">Français</a> | <a href="README.ko.md">한국어</a> | <a href="README.ru.md">Русский</a> | <strong>Español</strong></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Una alternativa nativa y local-first a Logitech Options+, escrita en Rust 🦀<br/>Aprovecha al máximo los ratones, teclados y webcams de Logitech mediante HID++ y UVC</strong></p>

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

> **¿Harto de Options+? Prueba OpenLogi.**

Funciona en macOS, Linux y Windows.

---

## Más allá de Options+

Cosas que hace OpenLogi y Options+ no:

- **No pesar.** Rust nativo + GPUI.
- **Funcionar en Linux.** En OpenLogi, Linux es una plataforma de primera.
- **Gestos en el botón que quieras.** Asigna los gestos a cualquier botón físico, o desactívalos del todo.
- **Configuración en texto plano.** Todo cabe en un único fichero TOML que puedes sincronizar entre máquinas como te convenga.
- **Automatizar.** Una CLI de verdad junto a la interfaz gráfica.

## Funciones

- Dispositivos conectados por receptor Logi Bolt, receptor Unifying, Bluetooth o cable, con porcentaje de batería y estado de carga
- Reasignación de botones mediante el hook de entrada del sistema: un catálogo de acciones integrado, más los atajos de teclado que definas tú en el TOML. La pulsación corta y la larga llevan acciones independientes, y hay acordes que se mantienen mientras no sueltes el botón, pensados para push-to-talk¹
- Perfiles por aplicación que se superponen y cambian solos según la app que tengas en primer plano (macOS + Windows; en Linux solo con X11 / XWayland)
- Luces Litra: encendido, brillo y temperatura de color, con encendido automático opcional que sigue la actividad de la cámara

**Ratón**

- Captura y reasignación del botón central, el de cambio de modo y la rueda lateral (el central en todos los dispositivos; el resto, donde el dispositivo lo permita)
- Asignación de gestos por dirección con captura en directo, en cualquier botón compatible
- Anillo de acciones: una superposición de ocho huecos centrada en el cursor (`ShowActionsRing`), con distribuciones por aplicación
- Control de DPI con preajustes y acciones de recorrer y fijar preajuste (`0x2201`)
- Rueda SmartShift: cambio de modo, sensibilidad y panel de trinquete permanente (`0x2111`)
- Inversión nativa del desplazamiento por dispositivo (`0x2121`, dispositivos compatibles)

**Teclado**

- Reasignación global de las teclas F: el mismo catálogo de acciones que el ratón, más acciones avanzadas: escribir texto, combinaciones de teclas y flujos de varios pasos (macOS + Windows)
- Iluminación RGB estática (`0x8070` / `0x8080`, dispositivos compatibles)

**Cámara**

- Cualquier webcam UVC de Logitech (Brio, StreamCam, la serie C920, …), sin configurar nada
- Vista previa en directo que solo abre la cámara mientras la miras: al salir, la libera por completo y el LED se apaga
- Controles de imagen escritos directamente en el hardware UVC: zoom, enfoque, exposición, brillo, contraste, saturación, nitidez, balance de blancos, matiz, antiparpadeo y compensación de poca luz, con modo automático para enfoque, exposición y balance de blancos, de modo que los cambios se aplican en Meet, Zoom, OBS y cualquier otra aplicación que use la cámara
- Perfiles de un clic: Predeterminado, Transmisión y Videollamada integrados, más instantáneas propias; los ajustes se guardan por cámara y se vuelven a escribir en el hardware la próxima vez que la abras

¹ En Linux, las acciones de teclas multimedia usan D-Bus MPRIS; unas pocas acciones propias de macOS no tienen equivalente universal en Linux y no hacen nada. Windows asigna las acciones de plataforma a sus equivalentes nativos cuando existen.

## Instalación

> [!IMPORTANT]
> Cierra antes **Logi Options+**: las dos aplicaciones se pelean por el acceso HID++ y un receptor solo puede pertenecer a una a la vez.

### macOS

Requiere macOS 13 o posterior.

Descarga el `.dmg` firmado y notarizado desde la [última release](https://github.com/AprilNEA/OpenLogi/releases/latest) y arrastra `OpenLogi.app` a `/Applications`.

O instálalo con [Homebrew](https://brew.sh):

```sh
brew install --cask openlogi
```

El cask oficial de Homebrew es la vía de instalación por defecto. Si prefieres
seguir explícitamente la última release de GitHub desde `aprilnea/tap`:

```sh
brew tap aprilnea/tap
brew install --cask aprilnea/tap/openlogi@latest
```

De `openlogi@latest` se encarga el flujo de publicación de OpenLogi, y puede
actualizarse antes de que el cask oficial se ponga al día. Instala `openlogi` o
`openlogi@latest`, pero no los dos.

### Linux

Descarga el paquete de tu distribución desde la
[última release](https://github.com/AprilNEA/OpenLogi/releases/latest):

```sh
# Debian / Ubuntu
sudo dpkg -i openlogi-*.deb

# Fedora / RHEL
sudo rpm -i openlogi-*.rpm

# Arch Linux
sudo pacman -U openlogi-*.pkg.tar.zst
```

Hay paquetes tanto para `x86_64`/`amd64` como para `arm64`/`aarch64`.
Los paquetes precompilados necesitan GLIBC 2.35 o superior (la base de Ubuntu 22.04).

En NixOS puedes importar el módulo del repositorio, que instala el paquete y las
reglas de udev, y arranca el agente con la sesión gráfica:

```nix
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.openlogi = {
    url = "github:AprilNEA/OpenLogi";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { nixpkgs, openlogi, ... }: {
    nixosConfigurations.my-host = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux"; # o aarch64-linux
      modules = [
        openlogi.nixosModules.default
        { programs.openlogi.enable = true; }
      ];
    };
  };
}
```

Todos los paquetes de Linux instalan reglas de udev que dan a tu usuario acceso a
`/dev/hidraw*`, `/dev/uinput` y el nodo `/dev/input/event*` de tu ratón Logitech
sin `sudo`. El módulo de NixOS arranca el agente automáticamente; tras instalar
un `.deb`, un `.rpm` o un `.pkg.tar.zst`, actívalo para tu usuario:

```sh
systemctl --user enable --now openlogi-agent.service
```

En [INSTALL-linux.md](INSTALL-linux.md) tienes todas las opciones de NixOS, la
instalación manual o desde el código fuente y las distribuciones sin systemd.

### Windows

Cada release incluye archivos `.zip` portables firmados e instaladores `.msi`
por usuario (x86_64 y arm64). Ambos traen la interfaz gráfica (`OpenLogi.exe`)
junto al agente en segundo plano (`openlogi-agent.exe`), que es quien gestiona
toda la E/S con los dispositivos. Si usas el zip portable, mantén los dos
ficheros juntos o la interfaz no tendrá a qué conectarse.

La compatibilidad con Windows se ha validado de principio a fin en Windows 11
con hardware real (un teclado por cable y un ratón con receptor Unifying),
incluidas la instalación, la actualización sobre la anterior y la desinstalación
del MSI. Es más reciente que la versión de macOS, así que
[avísanos](https://github.com/AprilNEA/OpenLogi/issues) si te encuentras alguna
aspereza. El agente muestra un icono en el área de notificación (Mostrar ventana
principal / Salir) para que la aplicación siga a mano después de cerrar la
ventana. Para desactivarlo en Windows, pon `show_in_menu_bar = false` en el
bloque `[app_settings]` del TOML y reinicia el agente; el interruptor de la
interfaz gráfica es de momento solo para macOS.

Para compilar desde el código fuente, consulta [DEVELOPMENT.md](DEVELOPMENT.md).


## Uso (CLI)

Consulta [USAGE.md](USAGE.md)

## Configuración

Consulta [CONFIGURATION.md](CONFIGURATION.md)

## Desarrollo

Consulta [DEVELOPMENT.md](DEVELOPMENT.md)

## Agradecimientos

- **Windows, cámaras e i18n** por [@davidbudnick](https://github.com/davidbudnick) — RGB del teclado, compatibilidad con Windows, compatibilidad con las webcams de Logitech
- **Port a Linux** por [@cserby](https://github.com/cserby) — compatibilidad con Linux
- [Solaar](https://github.com/pwr-Solaar/Solaar) de [@pwr](https://github.com/pwr) — implementación de HID++ de código abierto
- [Mouser](https://github.com/TomBadash/Mouser) de [@TomBadash](https://github.com/TomBadash) — un sustituto de Options+ local y sin cuentas

## Licencia

El código de este repositorio tiene licencia doble, a tu elección:

- Apache License, versión 2.0 ([LICENSE-APACHE](../LICENSE-APACHE))
- Licencia MIT ([LICENSE-MIT](../LICENSE-MIT))

### Código de terceros

`crates/openlogi-hidpp` es un fork integrado de [`hidpp`](https://crates.io/crates/hidpp)
de [@lus](https://github.com/lus), con licencia 0BSD.

### Logo y recursos de marca

Gracias a [@kubai087](https://github.com/kubai087) por diseñar el logo de
OpenLogi. El logo y el icono de la aplicación (los recursos de marca que hay en
[`design/`](../design/)) son © 2026 AprilNEA, todos los derechos reservados, y no
están cubiertos por las licencias MIT/Apache anteriores; consulta
[`design/LICENSE`](../design/LICENSE). Hacer un fork del código no da ningún
derecho sobre el nombre, el logo ni el icono de OpenLogi; por favor, no los uses
para representar tus propios proyectos, forks o distribuciones sin permiso
previo por escrito.

---

**Sin afiliación con Logitech.** «Logitech», «MX Master» y «Options+» son marcas comerciales de Logitech International S.A.

## Actividad del repositorio

![Repobeats analytics image](https://repobeats.com/AprilNEA/OpenLogi "Repobeats analytics image")
