# WlSnow

A native Wayland falling-snow renderer. It draws an animated field of falling
snowflakes as a `wlr-layer-shell` *bottom* layer — above the desktop wallpaper
and behind your windows — and never intercepts pointer or keyboard input. It
renders on every connected output. It is a from-scratch Wayland equivalent of the
old X11 `XSnow`.

## Requirements

- A Wayland compositor that supports `wlr-layer-shell`: wlroots-based
  compositors (Sway, Hyprland, Wayfire, river) or KDE Plasma `kwin_wayland`.
  GNOME does not support `wlr-layer-shell` and is not supported.
- A normal Wayland session: `$WAYLAND_DISPLAY` and `$XDG_RUNTIME_DIR` set.

## Building

```bash
cargo build --release
```

The binary is written to `target/release/wlsnow`.

## Running

```bash
cargo run --release            # defaults
cargo run --release -- --count 500 --speed 120.0
./target/release/wlsnow --size 5.0 --drift 20.0
```

The app runs in the foreground and renders to the bottom layer (behind windows,
over the wallpaper) on all connected outputs. Stop it with Ctrl-C, or via the
control socket's `quit` command (see below).

### CLI flags

| Flag       | Type  | Default | Meaning                              |
| ---------- | ----- | ------- | ------------------------------------ |
| `--count`  | int   | `200`   | Number of snowflakes                 |
| `--speed`  | float | `60.0`  | Fall speed, in pixels per second     |
| `--size`   | float | `3.0`   | Flake diameter, in pixels            |
| `--drift`  | float | `10.0`  | Horizontal drift, in pixels per second |

## Control protocol

While running, `wlsnow` serves a line-delimited JSON control protocol over a
Unix socket at `$XDG_RUNTIME_DIR/wlsnow.sock` (falling back to
`/tmp/wlsnow-<pid>.sock` if `$XDG_RUNTIME_DIR` is unset).

Each request is a single JSON object terminated by a newline; each response is a
single JSON object terminated by a newline. Every request must carry the
protocol version field `"v": 1`; requests with any other version are rejected
with an error.

`set_config` currently accepts `flake_count`, `speed`, and `size` (not `drift`).
A `set_config` or a changed output size rebuilds the simulation from scratch.

### Examples

Query the current configuration:

```bash
SOCK="$XDG_RUNTIME_DIR/wlsnow.sock"
printf '{"v":1,"cmd":"get_config"}\n' | socat - UNIX-CONNECT:"$SOCK"
# {"drift":10.0,"flake_count":200,"ok":true,"paused":false,"size":3.0,"speed":60.0,"v":1}
```

Change the snowflake count and fall speed:

```bash
printf '{"v":1,"cmd":"set_config","flake_count":500,"speed":120.0}\n' | socat - UNIX-CONNECT:"$SOCK"
# {"ok":true,"v":1}
```

Pause and resume the animation:

```bash
printf '{"v":1,"cmd":"pause"}\n'  | socat - UNIX-CONNECT:"$SOCK"   # {"ok":true,"v":1}
printf '{"v":1,"cmd":"resume"}\n' | socat - UNIX-CONNECT:"$SOCK"   # {"ok":true,"v":1}
```

Exit the app:

```bash
printf '{"v":1,"cmd":"quit"}\n' | socat - UNIX-CONNECT:"$SOCK"     # {"ok":true,"v":1}
```

### Commands

| Command      | Payload                      | Response                                            |
| ------------ | ---------------------------- | --------------------------------------------------- |
| `hello`      | —                            | `{"ok":true,"protocol":1,"v":1}`                    |
| `get_config` | —                            | `{"ok":true,"v":1,"flake_count":N,"speed":F,"size":F,"drift":F,"paused":bool}` |
| `set_config` | optional `flake_count`,`speed`,`size` | `{"ok":true,"v":1}`                       |
| `pause`      | —                            | `{"ok":true,"v":1}`                                 |
| `resume`     | —                            | `{"ok":true,"v":1}`                                 |
| `quit`       | —                            | `{"ok":true,"v":1}`, then the app exits             |

On any error (malformed JSON, unknown command, wrong protocol version) the
server responds with `{"v":1,"ok":false,"error":"<reason>"}` and continues
serving.

