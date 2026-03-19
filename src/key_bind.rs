use std::time::Instant;

use crate::config::{Action, Config};
use crate::keys::{InputEvent, KeyCode, MouseEvent};
use crate::state::{ClientId, ClientMode, PaneId, PromptAction, State, WindowId};

/// Result of processing input for a client.
pub enum InputResult {
    /// No action needed.
    None,
    /// Data to write to the active pane's PTY.
    PtyWrite(PaneId, Vec<u8>),
    /// Client should detach.
    Detach,
    /// Needs a full redraw.
    Redraw,
    /// A new pane was created — server must register its PTY with mio.
    NewPane(PaneId),
    /// Status message to display.
    StatusMessage(String),
    /// Multiple results.
    Multi(Vec<InputResult>),
}

/// Process an input event for a client. Returns what action to take.
pub fn process_input(
    state: &mut State,
    config: &Config,
    cid: ClientId,
    event: InputEvent,
) -> InputResult {
    let client = match state.clients.get(&cid) {
        Some(c) => c,
        None => return InputResult::None,
    };

    // Command prompt mode
    if client.mode == ClientMode::CommandPrompt {
        return process_prompt_input(state, config, cid, event);
    }

    // Copy mode
    if client.mode == ClientMode::CopyMode {
        return process_copy_input(state, config, cid, event);
    }

    // Check for prefix key
    let prefix = config.prefix;
    let prefix_active = client.prefix_active;

    // Check repeat deadline
    let in_repeat = client.repeat_deadline.is_some_and(|d| Instant::now() < d);

    match event {
        InputEvent::Key(key) => {
            if prefix_active {
                // In prefix mode — look up binding
                if let Some(client) = state.clients.get_mut(&cid) {
                    client.prefix_active = false;
                }

                if let Some(binding) = config.find_binding(key) {
                    let action = binding.action;
                    let repeat = binding.repeat;
                    if repeat && let Some(client) = state.clients.get_mut(&cid) {
                        let timeout = std::time::Duration::from_millis(config.repeat_time);
                        client.repeat_deadline = Some(Instant::now() + timeout);
                    }
                    return dispatch_action(state, config, cid, action);
                }

                // Unknown binding — redraw to clear prefix indicator
                return InputResult::Redraw;
            }

            // Check if this is the prefix key
            if key == prefix {
                if let Some(client) = state.clients.get_mut(&cid) {
                    client.prefix_active = true;
                }
                return InputResult::Redraw;
            }

            // In repeat window — check if key matches a repeat binding
            if in_repeat {
                if let Some(binding) = config.find_binding(key)
                    && binding.repeat
                {
                    let action = binding.action;
                    if let Some(client) = state.clients.get_mut(&cid) {
                        let timeout = std::time::Duration::from_millis(config.repeat_time);
                        client.repeat_deadline = Some(Instant::now() + timeout);
                    }
                    return dispatch_action(state, config, cid, action);
                }
                // Not a repeat binding — cancel repeat mode and forward key
                if let Some(client) = state.clients.get_mut(&cid) {
                    client.repeat_deadline = None;
                }
            }

            // Forward key to active pane
            if let Some(pid) = state.active_pane_for_client(cid) {
                let bytes = key_to_bytes(key, state, cid);
                if !bytes.is_empty() {
                    return InputResult::PtyWrite(pid, bytes);
                }
            }
            InputResult::None
        }
        InputEvent::Mouse(mouse) => process_mouse(state, config, cid, mouse),
        InputEvent::Paste(data) => {
            if let Some(pid) = state.active_pane_for_client(cid) {
                let pane = state.panes.get(&pid);
                let bracketed = pane.is_some_and(|p| {
                    p.active_screen()
                        .mode
                        .has(crate::screen::ScreenMode::BRACKETED_PASTE)
                });
                let mut buf = Vec::new();
                if bracketed {
                    buf.extend_from_slice(b"\x1b[200~");
                }
                buf.extend_from_slice(&data);
                if bracketed {
                    buf.extend_from_slice(b"\x1b[201~");
                }
                InputResult::PtyWrite(pid, buf)
            } else {
                InputResult::None
            }
        }
        InputEvent::FocusIn | InputEvent::FocusOut => {
            // Forward focus events to active pane if it requested them
            if let Some(pid) = state.active_pane_for_client(cid) {
                let pane = state.panes.get(&pid);
                let wants_focus = pane.is_some_and(|p| {
                    p.active_screen()
                        .mode
                        .has(crate::screen::ScreenMode::FOCUS_EVENTS)
                });
                if wants_focus {
                    let seq = if matches!(event, InputEvent::FocusIn) {
                        b"\x1b[I".to_vec()
                    } else {
                        b"\x1b[O".to_vec()
                    };
                    return InputResult::PtyWrite(pid, seq);
                }
            }
            InputResult::None
        }
    }
}

fn dispatch_action(
    state: &mut State,
    config: &Config,
    cid: ClientId,
    action: Action,
) -> InputResult {
    match action {
        Action::Detach => InputResult::Detach,
        Action::SendPrefix => {
            if let Some(pid) = state.active_pane_for_client(cid) {
                let bytes = key_to_bytes(config.prefix, state, cid);
                return InputResult::PtyWrite(pid, bytes);
            }
            InputResult::None
        }
        Action::NewWindow => {
            if let Some(client) = state.clients.get_mut(&cid) {
                client.mode = ClientMode::CommandPrompt;
                client.prompt_buf = Some(String::new());
                client.prompt_action = Some(PromptAction::NewWindow);
            }
            InputResult::Redraw
        }
        Action::RenameWindow => {
            if let Some(client) = state.clients.get_mut(&cid) {
                client.mode = ClientMode::CommandPrompt;
                client.prompt_buf = Some(String::new());
                client.prompt_action = Some(PromptAction::RenameWindow);
            }
            InputResult::Redraw
        }
        Action::NextWindow => {
            navigate_window(state, cid, 1);
            InputResult::Redraw
        }
        Action::PrevWindow => {
            navigate_window(state, cid, -1);
            InputResult::Redraw
        }
        Action::SelectWindow(idx) => {
            select_window_by_idx(state, cid, idx as u32);
            InputResult::Redraw
        }
        Action::SwapWindowLeft => {
            swap_window(state, cid, -1);
            InputResult::Redraw
        }
        Action::SwapWindowRight => {
            swap_window(state, cid, 1);
            InputResult::Redraw
        }
        Action::SplitH => {
            if let Some(pid) = split_pane(state, cid, crate::layout::SplitDir::Horizontal) {
                clear_client_screen(state, cid);
                mark_all_dirty(state);
                InputResult::Multi(vec![InputResult::NewPane(pid), InputResult::Redraw])
            } else {
                InputResult::None
            }
        }
        Action::SplitV => {
            if let Some(pid) = split_pane(state, cid, crate::layout::SplitDir::Vertical) {
                clear_client_screen(state, cid);
                mark_all_dirty(state);
                InputResult::Multi(vec![InputResult::NewPane(pid), InputResult::Redraw])
            } else {
                InputResult::None
            }
        }
        Action::KillPane => {
            kill_active_pane(state, cid);
            InputResult::Redraw
        }
        Action::ZoomPane => {
            toggle_zoom(state, cid);
            InputResult::Redraw
        }
        Action::FocusUp => {
            focus_direction(state, cid, 0, -1);
            InputResult::Redraw
        }
        Action::FocusDown => {
            focus_direction(state, cid, 0, 1);
            InputResult::Redraw
        }
        Action::FocusLeft => {
            focus_direction(state, cid, -1, 0);
            InputResult::Redraw
        }
        Action::FocusRight => {
            focus_direction(state, cid, 1, 0);
            InputResult::Redraw
        }
        Action::ReloadConfig => InputResult::StatusMessage("configuration reloaded".to_string()),
        Action::CommandPrompt => {
            if let Some(client) = state.clients.get_mut(&cid) {
                client.mode = ClientMode::CommandPrompt;
                client.prompt_buf = Some(String::new());
                client.prompt_action = Some(PromptAction::Command);
            }
            InputResult::Redraw
        }
        Action::BreakPane => {
            break_pane(state, cid);
            InputResult::Redraw
        }
        Action::MovePaneToWindow => {
            if let Some(client) = state.clients.get_mut(&cid) {
                client.mode = ClientMode::CommandPrompt;
                client.prompt_buf = Some(String::new());
                client.prompt_action = Some(PromptAction::MovePane);
            }
            InputResult::Redraw
        }
        Action::CopyMode => {
            if let Some(client) = state.clients.get_mut(&cid) {
                client.mode = ClientMode::CopyMode;
            }
            InputResult::Redraw
        }
        Action::SelectPane(_) => InputResult::None,
    }
}

fn clear_client_screen(state: &State, cid: ClientId) {
    if let Some(client) = state.clients.get(&cid) {
        let mut tty = crate::tty::TtyWriter::new();
        tty.clear_screen();
        tty.flush_to(client.tty_fd).ok();
    }
}

fn mark_all_dirty(state: &mut State) {
    for pane in state.panes.values_mut() {
        pane.active_screen_mut().grid.mark_all_dirty();
        pane.flags |= crate::state::PaneFlags::REDRAW;
    }
}

fn navigate_window(state: &mut State, cid: ClientId, delta: i32) {
    let Some(client) = state.clients.get(&cid) else {
        return;
    };
    let sid = client.session;
    let Some(session) = state.sessions.get(&sid) else {
        return;
    };
    let current_idx = session
        .windows
        .iter()
        .position(|&w| w == session.active_window);
    let Some(current_idx) = current_idx else {
        return;
    };
    let n = session.windows.len() as i32;
    let new_idx = ((current_idx as i32 + delta) % n + n) % n;
    let new_wid = session.windows[new_idx as usize];
    if let Some(session) = state.sessions.get_mut(&sid) {
        session.active_window = new_wid;
    }
    mark_all_dirty(state);
}

fn select_window_by_idx(state: &mut State, cid: ClientId, idx: u32) {
    let Some(client) = state.clients.get(&cid) else {
        return;
    };
    let sid = client.session;
    let Some(session) = state.sessions.get(&sid) else {
        return;
    };
    for &wid in &session.windows {
        if let Some(w) = state.windows.get(&wid)
            && w.idx == idx
        {
            if let Some(session) = state.sessions.get_mut(&sid) {
                session.active_window = wid;
            }
            mark_all_dirty(state);
            return;
        }
    }
}

fn swap_window(state: &mut State, cid: ClientId, delta: i32) {
    let Some(client) = state.clients.get(&cid) else {
        return;
    };
    let sid = client.session;
    let Some(session) = state.sessions.get_mut(&sid) else {
        return;
    };
    let current_idx = session
        .windows
        .iter()
        .position(|&w| w == session.active_window);
    let Some(current_idx) = current_idx else {
        return;
    };
    let n = session.windows.len() as i32;
    let new_idx = ((current_idx as i32 + delta) % n + n) % n;
    session.windows.swap(current_idx, new_idx as usize);
    state.renumber_windows(sid);
}

pub fn split_pane(
    state: &mut State,
    cid: ClientId,
    dir: crate::layout::SplitDir,
) -> Option<PaneId> {
    let wid = state.active_window_for_client(cid)?;
    let pid = state.active_pane_for_client(cid)?;

    let cwd = state.panes.get(&pid).and_then(|p| p.cwd.clone());
    let new_pid = state.alloc_pane_id();

    let socket_path = crate::protocol::socket_path();
    let (sx, sy) = state
        .panes
        .get(&pid)
        .map(|p| (p.sx, p.sy))
        .unwrap_or((80, 24));

    let (master, child_pid) = crate::pty::spawn_shell(
        sx,
        sy,
        cwd.as_deref(),
        &socket_path,
        std::process::id(),
        new_pid.0,
    )
    .ok()?;

    let pane = crate::state::Pane::new(new_pid, master, child_pid, sx, sy);
    state.panes.insert(new_pid, pane);

    let window = state.windows.get_mut(&wid)?;
    window.layout.split_pane(pid, new_pid, dir);
    window.panes.push(new_pid);
    window.active_pane = new_pid;

    if let Some(pane) = state.panes.get_mut(&new_pid) {
        pane.window = wid;
    }

    recalc_layout(state, wid);
    Some(new_pid)
}

pub fn recalc_layout(state: &mut State, wid: WindowId) {
    let Some(window) = state.windows.get(&wid) else {
        return;
    };
    let geos = window.layout.calculate(0, 0, window.sx, window.sy);
    for geo in &geos {
        if let Some(pane) = state.panes.get_mut(&geo.id) {
            let changed = pane.sx != geo.sx || pane.sy != geo.sy;
            pane.xoff = geo.xoff;
            pane.yoff = geo.yoff;
            pane.sx = geo.sx;
            pane.sy = geo.sy;
            if changed {
                pane.screen.resize(geo.sx, geo.sy);
                pane.alt_screen.resize(geo.sx, geo.sy);
                let _ = crate::sys::set_winsize(pane.pty_master, geo.sx, geo.sy);
                unsafe {
                    libc::killpg(pane.pid, libc::SIGWINCH);
                }
            }
            pane.flags |= crate::state::PaneFlags::REDRAW;
        }
    }
}

fn kill_active_pane(state: &mut State, cid: ClientId) {
    let Some(pid) = state.active_pane_for_client(cid) else {
        return;
    };
    let Some(pane) = state.panes.get(&pid) else {
        return;
    };
    // Send SIGHUP to the pane process
    unsafe {
        libc::kill(pane.pid, libc::SIGHUP);
    }
}

fn toggle_zoom(state: &mut State, cid: ClientId) {
    let Some(wid) = state.active_window_for_client(cid) else {
        return;
    };
    let Some(window) = state.windows.get_mut(&wid) else {
        return;
    };

    if window.zoomed.is_some() {
        // Unzoom
        window.zoomed = None;
    } else {
        // Zoom active pane
        window.zoomed = Some(window.active_pane);
    }

    recalc_layout_or_zoom(state, wid);
}

pub fn recalc_layout_or_zoom(state: &mut State, wid: WindowId) {
    let Some(window) = state.windows.get(&wid) else {
        return;
    };
    let sx = window.sx;
    let sy = window.sy;

    if let Some(zoomed_pid) = window.zoomed {
        // Zoomed: give the zoomed pane full window size
        if let Some(pane) = state.panes.get_mut(&zoomed_pid) {
            let changed = pane.sx != sx || pane.sy != sy;
            pane.xoff = 0;
            pane.yoff = 0;
            pane.sx = sx;
            pane.sy = sy;
            if changed {
                pane.screen.resize(sx, sy);
                pane.alt_screen.resize(sx, sy);
                let _ = crate::sys::set_winsize(pane.pty_master, sx, sy);
            }
            pane.flags |= crate::state::PaneFlags::REDRAW;
        }
    } else {
        recalc_layout(state, wid);
    }
}

fn focus_direction(state: &mut State, cid: ClientId, dx: i32, dy: i32) {
    let Some(wid) = state.active_window_for_client(cid) else {
        return;
    };
    let Some(pid) = state.active_pane_for_client(cid) else {
        return;
    };
    let Some(window) = state.windows.get(&wid) else {
        return;
    };

    let geos = window.layout.calculate(0, 0, window.sx, window.sy);
    let current = geos.iter().find(|g| g.id == pid);
    let Some(current) = current else { return };

    let cx = current.xoff as i32 + current.sx as i32 / 2;
    let cy = current.yoff as i32 + current.sy as i32 / 2;

    let mut best = None;
    let mut best_dist = i32::MAX;

    for geo in &geos {
        if geo.id == pid {
            continue;
        }
        let gx = geo.xoff as i32 + geo.sx as i32 / 2;
        let gy = geo.yoff as i32 + geo.sy as i32 / 2;

        // Check direction
        let in_direction = if dx > 0 {
            gx > cx
        } else if dx < 0 {
            gx < cx
        } else if dy > 0 {
            gy > cy
        } else {
            gy < cy
        };

        if !in_direction {
            continue;
        }

        let dist = (gx - cx).abs() + (gy - cy).abs();
        if dist < best_dist {
            best_dist = dist;
            best = Some(geo.id);
        }
    }

    if let Some(new_pid) = best
        && let Some(window) = state.windows.get_mut(&wid)
    {
        window.active_pane = new_pid;
    }
}

fn break_pane(state: &mut State, cid: ClientId) {
    let Some(pid) = state.active_pane_for_client(cid) else {
        return;
    };
    let Some(client) = state.clients.get(&cid) else {
        return;
    };
    let sid = client.session;
    let Some(old_wid) = state.active_window_for_client(cid) else {
        return;
    };

    // Remove pane from current window
    if let Some(window) = state.windows.get_mut(&old_wid) {
        window.panes.retain(|&p| p != pid);
        window.layout.remove_pane(pid);
        window.layout.simplify();
        if window.active_pane == pid {
            window.active_pane = *window.panes.first().unwrap_or(&PaneId(0));
        }
    }

    // Create new window with this pane
    let new_wid = state.alloc_window_id();
    let Some(session) = state.sessions.get_mut(&sid) else {
        return;
    };
    let idx = session.next_window_idx;
    session.next_window_idx += 1;

    let Some(client) = state.clients.get(&cid) else {
        return;
    };
    let sx = client.sx;
    let sy = client.sy.saturating_sub(1);

    let window = crate::state::Window {
        id: new_wid,
        idx,
        name: String::from("bash"),
        active_pane: pid,
        panes: vec![pid],
        sx,
        sy,
        zoomed: None,
        session: sid,
        layout: crate::layout::LayoutNode::Pane(pid),
    };
    state.windows.insert(new_wid, window);

    if let Some(session) = state.sessions.get_mut(&sid) {
        session.windows.push(new_wid);
        session.active_window = new_wid;
    }
    if let Some(pane) = state.panes.get_mut(&pid) {
        pane.window = new_wid;
    }

    state.renumber_windows(sid);
    recalc_layout(state, old_wid);
    recalc_layout(state, new_wid);
}

fn process_prompt_input(
    state: &mut State,
    _config: &Config,
    cid: ClientId,
    event: InputEvent,
) -> InputResult {
    let InputEvent::Key(key) = event else {
        return InputResult::None;
    };

    let base = key.base();

    if base == KeyCode::ESCAPE {
        // Cancel prompt
        if let Some(client) = state.clients.get_mut(&cid) {
            client.mode = ClientMode::Normal;
            client.prompt_buf = None;
            client.prompt_action = None;
        }
        return InputResult::Redraw;
    }

    if base == KeyCode::ENTER {
        // Submit
        let (buf, action) = {
            let client = match state.clients.get(&cid) {
                Some(c) => c,
                None => return InputResult::None,
            };
            (
                client.prompt_buf.clone().unwrap_or_default(),
                client.prompt_action.clone(),
            )
        };

        if let Some(client) = state.clients.get_mut(&cid) {
            client.mode = ClientMode::Normal;
            client.prompt_buf = None;
            client.prompt_action = None;
        }

        return execute_prompt(state, cid, &buf, action);
    }

    if base == KeyCode::BACKSPACE {
        if let Some(client) = state.clients.get_mut(&cid)
            && let Some(buf) = &mut client.prompt_buf
        {
            buf.pop();
        }
        return InputResult::Redraw;
    }

    // Printable character
    if (0x20..0x7F).contains(&base) && !key.has_ctrl() && !key.has_meta() {
        if let Some(client) = state.clients.get_mut(&cid)
            && let Some(buf) = &mut client.prompt_buf
        {
            buf.push(base as u8 as char);
        }
        return InputResult::Redraw;
    }

    InputResult::None
}

fn execute_prompt(
    state: &mut State,
    cid: ClientId,
    input: &str,
    action: Option<PromptAction>,
) -> InputResult {
    match action {
        Some(PromptAction::NewWindow) => {
            let name = if input.is_empty() {
                "bash".to_string()
            } else {
                input.to_string()
            };
            create_new_window(state, cid, &name)
        }
        Some(PromptAction::RenameWindow) => {
            if !input.is_empty()
                && let Some(wid) = state.active_window_for_client(cid)
                && let Some(window) = state.windows.get_mut(&wid)
            {
                window.name = input.to_string();
            }
            InputResult::Redraw
        }
        Some(PromptAction::MovePane) => {
            if let Ok(idx) = input.parse::<u32>() {
                move_pane_to_window(state, cid, idx);
            }
            InputResult::Redraw
        }
        Some(PromptAction::Command) => execute_command(state, cid, input),
        None => InputResult::None,
    }
}

fn create_new_window(state: &mut State, cid: ClientId, name: &str) -> InputResult {
    let Some(client) = state.clients.get(&cid) else {
        return InputResult::None;
    };
    let sid = client.session;
    let sx = client.sx;
    let sy = client.sy.saturating_sub(1);

    let pid = state.alloc_pane_id();
    let socket_path = crate::protocol::socket_path();

    let cwd = state
        .active_pane_for_client(cid)
        .and_then(|p| state.panes.get(&p))
        .and_then(|p| p.cwd.clone());

    let (master, child_pid) = match crate::pty::spawn_shell(
        sx,
        sy,
        cwd.as_deref(),
        &socket_path,
        std::process::id(),
        pid.0,
    ) {
        Ok(r) => r,
        Err(_) => return InputResult::StatusMessage("failed to spawn shell".to_string()),
    };

    let pane = crate::state::Pane::new(pid, master, child_pid, sx, sy);
    state.panes.insert(pid, pane);

    let wid = state.alloc_window_id();
    let Some(session) = state.sessions.get_mut(&sid) else {
        return InputResult::None;
    };
    let idx = session.next_window_idx;
    session.next_window_idx += 1;

    let window = crate::state::Window {
        id: wid,
        idx,
        name: name.to_string(),
        active_pane: pid,
        panes: vec![pid],
        sx,
        sy,
        zoomed: None,
        session: sid,
        layout: crate::layout::LayoutNode::Pane(pid),
    };
    state.windows.insert(wid, window);

    if let Some(pane) = state.panes.get_mut(&pid) {
        pane.window = wid;
    }

    if let Some(session) = state.sessions.get_mut(&sid) {
        session.windows.push(wid);
        session.active_window = wid;
    }

    state.renumber_windows(sid);
    InputResult::Multi(vec![InputResult::NewPane(pid), InputResult::Redraw])
}

fn move_pane_to_window(state: &mut State, cid: ClientId, target_idx: u32) {
    let Some(pid) = state.active_pane_for_client(cid) else {
        return;
    };
    let Some(client) = state.clients.get(&cid) else {
        return;
    };
    let sid = client.session;
    let Some(old_wid) = state.active_window_for_client(cid) else {
        return;
    };

    // Find target window
    let Some(session) = state.sessions.get(&sid) else {
        return;
    };
    let target_wid = session.windows.iter().find_map(|&wid| {
        state
            .windows
            .get(&wid)
            .and_then(|w| if w.idx == target_idx { Some(wid) } else { None })
    });
    let Some(target_wid) = target_wid else { return };
    if target_wid == old_wid {
        return;
    }

    // Remove from old window
    if let Some(window) = state.windows.get_mut(&old_wid) {
        window.panes.retain(|&p| p != pid);
        window.layout.remove_pane(pid);
        window.layout.simplify();
        if window.active_pane == pid {
            window.active_pane = *window.panes.first().unwrap_or(&PaneId(0));
        }
    }

    // Add to target window
    if let Some(window) = state.windows.get_mut(&target_wid) {
        window.panes.push(pid);
        window
            .layout
            .split_pane(window.active_pane, pid, crate::layout::SplitDir::Horizontal);
        window.active_pane = pid;
    }

    if let Some(pane) = state.panes.get_mut(&pid) {
        pane.window = target_wid;
    }

    recalc_layout(state, old_wid);
    recalc_layout(state, target_wid);
}

fn execute_command(state: &mut State, cid: ClientId, input: &str) -> InputResult {
    let parts: Vec<&str> = input.split_whitespace().collect();
    match parts.first().copied() {
        Some("rename-window") => {
            let name = parts.get(1).unwrap_or(&"");
            if !name.is_empty()
                && let Some(wid) = state.active_window_for_client(cid)
                && let Some(window) = state.windows.get_mut(&wid)
            {
                window.name = name.to_string();
            }
            InputResult::Redraw
        }
        Some("new-window") => {
            let name = if parts.len() >= 3 && parts[1] == "-n" {
                parts[2].to_string()
            } else {
                "bash".to_string()
            };
            create_new_window(state, cid, &name)
        }
        _ => InputResult::StatusMessage(format!("unknown command: {input}")),
    }
}

const SCROLL_LINES: u32 = 3;

fn enter_copy_mode(state: &mut State, cid: ClientId, pane_id: PaneId) {
    if let Some(client) = state.clients.get_mut(&cid) {
        client.mode = ClientMode::CopyMode;
        client.copy_oy = 0;
        client.copy_pane = pane_id;
    }
}

fn exit_copy_mode(state: &mut State, cid: ClientId) {
    if let Some(client) = state.clients.get_mut(&cid) {
        client.mode = ClientMode::Normal;
        client.copy_oy = 0;
        client.sel = None;
    }
    mark_all_dirty(state);
    clear_client_screen(state, cid);
}

fn copy_scroll(state: &mut State, cid: ClientId, delta: i32) -> InputResult {
    let Some(client) = state.clients.get(&cid) else {
        return InputResult::None;
    };
    let pane_id = client.copy_pane;
    let oy = client.copy_oy;

    let max_oy = state
        .panes
        .get(&pane_id)
        .map(|p| p.active_screen().grid.hsize())
        .unwrap_or(0);

    let new_oy = if delta > 0 {
        // Scroll up (into history)
        oy.saturating_add(delta as u32).min(max_oy)
    } else {
        // Scroll down (toward live)
        oy.saturating_sub((-delta) as u32)
    };

    if new_oy == 0 {
        exit_copy_mode(state, cid);
    } else if let Some(client) = state.clients.get_mut(&cid) {
        client.copy_oy = new_oy;
    }

    InputResult::Redraw
}

fn process_copy_input(
    state: &mut State,
    config: &Config,
    cid: ClientId,
    event: InputEvent,
) -> InputResult {
    match event {
        InputEvent::Key(_) => {
            exit_copy_mode(state, cid);
            InputResult::Redraw
        }
        InputEvent::Mouse(MouseEvent::WheelUp { .. }) => {
            copy_scroll(state, cid, SCROLL_LINES as i32)
        }
        InputEvent::Mouse(MouseEvent::WheelDown { .. }) => {
            copy_scroll(state, cid, -(SCROLL_LINES as i32))
        }
        InputEvent::Mouse(mouse) => {
            // Delegate press/drag/release to normal mouse handler for selection
            process_mouse(state, config, cid, mouse)
        }
        _ => InputResult::None,
    }
}

fn process_mouse(
    state: &mut State,
    _config: &Config,
    cid: ClientId,
    mouse: MouseEvent,
) -> InputResult {
    match mouse {
        MouseEvent::Press { button: 0, x, y } => {
            let Some(client) = state.clients.get(&cid) else {
                return InputResult::None;
            };
            let status_row = client.sy.saturating_sub(1);

            // Click on status bar — switch window
            if y == status_row {
                return click_status_bar(state, cid, x);
            }

            // Click on pane area — focus pane + start selection
            let Some(wid) = state.active_window_for_client(cid) else {
                return InputResult::None;
            };
            let Some(window) = state.windows.get(&wid) else {
                return InputResult::None;
            };
            let geos = window.layout.calculate(0, 0, window.sx, window.sy);
            if let Some(pid) = crate::layout::LayoutNode::pane_at(&window.layout, &geos, x, y) {
                if let Some(window) = state.windows.get_mut(&wid)
                    && window.active_pane != pid
                {
                    window.active_pane = pid;
                }
                if let Some(pane) = state.panes.get(&pid) {
                    let local_col = x.saturating_sub(pane.xoff);
                    let local_row = y.saturating_sub(pane.yoff);
                    let oy = state.clients.get(&cid).map_or(0, |c| c.copy_oy);
                    let abs_row = pane.active_screen().grid.hsize().saturating_sub(oy) + local_row;
                    if let Some(client) = state.clients.get_mut(&cid) {
                        client.sel = Some(crate::state::Selection {
                            pane: pid,
                            start_col: local_col,
                            start_row: abs_row,
                            end_col: local_col,
                            end_row: abs_row,
                        });
                    }
                    return InputResult::Redraw;
                }
            }
            InputResult::None
        }
        MouseEvent::Drag { button: 0, x, y } => {
            // Extend selection — enter copy mode on first drag
            let Some(client) = state.clients.get(&cid) else {
                return InputResult::None;
            };
            let Some(sel) = client.sel else {
                return InputResult::None;
            };
            let pid = sel.pane;

            // Enter copy mode if not already in it
            if client.mode != ClientMode::CopyMode && !pane_wants_mouse(state, pid) {
                enter_copy_mode(state, cid, pid);
            }

            if let Some(pane) = state.panes.get(&pid) {
                let local_col = x.saturating_sub(pane.xoff).min(pane.sx.saturating_sub(1));
                let local_row = y.saturating_sub(pane.yoff).min(pane.sy.saturating_sub(1));
                let oy = state.clients.get(&cid).map_or(0, |c| c.copy_oy);
                let abs_row = pane.active_screen().grid.hsize().saturating_sub(oy) + local_row;
                if let Some(client) = state.clients.get_mut(&cid)
                    && let Some(sel) = &mut client.sel
                {
                    sel.end_col = local_col;
                    sel.end_row = abs_row;
                }
                return InputResult::Redraw;
            }
            InputResult::None
        }
        MouseEvent::Release { .. } => {
            // End selection — extract text and send to clipboard via OSC 52
            let Some(client) = state.clients.get(&cid) else {
                return InputResult::None;
            };
            let Some(sel) = client.sel else {
                return InputResult::None;
            };
            let tty_fd = client.tty_fd;
            let pid = sel.pane;

            // Extract selected text
            let text = extract_selection(state, pid, &sel);

            // Clear selection and exit copy mode (full redraw)
            let was_copy = state
                .clients
                .get(&cid)
                .is_some_and(|c| c.mode == ClientMode::CopyMode);
            if let Some(client) = state.clients.get_mut(&cid) {
                client.sel = None;
            }
            if was_copy {
                exit_copy_mode(state, cid);
            } else {
                // Even without copy mode, need full redraw to clear highlight
                mark_all_dirty(state);
                clear_client_screen(state, cid);
            }

            if !text.is_empty() {
                // Send to clipboard via OSC 52
                let b64 = base64_encode(text.as_bytes());
                let osc = format!("\x1b]52;c;{b64}\x07");
                unsafe {
                    libc::write(tty_fd, osc.as_ptr() as *const libc::c_void, osc.len());
                }
            }
            InputResult::Redraw
        }
        MouseEvent::WheelUp { x, y } => {
            let Some(pid) = find_pane_at(state, cid, x, y) else {
                return InputResult::None;
            };
            // If pane wants mouse events (alt screen, or mouse tracking), forward
            if pane_wants_mouse(state, pid) {
                return forward_mouse_to_pane(state, pid, &mouse);
            }
            // Normal screen without mouse tracking: enter copy mode and scroll
            enter_copy_mode(state, cid, pid);
            copy_scroll(state, cid, SCROLL_LINES as i32)
        }
        MouseEvent::WheelDown { x, y } => {
            let Some(pid) = find_pane_at(state, cid, x, y) else {
                return InputResult::None;
            };
            if pane_wants_mouse(state, pid) {
                return forward_mouse_to_pane(state, pid, &mouse);
            }
            InputResult::None
        }
        _ => {
            // Forward other mouse events to active pane
            if let Some(pid) = state.active_pane_for_client(cid) {
                return forward_mouse_to_pane(state, pid, &mouse);
            }
            InputResult::None
        }
    }
}

pub fn extract_selection(state: &State, pid: PaneId, sel: &crate::state::Selection) -> String {
    let Some(pane) = state.panes.get(&pid) else {
        return String::new();
    };
    let grid = &pane.active_screen().grid;
    let ((sc, sr), (ec, er)) = sel.ordered();
    let mut text = String::new();

    for abs_row in sr..=er {
        let Some(line) = grid.line(abs_row) else {
            continue;
        };
        let col_start = if abs_row == sr { sc } else { 0 };
        let col_end = if abs_row == er {
            ec + 1
        } else {
            line.compact.len() as u32
        };

        for col in col_start..col_end.min(line.compact.len() as u32) {
            let cell = line.get_cell(col);
            if cell.ch[0] == 0 || (cell.ch_len == 1 && cell.ch[0] == b' ') {
                text.push(' ');
            } else {
                text.push_str(cell.ch_str());
            }
        }

        // Trim trailing spaces on each line
        let trimmed = text.trim_end_matches(' ').len();
        text.truncate(trimmed);

        if abs_row < er {
            // Add newline between lines, but not for wrapped lines
            if !line.flags.has(crate::grid::LineFlags::WRAPPED) {
                text.push('\n');
            }
        }
    }
    text
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn click_status_bar(state: &mut State, cid: ClientId, x: u32) -> InputResult {
    let Some(client) = state.clients.get(&cid) else {
        return InputResult::None;
    };
    let sid = client.session;
    let Some(session) = state.sessions.get(&sid) else {
        return InputResult::None;
    };

    // Reconstruct the status bar layout to find which window label was clicked.
    // Format: "(session_name) 1:name 2:name ..."
    let session_display = format!("({})", session.name);
    let mut pos = session_display.len() + 1; // +1 for space

    for &wid in &session.windows {
        let Some(window) = state.windows.get(&wid) else {
            continue;
        };
        let zoom = if window.zoomed.is_some() { " (Z)" } else { "" };
        let entry = format!("{}:{}{}", window.idx, window.name, zoom);
        let entry_end = pos + entry.len();

        if (x as usize) >= pos && (x as usize) < entry_end {
            // Clicked on this window
            if let Some(session) = state.sessions.get_mut(&sid) {
                session.active_window = wid;
            }
            // Full redraw for window switch
            for pane in state.panes.values_mut() {
                pane.active_screen_mut().grid.mark_all_dirty();
                pane.flags |= crate::state::PaneFlags::REDRAW;
            }
            return InputResult::Redraw;
        }
        pos = entry_end + 1; // +1 for space
    }
    InputResult::None
}

fn pane_wants_mouse(state: &State, pid: PaneId) -> bool {
    state.panes.get(&pid).is_some_and(|p| {
        let mode = p.active_screen().mode;
        p.is_alt_screen()
            || mode.has(crate::screen::ScreenMode::MOUSE_BUTTON)
            || mode.has(crate::screen::ScreenMode::MOUSE_ANY)
    })
}

fn find_pane_at(state: &State, cid: ClientId, x: u32, y: u32) -> Option<PaneId> {
    let wid = state.active_window_for_client(cid)?;
    let window = state.windows.get(&wid)?;
    let geos = window.layout.calculate(0, 0, window.sx, window.sy);
    crate::layout::LayoutNode::pane_at(&window.layout, &geos, x, y).or(Some(window.active_pane))
}

fn forward_mouse_to_pane(state: &State, pid: PaneId, mouse: &MouseEvent) -> InputResult {
    let Some(pane) = state.panes.get(&pid) else {
        return InputResult::None;
    };

    let has_mouse = pane
        .active_screen()
        .mode
        .has(crate::screen::ScreenMode::MOUSE_BUTTON)
        || pane
            .active_screen()
            .mode
            .has(crate::screen::ScreenMode::MOUSE_ANY);

    if !has_mouse {
        return InputResult::None;
    }

    let use_sgr = pane
        .active_screen()
        .mode
        .has(crate::screen::ScreenMode::MOUSE_SGR);

    if !use_sgr {
        return InputResult::None; // Only support SGR mode
    }

    // Translate coordinates to pane-local
    let (cb, x, y, final_ch) = match mouse {
        MouseEvent::Press { button, x, y } => {
            let lx = x.saturating_sub(pane.xoff) + 1;
            let ly = y.saturating_sub(pane.yoff) + 1;
            (*button as u32, lx, ly, 'M')
        }
        MouseEvent::Release { x, y } => {
            let lx = x.saturating_sub(pane.xoff) + 1;
            let ly = y.saturating_sub(pane.yoff) + 1;
            (0, lx, ly, 'm')
        }
        MouseEvent::Drag { button, x, y } => {
            let lx = x.saturating_sub(pane.xoff) + 1;
            let ly = y.saturating_sub(pane.yoff) + 1;
            (32 + *button as u32, lx, ly, 'M')
        }
        MouseEvent::WheelUp { x, y } => {
            let lx = x.saturating_sub(pane.xoff) + 1;
            let ly = y.saturating_sub(pane.yoff) + 1;
            (64, lx, ly, 'M')
        }
        MouseEvent::WheelDown { x, y } => {
            let lx = x.saturating_sub(pane.xoff) + 1;
            let ly = y.saturating_sub(pane.yoff) + 1;
            (65, lx, ly, 'M')
        }
    };

    let seq = format!("\x1b[<{cb};{x};{y}{final_ch}");
    InputResult::PtyWrite(pid, seq.into_bytes())
}

/// Convert a key code to bytes for writing to a PTY.
fn key_to_bytes(key: KeyCode, state: &State, cid: ClientId) -> Vec<u8> {
    let base = key.base();
    let ctrl = key.has_ctrl();
    let meta = key.has_meta();

    // Check if application cursor key mode is active
    let app_cursor = state
        .active_pane_for_client(cid)
        .and_then(|pid| state.panes.get(&pid))
        .is_some_and(|p| p.active_screen().mode.has(0x1000));

    let arrow_prefix = if app_cursor { b"\x1bO" } else { b"\x1b[" };

    let mut buf = Vec::new();

    if meta {
        buf.push(0x1B);
    }

    match base {
        KeyCode::UP => {
            buf.extend_from_slice(arrow_prefix);
            if ctrl {
                buf.extend_from_slice(b"1;5A");
            } else {
                buf.push(b'A');
            }
        }
        KeyCode::DOWN => {
            buf.extend_from_slice(arrow_prefix);
            if ctrl {
                buf.extend_from_slice(b"1;5B");
            } else {
                buf.push(b'B');
            }
        }
        KeyCode::RIGHT => {
            buf.extend_from_slice(arrow_prefix);
            if ctrl {
                buf.extend_from_slice(b"1;5C");
            } else {
                buf.push(b'C');
            }
        }
        KeyCode::LEFT => {
            buf.extend_from_slice(arrow_prefix);
            if ctrl {
                buf.extend_from_slice(b"1;5D");
            } else {
                buf.push(b'D');
            }
        }
        KeyCode::HOME => buf.extend_from_slice(b"\x1b[H"),
        KeyCode::END => buf.extend_from_slice(b"\x1b[F"),
        KeyCode::INSERT => buf.extend_from_slice(b"\x1b[2~"),
        KeyCode::DELETE => buf.extend_from_slice(b"\x1b[3~"),
        KeyCode::PAGEUP => buf.extend_from_slice(b"\x1b[5~"),
        KeyCode::PAGEDOWN => buf.extend_from_slice(b"\x1b[6~"),
        KeyCode::F1 => buf.extend_from_slice(b"\x1bOP"),
        KeyCode::F2 => buf.extend_from_slice(b"\x1bOQ"),
        KeyCode::F3 => buf.extend_from_slice(b"\x1bOR"),
        KeyCode::F4 => buf.extend_from_slice(b"\x1bOS"),
        KeyCode::F5 => buf.extend_from_slice(b"\x1b[15~"),
        KeyCode::F6 => buf.extend_from_slice(b"\x1b[17~"),
        KeyCode::F7 => buf.extend_from_slice(b"\x1b[18~"),
        KeyCode::F8 => buf.extend_from_slice(b"\x1b[19~"),
        KeyCode::F9 => buf.extend_from_slice(b"\x1b[20~"),
        KeyCode::F10 => buf.extend_from_slice(b"\x1b[21~"),
        KeyCode::F11 => buf.extend_from_slice(b"\x1b[23~"),
        KeyCode::F12 => buf.extend_from_slice(b"\x1b[24~"),
        KeyCode::ENTER => buf.push(0x0D),
        KeyCode::TAB => {
            if key.has_shift() {
                buf.extend_from_slice(b"\x1b[Z");
            } else {
                buf.push(0x09);
            }
        }
        KeyCode::BACKSPACE => buf.push(0x7F),
        KeyCode::ESCAPE => buf.push(0x1B),
        _ => {
            if ctrl && base >= b'a' as u32 && base <= b'z' as u32 {
                buf.push((base - b'a' as u32 + 1) as u8);
            } else if base < 0x80 {
                buf.push(base as u8);
            }
        }
    }

    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Action, Config};
    use crate::grid::{CellContent, LineFlags};
    use crate::keys::{InputEvent, KeyCode, MouseEvent};
    use crate::layout::LayoutNode;
    use crate::state::{
        Client, ClientId, ClientMode, Pane, PaneId, PromptAction, Selection, SessionId, State,
        Window, WindowId,
    };

    /// Build a minimal State with one session, one window, one pane, and one client.
    /// Returns (state, config, client_id, pane_id, window_id, session_id).
    fn setup() -> (State, Config, ClientId, PaneId, WindowId, SessionId) {
        let mut state = State::new();
        let config = Config::default_config();

        let pid = state.alloc_pane_id();
        let pane = Pane::new(pid, -1, 0, 80, 24);
        state.panes.insert(pid, pane);

        let sid = state.create_session("test", pid, 80, 25);
        let wid = state.sessions[&sid].active_window;

        let cid = state.alloc_client_id();
        state
            .clients
            .insert(cid, Client::new(cid, -1, -1, 80, 25, sid));

        (state, config, cid, pid, wid, sid)
    }

    // ---------------------------------------------------------------
    // Helpers for matching InputResult (the enum has no PartialEq)
    // ---------------------------------------------------------------

    fn is_none(r: &InputResult) -> bool {
        matches!(r, InputResult::None)
    }

    fn is_redraw(r: &InputResult) -> bool {
        matches!(r, InputResult::Redraw)
    }

    fn is_detach(r: &InputResult) -> bool {
        matches!(r, InputResult::Detach)
    }

    fn is_pty_write(r: &InputResult) -> bool {
        matches!(r, InputResult::PtyWrite(..))
    }

    fn pty_write_bytes(r: &InputResult) -> Option<(&PaneId, &Vec<u8>)> {
        match r {
            InputResult::PtyWrite(pid, data) => Some((pid, data)),
            _ => None,
        }
    }

    fn is_status_message(r: &InputResult) -> bool {
        matches!(r, InputResult::StatusMessage(..))
    }

    // =======================================================================
    // 1. Prefix key handling
    // =======================================================================

    #[test]
    fn pressing_prefix_activates_it() {
        let (mut state, config, cid, _pid, _wid, _sid) = setup();
        let prefix = config.prefix; // Ctrl-A

        let result = process_input(&mut state, &config, cid, InputEvent::Key(prefix));
        assert!(is_redraw(&result));
        assert!(state.clients[&cid].prefix_active);
    }

    #[test]
    fn bound_key_after_prefix_dispatches_action() {
        let (mut state, config, cid, _pid, _wid, _sid) = setup();

        // Activate prefix
        state.clients.get_mut(&cid).unwrap().prefix_active = true;

        // 'd' is bound to Detach
        let result = process_input(
            &mut state,
            &config,
            cid,
            InputEvent::Key(KeyCode::char('d')),
        );
        assert!(is_detach(&result));
        assert!(!state.clients[&cid].prefix_active);
    }

    #[test]
    fn unknown_key_after_prefix_clears_prefix() {
        let (mut state, config, cid, _pid, _wid, _sid) = setup();

        state.clients.get_mut(&cid).unwrap().prefix_active = true;

        // 'x' is not bound in default config
        let result = process_input(
            &mut state,
            &config,
            cid,
            InputEvent::Key(KeyCode::char('x')),
        );
        assert!(is_redraw(&result));
        assert!(!state.clients[&cid].prefix_active);
    }

    #[test]
    fn prefix_then_next_window() {
        let (mut state, config, cid, _pid, _wid, sid) = setup();

        // Add a second window so next-window has somewhere to go
        let p2 = state.alloc_pane_id();
        state.panes.insert(p2, Pane::new(p2, -1, 0, 80, 24));
        let w2 = state.alloc_window_id();
        let window2 = Window {
            id: w2,
            idx: 2,
            name: "w2".to_string(),
            active_pane: p2,
            panes: vec![p2],
            sx: 80,
            sy: 24,
            zoomed: None,
            session: sid,
            layout: LayoutNode::Pane(p2),
        };
        state.windows.insert(w2, window2);
        state.sessions.get_mut(&sid).unwrap().windows.push(w2);

        // Activate prefix then press Ctrl-Right (NextWindow)
        state.clients.get_mut(&cid).unwrap().prefix_active = true;
        let key = KeyCode(KeyCode::RIGHT | KeyCode::CTRL);
        let result = process_input(&mut state, &config, cid, InputEvent::Key(key));
        assert!(is_redraw(&result));

        // Active window should have changed
        assert_eq!(state.sessions[&sid].active_window, w2);
    }

    // =======================================================================
    // 2. Key forwarding
    // =======================================================================

    #[test]
    fn regular_key_forwarded_as_pty_write() {
        let (mut state, config, cid, pid, _wid, _sid) = setup();

        let result = process_input(
            &mut state,
            &config,
            cid,
            InputEvent::Key(KeyCode::char('a')),
        );
        let (write_pid, data) = pty_write_bytes(&result).expect("expected PtyWrite");
        assert_eq!(*write_pid, pid);
        assert_eq!(data, &vec![b'a']);
    }

    #[test]
    fn enter_forwarded_as_cr() {
        let (mut state, config, cid, pid, _wid, _sid) = setup();

        let result = process_input(
            &mut state,
            &config,
            cid,
            InputEvent::Key(KeyCode(KeyCode::ENTER)),
        );
        let (write_pid, data) = pty_write_bytes(&result).expect("expected PtyWrite");
        assert_eq!(*write_pid, pid);
        assert_eq!(data, &vec![0x0D]);
    }

    #[test]
    fn escape_forwarded() {
        let (mut state, config, cid, pid, _wid, _sid) = setup();

        let result = process_input(
            &mut state,
            &config,
            cid,
            InputEvent::Key(KeyCode(KeyCode::ESCAPE)),
        );
        let (write_pid, data) = pty_write_bytes(&result).expect("expected PtyWrite");
        assert_eq!(*write_pid, pid);
        assert_eq!(data, &vec![0x1B]);
    }

    #[test]
    fn arrow_key_forwarded_with_csi() {
        let (mut state, config, cid, _pid, _wid, _sid) = setup();

        let result = process_input(
            &mut state,
            &config,
            cid,
            InputEvent::Key(KeyCode(KeyCode::UP)),
        );
        let (_write_pid, data) = pty_write_bytes(&result).expect("expected PtyWrite");
        assert_eq!(data, b"\x1b[A");
    }

    #[test]
    fn no_client_returns_none() {
        let (mut state, config, _cid, _pid, _wid, _sid) = setup();
        let bogus = ClientId(999);

        let result = process_input(
            &mut state,
            &config,
            bogus,
            InputEvent::Key(KeyCode::char('a')),
        );
        assert!(is_none(&result));
    }

    // =======================================================================
    // 3. Copy mode
    // =======================================================================

    #[test]
    fn enter_copy_mode_via_action() {
        let (mut state, config, cid, _pid, _wid, _sid) = setup();

        // Prefix + CopyMode binding doesn't exist by default, so invoke dispatch directly
        let result = dispatch_action(&mut state, &config, cid, Action::CopyMode);
        assert!(is_redraw(&result));
        assert_eq!(state.clients[&cid].mode, ClientMode::CopyMode);
    }

    #[test]
    fn exit_copy_mode_on_keypress() {
        let (mut state, config, cid, pid, _wid, _sid) = setup();

        // Enter copy mode
        enter_copy_mode(&mut state, cid, pid);
        assert_eq!(state.clients[&cid].mode, ClientMode::CopyMode);

        // Any key exits copy mode
        let result = process_input(
            &mut state,
            &config,
            cid,
            InputEvent::Key(KeyCode::char('q')),
        );
        assert!(is_redraw(&result));
        assert_eq!(state.clients[&cid].mode, ClientMode::Normal);
    }

    #[test]
    fn copy_scroll_up_increases_offset() {
        let (mut state, _config, cid, pid, _wid, _sid) = setup();

        // Add some history so scrolling has room
        let grid = &mut state.panes.get_mut(&pid).unwrap().screen.grid;
        for _ in 0..10 {
            grid.scroll_up(0, grid.sy - 1);
        }
        let hsize = state.panes[&pid].screen.grid.hsize();
        assert!(hsize >= 3, "need history for scroll test");

        enter_copy_mode(&mut state, cid, pid);

        let result = copy_scroll(&mut state, cid, 3);
        assert!(is_redraw(&result));
        assert_eq!(state.clients[&cid].copy_oy, 3);
        assert_eq!(state.clients[&cid].mode, ClientMode::CopyMode);
    }

    #[test]
    fn copy_scroll_down_to_zero_exits_copy_mode() {
        let (mut state, _config, cid, pid, _wid, _sid) = setup();

        // Add some history
        let grid = &mut state.panes.get_mut(&pid).unwrap().screen.grid;
        for _ in 0..10 {
            grid.scroll_up(0, grid.sy - 1);
        }

        enter_copy_mode(&mut state, cid, pid);
        state.clients.get_mut(&cid).unwrap().copy_oy = 2;

        // Scroll down past 0
        let result = copy_scroll(&mut state, cid, -5);
        assert!(is_redraw(&result));
        assert_eq!(state.clients[&cid].mode, ClientMode::Normal);
        assert_eq!(state.clients[&cid].copy_oy, 0);
    }

    #[test]
    fn copy_scroll_clamped_to_max_history() {
        let (mut state, _config, cid, pid, _wid, _sid) = setup();

        // Add exactly 5 history lines
        let grid = &mut state.panes.get_mut(&pid).unwrap().screen.grid;
        for _ in 0..5 {
            grid.scroll_up(0, grid.sy - 1);
        }
        let hsize = state.panes[&pid].screen.grid.hsize();

        enter_copy_mode(&mut state, cid, pid);

        // Try to scroll far past history
        let result = copy_scroll(&mut state, cid, 1000);
        assert!(is_redraw(&result));
        assert_eq!(state.clients[&cid].copy_oy, hsize);
    }

    // =======================================================================
    // 4. Selection / extract_selection
    // =======================================================================

    #[test]
    fn extract_selection_single_line() {
        let (mut state, _config, _cid, pid, _wid, _sid) = setup();

        // Write "Hello" on the first visible line
        let grid = &mut state.panes.get_mut(&pid).unwrap().screen.grid;
        for (i, ch) in b"Hello".iter().enumerate() {
            let content = CellContent::from_ascii(*ch);
            grid.visible_line_mut(0)
                .unwrap()
                .set_cell(i as u32, &content);
        }

        let hsize = state.panes[&pid].screen.grid.hsize();
        let sel = Selection {
            pane: pid,
            start_col: 0,
            start_row: hsize,
            end_col: 4,
            end_row: hsize,
        };

        let text = extract_selection(&state, pid, &sel);
        assert_eq!(text, "Hello");
    }

    #[test]
    fn extract_selection_multi_line() {
        let (mut state, _config, _cid, pid, _wid, _sid) = setup();

        let grid = &mut state.panes.get_mut(&pid).unwrap().screen.grid;
        for (i, ch) in b"ABC".iter().enumerate() {
            grid.visible_line_mut(0)
                .unwrap()
                .set_cell(i as u32, &CellContent::from_ascii(*ch));
        }
        for (i, ch) in b"DEF".iter().enumerate() {
            grid.visible_line_mut(1)
                .unwrap()
                .set_cell(i as u32, &CellContent::from_ascii(*ch));
        }

        let hsize = state.panes[&pid].screen.grid.hsize();
        let sel = Selection {
            pane: pid,
            start_col: 0,
            start_row: hsize,
            end_col: 2,
            end_row: hsize + 1,
        };

        let text = extract_selection(&state, pid, &sel);
        assert_eq!(text, "ABC\nDEF");
    }

    #[test]
    fn extract_selection_respects_wrapped_flag() {
        let (mut state, _config, _cid, pid, _wid, _sid) = setup();

        let grid = &mut state.panes.get_mut(&pid).unwrap().screen.grid;
        for (i, ch) in b"ABCD".iter().enumerate() {
            grid.visible_line_mut(0)
                .unwrap()
                .set_cell(i as u32, &CellContent::from_ascii(*ch));
        }
        // Mark line 0 as WRAPPED
        grid.visible_line_mut(0).unwrap().flags = LineFlags(LineFlags::WRAPPED);

        for (i, ch) in b"EFGH".iter().enumerate() {
            grid.visible_line_mut(1)
                .unwrap()
                .set_cell(i as u32, &CellContent::from_ascii(*ch));
        }

        let hsize = state.panes[&pid].screen.grid.hsize();
        let sel = Selection {
            pane: pid,
            start_col: 0,
            start_row: hsize,
            end_col: 3,
            end_row: hsize + 1,
        };

        let text = extract_selection(&state, pid, &sel);
        // Wrapped lines should NOT have a newline between them
        assert_eq!(text, "ABCDEFGH");
    }

    #[test]
    fn extract_selection_trims_trailing_spaces() {
        let (mut state, _config, _cid, pid, _wid, _sid) = setup();

        let grid = &mut state.panes.get_mut(&pid).unwrap().screen.grid;
        // Write "Hi" followed by spaces (default cells are spaces)
        for (i, ch) in b"Hi".iter().enumerate() {
            grid.visible_line_mut(0)
                .unwrap()
                .set_cell(i as u32, &CellContent::from_ascii(*ch));
        }

        let hsize = state.panes[&pid].screen.grid.hsize();
        let sel = Selection {
            pane: pid,
            start_col: 0,
            start_row: hsize,
            end_col: 9, // extends past "Hi" into trailing spaces
            end_row: hsize,
        };

        let text = extract_selection(&state, pid, &sel);
        assert_eq!(text, "Hi");
    }

    #[test]
    fn extract_selection_reversed_coords_normalized() {
        let (mut state, _config, _cid, pid, _wid, _sid) = setup();

        let grid = &mut state.panes.get_mut(&pid).unwrap().screen.grid;
        for (i, ch) in b"Test".iter().enumerate() {
            grid.visible_line_mut(0)
                .unwrap()
                .set_cell(i as u32, &CellContent::from_ascii(*ch));
        }

        let hsize = state.panes[&pid].screen.grid.hsize();
        // Selection is end-to-start (reversed)
        let sel = Selection {
            pane: pid,
            start_col: 3,
            start_row: hsize,
            end_col: 0,
            end_row: hsize,
        };

        let text = extract_selection(&state, pid, &sel);
        assert_eq!(text, "Test");
    }

    // =======================================================================
    // 5. Base64 encoding
    // =======================================================================

    #[test]
    fn base64_encode_empty() {
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn base64_encode_known_vectors() {
        // Standard test vectors
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_encode_hello() {
        assert_eq!(base64_encode(b"Hello, World!"), "SGVsbG8sIFdvcmxkIQ==");
    }

    // =======================================================================
    // 6. Window navigation
    // =======================================================================

    fn setup_multi_window() -> (State, Config, ClientId, SessionId, Vec<WindowId>) {
        let mut state = State::new();
        let config = Config::default_config();

        let p1 = state.alloc_pane_id();
        state.panes.insert(p1, Pane::new(p1, -1, 0, 80, 24));
        let sid = state.create_session("test", p1, 80, 25);
        let w1 = state.sessions[&sid].active_window;

        let p2 = state.alloc_pane_id();
        state.panes.insert(p2, Pane::new(p2, -1, 0, 80, 24));
        let w2 = state.alloc_window_id();
        state.windows.insert(
            w2,
            Window {
                id: w2,
                idx: 2,
                name: "w2".to_string(),
                active_pane: p2,
                panes: vec![p2],
                sx: 80,
                sy: 24,
                zoomed: None,
                session: sid,
                layout: LayoutNode::Pane(p2),
            },
        );
        state.sessions.get_mut(&sid).unwrap().windows.push(w2);

        let p3 = state.alloc_pane_id();
        state.panes.insert(p3, Pane::new(p3, -1, 0, 80, 24));
        let w3 = state.alloc_window_id();
        state.windows.insert(
            w3,
            Window {
                id: w3,
                idx: 3,
                name: "w3".to_string(),
                active_pane: p3,
                panes: vec![p3],
                sx: 80,
                sy: 24,
                zoomed: None,
                session: sid,
                layout: LayoutNode::Pane(p3),
            },
        );
        state.sessions.get_mut(&sid).unwrap().windows.push(w3);

        let cid = state.alloc_client_id();
        state
            .clients
            .insert(cid, Client::new(cid, -1, -1, 80, 25, sid));

        (state, config, cid, sid, vec![w1, w2, w3])
    }

    #[test]
    fn navigate_window_forward() {
        let (mut state, _config, cid, sid, wins) = setup_multi_window();
        assert_eq!(state.sessions[&sid].active_window, wins[0]);

        navigate_window(&mut state, cid, 1);
        assert_eq!(state.sessions[&sid].active_window, wins[1]);

        navigate_window(&mut state, cid, 1);
        assert_eq!(state.sessions[&sid].active_window, wins[2]);
    }

    #[test]
    fn navigate_window_wraps_forward() {
        let (mut state, _config, cid, sid, wins) = setup_multi_window();

        // Go to last window
        navigate_window(&mut state, cid, 1);
        navigate_window(&mut state, cid, 1);
        assert_eq!(state.sessions[&sid].active_window, wins[2]);

        // Next should wrap to first
        navigate_window(&mut state, cid, 1);
        assert_eq!(state.sessions[&sid].active_window, wins[0]);
    }

    #[test]
    fn navigate_window_wraps_backward() {
        let (mut state, _config, cid, sid, wins) = setup_multi_window();
        assert_eq!(state.sessions[&sid].active_window, wins[0]);

        // Previous should wrap to last
        navigate_window(&mut state, cid, -1);
        assert_eq!(state.sessions[&sid].active_window, wins[2]);
    }

    #[test]
    fn select_window_by_idx_finds_correct_window() {
        let (mut state, _config, cid, sid, wins) = setup_multi_window();

        select_window_by_idx(&mut state, cid, 2);
        assert_eq!(state.sessions[&sid].active_window, wins[1]);

        select_window_by_idx(&mut state, cid, 3);
        assert_eq!(state.sessions[&sid].active_window, wins[2]);

        select_window_by_idx(&mut state, cid, 1);
        assert_eq!(state.sessions[&sid].active_window, wins[0]);
    }

    #[test]
    fn select_window_by_idx_nonexistent_is_noop() {
        let (mut state, _config, cid, sid, wins) = setup_multi_window();

        select_window_by_idx(&mut state, cid, 99);
        // Active window unchanged
        assert_eq!(state.sessions[&sid].active_window, wins[0]);
    }

    #[test]
    fn dispatch_select_window_action() {
        let (mut state, config, cid, sid, wins) = setup_multi_window();

        let result = dispatch_action(&mut state, &config, cid, Action::SelectWindow(2));
        assert!(is_redraw(&result));
        assert_eq!(state.sessions[&sid].active_window, wins[1]);
    }

    // =======================================================================
    // 7. Prompt input
    // =======================================================================

    #[test]
    fn prompt_escape_exits() {
        let (mut state, config, cid, _pid, _wid, _sid) = setup();

        // Enter prompt mode
        {
            let client = state.clients.get_mut(&cid).unwrap();
            client.mode = ClientMode::CommandPrompt;
            client.prompt_buf = Some(String::new());
            client.prompt_action = Some(PromptAction::Command);
        }

        let result = process_input(
            &mut state,
            &config,
            cid,
            InputEvent::Key(KeyCode(KeyCode::ESCAPE)),
        );
        assert!(is_redraw(&result));
        assert_eq!(state.clients[&cid].mode, ClientMode::Normal);
        assert!(state.clients[&cid].prompt_buf.is_none());
        assert!(state.clients[&cid].prompt_action.is_none());
    }

    #[test]
    fn prompt_printable_chars_append() {
        let (mut state, config, cid, _pid, _wid, _sid) = setup();

        {
            let client = state.clients.get_mut(&cid).unwrap();
            client.mode = ClientMode::CommandPrompt;
            client.prompt_buf = Some(String::new());
            client.prompt_action = Some(PromptAction::RenameWindow);
        }

        process_input(
            &mut state,
            &config,
            cid,
            InputEvent::Key(KeyCode::char('h')),
        );
        process_input(
            &mut state,
            &config,
            cid,
            InputEvent::Key(KeyCode::char('i')),
        );

        assert_eq!(state.clients[&cid].prompt_buf.as_deref(), Some("hi"));
    }

    #[test]
    fn prompt_backspace_deletes() {
        let (mut state, config, cid, _pid, _wid, _sid) = setup();

        {
            let client = state.clients.get_mut(&cid).unwrap();
            client.mode = ClientMode::CommandPrompt;
            client.prompt_buf = Some("abc".to_string());
            client.prompt_action = Some(PromptAction::RenameWindow);
        }

        process_input(
            &mut state,
            &config,
            cid,
            InputEvent::Key(KeyCode(KeyCode::BACKSPACE)),
        );

        assert_eq!(state.clients[&cid].prompt_buf.as_deref(), Some("ab"));
    }

    #[test]
    fn prompt_enter_rename_window() {
        let (mut state, config, cid, _pid, wid, _sid) = setup();

        {
            let client = state.clients.get_mut(&cid).unwrap();
            client.mode = ClientMode::CommandPrompt;
            client.prompt_buf = Some("newname".to_string());
            client.prompt_action = Some(PromptAction::RenameWindow);
        }

        let result = process_input(
            &mut state,
            &config,
            cid,
            InputEvent::Key(KeyCode(KeyCode::ENTER)),
        );
        assert!(is_redraw(&result));
        assert_eq!(state.clients[&cid].mode, ClientMode::Normal);
        assert_eq!(state.windows[&wid].name, "newname");
    }

    #[test]
    fn prompt_ctrl_keys_ignored() {
        let (mut state, config, cid, _pid, _wid, _sid) = setup();

        {
            let client = state.clients.get_mut(&cid).unwrap();
            client.mode = ClientMode::CommandPrompt;
            client.prompt_buf = Some(String::new());
            client.prompt_action = Some(PromptAction::Command);
        }

        // Ctrl-X should not add anything
        let result = process_input(
            &mut state,
            &config,
            cid,
            InputEvent::Key(KeyCode::ctrl('x')),
        );
        assert!(is_none(&result));
        assert_eq!(state.clients[&cid].prompt_buf.as_deref(), Some(""));
    }

    #[test]
    fn prompt_mouse_ignored() {
        let (mut state, config, cid, _pid, _wid, _sid) = setup();

        {
            let client = state.clients.get_mut(&cid).unwrap();
            client.mode = ClientMode::CommandPrompt;
            client.prompt_buf = Some(String::new());
            client.prompt_action = Some(PromptAction::Command);
        }

        let result = process_input(
            &mut state,
            &config,
            cid,
            InputEvent::Mouse(MouseEvent::Press {
                button: 0,
                x: 5,
                y: 5,
            }),
        );
        assert!(is_none(&result));
    }

    // =======================================================================
    // Additional dispatch tests
    // =======================================================================

    #[test]
    fn dispatch_command_prompt_enters_prompt_mode() {
        let (mut state, config, cid, _pid, _wid, _sid) = setup();

        let result = dispatch_action(&mut state, &config, cid, Action::CommandPrompt);
        assert!(is_redraw(&result));
        assert_eq!(state.clients[&cid].mode, ClientMode::CommandPrompt);
        assert!(state.clients[&cid].prompt_buf.is_some());
        assert!(matches!(
            state.clients[&cid].prompt_action,
            Some(PromptAction::Command)
        ));
    }

    #[test]
    fn dispatch_reload_config_returns_status_message() {
        let (mut state, config, cid, _pid, _wid, _sid) = setup();

        let result = dispatch_action(&mut state, &config, cid, Action::ReloadConfig);
        assert!(is_status_message(&result));
    }

    #[test]
    fn dispatch_new_window_enters_prompt() {
        let (mut state, config, cid, _pid, _wid, _sid) = setup();

        let result = dispatch_action(&mut state, &config, cid, Action::NewWindow);
        assert!(is_redraw(&result));
        assert_eq!(state.clients[&cid].mode, ClientMode::CommandPrompt);
        assert!(matches!(
            state.clients[&cid].prompt_action,
            Some(PromptAction::NewWindow)
        ));
    }

    #[test]
    fn dispatch_rename_window_enters_prompt() {
        let (mut state, config, cid, _pid, _wid, _sid) = setup();

        let result = dispatch_action(&mut state, &config, cid, Action::RenameWindow);
        assert!(is_redraw(&result));
        assert_eq!(state.clients[&cid].mode, ClientMode::CommandPrompt);
        assert!(matches!(
            state.clients[&cid].prompt_action,
            Some(PromptAction::RenameWindow)
        ));
    }

    #[test]
    fn swap_window_reorders() {
        let (mut state, _config, cid, sid, wins) = setup_multi_window();

        // Swap right from first window
        swap_window(&mut state, cid, 1);
        let session = &state.sessions[&sid];
        // w1 and w2 should be swapped in the windows list
        assert_eq!(session.windows[0], wins[1]);
        assert_eq!(session.windows[1], wins[0]);
    }

    // =======================================================================
    // key_to_bytes
    // =======================================================================

    #[test]
    fn key_to_bytes_ctrl_char() {
        let (state, _config, cid, _pid, _wid, _sid) = setup();
        let bytes = key_to_bytes(KeyCode::ctrl('c'), &state, cid);
        assert_eq!(bytes, vec![3]); // Ctrl-C = 0x03
    }

    #[test]
    fn key_to_bytes_backspace() {
        let (state, _config, cid, _pid, _wid, _sid) = setup();
        let bytes = key_to_bytes(KeyCode(KeyCode::BACKSPACE), &state, cid);
        assert_eq!(bytes, vec![0x7F]);
    }

    #[test]
    fn key_to_bytes_tab() {
        let (state, _config, cid, _pid, _wid, _sid) = setup();
        let bytes = key_to_bytes(KeyCode(KeyCode::TAB), &state, cid);
        assert_eq!(bytes, vec![0x09]);
    }

    #[test]
    fn key_to_bytes_shift_tab() {
        let (state, _config, cid, _pid, _wid, _sid) = setup();
        let bytes = key_to_bytes(KeyCode(KeyCode::TAB | KeyCode::SHIFT), &state, cid);
        assert_eq!(bytes, b"\x1b[Z");
    }

    #[test]
    fn key_to_bytes_function_keys() {
        let (state, _config, cid, _pid, _wid, _sid) = setup();

        assert_eq!(key_to_bytes(KeyCode(KeyCode::F1), &state, cid), b"\x1bOP");
        assert_eq!(key_to_bytes(KeyCode(KeyCode::F5), &state, cid), b"\x1b[15~");
        assert_eq!(
            key_to_bytes(KeyCode(KeyCode::F12), &state, cid),
            b"\x1b[24~"
        );
    }
}
