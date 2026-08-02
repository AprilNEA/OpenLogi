# Nix package for OpenLogi on Linux (CLI + agent + GUI).
#
# Build via the flake:
#   nix build .#openlogi
#
# ## Why this doesn't suffer the #262 cargoHash churn
#
# The previous flake (removed in #262) used fetchCargoVendor, whose single
# cargoHash covers one FOD containing every dependency plus a copy of
# Cargo.lock. Because the lock embeds the local openlogi* crate versions,
# every release bump invalidated the hash even when no dependency changed.
#
# This package uses rustPlatform's `cargoLock` (importCargoLock) instead:
# - crates.io dependencies are fetched individually using the checksums
#   already recorded in Cargo.lock — no manual hashes, ever.
# - git dependencies need one manual hash per repository (not per crate,
#   not per release): importCargoLock resolves `outputHashes` keys to git
#   commit SHAs, so the hashes below stay valid until a git pin actually
#   moves. A version bump of OpenLogi itself changes nothing here.
#
# The only recurring maintenance is: when a git pin (gpui, gpui-component,
# ...) is bumped, update the corresponding entry below — the failing build
# prints the correct hash to paste. The nix.yml workflow makes that failure
# happen in the PR that bumps the pin, not silently on master afterwards.
{
  lib,
  rustPlatform,
  fetchgit,
  src,
  pkg-config,
  makeWrapper,
  fontconfig,
  freetype,
  libxkbcommon,
  wayland,
  vulkan-loader,
  libxcb,
}:

let
  # Single source of truth for the version: [workspace.package] in the
  # workspace Cargo.toml (every crate uses version.workspace = true).
  version = (builtins.fromTOML (builtins.readFile "${src}/Cargo.toml")).workspace.package.version;

  # GPUI dlopens libwayland-client / libvulkan at runtime instead of linking
  # them, so they are absent from the binary's RUNPATH. Supply them through a
  # wrapper; everything else (libxkbcommon, xcb, fontconfig) resolves via
  # RUNPATH as usual.
  runtimeLibs = lib.makeLibraryPath [
    wayland
    vulkan-loader
  ];

  # gpui-component checkout for the GUI build script. The upstream themes live
  # at the repository root next to (not inside) the gpui-component crate, so
  # the per-crate vendor tree importCargoLock produces doesn't contain them.
  # build.rs provides OPENLOGI_THEMES_DIR as an explicit override — point it
  # at a separate checkout. The rev must match Cargo.lock (a mismatch fails
  # the build with a hash error, so it cannot drift silently); the hash is
  # shared with outputHashes below.
  gpuiComponentRev = "031555662e99a1b5a549990b47f246d475b8288a";
  gpuiComponentHash = "sha256-yOXdgxQgfvGN2/+OdDnl1pYti0DoGFvS3Tyqvj3Bkng=";
  gpuiComponentSrc = fetchgit {
    url = "https://github.com/longbridge/gpui-component";
    rev = gpuiComponentRev;
    hash = gpuiComponentHash;
  };
in
rustPlatform.buildRustPackage {
  pname = "openlogi";
  inherit version src;

  cargoLock = {
    lockFile = "${src}/Cargo.lock";
    # One hash per git repository, keyed by any crate from that repo.
    # Obtain new values from the error message of a failing build, or with
    # `nix-prefetch-git <url> --rev <rev>`.
    outputHashes = {
      "gpui-0.2.2" = "sha256-Av+unZNI39dEb+zwSIU+SkEjqagHWrc7W8KehEgQ4H8=";
      "gpui-component-0.5.2" = gpuiComponentHash;
      "gpui-updater-0.0.5" = "sha256-H2IW7nDD1q/Zbt7ZSft6VTMv5UpjiCg2FIQSoML/CMc=";
      "zed-font-kit-0.14.1-zed" = "sha256-KXygi0olNQi5yM8eaJVykNDtbPMDjT+cWPBF8UrtXR4=";
      "zed-reqwest-0.12.15-zed" = "sha256-p4SiUrOrbTlk/3bBrzN/mq/t+1Gzy2ot4nso6w6S+F8=";
      "zed-scap-0.0.8-zed" = "sha256-BihiQHlal/eRsktyf0GI3aSWsUCW7WcICMsC2Xvb7kw=";
      "zed-xim-0.4.0-zed" = "sha256-pRT4Sz1JU9ros47/7pmIW9kosWOGMOItcnNd+VrvnpE=";
    };
  };

  postPatch = ''
    # gpui-component's IconName proc-macro reads `../assets/assets/icons`
    # relative to its own crate, assuming the upstream repo's workspace
    # layout. The vendor tree lays crates out flat, so recreate the sibling
    # directory as a link to the gpui-component-assets crate. Fail loudly if
    # the glob doesn't resolve to exactly one directory.
    assets=("$cargoDepsCopy"/gpui-component-assets-*)
    if [ ''${#assets[@]} -ne 1 ] || [ ! -d "''${assets[0]}" ]; then
      echo "could not uniquely locate the vendored gpui-component-assets: ''${assets[*]}" >&2
      exit 1
    fi
    ln -sfn "''${assets[0]}" "$cargoDepsCopy/assets"

    # The workspace cargo config is dev-shell tooling: a macOS-scoped linker
    # and runner (inert on Linux), a default DEVELOPER_DIR, cargo aliases.
    # Nothing the sandboxed build needs — drop it so the build stays hermetic
    # rather than tracking whatever dev ergonomics land there next.
    rm -f .cargo/config.toml
  '';

  env.OPENLOGI_THEMES_DIR = "${gpuiComponentSrc}/themes";

  nativeBuildInputs = [
    pkg-config
    makeWrapper
    rustPlatform.bindgenHook # `media` (a gpui dep) runs bindgen — needs libclang
  ];

  # Only libraries whose *-sys crates appear in Cargo.lock. TLS is rustls;
  # evdev/hidraw are opened directly (pure Rust); vulkan is dlopened, so it
  # belongs in runtimeLibs above, not here.
  buildInputs = [
    fontconfig # GPUI text rendering (yeslogic-fontconfig-sys)
    freetype # font-kit (freetype-sys)
    libxkbcommon # GPUI keyboard handling
    wayland # wayland-sys
    libxcb # xcb / x11rb — the hook and GPUI's X11 backend
  ];

  # The three shipped binaries; xtask (macOS bundling/DMG) is not used on
  # Linux.
  cargoBuildFlags = [
    "--package=openlogi"
    "--package=openlogi-agent"
    "--package=openlogi-gui"
  ];

  # Some tests require real Logitech hardware, D-Bus, or uinput — none of
  # which exist in the sandbox. The Rust CI workflow runs the test suite.
  doCheck = false;

  postInstall = ''
    install -Dm644 packaging/linux/desktop/openlogi.desktop \
      "$out/share/applications/openlogi.desktop"
    install -Dm644 design/icon/openlogi.png \
      "$out/share/icons/hicolor/512x512/apps/openlogi.png"
    install -Dm644 packaging/linux/udev/70-openlogi.rules \
      "$out/lib/udev/rules.d/70-openlogi.rules"
    install -Dm644 packaging/linux/systemd/openlogi-agent.service \
      "$out/lib/systemd/user/openlogi-agent.service"
  '';

  postFixup = ''
    wrapProgram "$out/bin/openlogi-gui" \
      --prefix LD_LIBRARY_PATH : "${runtimeLibs}"

    # The packaged unit hardcodes /usr/bin; point it at this output.
    substituteInPlace "$out/lib/systemd/user/openlogi-agent.service" \
      --replace-fail /usr/bin/openlogi-agent "$out/bin/openlogi-agent"
  '';

  meta = {
    description = "Local-first companion for Logitech HID++ peripherals";
    homepage = "https://github.com/AprilNEA/OpenLogi";
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "openlogi";
    # Darwin support (the .app bundle, see nixpkgs' `openlogi`) could be
    # revived here later; this package is authored and tested on Linux.
    platforms = lib.platforms.linux;
  };
}
