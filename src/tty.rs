use std::os::unix::io::RawFd;

use crate::grid::{CellAttr, CellContent, Color};
use crate::screen::CursorStyle;

/// Buffered terminal output writer.
pub struct TtyWriter {
    pub buf: Vec<u8>,
    // Track current attributes to minimize escape sequences
    cur_attr: CellAttr,
    cur_fg: Color,
    cur_bg: Color,
    cur_us: Color,
}

impl TtyWriter {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(8192),
            cur_attr: CellAttr::default(),
            cur_fg: Color::Default,
            cur_bg: Color::Default,
            cur_us: Color::Default,
        }
    }

    pub fn reset_state(&mut self) {
        self.cur_attr = CellAttr::default();
        self.cur_fg = Color::Default;
        self.cur_bg = Color::Default;
        self.cur_us = Color::Default;
    }

    /// Write raw bytes to the buffer.
    pub fn write_raw(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Write a string to the buffer.
    pub fn write_str(&mut self, s: &str) {
        self.buf.extend_from_slice(s.as_bytes());
    }

    /// Move cursor to (row, col) — 0-based.
    pub fn cursor_goto(&mut self, row: u32, col: u32) {
        use std::io::Write;
let _ = write!(self.buf, "\x1b[{};{}H", row + 1, col + 1);
    }

    /// Hide cursor.
    pub fn cursor_hide(&mut self) {
        self.write_raw(b"\x1b[?25l");
    }

    /// Show cursor.
    pub fn cursor_show(&mut self) {
        self.write_raw(b"\x1b[?25h");
    }

    /// Set cursor style.
    pub fn cursor_style(&mut self, style: CursorStyle) {
        let n = match style {
            CursorStyle::BlinkingBlock => 1,
            CursorStyle::Block => 2,
            CursorStyle::BlinkingUnderline => 3,
            CursorStyle::Underline => 4,
            CursorStyle::BlinkingBeam => 5,
            CursorStyle::Beam => 6,
        };
        use std::io::Write;
let _ = write!(self.buf, "\x1b[{n} q");
    }

    /// Reset all attributes.
    pub fn reset_attrs(&mut self) {
        self.write_raw(b"\x1b[0m");
        self.cur_attr = CellAttr::default();
        self.cur_fg = Color::Default;
        self.cur_bg = Color::Default;
        self.cur_us = Color::Default;
    }

    /// Set attributes and colors to match a cell.
    pub fn set_cell_attrs(&mut self, cell: &CellContent) {
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
        use std::io::Write;
        match color {
            Color::Default => {
                self.buf.extend_from_slice(if is_fg { b"\x1b[39m" } else { b"\x1b[49m" });
            }
            Color::Palette(idx) => {
                if idx < 8 {
                    let code = if is_fg { 30 + idx } else { 40 + idx };
                    let _ = write!(self.buf, "\x1b[{code}m");
                } else if idx < 16 {
                    let code = if is_fg { 90 + idx - 8 } else { 100 + idx - 8 };
                    let _ = write!(self.buf, "\x1b[{code}m");
                } else {
                    let prefix = if is_fg { 38 } else { 48 };
                    let _ = write!(self.buf, "\x1b[{prefix};5;{idx}m");
                }
            }
            Color::Rgb(r, g, b) => {
                let prefix = if is_fg { 38 } else { 48 };
                let _ = write!(self.buf, "\x1b[{prefix};2;{r};{g};{b}m");
            }
        }
    }

    fn write_underline_color(&mut self, color: Color) {
        use std::io::Write;
        match color {
            Color::Default => {
                self.buf.extend_from_slice(b"\x1b[59m");
            }
            Color::Palette(idx) => {
                let _ = write!(self.buf, "\x1b[58;5;{idx}m");
            }
            Color::Rgb(r, g, b) => {
                let _ = write!(self.buf, "\x1b[58;2;{r};{g};{b}m");
            }
        }
    }

    /// Clear the entire screen.
    pub fn clear_screen(&mut self) {
        self.write_raw(b"\x1b[2J");
    }

    /// Clear to end of line.
    pub fn clear_eol(&mut self) {
        self.write_raw(b"\x1b[K");
    }

    /// Enable mouse SGR reporting.
    pub fn enable_mouse(&mut self) {
        self.write_raw(b"\x1b[?1000h\x1b[?1002h\x1b[?1006h");
    }

    /// Disable mouse reporting.
    pub fn disable_mouse(&mut self) {
        self.write_raw(b"\x1b[?1006l\x1b[?1002l\x1b[?1000l");
    }

    /// Enable focus events.
    pub fn enable_focus(&mut self) {
        self.write_raw(b"\x1b[?1004h");
    }

    /// Disable focus events.
    pub fn disable_focus(&mut self) {
        self.write_raw(b"\x1b[?1004l");
    }

    /// Enter alternate screen.
    pub fn enter_alt_screen(&mut self) {
        self.write_raw(b"\x1b[?1049h");
    }

    /// Leave alternate screen.
    pub fn leave_alt_screen(&mut self) {
        self.write_raw(b"\x1b[?1049l");
    }

    /// Begin synchronized output.
    pub fn sync_begin(&mut self) {
        self.write_raw(b"\x1b[?2026h");
    }

    /// End synchronized output.
    pub fn sync_end(&mut self) {
        self.write_raw(b"\x1b[?2026l");
    }

    /// Enable bracketed paste.
    pub fn enable_bracketed_paste(&mut self) {
        self.write_raw(b"\x1b[?2004h");
    }

    /// Disable bracketed paste.
    pub fn disable_bracketed_paste(&mut self) {
        self.write_raw(b"\x1b[?2004l");
    }

    /// Flush the buffer to a file descriptor.
    pub fn flush_to(&mut self, fd: RawFd) -> std::io::Result<()> {
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
    pub fn drain_into(&mut self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&self.buf);
        self.buf.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{CellContent, Color};

    fn new_writer() -> TtyWriter {
        TtyWriter::new()
    }

    #[test]
    fn cursor_goto_produces_correct_sequence() {
        let mut w = new_writer();
        // cursor_goto is 0-based, output is 1-based
        w.cursor_goto(0, 0);
        assert_eq!(w.buf, b"\x1b[1;1H");

        w.buf.clear();
        w.cursor_goto(4, 9);
        assert_eq!(w.buf, b"\x1b[5;10H");
    }

    #[test]
    fn cursor_hide_show() {
        let mut w = new_writer();
        w.cursor_hide();
        assert_eq!(w.buf, b"\x1b[?25l");

        w.buf.clear();
        w.cursor_show();
        assert_eq!(w.buf, b"\x1b[?25h");
    }

    #[test]
    fn reset_attrs_sequence() {
        let mut w = new_writer();
        w.reset_attrs();
        assert_eq!(w.buf, b"\x1b[0m");
    }

    #[test]
    fn set_cell_attrs_default_color() {
        let mut w = new_writer();
        // A cell with all defaults but different from the writer's initial state
        // shouldn't need any output since the writer starts at defaults too.
        let cell = CellContent::default();
        w.set_cell_attrs(&cell);
        assert!(w.buf.is_empty(), "no output when attrs match current state");
    }

    #[test]
    fn set_cell_attrs_no_change_no_output() {
        let mut w = new_writer();
        let cell = CellContent {
            fg: Color::Palette(1),
            ..CellContent::default()
        };
        w.set_cell_attrs(&cell);
        let first_len = w.buf.len();
        assert!(first_len > 0);

        // Call again with the same cell -- should produce no additional output
        w.set_cell_attrs(&cell);
        assert_eq!(w.buf.len(), first_len, "repeated set_cell_attrs with same state should produce no output");
    }

    #[test]
    fn set_cell_attrs_palette_fg() {
        let mut w = new_writer();
        // Palette index < 8 uses 30+idx for fg
        let cell = CellContent {
            fg: Color::Palette(3),
            ..CellContent::default()
        };
        w.set_cell_attrs(&cell);
        let out = String::from_utf8_lossy(&w.buf);
        assert!(out.contains("\x1b[33m"), "palette 3 fg should be \\x1b[33m, got: {out}");
    }

    #[test]
    fn set_cell_attrs_palette_bg() {
        let mut w = new_writer();
        // Palette index < 8 uses 40+idx for bg
        let cell = CellContent {
            bg: Color::Palette(5),
            ..CellContent::default()
        };
        w.set_cell_attrs(&cell);
        let out = String::from_utf8_lossy(&w.buf);
        assert!(out.contains("\x1b[45m"), "palette 5 bg should be \\x1b[45m, got: {out}");
    }

    #[test]
    fn set_cell_attrs_palette_bright() {
        let mut w = new_writer();
        // Palette 8-15 uses 90+idx-8 for fg
        let cell = CellContent {
            fg: Color::Palette(10),
            ..CellContent::default()
        };
        w.set_cell_attrs(&cell);
        let out = String::from_utf8_lossy(&w.buf);
        assert!(out.contains("\x1b[92m"), "palette 10 fg should be \\x1b[92m, got: {out}");
    }

    #[test]
    fn set_cell_attrs_palette_256() {
        let mut w = new_writer();
        // Palette >= 16 uses 38;5;idx
        let cell = CellContent {
            fg: Color::Palette(200),
            ..CellContent::default()
        };
        w.set_cell_attrs(&cell);
        let out = String::from_utf8_lossy(&w.buf);
        assert!(out.contains("\x1b[38;5;200m"), "palette 200 fg should use 256-color, got: {out}");
    }

    #[test]
    fn set_cell_attrs_rgb_fg() {
        let mut w = new_writer();
        let cell = CellContent {
            fg: Color::Rgb(100, 150, 200),
            ..CellContent::default()
        };
        w.set_cell_attrs(&cell);
        let out = String::from_utf8_lossy(&w.buf);
        assert!(out.contains("\x1b[38;2;100;150;200m"), "rgb fg, got: {out}");
    }

    #[test]
    fn set_cell_attrs_rgb_bg() {
        let mut w = new_writer();
        let cell = CellContent {
            bg: Color::Rgb(10, 20, 30),
            ..CellContent::default()
        };
        w.set_cell_attrs(&cell);
        let out = String::from_utf8_lossy(&w.buf);
        assert!(out.contains("\x1b[48;2;10;20;30m"), "rgb bg, got: {out}");
    }

    #[test]
    fn sync_begin_end() {
        let mut w = new_writer();
        w.sync_begin();
        assert_eq!(w.buf, b"\x1b[?2026h");

        w.buf.clear();
        w.sync_end();
        assert_eq!(w.buf, b"\x1b[?2026l");
    }

    #[test]
    fn enable_disable_mouse() {
        let mut w = new_writer();
        w.enable_mouse();
        assert_eq!(w.buf, b"\x1b[?1000h\x1b[?1002h\x1b[?1006h");

        w.buf.clear();
        w.disable_mouse();
        assert_eq!(w.buf, b"\x1b[?1006l\x1b[?1002l\x1b[?1000l");
    }
}
