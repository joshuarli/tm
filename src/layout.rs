use crate::state::PaneId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDir {
    Horizontal, // children side-by-side (columns)
    Vertical,   // children stacked (rows)
}

#[derive(Clone, Debug)]
pub enum LayoutNode {
    Pane(PaneId),
    Split {
        dir: SplitDir,
        children: Vec<LayoutNode>,
        /// Absolute sizes per child (width for Horizontal, height for Vertical).
        /// When empty, children are distributed evenly.
        sizes: Vec<u32>,
    },
}

/// Calculated position and size for a pane.
#[derive(Clone, Copy, Debug)]
pub struct PaneGeometry {
    pub id: PaneId,
    pub xoff: u32,
    pub yoff: u32,
    pub sx: u32,
    pub sy: u32,
}

impl LayoutNode {
    /// Calculate positions and sizes for all panes in this layout tree.
    pub fn calculate(&self, xoff: u32, yoff: u32, sx: u32, sy: u32) -> Vec<PaneGeometry> {
        let mut result = Vec::new();
        self.calculate_inner(xoff, yoff, sx, sy, &mut result);
        result
    }

    fn calculate_inner(&self, xoff: u32, yoff: u32, sx: u32, sy: u32, out: &mut Vec<PaneGeometry>) {
        match self {
            LayoutNode::Pane(id) => {
                out.push(PaneGeometry {
                    id: *id,
                    xoff,
                    yoff,
                    sx,
                    sy,
                });
            }
            LayoutNode::Split {
                dir,
                children,
                sizes,
            } => {
                if children.is_empty() {
                    return;
                }
                let n = children.len() as u32;
                let separators = n - 1;
                match dir {
                    SplitDir::Horizontal => {
                        let avail = sx.saturating_sub(separators);
                        let mut x = xoff;
                        for (i, child) in children.iter().enumerate() {
                            let w = if sizes.len() == children.len() {
                                sizes[i]
                            } else {
                                let base = avail / n;
                                base + if (i as u32) < avail % n { 1 } else { 0 }
                            };
                            child.calculate_inner(x, yoff, w, sy, out);
                            x += w + 1; // +1 for separator
                        }
                    }
                    SplitDir::Vertical => {
                        let avail = sy.saturating_sub(separators);
                        let mut y = yoff;
                        for (i, child) in children.iter().enumerate() {
                            let h = if sizes.len() == children.len() {
                                sizes[i]
                            } else {
                                let base = avail / n;
                                base + if (i as u32) < avail % n { 1 } else { 0 }
                            };
                            child.calculate_inner(xoff, y, sx, h, out);
                            y += h + 1; // +1 for separator
                        }
                    }
                }
            }
        }
    }

    /// Find and remove a pane from the layout tree. Returns true if found.
    pub fn remove_pane(&mut self, target: PaneId) -> bool {
        match self {
            LayoutNode::Pane(id) => *id == target,
            LayoutNode::Split {
                children, sizes, ..
            } => {
                // Find and remove the child containing the target
                let idx = children.iter().position(|c| match c {
                    LayoutNode::Pane(id) => *id == target,
                    LayoutNode::Split { .. } => false,
                });
                if let Some(idx) = idx {
                    children.remove(idx);
                    if sizes.len() > idx {
                        sizes.remove(idx);
                    }
                    // Clear sizes so remaining children redistribute evenly
                    sizes.clear();
                    return true;
                }
                // Recurse into split children
                for child in children.iter_mut() {
                    if child.remove_pane(target) {
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Collapse single-child splits after a removal.
    pub fn simplify(&mut self) {
        if let LayoutNode::Split { children, .. } = self {
            // Recursively simplify children first
            for child in children.iter_mut() {
                child.simplify();
            }
            // If only one child remains, replace self with that child
            if children.len() == 1 {
                *self = children.remove(0);
            }
        }
    }

    /// Split a pane. Returns the new layout with the added pane.
    pub fn split_pane(&mut self, target: PaneId, new_pane: PaneId, dir: SplitDir) -> bool {
        match self {
            LayoutNode::Pane(id) if *id == target => {
                // Replace this pane with a split containing both panes
                let old = LayoutNode::Pane(target);
                let new = LayoutNode::Pane(new_pane);
                *self = LayoutNode::Split {
                    dir,
                    children: vec![old, new],
                    sizes: Vec::new(),
                };
                true
            }
            LayoutNode::Split {
                dir: split_dir,
                children,
                sizes,
            } => {
                // Check if target is a direct child and the split direction matches
                if *split_dir == dir {
                    let idx = children
                        .iter()
                        .position(|c| matches!(c, LayoutNode::Pane(id) if *id == target));
                    if let Some(idx) = idx {
                        // Insert new pane after the target in the same split
                        children.insert(idx + 1, LayoutNode::Pane(new_pane));
                        // Clear custom sizes — redistribute evenly
                        sizes.clear();
                        return true;
                    }
                }
                // Recurse into children
                for child in children.iter_mut() {
                    if child.split_pane(target, new_pane, dir) {
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Count the number of panes in this layout.
    pub fn pane_count(&self) -> usize {
        match self {
            LayoutNode::Pane(_) => 1,
            LayoutNode::Split { children, .. } => children.iter().map(|c| c.pane_count()).sum(),
        }
    }

    /// Get all pane IDs in order.
    pub fn pane_ids(&self) -> Vec<PaneId> {
        let mut ids = Vec::new();
        self.collect_pane_ids(&mut ids);
        ids
    }

    fn collect_pane_ids(&self, out: &mut Vec<PaneId>) {
        match self {
            LayoutNode::Pane(id) => out.push(*id),
            LayoutNode::Split { children, .. } => {
                for child in children {
                    child.collect_pane_ids(out);
                }
            }
        }
    }

    /// Find the pane at a given position.
    pub fn pane_at(&self, geos: &[PaneGeometry], x: u32, y: u32) -> Option<PaneId> {
        for geo in geos {
            if x >= geo.xoff && x < geo.xoff + geo.sx && y >= geo.yoff && y < geo.yoff + geo.sy {
                return Some(geo.id);
            }
        }
        None
    }

    /// Resize the border adjacent to `pane_id` in the given direction by `delta` cells.
    /// Positive delta grows the pane (takes from the next neighbor), negative shrinks
    /// (gives to the previous neighbor). `total` is the available space in the resize
    /// direction for the current subtree.
    /// Returns true if the resize succeeded.
    pub fn resize_pane(&mut self, pane_id: PaneId, dir: SplitDir, delta: i32, total: u32) -> bool {
        let LayoutNode::Split {
            dir: split_dir,
            children,
            sizes,
        } = self
        else {
            return false;
        };

        let Some(idx) = children.iter().position(|c| c.contains_pane(pane_id)) else {
            return false;
        };

        // If this split's direction matches the resize direction, resize here
        if *split_dir == dir {
            let n = children.len();
            let separators = (n as u32).saturating_sub(1);
            let avail = total.saturating_sub(separators);

            // Initialize sizes from even distribution if not set
            if sizes.len() != n {
                let base = avail / n as u32;
                let extra = avail % n as u32;
                *sizes = (0..n)
                    .map(|i| base + if (i as u32) < extra { 1 } else { 0 })
                    .collect();
            }

            // Pick neighbor: prefer next sibling, fall back to previous
            let neighbor = if idx + 1 < n {
                idx + 1
            } else if idx > 0 {
                idx - 1
            } else {
                return false;
            };

            let abs = delta.unsigned_abs();
            let min_size = 2u32;
            if delta > 0 {
                // Grow this pane, shrink neighbor
                let actual = abs.min(sizes[neighbor].saturating_sub(min_size));
                if actual == 0 {
                    return false;
                }
                sizes[idx] += actual;
                sizes[neighbor] -= actual;
            } else {
                // Shrink this pane, grow neighbor
                let actual = abs.min(sizes[idx].saturating_sub(min_size));
                if actual == 0 {
                    return false;
                }
                sizes[idx] -= actual;
                sizes[neighbor] += actual;
            }
            return true;
        }

        // Wrong direction — recurse into the child that contains the pane.
        // Pass the total for the resize direction through unchanged (this split
        // is perpendicular so it doesn't subdivide that dimension).
        children[idx].resize_pane(pane_id, dir, delta, total)
    }

    /// Check if this subtree contains the given pane.
    pub fn contains_pane(&self, pane_id: PaneId) -> bool {
        match self {
            LayoutNode::Pane(id) => *id == pane_id,
            LayoutNode::Split { children, .. } => children.iter().any(|c| c.contains_pane(pane_id)),
        }
    }

    /// Find a border at the given coordinates. Returns the direction of the border
    /// and the pane on the "before" side (left or above the border).
    pub fn border_at(geos: &[PaneGeometry], x: u32, y: u32) -> Option<(SplitDir, PaneId)> {
        for geo in geos {
            // Right border: x == xoff + sx, within the pane's vertical range
            if x == geo.xoff + geo.sx && y >= geo.yoff && y < geo.yoff + geo.sy {
                return Some((SplitDir::Horizontal, geo.id));
            }
            // Bottom border: y == yoff + sy, within the pane's horizontal range
            if y == geo.yoff + geo.sy && x >= geo.xoff && x < geo.xoff + geo.sx {
                return Some((SplitDir::Vertical, geo.id));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_pane() {
        let layout = LayoutNode::Pane(PaneId(0));
        let geos = layout.calculate(0, 0, 80, 24);
        assert_eq!(geos.len(), 1);
        assert_eq!(geos[0].sx, 80);
        assert_eq!(geos[0].sy, 24);
    }

    #[test]
    fn test_horizontal_split() {
        let layout = LayoutNode::Split {
            dir: SplitDir::Horizontal,
            children: vec![LayoutNode::Pane(PaneId(0)), LayoutNode::Pane(PaneId(1))],
            sizes: Vec::new(),
        };
        let geos = layout.calculate(0, 0, 81, 24);
        assert_eq!(geos.len(), 2);
        // 81 cols - 1 separator = 80, split evenly = 40 each
        assert_eq!(geos[0].sx, 40);
        assert_eq!(geos[1].sx, 40);
        assert_eq!(geos[0].xoff, 0);
        assert_eq!(geos[1].xoff, 41);
    }

    #[test]
    fn test_split_and_remove() {
        let mut layout = LayoutNode::Pane(PaneId(0));
        layout.split_pane(PaneId(0), PaneId(1), SplitDir::Horizontal);
        assert_eq!(layout.pane_count(), 2);

        layout.remove_pane(PaneId(1));
        layout.simplify();
        assert_eq!(layout.pane_count(), 1);
    }
}
