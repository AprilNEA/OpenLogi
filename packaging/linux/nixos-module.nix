{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:

let
  cfg = config.programs.openlogi;
  # nixpkgs declares programs.openlogi.{enable,package} itself since
  # NixOS/nixpkgs@c729bab1b8 and installs the package and its udev rules.
  # Declaring them again fails evaluation, so on such a nixpkgs this module
  # only adds what upstream lacks: the agent's user service.
  upstream = builtins.pathExists (modulesPath + "/programs/openlogi.nix");
in
{
  options.programs.openlogi =
    lib.optionalAttrs (!upstream) {
      enable = lib.mkEnableOption "OpenLogi, a local-first Logitech device manager";

      package = lib.mkPackageOption pkgs "openlogi" { };
    }
    // {
      launchAtLogin = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Whether to start the OpenLogi agent with graphical sessions.";
      };
    };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      (lib.mkIf (!upstream) {
        environment.systemPackages = [ cfg.package ];
        services.udev.packages = [ cfg.package ];
      })
      {
        systemd.user.services.openlogi-agent = {
          description = "OpenLogi background agent";
          wantedBy = lib.optionals cfg.launchAtLogin [ "graphical-session.target" ];
          after = [ "graphical-session.target" ];
          partOf = lib.optionals cfg.launchAtLogin [ "graphical-session.target" ];

          serviceConfig = {
            ExecStart = lib.getExe' cfg.package "openlogi-agent";
            Restart = "on-failure";
            RestartSec = 5;
          };
        };
      }
    ]
  );
}
