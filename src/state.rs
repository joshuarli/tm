use std::collections::HashMap;
use std::os::unix::io::RawFd;
use std::time::Instant;

use crate::layout::LayoutNode;
use crate::screen::Screen;
use crate::vt::VtParser;

// Newtype IDs — Copy + Eq + Hash
macro_rules! id_type {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub(crate) struct $name(pub(crate) u32);
    };
}

id_type!(SessionId);
id_type!(WindowId);
id_type!(PaneId);
id_type!(ClientId);

pub(crate) struct State {
    pub(crate) sessions: HashMap<SessionId, Session>,
    pub(crate) windows: HashMap<WindowId, Window>,
    pub(crate) panes: HashMap<PaneId, Pane>,
    pub(crate) clients: HashMap<ClientId, Client>,

    next_session: u32,
    next_window: u32,
    next_pane: u32,
    next_client: u32,
}

impl State {
    pub(crate) fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            windows: HashMap::new(),
            panes: HashMap::new(),
            clients: HashMap::new(),
            next_session: 0,
            next_window: 0,
            next_pane: 0,
            next_client: 0,
        }
    }

    pub(crate) fn alloc_session_id(&mut self) -> SessionId {
        let id = SessionId(self.next_session);
        self.next_session += 1;
        id
    }

    pub(crate) fn alloc_window_id(&mut self) -> WindowId {
        let id = WindowId(self.next_window);
        self.next_window += 1;
        id
    }

    pub(crate) fn alloc_pane_id(&mut self) -> PaneId {
        let id = PaneId(self.next_pane);
        self.next_pane += 1;
        id
    }

    pub(crate) fn alloc_client_id(&mut self) -> ClientId {
        let id = ClientId(self.next_client);
        self.next_client += 1;
        id
    }

    /// Create a new session with one window containing one pane.
    pub(crate) fn create_session(
        &mut self,
        name: &str,
        pane_id: PaneId,
        sx: u32,
        sy: u32,
    ) -> SessionId {
        let sid = self.alloc_session_id();
        let wid = self.alloc_window_id();

        let status_height = 1u32;
        let pane_sy = sy.saturating_sub(status_height);

        let window = Window {
            id: wid,
            idx: 1,
            name: name.to_string(),
            active_pane: pane_id,
            panes: vec![pane_id],
            sx,
            sy: pane_sy,
            zoomed: None,
            session: sid,
            layout: LayoutNode::Pane(pane_id),
        };
        self.windows.insert(wid, window);

        // Update pane dimensions and position
        if let Some(pane) = self.panes.get_mut(&pane_id) {
            pane.sx = sx;
            pane.sy = pane_sy;
            pane.xoff = 0;
            pane.yoff = 0;
            pane.window = wid;
            pane.screen.resize(sx, pane_sy);
            pane.alt_screen.resize(sx, pane_sy);
        }

        let session = Session {
            id: sid,
            name: name.to_string(),
            windows: vec![wid],
            active_window: wid,
            next_window_idx: 2,
        };
        self.sessions.insert(sid, session);
        sid
    }

    /// Find a session by name.
    pub(crate) fn find_session_by_name(&self, name: &str) -> Option<SessionId> {
        self.sessions
            .values()
            .find(|s| s.name == name)
            .map(|s| s.id)
    }

    /// Renumber windows in a session starting from 1.
    pub(crate) fn renumber_windows(&mut self, sid: SessionId) {
        let Some(session) = self.sessions.get(&sid) else {
            return;
        };
        let wids: Vec<WindowId> = session.windows.clone();
        for (i, wid) in wids.iter().enumerate() {
            if let Some(w) = self.windows.get_mut(wid) {
                w.idx = (i + 1) as u32;
            }
        }
    }

    /// Get the active pane for a client.
    pub(crate) fn active_pane_for_client(&self, cid: ClientId) -> Option<PaneId> {
        let client = self.clients.get(&cid)?;
        let session = self.sessions.get(&client.session)?;
        let window = self.windows.get(&session.active_window)?;
        Some(window.active_pane)
    }

    /// Get the active window for a client.
    pub(crate) fn active_window_for_client(&self, cid: ClientId) -> Option<WindowId> {
        let client = self.clients.get(&cid)?;
        let session = self.sessions.get(&client.session)?;
        Some(session.active_window)
    }
}

pub(crate) struct Session {
    pub(crate) id: SessionId,
    pub(crate) name: String,
    pub(crate) windows: Vec<WindowId>,
    pub(crate) active_window: WindowId,
    pub(crate) next_window_idx: u32,
}

pub(crate) struct Window {
    pub(crate) id: WindowId,
    pub(crate) idx: u32,
    pub(crate) name: String,
    pub(crate) active_pane: PaneId,
    pub(crate) panes: Vec<PaneId>,
    pub(crate) sx: u32,
    pub(crate) sy: u32,
    pub(crate) zoomed: Option<PaneId>,
    pub(crate) session: SessionId,
    pub(crate) layout: LayoutNode,
}

pub(crate) struct Pane {
    pub(crate) id: PaneId,
    pub(crate) pty_master: RawFd,
    pub(crate) pid: libc::pid_t,
    pub(crate) screen: Screen,
    pub(crate) alt_screen: Screen,
    pub(crate) parser: VtParser,
    pub(crate) sx: u32,
    pub(crate) sy: u32,
    pub(crate) xoff: u32,
    pub(crate) yoff: u32,
    pub(crate) flags: PaneFlags,
    pub(crate) cwd: Option<String>,
    pub(crate) window: WindowId,
    pub(crate) using_alt: bool,
}

impl Pane {
    pub(crate) fn new(id: PaneId, pty_master: RawFd, pid: libc::pid_t, sx: u32, sy: u32) -> Self {
        Self {
            id,
            pty_master,
            pid,
            screen: Screen::new(sx, sy),
            alt_screen: Screen::new(sx, sy),
            parser: VtParser::new(),
            sx,
            sy,
            xoff: 0,
            yoff: 0,
            flags: PaneFlags::REDRAW,
            cwd: None,
            window: WindowId(0),
            using_alt: false,
        }
    }

    pub(crate) fn active_screen(&self) -> &Screen {
        if self.using_alt {
            &self.alt_screen
        } else {
            &self.screen
        }
    }

    pub(crate) fn active_screen_mut(&mut self) -> &mut Screen {
        if self.using_alt {
            &mut self.alt_screen
        } else {
            &mut self.screen
        }
    }

    pub(crate) fn enter_alt_screen(&mut self) {
        if !self.using_alt {
            self.using_alt = true;
            self.alt_screen.clear_all();
        }
    }

    pub(crate) fn exit_alt_screen(&mut self) {
        if self.using_alt {
            self.using_alt = false;
            self.flags |= PaneFlags::REDRAW;
        }
    }

    pub(crate) fn is_alt_screen(&self) -> bool {
        self.using_alt
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaneFlags(pub(crate) u32);

impl PaneFlags {
    pub(crate) const NONE: Self = Self(0);
    pub(crate) const REDRAW: Self = Self(0x1);

    pub(crate) fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOrAssign for PaneFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAndAssign for PaneFlags {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClientMode {
    Normal,
    CopyMode,
    CommandPrompt,
}

pub(crate) struct Client {
    pub(crate) id: ClientId,
    pub(crate) socket_fd: RawFd,
    pub(crate) tty_fd: RawFd,
    pub(crate) sx: u32,
    pub(crate) sy: u32,
    pub(crate) session: SessionId,
    pub(crate) prefix_active: bool,
    pub(crate) repeat_deadline: Option<Instant>,
    pub(crate) input_buf: Vec<u8>,
    pub(crate) output_buf: Vec<u8>,
    pub(crate) mode: ClientMode,
    pub(crate) status_message: Option<(String, Instant)>,
    pub(crate) prompt_buf: Option<String>,
    pub(crate) prompt_action: Option<PromptAction>,
}

#[derive(Clone, Debug)]
pub(crate) enum PromptAction {
    NewWindow,
    RenameWindow,
    Command,
    MovePane,
}

impl Client {
    pub(crate) fn new(
        id: ClientId,
        socket_fd: RawFd,
        tty_fd: RawFd,
        sx: u32,
        sy: u32,
        session: SessionId,
    ) -> Self {
        Self {
            id,
            socket_fd,
            tty_fd,
            sx,
            sy,
            session,
            prefix_active: false,
            repeat_deadline: None,
            input_buf: Vec::new(),
            output_buf: Vec::new(),
            mode: ClientMode::Normal,
            status_message: None,
            prompt_buf: None,
            prompt_action: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_pane(state: &mut State) -> PaneId {
        let pid = state.alloc_pane_id();
        let pane = Pane::new(pid, -1, 0, 80, 24);
        state.panes.insert(pid, pane);
        pid
    }

    #[test]
    fn test_create_session() {
        let mut state = State::new();
        let pid = make_test_pane(&mut state);
        let sid = state.create_session("test", pid, 80, 25);

        assert!(state.sessions.contains_key(&sid));
        let session = &state.sessions[&sid];
        assert_eq!(session.name, "test");
        assert_eq!(session.windows.len(), 1);

        let wid = session.active_window;
        let window = &state.windows[&wid];
        assert_eq!(window.idx, 1);
        assert_eq!(window.active_pane, pid);
    }

    #[test]
    fn test_find_session_by_name() {
        let mut state = State::new();
        let pid = make_test_pane(&mut state);
        let sid = state.create_session("mysession", pid, 80, 25);

        assert_eq!(state.find_session_by_name("mysession"), Some(sid));
        assert_eq!(state.find_session_by_name("nonexistent"), None);
    }

    #[test]
    fn test_renumber_windows() {
        let mut state = State::new();
        let p1 = make_test_pane(&mut state);
        let sid = state.create_session("s", p1, 80, 25);

        // Add a second window manually
        let p2 = make_test_pane(&mut state);
        let wid2 = state.alloc_window_id();
        let window2 = Window {
            id: wid2,
            idx: 5, // intentionally wrong
            name: "w2".to_string(),
            active_pane: p2,
            panes: vec![p2],
            sx: 80,
            sy: 24,
            zoomed: None,
            session: sid,
            layout: crate::layout::LayoutNode::Pane(p2),
        };
        state.windows.insert(wid2, window2);
        state.sessions.get_mut(&sid).unwrap().windows.push(wid2);

        state.renumber_windows(sid);

        let session = &state.sessions[&sid];
        let w1 = &state.windows[&session.windows[0]];
        let w2 = &state.windows[&session.windows[1]];
        assert_eq!(w1.idx, 1);
        assert_eq!(w2.idx, 2);
    }

    #[test]
    fn test_active_pane_for_client() {
        let mut state = State::new();
        let pid = make_test_pane(&mut state);
        let sid = state.create_session("s", pid, 80, 25);
        let cid = state.alloc_client_id();
        state.clients.insert(cid, Client::new(cid, -1, -1, 80, 25, sid));

        assert_eq!(state.active_pane_for_client(cid), Some(pid));
    }

    #[test]
    fn test_alt_screen() {
        let mut pane = Pane::new(PaneId(0), -1, 0, 80, 24);
        assert!(!pane.is_alt_screen());

        pane.enter_alt_screen();
        assert!(pane.is_alt_screen());

        pane.exit_alt_screen();
        assert!(!pane.is_alt_screen());
    }
}
