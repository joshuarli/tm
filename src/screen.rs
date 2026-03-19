use crate::grid::{CellContent, Color, Grid};

/// Screen mode bitflags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScreenMode(pub u32);

impl ScreenMode {
    pub const CURSOR_VISIBLE: u32 = 0x01;
    pub const INSERT: u32 = 0x02;
    pub const WRAP: u32 = 0x04;
    pub const ORIGIN: u32 = 0x08;
    pub const MOUSE_BUTTON: u32 = 0x10;
    pub const MOUSE_SGR: u32 = 0x20;
    pub const MOUSE_ANY: u32 = 0x40;
    pub const BRACKETED_PASTE: u32 = 0x80;
    pub const FOCUS_EVENTS: u32 = 0x100;
    pub const ALT_SCREEN: u32 = 0x200;
    pub const SYNCED_OUTPUT: u32 = 0x400;

    pub fn has(self, flag: u32) -> bool {
        self.0 & flag != 0
    }

    pub fn set(&mut self, flag: u32) {
        self.0 |= flag;
    }

    pub fn clear(&mut self, flag: u32) {
        self.0 &= !flag;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorStyle {
    #[default]
    Block,
    Underline,
    Beam,
    BlinkingBlock,
    BlinkingUnderline,
    BlinkingBeam,
}

pub struct Screen {
    pub grid: Grid,
    pub cx: u32,
    pub cy: u32,
    pub rupper: u32,
    pub rlower: u32,
    pub mode: ScreenMode,
    pub saved_cx: u32,
    pub saved_cy: u32,
    pub saved_cell: CellContent,
    pub tabs: Vec<bool>,
    pub title: String,
    pub cursor_style: CursorStyle,
    // Current cell style (used for next character written)
    pub cell: CellContent,
    // Pending wrap: cursor is at the right margin, next printable wraps
    pub pending_wrap: bool,
}

impl Screen {
    pub fn new(sx: u32, sy: u32) -> Self {
        let mut tabs = vec![false; sx as usize];
        // Default tab stops every 8 columns
        for i in (8..sx).step_by(8) {
            tabs[i as usize] = true;
        }

        let mode = ScreenMode(ScreenMode::CURSOR_VISIBLE | ScreenMode::WRAP);

        Self {
            grid: Grid::new(sx, sy, 10000),
            cx: 0,
            cy: 0,
            rupper: 0,
            rlower: sy.saturating_sub(1),
            mode,
            saved_cx: 0,
            saved_cy: 0,
            saved_cell: CellContent::default(),
            tabs,
            title: String::new(),
            cursor_style: CursorStyle::default(),
            cell: CellContent::default(),
            pending_wrap: false,
        }
    }

    pub fn sx(&self) -> u32 {
        self.grid.sx
    }

    pub fn sy(&self) -> u32 {
        self.grid.sy
    }

    /// Put a character at the cursor position, advancing the cursor.
    pub fn put_char(&mut self, ch: &[u8], ch_len: u8, ch_width: u8) {
        let sx = self.sx();

        // Handle pending wrap
        if self.pending_wrap {
            self.pending_wrap = false;
            if self.mode.has(ScreenMode::WRAP) {
                // Mark current line as wrapped
                if let Some(line) = self.grid.visible_line_mut(self.cy) {
                    line.flags.0 |= crate::grid::LineFlags::WRAPPED;
                }
                self.carriage_return();
                self.linefeed();
            }
        }

        // Build cell content
        let mut content = self.cell;
        content.ch[..ch_len as usize].copy_from_slice(&ch[..ch_len as usize]);
        content.ch_len = ch_len;
        content.ch_width = ch_width;

        // Insert mode: shift characters right
        if self.mode.has(ScreenMode::INSERT) {
            self.insert_cells(ch_width as u32);
        }

        // Write cell
        if let Some(line) = self.grid.visible_line_mut(self.cy) {
            line.set_cell(self.cx, &content);
        }

        // Advance cursor
        let new_cx = self.cx + ch_width as u32;
        if new_cx >= sx {
            // At right margin — set pending wrap
            self.cx = sx - 1;
            self.pending_wrap = true;
        } else {
            self.cx = new_cx;
        }
    }

    /// Fast path for a single printable ASCII byte.
    #[inline]
    pub fn put_ascii(&mut self, byte: u8) {
        self.put_ascii_run(std::slice::from_ref(&byte));
    }

    /// Batch-write a run of printable ASCII bytes. Each byte is 0x20..=0x7E.
    /// Handles line wrapping: fills to the right margin, wraps, continues.
    /// Gets the line pointer and pre-computes attr/fg/bg once per row segment.
    pub fn put_ascii_run(&mut self, bytes: &[u8]) {
        let sx = self.sx() as usize;
        let attr = self.cell.attr.basic();
        let fg = match self.cell.fg {
            crate::grid::Color::Palette(p) => p,
            _ => 0,
        };
        let bg = match self.cell.bg {
            crate::grid::Color::Palette(p) => p,
            _ => 0,
        };

        let mut i = 0;
        while i < bytes.len() {
            // Handle pending wrap from previous put
            if self.pending_wrap {
                self.pending_wrap = false;
                if self.mode.has(ScreenMode::WRAP) {
                    if let Some(line) = self.grid.visible_line_mut(self.cy) {
                        line.flags.0 |= crate::grid::LineFlags::WRAPPED;
                    }
                    self.carriage_return();
                    self.linefeed();
                }
            }

            // How many bytes can we write on this line?
            let cx = self.cx as usize;
            let avail = sx.saturating_sub(cx);
            if avail == 0 {
                break;
            }
            let n = (bytes.len() - i).min(avail);

            // Get the line pointer ONCE and write N cells in a tight loop.
            // Skip cells that already have identical content (tmux optimization).
            if let Some(line) = self.grid.visible_line_mut(self.cy) {
                let cells = &mut line.compact[cx..cx + n];
                for (j, c) in cells.iter_mut().enumerate() {
                    let new_ch = bytes[i + j];
                    if c.ch == new_ch
                        && c.attr == attr
                        && c.fg == fg
                        && c.bg == bg
                        && !c.is_extended()
                    {
                        continue; // identical — don't mark dirty
                    }
                    c.ch = new_ch;
                    c.attr = attr;
                    c.fg = fg;
                    c.bg = bg;
                    c.flags = crate::grid::CompactCell::DIRTY;
                }
            }

            i += n;
            let new_cx = (cx + n) as u32;
            if new_cx >= sx as u32 {
                self.cx = sx as u32 - 1;
                self.pending_wrap = true;
            } else {
                self.cx = new_cx;
            }
        }
    }

    /// Insert blank cells at cursor, shifting existing cells right.
    pub fn insert_cells(&mut self, count: u32) {
        let sx = self.sx();
        if let Some(line) = self.grid.visible_line_mut(self.cy) {
            let cx = self.cx as usize;
            let end = sx as usize;
            if cx < end {
                let count = count as usize;
                // Shift cells right
                for i in (cx + count..end).rev() {
                    line.compact[i] = line.compact[i - count];
                    line.compact[i].set_dirty();
                }
                // Clear inserted cells
                for i in cx..cx + count.min(end - cx) {
                    line.compact[i] = Default::default();
                    line.compact[i].set_dirty();
                }
            }
        }
    }

    pub fn carriage_return(&mut self) {
        self.cx = 0;
        self.pending_wrap = false;
    }

    pub fn linefeed(&mut self) {
        if self.cy == self.rlower {
            self.grid.scroll_up(self.rupper, self.rlower);
        } else if self.cy < self.sy() - 1 {
            self.cy += 1;
        }
    }

    pub fn reverse_index(&mut self) {
        if self.cy == self.rupper {
            self.grid.scroll_down(self.rupper, self.rlower);
        } else if self.cy > 0 {
            self.cy -= 1;
        }
        self.pending_wrap = false;
    }

    pub fn cursor_up(&mut self, n: u32) {
        let top = if self.cy >= self.rupper && self.cy <= self.rlower {
            self.rupper
        } else {
            0
        };
        self.cy = self.cy.saturating_sub(n).max(top);
        self.pending_wrap = false;
    }

    pub fn cursor_down(&mut self, n: u32) {
        let bottom = if self.cy >= self.rupper && self.cy <= self.rlower {
            self.rlower
        } else {
            self.sy() - 1
        };
        self.cy = (self.cy + n).min(bottom);
        self.pending_wrap = false;
    }

    pub fn cursor_left(&mut self, n: u32) {
        self.cx = self.cx.saturating_sub(n);
        self.pending_wrap = false;
    }

    pub fn cursor_right(&mut self, n: u32) {
        self.cx = (self.cx + n).min(self.sx() - 1);
        self.pending_wrap = false;
    }

    pub fn cursor_to(&mut self, row: u32, col: u32) {
        let (row, max_row) = if self.mode.has(ScreenMode::ORIGIN) {
            (row + self.rupper, self.rlower)
        } else {
            (row, self.sy() - 1)
        };
        self.cx = col.min(self.sx().saturating_sub(1));
        self.cy = row.min(max_row);
        self.pending_wrap = false;
    }

    pub fn save_cursor(&mut self) {
        self.saved_cx = self.cx;
        self.saved_cy = self.cy;
        self.saved_cell = self.cell;
    }

    pub fn restore_cursor(&mut self) {
        self.cx = self.saved_cx.min(self.sx().saturating_sub(1));
        self.cy = self.saved_cy.min(self.sy().saturating_sub(1));
        self.cell = self.saved_cell;
        self.pending_wrap = false;
    }

    pub fn tab(&mut self) {
        let sx = self.sx();
        let mut next = self.cx + 1;
        while next < sx {
            if next < self.tabs.len() as u32 && self.tabs[next as usize] {
                break;
            }
            next += 1;
        }
        self.cx = next.min(sx - 1);
        self.pending_wrap = false;
    }

    pub fn backspace(&mut self) {
        if self.cx > 0 {
            self.cx -= 1;
        }
        self.pending_wrap = false;
    }

    /// Erase in display (ED).
    pub fn erase_display(&mut self, mode: u32) {
        let sx = self.sx();
        let sy = self.sy();
        let blank = CellContent::default_with_bg(self.cell.bg);
        match mode {
            0 => {
                // Erase from cursor to end
                if let Some(line) = self.grid.visible_line_mut(self.cy) {
                    line.clear_range(self.cx, sx, &blank);
                }
                for row in (self.cy + 1)..sy {
                    if let Some(line) = self.grid.visible_line_mut(row) {
                        line.clear_range(0, sx, &blank);
                    }
                }
            }
            1 => {
                // Erase from start to cursor
                for row in 0..self.cy {
                    if let Some(line) = self.grid.visible_line_mut(row) {
                        line.clear_range(0, sx, &blank);
                    }
                }
                if let Some(line) = self.grid.visible_line_mut(self.cy) {
                    line.clear_range(0, self.cx + 1, &blank);
                }
            }
            2 | 3 => {
                // Erase entire display
                for row in 0..sy {
                    if let Some(line) = self.grid.visible_line_mut(row) {
                        line.clear_range(0, sx, &blank);
                    }
                }
            }
            _ => {}
        }
    }

    /// Erase in line (EL).
    pub fn erase_line(&mut self, mode: u32) {
        let sx = self.sx();
        let blank = CellContent::default_with_bg(self.cell.bg);
        match mode {
            0 => {
                // Erase from cursor to end of line
                if let Some(line) = self.grid.visible_line_mut(self.cy) {
                    line.clear_range(self.cx, sx, &blank);
                }
            }
            1 => {
                // Erase from start to cursor
                if let Some(line) = self.grid.visible_line_mut(self.cy) {
                    line.clear_range(0, self.cx + 1, &blank);
                }
            }
            2 => {
                // Erase entire line
                if let Some(line) = self.grid.visible_line_mut(self.cy) {
                    line.clear_range(0, sx, &blank);
                }
            }
            _ => {}
        }
    }

    /// Delete characters at cursor, shifting left.
    pub fn delete_chars(&mut self, n: u32) {
        let sx = self.sx();
        if let Some(line) = self.grid.visible_line_mut(self.cy) {
            let cx = self.cx as usize;
            let end = sx as usize;
            let n = n as usize;
            if cx < end {
                for i in cx..end {
                    let src = i + n;
                    if src < end {
                        line.compact[i] = line.compact[src];
                    } else {
                        line.compact[i] = Default::default();
                    }
                    line.compact[i].set_dirty();
                }
            }
        }
    }

    /// Insert blank lines at cursor row, shifting down.
    pub fn insert_lines(&mut self, n: u32) {
        if self.cy < self.rupper || self.cy > self.rlower {
            return;
        }
        for _ in 0..n {
            self.grid.scroll_down(self.cy, self.rlower);
        }
    }

    /// Delete lines at cursor row, shifting up.
    pub fn delete_lines(&mut self, n: u32) {
        if self.cy < self.rupper || self.cy > self.rlower {
            return;
        }
        for _ in 0..n {
            self.grid.scroll_up(self.cy, self.rlower);
        }
    }

    pub fn set_scroll_region(&mut self, top: u32, bottom: u32) {
        let sy = self.sy();
        let top = top.min(sy.saturating_sub(2));
        let bottom = bottom.min(sy.saturating_sub(1));
        if top >= bottom {
            return;
        }
        self.rupper = top;
        self.rlower = bottom;
        self.cursor_to(0, 0);
    }

    pub fn reset_scroll_region(&mut self) {
        self.rupper = 0;
        self.rlower = self.sy().saturating_sub(1);
    }

    pub fn resize(&mut self, sx: u32, sy: u32) {
        self.grid.resize(sx, sy);
        self.cx = self.cx.min(sx.saturating_sub(1));
        self.cy = self.cy.min(sy.saturating_sub(1));
        self.rupper = 0;
        self.rlower = sy.saturating_sub(1);
        self.pending_wrap = false;

        // Reset tab stops
        self.tabs = vec![false; sx as usize];
        for i in (8..sx).step_by(8) {
            self.tabs[i as usize] = true;
        }
    }

    pub fn clear_all(&mut self) {
        self.grid.clear();
        self.cx = 0;
        self.cy = 0;
        self.rupper = 0;
        self.rlower = self.sy().saturating_sub(1);
        self.pending_wrap = false;
        self.cell = CellContent::default();
    }

    pub fn mark_all_dirty(&mut self) {
        self.grid.mark_all_dirty();
    }

    /// Erase characters at cursor position (replace with blanks, don't shift).
    pub fn erase_chars(&mut self, n: u32) {
        let sx = self.sx();
        let end = (self.cx + n).min(sx);
        let blank = CellContent::default_with_bg(self.cell.bg);
        if let Some(line) = self.grid.visible_line_mut(self.cy) {
            line.clear_range(self.cx, end, &blank);
        }
    }
}

impl CellContent {
    pub fn default_with_bg(bg: Color) -> Self {
        Self {
            bg,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_screen_new() {
        let s = Screen::new(80, 24);
        assert_eq!(s.cx, 0);
        assert_eq!(s.cy, 0);
        assert_eq!(s.sx(), 80);
        assert_eq!(s.sy(), 24);
        assert_eq!(s.rupper, 0);
        assert_eq!(s.rlower, 23);
    }

    #[test]
    fn test_put_char() {
        let mut s = Screen::new(80, 24);
        s.put_char(b"A\0\0\0\0\0\0\0", 1, 1);
        assert_eq!(s.cx, 1);
        let cell = s.grid.visible_line(0).unwrap().get_cell(0);
        assert_eq!(cell.ch[0], b'A');
    }

    #[test]
    fn test_cursor_movement() {
        let mut s = Screen::new(80, 24);
        s.cursor_to(5, 10);
        assert_eq!(s.cy, 5);
        assert_eq!(s.cx, 10);

        s.cursor_up(3);
        assert_eq!(s.cy, 2);

        s.cursor_down(10);
        assert_eq!(s.cy, 12);

        s.cursor_left(5);
        assert_eq!(s.cx, 5);

        s.cursor_right(100);
        assert_eq!(s.cx, 79);
    }

    #[test]
    fn test_linefeed_scrolls() {
        let mut s = Screen::new(80, 5);
        // Move to bottom row
        s.cursor_to(4, 0);
        // Write 'A' on last row
        s.put_char(b"A\0\0\0\0\0\0\0", 1, 1);
        // Linefeed should scroll
        s.linefeed();
        assert_eq!(s.cy, 4);
        // 'A' should now be in history
        assert_eq!(s.grid.hsize(), 1);
    }

    #[test]
    fn test_scroll_region() {
        let mut s = Screen::new(80, 10);
        s.set_scroll_region(2, 5);
        assert_eq!(s.rupper, 2);
        assert_eq!(s.rlower, 5);
        // Cursor should be at 0,0 after setting scroll region
        assert_eq!(s.cx, 0);
        assert_eq!(s.cy, 0);
    }

    #[test]
    fn test_erase_line() {
        let mut s = Screen::new(80, 24);
        for c in b"Hello, World!" {
            s.put_char(&[*c, 0, 0, 0, 0, 0, 0, 0], 1, 1);
        }
        s.cx = 5;
        s.erase_line(0); // erase from cursor to end
        let cell = s.grid.visible_line(0).unwrap().get_cell(5);
        assert_eq!(cell.ch[0], b' ');
        // Chars before cursor should be preserved
        let cell = s.grid.visible_line(0).unwrap().get_cell(0);
        assert_eq!(cell.ch[0], b'H');
    }

    #[test]
    fn test_save_restore_cursor() {
        let mut s = Screen::new(80, 24);
        s.cursor_to(10, 20);
        s.save_cursor();
        s.cursor_to(0, 0);
        s.restore_cursor();
        assert_eq!(s.cy, 10);
        assert_eq!(s.cx, 20);
    }

    #[test]
    fn test_pending_wrap() {
        let mut s = Screen::new(5, 3);
        // Write to end of line
        for c in b"ABCDE" {
            s.put_char(&[*c, 0, 0, 0, 0, 0, 0, 0], 1, 1);
        }
        // Cursor should be at column 4 with pending wrap
        assert_eq!(s.cx, 4);
        assert!(s.pending_wrap);

        // Next char should wrap to next line
        s.put_char(b"F\0\0\0\0\0\0\0", 1, 1);
        assert_eq!(s.cy, 1);
        assert_eq!(s.cx, 1);
    }

    #[test]
    fn test_tab_stops() {
        let s = Screen::new(80, 24);
        // Default tabs at every 8 columns
        assert!(s.tabs[8]);
        assert!(s.tabs[16]);
        assert!(!s.tabs[5]);
    }

    #[test]
    fn test_resize() {
        let mut s = Screen::new(80, 24);
        s.cursor_to(20, 70);
        s.resize(40, 12);
        assert_eq!(s.sx(), 40);
        assert_eq!(s.sy(), 12);
        // Cursor should be clamped
        assert!(s.cx < 40);
        assert!(s.cy < 12);
    }

    #[test]
    fn test_delete_chars_shifts_left() {
        let mut s = Screen::new(10, 5);
        // Write "ABCDE" on row 0
        for (i, ch) in b"ABCDE".iter().enumerate() {
            s.cursor_to(0, i as u32);
            s.put_char(&[*ch, 0, 0, 0, 0, 0, 0, 0], 1, 1);
        }
        // Move to col 1, delete 2 chars
        s.cursor_to(0, 1);
        s.delete_chars(2);

        // 'B' and 'C' removed; 'D' shifts to col 1, 'E' to col 2
        let line = s.grid.visible_line(0).unwrap();
        assert_eq!(line.get_cell(0).ch[0], b'A');
        assert_eq!(line.get_cell(1).ch[0], b'D');
        assert_eq!(line.get_cell(2).ch[0], b'E');
        // Cells past original content should be blank
        assert_eq!(line.get_cell(3).ch[0], b' ');
        assert_eq!(line.get_cell(4).ch[0], b' ');
    }

    #[test]
    fn test_insert_lines_shifts_down() {
        let mut s = Screen::new(10, 5);
        // Put identifiable char on each row
        for row in 0..5u32 {
            s.cursor_to(row, 0);
            s.put_char(&[b'A' + row as u8, 0, 0, 0, 0, 0, 0, 0], 1, 1);
        }
        // Set scroll region to full screen (default), move cursor to row 1
        s.cursor_to(1, 0);
        s.insert_lines(1);

        // Row 0 untouched
        assert_eq!(s.grid.visible_line(0).unwrap().get_cell(0).ch[0], b'A');
        // Row 1 should now be blank (inserted line)
        assert_eq!(s.grid.visible_line(1).unwrap().get_cell(0).ch[0], b' ');
        // Row 2 should have what was on row 1 ('B')
        assert_eq!(s.grid.visible_line(2).unwrap().get_cell(0).ch[0], b'B');
        // Row 3 should have what was on row 2 ('C')
        assert_eq!(s.grid.visible_line(3).unwrap().get_cell(0).ch[0], b'C');
        // Row 4 should have what was on row 3 ('D')
        assert_eq!(s.grid.visible_line(4).unwrap().get_cell(0).ch[0], b'D');
        // 'E' (originally row 4) scrolled off the bottom
    }

    #[test]
    fn test_delete_lines_shifts_up() {
        let mut s = Screen::new(10, 5);
        for row in 0..5u32 {
            s.cursor_to(row, 0);
            s.put_char(&[b'A' + row as u8, 0, 0, 0, 0, 0, 0, 0], 1, 1);
        }
        // Move cursor to row 1, delete 1 line
        s.cursor_to(1, 0);
        s.delete_lines(1);

        // Row 0 untouched
        assert_eq!(s.grid.visible_line(0).unwrap().get_cell(0).ch[0], b'A');
        // Row 1 should have what was on row 2 ('C')
        assert_eq!(s.grid.visible_line(1).unwrap().get_cell(0).ch[0], b'C');
        // Row 2 should have what was on row 3 ('D')
        assert_eq!(s.grid.visible_line(2).unwrap().get_cell(0).ch[0], b'D');
        // Row 3 should have what was on row 4 ('E')
        assert_eq!(s.grid.visible_line(3).unwrap().get_cell(0).ch[0], b'E');
        // Row 4 should be blank (new line at bottom of region)
        assert_eq!(s.grid.visible_line(4).unwrap().get_cell(0).ch[0], b' ');
    }

    #[test]
    fn test_erase_display_mode_0_cursor_to_end() {
        let mut s = Screen::new(10, 5);
        // Fill rows 0-4 with characters
        for row in 0..5u32 {
            for col in 0..10u32 {
                s.cursor_to(row, col);
                s.put_char(b"X\0\0\0\0\0\0\0", 1, 1);
            }
        }
        // Place cursor at row 2, col 5
        s.cursor_to(2, 5);
        s.erase_display(0);

        // Row 2, cols 0-4 should still have 'X'
        for col in 0..5u32 {
            assert_eq!(s.grid.visible_line(2).unwrap().get_cell(col).ch[0], b'X');
        }
        // Row 2, cols 5-9 should be blank
        for col in 5..10u32 {
            assert_eq!(s.grid.visible_line(2).unwrap().get_cell(col).ch[0], b' ');
        }
        // Rows 3-4 should be entirely blank
        for row in 3..5u32 {
            for col in 0..10u32 {
                assert_eq!(s.grid.visible_line(row).unwrap().get_cell(col).ch[0], b' ');
            }
        }
        // Rows 0-1 should be untouched
        for row in 0..2u32 {
            assert_eq!(s.grid.visible_line(row).unwrap().get_cell(0).ch[0], b'X');
        }
    }

    #[test]
    fn test_erase_display_mode_1_start_to_cursor() {
        let mut s = Screen::new(10, 5);
        // Fill all with 'X'
        for row in 0..5u32 {
            for col in 0..10u32 {
                s.cursor_to(row, col);
                s.put_char(b"X\0\0\0\0\0\0\0", 1, 1);
            }
        }
        // Place cursor at row 2, col 5
        s.cursor_to(2, 5);
        s.erase_display(1);

        // Rows 0-1 should be entirely blank
        for row in 0..2u32 {
            for col in 0..10u32 {
                assert_eq!(s.grid.visible_line(row).unwrap().get_cell(col).ch[0], b' ');
            }
        }
        // Row 2, cols 0-5 should be blank (inclusive of cursor)
        for col in 0..=5u32 {
            assert_eq!(s.grid.visible_line(2).unwrap().get_cell(col).ch[0], b' ');
        }
        // Row 2, cols 6-9 should still have 'X'
        for col in 6..10u32 {
            assert_eq!(s.grid.visible_line(2).unwrap().get_cell(col).ch[0], b'X');
        }
        // Rows 3-4 should be untouched
        for row in 3..5u32 {
            assert_eq!(s.grid.visible_line(row).unwrap().get_cell(0).ch[0], b'X');
        }
    }

    #[test]
    fn test_erase_chars_blanks_without_shifting() {
        let mut s = Screen::new(10, 5);
        for (i, ch) in b"ABCDEFGHIJ".iter().enumerate() {
            s.cursor_to(0, i as u32);
            s.put_char(&[*ch, 0, 0, 0, 0, 0, 0, 0], 1, 1);
        }
        // Place cursor at col 3, erase 3 chars
        s.cursor_to(0, 3);
        s.erase_chars(3);

        let line = s.grid.visible_line(0).unwrap();
        // Cols 0-2 untouched
        assert_eq!(line.get_cell(0).ch[0], b'A');
        assert_eq!(line.get_cell(1).ch[0], b'B');
        assert_eq!(line.get_cell(2).ch[0], b'C');
        // Cols 3-5 blanked
        assert_eq!(line.get_cell(3).ch[0], b' ');
        assert_eq!(line.get_cell(4).ch[0], b' ');
        assert_eq!(line.get_cell(5).ch[0], b' ');
        // Cols 6-9 untouched (no shifting)
        assert_eq!(line.get_cell(6).ch[0], b'G');
        assert_eq!(line.get_cell(7).ch[0], b'H');
        assert_eq!(line.get_cell(8).ch[0], b'I');
        assert_eq!(line.get_cell(9).ch[0], b'J');
    }

    #[test]
    fn test_reverse_index_scrolls_down_at_top() {
        let mut s = Screen::new(10, 5);
        // Put chars on rows 0-4
        for row in 0..5u32 {
            s.cursor_to(row, 0);
            s.put_char(&[b'A' + row as u8, 0, 0, 0, 0, 0, 0, 0], 1, 1);
        }
        // Cursor at row 0 — reverse_index should scroll down
        s.cursor_to(0, 0);
        s.reverse_index();

        // Row 0 should be blank (new line inserted)
        assert_eq!(s.grid.visible_line(0).unwrap().get_cell(0).ch[0], b' ');
        // Row 1 should have what was on row 0 ('A')
        assert_eq!(s.grid.visible_line(1).unwrap().get_cell(0).ch[0], b'A');
        // Cursor should still be at row 0
        assert_eq!(s.cy, 0);
    }

    #[test]
    fn test_reverse_index_cursor_up_when_not_at_top() {
        let mut s = Screen::new(10, 5);
        s.cursor_to(3, 0);
        s.reverse_index();
        assert_eq!(s.cy, 2);
    }

    #[test]
    fn test_clear_all_resets_everything() {
        let mut s = Screen::new(10, 5);
        // Write some content, move cursor, set pending wrap
        for c in b"ABCDEFGHIJ" {
            s.put_char(&[*c, 0, 0, 0, 0, 0, 0, 0], 1, 1);
        }
        s.cursor_to(3, 5);
        s.cell.fg = Color::Palette(1);
        s.set_scroll_region(1, 3);

        s.clear_all();

        assert_eq!(s.cx, 0);
        assert_eq!(s.cy, 0);
        assert_eq!(s.rupper, 0);
        assert_eq!(s.rlower, 4);
        assert!(!s.pending_wrap);
        assert!(matches!(s.cell.fg, Color::Default));

        // All visible cells should be blank
        for row in 0..5u32 {
            for col in 0..10u32 {
                assert_eq!(s.grid.visible_line(row).unwrap().get_cell(col).ch[0], b' ');
            }
        }
    }
}
