{
  description = "OpenLogi — local-first companion for Logitech HID++ peripherals";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  inputs.crane.url = "github:ipetkov/crane";

  # The dev shell lives in devenv.nix (devenv.yaml); this flake only exposes
  # the buildable Linux package so `nix build` / `nix run` are first-class.
  # See nix/package.nix for why this vendoring approach avoids the cargoHash
  # churn that led to the previous flake's removal (#262).
  outputs =
    {
      self,
      nixpkgs,
      crane,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          openlogi = pkgs.callPackage ./nix/package.nix {
            src = self;
            craneLib = crane.mkLib pkgs;
          };
          default = self.packages.${system}.openlogi;
        }
      );
    };
}
