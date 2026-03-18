use crate::state::PaneId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SplitDir {
    Horizontal, // children side-by-side (columns)
    Vertical,   // children stacked (rows)
}

#[derive(Clone, Debug)]
pub(crate) enum LayoutNode {
    Pane(PaneId),
    Split {
        dir: SplitDir,
        children: Vec<LayoutNode>,
    },
}

/// Calculated position and size for a pane.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PaneGeometry {
    pub(crate) id: PaneId,
    pub(crate) xoff: u32,
    pub(crate) yoff: u32,
    pub(crate) sx: u32,
    pub(crate) sy: u32,
}

impl LayoutNode {
    /// Calculate positions and sizes for all panes in this layout tree.
    pub(crate) fn calculate(&self, xoff: u32, yoff: u32, sx: u32, sy: u32) -> Vec<PaneGeometry> {
        let mut result = Vec::new();
        self.calculate_inner(xoff, yoff, sx, sy, &mut result);
        result
    }

    fn calculate_inner(
        &self,
        xoff: u32,
        yoff: u32,
        sx: u32,
        sy: u32,
        out: &mut Vec<PaneGeometry>,
    ) {
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
            LayoutNode::Split { dir, children } => {
                if children.is_empty() {
                    return;
                }
                let n = children.len() as u32;
                match dir {
                    SplitDir::Horizontal => {
                        // Divide width evenly. Account for separator columns (1 char each).
                        let separators = n - 1;
                        let avail = sx.saturating_sub(separators);
                        let base_w = avail / n;
                        let extra = avail % n;
                        let mut x = xoff;
                        for (i, child) in children.iter().enumerate() {
                            let w = base_w + if (i as u32) < extra { 1 } else { 0 };
                            child.calculate_inner(x, yoff, w, sy, out);
                            x += w + 1; // +1 for separator
                        }
                    }
                    SplitDir::Vertical => {
                        let separators = n - 1;
                        let avail = sy.saturating_sub(separators);
                        let base_h = avail / n;
                        let extra = avail % n;
                        let mut y = yoff;
                        for (i, child) in children.iter().enumerate() {
                            let h = base_h + if (i as u32) < extra { 1 } else { 0 };
                            child.calculate_inner(xoff, y, sx, h, out);
                            y += h + 1; // +1 for separator
                        }
                    }
                }
            }
        }
    }

    /// Find and remove a pane from the layout tree. Returns true if found.
    pub(crate) fn remove_pane(&mut self, target: PaneId) -> bool {
        match self {
            LayoutNode::Pane(id) => *id == target,
            LayoutNode::Split { children, .. } => {
                // Find and remove the child containing the target
                let idx = children.iter().position(|c| match c {
                    LayoutNode::Pane(id) => *id == target,
                    LayoutNode::Split { .. } => false,
                });
                if let Some(idx) = idx {
                    children.remove(idx);
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
    pub(crate) fn simplify(&mut self) {
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
    pub(crate) fn split_pane(
        &mut self,
        target: PaneId,
        new_pane: PaneId,
        dir: SplitDir,
    ) -> bool {
        match self {
            LayoutNode::Pane(id) if *id == target => {
                // Replace this pane with a split containing both panes
                let old = LayoutNode::Pane(target);
                let new = LayoutNode::Pane(new_pane);
                *self = LayoutNode::Split {
                    dir,
                    children: vec![old, new],
                };
                true
            }
            LayoutNode::Split { dir: split_dir, children } => {
                // Check if target is a direct child and the split direction matches
                if *split_dir == dir {
                    let idx = children.iter().position(|c| matches!(c, LayoutNode::Pane(id) if *id == target));
                    if let Some(idx) = idx {
                        // Insert new pane after the target in the same split
                        children.insert(idx + 1, LayoutNode::Pane(new_pane));
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
    pub(crate) fn pane_count(&self) -> usize {
        match self {
            LayoutNode::Pane(_) => 1,
            LayoutNode::Split { children, .. } => children.iter().map(|c| c.pane_count()).sum(),
        }
    }

    /// Get all pane IDs in order.
    pub(crate) fn pane_ids(&self) -> Vec<PaneId> {
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
    pub(crate) fn pane_at(&self, geos: &[PaneGeometry], x: u32, y: u32) -> Option<PaneId> {
        for geo in geos {
            if x >= geo.xoff
                && x < geo.xoff + geo.sx
                && y >= geo.yoff
                && y < geo.yoff + geo.sy
            {
                return Some(geo.id);
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
