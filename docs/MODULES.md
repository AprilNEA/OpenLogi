# Key modules

A key module is a separate program that owns keys on an MX Creative Keypad.
It draws each key's face and handles its presses: a now-playing module shows
the album art and toggles playback, a weather module shows the temperature.
Modules live in their own repositories, under any license, and OpenLogi
contains no code for any of them. A user without Spotify never runs the
Spotify module.

To use one, put its folder in the modules folder, turn it on in Settings, and
bind it to any keypad key, on any page or in a per-app profile, like any other
action.

## Installing

The modules folder sits beside `config.toml`:

| Platform | Folder |
| --- | --- |
| macOS and Linux | `~/.config/openlogi/modules/` (`$XDG_CONFIG_HOME/openlogi/modules/`) |
| Windows | `%USERPROFILE%\.config\openlogi\modules\` |

Local macOS development builds use `openlogi-dev` in place of `openlogi`.
Each module is one folder in it holding a `module.toml`. Folder names are
yours to choose; folders whose names start with `.` are skipped.

A module runs only while it is turned on. The Modules section of Settings has
a checkbox per module, which writes its table in `config.toml`:

```toml
[modules."com.example.weather"]
enabled = true

[modules."com.example.weather".settings]
city = "Lynchburg"
units = "imperial"
```

`enabled` defaults to `false`. `settings` is free-form and handed to the
module unchanged; Settings has no editor for it, so edit it in `config.toml`.
Changing a module's `settings` restarts it.

Bind a key to the module in the binding editor, or in `config.toml`:

```toml
[devices."unit:0f1e2d3c".bindings]
gridMiddle = { Module = "com.example.weather" }
"page2:gridTopLeft" = { Module = "com.example.spotify" }
```

`openlogi modules list` prints every folder and where its module stands:
running, starting, restarting, turned off, failed, invalid, or not for this
platform, with the last error.

## The manifest

```toml
id = "com.example.spotify"        # required, reverse-DNS: a-z, 0-9, ".", "-"
name = "Spotify Now Playing"      # required, shown in the app
description = "Album art and play/pause for Spotify."
version = "1.0.0"                 # the module's own version, shown only
protocol = 1                      # required; the protocol it speaks
platforms = ["macos"]             # required; any of "macos", "linux", "windows"
executable = "spotify-module"     # required; a path inside the module folder
```

- `id` names the module everywhere: in bindings, in `[modules."<id>"]`, and in
  the protocol. It has at least two dot-separated parts, each non-empty and
  neither starting nor ending with `-`, and is at most 128 characters. Two
  folders with the same id are refused after the first.
- `protocol` must be one the host speaks; this OpenLogi speaks `1`.
- `executable` is relative to the module folder, never absolute and never
  with `..`. On Windows name the file with its extension
  (`spotify-module.exe`). A script works where the platform runs it directly,
  through its `#!` line on macOS and Linux.
- Unknown keys are refused. A folder whose manifest is missing or invalid is
  listed with its error and never run.

## Launch

The agent runs the executable with no arguments, in the module folder, with
the agent's environment plus:

| Variable | Value |
| --- | --- |
| `OPENLOGI_MODULE_ID` | the module's `id` |
| `OPENLOGI_MODULE_DIR` | the module folder |

The module runs as the user; on macOS with OpenLogi's permissions (see
Security). It talks to the agent over its stdin and stdout; stderr goes to
the agent's log at debug level.

## Protocol 1

Newline-delimited JSON, UTF-8: one object per line, each with a `type`.

Host to module, on stdin:

| `type` | Fields | Meaning |
| --- | --- | --- |
| `hello` | `protocol: 1`, `host: "openlogi"`, `host_version`, `module_id`, `settings` (object, `{}` when none) | The first message, once. |
| `keys` | `keys: [{ "device", "slot", "size" }]` | The full set of keys bound to this module now: on every connected keypad, the current page's keys, for the app in front. Sent after `ready` and whenever the set changes: a rebind, a page turn, an app switch, a keypad connecting or leaving. Empty when there are none. |
| `pressed` | `device`, `slot` | A key bound to the module went down. |
| `shutdown` | | The host is stopping the module: exit within 2 seconds or be killed. |

`device` names one keypad: `keypad-1`, `keypad-2`, and so on, numbered in
the order the agent first hands out that keypad's keys and kept for as long
as the agent runs. It is never the keypad's serial or its `config.toml` key.
`slot` names a key the way `config.toml` does: `gridTopLeft` through
`gridBottomRight` on page 1, and `page2:gridMiddle` and so on for later
pages. `size` is the key's side in pixels. Read it per key.

Module to host, on stdout:

| `type` | Fields | Meaning |
| --- | --- | --- |
| `ready` | `protocol: 1` | The reply to `hello`. The host sends nothing more until it arrives, and kills a module that has not sent it within 10 seconds. |
| `image` | `device`, `slot`, and `png` or `jpeg` | Show this image on that key. Ignored for a key the module does not own now. |
| `image_all` | `png` or `jpeg` | The same image on every key the module owns now. |
| `clear` | `device`, `slot` | Show the module glyph on that key again. Ignored for a key the module does not own now. |
| `log` | `level` (`debug`, `info`, `warn`, `error`), `message` | A line in the agent's log, under the module's id. |

`png` and `jpeg` hold the image base64-encoded (standard alphabet, with
padding). Either field takes either format. The image is cropped to a
centred square and scaled to the key; transparency shows black. Until a
module draws a key, the key shows a generic module glyph.

`image_all` covers the keys owned when it arrives. A key shows the module's
image only while the module owns it. A key that becomes the module's again
shows the last image the module sent for it, and one it never drew shows
the glyph, until the module sends again, so send each key's image, or
`clear`, again after every `keys` message.

Rules:

- `log` may come before `ready`; any other message before `ready` is
  logged and dropped.
- A message with an unknown `type` is logged once, within the rate limit
  below, and ignored, so a module written for a later protocol still runs.
  Unknown fields are ignored.
- A line that is not valid JSON, or longer than 8 MiB, is logged and dropped.
- An image is at most 4 MiB once decoded, and at most 4096 pixels on a side.
- Module output to the log, from stderr and `log` alike, is rate-limited.
- Read stdin promptly, and keep it open. A module that stops reading, or
  closes stdin and keeps running, is killed.
- Exit when stdin closes: the agent has gone.

## Lifecycle

- The agent starts every installed module that is turned on and lists this
  platform, once it is running. It scans the modules folder at start, on
  every change to `config.toml`, and every 10 seconds, so a folder dropped in
  is found without a restart.
- A module that exits, crashes, or is killed is started again after 1 second,
  then 2, 4, and so on up to 60 seconds between tries. The delay starts over
  once a run has stayed up for 60 seconds after `ready`.
- A module that fails to send `ready` 5 times in a row is marked failed and
  stays stopped until it is turned off and on again or its folder changes
  (its manifest or its executable).
- A change to the module's manifest or its executable restarts it, and so
  does a change to its `settings` table.
- Turning a module off, removing its folder, quitting the agent, or the
  agent replacing itself after an update sends it `shutdown`; one that is
  still running 2 seconds later is killed.
- On macOS and Linux a module leads its own process group, and the end of
  every run takes the whole group, whether the module was killed or left on
  its own: a wrapper script's children go with it, even when they hold its
  output open. On Windows only the module's own process is ended: a wrapper
  there must end its children itself.
- Since a module runs in its own process group on macOS and Linux, stdin
  closing is the only notice it gets if the agent crashes, and a module
  that ignores end of input outlives the agent.
- When a module stops, every image it sent goes with it: its keys show the
  module glyph until it runs and draws them again.

## Security

A module is a program you installed, and it runs as you. OpenLogi never
downloads modules and runs only the ones turned on from the modules folder.

On macOS a module runs as part of OpenLogi: macOS charges what it does to
OpenLogi, so it shares the agent's Input Monitoring and Accessibility
permissions, and an Automation consent given to OpenLogi (to control a music
app, say) covers every module. Install modules only from sources you trust.

## A minimal module

A module that draws nothing and logs its keys and presses, in Python:

```python
#!/usr/bin/env python3
import json, sys

def send(message):
    print(json.dumps(message), flush=True)

for line in sys.stdin:
    message = json.loads(line)
    if message["type"] == "hello":
        send({"type": "ready", "protocol": 1})
    elif message["type"] == "keys":
        send({"type": "log", "level": "info", "message": f"I own {len(message['keys'])} keys"})
    elif message["type"] == "pressed":
        send({"type": "log", "level": "info", "message": f"pressed {message['slot']}"})
    elif message["type"] == "shutdown":
        break
```

Save it as `module.py` beside this `module.toml`, and make it executable:

```toml
id = "com.example.hello"
name = "Hello"
protocol = 1
platforms = ["macos", "linux"]
executable = "module.py"
```

To draw, answer each `keys` message with an `image` per key, or one
`image_all`, carrying a base64 PNG or JPEG.
