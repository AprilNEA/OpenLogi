{ lib, pkgs, ... }:

let
  # GPUI's build compiles Metal shaders against the real Xcode toolchain.
  # devenv's Nix apple-sdk setup hook can point DEVELOPER_DIR/SDKROOT at an SDK
  # that has no `metal`, so macOS dev shells force the full Xcode install.
  # `MacOSX.sdk` is a stable symlink managed by Xcode, avoiding a shell-time
  # `xcrun --show-sdk-path` just to populate the environment.
  xcodeDeveloperDir = "/Applications/Xcode.app/Contents/Developer";
  xcodeSdkRoot = "${xcodeDeveloperDir}/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk";
  requireXcodeMetal = pkgs.lib.optionalString pkgs.stdenv.isDarwin ''
    if ! /usr/bin/xcrun --find metal >/dev/null 2>&1; then
      echo "OpenLogi GUI builds require full Xcode with Metal tools, not only Command Line Tools." >&2
      echo "Install Xcode, then run: sudo xcode-select -s ${xcodeDeveloperDir}" >&2
      exit 1
    fi
  '';
in
{
  # Use the system Xcode SDK instead of devenv's default Nix apple-sdk. GPUI
  # needs Xcode's Metal toolchain, and setting this to null keeps the env vars
  # below from being overwritten by the apple-sdk setup hook.
  apple.sdk = null;

  env = {
    RUSTC_WRAPPER = "sccache";
  }
  // lib.optionalAttrs pkgs.stdenv.isLinux {
    # GPUI loads its graphics backends dynamically, so they do not appear in
    # the development binary's RUNPATH until it is packaged.
    LD_LIBRARY_PATH = lib.makeLibraryPath [
      pkgs.libGL
      pkgs.wayland
      pkgs.vulkan-loader
    ];
    LIBCLANG_PATH = lib.makeLibraryPath [ pkgs.llvmPackages.libclang ];
  }
  // lib.optionalAttrs pkgs.stdenv.isDarwin {
    DEVELOPER_DIR = xcodeDeveloperDir;
    SDKROOT = xcodeSdkRoot;
  };

  packages =
    with pkgs;
    [
      git
      cmake
      sccache
      prek
    ]
    # create-dmg is macOS-only (meta.platforms = darwin); an unconditional entry
    # breaks evaluation of the shell on Linux.
    ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ create-dmg ]
    # The Linux build and GUI runtime use these libraries directly; declare
    # them instead of relying on transitive packages or the host environment.
    ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
      pkg-config
      fontconfig
      freetype
      libGL
      nfpm
      libxcb
      libxkbcommon
      wayland
      vulkan-loader
    ];

  languages.rust = {
    enable = true;
    channel = "stable";
    components = [
      "rustc"
      "cargo"
      "clippy"
      "rustfmt"
      "rust-analyzer"
      "rust-src"
    ];
    # Cross target for linting the Windows-only code paths locally. `cargo
    # clippy --target` is check-only (no linking), so this needs the target's
    # rust-std but NOT a mingw cross-linker; the agent's dep tree is pure Rust
    # plus prebuilt import libs (no `cc`-compiled C), so it lints cleanly. It is
    # a fast proxy for CI's authoritative `clippy (windows)` (msvc); building a
    # runnable .exe would additionally need pkgsCross.mingwW64 and is out of scope.
    targets = [ "x86_64-pc-windows-gnu" ];
  };

  enterShell = ''
    export PATH=$(echo "$PATH" | tr ':' '\n' | grep -v xcbuild | paste -sd: -)
    ${requireXcodeMetal}
  '';

  tasks = {
    "openlogi:run" = {
      description = "List connected Logitech HID++ devices.";
      exec = "cargo run -p openlogi -- list";
    };
    "openlogi:gui" = {
      description = "Run the desktop app.";
      exec = ''
        set -e
        ${requireXcodeMetal}
        cargo run -p openlogi-desktop
      '';
    };
    "openlogi:check" = {
      description = "Run fmt, clippy, tests, and rustdoc.";
      exec = ''
        set -e
        ${requireXcodeMetal}
        cargo fmt --all -- --check
        cargo clippy --workspace --all-targets -- -D warnings
        cargo test --workspace
        # Mirrors CI's `rustdoc (hid crates)` job: a broken intra-doc link is
        # neither a compile error nor a clippy lint, so nothing above catches it.
        RUSTDOCFLAGS="-D warnings" cargo doc -p openlogi-hid -p openlogi-hidpp \
          -p openlogi-hidpp-derive --no-deps --document-private-items
      '';
    };
    "openlogi:i18n-upload" = {
      description = "Upload en.yml sources and per-language translations to Crowdin.";
      exec = ''
        set -e
        ${pkgs.crowdin-cli}/bin/crowdin upload sources
        ${pkgs.crowdin-cli}/bin/crowdin upload translations
      '';
    };
    "openlogi:i18n-download" = {
      description = "Download Crowdin translations, merge into complete catalogs, run i18n tests.";
      exec = ''
        set -e
        ${requireXcodeMetal}
        ${pkgs.python3}/bin/python3 .github/scripts/i18n/merge_crowdin_download.py --self-test
        before="$(mktemp -d)"
        trap 'rm -rf "$before"' EXIT
        cp crates/openlogi-ui/locales/*.yml "$before/"
        ${pkgs.crowdin-cli}/bin/crowdin download --skip-untranslated-strings
        ${pkgs.python3}/bin/python3 .github/scripts/i18n/merge_crowdin_download.py \
          --before "$before" \
          --locales crates/openlogi-ui/locales \
          --en crates/openlogi-ui/locales/en.yml
        cargo test -p openlogi-desktop i18n
      '';
    };
    "openlogi:check-windows" = {
      description = "Lint the Windows code paths locally (check-only cross lint).";
      # `clippy --target` is check-only (no linker needed), but a C-compiling
      # build dep DOES need a cross C toolchain: openlogi-{assets,cli} and the
      # root `openlogi` pull ureq -> ring, whose curve25519.c can't cross-compile
      # from macOS without mingw. They have no Windows-specific code, so lint the
      # ring-free agent/leaf subset here; CI's clippy (windows) covers the rest
      # natively on windows-latest. The GUI is excluded (GPUI has no Windows
      # backend).
      exec = ''
        cargo clippy --target x86_64-pc-windows-gnu \
          -p openlogi-core -p openlogi-hidpp -p openlogi-hid -p openlogi-hook \
          -p openlogi-agent -p openlogi-agent-core \
          --all-targets -- -D warnings
      '';
    };
    "openlogi:assets" = {
      description = "Sync device assets.";
      exec = "cargo run -p openlogi --release -- assets sync";
    };
    "openlogi:bundle" = {
      description = "Build OpenLogi.app.";
      exec = ''
        set -e
        ${requireXcodeMetal}
        cargo run -p xtask -- macos bundle
      '';
    };
    "openlogi:dmg" = {
      description = "Build a macOS DMG.";
      exec = ''
        set -e
        ${requireXcodeMetal}
        cargo run -p xtask -- macos package
      '';
    };
  };
}
