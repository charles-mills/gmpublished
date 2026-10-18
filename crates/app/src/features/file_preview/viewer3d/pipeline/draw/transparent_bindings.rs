use super::super::{DrawItem, RenderMode};
use gmpublished_domain::scene::map::LightmapPageId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IndexSource {
    Original,
    Visible,
}

impl IndexSource {
    pub fn select(explicit_range: bool, visible_buffer: Option<bool>) -> Option<Self> {
        if explicit_range || visible_buffer.is_none() {
            Some(Self::Original)
        } else if visible_buffer == Some(true) {
            Some(Self::Visible)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Key {
    pub item: DrawItem,
    pub blend: RenderMode,
    pub source: IndexSource,
}

#[derive(Default)]
pub(super) struct State {
    mesh: Option<Key>,
    // The outer None means unbound; Some(None) is the unlit binding.
    page: Option<Option<LightmapPageId>>,
}

impl State {
    pub fn mesh(&mut self, key: Key) -> bool {
        if self.mesh == Some(key) {
            return false;
        }
        self.mesh = Some(key);
        self.page = None;
        true
    }

    pub fn page(&mut self, page: Option<LightmapPageId>) -> bool {
        if self.page == Some(page) {
            return false;
        }
        self.page = Some(page);
        true
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::Range;

    #[derive(Clone)]
    enum Operation {
        Mesh {
            item: DrawItem,
            blend: RenderMode,
            explicit_range: bool,
            visible_buffer: Option<bool>,
            parts: Vec<(Range<u32>, Option<LightmapPageId>)>,
        },
        Overlay,
        Sprite,
        PassBoundary,
    }

    #[derive(Debug, Eq, PartialEq)]
    enum DrawEvent {
        Indexed {
            key: Key,
            page: Option<LightmapPageId>,
            indices: Range<u32>,
        },
        Overlay,
        Sprite,
    }

    fn record(operations: &[Operation], suppress: bool) -> (Vec<DrawEvent>, usize) {
        let mut state = State::default();
        let mut bound_mesh = None;
        let mut bound_page = None;
        let mut mesh_binds = 0;
        let mut events = Vec::new();
        for operation in operations {
            match operation {
                Operation::Mesh {
                    item,
                    blend,
                    explicit_range,
                    visible_buffer,
                    parts,
                } => {
                    let source = if suppress {
                        IndexSource::select(*explicit_range, *visible_buffer)
                    } else {
                        match (*explicit_range, *visible_buffer) {
                            (true, _) | (false, None) => Some(IndexSource::Original),
                            (false, Some(true)) => Some(IndexSource::Visible),
                            (false, Some(false)) => None,
                        }
                    };
                    let Some(source) = source else {
                        state.reset();
                        // A skipped mesh may still change the underlying bindings.
                        bound_mesh = None;
                        continue;
                    };
                    let key = Key {
                        item: *item,
                        blend: *blend,
                        source,
                    };
                    if !suppress || state.mesh(key) {
                        bound_mesh = Some(key);
                        mesh_binds += 1;
                    }
                    for (indices, page) in parts {
                        if !suppress || state.page(*page) {
                            bound_page = Some(*page);
                        }
                        events.push(DrawEvent::Indexed {
                            key: bound_mesh.expect("mesh is bound before every indexed draw"),
                            page: bound_page.expect("page is bound before every indexed draw"),
                            indices: indices.clone(),
                        });
                    }
                }
                Operation::Overlay | Operation::Sprite | Operation::PassBoundary => {
                    state.reset();
                    bound_mesh = None;
                    bound_page = None;
                    match operation {
                        Operation::Overlay => events.push(DrawEvent::Overlay),
                        Operation::Sprite => events.push(DrawEvent::Sprite),
                        Operation::PassBoundary => {}
                        Operation::Mesh { .. } => unreachable!(),
                    }
                }
            }
        }
        (events, mesh_binds)
    }

    #[test]
    fn suppressed_draw_events_match_original_across_all_state_and_pass_boundaries() {
        let a = DrawItem {
            mesh_index: 3,
            material_slot: 4,
        };
        let mesh = |item, blend, explicit_range, visible_buffer, parts| Operation::Mesh {
            item,
            blend,
            explicit_range,
            visible_buffer,
            parts,
        };
        let ordinary = || mesh(a, RenderMode::Translucent, true, None, vec![(3..6, None)]);
        let operations = [
            ordinary(),
            ordinary(),
            mesh(
                a,
                RenderMode::Translucent,
                true,
                Some(false),
                vec![(9..12, Some(LightmapPageId(7))), (12..15, None)],
            ),
            mesh(
                a,
                RenderMode::Translucent,
                false,
                Some(true),
                vec![(0..3, None), (3..6, Some(LightmapPageId(7)))],
            ),
            mesh(
                a,
                RenderMode::Translucent,
                false,
                Some(true),
                vec![(6..9, Some(LightmapPageId(7)))],
            ),
            ordinary(), // Original source after a visible source.
            mesh(
                DrawItem {
                    material_slot: 5,
                    ..a
                },
                RenderMode::Translucent,
                true,
                None,
                vec![(21..24, None)],
            ),
            mesh(
                DrawItem { mesh_index: 6, ..a },
                RenderMode::Translucent,
                true,
                None,
                vec![(24..27, None)],
            ),
            mesh(a, RenderMode::Additive, true, None, vec![(27..30, None)]),
            mesh(a, RenderMode::Modulate, true, None, vec![(30..33, None)]),
            ordinary(),
            Operation::Overlay,
            ordinary(),
            Operation::Sprite,
            ordinary(),
            mesh(
                a,
                RenderMode::Translucent,
                false,
                Some(false),
                vec![(0..3, None)],
            ),
            ordinary(),
            Operation::PassBoundary,
            ordinary(),
        ];
        let (reference, old_binds) = record(&operations, false);
        let (suppressed, new_binds) = record(&operations, true);
        assert_eq!(suppressed, reference);
        assert_eq!(reference.len(), 19);
        assert_eq!(old_binds, 15);
        assert_eq!(new_binds, 12);
    }

    #[test]
    fn unbound_unlit_and_explicit_original_ranges_are_distinct() {
        assert_eq!(
            IndexSource::select(true, Some(false)),
            Some(IndexSource::Original)
        );
        assert_eq!(
            IndexSource::select(true, Some(true)),
            Some(IndexSource::Original)
        );
        assert_eq!(
            IndexSource::select(false, None),
            Some(IndexSource::Original)
        );
        assert_eq!(
            IndexSource::select(false, Some(true)),
            Some(IndexSource::Visible)
        );
        assert_eq!(IndexSource::select(false, Some(false)), None);
        let key = Key {
            item: DrawItem {
                mesh_index: 0,
                material_slot: 0,
            },
            blend: RenderMode::Translucent,
            source: IndexSource::Original,
        };
        let mut state = State::default();
        assert!(state.mesh(key));
        assert!(state.page(None));
        assert!(!state.mesh(key));
        assert!(!state.page(None));
        assert!(state.page(Some(LightmapPageId(1))));
        assert!(!state.page(Some(LightmapPageId(1))));
        assert!(state.page(None));
        state.reset();
        assert!(state.mesh(key));
        assert!(state.page(None));
        assert!(state.mesh(Key {
            blend: RenderMode::Additive,
            ..key
        }));
        assert!(state.page(None));
    }
}
