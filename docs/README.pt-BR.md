> [!WARNING]
> **OpenLogi está em desenvolvimento ativo** e ainda não é estável: recursos e configuração podem mudar. Dê uma estrela ⭐ e acompanhe 👀 o repositório para saber quando um novo lançamento sair.

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Uma alternativa nativa e local-first ao Logitech Options+, escrita em Rust 🦀<br/>Libere todo o potencial de mouses, teclados e webcams Logitech via HID++ e UVC</strong></p>

<div align="center">
    <a href="https://github.com/AprilNEA/OpenLogi/releases/latest" target="_blank">
    <img alt="GitHub release" src="https://img.shields.io/github/v/release/AprilNEA/OpenLogi?style=flat"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/releases" target="_blank">
    <img alt="GitHub downloads" src="https://img.shields.io/github/downloads/AprilNEA/OpenLogi/total.svg?style=flat"></a>
    <a href="https://github.com/AprilNEA/OpenLogi/commits" target="_blank">
    <img alt="GitHub commit" src="https://img.shields.io/github/commit-activity/m/AprilNEA/OpenLogi?style=flat"></a>
    <a href="#licença">
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
    <img alt="Português" src="https://img.shields.io/badge/Portugu%C3%AAs-blue?style=flat"></a>
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

> **Cansado do Options+? Experimente o OpenLogi.**

Roda em macOS, Linux e Windows.

---

## Além do Options+

O que o OpenLogi faz e o Options+ não faz:

- **Fica leve.** Rust nativo + GPUI.
- **Roda em Linux.** Linux é uma plataforma de primeira classe no OpenLogi.
- **Gestos nos botões compatíveis.** Atribua ações de gesto aos controles compatíveis, ou desligue os gestos por completo.
- **Configuração em texto puro.** Tudo em um único arquivo TOML, sincronizável entre máquinas do jeito que você preferir.
- **Programável.** Uma CLI de verdade ao lado da GUI: uma [API de automação](USAGE.md#automation-api) em JSON para DPI, SmartShift, Fn-lock e taxa de relatório, além de diagnósticos de hardware.

## Recursos

<p align="center">
    <img src="https://assets.openlogi.org/screenshots/macos/light/devices-grid.png" width="860" alt="Lista de dispositivos do OpenLogi com seis dispositivos Logitech: mouses, um teclado e uma luz Litra">
</p>

### Dispositivos

- Receptores Logi Bolt e Unifying, Bluetooth e conexões com fio, com nível de bateria e estado de carga
- Avisos de bateria fraca na parte de baixo da tela, uma vez por descarga, sem precisar de permissão de notificação do sistema
- Um indicador na tela quando um teclado liga ou desliga o Caps Lock, além de Num Lock e Scroll Lock no Windows e no Linux

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/easy-switch.png" width="400" alt="Cartão Easy-Switch listando três computadores pareados"></td>
        <td width="50%"><b>Easy-Switch</b><ul><li>Veja qual computador está pareado em cada canal, com o sistema operacional e a conexão</li><li>Renomeie um canal nos dispositivos que permitem</li></ul></td>
    </tr>
</table>

### Botões e ações

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/buttons.png" width="400" alt="Mapeamento de botões de um MX Master 4, com o inspetor do botão de gesto aberto"></td>
        <td width="50%"><b>Remapeie qualquer botão</b><ul><li>Um catálogo de ações embutido, atalhos personalizados, texto digitado ou fluxos de várias etapas</li><li>Ações separadas para toque e pressão longa, e combinações de push-to-talk que soltam junto com o botão</li><li>Gestos no clique do meio, Voltar/Avançar, DPI/ModeShift, no botão de gesto e no painel háptico</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/actions-ring.png" width="400" alt="Editor do Actions Ring com oito posições ao redor do cursor"></td>
        <td width="50%"><b>Actions Ring</b><ul><li>Oito ações em um anel ao redor do cursor</li><li>Um layout próprio para cada aplicativo</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/keys.png" width="400" alt="MX Keys S com três teclas vinculadas a Launchpad, Bloquear Tela e Captura de Tela"></td>
        <td width="50%"><b>Teclas do teclado</b><ul><li>Vincule teclas de mídia, emoji, captura de tela e outras teclas dedicadas como se fossem botões do mouse</li><li>Restaure uma tecla ao padrão do dispositivo ou redefina a linha inteira</li></ul></td>
    </tr>
</table>

Os perfis por aplicativo trocam junto com o app em foco. Um alias em `[apps]` permite que um perfil acompanhe um aplicativo em todas as plataformas.

### Ponteiro e rolagem

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/dpi.png" width="400" alt="Cartão de ajuste do ponteiro com um controle deslizante de DPI"></td>
        <td width="50%"><b>Ponteiro</b><ul><li>Predefinições de DPI com ações de ciclar e definir predefinição</li><li>Velocidade do ponteiro e taxa de relatório</li><li>Modo de precisão: um botão leva o mouse à velocidade mais lenta e de volta</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/smartshift.png" width="400" alt="Cartão SmartShift com modo da roda, sensibilidade e força de rolagem"></td>
        <td width="50%"><b>SmartShift</b><ul><li>Modo da roda e sensibilidade</li><li>Força de rolagem em mouses com SmartShift Enhanced</li><li>Uma chave de catraca permanente</li></ul></td>
    </tr>
    <tr>
        <td colspan="2"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/scrolling.png" width="820" alt="Cartão de rolagem com inversão, resolução da roda e roda suave"><br><b>Rolagem</b><ul><li>Inversão de rolagem por dispositivo, nativa ou por software</li><li>Resolução da roda e rolagem suave da roda</li><li>Segure Voltar ou Avançar e gire a roda para rolar na horizontal em mouses sem roda lateral</li></ul></td>
    </tr>
</table>

### Teclado e iluminação

<table>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/keys-backlight.png" width="400" alt="Cartões de teclas e luz de fundo de um MX Keys S"></td>
        <td width="50%"><b>Teclado</b><ul><li>Fn Lock, e teclados multiplataforma que seguem o layout do sistema operacional do host</li><li>Nível da luz de fundo, brilho automático, economia de bateria e por quanto tempo as teclas ficam acesas</li><li>O nível da luz de fundo mostrado na tela enquanto muda</li><li>RGB estático nos teclados compatíveis</li></ul></td>
    </tr>
    <tr>
        <td width="50%"><img src="https://assets.openlogi.org/screenshots/macos/light/cards/litra.png" width="400" alt="Painel de iluminação da Litra Glow com brilho e temperatura de cor"></td>
        <td width="50%"><b>Luzes Litra</b><ul><li>Liga/desliga, brilho e temperatura de cor</li><li>Liga junto com a câmera, se você quiser</li><li>Mantém um ajuste feito na própria luz</li></ul></td>
    </tr>
</table>

### Webcams

- Qualquer webcam UVC da Logitech (Brio, StreamCam, a série C920, …), plug and play
- Pré-visualização ao vivo que só ocupa a câmera enquanto você está olhando; saia dela e o LED apaga
- Zoom, pan, tilt, foco, exposição, balanço de branco e todos os outros controles UVC, gravados no hardware para que o Meet, o Zoom e o OBS vejam a mudança
- Perfis Padrão / Streaming / Chamada de vídeo embutidos mais seus próprios instantâneos, guardados por câmera

A cobertura de recursos depende do que cada dispositivo informa; o app mostra só os controles que o dispositivo suporta.

### Suporte por plataforma

| Recurso | macOS | Windows | Linux |
| --- | --- | --- | --- |
| Remapeamento de botões do mouse e gestos | ✓ | ✓ | ✓ |
| Remapeamento das teclas F do teclado | ✓ | ✓ | — |
| Perfis por aplicativo | ✓ | ✓ | X11, wlroots (sway, Hyprland), GNOME com a [extensão complementar](../crates/openlogi-hook/gnome-shell-extension/README.md) |
| Perfil do mouse segue a janela sob o ponteiro | ✓ | ✓ | Só X11 |
| Teclas de mídia | Nativo | Nativo | D-Bus MPRIS |
| Ações do gerenciador de janelas (áreas de trabalho, Mission Control, …) | Nativo | Equivalentes nativos | GNOME, KDE (KWin), Hyprland; algumas ações exclusivas do macOS não fazem nada |
| Ícone na barra de menus / bandeja | ✓ | ✓ | — |
| Atualizações no app | Sparkle | Embutido | Embutido |

## Instalação

> [!IMPORTANT]
> Feche o **Logi Options+** (e o Solaar no Linux) primeiro. Só um aplicativo pode controlar um receptor por vez.

| Plataforma | Sistema | Arquitetura |
| --- | --- | --- |
| macOS | macOS 13 ou mais recente | Apple silicon, Intel |
| Linux | GLIBC 2.35 ou mais recente | x86_64, arm64 |
| Windows | Testado no Windows 11 | x86_64, arm64 |

Todo download traz uma assinatura minisign e uma entrada em `SHA256SUMS`. Para compilar a partir do código-fonte, veja [DEVELOPMENT.md](DEVELOPMENT.md).

### macOS

<p>
    <a href="https://openlogi.org/download/mac-arm64"><img src="https://assets.openlogi.org/brand/buttons/download-macos-arm64.png" width="260" alt="Baixar para Mac: Apple silicon (.dmg)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/mac-x64"><img src="https://assets.openlogi.org/brand/buttons/download-macos-x64.png" width="260" alt="Baixar para Mac: Intel (.dmg)"></a>
</p>

Ou pelo Homebrew:

```sh
brew install --cask openlogi
```

Abra o OpenLogi e conceda **Acessibilidade** e **Monitoramento de entrada** quando o macOS pedir. Depois disso, o OpenLogi se atualiza sozinho pelo Sparkle.

### Linux

<p>
    <a href="https://openlogi.org/download/linux"><img src="https://assets.openlogi.org/brand/buttons/download-linux-deb.png" width="260" alt="Baixar para Linux: Debian, Ubuntu (.deb)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/linux-rpm"><img src="https://assets.openlogi.org/brand/buttons/download-linux-rpm.png" width="260" alt="Baixar para Linux: Fedora, SUSE (.rpm)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/linux-arch"><img src="https://assets.openlogi.org/brand/buttons/download-linux-arch.png" width="260" alt="Baixar para Linux: Arch (.pkg.tar.zst)"></a>
</p>

Ou instale o pacote certo para a sua distribuição com um único comando. Ele verifica a assinatura minisign e inicia o agente; instale o `minisign` antes:

```sh
curl -fsSL https://github.com/AprilNEA/OpenLogi/releases/latest/download/install.sh | sh
```

No Arch Linux, o pacote do AUR [`openlogi`](https://aur.archlinux.org/packages/openlogi) compila a versão a partir do código-fonte:

```sh
yay -S openlogi
```

Para ler o script antes de executá-lo, e para o pacote do AUR, Flatpak, NixOS e distribuições sem systemd, veja [INSTALL-linux.md](INSTALL-linux.md).

### Windows

<p>
    <a href="https://openlogi.org/download/windows"><img src="https://assets.openlogi.org/brand/buttons/download-windows-msi.png" width="260" alt="Baixar para Windows: instalador (.msi)"></a>
    &nbsp;
    <a href="https://openlogi.org/download/windows-zip"><img src="https://assets.openlogi.org/brand/buttons/download-windows-zip.png" width="260" alt="Baixar para Windows: portátil (.zip)"></a>
</p>

## Uso (CLI)

Veja [USAGE.md](USAGE.md)

## Configuração

As configurações usam TOML puro; salvar preserva arquivos de configuração que são links simbólicos. Veja [CONFIGURATION.md](CONFIGURATION.md).

## Desenvolvimento

Veja [DEVELOPMENT.md](DEVELOPMENT.md), incluindo [segurança do hook de entrada no macOS](DEVELOPMENT.md#macos-input-hook-safety).

## Agradecimentos

- **Windows, câmeras e i18n** por [@davidbudnick](https://github.com/davidbudnick): RGB do teclado, suporte a Windows, suporte às webcams Logitech
- **Port para Linux** por [@cserby](https://github.com/cserby): suporte a Linux
- [Solaar](https://github.com/pwr-Solaar/Solaar), de [@pwr](https://github.com/pwr): implementação de HID++ de código aberto
- [Mouser](https://github.com/TomBadash/Mouser), de [@TomBadash](https://github.com/TomBadash): alternativa local ao Options+, sem conta

## Licença

O código deste repositório é licenciado, à sua escolha, sob qualquer uma das seguintes licenças:

- Apache License, Version 2.0 ([LICENSE-APACHE](../LICENSE-APACHE))
- Licença MIT ([LICENSE-MIT](../LICENSE-MIT))

### Código de terceiros

`crates/openlogi-hidpp` é um fork vendorizado do [`hidpp`](https://crates.io/crates/hidpp),
de [@lus](https://github.com/lus), licenciado sob 0BSD.

### Logo e ativos de marca

Obrigado a [@kubai087](https://github.com/kubai087) por desenhar o logo do OpenLogi. O
logo e o ícone do app (os ativos de marca em [`design/`](../design/)) são © 2026 AprilNEA,
todos os direitos reservados, e não são cobertos pelas licenças MIT/Apache acima; veja
[`design/LICENSE`](../design/LICENSE). Fazer fork do código não dá direito ao nome, logo
ou ícone do OpenLogi; por favor, não os use para representar seus próprios projetos,
forks ou distribuições sem permissão prévia por escrito.

---

**Sem vínculo com a Logitech.** "Logitech", "MX Master" e "Options+" são marcas registradas da Logitech International S.A.

## Atividade do repositório

![Repobeats analytics image](https://repobeats.axiom.co/api/embed/4a0b576a03e9d528ad31ccf4797a1286c045d021.svg "Repobeats analytics image")
