# Installing OpenLogi on Linux

> [!NOTE]
> Linux support is in active development. HID++ device enumeration supports
> **Logi Bolt** (USB PID `0xC548`) and **Logi Unifying** (PID `0xC52B` and
> others) receivers, as well as Bluetooth-direct devices.

## Prerequisites

- **Quit Solaar** (or any other Logitech manager) before starting OpenLogi — the
  two applications fight over HID++ access.
- A kernel with `hidraw` and `uinput` module support (standard on all major
  distros).
- `systemd` + `udev` (standard on Ubuntu, Fedora, Arch, Debian, openSUSE, …).
- GLIBC 2.35 or newer for the pre-built packages (Ubuntu 22.04 baseline).

## NixOS

The repository Flake provides a package and a NixOS module for x86_64 and
aarch64 Linux. Importing the module is preferred over adding the package to
`environment.systemPackages` by itself: the module also registers the udev
rules required for device access and manages the agent's user service.

```nix
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.openlogi = {
    url = "github:AprilNEA/OpenLogi";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { nixpkgs, openlogi, ... }: {
    nixosConfigurations.my-host = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux"; # or aarch64-linux
      modules = [
        openlogi.nixosModules.default
        {
          programs.openlogi = {
            enable = true;
            # Starts openlogi-agent with graphical-session.target by default.
            launchAtLogin = true;
          };
        }
      ];
    };
  };
}
```

Set `programs.openlogi.launchAtLogin = false` to install the package and udev
rules without automatically starting the agent. It remains available as
`systemctl --user start openlogi-agent.service`.

For a build without installing the module:

```sh
nix build github:AprilNEA/OpenLogi#openlogi
```

## Build from source

Pre-built `.deb` and `.rpm` packages are available on the
[releases page](https://github.com/AprilNEA/OpenLogi/releases/latest) — see
the main [README](../README.md#linux) for the package-based install. To build
from source instead, use the stable Rust toolchain:

```sh
git clone https://github.com/AprilNEA/OpenLogi
cd OpenLogi
cargo build --release -p openlogi -p openlogi-desktop -p openlogi-agent
```

Four production executables land in `target/release/`:

| Binary | Role |
|---|---|
| `openlogi` | CLI — inventory, diagnostics, asset sync |
| `openlogi-desktop` | Desktop GUI |
| `openlogi-overlay` | Actions Ring overlay helper |
| `openlogi-agent` | Background agent — HID++ loop, input hook |

## Device access: udev rules

OpenLogi needs:

- **Write access to `/dev/uinput`** — to create the virtual input device for
  button remapping.
- **Read/write access to `/dev/hidraw*`** — to send HID++ commands to the Bolt
  receiver, or to the device itself when it is paired over Bluetooth.
- **Read access to the mouse's `/dev/input/event*` node** — the hook grabs the
  pointer there to capture button presses. Bluetooth mice need the bundled rule
  for this: their event node hangs off `/devices/virtual/misc/uhid`, which has
  no seat, so `logind` never grants the ACL on its own.

Install the bundled udev rules to grant access to the active-seat user without
requiring `sudo` or group membership (requires `systemd-logind`):

```sh
sudo cp packaging/linux/udev/70-openlogi.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Verify access (should open without error):

```sh
# Check uinput
openlogi-agent --check-uinput 2>/dev/null || \
    test -w /dev/uinput && echo "uinput OK"

# Check a hidraw node
ls -la /dev/hidraw*

# Check the mouse's event node — look for a "+" (ACL) in the mode, or your
# user in the ACL itself. Without it the agent logs
# "could not install OS mouse hook".
getfacl /dev/input/event*
```

The GUI Settings → Permissions page shows a live `Granted` / `Not granted`
indicator; check it after installing the rules (no restart needed).

> **Device already connected?** `udevadm trigger` re-evaluates rules but does
> not re-grant `uaccess` ACLs on nodes that were already open when the rules
> were installed. If access is still denied, unplug and replug your receiver or
> mouse (or power-cycle for wireless devices) to let udev apply the new rules on
> reconnect.

### Non-systemd systems (SysV init, OpenRC)

Replace `TAG+="uaccess"` in the rules file with `MODE="0660", GROUP="input"`,
then add your user to the `input` group:

```sh
sudo usermod -aG input "$USER"
# Re-login for the group change to take effect.
```

## Install with the script

The `packaging/linux/install.sh` script copies the binaries, udev rules,
systemd unit, desktop entry, and icon to system paths, then reloads `udevadm`.

```sh
# From the repo root, after building:
sudo packaging/linux/install.sh
# Or to a custom prefix (e.g. /usr):
packaging/linux/install.sh --prefix=/usr
```

To remove:

```sh
packaging/linux/uninstall.sh
```

## Autostart (launch at login)

The background agent (`openlogi-agent`) must be running for the GUI and CLI to
show connected devices. Enable it for your user session:

```sh
systemctl --user enable --now openlogi-agent.service
```

Alternatively, toggle **Settings → General → Launch at login** in the GUI — it
writes the unit to `~/.config/systemd/user/openlogi-agent.service`
automatically.

## Per-app profiles on GNOME/Wayland

Mutter lets no ordinary client see which window is focused, and implements
neither `wlr-foreign-toplevel` nor a focused-window portal, so per-app profiles
need a small companion GNOME Shell extension. The packages, the Nix derivation
and `install.sh` all place it at
`/usr/share/gnome-shell/extensions/openlogi-frontmost@openlogi.dev/`.

They install it but do not enable it — GNOME loads only extensions the user
opted into. Log out and back in (Wayland cannot reload the shell in place),
then:

```sh
gnome-extensions enable openlogi-frontmost@openlogi.dev
```

The extension reads only `focus_window.get_wm_class()`; see
`crates/openlogi-hook/gnome-shell-extension/README.md`. Nothing else needs it —
button remaps, DPI and SmartShift work without it, and X11/XWayland sessions
resolve the focused window without any extension at all.

## Verify the installation

```sh
# List connected Logitech devices:
openlogi list

# Launch the GUI:
openlogi-desktop
```

## Known limitations

| Limitation | Status |
|---|---|
| Wayland: per-application profile switching | Requires XWayland (`WM_CLASS` lookup uses X11) |
| Button capture: middle / mode-shift / thumbwheel | Side buttons only today |
