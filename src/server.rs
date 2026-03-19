use std::collections::HashMap;
use std::os::unix::io::RawFd;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token};

use crate::config::Config;
use crate::key_bind::{self, InputResult};
use crate::keys;
use crate::protocol::{self, Message};
use crate::render;
use crate::state::{ClientId, PaneId, State};
use crate::tty::TtyWriter;
use crate::{sys, vt};

const TOKEN_LISTENER: Token = Token(0);
const TOKEN_SIGCHLD: Token = Token(1);
const TOKEN_SIGTERM: Token = Token(2);

// Dynamic tokens: clients start at 1000, panes at 100000
fn client_token(id: ClientId) -> Token {
    Token(1000 + id.0 as usize)
}
fn pane_token(id: PaneId) -> Token {
    Token(100000 + id.0 as usize)
}

/// Start server with an initial client connected via socketpair.
pub(crate) fn run_server_with_client(initial_client_fd: RawFd) -> Result<()> {
    run_server_inner(Some(initial_client_fd))
}

/// Start server (for future use — no initial client).
pub(crate) fn _run_server() -> Result<()> {
    run_server_inner(None)
}

fn run_server_inner(initial_client_fd: Option<RawFd>) -> Result<()> {
    crate::log::init();

    sys::ignore_sigpipe();

    let socket_path = protocol::socket_path();
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    // Remove stale socket
    let _ = std::fs::remove_file(&socket_path);

    // Bind Unix socket listener for subsequent clients
    let listener = std::os::unix::net::UnixListener::bind(&socket_path)
        .with_context(|| format!("binding {}", socket_path.display()))?;
    listener.set_nonblocking(true)?;
    let listener_fd = {
        use std::os::unix::io::AsRawFd;
        listener.as_raw_fd()
    };

    let sigchld_fd = sys::signal_pipe(libc::SIGCHLD).context("setting up SIGCHLD handler")?;
    let sigterm_fd = sys::signal_pipe(libc::SIGTERM).context("setting up SIGTERM handler")?;

    let mut config = Config::load();

    let mut poll = Poll::new()?;
    let mut events = Events::with_capacity(256);

    poll.registry().register(
        &mut SourceFd(&listener_fd),
        TOKEN_LISTENER,
        Interest::READABLE,
    )?;
    poll.registry().register(
        &mut SourceFd(&sigchld_fd),
        TOKEN_SIGCHLD,
        Interest::READABLE,
    )?;
    poll.registry().register(
        &mut SourceFd(&sigterm_fd),
        TOKEN_SIGTERM,
        Interest::READABLE,
    )?;

    let mut state = State::new();
    let mut tty = TtyWriter::new();

    let mut client_tokens: HashMap<Token, ClientId> = HashMap::new();
    let mut pane_tokens: HashMap<Token, PaneId> = HashMap::new();

    // Register the initial client from the socketpair (if any).
    // This client is already connected — no accept needed.
    if let Some(sock_fd) = initial_client_fd {
        register_new_connection(sock_fd, &mut state, &mut poll, &mut client_tokens)?;
    }

    let tick_interval = Duration::from_millis(16);
    let mut last_render = Instant::now();
    let mut needs_render = false;
    let mut had_session = false;

    // Reusable buffers — cleared each iteration, never reallocated
    let mut new_panes: Vec<PaneId> = Vec::new();
    let mut dead_clients: Vec<ClientId> = Vec::new();
    let mut dead_panes: Vec<PaneId> = Vec::new();
    let mut expired_msgs: Vec<ClientId> = Vec::new();
    let mut input_events: Vec<keys::InputEvent> = Vec::new();

    loop {
        let timeout = if needs_render {
            let since = last_render.elapsed();
            if since >= tick_interval {
                Some(Duration::ZERO)
            } else {
                Some(tick_interval - since)
            }
        } else {
            // Check for status message timeouts
            let mut next_timeout = Duration::from_secs(60);
            for client in state.clients.values() {
                if let Some((_, when)) = &client.status_message {
                    let elapsed = when.elapsed();
                    if elapsed >= Duration::from_secs(2) {
                        next_timeout = Duration::ZERO;
                    } else {
                        let remaining = Duration::from_secs(2) - elapsed;
                        next_timeout = next_timeout.min(remaining);
                    }
                }
            }
            Some(next_timeout)
        };

        match poll.poll(&mut events, timeout) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }

        new_panes.clear();
        dead_clients.clear();
        dead_panes.clear();
        expired_msgs.clear();
        let mut force_render = false;

        // Expire status messages (two-pass to avoid borrow conflict)
        for (cid, client) in &state.clients {
            if let Some((_, when)) = &client.status_message {
                if when.elapsed() >= Duration::from_secs(2) {
                    expired_msgs.push(*cid);
                }
            }
        }
        for cid in &expired_msgs {
            if let Some(client) = state.clients.get_mut(cid) {
                client.status_message = None;
                force_render = true;
            }
        }

        for event in events.iter() {
            match event.token() {
                TOKEN_LISTENER => {
                    // Accept new connections from the listening socket
                    loop {
                        let stream = match listener.accept() {
                            Ok((s, _)) => s,
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(_) => break,
                        };
                        let sock_fd = {
                            use std::os::unix::io::AsRawFd;
                            stream.as_raw_fd()
                        };
                        std::mem::forget(stream); // prevent close on drop

                        if register_new_connection(
                            sock_fd, &mut state, &mut poll, &mut client_tokens,
                        ).is_err() {
                            sys::close_fd(sock_fd);
                        }
                    }
                }
                TOKEN_SIGCHLD => {
                    // Drain signal pipe
                    drain_signal_pipe(sigchld_fd);

                    // Reap all dead children
                    loop {
                        let mut status: libc::c_int = 0;
                        let pid =
                            unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
                        if pid <= 0 {
                            break;
                        }
                        // Find and mark dead panes
                        for (pane_id, pane) in &state.panes {
                            if pane.pid == pid {
                                dead_panes.push(*pane_id);
                            }
                        }
                    }
                }
                TOKEN_SIGTERM => {
                    // Clean shutdown
                    cleanup(&state, &socket_path);
                    return Ok(());
                }
                token if client_tokens.contains_key(&token) => {
                    let cid = client_tokens[&token];
                    if let Err(_) = handle_client_data(
                        &mut state,
                        &mut config,
                        &mut poll,
                        cid,
                        &mut tty,
                        &mut new_panes,
                        &mut force_render,
                        &mut input_events,
                    ) {
                        dead_clients.push(cid);
                    }
                }
                token if pane_tokens.contains_key(&token) => {
                    let pid = pane_tokens[&token];
                    handle_pane_data(&mut state, pid, &mut tty, &client_tokens);
                    needs_render = true;
                }
                _ => {}
            }
        }

        // Register new pane tokens
        for pid in &new_panes {
            if let Some(pane) = state.panes.get(&pid) {
                let token = pane_token(*pid);
                poll.registry()
                    .register(
                        &mut SourceFd(&pane.pty_master),
                        token,
                        Interest::READABLE,
                    )
                    .ok();
                pane_tokens.insert(token, *pid);
            }
        }

        // Handle dead panes
        if !dead_panes.is_empty() {
            for pid in &dead_panes {
                handle_pane_death(&mut state, &mut poll, &mut pane_tokens, *pid);
            }
            // Full clear + redraw — panes resize to fill the dead pane's space
            for pane in state.panes.values_mut() {
                pane.active_screen_mut().grid.mark_all_dirty();
                pane.flags |= crate::state::PaneFlags::REDRAW;
            }
            for client in state.clients.values() {
                if client.session.0 != u32::MAX {
                    let mut w = TtyWriter::new();
                    w.clear_screen();
                    w.flush_to(client.tty_fd).ok();
                }
            }
            force_render = true;
        }

        // Handle dead clients
        for cid in &dead_clients {
            cleanup_client(&mut state, &mut poll, &mut client_tokens, *cid);
        }

        if !state.sessions.is_empty() {
            had_session = true;
        }

        // Check if all sessions are gone (only after we've had at least one)
        if had_session && state.sessions.is_empty() {
            // Detach all remaining clients
            if !state.clients.is_empty() {
                let cids: Vec<ClientId> = state.clients.keys().copied().collect();
                for cid in cids {
                    send_to_client(&state, cid, &Message::empty(protocol::MSG_EXIT));
                    cleanup_client(&mut state, &mut poll, &mut client_tokens, cid);
                }
            }
            cleanup(&state, &socket_path);
            return Ok(());
        }

        // Render on tick
        if needs_render || force_render {
            let now = Instant::now();
            if now.duration_since(last_render) >= tick_interval || force_render {
                render_all_clients(&mut state, &config, &mut tty);
                last_render = now;
                needs_render = false;
            }
        }
    }
}

/// Handle a new client connection: receive optional tty fd, register with mio.
fn register_new_connection(
    sock_fd: RawFd,
    state: &mut State,
    poll: &mut Poll,
    client_tokens: &mut HashMap<Token, ClientId>,
) -> Result<()> {
    // Ensure the socket is blocking for the initial handshake.
    // Accepted sockets may inherit non-blocking from the listener on some platforms.
    sys::set_blocking(sock_fd)?;

    // Receive the client's tty fd.
    // Returns None for non-interactive clients (ls, kill).
    let tty_fd = match protocol::recv_fd(sock_fd)? {
        Some(fd) => fd,
        None => -1,
    };

    sys::set_nonblock(sock_fd)?;

    let cid = state.alloc_client_id();
    let token = client_token(cid);

    poll.registry().register(
        &mut SourceFd(&sock_fd),
        token,
        Interest::READABLE,
    )?;

    let client = crate::state::Client::new(
        cid,
        sock_fd,
        tty_fd,
        80,
        24,
        crate::state::SessionId(u32::MAX),
    );
    state.clients.insert(cid, client);
    client_tokens.insert(token, cid);
    Ok(())
}

fn drain_signal_pipe(fd: RawFd) {
    let mut buf = [0u8; 64];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
    }
}

fn handle_client_data(
    state: &mut State,
    config: &mut Config,
    poll: &mut Poll,
    cid: ClientId,
    tty: &mut TtyWriter,
    new_panes: &mut Vec<PaneId>,
    force_render: &mut bool,
    input_events: &mut Vec<keys::InputEvent>,
) -> Result<(), ()> {
    let client = state.clients.get_mut(&cid).ok_or(())?;
    let sock_fd = client.socket_fd;

    let mut buf = [0u8; 8192];
    let n = unsafe { libc::read(sock_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            return Ok(()); // retry on next poll
        }
        return Err(());
    }
    if n == 0 {
        return Err(());
    }

    client.input_buf.extend_from_slice(&buf[..n as usize]);

    // Process complete messages
    loop {
        let input = &state.clients.get(&cid).ok_or(())?.input_buf;
        let Some((msg, consumed)) = Message::decode(input) else {
            break;
        };

        // Remove consumed bytes
        state
            .clients
            .get_mut(&cid)
            .ok_or(())?
            .input_buf
            .drain(..consumed);

        match msg.msg_type {
            protocol::MSG_IDENTIFY | protocol::MSG_NEW_SESSION => {
                handle_identify(state, config, poll, cid, &msg, new_panes, tty)?;
                *force_render = true;
            }
            protocol::MSG_ATTACH => {
                handle_attach(state, config, cid, &msg, tty)?;
                *force_render = true;
            }
            protocol::MSG_INPUT => {
                handle_input(state, config, cid, &msg.payload, new_panes, force_render, input_events);
            }
            protocol::MSG_RESIZE => {
                handle_resize(state, cid, &msg.payload);
                *force_render = true;
            }
            protocol::MSG_LIST => {
                handle_list(state, cid);
            }
            protocol::MSG_KILL_SESSION => {
                handle_kill(state, cid, &msg);
                *force_render = true;
            }
            protocol::MSG_DETACH => {
                return Err(());
            }
            _ => {}
        }
    }

    Ok(())
}

fn handle_identify(
    state: &mut State,
    config: &Config,
    _poll: &mut Poll,
    cid: ClientId,
    msg: &Message,
    new_panes: &mut Vec<PaneId>,
    tty: &mut TtyWriter,
) -> Result<(), ()> {
    let Some((name, sx, sy)) = protocol::decode_identify(&msg.payload) else {
        return Err(());
    };

    let client = state.clients.get_mut(&cid).ok_or(())?;
    client.sx = sx;
    client.sy = sy;

    // Set up the client's terminal
    tty.enter_alt_screen();
    tty.enable_mouse();
    if config.focus_events {
        tty.enable_focus();
    }
    tty.enable_bracketed_paste();
    tty.cursor_hide();
    tty.clear_screen();
    tty.flush_to(client.tty_fd).ok();

    let has_name = !name.is_empty();

    let sid = if msg.msg_type == protocol::MSG_ATTACH {
        let session_name = if has_name { name } else { "0".to_string() };
        if let Some(sid) = state.find_session_by_name(&session_name) {
            sid
        } else if !has_name {
            if let Some(&sid) = state.sessions.keys().next() {
                sid
            } else {
                create_session(state, "0", sx, sy, new_panes)?
            }
        } else {
            send_to_client(
                state,
                cid,
                &Message::new(protocol::MSG_ERROR, format!("session not found: {session_name}").into_bytes()),
            );
            return Err(());
        }
    } else {
        // MSG_NEW_SESSION
        let session_name = if has_name {
            // Check for duplicate
            if state.find_session_by_name(&name).is_some() {
                send_to_client(
                    state,
                    cid,
                    &Message::new(protocol::MSG_ERROR, format!("duplicate session: {name}").into_bytes()),
                );
                return Err(());
            }
            name
        } else {
            // Auto-generate: "0", "1", "2", ...
            let mut n = 0u32;
            loop {
                let candidate = n.to_string();
                if state.find_session_by_name(&candidate).is_none() {
                    break candidate;
                }
                n += 1;
            }
        };
        create_session(state, &session_name, sx, sy, new_panes)?
    };

    // Detach any existing client on this session
    let existing: Vec<ClientId> = state
        .clients
        .iter()
        .filter(|(id, c)| **id != cid && c.session == sid)
        .map(|(&id, _)| id)
        .collect();
    for old_cid in existing {
        send_to_client(state, old_cid, &Message::empty(protocol::MSG_DETACH));
    }

    let client = state.clients.get_mut(&cid).ok_or(())?;
    client.session = sid;

    // Resize window to match client
    resize_session_windows(state, sid, sx, sy);

    Ok(())
}

fn handle_attach(
    state: &mut State,
    config: &Config,
    cid: ClientId,
    msg: &Message,
    tty: &mut TtyWriter,
) -> Result<(), ()> {
    let Some((name, sx, sy)) = protocol::decode_identify(&msg.payload) else {
        return Err(());
    };

    let client = state.clients.get_mut(&cid).ok_or(())?;
    client.sx = sx;
    client.sy = sy;

    // Set up terminal
    tty.enter_alt_screen();
    tty.enable_mouse();
    if config.focus_events {
        tty.enable_focus();
    }
    tty.enable_bracketed_paste();
    tty.cursor_hide();
    tty.clear_screen();
    tty.flush_to(client.tty_fd).ok();

    let has_name = !name.is_empty();
    let session_name = if has_name { name } else { "0".to_string() };

    let sid = if let Some(sid) = state.find_session_by_name(&session_name) {
        sid
    } else if !has_name {
        if let Some(&sid) = state.sessions.keys().next() {
            sid
        } else {
            send_to_client(state, cid, &Message::new(protocol::MSG_ERROR, b"no sessions".to_vec()));
            return Err(());
        }
    } else {
        send_to_client(state, cid, &Message::new(
            protocol::MSG_ERROR,
            format!("session not found: {session_name}").into_bytes(),
        ));
        return Err(());
    };

    let client = state.clients.get_mut(&cid).ok_or(())?;
    client.session = sid;

    resize_session_windows(state, sid, sx, sy);
    Ok(())
}

fn create_session(
    state: &mut State,
    name: &str,
    sx: u32,
    sy: u32,
    new_panes: &mut Vec<PaneId>,
) -> Result<crate::state::SessionId, ()> {
    let pid = state.alloc_pane_id();
    let pane_sy = sy.saturating_sub(1); // status bar

    let socket_path = protocol::socket_path();
    let (master, child_pid) = crate::pty::spawn_shell(
        sx,
        pane_sy,
        None,
        &socket_path,
        std::process::id(),
        pid.0,
    )
    .map_err(|_| ())?;

    let pane = crate::state::Pane::new(pid, master, child_pid, sx, pane_sy);
    state.panes.insert(pid, pane);
    new_panes.push(pid);

    let sid = state.create_session(name, pid, sx, sy);
    Ok(sid)
}

fn handle_input(
    state: &mut State,
    config: &mut Config,
    cid: ClientId,
    data: &[u8],
    new_panes: &mut Vec<PaneId>,
    force_render: &mut bool,
    input_events: &mut Vec<keys::InputEvent>,
) {
    keys::parse_input_into(data, input_events);

    for event in input_events.drain(..) {
        let result = key_bind::process_input(state, config, cid, event);
        apply_result(state, config, cid, result, new_panes, force_render);
    }
}

fn apply_result(
    state: &mut State,
    config: &mut Config,
    cid: ClientId,
    result: InputResult,
    new_panes: &mut Vec<PaneId>,
    force_render: &mut bool,
) {
    match result {
        InputResult::None => {}
        InputResult::PtyWrite(pid, data) => {
            if let Some(pane) = state.panes.get(&pid) {
                unsafe {
                    libc::write(
                        pane.pty_master,
                        data.as_ptr() as *const libc::c_void,
                        data.len(),
                    );
                }
            }
        }
        InputResult::Detach => {
            send_to_client(state, cid, &Message::empty(protocol::MSG_DETACH));
            if let Some(client) = state.clients.get(&cid) {
                // Restore client terminal
                let mut tty = TtyWriter::new();
                tty.disable_mouse();
                tty.disable_focus();
                tty.disable_bracketed_paste();
                tty.leave_alt_screen();
                tty.cursor_show();
                tty.flush_to(client.tty_fd).ok();
            }
        }
        InputResult::Redraw => {
            *force_render = true;
        }
        InputResult::NewPane(pid) => {
            new_panes.push(pid);
        }
        InputResult::StatusMessage(msg) => {
            // Check if this is a config reload
            if msg == "configuration reloaded" {
                config.reload();
            }
            if let Some(client) = state.clients.get_mut(&cid) {
                client.status_message = Some((msg, Instant::now()));
            }
            *force_render = true;
        }
        InputResult::Multi(results) => {
            for r in results {
                apply_result(state, config, cid, r, new_panes, force_render);
            }
        }
    }
}

fn handle_resize(state: &mut State, cid: ClientId, payload: &[u8]) {
    let Some((sx, sy)) = protocol::decode_resize(payload) else {
        return;
    };
    let client = match state.clients.get_mut(&cid) {
        Some(c) => c,
        None => return,
    };
    client.sx = sx;
    client.sy = sy;

    let sid = client.session;
    resize_session_windows(state, sid, sx, sy);
}

fn resize_session_windows(state: &mut State, sid: crate::state::SessionId, sx: u32, sy: u32) {
    let Some(session) = state.sessions.get(&sid) else {
        return;
    };
    let wids: Vec<crate::state::WindowId> = session.windows.clone();
    for wid in wids {
        if let Some(window) = state.windows.get_mut(&wid) {
            window.sx = sx;
            window.sy = sy.saturating_sub(1); // status bar
        }
        key_bind::recalc_layout_or_zoom(state, wid);
    }
}

fn handle_list(state: &State, cid: ClientId) {
    let mut output = String::new();
    for session in state.sessions.values() {
        let n_windows = session.windows.len();
        let window_names: Vec<String> = session
            .windows
            .iter()
            .filter_map(|wid| state.windows.get(wid))
            .map(|w| format!("{}:{}", w.idx, w.name))
            .collect();
        output.push_str(&format!(
            "{}: {} windows ({})\n",
            session.name,
            n_windows,
            window_names.join(" ")
        ));
    }
    if output.is_empty() {
        output = "no sessions\n".to_string();
    }
    send_to_client(
        state,
        cid,
        &Message::new(protocol::MSG_LIST_RESPONSE, output.into_bytes()),
    );
}

fn handle_kill(state: &mut State, cid: ClientId, msg: &Message) {
    let Some((name, _, _)) = protocol::decode_identify(&msg.payload) else {
        return;
    };

    let target = if name.is_empty() {
        state.sessions.keys().next().copied()
    } else {
        state.find_session_by_name(&name)
    };

    let Some(sid) = target else {
        send_to_client(
            state,
            cid,
            &Message::new(protocol::MSG_ERROR, b"session not found".to_vec()),
        );
        return;
    };

    // Kill all panes in the session
    let Some(session) = state.sessions.get(&sid) else {
        return;
    };
    let wids: Vec<crate::state::WindowId> = session.windows.clone();
    for wid in wids {
        if let Some(window) = state.windows.get(&wid) {
            let pids: Vec<PaneId> = window.panes.clone();
            for pid in pids {
                if let Some(pane) = state.panes.get(&pid) {
                    unsafe {
                        libc::kill(pane.pid, libc::SIGHUP);
                    }
                }
            }
        }
    }

    send_to_client(state, cid, &Message::empty(protocol::MSG_EXIT));
}

fn handle_pane_data(
    state: &mut State,
    pid: PaneId,
    tty: &mut TtyWriter,
    _client_tokens: &HashMap<Token, ClientId>,
) {
    let Some(pane) = state.panes.get(&pid) else {
        return;
    };
    let master_fd = pane.pty_master;

    let mut buf = [0u8; 16384];
    let n = unsafe {
        libc::read(
            master_fd,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
        )
    };
    if n <= 0 {
        return;
    }

    let data = &buf[..n as usize];

    // Process VT100 escape sequences
    let actions = vt::process_pane_output(
        state.panes.get_mut(&pid).unwrap(),
        data,
    );

    // Handle actions
    for action in actions {
        match action {
            vt::VtAction::AltScreen(enter) => {
                let pane = state.panes.get_mut(&pid).unwrap();
                if enter {
                    pane.enter_alt_screen();
                } else {
                    pane.exit_alt_screen();
                }
            }
            vt::VtAction::Cwd(path) => {
                if let Some(pane) = state.panes.get_mut(&pid) {
                    pane.cwd = Some(path);
                }
            }
            vt::VtAction::Title(title) => {
                // Update window name if this is the active pane
                if let Some(pane) = state.panes.get(&pid) {
                    let wid = pane.window;
                    if let Some(window) = state.windows.get(&wid) {
                        if window.active_pane == pid {
                            if let Some(window) = state.windows.get_mut(&wid) {
                                window.name = title;
                            }
                        }
                    }
                }
            }
            vt::VtAction::Clipboard(data) => {
                // Forward OSC 52 to all clients viewing this pane
                let osc = format!("\x1b]52;c;{data}\x07");
                for client in state.clients.values() {
                    let _ = tty.write_raw(osc.as_bytes());
                    let _ = tty.flush_to(client.tty_fd);
                }
            }
            vt::VtAction::CursorStyle(_) => {
                // Will be handled during render
            }
            vt::VtAction::BracketedPaste(_)
            | vt::VtAction::FocusEvents(_)
            | vt::VtAction::MouseMode { .. } => {
                // Mode changes tracked in screen state
            }
        }
    }
}

fn handle_pane_death(
    state: &mut State,
    poll: &mut Poll,
    pane_tokens: &mut HashMap<Token, PaneId>,
    pid: PaneId,
) {
    // Deregister from mio
    let token = pane_token(pid);
    if let Some(pane) = state.panes.get(&pid) {
        let mut fd = SourceFd(&pane.pty_master);
        poll.registry().deregister(&mut fd).ok();
    }
    pane_tokens.remove(&token);

    // Close PTY and remove pane
    if let Some(pane) = state.panes.remove(&pid) {
        sys::close_fd(pane.pty_master);

        let wid = pane.window;

        // Remove from window
        if let Some(window) = state.windows.get_mut(&wid) {
            window.panes.retain(|&p| p != pid);
            window.layout.remove_pane(pid);
            window.layout.simplify();

            if window.zoomed == Some(pid) {
                window.zoomed = None;
            }

            if window.active_pane == pid {
                window.active_pane = *window.panes.first().unwrap_or(&PaneId(0));
            }

            if window.panes.is_empty() {
                // Window is empty — remove it
                let sid = window.session;
                let wid = window.id;
                state.windows.remove(&wid);

                if let Some(session) = state.sessions.get_mut(&sid) {
                    session.windows.retain(|&w| w != wid);
                    if session.active_window == wid {
                        session.active_window = *session
                            .windows
                            .first()
                            .unwrap_or(&crate::state::WindowId(0));
                    }

                    if session.windows.is_empty() {
                        // Session is empty — remove it
                        let sid = session.id;
                        state.sessions.remove(&sid);
                    } else {
                        state.renumber_windows(sid);
                    }
                }
            } else {
                key_bind::recalc_layout_or_zoom(state, wid);
            }
        }
    }
}

fn cleanup_client(
    state: &mut State,
    poll: &mut Poll,
    client_tokens: &mut HashMap<Token, ClientId>,
    cid: ClientId,
) {
    let token = client_token(cid);
    if let Some(client) = state.clients.get(&cid) {
        // Restore terminal
        let mut tty = TtyWriter::new();
        tty.disable_mouse();
        tty.disable_focus();
        tty.disable_bracketed_paste();
        tty.leave_alt_screen();
        tty.cursor_show();
        tty.reset_attrs();
        tty.flush_to(client.tty_fd).ok();

        let mut fd = SourceFd(&client.socket_fd);
        poll.registry().deregister(&mut fd).ok();
        sys::close_fd(client.socket_fd);
        sys::close_fd(client.tty_fd);
    }
    state.clients.remove(&cid);
    client_tokens.remove(&token);
}

fn render_all_clients(state: &mut State, config: &Config, tty: &mut TtyWriter) {
    let cids: Vec<ClientId> = state.clients.keys().copied().collect();
    for cid in cids {
        let tty_fd = match state.clients.get(&cid) {
            Some(c) => c.tty_fd,
            None => continue,
        };
        // Skip clients that haven't identified yet
        if state
            .clients
            .get(&cid)
            .map_or(true, |c| c.session.0 == u32::MAX)
        {
            continue;
        }

        render::render_client(state, config, cid, tty);
        tty.flush_to(tty_fd).ok();

        render::clear_dirty(state, cid);
    }
}

fn send_to_client(state: &State, cid: ClientId, msg: &Message) {
    if let Some(client) = state.clients.get(&cid) {
        let data = msg.encode();
        unsafe {
            libc::write(
                client.socket_fd,
                data.as_ptr() as *const libc::c_void,
                data.len(),
            );
        }
    }
}

fn cleanup(state: &State, socket_path: &std::path::Path) {
    // Restore all client terminals
    for client in state.clients.values() {
        let mut tty = TtyWriter::new();
        tty.disable_mouse();
        tty.disable_focus();
        tty.disable_bracketed_paste();
        tty.leave_alt_screen();
        tty.cursor_show();
        tty.reset_attrs();
        tty.flush_to(client.tty_fd).ok();
    }
    let _ = std::fs::remove_file(socket_path);
}
