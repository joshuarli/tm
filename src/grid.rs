use std::collections::VecDeque;

/// Color representation for cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    Palette(u8),
    Rgb(u8, u8, u8),
}

/// Cell attributes as bitflags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CellAttr(pub u16);

impl CellAttr {
    pub const BOLD: u16 = 0x01;
    pub const DIM: u16 = 0x02;
    pub const ITALIC: u16 = 0x04;
    pub const UNDERLINE: u16 = 0x08;
    pub const REVERSE: u16 = 0x10;
    pub const STRIKE: u16 = 0x20;
    pub const INVISIBLE: u16 = 0x40;
    pub const CURLY_UNDERLINE: u16 = 0x80;
    pub const DOUBLE_UNDERLINE: u16 = 0x100;
    pub const DOTTED_UNDERLINE: u16 = 0x200;
    pub const DASHED_UNDERLINE: u16 = 0x400;

    pub fn has(self, flag: u16) -> bool {
        self.0 & flag != 0
    }

    pub fn set(&mut self, flag: u16) {
        self.0 |= flag;
    }

    pub fn clear(&mut self, flag: u16) {
        self.0 &= !flag;
    }

    pub fn basic(self) -> u8 {
        (self.0 & 0xFF) as u8
    }

    pub fn fits_compact(self) -> bool {
        self.0 <= 0xFF
    }
}

/// Compact cell — 5 bytes. Covers ASCII with 256-color palette.
#[derive(Clone, Copy)]
pub struct CompactCell {
    pub ch: u8,    // ASCII byte, or 0xFF → extended
    pub attr: u8,  // basic attributes
    pub fg: u8,    // palette index
    pub bg: u8,    // palette index
    pub flags: u8, // EXTENDED | DIRTY | WIDE_CONTINUATION
}

impl Default for CompactCell {
    fn default() -> Self {
        Self {
            ch: b' ',
            attr: 0,
            fg: 0,
            bg: 0,
            flags: 0,
        }
    }
}

impl CompactCell {
    pub const EXTENDED: u8 = 0x01;
    pub const DIRTY: u8 = 0x02;
    pub const WIDE_CONT: u8 = 0x04;

    pub fn is_extended(self) -> bool {
        self.flags & Self::EXTENDED != 0
    }

    pub fn is_dirty(self) -> bool {
        self.flags & Self::DIRTY != 0
    }

    pub fn set_dirty(&mut self) {
        self.flags |= Self::DIRTY;
    }

    pub fn clear_dirty(&mut self) {
        self.flags &= !Self::DIRTY;
    }

    /// Get the extended index when ch == 0xFF. The attr/fg/bg fields store a u24 index.
    pub fn extended_idx(self) -> usize {
        ((self.attr as usize) << 16) | ((self.fg as usize) << 8) | (self.bg as usize)
    }
}

/// Extended cell — for Unicode, RGB colors, styled underlines.
#[derive(Clone, Copy, Default)]
pub struct ExtendedCell {
    pub ch: [u8; 8],
    pub ch_len: u8,
    pub ch_width: u8, // display width (1 or 2)
    pub attr: CellAttr,
    pub fg: Color,
    pub bg: Color,
    pub us: Color, // underline color
}

impl ExtendedCell {
    pub fn ch_str(&self) -> &str {
        std::str::from_utf8(&self.ch[..self.ch_len as usize]).unwrap_or(" ")
    }
}

/// Line flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineFlags(pub u8);

impl LineFlags {
    pub const WRAPPED: u8 = 0x01;

    pub fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

/// A single line in the grid.
pub struct GridLine {
    pub compact: Vec<CompactCell>,
    pub extended: Vec<ExtendedCell>,
    pub flags: LineFlags,
}

impl GridLine {
    pub fn new(width: u32) -> Self {
        Self {
            compact: vec![CompactCell::default(); width as usize],
            extended: Vec::new(),
            flags: LineFlags::default(),
        }
    }

    /// Set a cell from a CellContent description. Marks the cell dirty.
    pub fn set_cell(&mut self, col: u32, content: &CellContent) {
        let col = col as usize;
        if col >= self.compact.len() {
            return;
        }

        let needs_extended = content.ch_len > 1
            || content.ch_width > 1
            || !content.attr.fits_compact()
            || !matches!(content.fg, Color::Default | Color::Palette(_))
            || !matches!(content.bg, Color::Default | Color::Palette(_))
            || matches!(content.fg, Color::Rgb(..))
            || matches!(content.bg, Color::Rgb(..))
            || !matches!(content.us, Color::Default);

        if needs_extended {
            let ext_idx = self.extended.len();
            self.extended.push(ExtendedCell {
                ch: content.ch,
                ch_len: content.ch_len,
                ch_width: content.ch_width,
                attr: content.attr,
                fg: content.fg,
                bg: content.bg,
                us: content.us,
            });
            let c = &mut self.compact[col];
            c.ch = 0xFF;
            c.attr = ((ext_idx >> 16) & 0xFF) as u8;
            c.fg = ((ext_idx >> 8) & 0xFF) as u8;
            c.bg = (ext_idx & 0xFF) as u8;
            c.flags = CompactCell::EXTENDED | CompactCell::DIRTY;
        } else {
            let c = &mut self.compact[col];
            c.ch = if content.ch_len == 1 {
                content.ch[0]
            } else {
                b' '
            };
            c.attr = content.attr.basic();
            c.fg = match content.fg {
                Color::Palette(p) => p,
                _ => 0,
            };
            c.bg = match content.bg {
                Color::Palette(p) => p,
                _ => 0,
            };
            c.flags = CompactCell::DIRTY;
        }

        // Handle wide characters: mark the next cell as a continuation
        if content.ch_width == 2 && (col + 1) < self.compact.len() {
            let next = &mut self.compact[col + 1];
            next.ch = b' ';
            next.attr = 0;
            next.fg = 0;
            next.bg = 0;
            next.flags = CompactCell::WIDE_CONT | CompactCell::DIRTY;
        }
    }

    /// Get a resolved cell at a column.
    pub fn get_cell(&self, col: u32) -> CellContent {
        let col = col as usize;
        if col >= self.compact.len() {
            return CellContent::default();
        }
        let c = &self.compact[col];
        if c.is_extended() {
            let idx = c.extended_idx();
            if idx < self.extended.len() {
                let e = &self.extended[idx];
                return CellContent {
                    ch: e.ch,
                    ch_len: e.ch_len,
                    ch_width: e.ch_width,
                    attr: e.attr,
                    fg: e.fg,
                    bg: e.bg,
                    us: e.us,
                };
            }
        }
        CellContent {
            ch: {
                let mut buf = [0u8; 8];
                buf[0] = c.ch;
                buf
            },
            ch_len: 1,
            ch_width: 1,
            attr: CellAttr(c.attr as u16),
            fg: if c.fg == 0 {
                Color::Default
            } else {
                Color::Palette(c.fg)
            },
            bg: if c.bg == 0 {
                Color::Default
            } else {
                Color::Palette(c.bg)
            },
            us: Color::Default,
        }
    }

    /// Clear cells from `start` to `end` (exclusive).
    pub fn clear_range(&mut self, start: u32, end: u32, content: &CellContent) {
        let start = start as usize;
        let end = (end as usize).min(self.compact.len());
        for col in start..end {
            let c = &mut self.compact[col];
            c.ch = b' ';
            c.attr = content.attr.basic();
            c.fg = match content.fg {
                Color::Palette(p) => p,
                _ => 0,
            };
            c.bg = match content.bg {
                Color::Palette(p) => p,
                _ => 0,
            };
            c.flags = CompactCell::DIRTY;
        }
    }

    /// Mark all cells dirty.
    pub fn mark_dirty(&mut self) {
        for c in &mut self.compact {
            c.flags |= CompactCell::DIRTY;
        }
    }

    /// Clear and resize to width, reusing the existing allocation.
    pub fn clear_to(&mut self, width: u32) {
        let width = width as usize;
        self.compact.clear();
        self.compact.resize(width, CompactCell::default());
        self.extended.clear();
        self.flags = LineFlags::default();
    }

    /// Resize this line to a new width. Only grows — never truncates existing
    /// content, so data is preserved when a pane shrinks then expands.
    pub fn resize(&mut self, new_width: u32) {
        let new_width = new_width as usize;
        if new_width > self.compact.len() {
            self.compact
                .resize(new_width, CompactCell::default());
        }
        // Mark the whole line dirty after resize
        self.mark_dirty();
    }
}

/// Resolved cell content used for get/set operations.
#[derive(Clone, Copy)]
pub struct CellContent {
    pub ch: [u8; 8],
    pub ch_len: u8,
    pub ch_width: u8,
    pub attr: CellAttr,
    pub fg: Color,
    pub bg: Color,
    pub us: Color,
}

impl Default for CellContent {
    fn default() -> Self {
        Self {
            ch: {
                let mut buf = [0u8; 8];
                buf[0] = b' ';
                buf
            },
            ch_len: 1,
            ch_width: 1,
            attr: CellAttr::default(),
            fg: Color::Default,
            bg: Color::Default,
            us: Color::Default,
        }
    }
}

impl CellContent {
    pub fn ch_str(&self) -> &str {
        std::str::from_utf8(&self.ch[..self.ch_len as usize]).unwrap_or(" ")
    }

    pub fn from_ascii(ch: u8) -> Self {
        let mut c = Self::default();
        c.ch[0] = ch;
        c
    }
}

/// The grid: ring buffer of lines with visible area + scrollback history.
pub struct Grid {
    pub lines: VecDeque<GridLine>,
    pub sx: u32,
    pub sy: u32,
    pub hlimit: u32,
}

impl Grid {
    pub fn new(sx: u32, sy: u32, hlimit: u32) -> Self {
        let mut lines = VecDeque::with_capacity(sy as usize);
        for _ in 0..sy {
            lines.push_back(GridLine::new(sx));
        }
        Self {
            lines,
            sx,
            sy,
            hlimit,
        }
    }

    /// Number of history lines (lines above the visible area).
    pub fn hsize(&self) -> u32 {
        self.lines.len().saturating_sub(self.sy as usize) as u32
    }

    /// Get a line by absolute index (0 = oldest history line).
    pub fn line(&self, idx: u32) -> Option<&GridLine> {
        self.lines.get(idx as usize)
    }

    /// Get a mutable line by absolute index.
    pub fn line_mut(&mut self, idx: u32) -> Option<&mut GridLine> {
        self.lines.get_mut(idx as usize)
    }

    /// Get a visible line (0 = top of visible area).
    pub fn visible_line(&self, row: u32) -> Option<&GridLine> {
        let abs = self.hsize() + row;
        self.line(abs)
    }

    /// Get a mutable visible line.
    pub fn visible_line_mut(&mut self, row: u32) -> Option<&mut GridLine> {
        let abs = self.hsize() + row;
        self.line_mut(abs)
    }

    /// Scroll up: move the top visible line into history, add a new blank line at bottom.
    /// If we exceed hlimit, drop the oldest history line.
    pub fn scroll_up(&mut self, top: u32, bottom: u32) {
        if top == 0 && bottom == self.sy.saturating_sub(1) {
            // Reuse a discarded history line if available, avoiding allocation
            let new_line = if self.hsize() >= self.hlimit {
                let mut recycled = self.lines.pop_front().unwrap();
                recycled.clear_to(self.sx);
                recycled
            } else {
                GridLine::new(self.sx)
            };
            self.lines.push_back(new_line);
            // Mark ALL visible lines dirty — every row now shows a different line
            let hsize = self.hsize();
            for row in 0..self.sy {
                let abs = (hsize + row) as usize;
                if let Some(line) = self.lines.get_mut(abs) {
                    line.mark_dirty();
                }
            }
        } else {
            // Scroll region: remove line at `top` of region, insert blank at `bottom`
            let hsize = self.hsize();
            let abs_top = hsize + top;
            let abs_bottom = hsize + bottom;
            if abs_top < self.lines.len() as u32 && abs_bottom < self.lines.len() as u32 {
                self.lines.remove(abs_top as usize);
                let new_line = GridLine::new(self.sx);
                self.lines.insert(abs_bottom as usize, new_line);
                // Mark affected lines dirty
                for row in top..=bottom {
                    let abs = hsize + row;
                    if let Some(line) = self.lines.get_mut(abs as usize) {
                        line.mark_dirty();
                    }
                }
            }
        }
    }

    /// Scroll down: insert a blank line at top of region, remove line at bottom.
    pub fn scroll_down(&mut self, top: u32, bottom: u32) {
        let hsize = self.hsize();
        let abs_top = hsize + top;
        let abs_bottom = hsize + bottom;
        if abs_top < self.lines.len() as u32 && abs_bottom < self.lines.len() as u32 {
            self.lines.remove(abs_bottom as usize);
            let new_line = GridLine::new(self.sx);
            self.lines.insert(abs_top as usize, new_line);
            // Mark affected lines dirty
            for row in top..=bottom {
                let abs = hsize + row;
                if let Some(line) = self.lines.get_mut(abs as usize) {
                    line.mark_dirty();
                }
            }
        }
    }

    /// Resize the grid to new dimensions with reflow.
    /// Lines marked WRAPPED are joined and re-split at the new width.
    pub fn resize(&mut self, new_sx: u32, new_sy: u32) {
        if new_sx != self.sx && new_sx > 0 {
            self.reflow(new_sx);
        } else {
            // Just ensure lines are wide enough
            for line in &mut self.lines {
                line.resize(new_sx);
            }
        }
        self.sx = new_sx;

        // Adjust number of visible lines
        let total = self.lines.len() as u32;
        if new_sy > total {
            for _ in 0..(new_sy - total) {
                self.lines.push_back(GridLine::new(new_sx));
            }
        }
        self.sy = new_sy;
    }

    /// Reflow all lines to a new width.
    /// Consecutive WRAPPED lines are joined into a logical line,
    /// then re-split at the new width.
    fn reflow(&mut self, new_sx: u32) {
        let old_lines: Vec<GridLine> = std::mem::take(&mut self.lines).into();
        let mut new_lines = VecDeque::with_capacity(old_lines.len());
        let new_sx_usize = new_sx as usize;

        // Reusable buffer — cleared between logical lines, not reallocated
        let mut compact_buf: Vec<CompactCell> = Vec::with_capacity(256);
        let mut ext_buf: Vec<ExtendedCell> = Vec::new();

        let mut i = 0;
        while i < old_lines.len() {
            compact_buf.clear();
            ext_buf.clear();

            // Join wrapped lines into one logical line (compact cells + extended)
            loop {
                let line = &old_lines[i];
                let wrapped = line.flags.has(LineFlags::WRAPPED);

                for c in &line.compact {
                    if c.is_extended() {
                        // Remap extended index into our merged ext_buf
                        let old_idx = c.extended_idx();
                        if old_idx < line.extended.len() {
                            let new_idx = ext_buf.len();
                            ext_buf.push(line.extended[old_idx]);
                            let mut new_c = *c;
                            new_c.attr = ((new_idx >> 16) & 0xFF) as u8;
                            new_c.fg = ((new_idx >> 8) & 0xFF) as u8;
                            new_c.bg = (new_idx & 0xFF) as u8;
                            compact_buf.push(new_c);
                        } else {
                            compact_buf.push(CompactCell::default());
                        }
                    } else {
                        compact_buf.push(*c);
                    }
                }

                i += 1;
                if !wrapped || i >= old_lines.len() {
                    break;
                }
            }

            // Trim trailing default spaces
            while compact_buf.last().is_some_and(|c| {
                !c.is_extended() && c.ch == b' ' && c.attr == 0 && c.fg == 0 && c.bg == 0
            }) {
                compact_buf.pop();
            }

            // Split into new lines at new_sx
            if compact_buf.is_empty() {
                new_lines.push_back(GridLine {
                    compact: vec![CompactCell::default(); new_sx_usize],
                    extended: Vec::new(),
                    flags: LineFlags::default(),
                });
            } else {
                let nchunks = (compact_buf.len() + new_sx_usize - 1) / new_sx_usize;
                for ci in 0..nchunks {
                    let start = ci * new_sx_usize;
                    let end = (start + new_sx_usize).min(compact_buf.len());
                    let chunk = &compact_buf[start..end];

                    // Build the new line's compact vec
                    let mut new_compact = Vec::with_capacity(new_sx_usize);
                    // Collect extended cells referenced by this chunk
                    let mut new_ext = Vec::new();
                    for c in chunk {
                        if c.is_extended() {
                            let old_idx = c.extended_idx();
                            if old_idx < ext_buf.len() {
                                let new_idx = new_ext.len();
                                new_ext.push(ext_buf[old_idx]);
                                let mut new_c = *c;
                                new_c.attr = ((new_idx >> 16) & 0xFF) as u8;
                                new_c.fg = ((new_idx >> 8) & 0xFF) as u8;
                                new_c.bg = (new_idx & 0xFF) as u8;
                                new_compact.push(new_c);
                            } else {
                                new_compact.push(CompactCell::default());
                            }
                        } else {
                            let mut c = *c;
                            c.flags |= CompactCell::DIRTY;
                            new_compact.push(c);
                        }
                    }
                    // Pad to new_sx
                    new_compact.resize(new_sx_usize, CompactCell::default());

                    new_lines.push_back(GridLine {
                        compact: new_compact,
                        extended: new_ext,
                        flags: if ci < nchunks - 1 {
                            LineFlags(LineFlags::WRAPPED)
                        } else {
                            LineFlags::default()
                        },
                    });
                }
            }
        }

        self.lines = new_lines;

        while self.lines.len() > (self.sy + self.hlimit) as usize {
            self.lines.pop_front();
        }
    }

    /// Clear all content.
    pub fn clear(&mut self) {
        self.lines.clear();
        for _ in 0..self.sy {
            self.lines.push_back(GridLine::new(self.sx));
        }
    }

    /// Mark all visible lines dirty.
    pub fn mark_all_dirty(&mut self) {
        let hsize = self.hsize();
        for row in 0..self.sy {
            let abs = (hsize + row) as usize;
            if let Some(line) = self.lines.get_mut(abs) {
                line.mark_dirty();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grid_new() {
        let grid = Grid::new(80, 24, 1000);
        assert_eq!(grid.sx, 80);
        assert_eq!(grid.sy, 24);
        assert_eq!(grid.hsize(), 0);
        assert_eq!(grid.lines.len(), 24);
    }

    #[test]
    fn test_grid_scroll_up() {
        let mut grid = Grid::new(80, 24, 1000);
        // Write to first visible line
        let content = CellContent::from_ascii(b'A');
        grid.visible_line_mut(0).unwrap().set_cell(0, &content);

        grid.scroll_up(0, 23);

        // First visible line should now be blank
        let cell = grid.visible_line(0).unwrap().get_cell(0);
        assert_eq!(cell.ch[0], b' ');

        // History should have one line with 'A'
        assert_eq!(grid.hsize(), 1);
        let hist = grid.line(0).unwrap().get_cell(0);
        assert_eq!(hist.ch[0], b'A');
    }

    #[test]
    fn test_grid_history_limit() {
        let mut grid = Grid::new(80, 5, 3);
        // Scroll more than hlimit
        for _ in 0..10 {
            grid.scroll_up(0, 4);
        }
        assert!(grid.hsize() <= 3);
    }

    #[test]
    fn test_compact_cell_ascii() {
        let mut line = GridLine::new(80);
        let content = CellContent {
            ch: { let mut b = [0u8; 8]; b[0] = b'X'; b },
            ch_len: 1,
            ch_width: 1,
            attr: CellAttr::default(),
            fg: Color::Palette(1),
            bg: Color::Default,
            us: Color::Default,
        };
        line.set_cell(5, &content);

        let got = line.get_cell(5);
        assert_eq!(got.ch[0], b'X');
        assert_eq!(got.fg, Color::Palette(1));
        assert!(line.compact[5].is_dirty());
    }

    #[test]
    fn test_extended_cell_rgb() {
        let mut line = GridLine::new(80);
        let content = CellContent {
            ch: { let mut b = [0u8; 8]; b[0] = b'Z'; b },
            ch_len: 1,
            ch_width: 1,
            attr: CellAttr::default(),
            fg: Color::Rgb(255, 0, 128),
            bg: Color::Default,
            us: Color::Default,
        };
        line.set_cell(3, &content);

        assert!(line.compact[3].is_extended());
        let got = line.get_cell(3);
        assert_eq!(got.fg, Color::Rgb(255, 0, 128));
    }

    #[test]
    fn test_grid_resize() {
        let mut grid = Grid::new(80, 24, 1000);
        grid.resize(40, 12);
        assert_eq!(grid.sx, 40);
        assert_eq!(grid.sy, 12);
        // Existing lines keep their data (>= new width), never truncated
        for line in &grid.lines {
            assert!(line.compact.len() >= 40);
        }
    }

    #[test]
    fn test_grid_resize_preserves_data() {
        let mut grid = Grid::new(80, 5, 1000);
        // Write to column 60
        let content = CellContent::from_ascii(b'X');
        grid.visible_line_mut(0).unwrap().set_cell(60, &content);
        // Shrink to 40 cols
        grid.resize(40, 5);
        // Expand back to 80
        grid.resize(80, 5);
        // Data at column 60 should still be there
        let cell = grid.visible_line(0).unwrap().get_cell(60);
        assert_eq!(cell.ch[0], b'X');
    }

    #[test]
    fn test_reflow_wrap() {
        // Write 10 chars on a 10-col grid, shrink to 5 — should wrap into 2 lines
        let mut grid = Grid::new(10, 5, 100);
        for i in 0..10u8 {
            let content = CellContent::from_ascii(b'A' + i);
            grid.visible_line_mut(0).unwrap().set_cell(i as u32, &content);
        }
        grid.visible_line_mut(0).unwrap().flags = LineFlags(LineFlags::WRAPPED);

        for i in 0..5u8 {
            let content = CellContent::from_ascii(b'a' + i);
            grid.visible_line_mut(1).unwrap().set_cell(i as u32, &content);
        }

        // Shrink to 5 cols — "ABCDEFGHIJabcde" becomes 3 lines
        grid.resize(5, 5);

        // Use absolute lines (reflow may shift content into history)
        // Find where 'A' starts
        let mut found = None;
        for idx in 0..grid.lines.len() {
            let cell = grid.lines[idx].get_cell(0);
            if cell.ch[0] == b'A' {
                found = Some(idx);
                break;
            }
        }
        let start = found.expect("should find 'A'");

        let row0 = &grid.lines[start];
        assert_eq!(row0.get_cell(0).ch[0], b'A');
        assert_eq!(row0.get_cell(4).ch[0], b'E');
        assert!(row0.flags.has(LineFlags::WRAPPED));

        let row1 = &grid.lines[start + 1];
        assert_eq!(row1.get_cell(0).ch[0], b'F');
        assert_eq!(row1.get_cell(4).ch[0], b'J');
        assert!(row1.flags.has(LineFlags::WRAPPED));

        let row2 = &grid.lines[start + 2];
        assert_eq!(row2.get_cell(0).ch[0], b'a');
        assert_eq!(row2.get_cell(4).ch[0], b'e');
        assert!(!row2.flags.has(LineFlags::WRAPPED));
    }

    #[test]
    fn test_reflow_unwrap() {
        // 2 wrapped lines of 5 cols each, expand to 10 — should merge into 1 line
        let mut grid = Grid::new(5, 5, 100);
        for i in 0..5u8 {
            let content = CellContent::from_ascii(b'A' + i);
            grid.visible_line_mut(0).unwrap().set_cell(i as u32, &content);
        }
        grid.visible_line_mut(0).unwrap().flags = LineFlags(LineFlags::WRAPPED);

        for i in 0..5u8 {
            let content = CellContent::from_ascii(b'F' + i);
            grid.visible_line_mut(1).unwrap().set_cell(i as u32, &content);
        }

        grid.resize(10, 5);

        // Find where 'A' starts
        let mut found = None;
        for idx in 0..grid.lines.len() {
            let cell = grid.lines[idx].get_cell(0);
            if cell.ch[0] == b'A' {
                found = Some(idx);
                break;
            }
        }
        let start = found.expect("should find 'A'");

        let row0 = &grid.lines[start];
        assert_eq!(row0.get_cell(0).ch[0], b'A');
        assert_eq!(row0.get_cell(4).ch[0], b'E');
        assert_eq!(row0.get_cell(5).ch[0], b'F');
        assert_eq!(row0.get_cell(9).ch[0], b'J');
        assert!(!row0.flags.has(LineFlags::WRAPPED));
    }

    #[test]
    fn test_scroll_down_content_shifts() {
        let mut grid = Grid::new(10, 5, 100);
        // Write 'A' on the top visible line
        let content = CellContent::from_ascii(b'A');
        grid.visible_line_mut(0).unwrap().set_cell(0, &content);
        // Write 'B' on the second visible line
        let content_b = CellContent::from_ascii(b'B');
        grid.visible_line_mut(1).unwrap().set_cell(0, &content_b);

        grid.scroll_down(0, 4);

        // After scroll_down: new blank line at top, old lines shift down by 1
        let top_cell = grid.visible_line(0).unwrap().get_cell(0);
        assert_eq!(top_cell.ch[0], b' '); // new blank line at top

        let row1_cell = grid.visible_line(1).unwrap().get_cell(0);
        assert_eq!(row1_cell.ch[0], b'A'); // 'A' moved from row 0 to row 1

        let row2_cell = grid.visible_line(2).unwrap().get_cell(0);
        assert_eq!(row2_cell.ch[0], b'B'); // 'B' moved from row 1 to row 2
    }

    #[test]
    fn test_scroll_up_with_scroll_region() {
        let mut grid = Grid::new(10, 5, 100);
        // Write identifiable chars on each row
        for row in 0..5u32 {
            let content = CellContent::from_ascii(b'A' + row as u8);
            grid.visible_line_mut(row).unwrap().set_cell(0, &content);
        }

        // Scroll up only the region rows 1..3
        grid.scroll_up(1, 3);

        // Row 0 should be untouched
        assert_eq!(grid.visible_line(0).unwrap().get_cell(0).ch[0], b'A');
        // Row 1 should now have what was on row 2 ('C')
        assert_eq!(grid.visible_line(1).unwrap().get_cell(0).ch[0], b'C');
        // Row 2 should now have what was on row 3 ('D')
        assert_eq!(grid.visible_line(2).unwrap().get_cell(0).ch[0], b'D');
        // Row 3 should be blank (new line)
        assert_eq!(grid.visible_line(3).unwrap().get_cell(0).ch[0], b' ');
        // Row 4 should be untouched
        assert_eq!(grid.visible_line(4).unwrap().get_cell(0).ch[0], b'E');
    }

    #[test]
    fn test_scroll_up_marks_all_visible_dirty() {
        let mut grid = Grid::new(10, 5, 100);
        // Clear all dirty flags first
        for row in 0..5u32 {
            if let Some(line) = grid.visible_line_mut(row) {
                for c in &mut line.compact {
                    c.clear_dirty();
                }
            }
        }

        // Full-screen scroll up
        grid.scroll_up(0, 4);

        // All visible lines should be dirty
        for row in 0..5u32 {
            let line = grid.visible_line(row).unwrap();
            assert!(
                line.compact[0].is_dirty(),
                "row {row} should be dirty after scroll_up"
            );
        }
    }

    #[test]
    fn test_gridline_clear_range() {
        let mut line = GridLine::new(10);
        // Fill with 'X'
        for col in 0..10u32 {
            let content = CellContent::from_ascii(b'X');
            line.set_cell(col, &content);
        }
        // Clear dirty flags
        for c in &mut line.compact {
            c.clear_dirty();
        }

        let blank = CellContent::default();
        line.clear_range(3, 7, &blank);

        // Columns 0-2 should still be 'X'
        for col in 0..3u32 {
            assert_eq!(line.get_cell(col).ch[0], b'X');
        }
        // Columns 3-6 should be cleared to space
        for col in 3..7u32 {
            assert_eq!(line.get_cell(col).ch[0], b' ');
            assert!(line.compact[col as usize].is_dirty());
        }
        // Columns 7-9 should still be 'X'
        for col in 7..10u32 {
            assert_eq!(line.get_cell(col).ch[0], b'X');
        }
    }

    #[test]
    fn test_gridline_extended_cell_rgb_roundtrip() {
        let mut line = GridLine::new(10);
        let content = CellContent {
            ch: {
                let mut b = [0u8; 8];
                b[0] = b'Q';
                b
            },
            ch_len: 1,
            ch_width: 1,
            attr: CellAttr::default(),
            fg: Color::Rgb(10, 20, 30),
            bg: Color::Rgb(100, 200, 255),
            us: Color::Default,
        };
        line.set_cell(2, &content);

        assert!(line.compact[2].is_extended());
        let got = line.get_cell(2);
        assert_eq!(got.ch[0], b'Q');
        assert_eq!(got.fg, Color::Rgb(10, 20, 30));
        assert_eq!(got.bg, Color::Rgb(100, 200, 255));
    }

    #[test]
    fn test_hsize_after_multiple_scrolls() {
        let mut grid = Grid::new(10, 5, 100);
        assert_eq!(grid.hsize(), 0);

        // Each full-screen scroll_up adds one history line
        grid.scroll_up(0, 4);
        assert_eq!(grid.hsize(), 1);

        grid.scroll_up(0, 4);
        assert_eq!(grid.hsize(), 2);

        grid.scroll_up(0, 4);
        assert_eq!(grid.hsize(), 3);

        // Verify total line count = history + visible
        assert_eq!(grid.lines.len(), (grid.hsize() + grid.sy) as usize);
    }

    #[test]
    fn test_reflow_empty_lines_pass_through() {
        // Grid with all blank lines
        let mut grid = Grid::new(10, 5, 100);
        // No content written — all lines are blank

        // Reflow to a different width
        grid.resize(20, 5);

        // Should still have at least 5 visible lines, all blank
        assert!(grid.lines.len() >= 5);
        for row in 0..5u32 {
            let cell = grid.visible_line(row).unwrap().get_cell(0);
            assert_eq!(cell.ch[0], b' ');
        }
    }

    #[test]
    fn test_reflow_roundtrip_wrap_unwrap() {
        // Write 8 chars on a 10-col grid, shrink to 5, expand back to 10
        let mut grid = Grid::new(10, 5, 100);
        let chars = b"ABCDEFGH";
        for (i, &ch) in chars.iter().enumerate() {
            let content = CellContent::from_ascii(ch);
            grid.visible_line_mut(0).unwrap().set_cell(i as u32, &content);
        }

        // Shrink to 5: "ABCDEFGH" splits into "ABCDE" (wrapped) + "FGH"
        grid.resize(5, 5);

        // Expand back to 10: should merge back into one line "ABCDEFGH"
        grid.resize(10, 5);

        // Find where 'A' starts
        let mut found = None;
        for idx in 0..grid.lines.len() {
            let cell = grid.lines[idx].get_cell(0);
            if cell.ch[0] == b'A' {
                found = Some(idx);
                break;
            }
        }
        let start = found.expect("should find 'A'");
        let row = &grid.lines[start];

        // All 8 chars should be on one line
        for (i, &ch) in chars.iter().enumerate() {
            assert_eq!(row.get_cell(i as u32).ch[0], ch);
        }
        assert!(!row.flags.has(LineFlags::WRAPPED));
    }

}
