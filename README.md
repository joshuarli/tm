# tm

A fast, minimal terminal multiplexer. Three runtime dependencies (libc, mio, anyhow). One binary. No config required.

## Features

### Tmux parity

- **Client/server architecture** — detach with `prefix+d`, reattach with `tm attach`. Sessions persist in the background.
- **Windows and panes** — split horizontally (`prefix+\`) or vertically (`prefix+-`). Navigate panes directionally. Zoom a pane to full screen with `prefix+z`.
- **Mouse support** — click to focus panes, click status bar to switch windows. Mouse events forwarded to apps that request them (button, motion, SGR reporting).
- **Scrollback history** — 10,000 lines per pane (configurable). Ring buffer, compact cell storage.
- **Bracketed paste** — paste markers forwarded to apps that request them.
- **Focus events** — forwarded to apps that request them.
- **Alternate screen** — apps like vim/less get their own screen buffer.
- **True color** — 24-bit RGB, 256-color palette, styled underlines (curly, double, dotted, dashed).
- **Unicode** — full UTF-8 with CJK wide character support.
- **Configurable** — `~/.config/tm/tm.conf` for prefix key, colors, bindings, history limit, and more. `prefix+Enter` to reload.
- **Status bar** — session name, window list with indices, zoom indicator. Top or bottom positioning. Clickable.
- **Window management** — create (`prefix+n`), rename (`prefix+r`), reorder (`prefix+arrows`), select by number (`prefix+1-9`).
- **Pane operations** — kill (`prefix+k`), move to another window (`prefix+m`), break to new window (`prefix+ctrl+n`).

### Non-features

These are intentional omissions, not a TODO list.

- **No scripting engine** — no tmux command language, no run-shell. Configure with a plain config file.
- **No vi/emacs copy mode navigation** — scroll with the mouse wheel. Select with click-drag. That's it.
- **No status bar templating** — fixed format. No strftime, no shell interpolation, no conditional sections.
- **No plugins or hooks** — the multiplexer does multiplexing.
- **No popup windows or menus** — use your shell.
- **No session grouping or linking** — one session, one view.
- **No logging or pipe-pane** — use `script` or redirect in your shell.

### Innovations

- **Per-pane copy mode** — multiple panes can be independently scrolled back and frozen. The display stays locked at the scroll position even as the underlying app continues producing output. Panes in copy mode get a yellow border overlay. This is less "copy mode" and more "freeze/reading mode" — scroll back to read something, click to another pane to keep working, and your reading position is preserved.
- **Extended keys by default** — requests `modifyOtherKeys` level 2 from the outer terminal on startup. Apps inside tm that understand CSI u encoding get unambiguous modifier information. No configuration needed.
- **Scroll coalescing** — mouse wheel deltas accumulate over the 16ms render tick, so rapid scrolling produces one reposition instead of many.
- **Compact cell storage** — common ASCII cells use 5 bytes. Extended cells (UTF-8, RGB, underline color) expand only when needed. Scrollback for a typical terminal session uses a fraction of the memory compared to other multiplexers.
- **Scroll optimization** — when a full-width pane scrolls, tm emits CSI S (hardware scroll) and only repaints the new lines instead of the entire viewport.
- **Three dependencies** — libc for syscalls, mio for the event loop, anyhow for errors. That's the full dependency tree. The VT parser, grid, renderer, protocol, and layout engine are all from scratch.
- **Fast VT parsing** — SIMD-accelerated ASCII scanning. Common escape sequences have a dedicated fast path before the general state machine.
- **Synchronized output** — supports DEC mode 2026 so apps can batch their drawing and avoid flicker.

## Usage

```
tm new [name]       # create a new session (default name: "main")
tm attach [name]    # attach to an existing session
tm a [name]         # alias for attach
tm ls               # list sessions
tm kill [name]      # kill a session
```

Running `tm` with no arguments attaches to an existing session or creates one.

## Default key bindings

All bindings require the prefix key first (default: `Ctrl+A`).

| Key | Action |
|---|---|
| `d` | Detach |
| `n` | New window |
| `r` | Rename window |
| `1`-`9` | Select window |
| `Ctrl+Left/Right` | Previous/next window |
| `Left/Right` | Swap window left/right |
| `Up/Down` | Focus pane up/down |
| `Shift+Left/Right` | Focus pane left/right |
| `\` | Split horizontally |
| `-` | Split vertically |
| `z` | Zoom pane |
| `k` | Kill pane |
| `m` | Move pane to window |
| `Ctrl+N` | Break pane to new window |
| `/` | Command prompt |
| `Ctrl+S` | Send prefix to app |
| `Enter` | Reload config |

## Mouse

- **Click pane** — focus it
- **Click status bar** — switch window
- **Scroll wheel** — enter copy mode and scroll back (or forward to app if it captures mouse)
- **Click-drag** — select text, copies to clipboard on release via OSC 52

## Building

```
cargo build --release
```

Requires Rust 2024 edition. The release binary is ~500KB with LTO and strip enabled.
