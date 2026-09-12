> [!WARNING]
> **OpenLogi está em desenvolvimento ativo** e ainda não é estável: recursos e configuração podem mudar. Dê uma estrela ⭐ e acompanhe 👀 o repositório para saber quando um novo lançamento sair.

<h4 align="right"><a href="../README.md">English</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.ja.md">日本語</a> | <a href="README.de.md">Deutsch</a> | <a href="README.fr.md">Français</a> | <a href="README.ko.md">한국어</a> | <a href="README.ru.md">Русский</a> | <strong>Português</strong></h4>

<p align="center">
    <img src="https://assets.openlogi.org/brand/openlogi-icon.png" width="138" alt="OpenLogi"/>
</p>

<h1 align="center">OpenLogi</h1>
<p align="center"><strong>⚡️ Uma alternativa nativa e local-first ao Logitech Options+, escrita em Rust 🦀<br/>Libere todo o potencial de mouses, teclados e webcams Logitech via HID++ e UVC</strong></p>

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

> **Cansado do Options+? Experimente o OpenLogi.**

Roda em macOS, Linux e Windows.

---

## Além do Options+

O que o OpenLogi faz e o Options+ não faz:

- **Fica leve.** Rust nativo + GPUI.
- **Roda em Linux.** Linux é uma plataforma de primeira classe no OpenLogi.
- **Gestos em qualquer botão.** Dê o papel de gesto a qualquer botão físico, ou desligue os gestos por completo.
- **Configuração em texto puro.** Tudo em um único arquivo TOML, sincronizável entre máquinas do jeito que você preferir.
- **Programável.** Uma CLI de verdade ao lado da GUI.

## Recursos

- Dispositivos conectados via receptores Logi Bolt, receptores Unifying, Bluetooth ou cabo, com porcentagem de bateria e estado de carga
- Remapeamento de botões pelo hook de entrada do sistema operacional: um catálogo de ações embutido mais atalhos de teclado personalizados definidos no TOML, incluindo ações independentes para pressão curta/longa e combinações hold-until-release para push-to-talk¹
- Sobreposições de perfil por aplicativo, trocadas automaticamente ao focar a janela (macOS + Windows; no Linux, só em X11 / XWayland)
- Luzes Litra: liga/desliga, brilho e temperatura de cor, com opção de ligar automaticamente seguindo o uso da câmera

**Mouse**

- Captura e remapeamento dos botões do meio, mode-shift e da roda lateral (o botão do meio funciona em qualquer dispositivo, os outros dependem do que o hardware expõe)
- Vínculos de gesto por direção, com captura ao vivo, em qualquer botão compatível
- Actions Ring: uma sobreposição de oito posições centrada no cursor (`ShowActionsRing`), com layouts por aplicativo
- Controle de DPI com predefinições e ações de ciclar / definir predefinição (`0x2201`)
- Roda SmartShift: alternância de modo, sensibilidade e um painel de catraca permanente (`0x2111`)
- Inversão nativa de rolagem por dispositivo (`0x2121`, em dispositivos compatíveis)

**Teclado**

- Remapeamento global das teclas F: o mesmo catálogo de ações do mouse, mais ações avançadas como texto digitado, combinações de teclas e fluxos de várias etapas (macOS + Windows)
- Iluminação RGB estática (`0x8070` / `0x8080`, em dispositivos compatíveis)

**Câmera**

- Qualquer webcam UVC da Logitech (Brio, StreamCam, a série C920, entre outras), plug and play
- Pré-visualização ao vivo que só abre a câmera enquanto você está olhando; sair da tela libera a câmera por completo e o LED apaga
- Controles de imagem gravados direto no hardware UVC: zoom, foco, exposição, brilho, contraste, saturação, nitidez, balanço de branco, tonalidade, anti-flicker e compensação de pouca luz, com alternância de modo automático para foco / exposição / balanço de branco, então as mudanças valem no Meet, no Zoom, no OBS e em qualquer outro app que use a câmera
- Perfis de um clique: Padrão / Streaming / Chamada de vídeo embutidos, mais instantâneos personalizados; as configurações persistem por câmera e são gravadas de volta no hardware na próxima vez que você abrir a visualização

¹ Ações de tecla de mídia usam D-Bus MPRIS no Linux; algumas ações específicas do macOS não têm equivalente universal no Linux e não fazem nada. No Windows, as ações de plataforma são mapeadas para equivalentes nativos quando disponível.

## Instalação

> [!IMPORTANT]
> Feche o **Logi Options+** primeiro: os dois aplicativos disputam o acesso HID++, e só um pode controlar um receptor por vez.

### macOS

Requer macOS 13 ou mais recente.

Baixe o `.dmg` assinado e notarizado do [último lançamento](https://github.com/AprilNEA/OpenLogi/releases/latest) e arraste `OpenLogi.app` para `/Applications`.

Ou instale pelo [Homebrew](https://brew.sh):

```sh
brew install --cask openlogi
```

O cask oficial do Homebrew é o caminho padrão de instalação. Para acompanhar explicitamente o último lançamento do GitHub via `aprilnea/tap`:

```sh
brew tap aprilnea/tap
brew install --cask aprilnea/tap/openlogi@latest
```

`openlogi@latest` é mantido pelo workflow de lançamento do OpenLogi e pode se atualizar antes do autobump do cask oficial. Instale `openlogi` ou `openlogi@latest`, nunca os dois.

### Linux

Baixe o pacote da sua distribuição no [último lançamento](https://github.com/AprilNEA/OpenLogi/releases/latest):

```sh
# Debian / Ubuntu
sudo dpkg -i openlogi-*.deb

# Fedora / RHEL
sudo rpm -i openlogi-*.rpm

# Arch Linux
sudo pacman -U openlogi-*.pkg.tar.zst
```

Pacotes são publicados tanto para `x86_64`/`amd64` quanto para `arm64`/`aarch64`.
Os pacotes pré-compilados exigem GLIBC 2.35 ou mais recente (base do Ubuntu 22.04).

Usuários de NixOS podem importar o módulo do repositório, que instala o pacote e as regras do udev e inicia o agente junto com a sessão gráfica:

```nix
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.openlogi = {
    url = "github:AprilNEA/OpenLogi";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { nixpkgs, openlogi, ... }: {
    nixosConfigurations.my-host = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux"; # ou aarch64-linux
      modules = [
        openlogi.nixosModules.default
        { programs.openlogi.enable = true; }
      ];
    };
  };
}
```

Todos os pacotes Linux instalam regras udev que dão ao seu usuário acesso a
`/dev/hidraw*`, `/dev/uinput` e ao nó `/dev/input/event*` do seu mouse Logitech
sem precisar de `sudo`. O módulo NixOS inicia o agente automaticamente; depois de uma
instalação via `.deb`, `.rpm` ou `.pkg.tar.zst`, habilite-o para o seu usuário:

```sh
systemctl --user enable --now openlogi-agent.service
```

Veja [INSTALL-linux.md](INSTALL-linux.md) para as opções completas do NixOS, instalação
manual / a partir do código-fonte, e distribuições sem systemd.

### Windows

Arquivos `.zip` portáteis assinados e instaladores `.msi` por usuário (x86_64 e arm64)
acompanham cada lançamento. Os dois trazem a GUI (`OpenLogi.exe`) junto com o agente em
segundo plano (`openlogi-agent.exe`), que controla toda a E/S de dispositivos. Mantenha
os dois arquivos juntos ao usar o zip portátil, senão a GUI não tem a quem se conectar.

O suporte a Windows foi validado de ponta a ponta no Windows 11 com hardware real (um
teclado com fio e um mouse com receptor Unifying), incluindo instalação, atualização no
local e desinstalação do MSI. É mais recente que o build de macOS; se encontrar algum
problema, [relate aqui](https://github.com/AprilNEA/OpenLogi/issues). O agente mostra um
ícone na bandeja do sistema (Show Main Window / Quit), então o app continua acessível
depois que a janela principal é fechada. Para desativar esse ícone no Windows, defina
`show_in_menu_bar = false` no bloco `[app_settings]` do TOML e reinicie o agente; a opção
na GUI hoje é só para macOS.

Para compilar a partir do código-fonte, veja [DEVELOPMENT.md](DEVELOPMENT.md).


## Uso (CLI)

Veja [USAGE.md](USAGE.md)

## Configuração

Veja [CONFIGURATION.md](CONFIGURATION.md)

## Desenvolvimento

Veja [DEVELOPMENT.md](DEVELOPMENT.md)

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
