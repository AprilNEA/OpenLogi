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
      (src + "/design/icon/openlogi.png")
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

  # The packaged binaries, defined once: install, fixup, and the install check
  # all iterate these lists, so adding a binary in one place cannot leave it
  # unpatched or unchecked. GPUI binaries are the ones that need the runtime
  # graphics backends on their RUNPATH.
  gpuiBinaries = [
    "openlogi-desktop"
    "openlogi-overlay"
  ];
  binaries = [
    "openlogi"
    "openlogi-agent"
  ]
  ++ gpuiBinaries;

  # GPUI discovers these graphics backends at runtime instead of linking them,
  # so normal fixup cannot infer their store paths. Add only those paths to the
  # RUNPATH of every GPUI binary; linked xkbcommon/xcb/font libraries are fixed
  # up normally.
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
      "gpui-0.2.2" = "sha256-Av+unZNI39dEb+zwSIU+SkEjqagHWrc7W8KehEgQ4H8=";
      "gpui-component-0.5.2" = gpuiComponentHash;
      "gpui-updater-0.0.7" = "sha256-hxdATcCif7csqKLNoi41ETe09Ym6zM4rVzYvBDEvVg4=";
      "proptest-1.10.0" = "sha256-p5NTcHhruI8QQvANACg8AMRVNmuvGxs2NLit+/8PaWo=";
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
    for binary in ${toString binaries}; do
      install -Dm755 "$releaseDir/$binary" "$out/bin/$binary"
    done

    install -Dm644 packaging/linux/desktop/openlogi.desktop \
      "$out/share/applications/openlogi.desktop"
    install -Dm644 design/icon/openlogi.png \
      "$out/share/icons/hicolor/1024x1024/apps/openlogi.png"
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

  # The desktop app and the Actions Ring overlay are siblings: both are GPUI
  # processes, so both need the runtime backends on their RUNPATH. Patching
  # only the desktop app left the overlay panicking on `NoWaylandLib` at
  # startup, which the agent's supervisor turned into a restart loop.
  postFixup = ''
    for binary in ${toString gpuiBinaries}; do
      patchelf --add-rpath "${runtimeLibs}" "$out/bin/$binary"
    done
  '';

  doInstallCheck = true;
  nativeInstallCheckInputs = [ versionCheckHook ];
  preInstallCheck = ''
    for binary in ${toString binaries}; do
      test -x "$out/bin/$binary"
    done
    test ! -e "$out/bin/openlogi-agent-mock"
    test -f "$out/lib/udev/rules.d/70-openlogi.rules"
    test -f "$out/share/applications/openlogi.desktop"
    test -f "$out/share/icons/hicolor/1024x1024/apps/openlogi.png"
    grep -Fqx \
      "ExecStart=$out/bin/openlogi-agent" \
      "$out/share/systemd/user/openlogi-agent.service"

    # GPUI dlopens its graphics backends, so a missing RUNPATH entry is not a
    # link error the build can catch: the process panics with `NoWaylandLib`
    # the instant it starts. Two GPUI processes ship here, and the agent
    # restarts the overlay indefinitely, so an unpatched overlay degrades into
    # a restart loop rather than into a visible failure. Assert the RUNPATH of
    # every GPUI binary instead of trusting postFixup to have listed them all.
    for binary in ${toString gpuiBinaries}; do
      rpath=$(patchelf --print-rpath "$out/bin/$binary")
      for entry in $(echo "${runtimeLibs}" | tr ':' ' '); do
        case ":$rpath:" in
          *":$entry:"*) ;;
          *)
            echo "$binary lacks $entry in its RUNPATH ($rpath)" >&2
            exit 1
            ;;
        esac
      done
    done
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
