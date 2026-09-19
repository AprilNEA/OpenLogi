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

    # The udev rule grants /dev/uinput with TAG+="uaccess", which logind applies
    # in response to a device event — and a host that has never loaded the
    # uinput module has no such device, so the node stays root-owned and the
    # agent cannot create the virtual device button remapping needs. The other
    # packaging paths ship /etc/modules-load.d/openlogi.conf for this; on NixOS
    # the equivalent is asking for the module here.
    boot.kernelModules = [ "uinput" ];

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
