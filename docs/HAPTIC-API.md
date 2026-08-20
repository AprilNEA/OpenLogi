# The JSON haptics API

OpenLogi can expose device haptics to your own apps over a local socket, so a
script can make a supported mouse buzz. It is **off by default**.

Enable it in `~/.config/openlogi/config.toml` and restart the agent:

```toml
[app_settings]
haptic_api = true
```

The socket is `haptic.sock`, next to the agent's own socket in the runtime
directory (usually `~/.config/openlogi/`). On Windows it is the named pipe
`\\.\pipe\openlogi-haptic.sock`. There is no port and no network listener.

Access is whatever the platform gives the endpoint beside it: on Unix, the
runtime directory's permissions; on Windows, the default pipe DACL, which grants
the creating user **and administrators**. Both are fine for a single-user
desktop and neither isolates users on a shared machine.

Hardware support is narrow. HID++ `0x19b0` is a reverse-engineered feature, and
in practice this means a recent haptic-capable Logitech mouse such as the
MX Master 4. `devices` tells you what qualifies on your machine.

## Protocol

One JSON object per line, in each direction. Send a request, read a response.
If a request carries an `id`, the response carries the same one back; requests
on a connection are answered in order, so a client with only one call in flight
can leave `id` out entirely.

### `hello`

```json
-> {"cmd":"hello"}
<- {"ok":{"protocol":1,"agent":"0.7.1"}}
```

`protocol` moves only if an existing field changes meaning — new fields may
appear without a bump, so parse leniently.

### `devices`

Lists the online devices that have a haptic engine.

```json
-> {"cmd":"devices"}
<- {"ok":{"devices":[{"key":"receiver:aabbccdd:slot:1","name":"MX Master 4"}]}}
```

`key` is the stable per-device identifier — the same one `config.toml` uses.
`name` is for showing a person; don't match on it.

### `play`

```json
-> {"cmd":"play"}
-> {"cmd":"play","waveform":"damp","device":"receiver:aabbccdd:slot:1"}
<- {"ok":{"accepted":true}}
```

- `waveform`: `subtle` (default) — a light boundary tick — or `damp`, a firmer
  confirmation pulse. These are the only two waveforms confirmed on real
  hardware; an unknown value is an error rather than a silent default.
- `device`: a `key` from `devices`. Omit it for whichever device the agent
  currently considers active, which is what you want if you just mean "the
  mouse in front of me".

**`accepted` is not `played`.** The agent queues the waveform on the same
single-flight worker the Actions Ring uses. HID++ allows one in-flight
transaction per channel, shared with the input-capture path, so a client free
to queue buzzes faster than the receiver drains them would time out unrelated
device writes for seconds. If a second request arrives while one is mid-flight,
the older unplayed one is dropped — a late buzz is worse than no buzz.

Practically: don't build a waveform sequencer on this. It is for discrete
feedback — a build finished, a message arrived.

### Errors

```json
<- {"error":{"code":"device_not_found","message":"no such device"}}
```

`code` is the stable half; `message` is for humans and may change.

| code | meaning |
|---|---|
| `bad_request` | the line was not a request this version understands |
| `device_not_found` | no such device, or it is asleep — worth retrying later |
| `feature_unsupported` | that device has no haptic engine — never retry |
| `device_error` | something else went wrong talking to the device |

A malformed line is answered and the connection stays open, so you can debug
against it interactively.

## Examples

On Unix the endpoint is a filesystem socket; on Windows it is a named pipe. The
protocol is identical — only the address and the connect call differ.

### Unix

Shell, with `socat`:

```sh
echo '{"cmd":"play","waveform":"damp"}' | socat - UNIX-CONNECT:$HOME/.config/openlogi/haptic.sock
```

Python:

```python
import json, os, socket

with socket.socket(socket.AF_UNIX) as s:
    s.connect(os.path.expanduser("~/.config/openlogi/haptic.sock"))
    s.sendall(json.dumps({"cmd": "play", "waveform": "subtle"}).encode() + b"\n")
    print(json.loads(s.makefile().readline()))
```

### Windows

PowerShell — `NamedPipeClientStream` takes the pipe's name without the
`\\.\pipe\` prefix:

```powershell
$pipe = New-Object System.IO.Pipes.NamedPipeClientStream '.', 'openlogi-haptic.sock', 'InOut'
$pipe.Connect(2000)
$writer = New-Object System.IO.StreamWriter $pipe
$reader = New-Object System.IO.StreamReader $pipe
$writer.WriteLine('{"cmd":"play","waveform":"damp"}')
$writer.Flush()
$reader.ReadLine()
$pipe.Dispose()
```

### Node (either platform)

`net.createConnection` takes a pipe path on Windows and a socket path on Unix,
so one client covers both:

```js
import net from "node:net";
import os from "node:os";

const address =
  process.platform === "win32"
    ? String.raw`\\.\pipe\openlogi-haptic.sock`
    : `${os.homedir()}/.config/openlogi/haptic.sock`;

const sock = net.createConnection(address);
sock.write(JSON.stringify({ cmd: "play", waveform: "damp" }) + "\n");
sock.once("data", (buf) => {
  console.log(JSON.parse(buf.toString()));
  sock.end();
});
```

## Scope

This endpoint plays waveforms and lists what can be buzzed. It cannot change
DPI, pair devices, or read your configuration — it is deliberately much smaller
than the agent's own IPC socket sitting beside it, and is meant to stay that
way. Anything that needs the full agent contract should speak that protocol
instead (`crates/openlogi-ipc/examples/haptic.rs` is a starting point).
