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
- `minisign`, installed through your distribution's package manager, for
  authenticating release packages.

## Install a release package

Install `minisign` through your distribution, then run the installer as your
normal user (not with `sudo`):

```sh
curl -fsSL https://github.com/AprilNEA/OpenLogi/releases/latest/download/install.sh | sh
```

The script resolves the latest GitHub release, detects `x86_64`/`aarch64` and
apt, dnf, yum, zypper, rpm, or pacman, then downloads the exact matching
`.deb`, `.rpm`, or `.pkg.tar.zst`. It authenticates that package's detached
signature against the minisign public key embedded in the script, verifies the
package's one entry in the release `SHA256SUMS`, and only then runs the
package-manager command with `sudo`. nFPM's package scripts reload udev and the
desktop/icon caches. The installer refuses to run as root.

### Review the installer first

The one-line command trusts the `install.sh` attached to the release. To read
and verify the script before it runs, download it with its signature instead:

```sh
base=https://github.com/AprilNEA/OpenLogi/releases/latest/download
curl --proto '=https' --tlsv1.2 -fsSLO "$base/install.sh"
curl --proto '=https' --tlsv1.2 -fsSLO "$base/install.sh.minisig"
minisign -Vm install.sh -P RWRRkFtw+rqkvTlCTGKUszSE5dX9CK1teaQD45jO4P9rYlWLO4/nHVUF
less install.sh
sh install.sh
```

The public key is the same one the script embeds for package signatures.

Useful options:

```sh
# Pin a release (an optional leading v is accepted):
sh install.sh --version "$VERSION"

# Pass options through the one-line command with `sh -s --`:
curl -fsSL https://github.com/AprilNEA/OpenLogi/releases/latest/download/install.sh | sh -s -- --dry-run

# Override detection, verify without installing, or leave the agent stopped:
sh install.sh --package-manager zypper
sh install.sh --dry-run
sh install.sh --no-start
```

The installer enables and starts `openlogi-agent.service` for the current user
when systemd is available; failure to reach the user service manager does not
roll back an otherwise successful package installation.

## NixOS

The Flake in the repository's `packaging/` directory provides a package and a
NixOS module for x86_64 and aarch64 Linux. Importing the module is preferred
over adding the package to `environment.systemPackages` by itself: the module
also registers the udev rules required for device access and manages the
agent's user service.

```nix
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.openlogi = {
    url = "github:AprilNEA/OpenLogi?dir=packaging";
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
nix build 'github:AprilNEA/OpenLogi?dir=packaging#openlogi'
```

Earlier versions of this guide used `github:AprilNEA/OpenLogi` without
`?dir=packaging`. That URL now points to a deprecated alias: it needs Nix 2.26
or newer, prints a warning on every evaluation, and will be removed in a later
release. Add `?dir=packaging` to the input URL to move off it.

## Flatpak

Each release carries a `.flatpak` bundle for `amd64` and `arm64`, on the
[releases page](https://github.com/AprilNEA/OpenLogi/releases/latest):

```sh
flatpak install --user ./openlogi-*.flatpak
```

It is a one-time install. There is no remote behind a bundle, so `flatpak
update` has nothing to check and a new version means downloading the next one.

> [!IMPORTANT]
> A Flatpak cannot write to `/etc`, so the udev rules have to be installed on
> the host separately. **Until you do, no devices are detected at all**: the
> sandbox is allowed to reach `/dev/hidraw*`, `/dev/uinput` and
> `/dev/input/event*`, but the kernel still refuses your user access to them,
> so the app opens to an empty device list.

The same rules the packages install ship inside the app, so this needs no
checkout and no extra download:

```sh
flatpak run --command=cat org.openlogi.OpenLogi \
  /app/share/openlogi/udev/70-openlogi.rules |
  sudo tee /etc/udev/rules.d/70-openlogi.rules >/dev/null

sudo udevadm control --reload-rules
sudo udevadm trigger
```

Nothing extra is needed for `/dev/uinput`: the rules create its node at boot
with `static_node=uinput`, and opening it loads the module. See
[Device access: udev rules](#device-access-udev-rules) for what the rules grant
and how to verify them.

The agent installs its input hook once at startup, so restart the app after
granting access:

```sh
flatpak kill org.openlogi.OpenLogi
```

Then launch it again. There is no user service to enable — unlike the packages,
the application launches its own agent.

## Build from source

Pre-built `.deb` and `.rpm` packages are available on the
[releases page](https://github.com/AprilNEA/OpenLogi/releases/latest) — see
the main [README](../README.md#linux) for the package-based install. To build
from source instead, use the stable Rust toolchain:

```sh
git clone https://github.com/AprilNEA/OpenLogi
cd OpenLogi
cargo build --release \
  -p openlogi -p openlogi-desktop -p openlogi-overlay -p openlogi-agent
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

## Install from source with the script

From a repository checkout, `--from-source` copies the four local
`target/release` binaries, udev rules, systemd unit, desktop entry, and icon to
system paths, then reloads udev and desktop/icon caches. The script requests
`sudo` only for system files and lifecycle commands; do not run the whole
installer with `sudo`. `--prefix` applies only to this mode.

```sh
# From the repo root, after building:
packaging/linux/install.sh --from-source
# Or to a custom prefix (e.g. /usr):
packaging/linux/install.sh --from-source --prefix=/usr
```

To remove a source installation (release packages should be removed through
their package manager):

```sh
packaging/linux/uninstall.sh
```

## Autostart (launch at login)

The background agent (`openlogi-agent`) must be running for the GUI and CLI to
show connected devices. Enable it for your user session:

```sh
systemctl --user enable --now openlogi-agent.service
```

Alternatively, toggle **Settings → General → Launch at login** in the GUI. When
a packaged unit is already installed it simply enables that one. Otherwise — a
build from source, or an install under a custom prefix — it generates a unit at
`~/.local/share/systemd/user/openlogi-agent.service` pointing at the running
binary.

Either way `~/.config/systemd/user/openlogi-agent.service` stays yours: systemd
ranks it above both locations, so a unit you write there overrides whatever
OpenLogi does. Use `systemctl --user edit openlogi-agent.service` for a drop-in
that survives package upgrades.

OpenLogi never overwrites or deletes a unit it did not generate, at either
location, and it only turns off an autostart it turned on. Enabling the service
yourself with the command above keeps working regardless of the GUI toggle.

## Brightness keys on external monitors

The Brightness Up / Down actions press the standard Linux brightness keys
(`KEY_BRIGHTNESSUP` / `KEY_BRIGHTNESSDOWN`). Your desktop decides what to do
with them. On a laptop, GNOME and KDE change the built-in screen, so it should
work natively.

An external monitor is different. If `/sys/class/backlight` is empty, your
desktop ignores the keys. You can still control the monitor over DDC/CI with
[ddcutil](https://www.ddcutil.com/). This setup is not part of OpenLogi; it is
one example that works.

1. Install `ddcutil` and a daemon that runs a command when a key is pressed,
   such as `triggerhappy`. For example, on Debian or Ubuntu:

   ```sh
   sudo apt install ddcutil triggerhappy
   ```

2. Find the I2C bus of your monitor. Look for the `I2C bus: /dev/i2c-N` line:

   ```sh
   sudo ddcutil detect
   ```

3. Check that the monitor accepts DDC/CI (code `10` is brightness):

   ```sh
   sudo ddcutil --bus N getvcp 10
   ```

4. Add the key rules in `/etc/triggerhappy/triggers.d/brightness.conf`. Replace
   `N` with your bus number:

   ```
   KEY_BRIGHTNESSUP    1    /usr/bin/ddcutil --bus N --disable-dynamic-sleep --sleep-multiplier 0.2 setvcp 10 + 10
   KEY_BRIGHTNESSDOWN  1    /usr/bin/ddcutil --bus N --disable-dynamic-sleep --sleep-multiplier 0.2 setvcp 10 - 10
   ```

   These flags make each press fast (about 0.2 s instead of 2 s or more):

   - `--bus N` skips the search for the monitor.
   - `--disable-dynamic-sleep` stops ddcutil from waiting longer and longer
     when the monitor was idle for a while.
   - `--sleep-multiplier 0.2` shortens the standard DDC/CI waits. If some
     presses do nothing on your monitor, try a bigger value like `0.5`.

5. ddcutil needs access to `/dev/i2c-*`, and a packaged triggerhappy service
   may run as an unprivileged user (on Debian it is `nobody`). One simple way
   is to run it as root. For example, open an override with
   `sudo systemctl edit triggerhappy` and add:

   ```ini
   [Service]
   ExecStart=
   ExecStart=/usr/sbin/thd --triggers /etc/triggerhappy/triggers.d/ --socket /run/thd.socket --deviceglob /dev/input/event*
   ```

6. Debian only: also fix the triggerhappy socket. Debian's `triggerhappy.socket`
   creates a stream socket, but triggerhappy talks over a datagram socket. With
   the wrong type, triggerhappy never sees input devices that show up after it
   starts, and OpenLogi's virtual keyboard is one of them. Open an override
   with `sudo systemctl edit triggerhappy.socket` and add:

   ```ini
   [Socket]
   ListenStream=
   ListenDatagram=/run/thd.socket
   ```

7. Restart the socket and the service together. Restarting only the service
   removes the socket file:

   ```sh
   sudo systemctl stop triggerhappy.service triggerhappy.socket
   sudo systemctl start triggerhappy.socket triggerhappy.service
   ```

   To check the socket, run `th-cmd --socket /run/thd.socket --enable`. It
   should print nothing.

## Verify the installation

```sh
# List connected Logitech devices:
openlogi list

# Launch the GUI:
openlogi-desktop
```

## Hyprland and Omarchy

When the agent runs inside a Hyprland session, native actions use the
Hyprland/Omarchy-compatible commands below:

| OpenLogi action | Linux behavior |
|---|---|
| `LaunchpadShow` | `omarchy-menu toggle` |
| `PreviousDesktop` | `hyprctl dispatch workspace e-1` |
| `NextDesktop` | `hyprctl dispatch workspace e+1` |
| `LockScreen` | `omarchy-system-lock` |

The lock action never sends `Super+L` on Hyprland because Omarchy uses that
shortcut to change the workspace layout. If an Omarchy helper is unavailable
or exits unsuccessfully, OpenLogi falls back to the corresponding compositor
shortcut; the lock fallback is `Super+Ctrl+L`.

The agent selects this behavior when `HYPRLAND_INSTANCE_SIGNATURE` is present.
Because the agent normally runs as a systemd user service, verify that its
manager environment contains both that variable and `XDG_RUNTIME_DIR`:

```sh
systemctl --user show-environment | \
    grep -E '^(HYPRLAND_INSTANCE_SIGNATURE|XDG_RUNTIME_DIR)='
```

If either variable is missing, configure the session to import it into the
systemd user manager before restarting `openlogi-agent.service`. The helper
executables are resolved from `/usr/bin` and `/usr/local/bin`; missing helpers
use the keyboard fallbacks above.

## Known limitations

| Limitation | Status |
|---|---|
| Wayland: per-application profile switching | Native on wlroots compositors (sway, Hyprland, …); GNOME needs the [companion extension](../crates/openlogi-hook/gnome-shell-extension/README.md); KDE falls back to XWayland. wlroots reports the xdg-shell `app_id` while X11 and GNOME report `WM_CLASS`, so a profile keyed under one does not match under the other |
| Button capture: middle / mode-shift / thumbwheel | Side buttons only today |
