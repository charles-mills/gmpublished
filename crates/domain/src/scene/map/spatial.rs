use super::{MapBounds, Vec3};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct BoundsTree {
    nodes: Vec<BoundsNode>,
    indices: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BoundsNode {
    bounds: MapBounds,
    kind: NodeKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum NodeKind {
    Leaf { first: usize, count: usize },
    Branch { left: usize, right: usize },
}

impl BoundsTree {
    pub fn new(bounds: impl Iterator<Item = MapBounds>) -> Self {
        let bounds = bounds.collect::<Vec<_>>();
        let mut tree = Self {
            nodes: Vec::new(),
            indices: (0..bounds.len()).collect(),
        };
        if !bounds.is_empty() {
            tree.build(&bounds, 0, bounds.len());
        }
        tree
    }

    fn build(&mut self, source: &[MapBounds], first: usize, count: usize) -> usize {
        let mut bounds = source[self.indices[first]];
        for &index in &self.indices[first + 1..first + count] {
            for axis in 0..3 {
                bounds.min[axis] = bounds.min[axis].min(source[index].min[axis]);
                bounds.max[axis] = bounds.max[axis].max(source[index].max[axis]);
            }
        }
        let node = self.nodes.len();
        self.nodes.push(BoundsNode {
            bounds,
            kind: NodeKind::Leaf { first, count },
        });
        if count > 8 {
            let extent = bounds.max - bounds.min;
            let axis = (0..3)
                .max_by(|&a, &b| extent[a].total_cmp(&extent[b]))
                .unwrap_or(0);
            let half = count / 2;
            self.indices[first..first + count].select_nth_unstable_by(half, |&a, &b| {
                let a_center = source[a].min[axis] + source[a].max[axis];
                let b_center = source[b].min[axis] + source[b].max[axis];
                a_center.total_cmp(&b_center).then(a.cmp(&b))
            });
            let left = self.build(source, first, half);
            let right = self.build(source, first + half, count - half);
            self.nodes[node].kind = NodeKind::Branch { left, right };
        }
        node
    }

    pub fn query(&self, start: Vec3, end: Vec3) -> Vec<usize> {
        if self.nodes.is_empty() {
            return Vec::new();
        }
        let direction = end - start;
        let mut stack = vec![0];
        let mut candidates = Vec::new();
        while let Some(index) = stack.pop() {
            let node = self.nodes[index];
            if !intersects_segment(node.bounds, start, direction) {
                continue;
            }
            match node.kind {
                NodeKind::Leaf { first, count } => {
                    candidates.extend_from_slice(&self.indices[first..first + count]);
                }
                NodeKind::Branch { left, right } => {
                    stack.push(right);
                    stack.push(left);
                }
            }
        }
        // Equal-fraction surfaces and overlapping solid brushes retain the
        // original compiled order, independently of spatial partitioning.
        candidates.sort_unstable();
        candidates
    }
}

fn intersects_segment(bounds: MapBounds, start: Vec3, direction: Vec3) -> bool {
    let mut enter = 0.0_f32;
    let mut leave = 1.0_f32;
    for axis in 0..3 {
        if direction[axis] == 0.0 {
            if start[axis] < bounds.min[axis] || start[axis] > bounds.max[axis] {
                return false;
            }
            continue;
        }
        let a = (bounds.min[axis] - start[axis]) / direction[axis];
        let b = (bounds.max[axis] - start[axis]) / direction[axis];
        enter = enter.max(a.min(b));
        leave = leave.min(a.max(b));
        if enter > leave {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_query_is_conservative_bounded_and_preserves_source_order() {
        let bounds = (0..4096)
            .map(|i| MapBounds {
                min: Vec3::new(i as f32 * 16.0, -2.0, -2.0),
                max: Vec3::new(i as f32 * 16.0 + 4.0, 2.0, 2.0),
            })
            .collect::<Vec<_>>();
        let tree = BoundsTree::new(bounds.iter().copied());
        for (start, end) in [
            (Vec3::new(1234.0, 0.0, 0.0), Vec3::new(1400.0, 0.0, 0.0)),
            (Vec3::new(1400.0, 0.0, 0.0), Vec3::new(1234.0, 0.0, 0.0)),
            (Vec3::new(2048.0, 0.0, 0.0), Vec3::new(2048.0, 0.0, 0.0)),
            (Vec3::new(1234.0, 4.0, 0.0), Vec3::new(1400.0, 4.0, 0.0)),
        ] {
            let candidates = tree.query(start, end);
            assert!(candidates.len() <= 32);
            assert!(candidates.windows(2).all(|pair| pair[0] < pair[1]));
            for (index, &bound) in bounds.iter().enumerate() {
                if intersects_segment(bound, start, end - start) {
                    assert!(candidates.binary_search(&index).is_ok());
                }
            }
        }
        assert!(
            BoundsTree::new(std::iter::empty())
                .query(Vec3::ZERO, Vec3::splat(1.0))
                .is_empty()
        );
    }
}
