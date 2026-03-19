# tm — Agent Guide

Minimal terminal multiplexer in Rust. ~8K lines, 3 deps (libc, anyhow, mio).

## Build & Test

```
cargo build          # debug build
cargo test           # 61 unit tests
cargo clippy         # lint (dead_code warnings expected — phased implementation)
```

Quiet mode is configured in `.cargo/config.toml`. No pre-commit hooks yet.

## Architecture

### Server-Client Model

tm uses a **server-client architecture identical to tmux**. One long-lived server process owns all sessions, windows, panes, and PTYs. Thin client processes connect, pass their tty fd, and forward raw input.

**Critical**: The server writes escape sequences directly to the client's tty fd (received via `SCM_RIGHTS`). The server must NOT call `setsid()` — doing so detaches from the controlling terminal, causing `EIO` on all tty writes.

### Connection Flow

**First client (`tm new`)** — uses a socketpair to avoid startup races:
1. Parent creates `socketpair(AF_UNIX, SOCK_STREAM)`
2. `fork()` — child gets `pair[1]`, parent gets `pair[0]`
3. Child redirects stdio to `/dev/null`, calls `run_server_with_client(pair[1])`
4. Server registers `pair[1]` as the first client connection (no accept needed)
5. Parent calls `run_client_on_fd(pair[0])` — already connected, no race

**Subsequent clients (`tm attach`, `tm ls`)** — connect to the Unix socket:
1. Client connects to `$TMPDIR/tm-$UID/default`
2. For interactive commands: client sends tty fd via `SCM_RIGHTS`, then `MSG_IDENTIFY`
3. For non-interactive (`ls`, `kill`): client sends a plain byte (no fd), then message
4. Server calls `recv_fd()` which returns `Option<RawFd>` — `Some` or `None`

### Pitfalls learned the hard way

- **kqueue rejects `/dev/tty`**: On macOS, `open("/dev/tty")` returns a device fd that kqueue refuses to monitor (`EINVAL`). The client uses `STDIN_FILENO` (fd 0) for mio polling instead — it's the same tty but as a pty slave that kqueue accepts. The separately-opened `/dev/tty` fd is only sent to the server for rendering.

- **Accepted sockets inherit non-blocking**: After closing a client's fd and accepting a new connection, the reused fd number may be non-blocking (inherited from the listener on macOS). `register_new_connection` calls `set_blocking()` before `recv_fd()`.

- **EINTR everywhere**: Signals (especially `SIGCHLD` from pane shells) interrupt `poll()`, `recvmsg()`, `sendmsg()`. All three have retry loops. `poll()` uses `match` with `Interrupted => continue`.

- **`setsid()` kills tty access**: The server must keep the controlling terminal association. Only redirect stdio to `/dev/null`.

- **Enter is 0x0D, not Ctrl-M**: In the input parser, `0x09` (Tab), `0x0A` (LF), and `0x0D` (CR) must be matched before the `0x01..=0x1A` Ctrl-letter range, or they become `Ctrl-I`, `Ctrl-J`, `Ctrl-M`.

- **New panes must register with mio**: `split_pane` and `create_new_window` spawn PTYs but the server event loop only reads fds registered with mio. These return `InputResult::NewPane(pid)` so the server can register the pty master fd.

## Module Map

| File | Lines | Role |
|------|-------|------|
| `server.rs` | ~990 | Event loop, accept connections, signal handling, render dispatch |
| `key_bind.rs` | ~1094 | Prefix key, binding table, action dispatch, split/window/pane ops |
| `vt.rs` | ~1300 | VT100 parser: CSI, OSC, SGR (256+RGB), alt screen, mouse modes |
| `keys.rs` | ~575 | Input parsing: escape sequences, SGR mouse, bracketed paste |
| `grid.rs` | ~570 | Ring buffer grid, two-tier cells (compact 5B + extended ~20B) |
| `screen.rs` | ~566 | Cursor, scroll regions, erase, insert/delete, pending wrap |
| `state.rs` | ~429 | Central State struct, entity types, ID newtypes, CRUD |
| `render.rs` | ~386 | Dirty-cell rendering, borders, status bar, prompt overlay |
| `client.rs` | ~356 | Connect, raw mode, forward input, SIGWINCH |
| `protocol.rs` | ~341 | Wire format, SCM_RIGHTS fd passing, message encode/decode |
| `config.rs` | ~328 | Parse `~/.config/tm/tm.conf`, set/bind directives |
| `tty.rs` | ~317 | Buffered escape sequence output, SGR colors, cursor control |
| `layout.rs` | ~248 | Binary split tree, calculate geometry, zoom |
| `sys.rs` | ~222 | Signal pipes, fcntl, winsize, pipe_cloexec, platform glue |
| `main.rs` | ~113 | CLI parsing, fork+socketpair, start_or_connect |
| `pty.rs` | ~103 | forkpty, spawn login shell, set TERM/TM env |
| `log.rs` | ~48 | File logger (when `TM_LOG=1`) |

## Key Data Flow

```
Terminal Input → client stdin → MSG_INPUT → server → parse_input() → process_input()
                                                                          ↓
                                                              key_bind dispatch
                                                                    ↓
                                                      InputResult::PtyWrite(pid, bytes)
                                                                    ↓
                                                        write(pane.pty_master, bytes)
                                                                    ↓
                                                      shell processes input, writes output
                                                                    ↓
PTY output → mio READABLE → handle_pane_data() → vt::process_pane_output() → Screen/Grid
                                                                                    ↓
                                                                        render_client() → tty.flush_to(client.tty_fd)
                                                                                                    ↓
                                                                                            Terminal Output
```

## Entity Relationships

```
Session ──has many──→ Window (ordered, 1-indexed)
Window  ──has many──→ Pane (via layout tree + panes vec)
Window  ──has one───→ LayoutNode (binary split tree)
Client  ──attached──→ Session (one client per session)
Pane    ──owns──────→ Screen (+ alt_screen), VtParser, PTY master fd
```

All cross-references are ID newtypes (`SessionId`, `WindowId`, `PaneId`, `ClientId`). Lookup at point of use from `State`'s HashMaps.

## Testing

Tests live in `#[cfg(test)] mod tests` blocks within each module. Key coverage:

- **protocol**: Message encode/decode roundtrips, fd passing via socketpair, cmsg alignment
- **keys**: Input parsing (Enter, Escape, Tab, Ctrl, CSI sequences, SGR mouse)
- **grid**: Scroll, history limits, compact/extended cells, resize
- **screen**: Cursor movement, line wrapping, scroll regions, erase
- **vt**: Full parser tests (cursor, SGR colors, OSC title/cwd, alt screen, DECSC/RC)
- **layout**: Split tree geometry, remove+simplify, pane counting
- **config**: Parse set/bind directives, key name resolution
- **sys**: pipe_cloexec flags, nonblock, signal pipe delivery
- **state**: Session/window/pane creation, renumbering, client lookup

## What's Not Done Yet

- **Copy mode**: Scaffolded but not functional (mouse scroll, selection, OSC 52 clipboard)
- **Grid reflow on resize**: Lines don't rewrap using WRAPPED flags
- **Pane focus with mouse click**: Partially implemented
- **Integration tests**: No PTY-based end-to-end tests yet
