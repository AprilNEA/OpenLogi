# Nix package for OpenLogi on Linux (CLI + agent + GUI).
#
# Build via the flake:
#   nix build .#openlogi
#
# ## Why crane, and why there are no hashes here
#
# The flake removed in #262 used fetchCargoVendor: one `cargoHash` covering
# every dependency *plus* a copy of Cargo.lock, so every release bump
# invalidated it. Its replacement (#491) used rustPlatform's `cargoLock`, which
# fixed that but still needed one manual hash per git repository and — being a
# single derivation — recompiled the whole dependency tree, gpui included, on
# every source change. That is the ~22 min every PR was paying.
#
# crane addresses both:
# - Dependencies build in their own derivation, so a source-only change reuses
#   them and only the workspace's own crates recompile.
# - Git dependencies are fetched with `builtins.fetchGit`, which is addressed
#   by the revision Cargo.lock already pins. There are no `outputHashes` to
#   maintain, and none to go stale when a pin moves.
{
  lib,
  craneLib,
  src,
  rustPlatform,
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
  # at the repository root next to (not inside) the gpui-component crate, and
  # vendoring extracts crates rather than repositories — so they are absent
  # from the vendor tree. build.rs takes OPENLOGI_THEMES_DIR as an explicit
  # override; point it at a separate checkout. The rev must match Cargo.lock,
  # or the GUI builds against themes it was not locked to.
  gpuiComponentSrc = builtins.fetchGit {
    url = "https://github.com/longbridge/gpui-component";
    rev = "031555662e99a1b5a549990b47f246d475b8288a";
    allRefs = true;
  };

  cargoVendorDir = craneLib.vendorCargoDeps {
    inherit src;

    # gpui-component's IconName proc-macro reads `../assets/assets/icons`
    # relative to its own crate — the upstream repo's layout, where the assets
    # live beside it rather than inside it. Vendoring extracts that repo's
    # crates as siblings, so the directory the macro walks up to is missing;
    # recreate it as a link to the assets crate. This has to happen inside the
    # checkout rather than over the finished vendor tree, because the tree's
    # generated config.toml points cargo at these store paths by absolute
    # path — a patched copy of it would simply never be read. Fail loudly if
    # the glob doesn't resolve to exactly one directory.
    overrideVendorGitCheckout =
      packages: drv:
      if lib.any (p: p.name == "gpui-component") packages then
        drv.overrideAttrs (old: {
          postInstall = (old.postInstall or "") + ''
            assets=("$out"/gpui-component-assets-*)
            if [ ''${#assets[@]} -ne 1 ] || [ ! -d "''${assets[0]}" ]; then
              echo "could not uniquely locate the vendored gpui-component-assets: ''${assets[*]}" >&2
              exit 1
            fi
            ln -sfn "''${assets[0]}" "$out/assets"
          '';
        })
      else
        drv;
  };

  commonArgs = {
    inherit src version cargoVendorDir;
    pname = "openlogi";
    strictDeps = true;

    # The workspace cargo config is dev-shell tooling: a macOS-scoped linker
    # and runner (inert on Linux), a default DEVELOPER_DIR, cargo aliases.
    # Nothing the sandboxed build needs — drop it so the build stays hermetic
    # rather than tracking whatever dev ergonomics land there next.
    postPatch = ''
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
    cargoExtraArgs = "--package=openlogi --package=openlogi-agent --package=openlogi-gui";

    # Some tests require real Logitech hardware, D-Bus, or uinput — none of
    # which exist in the sandbox. The Rust CI workflow runs the test suite.
    doCheck = false;
  };
in
craneLib.buildPackage (
  commonArgs
  // {
    # The dependency-only build that every later source change reuses.
    cargoArtifacts = craneLib.buildDepsOnly commonArgs;

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
)
