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

## Rendering Pipeline

```
Shell → PTY → tm server (16ms tick) → escape sequences → terminal emulator (vsync) → display
```

Two independent render clocks exist: tm's 16ms poll tick (~60fps) and the terminal emulator's display refresh. Key design considerations:

**Synchronized output (Mode 2026)** prevents tearing. tm wraps each render in `\x1b[?2026h`...`\x1b[?2026l`. The terminal accumulates all changes during the BSU/ESU window and renders them atomically on the next frame. This makes the exact timing of tm's render irrelevant — the terminal handles frame pacing.

**The 16ms tick** is a coalesce window, not a frame rate. When panes produce output, tm waits up to 16ms for more data before rendering. This batches rapid output (like `yes` or `cat largefile`) into single frames. The tick is implemented as `mio::Poll::poll()` timeout — when idle, it's a single syscall sleeping in the kernel with negligible CPU cost.

**Why 16ms, not faster?** tm outputs escape sequences, not pixels — it sits inside a terminal emulator that handles final frame pacing. 60fps is more than sufficient for text content (scrolling, cursor movement). A longer coalesce window is actually better for throughput — more pane output batched per frame means fewer escape sequence writes to the tty pipe.

**Why not event-driven (render immediately on data)?** Without a coalesce window, programs producing unlimited output would flood the tty pipe with escape sequences faster than the terminal can consume them. The tick provides natural backpressure.

**Scroll coalescing** uses the same principle: wheel events accumulate `scroll_deferred` between ticks, flushed as a single offset change on the next render. This avoids per-wheel-tick full redraws during rapid scrolling.

## Single-Threaded Performance Model

Everything runs on one thread — no async, no locks, no channels. This works because the bottleneck is bytes written to the client tty, not CPU. Every optimization targets reducing tty output volume:

**Why single-threaded works**: A terminal multiplexer transforms PTY output into escape sequences written to the client tty. The CPU cost of parsing VT sequences and updating a grid is negligible compared to the I/O cost of writing the rendered output to the tty pipe. Threading would add synchronization overhead without meaningfully reducing the tty write volume — which is the actual bottleneck.

**Layer-by-layer optimizations**, from VT input to tty output:

| Layer | Optimization | Effect |
|---|---|---|
| VT parser | SIMD ASCII scan (~58 GB/s) | Skip per-byte dispatch for 95% of traffic |
| Grid cells | Two-tier encoding (CompactCell 5B / ExtendedCell ~20B) | Cache-friendly, low memory (3MB/10K history) |
| Grid cells | Skip-if-unchanged in `set_cell` | Avoid marking cells dirty when VT output matches current content |
| Grid scroll | Line recycling in `scroll_up` | Reuse oldest history line allocation instead of alloc+dealloc |
| Dirty tracking | Per-cell dirty flag + `SCROLL_DIRTY` flag | Distinguish scroll-shifted cells from content writes |
| Render | Terminal scroll commands (`CSI S`) | For full-width panes, emit 3-byte scroll command instead of repainting shifted rows |
| Render | Dirty-line skip | Skip entire rows with no dirty cells |
| TtyWriter | SGR attribute deduplication | Skip redundant color/attribute sequences for consecutive same-styled cells |
| TtyWriter | Buffered single `write()` per client per frame | Minimize syscall overhead |
| Frame pacing | 16ms coalesce window | Batch rapid output into single render pass |

**Scroll optimization detail**: When a program outputs new lines (build output, `cat`, logs), content scrolls up line by line. Without optimization, every visible row is repainted (all cells dirty from the scroll). With optimization, `scroll_up` marks shifted rows with `SCROLL_DIRTY` (not `DIRTY`). The renderer detects `scroll_pending > 0`, emits `CSI S` to shift the terminal content, then only repaints rows with `DIRTY` cells (the new bottom lines). Individual VT writes between scrolls use `DIRTY`, so they're always rendered correctly.

**Measured byte reduction** (scroll optimization):

| Scenario | Full repaint | After 1 scroll | Reduction |
|---|---|---|---|
| 80×24 | 2,318 B | 247 B | 89% |
| 200×50 | 10,804 B | 487 B | 95% |

The optimization applies to full-width panes (zoomed, single-pane, horizontal splits). Side-by-side splits fall back to dirty-cell rendering because `CSI S` scrolls the full terminal width.

## Platform Support

| | macOS aarch64 | Linux aarch64 | Linux x86_64 |
|--|:---:|:---:|:---:|
| SIMD | NEON | NEON | SSE2 |
| Event loop | kqueue | epoll | epoll |
| pipe | pipe+fcntl | pipe2 | pipe2 |

## What's Not Done Yet

- Integration tests (PTY-based end-to-end)
