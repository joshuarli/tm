# tm — Agent Guide

Minimal terminal multiplexer in Rust. ~10K lines, 3 deps (libc, anyhow, mio).

## Build & Test

```
cargo build                      # debug build
cargo test --bin tm              # 157 unit tests
cargo bench                      # 34 criterion benchmarks
make release-pgo                 # PGO-optimized release build
make bench-pgo                   # compare regular vs PGO (needs critcmp)
```

Quiet mode is configured in `.cargo/config.toml`.

## Architecture

### Server-Client Model

tm uses a **server-client architecture identical to tmux**. One long-lived server process owns all sessions, windows, panes, and PTYs. Thin client processes connect, pass their tty fd, and forward raw input.

**Critical**: The server writes escape sequences directly to the client's tty fd (received via `SCM_RIGHTS`). The server must NOT call `setsid()` — doing so detaches from the controlling terminal, causing `EIO` on all tty writes.

### Three-Layer VT Parser

Inspired by tty.app's parser architecture. Three fast paths before the full state machine:

```
PTY data
  │
  ├─ Layer 1: SIMD ASCII scan
  │   NEON (aarch64) / SSE2 (x86_64) — 64 bytes/iter
  │   Finds printable ASCII run length at ~58 GB/s
  │   Entire run passed to put_ascii_run() as single slice
  │
  ├─ Layer 2: Inline controls + CSI fast path
  │   CR, LF, BS, TAB handled inline (no state machine)
  │   try_csi_fast() parses common CSI sequences directly:
  │     ESC[m (SGR reset), ESC[Nm (single SGR), ESC[N;NH (CUP),
  │     ESC[A-D (cursor), ESC[G (CHA), ESC[J/K (erase), ESC[r (DECSTBM)
  │   Falls back on: private modes (?), intermediates, colon sub-params
  │
  └─ Layer 3: Full state machine
      VT100/VT500 — enum state + match arms
      CSI, OSC, DCS, C0, DECSC/RC, alternate screen, mouse modes,
      SGR (256+RGB+styled underlines), cursor style, synchronized output
```

**Performance** (criterion, Apple Silicon):
- 4KB ASCII: 27µs (2.2x faster than naive per-byte parsing)
- 64KB ASCII: 439µs
- SGR colored output: 63µs for 500 sequences
- Full screen redraw (cursor+SGR): 33µs for 80×24

**Optimizations**:
- `put_ascii_run()` batch-writes cells: one line lookup, pre-computed attrs, tight inner loop
- Skip-if-unchanged: cells compared before writing — identical cells not marked dirty (from tmux)
- Zero allocations in steady-state event loop: all scratch Vecs hoisted and reused

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

### Grid & Dirty Tracking

Two-tier cell encoding: CompactCell (5 bytes, ASCII + 256-color) and ExtendedCell (~20 bytes, Unicode + RGB). Cells compared before writing — unchanged cells skip the dirty flag, reducing render work for programs that redraw the same screen (htop, vim status bars).

Grid reflow on resize: consecutive WRAPPED lines are joined into logical lines, then re-split at the new width (2.5x faster than naive via reusable buffers).

Scroll recycling: when history overflows, evicted GridLines are recycled via `clear_to()` instead of allocating new ones.

### Pitfalls learned the hard way

- **kqueue rejects `/dev/tty`**: On macOS, `open("/dev/tty")` returns a device fd that kqueue refuses to monitor (`EINVAL`). The client uses `STDIN_FILENO` (fd 0) for mio polling instead.

- **Accepted sockets inherit non-blocking**: On macOS, reused fd numbers may be non-blocking (inherited from the listener). `register_new_connection` calls `set_blocking()` before `recv_fd()`.

- **EINTR everywhere**: Signals (especially `SIGCHLD`) interrupt `poll()`, `recvmsg()`, `sendmsg()`. All have retry loops.

- **`setsid()` kills tty access**: Server must keep controlling terminal. Only redirect stdio to `/dev/null`.

- **Enter is 0x0D, not Ctrl-M**: `0x09` (Tab), `0x0A` (LF), `0x0D` (CR) must match before the `0x01..=0x1A` Ctrl-letter range.

- **New panes must register with mio**: `split_pane` and `create_new_window` return `InputResult::NewPane(pid)` so the server registers the pty master fd.

## Module Map

| File | Lines | Role |
|------|-------|------|
| `key_bind.rs` | ~2230 | Prefix key, binding table, action dispatch, copy mode, selection |
| `vt.rs` | ~1600 | Three-layer VT parser: SIMD → CSI fast → state machine |
| `grid.rs` | ~1005 | Ring buffer grid, two-tier cells, reflow, dirty tracking |
| `server.rs` | ~1002 | Event loop, accept connections, signal handling, render dispatch |
| `screen.rs` | ~858 | Cursor, scroll regions, erase, put_ascii_run, pending wrap |
| `keys.rs` | ~582 | Input parsing: escape sequences, SGR mouse, bracketed paste |
| `render.rs` | ~516 | Dirty-cell rendering, borders, status bar, selection highlight |
| `tty.rs` | ~471 | Buffered escape sequence output, zero-alloc SGR via io::Write |
| `state.rs` | ~459 | Central State struct, entity types, ID newtypes, CRUD |
| `client.rs` | ~356 | Connect, raw mode, forward input, SIGWINCH |
| `protocol.rs` | ~341 | Wire format, SCM_RIGHTS fd passing, message encode/decode |
| `config.rs` | ~328 | Parse `~/.config/tm/tm.conf`, set/bind directives |
| `layout.rs` | ~248 | Binary split tree, calculate geometry, zoom |
| `simd.rs` | ~242 | NEON (aarch64) + SSE2 (x86_64) ASCII scanner |
| `sys.rs` | ~222 | Signal pipes, fcntl, winsize, pipe_cloexec, platform glue |
| `main.rs` | ~114 | CLI parsing, fork+socketpair, start_or_connect |
| `pty.rs` | ~103 | forkpty, spawn login shell, set TERM/TM env |
| `log.rs` | ~38 | File logger (when `TM_LOG=1`) |

## Platform Support

| Component | macOS aarch64 | Linux aarch64 | Linux x86_64 |
|-----------|:---:|:---:|:---:|
| SIMD scanner | NEON | NEON | SSE2 |
| Event loop | kqueue | epoll | epoll |
| pipe_cloexec | pipe+fcntl | pipe2 | pipe2 |
| Everything else | POSIX | POSIX | POSIX |

## Testing

157 unit tests in `#[cfg(test)] mod tests` blocks. 34 criterion benchmarks covering hot and cold paths.

## What's Not Done Yet

- **Scroll coalescing**: Accumulate wheel deltas over 16ms timer
- **Integration tests**: No PTY-based end-to-end tests yet
