# tm — Minimal Terminal Multiplexer

## Context

tmux is 91K lines of C with a full command language (Bison parser), 145 configuration options, a 5,900-line format interpolation engine, and decades of legacy terminal support. The user needs ~10% of that: sessions, windows, panes, mouse-driven copy mode, and prefix nesting. This project builds a focused replacement in Rust with 3 dependencies (libc, anyhow, mio).

## Stack

- **Language**: Rust, edition 2024
- **Dependencies**: `libc` (FFI), `anyhow` (error handling), `mio` (event loop: kqueue on macOS, epoll on Linux)
- **Platforms**: macOS + Linux
- **Build**: `cargo build`, no build.rs needed
- **TERM**: `tmux-256color` (reuses existing terminfo entry)
- **Terminal output**: Hardcoded ANSI/xterm escape sequences — no terminfo/ncurses dependency

## Architecture

### Server-Client Model
- Single Unix socket at `$XDG_RUNTIME_DIR/tm/default` or `$TMPDIR/tm-$UID/default`
- Client passes its tty fd to server via `SCM_RIGHTS`
- Server renders directly to client's tty (all rendering logic server-side)
- Client is a thin process: enters raw mode, forwards raw input bytes to server, handles SIGWINCH
- Single client per session — new attach detaches the old client
- IPC protocol: `[u32 length][u16 type][payload]` — no external serialization library
- One socket serves all sessions

### Event Loop
- Single-threaded, mio-based
- 16ms render tick (~60fps): collect all pane output, dirty cells, render once
- Always drain pane pty output (process doesn't block), discard intermediate frames
- Timers managed via mio poll timeout + deadline tracking

### Entity Graph (Handles, Not Pointers)
- Central `State` struct owns all entities in `HashMap<Id, T>`
- `SessionId`, `WindowId`, `PaneId`, `ClientId` are newtype `u32` wrappers (`Copy + Eq + Hash`)
- All cross-references are IDs, looked up at point of use
- No `Rc`, no `RefCell`, no lifetime parameters on entity types

```rust
struct State {
    sessions: HashMap<SessionId, Session>,
    windows: HashMap<WindowId, Window>,
    panes: HashMap<PaneId, Pane>,
    clients: HashMap<ClientId, Client>,
    // ID generators
    next_session: u32,
    next_window: u32,
    next_pane: u32,
    next_client: u32,
}
```

### Server Lifecycle
- Server starts when first `tm new` is run (no existing socket)
- Server exits when last session dies (all pane processes exited)
- SIGTERM: just exit — kernel closes PTY masters, sends SIGHUP to pane processes
- SIGCHLD: reap pane processes, cascade close (last pane → close window → last window → end session → detach client)
- SIGWINCH: resize client tty, recalculate layouts

## Data Structures

### Grid (Ring Buffer + Two-Tier Cells)

```rust
struct Grid {
    lines: VecDeque<GridLine>,  // ring buffer: visible + history
    sx: u32,                    // visible width
    sy: u32,                    // visible height
    hlimit: u32,                // max history lines (default 10000)
}

struct GridLine {
    compact: Vec<CompactCell>,    // 5 bytes each, one per used column
    extended: Vec<ExtendedCell>,  // overflow for non-ASCII/RGB cells
    flags: LineFlags,             // WRAPPED (soft line break), etc.
}

// 5 bytes — for ASCII chars with 256-color palette
#[derive(Clone, Copy, Default)]
struct CompactCell {
    ch: u8,     // ASCII byte, or 0xFF → look up in extended[]
    attr: u8,   // basic attributes (bold, dim, italic, underline, reverse, strike)
    fg: u8,     // palette color index (0-255)
    bg: u8,     // palette color index (0-255)
    flags: u8,  // EXTENDED flag + dirty bit + wide-char continuation
}

// ~20 bytes — for Unicode, RGB colors, styled underlines
#[derive(Clone, Copy, Default)]
struct ExtendedCell {
    ch: [u8; 8],   // UTF-8 bytes
    ch_len: u8,
    ch_width: u8,   // display width (1 or 2)
    attr: u16,      // full attributes including underline style
    fg: Color,      // Color enum: Default | Palette(u8) | Rgb(u8,u8,u8)
    bg: Color,
    us: Color,      // underline color
}

// When CompactCell.ch == 0xFF: attr/fg/bg are repurposed as u24 index into extended[]
```

**Memory**: ~3MB per pane at 10K history × 60 avg cols × 5 bytes.

**Dirty tracking**: Each `CompactCell` has a dirty bit in `flags`. On render, iterate only dirty cells. After render, clear dirty bits. Status bar has a separate dirty flag.

**Line flags**: `WRAPPED` flag distinguishes soft line breaks (for reflow) from hard line breaks (user pressed Enter).

### Screen

```rust
struct Screen {
    grid: Grid,
    cx: u32, cy: u32,          // cursor position
    rupper: u32, rlower: u32,  // scroll region
    mode: ScreenMode,          // bitflags: CURSOR_VISIBLE, INSERT, WRAP, ORIGIN, etc.
    saved_cx: u32, saved_cy: u32,  // DECSC state
    saved_cell: ExtendedCell,
    tabs: BitVec,              // tab stops (or Vec<bool>)
    title: String,
    cursor_style: CursorStyle, // block, beam, underline (passed through)
}
```

### Pane

```rust
struct Pane {
    id: PaneId,
    pty_master: RawFd,
    pid: pid_t,
    screen: Screen,         // active screen
    alt_screen: Screen,     // alternate screen buffer
    parser: VtParser,       // VT100 state machine
    sx: u32, sy: u32,       // pane dimensions (cells)
    xoff: u32, yoff: u32,   // position within window
    flags: PaneFlags,       // REDRAW, etc.
    cwd: Option<String>,    // from OSC 7
    window: WindowId,       // parent
}
```

### Window

```rust
struct Window {
    id: WindowId,
    idx: u32,               // 1-based display index
    name: String,
    active_pane: PaneId,
    panes: Vec<PaneId>,     // ordered list
    sx: u32, sy: u32,       // window dimensions
    zoomed: Option<PaneId>, // if a pane is zoomed
    session: SessionId,     // parent
}
```

### Session

```rust
struct Session {
    id: SessionId,
    name: String,
    windows: Vec<WindowId>,  // ordered by idx
    active_window: WindowId,
    next_window_idx: u32,
}
```

### Client

```rust
struct Client {
    id: ClientId,
    socket_fd: RawFd,       // IPC socket to client process
    tty_fd: RawFd,          // client's terminal (via SCM_RIGHTS)
    sx: u32, sy: u32,       // terminal size
    session: SessionId,
    prefix_active: bool,
    repeat_deadline: Option<Instant>,  // for -r bindings
    input_buf: Vec<u8>,     // raw bytes from client
    output_buf: Vec<u8>,    // pending escape sequences to write to tty
    mode: ClientMode,       // Normal, CopyMode, CommandPrompt
}
```

### VT100 Parser (Enum State Machine)

```rust
enum VtState {
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    OscString,
    DcsEntry,
    DcsPassthrough,
    SosPmApc,  // consume until ST
}

struct VtParser {
    state: VtState,
    params: Vec<u16>,       // CSI parameter values
    intermediates: Vec<u8>, // intermediate bytes
    osc_buf: Vec<u8>,       // OSC string accumulator
    utf8_buf: [u8; 4],      // partial UTF-8
    utf8_len: u8,
    utf8_need: u8,
}
```

Transition logic expressed as `match (state, byte)` arms. Port the semantic logic from tmux's `input.c` but express as idiomatic Rust enums/matches.

**Sequences handled**: CSI (cursor movement, SGR colors including 256+RGB+styled underlines, erase, scroll region, mode set/reset, device attributes), OSC (title, OSC 7 cwd, OSC 8 hyperlinks, OSC 52 clipboard), C0 controls (CR, LF, BS, TAB, BEL, ESC), DCS passthrough, DECSC/DECRC, alternate screen (DECSET 47/1047/1049), cursor style (DECSCUSR), mouse mode, focus events, bracketed paste mode, synchronized output.

**Unknown sequences**: consumed and dropped silently, but logged at debug level (to a log file, not stderr/stdout — those belong to the terminal). Debug log path: `$XDG_RUNTIME_DIR/tm/tm.log` or `$TMPDIR/tm-$UID/tm.log`, only written when `TM_LOG=1` env var is set.

### Key Bindings

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct KeyCode(u32);
// Lower 21 bits: Unicode codepoint or special key enum value
// Bits 24-26: KEY_CTRL, KEY_META, KEY_SHIFT

enum Action {
    Detach,
    NewWindow,          // opens command prompt for name
    RenameWindow,       // opens command prompt
    NextWindow,
    PrevWindow,
    SwapWindowLeft,
    SwapWindowRight,
    SelectWindow(u8),   // 1-9
    SplitH,
    SplitV,
    KillPane,
    ZoomPane,
    FocusPaneUp,
    FocusPaneDown,
    FocusPaneLeft,
    FocusPaneRight,
    SelectPane(u8),     // by index
    MovePaneToWindow,   // opens command prompt
    BreakPane,
    CopyMode,
    CommandPrompt,
    ReloadConfig,
    SendPrefix,
    DisplayMessage(String),
}

struct KeyBinding {
    key: KeyCode,
    action: Action,
    repeat: bool,  // -r flag
}
```

Default bindings match user's tmux config. Prefix: C-a. Repeat timeout: 500ms.

## Features

### Copy Mode (Mouse-Driven)
- **Enter**: mouse wheel up auto-enters copy mode, freezes pane output
- **Scroll**: mouse wheel up/down, page-up/page-down keys
- **Select**: click-drag for character selection
- **Copy**: auto-copy to system clipboard (OSC 52) on mouse button release
- **Selection style**: reverse video (swap fg/bg of selected cells)
- **Exit**: scroll down past bottom of scrollback, or press Escape/q
- **No keyboard navigation** (no emacs/vi movement commands)
- **No search** (deferred to future)
- **No scroll position indicator**
- Scroll coalescing: accumulate wheel deltas, render on 16ms timer, use insertline/deleteline optimization (port from tmux optimized-scrolling branch)

### Alt Screen + Mouse Wheel
- When pane is in alternate screen (vim, less, htop), mouse wheel up/down sends Up/Down arrow keys to the program instead of scrolling

### Mouse
- Click: select/focus pane
- Drag: text selection (in copy mode)
- Wheel: scroll / enter copy mode
- SGR mouse protocol (1006) for position reporting
- No double/triple click handling

### Windows
- Base index 1, renumber on close
- Navigate: C-a C-Left/C-Right, C-a 1-9
- Swap: C-a S-Left/S-Right (shift+arrow)
- New: C-a n (prompts for name)
- Rename: C-a r (prompts for name)

### Panes
- Split: C-a \ (horizontal, even layout), C-a - (vertical, even layout). Focus moves to new pane.
- Focus: mouse click, C-a arrows (directional), C-a number
- Zoom: C-a z (toggle)
- Kill: C-a k (cascades)
- Break out: C-a C-n
- Move to window: C-a m (prompts for window index)
- CWD: tracked via OSC 7 from shell, inherited on split
- Borders: Unicode box drawing (│ ─ ┌ ┐ └ ┘ ┬ ┴ ├ ┤ ┼)

### Layouts (Binary Split Tree)
Layout is a binary tree of splits:
```rust
enum LayoutNode {
    Pane(PaneId),
    Split {
        dir: SplitDir,           // Horizontal (columns) or Vertical (rows)
        children: Vec<LayoutNode>,
        // Space divided equally among children
    },
}
```
- split-h on a pane: if parent is horizontal split, add sibling; otherwise wrap in new horizontal split
- split-v on a pane: if parent is vertical split, add sibling; otherwise wrap in new vertical split
- Children in a split share space equally (even division)
- Zoom: active pane fills window, layout tree preserved for unzoom
- On pane close: remove from tree, collapse single-child splits
- Example: split-h, then split-v on right pane → `Horizontal([Pane(1), Vertical([Pane(2), Pane(3)])])`

### Status Bar (Hardcoded Layout)
```
(session) 1:name 2:name 3:name
```
- Left: `(session_name)` — yellow when prefix is active, white otherwise
- Window list: `idx:name` — green for current window, dim for others
- Zoom indicator: ` (Z)` appended to zoomed window
- Right: empty
- Background: black, foreground: white
- Position: bottom
- Messages: overlay status bar for 2 seconds, then restore

### Passthrough
- Cursor style (DECSCUSR): block/beam/underline changes forwarded to client terminal
- Bracketed paste mode
- Focus in/out events
- OSC 52 clipboard (both directions)
- OSC 7 CWD reporting (consumed by tm, not forwarded)
- Synchronized output

### Config
Path: `~/.config/tm/tm.conf` — not created by default, built-in defaults used if absent.

```
# set OPTION VALUE
set prefix C-a
set escape-time 0
set mouse on
set history-limit 10000
set base-index 1
set renumber-windows on
set focus-events on
set status-position bottom
set status-bg black
set status-fg white
set repeat-time 500

# bind [-r] KEY ACTION
bind Enter reload-config
bind d detach
bind C-s send-prefix
bind / command-prompt
bind n new-window
bind r rename-window
bind -r C-Left prev-window
bind -r C-Right next-window
bind -r S-Left swap-window-left
bind -r S-Right swap-window-right
bind \\ split-h
bind - split-v
bind z zoom-pane
bind k kill-pane
bind m move-pane
bind -r C-n break-pane
bind Up focus-up
bind Down focus-down
bind Left focus-left
bind Right focus-right
```

On reload (C-a Enter): reset all bindings to built-in defaults, then re-apply config file. Display "configuration reloaded" message for 2 seconds.

### Command Prompt (C-a /)
- Basic line editing: type characters, backspace, Enter to submit, Escape to cancel
- No tab completion
- Accepts: `rename-window NAME`, `new-window [-n NAME]`, `join-pane -h -t INDEX`

### CLI
```
tm new [-s NAME]      Create session and attach
tm attach [-t NAME]   Attach to session (creates new if none exist)
tm ls                 List sessions
tm kill [-t NAME]     Kill session
```
Hand-rolled arg parsing, ~50 lines.

### Environment
Panes get:
- `TERM=tmux-256color`
- `TM=/path/to/socket,server_pid,pane_id`
- `TMUX=/path/to/socket,server_pid,pane_id` (compatibility)
- Shell: `$SHELL` as login shell (argv[0] prefixed with `-`)

## File Structure

```
tm/
  Cargo.toml
  src/
    main.rs          Entry point, CLI parsing, client-or-server        (~200 lines)
    server.rs        Server: accept connections, main loop, signals     (~500 lines)
    client.rs        Client: connect, raw mode, forward bytes           (~300 lines)
    protocol.rs      IPC message types, serialize, SCM_RIGHTS           (~250 lines)
    state.rs         Central State struct, entity CRUD, ID types        (~300 lines)
    grid.rs          Grid, GridLine, CompactCell, ExtendedCell          (~500 lines)
    screen.rs        Screen state, screen_write operations              (~500 lines)
    vt.rs            VT100 parser (enum state machine)                  (~800 lines)
    tty.rs           Terminal output: escape sequences, buffered write  (~500 lines)
    keys.rs          Key code types, input parsing, trie for sequences  (~450 lines)
    key_bind.rs      Binding tables, prefix handling, action dispatch   (~300 lines)
    copy.rs          Copy mode: scroll, select, OSC 52 clipboard       (~400 lines)
    layout.rs        Binary split tree, zoom, resize calculation        (~400 lines)
    render.rs        Compositing: pane content, borders, status bar     (~400 lines)
    config.rs        Config file parser, option storage, defaults       (~200 lines)
    prompt.rs        Command prompt overlay, basic line editing         (~150 lines)
    pty.rs           PTY allocation (forkpty), process spawn            (~150 lines)
    sys.rs           Platform-specific: signals, forkpty, cwd           (~200 lines)
    log.rs           Simple file logger (when TM_LOG=1)                (~40 lines)
                                                                Total: ~6,500 lines
```

## What We Cut from tmux

| tmux subsystem | Lines | Our replacement |
|----------------|-------|----------------|
| Format engine (format.c + format-draw.c) | 7,147 | Hardcoded status bar (~50 lines in render.rs) |
| Command parser (cmd-parse.c, Bison) | 3,424 | Enum dispatch (~100 lines) |
| Options system (options.c + options-table.c) | 2,908 | Flat config struct (~100 lines) |
| 65 cmd-*.c files | ~20,000 | Action enum + dispatch (~200 lines) |
| Copy mode (window-copy.c) | 6,477 | Mouse-only copy (~400 lines) |
| Control mode | ~1,500 | Cut entirely |
| Style system (style.c) | ~800 | Hardcoded colors |
| Mode tree, choose-tree, window modes | ~5,000 | Cut entirely |
| Compat layer (compat/) | ~3,000 | Rust stdlib handles this |

## What We Port from tmux (Reference, Not Copy)

| Our module | tmux file | What to study |
|------------|-----------|---------------|
| vt.rs | ~/d/tmux/input.c | State transitions, CSI dispatch table, which sequences matter |
| grid.rs | ~/d/tmux/grid.c | Two-tier cell encoding, line management |
| copy.rs | ~/d/tmux/window-copy.c + optimized-scrolling diff | Scroll coalescing: accumulate deltas, 16ms timer, insertline/deleteline fast path |
| keys.rs | ~/d/tmux/tty-keys.c | Ternary trie for matching multi-byte escape sequences |
| layout.rs | ~/d/tmux/layout-set.c | Even-horizontal/vertical cell spread algorithm |
| screen.rs | ~/d/tmux/screen.c + screen-write.c | Scroll region logic, mode tracking |
| render.rs | ~/d/tmux/tty.c | Which escape sequences to emit for cursor movement, colors, attributes |

## Implementation Phases

### Phase 1: Single Pane Shell
**Goal**: `tm new` gives a working shell with colors and cursor movement.

1. `Cargo.toml` — project setup, deps (libc, anyhow, mio)
2. `sys.rs` — platform abstractions (signal handling, `forkpty` wrapper)
3. `protocol.rs` — message types, serialization, SCM_RIGHTS fd passing
4. `main.rs` — CLI parsing: `tm new`, `tm attach`, `tm ls`, `tm kill`
5. `client.rs` — connect to socket, enter raw mode, forward bytes, handle SIGWINCH
6. `server.rs` — create socket, accept connections, mio event loop, 16ms tick
7. `state.rs` — State struct, ID types, session/window/pane creation
8. `pty.rs` — forkpty, spawn login shell, set TERM/TM/TMUX env vars
9. `grid.rs` — Grid (VecDeque ring buffer), GridLine, two-tier cells, dirty tracking
10. `screen.rs` — Screen state, write operations (put_cell, linefeed, carriage_return, cursor_move, scroll_up, scroll_down, erase, set_scroll_region, mode_set/reset, alternate screen)
11. `vt.rs` — VT100 parser: Ground, Escape, CSI (cursor movement, SGR, erase, scroll region, mode set/reset, device attributes), OSC (title, cwd, clipboard), C0, DECSC/DECRC, alternate screen, cursor style
12. `tty.rs` — buffered output: cursor_goto, set_attrs (SGR), clear, scroll_region, synchronized output begin/end
13. `keys.rs` — parse raw bytes into key events (regular chars, escape sequences, function keys, arrow keys, mouse SGR)
14. `render.rs` — render single pane: iterate dirty cells, emit escape sequences, clear dirty bits

**Test**: Run `tm new`. Shell works. Run `vim`, `htop`, `ls --color`. Colors, cursor, alternate screen all work.

### Phase 2: Windows + Status Bar + Key Bindings
1. Window management in `state.rs`: create, destroy, rename, navigate, swap, renumber (base-index 1)
2. `render.rs` — add status bar rendering (hardcoded format)
3. `key_bind.rs` — prefix key (C-a), binding table, repeat timer (500ms), action dispatch
4. `config.rs` — parse tm.conf (set/bind lines), option storage, defaults
5. `render.rs` — full compositing: pane content + status bar, status messages with 2s timeout

**Test**: C-a n creates windows, C-a C-Left/C-Right navigates, C-a 1-9 selects, C-a r renames. Status bar shows window list with correct formatting. C-a Enter reloads config.

### Phase 3: Panes + Layouts
1. Pane splitting in `state.rs`: allocate new PTY, add pane to window, inherit CWD (from OSC 7)
2. `layout.rs` — binary split tree: insert/remove pane, calculate positions/sizes (recursive), zoom save/restore
3. `render.rs` — Unicode box-drawing borders between panes, multi-pane compositing
4. Pane focus: mouse click detection (check click coordinates against pane bounds), C-a arrow directional focus, C-a number selection
5. Actions: join-pane, break-pane
6. `prompt.rs` — command prompt overlay for new-window name, rename-window, join-pane target

**Test**: C-a \ splits horizontal, C-a - splits vertical. Layout auto-adjusts. C-a z zooms. Click switches pane. C-a k kills pane, layout recalculates.

### Phase 4: Copy Mode + Mouse
1. `copy.rs` — enter copy mode (freeze pane, create viewport into scrollback)
2. Mouse wheel: auto-enter copy mode, scroll up/down with accumulator + 16ms coalescing
3. Insertline/deleteline optimization for fast scroll rendering (port from optimized-scrolling branch)
4. Click-drag selection: track selection start/end, highlight selected cells
5. Auto-copy on mouse release: encode selection as OSC 52, write to client tty
6. Alt screen wheel: detect alternate screen mode, send Up/Down arrow keys instead
7. Mouse click pane selection (if not in copy mode)
8. Exit copy mode: scroll past bottom, Escape, q

**Test**: Mouse wheel scrolls smoothly through 10K lines of history. Click-drag selects, text appears in system clipboard on release. vim/less scroll with mouse wheel.

### Phase 5: Polish + Detach/Reattach
1. Detach: C-a d sends detach message, client exits cleanly, server keeps session alive
2. Reattach: `tm attach` reconnects, server redraws full screen to new client tty
3. `tm ls` lists sessions with status
4. `tm kill` destroys session
5. `tm attach` creates new session when none exist
6. Focus events passthrough (DECSET 1004)
7. Bracketed paste passthrough
8. Grid reflow on resize (use WRAPPED line flags to rewrap)
9. Handle edge cases: zero-size panes, very long lines, wide characters (CJK), terminal resize during copy mode
10. ASan integration (`cargo test` under address sanitizer), thorough #[cfg(test)] modules
11. Integration tests: spawn tm server, interact via PTY, verify output

## Verification

1. **Shell**: `tm new` → run shell, vim, htop, ls --color — colors, cursor, alternate screen work
2. **Windows**: Create 3+ windows, navigate (C-a 1/2/3), rename, swap — status bar correct
3. **Panes**: Split h/v, zoom, kill — borders draw correctly, layout recalculates
4. **Copy mode**: Mouse wheel scrolls smoothly. Click-drag selects. Clipboard works (paste elsewhere to verify)
5. **Detach/reattach**: C-a d, then `tm attach` — full screen restored including scrollback
6. **Config**: Edit tm.conf, C-a Enter — bindings update, message shown
7. **Alt screen**: Open vim, mouse wheel scrolls vim content
8. **Flow control**: Run `yes` in a pane — tm stays responsive, no terminal lag
9. **Resize**: Resize terminal window — panes relayout, content reflows (phase 5)
10. **Unit tests**: `cargo test` — grid ops, VT100 parser, key parsing, config parsing all pass
11. **Integration tests**: Automated PTY-based tests verify end-to-end behavior
12. **Sanitizers**: No undefined behavior, no leaks under address sanitizer
