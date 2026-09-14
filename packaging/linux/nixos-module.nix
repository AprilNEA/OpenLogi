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
        # See packaging/linux/systemd/openlogi-agent.service: a user unit can't
        # order against system bluetooth.service via after=, so this waits for
        # the adapter node to appear (and settle) before HID++ probing starts,
        # avoiding a boot-time race with kernel-side Bluetooth bring-up (#1065).
        ExecStartPre = "${pkgs.bash}/bin/bash -c '[ -d /sys/class/bluetooth ] || exit 0; for i in $(seq 1 20); do ls /sys/class/bluetooth/hci* >/dev/null 2>&1 && { sleep 3; exit 0; }; sleep 0.5; done'";
        ExecStart = lib.getExe' cfg.package "openlogi-agent";
        Restart = "on-failure";
        RestartSec = 5;
      };
    };
  };
}
