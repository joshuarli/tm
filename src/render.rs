use crate::config::Config;
use crate::grid::{CellAttr, CellContent, Color, CompactCell};
use crate::screen::ScreenMode;
use crate::state::{ClientId, ClientMode, PaneId, State, WindowId};
use crate::tty::TtyWriter;

/// Render the full screen for a client.
pub(crate) fn render_client(state: &State, config: &Config, cid: ClientId, tty: &mut TtyWriter) {
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

    // Determine copy mode scroll offset
    let copy_oy = if client.mode == ClientMode::CopyMode {
        Some((client.copy_pane, client.copy_oy))
    } else {
        None
    };

    let sel = client.sel;

    // Render panes
    if let Some(zoomed_pid) = window.zoomed {
        let oy = copy_oy.and_then(|(p, o)| if p == zoomed_pid { Some(o) } else { None }).unwrap_or(0);
        let pane_sel = sel.filter(|s| s.pane == zoomed_pid);
        render_pane(state, zoomed_pid, 0, 0, sx, status_row, oy, pane_sel.as_ref(), tty);
    } else {
        let geos = window.layout.calculate(0, 0, window.sx, window.sy);

        // Render pane borders
        render_borders(state, window, &geos, sx, status_row, tty);

        // Render each pane
        for geo in &geos {
            let oy = copy_oy.and_then(|(p, o)| if p == geo.id { Some(o) } else { None }).unwrap_or(0);
            let pane_sel = sel.filter(|s| s.pane == geo.id);
            render_pane(state, geo.id, geo.xoff, geo.yoff, geo.sx, geo.sy, oy, pane_sel.as_ref(), tty);
        }
    }

    // Render status bar
    render_status(state, config, cid, session.active_window, status_row, sx, tty);

    // Position cursor at active pane's cursor
    let active_pid = window.active_pane;
    if let Some(pane) = state.panes.get(&active_pid) {
        let screen = pane.active_screen();
        if screen.mode.has(ScreenMode::CURSOR_VISIBLE) && client.mode == ClientMode::Normal {
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

fn render_pane(
    state: &State,
    pid: PaneId,
    xoff: u32,
    yoff: u32,
    sx: u32,
    sy: u32,
    copy_oy: u32,
    sel: Option<&crate::state::Selection>,
    tty: &mut TtyWriter,
) {
    let Some(pane) = state.panes.get(&pid) else {
        return;
    };
    let screen = pane.active_screen();
    let grid = &screen.grid;
    let force = pane.flags.contains(crate::state::PaneFlags::REDRAW)
        || copy_oy > 0
        || sel.is_some();

    // Pre-compute selection range
    let sel_range = sel.map(|s| s.ordered());

    for row in 0..sy {
        // Compute absolute grid row for this viewport line
        let abs_row = if copy_oy > 0 {
            (grid.hsize() as i64 - copy_oy as i64 + row as i64) as u32
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
            let any_dirty = line
                .compact
                .iter()
                .take(sx as usize)
                .any(|c| c.is_dirty());
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

    // For each border cell, check if it's adjacent to the active pane
    let is_active_border = |x: u32, y: u32| -> bool {
        let Some(ag) = active_geo else {
            return false;
        };
        // Cell is on the active pane's right edge
        if x == ag.xoff + ag.sx && y >= ag.yoff && y < ag.yoff + ag.sy {
            return true;
        }
        // Cell is on the active pane's left edge
        if x + 1 == ag.xoff && y >= ag.yoff && y < ag.yoff + ag.sy {
            return true;
        }
        // Cell is on the active pane's bottom edge
        if y == ag.yoff + ag.sy && x >= ag.xoff && x < ag.xoff + ag.sx {
            return true;
        }
        // Cell is on the active pane's top edge
        if y + 1 == ag.yoff && x >= ag.xoff && x < ag.xoff + ag.sx {
            return true;
        }
        false
    };

    let dim_border = CellContent::default();
    let green_border = CellContent {
        fg: Color::Palette(2),
        ..CellContent::default()
    };

    for geo in geos {
        // Right border (vertical line)
        let right_x = geo.xoff + geo.sx;
        if right_x < sx {
            for row in geo.yoff..geo.yoff + geo.sy {
                if row < sy {
                    tty.cursor_goto(row, right_x);
                    tty.set_cell_attrs(if is_active_border(right_x, row) {
                        &green_border
                    } else {
                        &dim_border
                    });
                    tty.write_str("\u{2502}"); // │
                }
            }
        }

        // Bottom border (horizontal line)
        let bottom_y = geo.yoff + geo.sy;
        if bottom_y < sy {
            for col in geo.xoff..geo.xoff + geo.sx {
                tty.cursor_goto(bottom_y, col);
                tty.set_cell_attrs(if is_active_border(col, bottom_y) {
                    &green_border
                } else {
                    &dim_border
                });
                tty.write_str("\u{2500}"); // ─
            }
            // Corner/intersection
            if geo.xoff + geo.sx < sx {
                tty.cursor_goto(bottom_y, geo.xoff + geo.sx);
                tty.set_cell_attrs(if is_active_border(geo.xoff + geo.sx, bottom_y) {
                    &green_border
                } else {
                    &dim_border
                });
                tty.write_str("\u{253C}"); // ┼
            }
        }
    }
}

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
    let session_display = format!("({})", session.name);
    if client.prefix_active {
        tty.set_cell_attrs(&CellContent {
            fg: Color::Palette(3), // yellow
            bg: config.status_bg,
            ..CellContent::default()
        });
    }
    tty.write_str(&session_display);
    pos += session_display.len();

    // Copy mode indicator
    if client.mode == ClientMode::CopyMode {
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
        let zoom_indicator = if window.zoomed.is_some() { " (Z)" } else { "" };
        let entry = format!("{}:{}{}", window.idx, window.name, zoom_indicator);

        if pos + entry.len() + 1 >= sx as usize {
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

        tty.write_str(&entry);
        pos += entry.len();

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
pub(crate) fn clear_dirty(state: &mut State, cid: ClientId) {
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
