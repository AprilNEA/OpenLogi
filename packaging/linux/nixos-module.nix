{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.openlogi;
in
{
  options.programs.openlogi = {
    enable = lib.mkEnableOption "OpenLogi, a local-first Logitech device manager";

    package = lib.mkPackageOption pkgs "openlogi" { };

    launchAtLogin = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Whether to start the OpenLogi agent with graphical sessions.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    services.udev.packages = [ cfg.package ];

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
  };
}
