# Nix package for OpenLogi (the GUI .app), modeled on nixpkgs' zed-editor.
#
# Build via the flake (`nix build .#openlogi`) or standalone:
#   nix-build -E 'with import <nixpkgs> {}; callPackage ./nix/package.nix {}'
#
# For a nixpkgs submission, swap the local `src` for the fetchFromGitHub block
# below, pointed at a release tag that ships crates/openlogi-gui/icon/AppIcon.icns.
{
  lib,
  rustPlatform,
  fetchFromGitHub,
  stdenv,
  cargo-bundle,
  nix-update-script,
}:

rustPlatform.buildRustPackage (finalAttrs: {
  pname = "openlogi";
  version = "0.5.3";

  # Build from the working tree (target/.git/etc. filtered out). cargo-bundle
  # uses the committed crates/openlogi-gui/icon/AppIcon.icns.
  src = lib.cleanSourceWith {
    src = ../.;
    filter =
      path: _type:
      !(builtins.elem (baseNameOf path) [
        "target"
        ".git"
        ".devenv"
        ".direnv"
        "result"
      ]);
  };
  # For nixpkgs, replace the local `src` above with a tagged tarball:
  #   src = fetchFromGitHub {
  #     owner = "AprilNEA";
  #     repo = "OpenLogi";
  #     tag = "v${finalAttrs.version}"; # a release that ships the committed .icns
  #     hash = lib.fakeHash;            # TODO
  #   };

  # One FOD vendors every dependency, including the zed / wgpu / font-kit git
  # forks gpui pulls in. Same approach as nixpkgs' zed-editor.
  # Stable across version bumps (cargoDepsName decouples the vendored-dir
  # name from the version so the hash only changes when Cargo.lock deps change).
  cargoDepsName = finalAttrs.pname;
  cargoHash = "sha256-HJuEQvMwlIZXb2W44m2WzFSiNb56pozSJSDe7ApmmGI=";

  postPatch = ''
    # .cargo/config.toml forces `linker = /usr/bin/cc` + a /Applications/Xcode
    # DEVELOPER_DIR for local macOS dev — neither exists in the Nix sandbox, so
    # linking would fail. Drop it; Nix's cc wrapper + runtime_shaders (below)
    # are what we want. (nixpkgs' zed-editor drops its config for the same reason.)
    rm -f .cargo/config.toml

    # gpui-component's IconName proc-macro reads `../assets/assets/icons`
    # relative to its own crate, assuming the upstream repo's workspace layout.
    # Cargo vendors each git crate separately, so that sibling dir is gone —
    # recreate it by linking the gpui-component-assets crate (the lucide SVGs).
    ( cd "$cargoDepsCopy/source-git-1" && ln -sfn gpui-component-assets-* assets )
  '';

  nativeBuildInputs = [
    cargo-bundle # assembles OpenLogi.app from [package.metadata.bundle]
  ];

  # Only the GUI crate. The CLI (`openlogi`) doesn't depend on gpui_platform, so
  # it can't share the `buildFeatures` below — package it separately if wanted.
  cargoBuildFlags = [ "--package=openlogi-gui" ];

  # Required on darwin: compile Metal shaders at runtime instead of invoking the
  # proprietary `metal` compiler at build time (it isn't in the Nix sandbox).
  # Same trick nixpkgs' zed-editor uses to ship gpui on macOS.
  buildFeatures = lib.optionals stdenv.hostPlatform.isDarwin [ "gpui_platform/runtime_shaders" ];

  # gpui tests would hit the same Metal path; nothing here is worth running.
  doCheck = false;

  # The in-app updater reads `option_env!("OPENLOGI_UPDATE_MANIFEST_URL")`;
  # leaving it unset disables self-updates — correct for a Nix-managed install.

  installPhase = lib.optionalString stdenv.hostPlatform.isDarwin ''
    runHook preInstall

    release_target="target/${stdenv.hostPlatform.rust.cargoShortTarget}/release"
    # cargo-bundle (with SKIP_BUILD) expects the binary at target/release.
    mv "$release_target/openlogi-gui" target/release/openlogi-gui

    pushd crates/openlogi-gui
    export CARGO_BUNDLE_SKIP_BUILD=true
    app_path=$(cargo bundle --release | xargs)
    popd

    mkdir -p "$out/Applications"
    mv "$app_path" "$out/Applications/"

    runHook postInstall
  '';

  # `nix-update openlogi` (and nixpkgs' autobump) bump the version and refetch
  # src.hash + cargoHash automatically — effective once `src` is the
  # fetchFromGitHub form above (a local `src` has no remote version to track).
  passthru.updateScript = nix-update-script { };

  meta = {
    description = "Local-first alternative to Logitech Options+ for HID++ devices";
    homepage = "https://openlogi.org/";
    license = with lib.licenses; [
      mit
      asl20
    ];
    maintainers = [ ]; # TODO: add yourself
    platforms = lib.platforms.darwin; # Linux once the port lands (+ x11/wayland/vulkan inputs)
  };
})
