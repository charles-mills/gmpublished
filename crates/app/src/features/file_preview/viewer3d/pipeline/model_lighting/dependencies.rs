#[derive(Debug)]
pub(in super::super) struct UsedStyles<const N: usize> {
    indices: Vec<usize>,
}

impl<const N: usize> UsedStyles<N> {
    pub fn new(styles: impl IntoIterator<Item = usize>) -> Self {
        let mut indices = styles
            .into_iter()
            .filter(|&style| style < N)
            .collect::<Vec<_>>();
        indices.sort_unstable();
        indices.dedup();
        Self { indices }
    }

    pub fn include(&mut self, styles: impl IntoIterator<Item = usize>) {
        for style in styles.into_iter().filter(|&style| style < N) {
            if let Err(index) = self.indices.binary_search(&style) {
                self.indices.insert(index, style);
            }
        }
    }

    pub fn changed(&self, previous: &[f32; N], next: &[f32; N]) -> bool {
        self.indices
            .iter()
            .any(|&style| previous[style] != next[style])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unused_default_animation_does_not_invalidate_static_or_named_style_lighting() {
        let used = UsedStyles::<256>::new([0, 32, 33]);
        let previous = [1.0; 256];
        let mut next = previous;
        next[2] = 0.0;
        assert!(!used.changed(&previous, &next));
        next[32] = 0.0;
        assert!(used.changed(&previous, &next));
        assert!(used.changed(&next, &previous));
    }

    #[test]
    fn used_default_animation_is_not_suppressed() {
        let used = UsedStyles::<128>::new([2, 2, 127, 128]);
        let previous = [1.0; 128];
        let mut next = previous;
        next[2] = 0.0;
        assert!(used.changed(&previous, &next));
        next[2] = 1.0;
        assert!(!used.changed(&previous, &next));
        next[127] = 0.0;
        assert!(used.changed(&previous, &next));
    }
}
