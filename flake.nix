{
  description = "OpenLogi — local-first alternative to Logitech Options+ for HID++ devices";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  # Dev shell lives in devenv.nix (devenv.yaml); this flake only exposes the
  # buildable package so `nix build` is first-class.
  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (system: {
        openlogi = nixpkgs.legacyPackages.${system}.callPackage ./nix/package.nix { };
        default = self.packages.${system}.openlogi;
      });
    };
}
