# Complex TUI apps and copy mode

Complex TUI apps (e.g. Claude Code CLI / Ink-based) that render in the normal
buffer (no alternate screen) re-render their entire visible output on every
update. Each re-render writes more lines than the screen height, causing
linefeeds that push redundant content into pane scrollback via
`grid.scroll_up(0, sy-1)`.

## Mechanism

- `screen.linefeed()` -> `grid.scroll_up(0, sy-1)` -> top visible line pushed
  into history -> `hsize` grows
- For a 200-line conversation on a 50-row screen, each re-render pushes ~150
  redundant lines into scrollback
- Over a streaming response with 100 re-renders, ~15,000 lines of duplicated
  content accumulate

The alternate screen (`DECSET 1049`) was designed to prevent this -- TUI apps
render on a separate buffer with no scrollback. But apps that don't use it
(like Ink-based CLIs) flood the normal buffer's history with re-render copies.

## Copy mode interaction

The freeze from commit `1c0d242` (using absolute `copy_top` instead of relative
offset) correctly pins the viewport. But the gap between `copy_top` and the live
bottom grows rapidly, filling with junk. Scrolling down in copy mode means
traversing thousands of redundant lines.

## Key code paths

- `screen.rs:247-249` -- linefeed triggers scroll_up
- `grid.rs:439-501` -- scroll_up pushes to history (full-screen) or discards (scroll region)
- `screen.rs:339-376` -- ED (ESC[2J] does NOT push to scrollback, just clears cells
- Alt screen has its own grid with hlimit=10000 (`screen.rs:75`)
