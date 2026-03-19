use crate::grid::{CellAttr, Color};
use crate::screen::{CursorStyle, ScreenMode};
use crate::state::Pane;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    SosPmApc,
}

pub struct VtParser {
    state: VtState,
    params: Vec<u16>,
    intermediates: Vec<u8>,
    osc_buf: Vec<u8>,
    utf8_buf: [u8; 4],
    utf8_len: u8,
    utf8_need: u8,
    final_byte: u8,
}

/// Actions emitted by the parser that the caller handles.
pub enum VtAction {
    /// Forward cursor style change to client terminal.
    CursorStyle(CursorStyle),
    /// Set window title.
    Title(String),
    /// OSC 7 — current working directory.
    Cwd(String),
    /// OSC 52 — clipboard content (base64-encoded).
    Clipboard(String),
    /// Switch to/from alternate screen.
    AltScreen(bool),
    /// Bracketed paste mode changed.
    BracketedPaste(bool),
    /// Focus events mode changed.
    FocusEvents(bool),
    /// Mouse mode changed. (button, sgr, any)
    MouseMode { button: bool, sgr: bool, any: bool },
}

impl VtParser {
    pub fn new() -> Self {
        Self {
            state: VtState::Ground,
            params: Vec::with_capacity(16),
            intermediates: Vec::new(),
            osc_buf: Vec::new(),
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
            final_byte: 0,
        }
    }

    /// Feed raw bytes from the PTY into the parser, updating the pane's screen.
    /// Returns actions that need to be forwarded to the client.
    pub fn feed(&mut self, pane: &mut PaneScreenAccess, data: &[u8]) -> Vec<VtAction> {
        let mut actions = Vec::new();
        let mut i = 0;
        while i < data.len() {
            // Fast path: handle ASCII text + common controls inline in Ground state.
            // This avoids per-byte state machine dispatch for the ~95% of terminal
            // traffic that is printable ASCII, newlines, and carriage returns.
            if self.state == VtState::Ground && self.utf8_need == 0 {
                let screen = pane.screen_mut();
                let can_fast = !screen.mode.has(crate::screen::ScreenMode::INSERT)
                    && screen.cell.attr.fits_compact()
                    && matches!(screen.cell.fg, crate::grid::Color::Default | crate::grid::Color::Palette(_))
                    && matches!(screen.cell.bg, crate::grid::Color::Default | crate::grid::Color::Palette(_))
                    && matches!(screen.cell.us, crate::grid::Color::Default);
                if can_fast {
                    let mut advanced = false;
                    while i < data.len() {
                        // SIMD scan: find the length of the printable ASCII run
                        let ascii_run = crate::simd::SimdScanner::scan(&data[i..]);
                        if ascii_run > 0 {
                            for &byte in &data[i..i + ascii_run] {
                                screen.put_ascii(byte);
                            }
                            i += ascii_run;
                            advanced = true;
                            continue;
                        }

                        // Not printable ASCII — handle common controls inline
                        match data[i] {
                            0x0A => {
                                screen.linefeed();
                                i += 1;
                                advanced = true;
                            }
                            0x0D => {
                                screen.carriage_return();
                                i += 1;
                                advanced = true;
                            }
                            0x08 => {
                                screen.backspace();
                                i += 1;
                                advanced = true;
                            }
                            0x09 => {
                                screen.tab();
                                i += 1;
                                advanced = true;
                            }
                            0x1B => {
                                // ESC — try CSI fast path
                                if let Some(consumed) = self.try_csi_fast(pane, &data[i..], &mut actions) {
                                    i += consumed;
                                    advanced = true;
                                    break; // re-check can_fast after SGR change
                                }
                                break; // fall to state machine
                            }
                            _ => break,
                        }
                    }
                    if advanced {
                        continue;
                    }
                }
            }
            self.process_byte(pane, data[i], &mut actions);
            i += 1;
        }
        actions
    }

    /// Try to parse a CSI sequence directly from the buffer without the state machine.
    /// Returns Some(bytes_consumed) on success, None if not a recognized fast-path sequence.
    fn try_csi_fast(
        &mut self,
        pane: &mut PaneScreenAccess,
        buf: &[u8],
        actions: &mut Vec<VtAction>,
    ) -> Option<usize> {
        // Need at least ESC [ <final>
        if buf.len() < 3 || buf[0] != 0x1B || buf[1] != b'[' {
            return None;
        }

        let params = &buf[2..];

        // ESC[m — SGR reset (most common)
        if params.first() == Some(&b'm') {
            pane.screen_mut().cell = crate::grid::CellContent::default();
            return Some(3);
        }

        // ESC[K — erase to end of line (default mode 0)
        if params.first() == Some(&b'K') {
            pane.screen_mut().erase_line(0);
            return Some(3);
        }

        // ESC[H — cursor home
        if params.first() == Some(&b'H') {
            pane.screen_mut().cursor_to(0, 0);
            return Some(3);
        }

        // ESC[J — erase display (default mode 0)
        if params.first() == Some(&b'J') {
            pane.screen_mut().erase_display(0);
            return Some(3);
        }

        // Parse numeric parameters: ESC[ N1 ; N2 ; ... <final>
        // Scan for the final byte (0x40-0x7E), collecting params
        let mut p = [0u16; 8];
        let mut pi = 0;
        let mut j = 0;
        while j < params.len() {
            match params[j] {
                b'0'..=b'9' => {
                    if pi < p.len() {
                        p[pi] = p[pi].saturating_mul(10).saturating_add((params[j] - b'0') as u16);
                    }
                    j += 1;
                }
                b';' => {
                    pi += 1;
                    j += 1;
                }
                b':' => {
                    // Colon sub-params — bail to state machine for complex SGR
                    return None;
                }
                b'?' | b'>' | b'!' | b' ' => {
                    // Private/intermediate — bail
                    return None;
                }
                0x40..=0x7E => {
                    let nparams = pi + 1;
                    let total = 2 + j + 1; // ESC[ + params + final
                    match params[j] {
                        b'm' => {
                            // SGR — set params and dispatch
                            self.params.clear();
                            for k in 0..nparams {
                                self.params.push(p[k]);
                            }
                            self.sgr(pane);
                            return Some(total);
                        }
                        b'H' | b'f' => {
                            let row = if p[0] == 0 { 0 } else { p[0] as u32 - 1 };
                            let col = if nparams > 1 && p[1] > 0 { p[1] as u32 - 1 } else { 0 };
                            pane.screen_mut().cursor_to(row, col);
                            return Some(total);
                        }
                        b'A' => {
                            pane.screen_mut().cursor_up(if p[0] == 0 { 1 } else { p[0] as u32 });
                            return Some(total);
                        }
                        b'B' => {
                            pane.screen_mut().cursor_down(if p[0] == 0 { 1 } else { p[0] as u32 });
                            return Some(total);
                        }
                        b'C' => {
                            pane.screen_mut().cursor_right(if p[0] == 0 { 1 } else { p[0] as u32 });
                            return Some(total);
                        }
                        b'D' => {
                            pane.screen_mut().cursor_left(if p[0] == 0 { 1 } else { p[0] as u32 });
                            return Some(total);
                        }
                        b'G' => {
                            let col = if p[0] == 0 { 0 } else { p[0] as u32 - 1 };
                            let screen = pane.screen_mut();
                            screen.cx = col.min(screen.sx() - 1);
                            screen.pending_wrap = false;
                            return Some(total);
                        }
                        b'J' => {
                            pane.screen_mut().erase_display(p[0] as u32);
                            return Some(total);
                        }
                        b'K' => {
                            pane.screen_mut().erase_line(p[0] as u32);
                            return Some(total);
                        }
                        b'r' => {
                            let screen = pane.screen_mut();
                            let top = if p[0] == 0 { 0 } else { p[0] as u32 - 1 };
                            let bot = if nparams > 1 && p[1] > 0 { p[1] as u32 - 1 } else { screen.sy() - 1 };
                            screen.set_scroll_region(top, bot);
                            return Some(total);
                        }
                        _ => return None,
                    }
                }
                _ => return None, // unexpected byte
            }
        }
        // Incomplete sequence — don't consume, let state machine handle it
        None
    }

    fn process_byte(
        &mut self,
        pane: &mut PaneScreenAccess,
        byte: u8,
        actions: &mut Vec<VtAction>,
    ) {
        // Handle UTF-8 continuation in Ground state
        if self.utf8_need > 0 && self.state == VtState::Ground {
            if byte & 0xC0 == 0x80 {
                self.utf8_buf[self.utf8_len as usize] = byte;
                self.utf8_len += 1;
                if self.utf8_len == self.utf8_need {
                    let ch = &self.utf8_buf[..self.utf8_len as usize];
                    let width = utf8_char_width(ch);
                    let screen = pane.screen_mut();
                    screen.put_char(ch, self.utf8_len, width);
                    self.utf8_len = 0;
                    self.utf8_need = 0;
                }
                return;
            } else {
                // Invalid continuation — discard partial sequence
                self.utf8_len = 0;
                self.utf8_need = 0;
            }
        }

        match self.state {
            VtState::Ground => self.ground(pane, byte, actions),
            VtState::Escape => self.escape(pane, byte, actions),
            VtState::EscapeIntermediate => self.escape_intermediate(pane, byte, actions),
            VtState::CsiEntry => self.csi_entry(pane, byte, actions),
            VtState::CsiParam => self.csi_param(pane, byte, actions),
            VtState::CsiIntermediate => self.csi_intermediate(pane, byte, actions),
            VtState::OscString => self.osc_string(pane, byte, actions),
            VtState::DcsEntry => self.dcs_entry(byte),
            VtState::DcsPassthrough => self.dcs_passthrough(byte),
            VtState::SosPmApc => self.sos_pm_apc(byte),
        }
    }

    fn ground(
        &mut self,
        pane: &mut PaneScreenAccess,
        byte: u8,
        actions: &mut Vec<VtAction>,
    ) {
        match byte {
            // C0 controls
            0x00 => {} // NUL — ignore
            0x07 => {} // BEL — ignore (could ring bell)
            0x08 => pane.screen_mut().backspace(),
            0x09 => pane.screen_mut().tab(),
            0x0A | 0x0B | 0x0C => pane.screen_mut().linefeed(),
            0x0D => pane.screen_mut().carriage_return(),
            0x0E | 0x0F => {} // SO/SI — charset switching, ignore
            0x1B => {
                self.state = VtState::Escape;
                self.intermediates.clear();
            }
            // Printable ASCII
            0x20..=0x7E => {
                let ch = [byte, 0, 0, 0, 0, 0, 0, 0];
                pane.screen_mut().put_char(&ch, 1, 1);
            }
            0x7F => {} // DEL — ignore
            // UTF-8 start bytes
            0xC0..=0xDF => {
                self.utf8_buf[0] = byte;
                self.utf8_len = 1;
                self.utf8_need = 2;
            }
            0xE0..=0xEF => {
                self.utf8_buf[0] = byte;
                self.utf8_len = 1;
                self.utf8_need = 3;
            }
            0xF0..=0xF7 => {
                self.utf8_buf[0] = byte;
                self.utf8_len = 1;
                self.utf8_need = 4;
            }
            _ => {} // Ignore other bytes (0x80-0xBF as standalone, 0xF8+)
        }
        // Ignore unused `actions` in this arm — used by other states
        let _ = actions;
    }

    fn escape(
        &mut self,
        pane: &mut PaneScreenAccess,
        byte: u8,
        actions: &mut Vec<VtAction>,
    ) {
        match byte {
            0x5B => {
                // ESC [ → CSI
                self.state = VtState::CsiEntry;
                self.params.clear();
                self.intermediates.clear();
            }
            0x5D => {
                // ESC ] → OSC
                self.state = VtState::OscString;
                self.osc_buf.clear();
            }
            0x50 => {
                // ESC P → DCS
                self.state = VtState::DcsEntry;
            }
            0x58 | 0x5E | 0x5F => {
                // SOS, PM, APC
                self.state = VtState::SosPmApc;
            }
            0x20..=0x2F => {
                // Intermediate bytes
                self.intermediates.push(byte);
                self.state = VtState::EscapeIntermediate;
            }
            // ESC dispatch
            0x37 => {
                // ESC 7 — DECSC
                pane.screen_mut().save_cursor();
                self.state = VtState::Ground;
            }
            0x38 => {
                // ESC 8 — DECRC
                pane.screen_mut().restore_cursor();
                self.state = VtState::Ground;
            }
            0x44 => {
                // ESC D — IND (index, linefeed)
                pane.screen_mut().linefeed();
                self.state = VtState::Ground;
            }
            0x45 => {
                // ESC E — NEL (newline)
                pane.screen_mut().carriage_return();
                pane.screen_mut().linefeed();
                self.state = VtState::Ground;
            }
            0x48 => {
                // ESC H — HTS (set tab stop)
                let cx = pane.screen_mut().cx as usize;
                if cx < pane.screen_mut().tabs.len() {
                    pane.screen_mut().tabs[cx] = true;
                }
                self.state = VtState::Ground;
            }
            0x4D => {
                // ESC M — RI (reverse index)
                pane.screen_mut().reverse_index();
                self.state = VtState::Ground;
            }
            0x63 => {
                // ESC c — RIS (full reset)
                let sx = pane.screen_mut().sx();
                let sy = pane.screen_mut().sy();
                *pane.screen_mut() = crate::screen::Screen::new(sx, sy);
                self.state = VtState::Ground;
            }
            0x1B => {
                // ESC ESC — stay in escape
            }
            _ => {
                // Unknown — return to ground
                crate::log::log(&format!("unknown ESC {byte:#04x}"));
                self.state = VtState::Ground;
            }
        }
        let _ = actions;
    }

    fn escape_intermediate(
        &mut self,
        pane: &mut PaneScreenAccess,
        byte: u8,
        actions: &mut Vec<VtAction>,
    ) {
        match byte {
            0x20..=0x2F => {
                self.intermediates.push(byte);
            }
            0x30..=0x7E => {
                // Final byte — dispatch
                // ESC ( B, ESC ) B etc. — charset designation, ignore
                self.state = VtState::Ground;
            }
            0x1B => {
                self.state = VtState::Escape;
                self.intermediates.clear();
            }
            _ => {
                self.state = VtState::Ground;
            }
        }
        let _ = (pane, actions);
    }

    fn csi_entry(
        &mut self,
        pane: &mut PaneScreenAccess,
        byte: u8,
        actions: &mut Vec<VtAction>,
    ) {
        match byte {
            0x30..=0x39 => {
                // Digit — start collecting parameter
                self.params.push((byte - 0x30) as u16);
                self.state = VtState::CsiParam;
            }
            0x3B => {
                // Semicolon — empty first param (default)
                self.params.push(0);
                self.params.push(0);
                self.state = VtState::CsiParam;
            }
            0x3C..=0x3F => {
                // Private marker (?, >, =, <)
                self.intermediates.push(byte);
                self.state = VtState::CsiParam;
            }
            0x20..=0x2F => {
                self.intermediates.push(byte);
                self.state = VtState::CsiIntermediate;
            }
            0x40..=0x7E => {
                // Final byte — dispatch with no params
                self.csi_dispatch(pane, byte, actions);
                self.state = VtState::Ground;
            }
            0x1B => {
                self.state = VtState::Escape;
                self.intermediates.clear();
            }
            _ => {
                self.state = VtState::Ground;
            }
        }
    }

    fn csi_param(
        &mut self,
        pane: &mut PaneScreenAccess,
        byte: u8,
        actions: &mut Vec<VtAction>,
    ) {
        match byte {
            0x30..=0x39 => {
                // Digit — accumulate
                if let Some(last) = self.params.last_mut() {
                    *last = last.saturating_mul(10).saturating_add((byte - 0x30) as u16);
                } else {
                    self.params.push((byte - 0x30) as u16);
                }
            }
            0x3B => {
                // Semicolon — next parameter
                if self.params.is_empty() {
                    self.params.push(0);
                }
                self.params.push(0);
            }
            0x3A => {
                // Colon — sub-parameter separator (used in SGR for underline style)
                // Treat like semicolon for our purposes
                if self.params.is_empty() {
                    self.params.push(0);
                }
                self.params.push(0);
            }
            0x3C..=0x3F => {
                // Private marker in the middle — push as intermediate
                self.intermediates.push(byte);
            }
            0x20..=0x2F => {
                self.intermediates.push(byte);
                self.state = VtState::CsiIntermediate;
            }
            0x40..=0x7E => {
                // Final byte — dispatch
                self.csi_dispatch(pane, byte, actions);
                self.state = VtState::Ground;
            }
            0x1B => {
                self.state = VtState::Escape;
                self.intermediates.clear();
            }
            _ => {
                self.state = VtState::Ground;
            }
        }
    }

    fn csi_intermediate(
        &mut self,
        pane: &mut PaneScreenAccess,
        byte: u8,
        actions: &mut Vec<VtAction>,
    ) {
        match byte {
            0x20..=0x2F => {
                self.intermediates.push(byte);
            }
            0x40..=0x7E => {
                self.csi_dispatch(pane, byte, actions);
                self.state = VtState::Ground;
            }
            0x1B => {
                self.state = VtState::Escape;
                self.intermediates.clear();
            }
            _ => {
                self.state = VtState::Ground;
            }
        }
    }

    fn csi_dispatch(
        &mut self,
        pane: &mut PaneScreenAccess,
        byte: u8,
        actions: &mut Vec<VtAction>,
    ) {
        let is_private = self.intermediates.first() == Some(&b'?');
        let is_gt = self.intermediates.first() == Some(&b'>');
        let p = |idx: usize, default: u32| -> u32 {
            self.params
                .get(idx)
                .copied()
                .map(|v| if v == 0 { default } else { v as u32 })
                .unwrap_or(default)
        };
        let p0 = |idx: usize| -> u32 {
            self.params
                .get(idx)
                .copied()
                .unwrap_or(0) as u32
        };

        match byte {
            b'A' => {
                // CUU — cursor up
                pane.screen_mut().cursor_up(p(0, 1));
            }
            b'B' | b'e' => {
                // CUD — cursor down
                pane.screen_mut().cursor_down(p(0, 1));
            }
            b'C' | b'a' => {
                // CUF — cursor forward (right)
                pane.screen_mut().cursor_right(p(0, 1));
            }
            b'D' => {
                // CUB — cursor back (left)
                pane.screen_mut().cursor_left(p(0, 1));
            }
            b'E' => {
                // CNL — cursor next line
                let n = p(0, 1);
                pane.screen_mut().cursor_down(n);
                pane.screen_mut().carriage_return();
            }
            b'F' => {
                // CPL — cursor previous line
                let n = p(0, 1);
                pane.screen_mut().cursor_up(n);
                pane.screen_mut().carriage_return();
            }
            b'G' | b'`' => {
                // CHA — cursor horizontal absolute
                let col = p(0, 1).saturating_sub(1);
                pane.screen_mut().cx = col.min(pane.screen_mut().sx() - 1);
                pane.screen_mut().pending_wrap = false;
            }
            b'H' | b'f' => {
                // CUP — cursor position
                let row = p(0, 1).saturating_sub(1);
                let col = p(1, 1).saturating_sub(1);
                pane.screen_mut().cursor_to(row, col);
            }
            b'J' => {
                // ED — erase in display
                pane.screen_mut().erase_display(p0(0));
            }
            b'K' => {
                // EL — erase in line
                pane.screen_mut().erase_line(p0(0));
            }
            b'L' => {
                // IL — insert lines
                pane.screen_mut().insert_lines(p(0, 1));
            }
            b'M' => {
                // DL — delete lines
                pane.screen_mut().delete_lines(p(0, 1));
            }
            b'P' => {
                // DCH — delete characters
                pane.screen_mut().delete_chars(p(0, 1));
            }
            b'S' => {
                // SU — scroll up
                let n = p(0, 1);
                let screen = pane.screen_mut();
                for _ in 0..n {
                    screen.grid.scroll_up(screen.rupper, screen.rlower);
                }
            }
            b'T' => {
                if !is_gt && self.params.len() <= 1 {
                    // SD — scroll down
                    let n = p(0, 1);
                    let screen = pane.screen_mut();
                    for _ in 0..n {
                        screen.grid.scroll_down(screen.rupper, screen.rlower);
                    }
                }
            }
            b'X' => {
                // ECH — erase characters
                pane.screen_mut().erase_chars(p(0, 1));
            }
            b'@' => {
                // ICH — insert blank characters
                let n = p(0, 1);
                pane.screen_mut().insert_cells(n);
            }
            b'd' => {
                // VPA — line position absolute
                let row = p(0, 1).saturating_sub(1);
                let cx = pane.screen_mut().cx;
                pane.screen_mut().cursor_to(row, cx);
            }
            b'g' => {
                // TBC — tab clear
                match p0(0) {
                    0 => {
                        let cx = pane.screen_mut().cx as usize;
                        if cx < pane.screen_mut().tabs.len() {
                            pane.screen_mut().tabs[cx] = false;
                        }
                    }
                    3 => {
                        pane.screen_mut().tabs.fill(false);
                    }
                    _ => {}
                }
            }
            b'h' => {
                if is_private {
                    self.set_mode_private(pane, true, actions);
                } else {
                    // SM — set mode
                    match p0(0) {
                        4 => pane.screen_mut().mode.set(ScreenMode::INSERT),
                        _ => {}
                    }
                }
            }
            b'l' => {
                if is_private {
                    self.set_mode_private(pane, false, actions);
                } else {
                    // RM — reset mode
                    match p0(0) {
                        4 => pane.screen_mut().mode.clear(ScreenMode::INSERT),
                        _ => {}
                    }
                }
            }
            b'm' => {
                // SGR — select graphic rendition
                self.sgr(pane);
            }
            b'n' => {
                // DSR — device status report
                if !is_private {
                    match p0(0) {
                        5 => {
                            // Status report — respond "OK"
                            pane.write_back(b"\x1b[0n");
                        }
                        6 => {
                            // Cursor position report
                            let screen = pane.screen_mut();
                            let row = screen.cy + 1;
                            let col = screen.cx + 1;
                            let response = format!("\x1b[{row};{col}R");
                            pane.write_back(response.as_bytes());
                        }
                        _ => {}
                    }
                }
            }
            b'c' => {
                if is_gt {
                    // DA2 — secondary device attributes
                    pane.write_back(b"\x1b[>0;0;0c");
                } else if !is_private {
                    // DA1 — primary device attributes
                    pane.write_back(b"\x1b[?62;22c");
                }
            }
            b'q' => {
                if self.intermediates.first() == Some(&b' ') {
                    // DECSCUSR — set cursor style
                    let style = match p0(0) {
                        0 | 1 => CursorStyle::BlinkingBlock,
                        2 => CursorStyle::Block,
                        3 => CursorStyle::BlinkingUnderline,
                        4 => CursorStyle::Underline,
                        5 => CursorStyle::BlinkingBeam,
                        6 => CursorStyle::Beam,
                        _ => CursorStyle::Block,
                    };
                    pane.screen_mut().cursor_style = style;
                    actions.push(VtAction::CursorStyle(style));
                }
            }
            b'r' => {
                if !is_private {
                    // DECSTBM — set scroll region
                    let top = p(0, 1).saturating_sub(1);
                    let bottom = p(1, 0);
                    let sy = pane.screen_mut().sy();
                    let bottom = if bottom == 0 { sy } else { bottom };
                    pane.screen_mut()
                        .set_scroll_region(top, bottom.saturating_sub(1));
                }
            }
            b's' => {
                if !is_private {
                    // SCOSC — save cursor
                    pane.screen_mut().save_cursor();
                }
            }
            b'u' => {
                if !is_private {
                    // SCORC — restore cursor
                    pane.screen_mut().restore_cursor();
                }
            }
            b't' => {
                // Window manipulation — mostly ignored
                // Some programs query these
            }
            _ => {
                crate::log::log(&format!(
                    "unknown CSI {}{}",
                    if is_private { "?" } else if is_gt { ">" } else { "" },
                    byte as char
                ));
            }
        }
    }

    fn set_mode_private(
        &self,
        pane: &mut PaneScreenAccess,
        enable: bool,
        actions: &mut Vec<VtAction>,
    ) {
        for &param in &self.params {
            match param {
                1 => {
                    // DECCKM — cursor keys mode (application vs normal)
                    // We pass this through in key handling
                    if enable {
                        pane.screen_mut().mode.set(0x1000);
                    } else {
                        pane.screen_mut().mode.clear(0x1000);
                    }
                }
                7 => {
                    // DECAWM — auto-wrap
                    if enable {
                        pane.screen_mut().mode.set(ScreenMode::WRAP);
                    } else {
                        pane.screen_mut().mode.clear(ScreenMode::WRAP);
                    }
                }
                6 => {
                    // DECOM — origin mode
                    if enable {
                        pane.screen_mut().mode.set(ScreenMode::ORIGIN);
                    } else {
                        pane.screen_mut().mode.clear(ScreenMode::ORIGIN);
                    }
                    pane.screen_mut().cursor_to(0, 0);
                }
                12 => {
                    // Cursor blink — ignore
                }
                25 => {
                    // DECTCEM — cursor visible
                    if enable {
                        pane.screen_mut().mode.set(ScreenMode::CURSOR_VISIBLE);
                    } else {
                        pane.screen_mut().mode.clear(ScreenMode::CURSOR_VISIBLE);
                    }
                }
                47 | 1047 => {
                    // Alternate screen buffer
                    actions.push(VtAction::AltScreen(enable));
                }
                1049 => {
                    // Alternate screen buffer + save/restore cursor
                    if enable {
                        pane.screen_mut().save_cursor();
                    }
                    actions.push(VtAction::AltScreen(enable));
                    if !enable {
                        pane.screen_mut().restore_cursor();
                    }
                }
                1000 => {
                    // Mouse button tracking
                    if enable {
                        pane.screen_mut().mode.set(ScreenMode::MOUSE_BUTTON);
                    } else {
                        pane.screen_mut().mode.clear(ScreenMode::MOUSE_BUTTON);
                    }
                }
                1002 => {
                    // Mouse any-event tracking (button motion)
                    if enable {
                        pane.screen_mut().mode.set(ScreenMode::MOUSE_ANY);
                    } else {
                        pane.screen_mut().mode.clear(ScreenMode::MOUSE_ANY);
                    }
                }
                1003 => {
                    // Mouse all motion tracking
                    if enable {
                        pane.screen_mut().mode.set(ScreenMode::MOUSE_ANY);
                    } else {
                        pane.screen_mut().mode.clear(ScreenMode::MOUSE_ANY);
                    }
                }
                1006 => {
                    // SGR mouse mode
                    if enable {
                        pane.screen_mut().mode.set(ScreenMode::MOUSE_SGR);
                    } else {
                        pane.screen_mut().mode.clear(ScreenMode::MOUSE_SGR);
                    }
                }
                1004 => {
                    // Focus events
                    if enable {
                        pane.screen_mut().mode.set(ScreenMode::FOCUS_EVENTS);
                    } else {
                        pane.screen_mut().mode.clear(ScreenMode::FOCUS_EVENTS);
                    }
                    actions.push(VtAction::FocusEvents(enable));
                }
                2004 => {
                    // Bracketed paste
                    if enable {
                        pane.screen_mut().mode.set(ScreenMode::BRACKETED_PASTE);
                    } else {
                        pane.screen_mut().mode.clear(ScreenMode::BRACKETED_PASTE);
                    }
                    actions.push(VtAction::BracketedPaste(enable));
                }
                2026 => {
                    // Synchronized output
                    if enable {
                        pane.screen_mut().mode.set(ScreenMode::SYNCED_OUTPUT);
                    } else {
                        pane.screen_mut().mode.clear(ScreenMode::SYNCED_OUTPUT);
                    }
                }
                _ => {
                    crate::log::log(&format!(
                        "unknown DECSET/DECRST {}{}",
                        param,
                        if enable { "h" } else { "l" }
                    ));
                }
            }
        }
    }

    fn sgr(&mut self, pane: &mut PaneScreenAccess) {
        if self.params.is_empty() {
            // ESC[m — reset all
            pane.screen_mut().cell = crate::grid::CellContent::default();
            return;
        }

        let mut i = 0;
        while i < self.params.len() {
            let code = self.params[i] as u32;
            match code {
                0 => {
                    pane.screen_mut().cell.attr = CellAttr::default();
                    pane.screen_mut().cell.fg = Color::Default;
                    pane.screen_mut().cell.bg = Color::Default;
                    pane.screen_mut().cell.us = Color::Default;
                }
                1 => pane.screen_mut().cell.attr.set(CellAttr::BOLD),
                2 => pane.screen_mut().cell.attr.set(CellAttr::DIM),
                3 => pane.screen_mut().cell.attr.set(CellAttr::ITALIC),
                4 => {
                    // Underline — check for sub-parameter
                    if i + 1 < self.params.len() {
                        let sub = self.params[i + 1] as u32;
                        match sub {
                            0 => {
                                pane.screen_mut().cell.attr.clear(CellAttr::UNDERLINE);
                                pane.screen_mut().cell.attr.clear(CellAttr::DOUBLE_UNDERLINE);
                                pane.screen_mut().cell.attr.clear(CellAttr::CURLY_UNDERLINE);
                                pane.screen_mut().cell.attr.clear(CellAttr::DOTTED_UNDERLINE);
                                pane.screen_mut().cell.attr.clear(CellAttr::DASHED_UNDERLINE);
                            }
                            1 => pane.screen_mut().cell.attr.set(CellAttr::UNDERLINE),
                            2 => pane.screen_mut().cell.attr.set(CellAttr::DOUBLE_UNDERLINE),
                            3 => pane.screen_mut().cell.attr.set(CellAttr::CURLY_UNDERLINE),
                            4 => pane.screen_mut().cell.attr.set(CellAttr::DOTTED_UNDERLINE),
                            5 => pane.screen_mut().cell.attr.set(CellAttr::DASHED_UNDERLINE),
                            _ => pane.screen_mut().cell.attr.set(CellAttr::UNDERLINE),
                        }
                        // Note: colon-separated sub-params were already split into params
                    } else {
                        pane.screen_mut().cell.attr.set(CellAttr::UNDERLINE);
                    }
                }
                7 => pane.screen_mut().cell.attr.set(CellAttr::REVERSE),
                8 => pane.screen_mut().cell.attr.set(CellAttr::INVISIBLE),
                9 => pane.screen_mut().cell.attr.set(CellAttr::STRIKE),
                21 => pane.screen_mut().cell.attr.set(CellAttr::DOUBLE_UNDERLINE),
                22 => {
                    pane.screen_mut().cell.attr.clear(CellAttr::BOLD);
                    pane.screen_mut().cell.attr.clear(CellAttr::DIM);
                }
                23 => pane.screen_mut().cell.attr.clear(CellAttr::ITALIC),
                24 => {
                    pane.screen_mut().cell.attr.clear(CellAttr::UNDERLINE);
                    pane.screen_mut().cell.attr.clear(CellAttr::DOUBLE_UNDERLINE);
                    pane.screen_mut().cell.attr.clear(CellAttr::CURLY_UNDERLINE);
                    pane.screen_mut().cell.attr.clear(CellAttr::DOTTED_UNDERLINE);
                    pane.screen_mut().cell.attr.clear(CellAttr::DASHED_UNDERLINE);
                }
                27 => pane.screen_mut().cell.attr.clear(CellAttr::REVERSE),
                28 => pane.screen_mut().cell.attr.clear(CellAttr::INVISIBLE),
                29 => pane.screen_mut().cell.attr.clear(CellAttr::STRIKE),
                // Foreground colors
                30..=37 => {
                    pane.screen_mut().cell.fg = Color::Palette((code - 30) as u8);
                }
                38 => {
                    if let Some(color) = self.parse_extended_color(i + 1) {
                        pane.screen_mut().cell.fg = color.0;
                        i += color.1;
                    }
                }
                39 => pane.screen_mut().cell.fg = Color::Default,
                // Background colors
                40..=47 => {
                    pane.screen_mut().cell.bg = Color::Palette((code - 40) as u8);
                }
                48 => {
                    if let Some(color) = self.parse_extended_color(i + 1) {
                        pane.screen_mut().cell.bg = color.0;
                        i += color.1;
                    }
                }
                49 => pane.screen_mut().cell.bg = Color::Default,
                // Underline color
                58 => {
                    if let Some(color) = self.parse_extended_color(i + 1) {
                        pane.screen_mut().cell.us = color.0;
                        i += color.1;
                    }
                }
                59 => pane.screen_mut().cell.us = Color::Default,
                // Bright foreground
                90..=97 => {
                    pane.screen_mut().cell.fg = Color::Palette((code - 90 + 8) as u8);
                }
                // Bright background
                100..=107 => {
                    pane.screen_mut().cell.bg = Color::Palette((code - 100 + 8) as u8);
                }
                _ => {}
            }
            i += 1;
        }
    }

    /// Parse extended color (256-color or RGB) starting at params[start].
    /// Returns (Color, params_consumed).
    fn parse_extended_color(&self, start: usize) -> Option<(Color, usize)> {
        let mode = *self.params.get(start)?;
        match mode {
            5 => {
                // 256-color: ;5;N
                let idx = *self.params.get(start + 1)? as u8;
                Some((Color::Palette(idx), 2))
            }
            2 => {
                // RGB: ;2;R;G;B
                let r = *self.params.get(start + 1)? as u8;
                let g = *self.params.get(start + 2)? as u8;
                let b = *self.params.get(start + 3)? as u8;
                Some((Color::Rgb(r, g, b), 4))
            }
            _ => None,
        }
    }

    fn osc_string(
        &mut self,
        pane: &mut PaneScreenAccess,
        byte: u8,
        actions: &mut Vec<VtAction>,
    ) {
        match byte {
            0x07 => {
                // BEL terminates OSC
                self.osc_dispatch(pane, actions);
                self.state = VtState::Ground;
            }
            0x1B => {
                // ESC — could be ST (\x1b\\)
                // Peek at next byte, but we can't here. Use a simple approach:
                // Store ESC and check next byte
                self.osc_buf.push(0x1B);
            }
            0x5C if self.osc_buf.last() == Some(&0x1B) => {
                // ST (ESC \) terminates OSC
                self.osc_buf.pop(); // remove the ESC
                self.osc_dispatch(pane, actions);
                self.state = VtState::Ground;
            }
            0x9C => {
                // ST (8-bit)
                self.osc_dispatch(pane, actions);
                self.state = VtState::Ground;
            }
            _ => {
                self.osc_buf.push(byte);
            }
        }
    }

    fn osc_dispatch(&mut self, pane: &mut PaneScreenAccess, actions: &mut Vec<VtAction>) {
        let buf = &self.osc_buf;
        if buf.is_empty() {
            return;
        }

        // Find the first ; to separate the command number
        let sep = buf.iter().position(|&b| b == b';');
        let (cmd_str, payload) = if let Some(pos) = sep {
            (&buf[..pos], &buf[pos + 1..])
        } else {
            (&buf[..], &[][..])
        };

        let cmd: u32 = std::str::from_utf8(cmd_str)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(u32::MAX);

        match cmd {
            0 | 2 => {
                // Set window title
                if let Ok(title) = std::str::from_utf8(payload) {
                    pane.screen_mut().title = title.to_string();
                    actions.push(VtAction::Title(title.to_string()));
                }
            }
            7 => {
                // CWD (file:// URL)
                if let Ok(url) = std::str::from_utf8(payload) {
                    let path = url
                        .strip_prefix("file://")
                        .and_then(|s| {
                            // file://hostname/path — skip hostname
                            s.find('/').map(|i| &s[i..])
                        })
                        .unwrap_or(url);
                    actions.push(VtAction::Cwd(path.to_string()));
                }
            }
            8 => {
                // Hyperlink — ignore for now, but consume
            }
            52 => {
                // Clipboard
                if let Ok(content) = std::str::from_utf8(payload) {
                    // Format: "c;base64data" or "p;base64data"
                    if let Some(data) = content.strip_prefix("c;").or(content.strip_prefix("p;")) {
                        actions.push(VtAction::Clipboard(data.to_string()));
                    }
                }
            }
            _ => {
                crate::log::log(&format!("unknown OSC {cmd}"));
            }
        }
    }

    fn dcs_entry(&mut self, byte: u8) {
        match byte {
            0x1B => {
                self.state = VtState::Escape;
                self.intermediates.clear();
            }
            0x9C => self.state = VtState::Ground,
            _ => self.state = VtState::DcsPassthrough,
        }
    }

    fn dcs_passthrough(&mut self, byte: u8) {
        match byte {
            0x1B => {
                self.state = VtState::Escape;
                self.intermediates.clear();
            }
            0x9C => self.state = VtState::Ground,
            _ => {} // consume
        }
    }

    fn sos_pm_apc(&mut self, byte: u8) {
        match byte {
            0x1B => {
                self.state = VtState::Escape;
                self.intermediates.clear();
            }
            0x9C => self.state = VtState::Ground,
            _ => {} // consume
        }
    }
}

/// Trait-like access to pane's screen, avoiding borrow issues.
/// This wraps a mutable reference to a Pane and provides the
/// operations the VT parser needs.
pub struct PaneScreenAccess<'a> {
    pane: &'a mut Pane,
}

impl<'a> PaneScreenAccess<'a> {
    pub fn new(pane: &'a mut Pane) -> Self {
        Self { pane }
    }

    pub fn screen_mut(&mut self) -> &mut crate::screen::Screen {
        self.pane.active_screen_mut()
    }

    /// Write data back to the PTY master (response to the application).
    pub fn write_back(&self, data: &[u8]) {
        // SAFETY: writing to a valid PTY master fd.
        unsafe {
            libc::write(
                self.pane.pty_master,
                data.as_ptr() as *const libc::c_void,
                data.len(),
            );
        }
    }
}

/// Determine the display width of a UTF-8 character.
fn utf8_char_width(bytes: &[u8]) -> u8 {
    if bytes.is_empty() {
        return 1;
    }

    // Decode the codepoint
    let cp = match bytes.len() {
        1 => bytes[0] as u32,
        2 => ((bytes[0] as u32 & 0x1F) << 6) | (bytes[1] as u32 & 0x3F),
        3 => {
            ((bytes[0] as u32 & 0x0F) << 12)
                | ((bytes[1] as u32 & 0x3F) << 6)
                | (bytes[2] as u32 & 0x3F)
        }
        4 => {
            ((bytes[0] as u32 & 0x07) << 18)
                | ((bytes[1] as u32 & 0x3F) << 12)
                | ((bytes[2] as u32 & 0x3F) << 6)
                | (bytes[3] as u32 & 0x3F)
        }
        _ => return 1,
    };

    // Wide characters: CJK Unified Ideographs, Hangul, Fullwidth forms, etc.
    if is_wide_codepoint(cp) {
        2
    } else {
        1
    }
}

fn is_wide_codepoint(cp: u32) -> bool {
    matches!(cp,
        0x1100..=0x115F   // Hangul Jamo
        | 0x2329..=0x232A // Misc Technical
        | 0x2E80..=0x303E // CJK Radicals Supplement..CJK Symbols
        | 0x3040..=0x33BF // Hiragana..CJK Compatibility
        | 0x3400..=0x4DBF // CJK Unified Ideographs Extension A
        | 0x4E00..=0xA4CF // CJK Unified Ideographs..Yi Radicals
        | 0xA960..=0xA97C // Hangul Jamo Extended-A
        | 0xAC00..=0xD7A3 // Hangul Syllables
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0xFE10..=0xFE19 // Vertical forms
        | 0xFE30..=0xFE6F // CJK Compatibility Forms
        | 0xFF01..=0xFF60 // Fullwidth Forms
        | 0xFFE0..=0xFFE6 // Fullwidth Sign
        | 0x1F000..=0x1F9FF // Various Emoji/Symbol blocks
        | 0x20000..=0x2FFFD // CJK Unified Ideographs Extension B+
        | 0x30000..=0x3FFFD // CJK Unified Ideographs Extension G+
    )
}

/// Process VT data for a pane. Call this from the server when PTY data arrives.
pub fn process_pane_output(pane: &mut Pane, data: &[u8]) -> Vec<VtAction> {
    // Split borrow: take the parser out, process, put it back
    let mut parser = std::mem::replace(&mut pane.parser, VtParser::new());
    let mut access = PaneScreenAccess::new(pane);
    let actions = parser.feed(&mut access, data);
    pane.parser = parser;
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::Screen;

    /// Helper: create a screen and feed VT data to it via the parser.
    fn feed_screen(sx: u32, sy: u32, data: &[u8]) -> Screen {
        let mut screen = Screen::new(sx, sy);
        let mut parser = VtParser::new();

        // We need a Pane to use PaneScreenAccess, but for tests
        // we can work around it by using a test-specific approach.
        // Actually, let's create a minimal pane with invalid fds.
        let mut pane = Pane::new(
            crate::state::PaneId(0),
            -1, // invalid fd, but we won't write to it in tests
            0,
            sx,
            sy,
        );

        let mut access = PaneScreenAccess::new(&mut pane);
        let _actions = parser.feed(&mut access, data);
        // Extract the screen
        std::mem::replace(&mut pane.screen, Screen::new(1, 1))
    }

    // Alternative approach: just create a Pane and use process_pane_output
    fn make_test_pane(sx: u32, sy: u32) -> Pane {
        Pane::new(crate::state::PaneId(0), -1, 0, sx, sy)
    }

    #[test]
    fn test_printable_ascii() {
        let mut pane = make_test_pane(80, 24);
        process_pane_output(&mut pane, b"Hello");
        assert_eq!(pane.screen.cx, 5);
        let cell = pane.screen.grid.visible_line(0).unwrap().get_cell(0);
        assert_eq!(cell.ch[0], b'H');
        let cell = pane.screen.grid.visible_line(0).unwrap().get_cell(4);
        assert_eq!(cell.ch[0], b'o');
    }

    #[test]
    fn test_cursor_movement_csi() {
        let mut pane = make_test_pane(80, 24);
        // CUP: move to row 5, col 10
        process_pane_output(&mut pane, b"\x1b[6;11H");
        assert_eq!(pane.screen.cy, 5);
        assert_eq!(pane.screen.cx, 10);

        // CUU: cursor up 2
        process_pane_output(&mut pane, b"\x1b[2A");
        assert_eq!(pane.screen.cy, 3);

        // CUF: cursor right 5
        process_pane_output(&mut pane, b"\x1b[5C");
        assert_eq!(pane.screen.cx, 15);
    }

    #[test]
    fn test_sgr_colors() {
        let mut pane = make_test_pane(80, 24);
        // Set foreground red (31), write 'R', reset
        process_pane_output(&mut pane, b"\x1b[31mR\x1b[0m");
        let cell = pane.screen.grid.visible_line(0).unwrap().get_cell(0);
        assert_eq!(cell.ch[0], b'R');
        assert_eq!(cell.fg, crate::grid::Color::Palette(1));
    }

    #[test]
    fn test_sgr_256_color() {
        let mut pane = make_test_pane(80, 24);
        // Set fg to color 200
        process_pane_output(&mut pane, b"\x1b[38;5;200mX");
        let cell = pane.screen.grid.visible_line(0).unwrap().get_cell(0);
        assert_eq!(cell.fg, crate::grid::Color::Palette(200));
    }

    #[test]
    fn test_sgr_rgb_color() {
        let mut pane = make_test_pane(80, 24);
        process_pane_output(&mut pane, b"\x1b[38;2;255;128;0mX");
        let cell = pane.screen.grid.visible_line(0).unwrap().get_cell(0);
        assert_eq!(cell.fg, crate::grid::Color::Rgb(255, 128, 0));
    }

    #[test]
    fn test_sgr_bold_italic() {
        let mut pane = make_test_pane(80, 24);
        process_pane_output(&mut pane, b"\x1b[1;3mB");
        let cell = pane.screen.grid.visible_line(0).unwrap().get_cell(0);
        assert!(cell.attr.has(crate::grid::CellAttr::BOLD));
        assert!(cell.attr.has(crate::grid::CellAttr::ITALIC));
    }

    #[test]
    fn test_erase_display() {
        let mut pane = make_test_pane(80, 5);
        process_pane_output(&mut pane, b"ABCDE");
        process_pane_output(&mut pane, b"\x1b[2J"); // clear all
        let cell = pane.screen.grid.visible_line(0).unwrap().get_cell(0);
        assert_eq!(cell.ch[0], b' ');
    }

    #[test]
    fn test_erase_line() {
        let mut pane = make_test_pane(80, 5);
        process_pane_output(&mut pane, b"Hello World");
        // Move cursor to col 5, erase from cursor to end of line
        process_pane_output(&mut pane, b"\x1b[6G\x1b[K");
        let cell = pane.screen.grid.visible_line(0).unwrap().get_cell(5);
        assert_eq!(cell.ch[0], b' ');
        // chars before should still be there
        let cell = pane.screen.grid.visible_line(0).unwrap().get_cell(0);
        assert_eq!(cell.ch[0], b'H');
    }

    #[test]
    fn test_newline() {
        let mut pane = make_test_pane(80, 5);
        process_pane_output(&mut pane, b"A\r\nB");
        assert_eq!(pane.screen.cy, 1);
        assert_eq!(pane.screen.cx, 1);
        let cell = pane.screen.grid.visible_line(1).unwrap().get_cell(0);
        assert_eq!(cell.ch[0], b'B');
    }

    #[test]
    fn test_osc_title() {
        let mut pane = make_test_pane(80, 24);
        let actions = process_pane_output(&mut pane, b"\x1b]2;My Title\x07");
        assert!(actions.iter().any(|a| matches!(a, VtAction::Title(t) if t == "My Title")));
        assert_eq!(pane.screen.title, "My Title");
    }

    #[test]
    fn test_osc_cwd() {
        let mut pane = make_test_pane(80, 24);
        let actions = process_pane_output(
            &mut pane,
            b"\x1b]7;file://hostname/home/user\x07",
        );
        assert!(actions.iter().any(|a| matches!(a, VtAction::Cwd(p) if p == "/home/user")));
    }

    #[test]
    fn test_alt_screen() {
        let mut pane = make_test_pane(80, 24);
        process_pane_output(&mut pane, b"Normal");
        let actions = process_pane_output(&mut pane, b"\x1b[?1049h");
        assert!(actions.iter().any(|a| matches!(a, VtAction::AltScreen(true))));
    }

    #[test]
    fn test_scroll_region() {
        let mut pane = make_test_pane(80, 24);
        process_pane_output(&mut pane, b"\x1b[5;20r");
        assert_eq!(pane.screen.rupper, 4);
        assert_eq!(pane.screen.rlower, 19);
    }

    #[test]
    fn test_decsc_decrc() {
        let mut pane = make_test_pane(80, 24);
        // Move to (5, 10), save, move elsewhere, restore
        process_pane_output(&mut pane, b"\x1b[6;11H\x1b7\x1b[1;1H\x1b8");
        assert_eq!(pane.screen.cy, 5);
        assert_eq!(pane.screen.cx, 10);
    }

    #[test]
    fn test_utf8_char() {
        let mut pane = make_test_pane(80, 24);
        // Write a 2-byte UTF-8 char: é (0xC3 0xA9)
        process_pane_output(&mut pane, &[0xC3, 0xA9]);
        assert_eq!(pane.screen.cx, 1); // width 1
    }

    #[test]
    fn test_delete_characters() {
        let mut pane = make_test_pane(80, 5);
        process_pane_output(&mut pane, b"ABCDE");
        // Move to col 1, delete 2 chars
        process_pane_output(&mut pane, b"\x1b[2G\x1b[2P");
        // B should be gone, D should now be at position 1
        let cell = pane.screen.grid.visible_line(0).unwrap().get_cell(1);
        assert_eq!(cell.ch[0], b'D');
    }

    #[test]
    fn test_sgr_multiple_params_in_one_sequence() {
        let mut pane = make_test_pane(80, 24);
        // Bold (1) + red fg (31) + green bg (42) in a single sequence
        process_pane_output(&mut pane, b"\x1b[1;31;42mX");
        let cell = pane.screen.grid.visible_line(0).unwrap().get_cell(0);
        assert_eq!(cell.ch[0], b'X');
        assert!(cell.attr.has(CellAttr::BOLD));
        assert_eq!(cell.fg, Color::Palette(1)); // red
        assert_eq!(cell.bg, Color::Palette(2)); // green
    }

    #[test]
    fn test_sgr_reset_clears_all() {
        let mut pane = make_test_pane(80, 24);
        // Set bold + italic + red fg, then reset, then write
        process_pane_output(&mut pane, b"\x1b[1;3;31m");
        assert!(pane.screen.cell.attr.has(CellAttr::BOLD));
        assert!(pane.screen.cell.attr.has(CellAttr::ITALIC));
        assert_eq!(pane.screen.cell.fg, Color::Palette(1));

        process_pane_output(&mut pane, b"\x1b[0m");
        assert!(!pane.screen.cell.attr.has(CellAttr::BOLD));
        assert!(!pane.screen.cell.attr.has(CellAttr::ITALIC));
        assert!(matches!(pane.screen.cell.fg, Color::Default));
        assert!(matches!(pane.screen.cell.bg, Color::Default));
    }

    #[test]
    fn test_insert_mode() {
        let mut pane = make_test_pane(80, 5);
        process_pane_output(&mut pane, b"ABCDE");
        // Enable insert mode
        process_pane_output(&mut pane, b"\x1b[4h");
        assert!(pane.screen.mode.has(ScreenMode::INSERT));

        // Move to col 2, write 'X' in insert mode — should shift CDE right
        process_pane_output(&mut pane, b"\x1b[3G");
        process_pane_output(&mut pane, b"X");

        let line = pane.screen.grid.visible_line(0).unwrap();
        assert_eq!(line.get_cell(0).ch[0], b'A');
        assert_eq!(line.get_cell(1).ch[0], b'B');
        assert_eq!(line.get_cell(2).ch[0], b'X');
        assert_eq!(line.get_cell(3).ch[0], b'C');
        assert_eq!(line.get_cell(4).ch[0], b'D');
        assert_eq!(line.get_cell(5).ch[0], b'E');
    }

    #[test]
    fn test_device_attributes_no_panic() {
        let mut pane = make_test_pane(80, 24);
        // DA1: should trigger a write_back (which will fail silently on fd -1)
        // but should not panic and should not alter screen content
        process_pane_output(&mut pane, b"Hello\x1b[c");
        // Screen should still have "Hello"
        let cell = pane.screen.grid.visible_line(0).unwrap().get_cell(0);
        assert_eq!(cell.ch[0], b'H');
        assert_eq!(pane.screen.cx, 5);
    }

    #[test]
    fn test_tab_advances_to_next_stop() {
        let mut pane = make_test_pane(80, 24);
        // Default tab stops every 8 cols. Cursor starts at 0.
        process_pane_output(&mut pane, b"\x09");
        assert_eq!(pane.screen.cx, 8);

        // Tab again should go to 16
        process_pane_output(&mut pane, b"\x09");
        assert_eq!(pane.screen.cx, 16);
    }

    #[test]
    fn test_tab_clear_all() {
        let mut pane = make_test_pane(80, 24);
        // Clear all tab stops
        process_pane_output(&mut pane, b"\x1b[3g");
        // Now tab should go all the way to the right margin
        process_pane_output(&mut pane, b"\x09");
        assert_eq!(pane.screen.cx, 79);
    }

    #[test]
    fn test_multiple_cursor_movements() {
        let mut pane = make_test_pane(80, 24);
        // Move to (5,10), then right 3, down 2, left 1
        process_pane_output(&mut pane, b"\x1b[6;11H\x1b[3C\x1b[2B\x1b[1D");
        assert_eq!(pane.screen.cx, 12); // 10 + 3 - 1
        assert_eq!(pane.screen.cy, 7); // 5 + 2
    }

    #[test]
    fn test_dcs_passthrough_no_crash() {
        let mut pane = make_test_pane(80, 24);
        // DCS sequence with payload, terminated by ST
        process_pane_output(&mut pane, b"\x1bPsome DCS payload\x1b\\");
        // After DCS+ST, parser should be back in ground state.
        // Write a normal character to verify.
        process_pane_output(&mut pane, b"Z");
        let cell = pane.screen.grid.visible_line(0).unwrap().get_cell(0);
        assert_eq!(cell.ch[0], b'Z');
    }

    #[test]
    fn test_unknown_sequences_consumed_silently() {
        let mut pane = make_test_pane(80, 24);
        // Feed an unknown CSI sequence, then a normal character
        process_pane_output(&mut pane, b"\x1b[999zABC");
        // Parser should recover and print ABC
        let cell = pane.screen.grid.visible_line(0).unwrap().get_cell(0);
        assert_eq!(cell.ch[0], b'A');
        assert_eq!(pane.screen.cx, 3);
    }
}
