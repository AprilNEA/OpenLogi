{ pkgs, lib, config, inputs, ... }:

{
  env = {
    GREET = "devenv";
    RUSTC_WRAPPER = "sccache";

    # GPUI's macOS backend links against real Apple frameworks (Metal, AppKit,
    # CoreVideo, ...) and its build scripts shell out to Apple's toolchain
    # (`xcrun metal`, `metallib`). devenv's default Nix `apple-sdk-14.4` is a
    # stub for cross-compilation pretend — it doesn't include the Metal
    # toolchain (Xcode 16+ ships it as an optional component) and doesn't
    # expose `libSystem` in a way the macOS linker can find. Point the SDK
    # and developer dir at the real Xcode install.
    DEVELOPER_DIR = "/Applications/Xcode.app/Contents/Developer";
    SDKROOT = "/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk";
  };

  packages = with pkgs; [
    git
    cmake
    sccache
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

  # Strip Nix's `xcbuild-0.1.1-xcrun` stub from PATH so `xcrun` resolves to
  # the real `/usr/bin/xcrun` from Xcode, which knows about the cryptex-
  # mounted Metal toolchain. The Nix stub is a 2019 reimplementation that
  # predates Metal-as-optional-component.
  enterShell = ''
    export PATH=$(echo "$PATH" | tr ':' '\n' | grep -v xcbuild | paste -sd: -)
  '';

  # Project tasks. Run with `devenv tasks run <name>`, list with
  # `devenv tasks list`. All tasks inherit the env + PATH fixes above.
  tasks = {
    "optminus:run" = {
      description = "Run the CLI: enumerate connected Logitech HID++ devices.";
      exec = "cargo run -p optminus-cli -- list";
    };
    "optminus:gui" = {
      description = "Run the GPUI desktop window.";
      exec = "cargo run -p optminus-gui";
    };
    "optminus:check" = {
      description = "Pre-commit check: fmt --check, clippy -D warnings, tests. Mirrors CI.";
      exec = ''
        set -e
        cargo fmt --all -- --check
        cargo clippy --workspace --all-targets -- -D warnings
        cargo test --workspace
      '';
    };
  };
}
