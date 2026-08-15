{
  description = "OpenLogi — local-first companion for Logitech HID++ peripherals";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  # The dev shell lives in devenv.nix (devenv.yaml). This flake owns the Linux
  # package, its NixOS integration, checks, and formatter.
  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      packageFor =
        system: nixpkgs.legacyPackages.${system}.callPackage ./packaging/linux/package.nix { src = ./.; };
      moduleCheckFor =
        system:
        let
          lib = nixpkgs.lib;
          pkgs = nixpkgs.legacyPackages.${system};
          package = self.packages.${system}.openlogi;
          evaluate =
            launchAtLogin:
            (lib.nixosSystem {
              inherit system;
              modules = [
                self.nixosModules.default
                {
                  programs.openlogi = {
                    enable = true;
                    inherit launchAtLogin;
                  };
                  system.stateVersion = "26.05";
                }
              ];
            }).config;
          enabled = evaluate true;
          manual = evaluate false;
        in
        assert lib.elem package enabled.environment.systemPackages;
        assert lib.elem package enabled.services.udev.packages;
        assert enabled.systemd.user.services.openlogi-agent.wantedBy == [ "graphical-session.target" ];
        assert manual.systemd.user.services.openlogi-agent.wantedBy == [ ];
        assert enabled.systemd.user.services.openlogi-agent.after == [ "graphical-session.target" ];
        assert enabled.systemd.user.services.openlogi-agent.partOf == [ "graphical-session.target" ];
        assert manual.systemd.user.services.openlogi-agent.partOf == [ ];
        assert
          enabled.systemd.user.services.openlogi-agent.serviceConfig.ExecStart
          == "${package}/bin/openlogi-agent";
        pkgs.runCommand "openlogi-nixos-module-check" { } ''
          touch "$out"
        '';
    in
    {
      packages = forAllSystems (
        system:
        let
          openlogi = packageFor system;
        in
        {
          inherit openlogi;
          default = openlogi;
        }
      );

      checks = forAllSystems (system: {
        package = self.packages.${system}.openlogi;
        nixos-module = moduleCheckFor system;
      });

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);

      nixosModules = {
        openlogi =
          {
            lib,
            pkgs,
            ...
          }:
          {
            imports = [ ./packaging/linux/nixos-module.nix ];
            programs.openlogi.package = lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.openlogi;
          };
        default = self.nixosModules.openlogi;
      };
    };
}
