use crate::config::Config;
use crate::grid::{CellAttr, CellContent, Color, CompactCell};
use crate::screen::ScreenMode;
use crate::state::{ClientId, ClientMode, PaneId, State, WindowId};
use crate::tty::TtyWriter;

/// Render the full screen for a client.
pub fn render_client(state: &State, config: &Config, cid: ClientId, tty: &mut TtyWriter) {
    let Some(client) = state.clients.get(&cid) else {
        return;
    };
    let Some(session) = state.sessions.get(&client.session) else {
        return;
    };
    let Some(window) = state.windows.get(&session.active_window) else {
        return;
    };

    let sx = client.sx;
    let sy = client.sy;
    let status_row = sy.saturating_sub(1);

    tty.sync_begin();
    tty.cursor_hide();

    let copy_modes = &client.copy_modes;
    let sel = client.sel;

    // Compute adjusted copy_top for a pane, accounting for pruned history lines
    let adjusted_copy_top = |pid: PaneId| -> Option<u32> {
        let cs = copy_modes.get(&pid)?;
        let pruned_since = state
            .panes
            .get(&pid)
            .map(|p| p.active_screen().grid.lines_pruned)
            .unwrap_or(cs.pruned_at);
        let drift = (pruned_since - cs.pruned_at) as u32;
        Some(cs.top.saturating_sub(drift))
    };

    // Render panes
    if let Some(zoomed_pid) = window.zoomed {
        let ct = adjusted_copy_top(zoomed_pid);
        let pane_sel = sel.filter(|s| s.pane == zoomed_pid);
        render_pane(
            state,
            zoomed_pid,
            0,
            0,
            sx,
            status_row,
            ct,
            pane_sel.as_ref(),
            sx,
            tty,
        );
    } else {
        let geos = window.layout.calculate(0, 0, window.sx, window.sy);

        // Render pane borders (with yellow overlay for copy-mode panes)
        render_borders(state, window, &geos, copy_modes, sx, status_row, tty);

        // Render each pane
        for geo in &geos {
            let ct = adjusted_copy_top(geo.id);
            let pane_sel = sel.filter(|s| s.pane == geo.id);
            render_pane(
                state,
                geo.id,
                geo.xoff,
                geo.yoff,
                geo.sx,
                geo.sy,
                ct,
                pane_sel.as_ref(),
                sx,
                tty,
            );
        }
    }

    // Render status bar
    render_status(
        state,
        config,
        cid,
        session.active_window,
        status_row,
        sx,
        tty,
    );

    // Position cursor at active pane's cursor
    let active_pid = window.active_pane;
    if let Some(pane) = state.panes.get(&active_pid) {
        let screen = pane.active_screen();
        if screen.mode.has(ScreenMode::CURSOR_VISIBLE)
            && client.mode == ClientMode::Normal
            && !client.copy_modes.contains_key(&active_pid)
        {
            let (xoff, yoff) = if window.zoomed.is_some() {
                (0, 0)
            } else {
                (pane.xoff, pane.yoff)
            };
            tty.cursor_goto(yoff + screen.cy, xoff + screen.cx);
            tty.cursor_style(screen.cursor_style);
            tty.cursor_show();
        }
    }

    tty.sync_end();
}

#[allow(clippy::too_many_arguments)]
fn render_pane(
    state: &State,
    pid: PaneId,
    xoff: u32,
    yoff: u32,
    sx: u32,
    sy: u32,
    copy_top: Option<u32>,
    sel: Option<&crate::state::Selection>,
    client_sx: u32,
    tty: &mut TtyWriter,
) {
    let Some(pane) = state.panes.get(&pid) else {
        return;
    };
    let screen = pane.active_screen();
    let grid = &screen.grid;
    let force =
        pane.flags.contains(crate::state::PaneFlags::REDRAW) || copy_top.is_some() || sel.is_some();

    // Scroll optimization: when the grid has pending full-screen scrolls and
    // the pane spans the full terminal width, emit CSI S to scroll the terminal
    // and only repaint lines with actual content changes.
    let scroll_pending = grid.scroll_pending;
    let use_scroll_opt =
        scroll_pending > 0 && scroll_pending < sy && !force && xoff == 0 && sx >= client_sx;

    if use_scroll_opt {
        tty.set_scroll_region(yoff, yoff + sy - 1);
        tty.scroll_up_lines(scroll_pending);
        tty.reset_scroll_region();
    }

    // Pre-compute selection range
    let sel_range = sel.map(|s| s.ordered());

    for row in 0..sy {
        // Compute absolute grid row for this viewport line
        let abs_row = if let Some(top) = copy_top {
            top + row
        } else {
            grid.hsize() + row
        };

        let line = grid.line(abs_row);
        let Some(line) = line else {
            tty.cursor_goto(yoff + row, xoff);
            tty.reset_attrs();
            for _ in 0..sx {
                tty.write_raw(b" ");
            }
            continue;
        };

        if !force {
            let any_dirty = if use_scroll_opt {
                // Only render lines with content changes — scroll-shifted-only
                // lines are already correct on the terminal from CSI S.
                line.compact
                    .iter()
                    .take(sx as usize)
                    .any(|c| c.is_content_dirty())
            } else {
                line.compact.iter().take(sx as usize).any(|c| c.is_dirty())
            };
            if !any_dirty {
                continue;
            }
        }

        tty.cursor_goto(yoff + row, xoff);
        tty.reset_attrs();

        let cols = sx.min(line.compact.len() as u32);
        let mut col = 0u32;
        while col < cols {
            let c = &line.compact[col as usize];

            if c.flags & CompactCell::WIDE_CONT != 0 {
                col += 1;
                continue;
            }

            let mut cell = line.get_cell(col);

            // Check if this cell is in the selection — yellow background
            if let Some(((sc, sr), (ec, er))) = sel_range {
                let in_sel = if sr == er {
                    abs_row == sr && col >= sc && col <= ec
                } else if abs_row == sr {
                    col >= sc
                } else if abs_row == er {
                    col <= ec
                } else {
                    abs_row > sr && abs_row < er
                };
                if in_sel {
                    cell.bg = Color::Palette(3); // yellow
                    cell.fg = Color::Palette(0); // black text on yellow
                }
            }

            tty.set_cell_attrs(&cell);

            let s = cell.ch_str();
            tty.write_str(s);

            col += cell.ch_width as u32;
        }

        // Clear to end of pane area if line is shorter
        if cols < sx {
            tty.reset_attrs();
            let remaining = sx - cols;
            for _ in 0..remaining {
                tty.write_raw(b" ");
            }
        }
    }
}

fn render_borders(
    _state: &State,
    window: &crate::state::Window,
    geos: &[crate::layout::PaneGeometry],
    copy_modes: &std::collections::HashMap<PaneId, crate::state::CopyState>,
    sx: u32,
    sy: u32,
    tty: &mut TtyWriter,
) {
    if geos.len() <= 1 {
        return;
    }

    let active_pid = window.active_pane;

    // Find the active pane geometry
    let active_geo = geos.iter().find(|g| g.id == active_pid);

    // Check which side of a border cell gets priority for coloring
    let border_attr = |x: u32, y: u32| -> &CellContent {
        // Check if adjacent to a copy-mode pane (yellow takes priority)
        for geo in geos {
            if !copy_modes.contains_key(&geo.id) {
                continue;
            }
            if (x == geo.xoff + geo.sx || x + 1 == geo.xoff)
                && y >= geo.yoff
                && y < geo.yoff + geo.sy
            {
                return &YELLOW_BORDER;
            }
            if (y == geo.yoff + geo.sy || y + 1 == geo.yoff)
                && x >= geo.xoff
                && x < geo.xoff + geo.sx
            {
                return &YELLOW_BORDER;
            }
        }
        // Check if adjacent to the active pane (green)
        if let Some(ag) = active_geo {
            if (x == ag.xoff + ag.sx || x + 1 == ag.xoff) && y >= ag.yoff && y < ag.yoff + ag.sy {
                return &GREEN_BORDER;
            }
            if (y == ag.yoff + ag.sy || y + 1 == ag.yoff) && x >= ag.xoff && x < ag.xoff + ag.sx {
                return &GREEN_BORDER;
            }
        }
        &DIM_BORDER
    };

    for geo in geos {
        // Right border (vertical line)
        let right_x = geo.xoff + geo.sx;
        if right_x < sx {
            for row in geo.yoff..geo.yoff + geo.sy {
                if row < sy {
                    tty.cursor_goto(row, right_x);
                    tty.set_cell_attrs(border_attr(right_x, row));
                    tty.write_str("\u{2502}"); // │
                }
            }
        }

        // Bottom border (horizontal line)
        let bottom_y = geo.yoff + geo.sy;
        if bottom_y < sy {
            for col in geo.xoff..geo.xoff + geo.sx {
                tty.cursor_goto(bottom_y, col);
                tty.set_cell_attrs(border_attr(col, bottom_y));
                tty.write_str("\u{2500}"); // ─
            }
            // Corner/intersection
            if geo.xoff + geo.sx < sx {
                tty.cursor_goto(bottom_y, geo.xoff + geo.sx);
                tty.set_cell_attrs(border_attr(geo.xoff + geo.sx, bottom_y));
                tty.write_str("\u{253C}"); // ┼
            }
        }
    }
}

const DIM_BORDER: CellContent = CellContent {
    ch: [0; 8],
    ch_len: 0,
    ch_width: 0,
    fg: Color::Default,
    bg: Color::Default,
    us: Color::Default,
    attr: CellAttr(0),
};
const GREEN_BORDER: CellContent = CellContent {
    ch: [0; 8],
    ch_len: 0,
    ch_width: 0,
    fg: Color::Palette(2),
    bg: Color::Default,
    us: Color::Default,
    attr: CellAttr(0),
};
const YELLOW_BORDER: CellContent = CellContent {
    ch: [0; 8],
    ch_len: 0,
    ch_width: 0,
    fg: Color::Palette(3),
    bg: Color::Default,
    us: Color::Default,
    attr: CellAttr(0),
};

fn render_status(
    state: &State,
    config: &Config,
    cid: ClientId,
    active_wid: WindowId,
    row: u32,
    sx: u32,
    tty: &mut TtyWriter,
) {
    let Some(client) = state.clients.get(&cid) else {
        return;
    };
    let Some(session) = state.sessions.get(&client.session) else {
        return;
    };

    tty.cursor_goto(row, 0);

    // Check for status message overlay
    if let Some((msg, _)) = &client.status_message {
        tty.reset_attrs();
        tty.set_cell_attrs(&CellContent {
            fg: Color::Palette(3), // yellow
            bg: Color::Default,
            ..CellContent::default()
        });
        let msg_display: String = msg.chars().take(sx as usize).collect();
        tty.write_str(&msg_display);
        // Pad to full width
        let remaining = sx as usize - msg_display.len().min(sx as usize);
        for _ in 0..remaining {
            tty.write_raw(b" ");
        }
        return;
    }

    // Check for command prompt
    if client.mode == ClientMode::CommandPrompt {
        tty.reset_attrs();
        tty.set_cell_attrs(&CellContent {
            fg: Color::Palette(7), // white
            bg: Color::Default,
            ..CellContent::default()
        });
        let prompt_prefix = match &client.prompt_action {
            Some(crate::state::PromptAction::NewWindow) => "window name: ",
            Some(crate::state::PromptAction::RenameWindow) => "rename: ",
            Some(crate::state::PromptAction::MovePane) => "target window: ",
            Some(crate::state::PromptAction::Command) => ": ",
            None => ": ",
        };
        let input = client.prompt_buf.as_deref().unwrap_or("");
        let display = format!("{prompt_prefix}{input}");
        let display: String = display.chars().take(sx as usize).collect();
        tty.write_str(&display);
        let remaining = sx as usize - display.len().min(sx as usize);
        for _ in 0..remaining {
            tty.write_raw(b" ");
        }
        return;
    }

    // Normal status bar
    // Background
    tty.reset_attrs();
    tty.set_cell_attrs(&CellContent {
        fg: config.status_fg,
        bg: config.status_bg,
        ..CellContent::default()
    });

    let mut pos = 0usize;

    // Session name: (name)
    if client.prefix_active {
        tty.set_cell_attrs(&CellContent {
            fg: Color::Palette(3), // yellow
            bg: config.status_bg,
            ..CellContent::default()
        });
    }
    tty.write_raw(b"(");
    tty.write_str(&session.name);
    tty.write_raw(b")");
    pos += session.name.len() + 2;

    // Copy mode indicator
    if !client.copy_modes.is_empty() {
        tty.set_cell_attrs(&CellContent {
            fg: Color::Palette(3), // yellow
            bg: config.status_bg,
            attr: CellAttr(CellAttr::BOLD),
            ..CellContent::default()
        });
        tty.write_str("(copy)");
        pos += 6;
    }

    // Reset color for window list
    tty.set_cell_attrs(&CellContent {
        fg: config.status_fg,
        bg: config.status_bg,
        ..CellContent::default()
    });
    tty.write_raw(b" ");
    pos += 1;

    // Window list
    for &wid in &session.windows {
        let Some(window) = state.windows.get(&wid) else {
            continue;
        };
        let is_active = wid == active_wid;
        let zoom = window.zoomed.is_some();
        // Estimate entry length without allocating
        let idx_len = if window.idx >= 10 { 2 } else { 1 };
        let entry_len = idx_len + 1 + window.name.len() + if zoom { 4 } else { 0 };

        if pos + entry_len + 1 >= sx as usize {
            break;
        }

        if is_active {
            tty.set_cell_attrs(&CellContent {
                fg: Color::Palette(2), // green
                bg: config.status_bg,
                attr: CellAttr(CellAttr::BOLD),
                ..CellContent::default()
            });
        } else {
            tty.set_cell_attrs(&CellContent {
                fg: config.status_fg,
                bg: config.status_bg,
                attr: CellAttr(CellAttr::DIM),
                ..CellContent::default()
            });
        }

        // Write idx:name(Z) directly without format! allocation
        {
            use std::io::Write;
            let _ = write!(tty.buf, "{}:{}", window.idx, window.name);
            if zoom {
                tty.buf.extend_from_slice(b" (Z)");
            }
        }
        pos += entry_len;

        tty.set_cell_attrs(&CellContent {
            fg: config.status_fg,
            bg: config.status_bg,
            ..CellContent::default()
        });
        tty.write_raw(b" ");
        pos += 1;
    }

    // Pad remaining with background
    while pos < sx as usize {
        tty.write_raw(b" ");
        pos += 1;
    }
}

/// Clear dirty flags on all visible cells for a client's current window.
pub fn clear_dirty(state: &mut State, cid: ClientId) {
    let Some(client) = state.clients.get(&cid) else {
        return;
    };
    let Some(session) = state.sessions.get(&client.session) else {
        return;
    };
    let Some(window) = state.windows.get(&session.active_window) else {
        return;
    };

    for &pid in &window.panes {
        if let Some(pane) = state.panes.get_mut(&pid) {
            let screen = pane.active_screen_mut();
            screen.grid.scroll_pending = 0;
            let sy = screen.sy();
            for row in 0..sy {
                if let Some(line) = screen.grid.visible_line_mut(row) {
                    for c in &mut line.compact {
                        c.clear_dirty();
                    }
                }
            }
            pane.flags = crate::state::PaneFlags::NONE;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::state::{Client, Pane, PaneFlags, State};
    use crate::tty::TtyWriter;

    fn make_test_state() -> (State, crate::state::ClientId) {
        let mut state = State::new();
        let pid = state.alloc_pane_id();
        let pane = Pane::new(pid, -1, 0, 80, 24);
        state.panes.insert(pid, pane);
        let sid = state.create_session("test", pid, 80, 25);
        let cid = state.alloc_client_id();
        state
            .clients
            .insert(cid, Client::new(cid, -1, -1, 80, 25, sid));
        (state, cid)
    }

    #[test]
    fn clear_dirty_clears_flags() {
        let (mut state, cid) = make_test_state();

        // Mark cells dirty on the pane's grid
        let pid = state.active_pane_for_client(cid).unwrap();
        {
            let pane = state.panes.get_mut(&pid).unwrap();
            let screen = pane.active_screen_mut();
            let line = screen.grid.visible_line_mut(0).unwrap();
            line.compact[0].set_dirty();
            line.compact[1].set_dirty();
            assert!(line.compact[0].is_dirty());
        }

        super::clear_dirty(&mut state, cid);

        // After clearing, dirty flags should be gone
        let pane = state.panes.get(&pid).unwrap();
        let screen = pane.active_screen();
        let line = screen.grid.line(screen.grid.hsize()).unwrap();
        assert!(!line.compact[0].is_dirty());
        assert!(!line.compact[1].is_dirty());
        assert_eq!(pane.flags, PaneFlags::NONE);
    }

    #[test]
    fn render_client_no_panic() {
        let (state, cid) = make_test_state();
        let config = Config::default_config();
        let mut tty = TtyWriter::new();

        // Should not panic with a properly constructed state
        super::render_client(&state, &config, cid, &mut tty);

        // The tty buffer should have some output (at minimum sync_begin, cursor_hide, etc.)
        assert!(!tty.is_empty());
    }

    #[test]
    fn scroll_optimization_reduces_output() {
        use crate::grid::CellContent;

        let (mut state, cid) = make_test_state();
        let config = Config::default_config();
        let pid = state.active_pane_for_client(cid).unwrap();

        // Fill visible lines
        let pane = state.panes.get_mut(&pid).unwrap();
        for row in 0..24u32 {
            for col in 0..80u32 {
                let content = CellContent::from_ascii(b'A' + (col % 26) as u8);
                pane.screen
                    .grid
                    .visible_line_mut(row)
                    .unwrap()
                    .set_cell(col, &content);
            }
        }

        // Initial render + clear dirty (establishes terminal state)
        let mut tty = TtyWriter::new();
        super::render_client(&state, &config, cid, &mut tty);
        let full_render_bytes = tty.buf.len();
        super::clear_dirty(&mut state, cid);

        // Scroll 1 line + write new bottom content
        let pane = state.panes.get_mut(&pid).unwrap();
        let sy = pane.screen.grid.sy;
        pane.screen.grid.scroll_up(0, sy - 1);
        for col in 0..80u32 {
            let content = CellContent::from_ascii(b'a' + (col % 26) as u8);
            pane.screen
                .grid
                .visible_line_mut(sy - 1)
                .unwrap()
                .set_cell(col, &content);
        }

        // Render after scroll — should use scroll optimization
        tty.buf.clear();
        tty.reset_state();
        super::render_client(&state, &config, cid, &mut tty);
        let scroll_render_bytes = tty.buf.len();

        // Scroll-optimized render should be much smaller than full render
        assert!(
            scroll_render_bytes < full_render_bytes / 2,
            "scroll render ({scroll_render_bytes}B) should be much less than \
             full render ({full_render_bytes}B)"
        );

        // Verify scroll_pending is consumed by clear_dirty
        super::clear_dirty(&mut state, cid);
        let pane = state.panes.get(&pid).unwrap();
        assert_eq!(pane.screen.grid.scroll_pending, 0);
    }

    #[test]
    fn scroll_optimization_byte_counts() {
        use crate::grid::CellContent;

        for (sx, sy_total, label) in [(80, 25, "80x24"), (200, 51, "200x50")] {
            let mut state = State::new();
            let config = Config::default_config();
            let pid = state.alloc_pane_id();
            let sy = sy_total - 1; // status bar
            let pane = Pane::new(pid, -1, 0, sx, sy);
            state.panes.insert(pid, pane);
            let sid = state.create_session("test", pid, sx, sy_total);
            let cid = state.alloc_client_id();
            state
                .clients
                .insert(cid, Client::new(cid, -1, -1, sx, sy_total, sid));

            // Fill and do initial render
            let pane = state.panes.get_mut(&pid).unwrap();
            for row in 0..sy {
                for col in 0..sx {
                    let content = CellContent::from_ascii(b'A' + (col % 26) as u8);
                    pane.screen
                        .grid
                        .visible_line_mut(row)
                        .unwrap()
                        .set_cell(col, &content);
                }
            }
            let mut tty = TtyWriter::new();
            super::render_client(&state, &config, cid, &mut tty);
            let full = tty.buf.len();
            super::clear_dirty(&mut state, cid);

            // Scroll 1 + render
            let pane = state.panes.get_mut(&pid).unwrap();
            pane.screen.grid.scroll_up(0, sy - 1);
            for col in 0..sx {
                let content = CellContent::from_ascii(b'a' + (col % 26) as u8);
                pane.screen
                    .grid
                    .visible_line_mut(sy - 1)
                    .unwrap()
                    .set_cell(col, &content);
            }
            tty.buf.clear();
            tty.reset_state();
            super::render_client(&state, &config, cid, &mut tty);
            let scroll1 = tty.buf.len();
            super::clear_dirty(&mut state, cid);

            // Scroll 5 + render
            let pane = state.panes.get_mut(&pid).unwrap();
            for i in 0..5u32 {
                pane.screen.grid.scroll_up(0, sy - 1);
                for col in 0..sx {
                    let content = CellContent::from_ascii(b'a' + ((col + i) % 26) as u8);
                    pane.screen
                        .grid
                        .visible_line_mut(sy - 1)
                        .unwrap()
                        .set_cell(col, &content);
                }
            }
            tty.buf.clear();
            tty.reset_state();
            super::render_client(&state, &config, cid, &mut tty);
            let scroll5 = tty.buf.len();
            super::clear_dirty(&mut state, cid);

            eprintln!(
                "{label}: full={full}B  scroll1={scroll1}B ({:.0}%)  scroll5={scroll5}B ({:.0}%)",
                scroll1 as f64 / full as f64 * 100.0,
                scroll5 as f64 / full as f64 * 100.0
            );
        }
    }
}
