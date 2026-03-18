use std::os::unix::io::RawFd;

use crate::grid::{CellAttr, CellContent, Color};
use crate::screen::CursorStyle;

/// Buffered terminal output writer.
pub(crate) struct TtyWriter {
    buf: Vec<u8>,
    // Track current attributes to minimize escape sequences
    cur_attr: CellAttr,
    cur_fg: Color,
    cur_bg: Color,
    cur_us: Color,
}

impl TtyWriter {
    pub(crate) fn new() -> Self {
        Self {
            buf: Vec::with_capacity(8192),
            cur_attr: CellAttr::default(),
            cur_fg: Color::Default,
            cur_bg: Color::Default,
            cur_us: Color::Default,
        }
    }

    pub(crate) fn reset_state(&mut self) {
        self.cur_attr = CellAttr::default();
        self.cur_fg = Color::Default;
        self.cur_bg = Color::Default;
        self.cur_us = Color::Default;
    }

    /// Write raw bytes to the buffer.
    pub(crate) fn write_raw(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Write a string to the buffer.
    pub(crate) fn write_str(&mut self, s: &str) {
        self.buf.extend_from_slice(s.as_bytes());
    }

    /// Move cursor to (row, col) — 0-based.
    pub(crate) fn cursor_goto(&mut self, row: u32, col: u32) {
        use std::fmt::Write;
        let mut s = String::new();
        write!(s, "\x1b[{};{}H", row + 1, col + 1).unwrap();
        self.write_str(&s);
    }

    /// Hide cursor.
    pub(crate) fn cursor_hide(&mut self) {
        self.write_raw(b"\x1b[?25l");
    }

    /// Show cursor.
    pub(crate) fn cursor_show(&mut self) {
        self.write_raw(b"\x1b[?25h");
    }

    /// Set cursor style.
    pub(crate) fn cursor_style(&mut self, style: CursorStyle) {
        let n = match style {
            CursorStyle::BlinkingBlock => 1,
            CursorStyle::Block => 2,
            CursorStyle::BlinkingUnderline => 3,
            CursorStyle::Underline => 4,
            CursorStyle::BlinkingBeam => 5,
            CursorStyle::Beam => 6,
        };
        let s = format!("\x1b[{n} q");
        self.write_str(&s);
    }

    /// Reset all attributes.
    pub(crate) fn reset_attrs(&mut self) {
        self.write_raw(b"\x1b[0m");
        self.cur_attr = CellAttr::default();
        self.cur_fg = Color::Default;
        self.cur_bg = Color::Default;
        self.cur_us = Color::Default;
    }

    /// Set attributes and colors to match a cell.
    pub(crate) fn set_cell_attrs(&mut self, cell: &CellContent) {
        if cell.attr == self.cur_attr
            && cell.fg == self.cur_fg
            && cell.bg == self.cur_bg
            && cell.us == self.cur_us
        {
            return;
        }

        // If the new attrs are simpler, it's cheaper to reset and reapply
        let need_reset = (self.cur_attr.0 & !cell.attr.0) != 0
            || (self.cur_fg != Color::Default && cell.fg == Color::Default)
            || (self.cur_bg != Color::Default && cell.bg == Color::Default);

        if need_reset {
            self.write_raw(b"\x1b[0m");
            self.cur_attr = CellAttr::default();
            self.cur_fg = Color::Default;
            self.cur_bg = Color::Default;
            self.cur_us = Color::Default;
        }

        // Apply attributes
        let new_bits = cell.attr.0 & !self.cur_attr.0;
        if new_bits & CellAttr::BOLD != 0 {
            self.write_raw(b"\x1b[1m");
        }
        if new_bits & CellAttr::DIM != 0 {
            self.write_raw(b"\x1b[2m");
        }
        if new_bits & CellAttr::ITALIC != 0 {
            self.write_raw(b"\x1b[3m");
        }
        if new_bits & CellAttr::UNDERLINE != 0 {
            self.write_raw(b"\x1b[4m");
        }
        if new_bits & CellAttr::REVERSE != 0 {
            self.write_raw(b"\x1b[7m");
        }
        if new_bits & CellAttr::INVISIBLE != 0 {
            self.write_raw(b"\x1b[8m");
        }
        if new_bits & CellAttr::STRIKE != 0 {
            self.write_raw(b"\x1b[9m");
        }
        if new_bits & CellAttr::DOUBLE_UNDERLINE != 0 {
            self.write_raw(b"\x1b[21m");
        }
        if new_bits & CellAttr::CURLY_UNDERLINE != 0 {
            self.write_raw(b"\x1b[4:3m");
        }
        if new_bits & CellAttr::DOTTED_UNDERLINE != 0 {
            self.write_raw(b"\x1b[4:4m");
        }
        if new_bits & CellAttr::DASHED_UNDERLINE != 0 {
            self.write_raw(b"\x1b[4:5m");
        }

        // Foreground
        if cell.fg != self.cur_fg {
            self.write_color(cell.fg, true);
        }

        // Background
        if cell.bg != self.cur_bg {
            self.write_color(cell.bg, false);
        }

        // Underline color
        if cell.us != self.cur_us {
            self.write_underline_color(cell.us);
        }

        self.cur_attr = cell.attr;
        self.cur_fg = cell.fg;
        self.cur_bg = cell.bg;
        self.cur_us = cell.us;
    }

    fn write_color(&mut self, color: Color, is_fg: bool) {
        match color {
            Color::Default => {
                if is_fg {
                    self.write_raw(b"\x1b[39m");
                } else {
                    self.write_raw(b"\x1b[49m");
                }
            }
            Color::Palette(idx) => {
                if idx < 8 {
                    let base = if is_fg { 30 } else { 40 };
                    let s = format!("\x1b[{}m", base + idx);
                    self.write_str(&s);
                } else if idx < 16 {
                    let base = if is_fg { 90 } else { 100 };
                    let s = format!("\x1b[{}m", base + idx - 8);
                    self.write_str(&s);
                } else {
                    let prefix = if is_fg { 38 } else { 48 };
                    let s = format!("\x1b[{prefix};5;{idx}m");
                    self.write_str(&s);
                }
            }
            Color::Rgb(r, g, b) => {
                let prefix = if is_fg { 38 } else { 48 };
                let s = format!("\x1b[{prefix};2;{r};{g};{b}m");
                self.write_str(&s);
            }
        }
    }

    fn write_underline_color(&mut self, color: Color) {
        match color {
            Color::Default => {
                self.write_raw(b"\x1b[59m");
            }
            Color::Palette(idx) => {
                let s = format!("\x1b[58;5;{idx}m");
                self.write_str(&s);
            }
            Color::Rgb(r, g, b) => {
                let s = format!("\x1b[58;2;{r};{g};{b}m");
                self.write_str(&s);
            }
        }
    }

    /// Clear the entire screen.
    pub(crate) fn clear_screen(&mut self) {
        self.write_raw(b"\x1b[2J");
    }

    /// Clear to end of line.
    pub(crate) fn clear_eol(&mut self) {
        self.write_raw(b"\x1b[K");
    }

    /// Enable mouse SGR reporting.
    pub(crate) fn enable_mouse(&mut self) {
        self.write_raw(b"\x1b[?1000h\x1b[?1002h\x1b[?1006h");
    }

    /// Disable mouse reporting.
    pub(crate) fn disable_mouse(&mut self) {
        self.write_raw(b"\x1b[?1006l\x1b[?1002l\x1b[?1000l");
    }

    /// Enable focus events.
    pub(crate) fn enable_focus(&mut self) {
        self.write_raw(b"\x1b[?1004h");
    }

    /// Disable focus events.
    pub(crate) fn disable_focus(&mut self) {
        self.write_raw(b"\x1b[?1004l");
    }

    /// Enter alternate screen.
    pub(crate) fn enter_alt_screen(&mut self) {
        self.write_raw(b"\x1b[?1049h");
    }

    /// Leave alternate screen.
    pub(crate) fn leave_alt_screen(&mut self) {
        self.write_raw(b"\x1b[?1049l");
    }

    /// Begin synchronized output.
    pub(crate) fn sync_begin(&mut self) {
        self.write_raw(b"\x1b[?2026h");
    }

    /// End synchronized output.
    pub(crate) fn sync_end(&mut self) {
        self.write_raw(b"\x1b[?2026l");
    }

    /// Enable bracketed paste.
    pub(crate) fn enable_bracketed_paste(&mut self) {
        self.write_raw(b"\x1b[?2004h");
    }

    /// Disable bracketed paste.
    pub(crate) fn disable_bracketed_paste(&mut self) {
        self.write_raw(b"\x1b[?2004l");
    }

    /// Flush the buffer to a file descriptor.
    pub(crate) fn flush_to(&mut self, fd: RawFd) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let mut written = 0;
        while written < self.buf.len() {
            // SAFETY: writing to a valid fd with proper buffer bounds.
            let n = unsafe {
                libc::write(
                    fd,
                    self.buf[written..].as_ptr() as *const libc::c_void,
                    self.buf.len() - written,
                )
            };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    // Store remaining bytes
                    let remaining = self.buf[written..].to_vec();
                    self.buf = remaining;
                    return Ok(());
                }
                self.buf.clear();
                return Err(err);
            }
            written += n as usize;
        }
        self.buf.clear();
        Ok(())
    }

    /// Append buffered data to an output Vec (for client output_buf).
    pub(crate) fn drain_into(&mut self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&self.buf);
        self.buf.clear();
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}
