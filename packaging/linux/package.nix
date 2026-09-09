# Nix package for OpenLogi on Linux (CLI + agent + GUI/overlay).
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
# prints the correct hash to paste. Nix CI watches Cargo.lock and the workspace
# manifests so that failure happens in the PR that moves the pin.
{
  lib,
  stdenv,
  rustPlatform,
  fetchgit,
  src,
  git,
  pkg-config,
  patchelf,
  versionCheckHook,
  fontconfig,
  freetype,
  libGL,
  libxkbcommon,
  wayland,
  vulkan-loader,
  libxcb,
}:

let
  # Keep documentation, CI metadata, and unrelated release tooling out of the
  # source derivation so editing them does not rebuild the application.
  source = lib.fileset.toSource {
    root = src;
    fileset = lib.fileset.unions [
      (src + "/Cargo.lock")
      (src + "/Cargo.toml")
      (src + "/LICENSE-APACHE")
      (src + "/LICENSE-MIT")
      (src + "/crates")
      (src + "/design/icon")
      (src + "/docs/config.example.toml")
      (src + "/packaging/linux/desktop")
      (src + "/packaging/linux/systemd")
      (src + "/packaging/linux/udev")
      # xtask's packaged-bins test include_str!s this; omitting it fails
      # `cargo test --workspace` inside the Nix sandbox.
      (src + "/packaging/linux/nfpm.yaml")
      (src + "/xtask")
    ];
  };

  # Single source of truth for the version: [workspace.package] in the
  # workspace Cargo.toml (every crate uses version.workspace = true). Read it
  # through the flake source accessor so package evaluation does not require
  # materialising the filtered build source first.
  version = (builtins.fromTOML (builtins.readFile (src + "/Cargo.toml"))).workspace.package.version;

  # GPUI discovers these graphics backends at runtime instead of linking them,
  # so normal fixup cannot infer their store paths. Add only those paths to the
  # GUI's RUNPATH; linked xkbcommon/xcb/font libraries are fixed up normally.
  runtimeLibs = lib.makeLibraryPath [
    libGL
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
  gpuiComponentRev = "da4f93696dc2b2b4d91bcc42412b9053a3d24de8";
  gpuiComponentHash = "sha256-A5NitDokr5+8tQaSO6gSyQpH3X/8FWQHC140CHnpVsY=";
  gpuiComponentSrc = fetchgit {
    url = "https://github.com/longbridge/gpui-component";
    rev = gpuiComponentRev;
    hash = gpuiComponentHash;
  };
in
rustPlatform.buildRustPackage {
  pname = "openlogi";
  inherit version;
  src = source;
  strictDeps = true;

  cargoLock = {
    # Parse dependency metadata through the flake source accessor as above;
    # only the actual build consumes the filtered source derivation.
    lockFile = src + "/Cargo.lock";
    # One hash per git repository, keyed by any crate from that repo.
    # Obtain new values from the error message of a failing build, or with
    # `nix-prefetch-git <url> --rev <rev>`.
    outputHashes = {
      "appicon-0.1.0" = "sha256-XY8NS2qrpPbUXZ3xCPGjZbbT0tSVpapbcTbgA2H5+/I=";
      "gpui-0.2.2" = "sha256-d2GVmZgvJzLk1pbNtPedw0V09+ANZFORZjTSLVxw7jc=";
      "gpui-component-0.5.2" = gpuiComponentHash;
      "gpui-updater-0.0.7" = "sha256-hxdATcCif7csqKLNoi41ETe09Ym6zM4rVzYvBDEvVg4=";
      "proptest-1.10.0" = "sha256-p5NTcHhruI8QQvANACg8AMRVNmuvGxs2NLit+/8PaWo=";
      "wasm_thread-0.3.3" = "sha256-+lRLCIk0S6Y5ORYjDKsYYHia2FtoSoh+rWkQh7mnPBE=";
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
  '';

  env.OPENLOGI_THEMES_DIR = "${gpuiComponentSrc}/themes";

  nativeBuildInputs = [
    pkg-config
    patchelf
    rustPlatform.bindgenHook # `media` (a gpui dep) runs bindgen — needs libclang
  ];

  # The xtask release tests exercise version-bump checkout against throwaway
  # repositories they `git init` themselves, so the sandboxed `cargo test`
  # needs a git binary even though the build does not.
  nativeCheckInputs = [ git ];

  # Only libraries whose *-sys crates appear in Cargo.lock. TLS is rustls and
  # evdev/hidraw are opened directly. Runtime-selected graphics libraries also
  # appear here when their headers/pkg-config metadata are needed at build time.
  buildInputs = [
    fontconfig # GPUI text rendering (yeslogic-fontconfig-sys)
    freetype # font-kit (freetype-sys)
    libGL # GPUI's OpenGL fallback
    libxkbcommon # GPUI keyboard handling
    wayland # wayland-sys
    vulkan-loader # GPUI's primary Linux renderer
    libxcb # xcb / x11rb — the hook and GPUI's X11 backend
  ];

  # Select production binaries explicitly: selecting the agent package alone
  # also builds the development-only openlogi-agent-mock target.
  cargoBuildFlags = [
    "--package=openlogi"
    "--bin=openlogi"
    "--package=openlogi-agent"
    "--bin=openlogi-agent"
    "--package=openlogi-desktop"
    "--bin=openlogi-desktop"
    "--package=openlogi-overlay"
    "--bin=openlogi-overlay"
  ];

  # Match Linux CI: the pure workspace tests run in the sandbox; GUI tests are
  # exercised on macOS because GPUI's Linux test harness is not headless.
  cargoTestFlags = [
    "--workspace"
    "--exclude=openlogi-desktop"
  ];

  installPhase = ''
    runHook preInstall

    releaseDir=target/${stdenv.hostPlatform.rust.rustcTarget}/release
    for binary in openlogi openlogi-agent openlogi-desktop openlogi-overlay; do
      install -Dm755 "$releaseDir/$binary" "$out/bin/$binary"
    done

    install -Dm644 packaging/linux/desktop/openlogi.desktop \
      "$out/share/applications/openlogi.desktop"
    # Every standard indexed hicolor size: a stock `hicolor/index.theme`
    # stops at 512x512, so an icon installed only under `1024x1024/apps` is
    # invisible to launchers that resolve by theme index.
    install -Dm644 design/icon/openlogi.png \
      "$out/share/icons/hicolor/1024x1024/apps/openlogi.png"
    for size in 512 256 128 64 48 32 16; do
      install -Dm644 "design/icon/openlogi-$size.png" \
        "$out/share/icons/hicolor/''${size}x''${size}/apps/openlogi.png"
    done
    # The GNOME Shell companion extension: per-app profiles read the focused
    # window's WM_CLASS through it, because Mutter exposes that nowhere else.
    # Installed, not enabled — GNOME only loads what the user opted into.
    for file in metadata.json extension.js; do
      install -Dm644 "crates/openlogi-hook/gnome-shell-extension/openlogi-frontmost@openlogi.dev/$file" \
        "$out/share/gnome-shell/extensions/openlogi-frontmost@openlogi.dev/$file"
    done
    install -Dm644 packaging/linux/udev/70-openlogi.rules \
      "$out/lib/udev/rules.d/70-openlogi.rules"
    install -Dm644 packaging/linux/systemd/openlogi-agent.service \
      "$out/share/systemd/user/openlogi-agent.service"
    install -Dm644 LICENSE-APACHE "$out/share/licenses/openlogi/LICENSE-APACHE"
    install -Dm644 LICENSE-MIT "$out/share/licenses/openlogi/LICENSE-MIT"

    substituteInPlace "$out/share/systemd/user/openlogi-agent.service" \
      --replace-fail \
        "ExecStart=/usr/bin/openlogi-agent" \
        "ExecStart=$out/bin/openlogi-agent"

    runHook postInstall
  '';

  postFixup = ''
    patchelf --add-rpath "${runtimeLibs}" "$out/bin/openlogi-desktop"
  '';

  doInstallCheck = true;
  nativeInstallCheckInputs = [ versionCheckHook ];
  preInstallCheck = ''
    for binary in openlogi openlogi-agent openlogi-desktop openlogi-overlay; do
      test -x "$out/bin/$binary"
    done
    test ! -e "$out/bin/openlogi-agent-mock"
    test -f "$out/lib/udev/rules.d/70-openlogi.rules"
    test -f "$out/share/applications/openlogi.desktop"
    for size in 1024 512 256 128 64 48 32 16; do
      test -f "$out/share/icons/hicolor/''${size}x''${size}/apps/openlogi.png"
    done
    test -f "$out/share/gnome-shell/extensions/openlogi-frontmost@openlogi.dev/metadata.json"
    test -f "$out/share/gnome-shell/extensions/openlogi-frontmost@openlogi.dev/extension.js"
    grep -Fqx \
      "ExecStart=$out/bin/openlogi-agent" \
      "$out/share/systemd/user/openlogi-agent.service"
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
