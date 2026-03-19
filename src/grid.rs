use std::collections::VecDeque;

/// Color representation for cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Color {
    #[default]
    Default,
    Palette(u8),
    Rgb(u8, u8, u8),
}

/// Cell attributes as bitflags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct CellAttr(pub(crate) u16);

impl CellAttr {
    pub(crate) const BOLD: u16 = 0x01;
    pub(crate) const DIM: u16 = 0x02;
    pub(crate) const ITALIC: u16 = 0x04;
    pub(crate) const UNDERLINE: u16 = 0x08;
    pub(crate) const REVERSE: u16 = 0x10;
    pub(crate) const STRIKE: u16 = 0x20;
    pub(crate) const INVISIBLE: u16 = 0x40;
    pub(crate) const CURLY_UNDERLINE: u16 = 0x80;
    pub(crate) const DOUBLE_UNDERLINE: u16 = 0x100;
    pub(crate) const DOTTED_UNDERLINE: u16 = 0x200;
    pub(crate) const DASHED_UNDERLINE: u16 = 0x400;

    pub(crate) fn has(self, flag: u16) -> bool {
        self.0 & flag != 0
    }

    pub(crate) fn set(&mut self, flag: u16) {
        self.0 |= flag;
    }

    pub(crate) fn clear(&mut self, flag: u16) {
        self.0 &= !flag;
    }

    pub(crate) fn basic(self) -> u8 {
        (self.0 & 0xFF) as u8
    }

    pub(crate) fn fits_compact(self) -> bool {
        self.0 <= 0xFF
    }
}

/// Compact cell — 5 bytes. Covers ASCII with 256-color palette.
#[derive(Clone, Copy)]
pub(crate) struct CompactCell {
    pub(crate) ch: u8,    // ASCII byte, or 0xFF → extended
    pub(crate) attr: u8,  // basic attributes
    pub(crate) fg: u8,    // palette index
    pub(crate) bg: u8,    // palette index
    pub(crate) flags: u8, // EXTENDED | DIRTY | WIDE_CONTINUATION
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
    pub(crate) const EXTENDED: u8 = 0x01;
    pub(crate) const DIRTY: u8 = 0x02;
    pub(crate) const WIDE_CONT: u8 = 0x04;

    pub(crate) fn is_extended(self) -> bool {
        self.flags & Self::EXTENDED != 0
    }

    pub(crate) fn is_dirty(self) -> bool {
        self.flags & Self::DIRTY != 0
    }

    pub(crate) fn set_dirty(&mut self) {
        self.flags |= Self::DIRTY;
    }

    pub(crate) fn clear_dirty(&mut self) {
        self.flags &= !Self::DIRTY;
    }

    /// Get the extended index when ch == 0xFF. The attr/fg/bg fields store a u24 index.
    pub(crate) fn extended_idx(self) -> usize {
        ((self.attr as usize) << 16) | ((self.fg as usize) << 8) | (self.bg as usize)
    }
}

/// Extended cell — for Unicode, RGB colors, styled underlines.
#[derive(Clone, Copy, Default)]
pub(crate) struct ExtendedCell {
    pub(crate) ch: [u8; 8],
    pub(crate) ch_len: u8,
    pub(crate) ch_width: u8, // display width (1 or 2)
    pub(crate) attr: CellAttr,
    pub(crate) fg: Color,
    pub(crate) bg: Color,
    pub(crate) us: Color, // underline color
}

impl ExtendedCell {
    pub(crate) fn ch_str(&self) -> &str {
        std::str::from_utf8(&self.ch[..self.ch_len as usize]).unwrap_or(" ")
    }
}

/// Line flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LineFlags(pub(crate) u8);

impl LineFlags {
    pub(crate) const WRAPPED: u8 = 0x01;

    pub(crate) fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

/// A single line in the grid.
pub(crate) struct GridLine {
    pub(crate) compact: Vec<CompactCell>,
    pub(crate) extended: Vec<ExtendedCell>,
    pub(crate) flags: LineFlags,
}

impl GridLine {
    pub(crate) fn new(width: u32) -> Self {
        Self {
            compact: vec![CompactCell::default(); width as usize],
            extended: Vec::new(),
            flags: LineFlags::default(),
        }
    }

    /// Set a cell from a CellContent description. Marks the cell dirty.
    pub(crate) fn set_cell(&mut self, col: u32, content: &CellContent) {
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
    pub(crate) fn get_cell(&self, col: u32) -> CellContent {
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
    pub(crate) fn clear_range(&mut self, start: u32, end: u32, content: &CellContent) {
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
    pub(crate) fn mark_dirty(&mut self) {
        for c in &mut self.compact {
            c.flags |= CompactCell::DIRTY;
        }
    }

    /// Resize this line to a new width. Only grows — never truncates existing
    /// content, so data is preserved when a pane shrinks then expands.
    pub(crate) fn resize(&mut self, new_width: u32) {
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
pub(crate) struct CellContent {
    pub(crate) ch: [u8; 8],
    pub(crate) ch_len: u8,
    pub(crate) ch_width: u8,
    pub(crate) attr: CellAttr,
    pub(crate) fg: Color,
    pub(crate) bg: Color,
    pub(crate) us: Color,
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
    pub(crate) fn ch_str(&self) -> &str {
        std::str::from_utf8(&self.ch[..self.ch_len as usize]).unwrap_or(" ")
    }

    pub(crate) fn from_ascii(ch: u8) -> Self {
        let mut c = Self::default();
        c.ch[0] = ch;
        c
    }
}

/// The grid: ring buffer of lines with visible area + scrollback history.
pub(crate) struct Grid {
    pub(crate) lines: VecDeque<GridLine>,
    pub(crate) sx: u32,
    pub(crate) sy: u32,
    pub(crate) hlimit: u32,
}

impl Grid {
    pub(crate) fn new(sx: u32, sy: u32, hlimit: u32) -> Self {
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
    pub(crate) fn hsize(&self) -> u32 {
        self.lines.len().saturating_sub(self.sy as usize) as u32
    }

    /// Get a line by absolute index (0 = oldest history line).
    pub(crate) fn line(&self, idx: u32) -> Option<&GridLine> {
        self.lines.get(idx as usize)
    }

    /// Get a mutable line by absolute index.
    pub(crate) fn line_mut(&mut self, idx: u32) -> Option<&mut GridLine> {
        self.lines.get_mut(idx as usize)
    }

    /// Get a visible line (0 = top of visible area).
    pub(crate) fn visible_line(&self, row: u32) -> Option<&GridLine> {
        let abs = self.hsize() + row;
        self.line(abs)
    }

    /// Get a mutable visible line.
    pub(crate) fn visible_line_mut(&mut self, row: u32) -> Option<&mut GridLine> {
        let abs = self.hsize() + row;
        self.line_mut(abs)
    }

    /// Scroll up: move the top visible line into history, add a new blank line at bottom.
    /// If we exceed hlimit, drop the oldest history line.
    pub(crate) fn scroll_up(&mut self, top: u32, bottom: u32) {
        // If this is a full-screen scroll (top=0), the top line goes to history
        if top == 0 && bottom == self.sy.saturating_sub(1) {
            self.lines.push_back(GridLine::new(self.sx));
            // Trim history if needed
            while self.hsize() > self.hlimit {
                self.lines.pop_front();
            }
            // Mark the new line dirty
            if let Some(line) = self.lines.back_mut() {
                line.mark_dirty();
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
    pub(crate) fn scroll_down(&mut self, top: u32, bottom: u32) {
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

    /// Resize the grid to new dimensions.
    pub(crate) fn resize(&mut self, new_sx: u32, new_sy: u32) {
        // Resize existing lines to new width
        for line in &mut self.lines {
            line.resize(new_sx);
        }
        self.sx = new_sx;

        // Adjust number of visible lines
        let total = self.lines.len() as u32;
        if new_sy > total {
            // Need more lines
            for _ in 0..(new_sy - total) {
                self.lines.push_back(GridLine::new(new_sx));
            }
        }
        // If new_sy < old sy, history grows (lines become history). That's fine.
        self.sy = new_sy;
    }

    /// Clear all content.
    pub(crate) fn clear(&mut self) {
        self.lines.clear();
        for _ in 0..self.sy {
            self.lines.push_back(GridLine::new(self.sx));
        }
    }

    /// Mark all visible lines dirty.
    pub(crate) fn mark_all_dirty(&mut self) {
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
}
