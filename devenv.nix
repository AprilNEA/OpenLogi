{ pkgs, ... }:

let
  # gpui's build compiles Metal shaders against the REAL Xcode toolchain.
  # devenv's Nix apple-sdk setup hook sets DEVELOPER_DIR/SDKROOT to an SDK
  # that has no `metal`, so anything compiling the GUI must force Xcode.
  # Non-GUI crates still compile fine under this (the Nix clang wrapper keeps
  # its own isysroot via NIX_CFLAGS), so applying it broadly is safe.
  xcodeEnv = ''
    export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
    export SDKROOT="$(/usr/bin/xcrun --sdk macosx --show-sdk-path)"
  '';
in
{
  env = {
    GREET = "devenv";
    RUSTC_WRAPPER = "sccache";
    # DEVELOPER_DIR/SDKROOT are intentionally NOT set here: the Nix apple-sdk
    # setup hook would override them anyway. Xcode is forced in enterShell and
    # in the GUI tasks via xcodeEnv (above).
  };

  packages = with pkgs; [
    git
    cmake
    sccache
    prek
    create-dmg
    crowdin-cli
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
    ${xcodeEnv}
  '';

  tasks = {
    "openlogi:run" = {
      description = "List connected Logitech HID++ devices.";
      exec = "cargo run -p openlogi -- list";
    };
    "openlogi:gui" = {
      description = "Run the desktop app.";
      exec = xcodeEnv + "cargo run -p openlogi-gui";
    };
    "openlogi:check" = {
      description = "Run fmt, clippy, and tests.";
      exec = ''
        set -e
        ${xcodeEnv}
        cargo fmt --all -- --check
        cargo clippy --workspace --all-targets -- -D warnings
        cargo test --workspace
      '';
    };
    "openlogi:i18n-upload" = {
      description = "Upload English source strings to Crowdin.";
      exec = "crowdin upload sources";
    };
    "openlogi:i18n-download" = {
      description = "Download translated locale files from Crowdin.";
      exec = ''
        set -e
        crowdin download
        cargo test -p openlogi-gui i18n
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
        ${xcodeEnv}
        cargo run -p xtask -- bundle-macos
      '';
    };
    "openlogi:dmg" = {
      description = "Build a macOS DMG.";
      exec = "cargo run -p xtask -- package-macos";
    };
  };
}
