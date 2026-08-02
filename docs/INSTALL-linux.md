# Installing OpenLogi on Linux

> [!NOTE]
> Linux support is in active development. HID++ device enumeration supports
> **Logi Bolt** (USB PID `0xC548`), **Logi Unifying** (`0xC52B`, `0xC532`), and
> **Logitech LIGHTSPEED** gaming receivers (including G305 receiver `0xC53F`),
> as well as Bluetooth-direct devices.

## Prerequisites

- **Quit Solaar** (or any other Logitech manager) before starting OpenLogi — the
  two applications fight over HID++ access.
- A kernel with `hidraw` and `uinput` module support (standard on all major
  distros).
- `systemd` + `udev` (standard on Ubuntu, Fedora, Arch, Debian, openSUSE, …).

## Build from source

Pre-built `.deb` and `.rpm` packages are available on the
[releases page](https://github.com/AprilNEA/OpenLogi/releases/latest) — see
the main [README](../README.md#linux) for the package-based install. To build
from source instead, use the stable Rust toolchain:

```sh
git clone https://github.com/AprilNEA/OpenLogi
cd OpenLogi
cargo build --release
```

The three binaries land in `target/release/`:

| Binary | Role |
|---|---|
| `openlogi` | CLI — inventory, diagnostics, asset sync |
| `openlogi-gui` | Desktop GUI |
| `openlogi-agent` | Background agent — HID++ loop, input hook |

## Device access: udev rules

OpenLogi needs:

- **Write access to `/dev/uinput`** — to create the virtual input device for
  button remapping.
- **Read/write access to `/dev/hidraw*`** — to send HID++ commands to a
  supported receiver or directly connected device.

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

## Verify the installation

```sh
# List connected Logitech devices:
openlogi list

# Launch the GUI:
openlogi-gui
```

## Known limitations

| Limitation | Status |
|---|---|
| Wayland: per-application profile switching | Requires XWayland (`WM_CLASS` lookup uses X11) |
| Button capture: middle / mode-shift / thumbwheel | Side buttons only today |
