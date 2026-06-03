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
    librsvg # rsvg-convert: renders design/icon/openlogi.svg for `xtask macos-icns`
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
