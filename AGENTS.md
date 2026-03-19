# tm — Agent Guide

Minimal terminal multiplexer in Rust. 3 deps (libc, anyhow, mio).

## Build & Test

```
cargo build                      # debug build
cargo test --bin tm              # unit tests
cargo bench                      # criterion benchmarks
make release-pgo                 # PGO-optimized release build
make bench-pgo                   # compare regular vs PGO (needs critcmp)
make bump-version                # increment patch, tag release/x.y.z
```

## Architecture

### Server-Client Model

tm uses a **server-client architecture identical to tmux**. One long-lived server process owns all sessions, windows, panes, and PTYs. Thin client processes connect, pass their tty fd, and forward raw input.

**Critical**: The server writes escape sequences directly to the client's tty fd (received via `SCM_RIGHTS`). The server must NOT call `setsid()` — doing so detaches from the controlling terminal, causing `EIO` on all tty writes.

### Three-Layer VT Parser

Inspired by tty.app's parser architecture:

```
PTY data
  ├─ Layer 1: SIMD ASCII scan (NEON / SSE2, 64 bytes/iter, ~58 GB/s)
  │   Entire run passed to put_ascii_run() as single slice
  ├─ Layer 2: Inline controls + CSI fast path
  │   CR, LF, BS, TAB inline; common CSI parsed without state machine
  └─ Layer 3: Full VT100/VT500 state machine
```

**Optimizations**: batch cell writes via `put_ascii_run()`, skip-if-unchanged (from tmux), zero allocations in steady-state event loop, scroll line recycling, reusable reflow buffers.

### Connection Flow

**First client (`tm new`)** — socketpair eliminates startup race:
1. `socketpair()` → child gets one end, parent gets the other
2. Child becomes server, parent becomes client — already connected

**Subsequent clients** — connect to the Unix socket at `$TMPDIR/tm-$UID/default`.

### Grid & Dirty Tracking

Two-tier cells: CompactCell (5 bytes) and ExtendedCell (~20 bytes). Cells compared before writing — unchanged cells skip the dirty flag. Grid reflow joins WRAPPED lines and re-splits at new width.

### Copy Mode & Scroll Coalescing

Mouse wheel enters copy mode with `oy` offset into scrollback. Wheel deltas are coalesced over 16ms — multiple rapid wheel events accumulate into a single scroll + render, avoiding per-tick full redraws.

### Pitfalls

- **kqueue rejects `/dev/tty`**: Client uses `STDIN_FILENO` for mio polling
- **Accepted sockets inherit non-blocking**: `set_blocking()` before `recv_fd()`
- **EINTR everywhere**: All syscalls have retry loops
- **`setsid()` kills tty access**: Only redirect stdio to `/dev/null`
- **Enter is 0x0D, not Ctrl-M**: Match before the Ctrl-letter range
- **New panes must register with mio**: Return `InputResult::NewPane(pid)`

## Module Map

| File | Role |
|------|------|
| `key_bind.rs` | Prefix key, binding table, action dispatch, copy mode, selection |
| `vt.rs` | Three-layer VT parser: SIMD → CSI fast → state machine |
| `grid.rs` | Ring buffer grid, two-tier cells, reflow, dirty tracking |
| `server.rs` | Event loop, accept connections, signal handling, render dispatch |
| `screen.rs` | Cursor, scroll regions, erase, put_ascii_run, pending wrap |
| `keys.rs` | Input parsing: escape sequences, SGR mouse, bracketed paste |
| `render.rs` | Dirty-cell rendering, borders, status bar, selection highlight |
| `tty.rs` | Buffered escape sequence output, zero-alloc SGR via io::Write |
| `state.rs` | Central State struct, entity types, ID newtypes, CRUD |
| `client.rs` | Connect, raw mode, forward input, SIGWINCH |
| `protocol.rs` | Wire format, SCM_RIGHTS fd passing, message encode/decode |
| `config.rs` | Parse `~/.config/tm/tm.conf`, set/bind directives |
| `layout.rs` | Binary split tree, calculate geometry, zoom |
| `simd.rs` | NEON (aarch64) + SSE2 (x86_64) ASCII scanner |
| `sys.rs` | Signal pipes, fcntl, winsize, pipe_cloexec, platform glue |
| `main.rs` | CLI parsing, fork+socketpair, start_or_connect |
| `pty.rs` | forkpty, spawn login shell, set TERM/TM env |
| `log.rs` | File logger (when `TM_LOG=1`) |

## Platform Support

| | macOS aarch64 | Linux aarch64 | Linux x86_64 |
|--|:---:|:---:|:---:|
| SIMD | NEON | NEON | SSE2 |
| Event loop | kqueue | epoll | epoll |
| pipe | pipe+fcntl | pipe2 | pipe2 |

## What's Not Done Yet

- Integration tests (PTY-based end-to-end)
