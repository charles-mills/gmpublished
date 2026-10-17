use super::super::{MapBsp, MapLeafLocator};
use super::{MapBoundsCullMask, MapBoundsCulling};
use crate::math::Vec3;
use std::ops::Range;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct BrushModel {
    root: i32,
    pub(super) bounds: super::super::MapBounds,
    faces: Range<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Face {
    plane: u16,
    back: bool,
    node: bool,
    underwater_membership: bool,
    displacement: Option<u32>,
    material_group: super::MapWorldMaterialGroup,
}

enum WorldSurfaceEncounter {
    Leaf(u32),
    Face(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DisplacementGridSize {
    vertices: u32,
    indices: u32,
}

impl DisplacementGridSize {
    fn from_power(power: i32) -> Option<Self> {
        let side = 1_u32.checked_shl(u32::try_from(power).ok()?)?;
        Some(Self {
            vertices: side.checked_add(1)?.checked_pow(2)?,
            indices: side.checked_mul(side)?.checked_mul(6)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct FaceOrdering {
    root: i32,
    models: Vec<Option<BrushModel>>,
    displacements: Vec<Option<DisplacementGridSize>>,
    node_faces: Vec<Range<u32>>,
    node_bounds: Vec<super::super::MapBounds>,
    child_bounds_contained: Vec<[bool; 2]>,
    leaf_bounds: Vec<super::super::MapBounds>,
    leaf_solid: Vec<bool>,
    leaf_underwater: Vec<bool>,
    leaf_displacements: Vec<Vec<u32>>,
    leaf_ranges: Vec<Range<usize>>,
    leaf_faces: Vec<u16>,
    faces: Vec<Face>,
}

impl FaceOrdering {
    pub(super) fn from_bsp(
        bsp: &MapBsp,
        locator: &MapLeafLocator,
        displacement_bounds: impl Iterator<Item = (u32, super::super::MapBounds)>,
    ) -> Self {
        let root = bsp.models.first().map_or(0, |model| model.head_node);
        let node_bounds = bsp
            .nodes
            .iter()
            .map(|node| super::super::MapBounds {
                min: Vec3::from(node.mins.map(f32::from)),
                max: Vec3::from(node.maxs.map(f32::from)),
            })
            .collect::<Vec<_>>();
        let leaf_solid = bsp
            .leaves
            .iter()
            .map(|leaf| leaf.contents == super::super::contents_flags::SOLID)
            .collect::<Vec<_>>();
        let mut leaf_displacements = vec![Vec::new(); bsp.leaves.len()];
        for (face, bounds) in displacement_bounds {
            if let Some(leaves) = locator.leaves_for_aabb(root, bounds, &node_bounds, &leaf_solid) {
                for leaf in leaves {
                    leaf_displacements[leaf as usize].push(face);
                }
            } else {
                log::warn!("bsp displacement {face} has invalid leaf bounds or topology");
            }
        }
        for faces in &mut leaf_displacements {
            faces.sort_by_key(|&face| {
                bsp.faces
                    .get(face as usize)
                    .map_or(i16::MAX, |face| face.displacement)
            });
        }
        let mut ordering = Self {
            displacements: bsp
                .displacements
                .iter()
                .enumerate()
                .map(|(index, displacement)| {
                    let counts = DisplacementGridSize::from_power(displacement.power);
                    if counts.is_none() {
                        log::warn!(
                            "bsp displacement {index} has invalid full grid power {}",
                            displacement.power
                        );
                    }
                    counts
                })
                .collect(),
            models: bsp
                .models
                .iter()
                .map(|model| {
                    let bounds = super::super::MapBounds {
                        min: Vec3::from(model.mins),
                        max: Vec3::from(model.maxs),
                    };
                    (bounds.min.is_finite()
                        && bounds.max.is_finite()
                        && (0..3).all(|axis| bounds.min[axis] <= bounds.max[axis]))
                    .then_some(())?;
                    Some(BrushModel {
                        root: model.head_node,
                        bounds,
                        faces: super::model_face_range(model, bsp.faces.len())?,
                    })
                })
                .collect(),
            leaf_displacements,
            leaf_solid,
            leaf_underwater: bsp
                .leaves
                .iter()
                .map(|leaf| leaf.leaf_water_data != -1)
                .collect(),
            root,
            node_bounds,
            child_bounds_contained: Vec::new(),
            leaf_bounds: bsp
                .leaves
                .iter()
                .map(|leaf| super::super::MapBounds {
                    min: Vec3::from(leaf.mins.map(f32::from)),
                    max: Vec3::from(leaf.maxs.map(f32::from)),
                })
                .collect(),
            node_faces: bsp
                .nodes
                .iter()
                .map(|node| {
                    u32::from(node.first_face)
                        ..u32::from(node.first_face) + u32::from(node.face_count)
                })
                .collect(),
            leaf_ranges: bsp
                .leaves
                .iter()
                .map(|leaf| {
                    usize::from(leaf.first_leaf_face)
                        ..usize::from(leaf.first_leaf_face) + usize::from(leaf.leaf_face_count)
                })
                .collect(),
            leaf_faces: bsp.leaf_faces.clone(),
            faces: bsp
                .faces
                .iter()
                .map(|face| Face {
                    plane: face.plane,
                    back: face.side != 0,
                    node: face.on_node != 0,
                    underwater_membership: false,
                    displacement: u32::try_from(face.displacement).ok(),
                    material_group: super::MapWorldMaterialGroup::AboveWater,
                })
                .collect(),
        };
        ordering.validate_bounds_hierarchy(locator);
        ordering.classify_material_groups(bsp, locator);
        ordering
    }

    fn validate_bounds_hierarchy(&mut self, locator: &MapLeafLocator) {
        let contains = |parent: super::super::MapBounds, child: super::super::MapBounds| {
            parent.min.is_finite()
                && parent.max.is_finite()
                && child.min.is_finite()
                && child.max.is_finite()
                && (0..3).all(|axis| {
                    parent.min[axis] <= child.min[axis]
                        && child.min[axis] <= child.max[axis]
                        && child.max[axis] <= parent.max[axis]
                })
        };
        self.child_bounds_contained = locator
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                node.children.map(|child| {
                    let Some(&parent) = self.node_bounds.get(index) else {
                        return false;
                    };
                    let bounds = if child < 0 {
                        self.leaf_bounds.get((-i64::from(child) - 1) as usize)
                    } else {
                        self.node_bounds.get(child as usize)
                    };
                    bounds.is_some_and(|&child| contains(parent, child))
                })
            })
            .collect();
    }

    fn classify_material_groups(&mut self, bsp: &MapBsp, locator: &MapLeafLocator) {
        let mut membership = vec![(false, false); self.faces.len()];
        self.visit_world_surfaces(
            locator,
            Vec3::ZERO,
            |_| false,
            |_| true,
            |_| true,
            |encounter| {
                let WorldSurfaceEncounter::Leaf(index) = encounter else {
                    return;
                };
                let leaf = &bsp.leaves[index as usize];
                let underwater = leaf.leaf_water_data != -1;
                let range = usize::from(leaf.first_leaf_face)
                    ..usize::from(leaf.first_leaf_face) + usize::from(leaf.leaf_face_count);
                for &face in bsp.leaf_faces.get(range).into_iter().flatten() {
                    let index = usize::from(face);
                    let Some(raw) = bsp.faces.get(index) else {
                        continue;
                    };
                    let water = bsp
                        .face_texinfo(raw)
                        .is_some_and(|info| info.flags & super::super::texture_flags::WARP != 0);
                    if raw.displacement < 0 && !water {
                        let (above, under) = &mut membership[index];
                        *above |= !underwater;
                        *under |= underwater;
                    }
                }
                for &face in self
                    .leaf_displacements
                    .get(index as usize)
                    .into_iter()
                    .flatten()
                {
                    if let Some((above, under)) = membership.get_mut(face as usize) {
                        *above |= !underwater;
                        *under |= underwater;
                    }
                }
            },
        );
        let mut unclassified = 0_usize;
        for (index, face) in self.faces.iter_mut().enumerate() {
            face.underwater_membership = membership[index].1;
            let water = bsp
                .face_texinfo(&bsp.faces[index])
                .is_some_and(|info| info.flags & super::super::texture_flags::WARP != 0);
            face.material_group = if water {
                super::MapWorldMaterialGroup::Water
            } else {
                match membership[index] {
                    (true, true) => super::MapWorldMaterialGroup::IntersectsWater,
                    (false, true) => super::MapWorldMaterialGroup::Underwater,
                    (true, false) => super::MapWorldMaterialGroup::AboveWater,
                    (false, false) => {
                        unclassified += 1;
                        super::MapWorldMaterialGroup::AboveWater
                    }
                }
            };
        }
        if unclassified > 0 {
            log::debug!("bsp world material groups: {unclassified} surfaces lack leaf membership");
        }
    }

    pub(super) fn surface_rendering(&self, face: u32) -> Option<super::MapWorldSurfaceRendering> {
        let face = self.faces.get(face as usize)?;
        Some(super::MapWorldSurfaceRendering {
            material_group: face.material_group,
            displacement: face
                .displacement
                .and_then(|ordinal| self.displacement_rendering(ordinal)),
        })
    }

    fn displacement_rendering(&self, ordinal: u32) -> Option<super::MapDisplacementRendering> {
        let counts = self.displacements.get(ordinal as usize)?.as_ref()?;
        Some(super::MapDisplacementRendering {
            ordinal,
            full_grid_vertices: counts.vertices,
            full_grid_indices: counts.indices,
        })
    }

    pub(super) fn displacement_surfaces(
        &self,
    ) -> impl Iterator<Item = (u32, super::MapDisplacementRendering)> + '_ {
        self.faces.iter().enumerate().filter_map(|(index, face)| {
            Some((
                index as u32,
                self.displacement_rendering(face.displacement?)?,
            ))
        })
    }

    pub(super) fn brush_model(&self, index: super::super::ModelIndex) -> Option<&BrushModel> {
        self.models.get(index.get())?.as_ref()
    }

    pub(super) fn leaf_is_underwater(&self, leaf: u32) -> Option<bool> {
        self.leaf_underwater.get(leaf as usize).copied()
    }

    pub(super) fn leaves_for_aabb(
        &self,
        locator: &MapLeafLocator,
        bounds: super::super::MapBounds,
    ) -> Option<Vec<u32>> {
        locator.leaves_for_aabb(self.root, bounds, &self.node_bounds, &self.leaf_solid)
    }

    pub(super) fn brush_surface_groups(
        &self,
        locator: &MapLeafLocator,
        model: &BrushModel,
        eye: Vec3,
        two_sided: impl Fn(u32) -> bool,
    ) -> Vec<super::MapBrushSurfaceGroup> {
        enum Visit {
            Child(i32),
            Faces(usize, bool),
        }
        if !eye.is_finite() {
            return Vec::new();
        }
        let mut stack = vec![Visit::Child(model.root)];
        let mut visited = vec![false; locator.nodes.len()];
        let mut emitted = vec![false; self.faces.len()];
        let mut groups = Vec::new();
        while let Some(visit) = stack.pop() {
            match visit {
                Visit::Child(child) => {
                    let Ok(index) = usize::try_from(child) else {
                        continue;
                    };
                    let Some(node) = locator.nodes.get(index) else {
                        continue;
                    };
                    if visited[index] {
                        continue;
                    }
                    visited[index] = true;
                    let Some(plane) = usize::try_from(node.plane_index)
                        .ok()
                        .and_then(|index| locator.planes.get(index))
                    else {
                        continue;
                    };
                    let back = eye.dot(plane.normal) - plane.dist < 0.0;
                    let side = usize::from(back);
                    stack.push(Visit::Child(node.children[side]));
                    stack.push(Visit::Faces(index, back));
                    stack.push(Visit::Child(node.children[1 - side]));
                }
                Visit::Faces(node, back) => {
                    let Some(range) = self.node_faces.get(node) else {
                        continue;
                    };
                    let mut faces = Vec::new();
                    for index in range.clone() {
                        let Some(face) = self.faces.get(index as usize) else {
                            continue;
                        };
                        if model.faces.contains(&(index as usize))
                            && !emitted[index as usize]
                            && (face.back == back || two_sided(index))
                        {
                            emitted[index as usize] = true;
                            faces.push(index);
                        }
                    }
                    if !faces.is_empty() {
                        groups.push(super::MapBrushSurfaceGroup { faces });
                    }
                }
            }
        }
        groups
    }

    pub(super) fn ordered_faces(
        &self,
        locator: &MapLeafLocator,
        eye: Vec3,
        visible: impl Fn(u32) -> bool,
        bounds_visible: impl Fn(super::super::MapBounds) -> bool,
        cluster_visible: impl Fn(i16) -> bool,
    ) -> Vec<u32> {
        let mut faces = Vec::new();
        self.visit_world_surfaces(
            locator,
            eye,
            visible,
            bounds_visible,
            cluster_visible,
            |encounter| {
                if let WorldSurfaceEncounter::Face(face) = encounter {
                    faces.push(face);
                }
            },
        );
        faces
    }

    pub(super) fn world_surface_groups(
        &self,
        locator: &MapLeafLocator,
        eye: Vec3,
        visible: impl Fn(u32) -> bool,
        bounds_visible: impl Fn(super::super::MapBounds) -> bool,
        cluster_visible: impl Fn(i16) -> bool,
    ) -> Vec<super::MapWorldSurfaceGroup> {
        self.world_surface_groups_with_culling(
            locator,
            eye,
            visible,
            |bounds, _| {
                if bounds_visible(bounds) {
                    MapBoundsCulling::Visible(MapBoundsCullMask::ALL)
                } else {
                    MapBoundsCulling::Outside
                }
            },
            cluster_visible,
        )
    }

    pub(super) fn world_surface_groups_with_culling(
        &self,
        locator: &MapLeafLocator,
        eye: Vec3,
        visible: impl Fn(u32) -> bool,
        culling: impl Fn(super::super::MapBounds, MapBoundsCullMask) -> MapBoundsCulling,
        cluster_visible: impl Fn(i16) -> bool,
    ) -> Vec<super::MapWorldSurfaceGroup> {
        let mut groups = Vec::<super::MapWorldSurfaceGroup>::new();
        self.visit_world_surfaces_with_culling(
            locator,
            eye,
            visible,
            culling,
            cluster_visible,
            |encounter| match encounter {
                WorldSurfaceEncounter::Leaf(leaf_index) => {
                    groups.push(super::MapWorldSurfaceGroup {
                        leaf_index,
                        faces: Vec::new(),
                    });
                }
                WorldSurfaceEncounter::Face(face) => {
                    if let Some(group) = groups.last_mut() {
                        group.faces.push(face);
                    }
                }
            },
        );
        groups
    }

    fn visit_world_surfaces(
        &self,
        locator: &MapLeafLocator,
        eye: Vec3,
        visible: impl Fn(u32) -> bool,
        bounds_visible: impl Fn(super::super::MapBounds) -> bool,
        cluster_visible: impl Fn(i16) -> bool,
        emit: impl FnMut(WorldSurfaceEncounter),
    ) {
        self.visit_world_surfaces_with_culling(
            locator,
            eye,
            visible,
            |bounds, _| {
                if bounds_visible(bounds) {
                    MapBoundsCulling::Visible(MapBoundsCullMask::ALL)
                } else {
                    MapBoundsCulling::Outside
                }
            },
            cluster_visible,
            emit,
        );
    }

    fn visit_world_surfaces_with_culling(
        &self,
        locator: &MapLeafLocator,
        eye: Vec3,
        visible: impl Fn(u32) -> bool,
        culling: impl Fn(super::super::MapBounds, MapBoundsCullMask) -> MapBoundsCulling,
        cluster_visible: impl Fn(i16) -> bool,
        mut emit: impl FnMut(WorldSurfaceEncounter),
    ) {
        enum Visit {
            Child(i32, MapBoundsCullMask),
            Faces(usize, bool),
        }
        let mut stack = vec![Visit::Child(self.root, MapBoundsCullMask::ALL)];
        let mut visited = vec![false; locator.nodes.len()];
        let mut marked = vec![false; self.faces.len()];
        let mut emitted = vec![false; self.faces.len()];
        while let Some(visit) = stack.pop() {
            match visit {
                Visit::Child(child, mask) if child < 0 => {
                    let leaf = (-i64::from(child) - 1) as usize;
                    if self.leaf_solid.get(leaf).copied().unwrap_or(false)
                        || locator
                            .leaves
                            .get(leaf)
                            .is_none_or(|leaf| !cluster_visible(leaf.cluster))
                    {
                        continue;
                    }
                    if self.leaf_bounds.get(leaf).is_some_and(|bounds| {
                        matches!(culling(*bounds, mask), MapBoundsCulling::Outside)
                    }) {
                        continue;
                    }
                    emit(WorldSurfaceEncounter::Leaf(leaf as u32));
                    let Some(range) = self.leaf_ranges.get(leaf) else {
                        continue;
                    };
                    let Some(indices) = self.leaf_faces.get(range.clone()) else {
                        continue;
                    };
                    for &index in self.leaf_displacements.get(leaf).into_iter().flatten() {
                        if let Some(emitted) = emitted.get_mut(index as usize)
                            && !*emitted
                            && visible(index)
                        {
                            *emitted = true;
                            emit(WorldSurfaceEncounter::Face(index));
                        }
                    }
                    for &index in indices {
                        let index = usize::from(index);
                        let Some(face) = self.faces.get(index) else {
                            continue;
                        };
                        if visible(index as u32) && face.node {
                            marked[index] = true;
                        }
                    }
                    for displacement in [true, false] {
                        for &index in indices {
                            let index = usize::from(index);
                            let Some(face) = self.faces.get(index) else {
                                continue;
                            };
                            if face.node
                                || face.displacement.is_some() != displacement
                                || emitted[index]
                                || !visible(index as u32)
                            {
                                continue;
                            }
                            let Some(plane) = locator.planes.get(usize::from(face.plane)) else {
                                continue;
                            };
                            let facing = eye.dot(plane.normal) - plane.dist;
                            if !displacement && facing < -0.01 {
                                continue;
                            }
                            emitted[index] = true;
                            emit(WorldSurfaceEncounter::Face(index as u32));
                        }
                    }
                }
                Visit::Child(child, mask) => {
                    let Ok(index) = usize::try_from(child) else {
                        continue;
                    };
                    let Some(node) = locator.nodes.get(index) else {
                        continue;
                    };
                    if visited[index] {
                        continue;
                    }
                    visited[index] = true;
                    let remaining = match self.node_bounds.get(index) {
                        Some(bounds) => match culling(*bounds, mask) {
                            MapBoundsCulling::Outside => continue,
                            MapBoundsCulling::Visible(remaining) => remaining,
                        },
                        None => MapBoundsCullMask::ALL,
                    };
                    let Some(plane) = usize::try_from(node.plane_index)
                        .ok()
                        .and_then(|index| locator.planes.get(index))
                    else {
                        continue;
                    };
                    let back = eye.dot(plane.normal) - plane.dist < 0.0;
                    let side = usize::from(back);
                    let child_mask = |side| {
                        if self
                            .child_bounds_contained
                            .get(index)
                            .is_some_and(|contained| contained[side])
                        {
                            remaining
                        } else {
                            MapBoundsCullMask::ALL
                        }
                    };
                    stack.push(Visit::Child(node.children[1 - side], child_mask(1 - side)));
                    stack.push(Visit::Faces(index, back));
                    stack.push(Visit::Child(node.children[side], child_mask(side)));
                }
                Visit::Faces(node, back) => {
                    let Some(range) = self.node_faces.get(node) else {
                        continue;
                    };
                    for index in range.clone() {
                        let Some(face) = self.faces.get(index as usize) else {
                            continue;
                        };
                        if marked[index as usize]
                            && !emitted[index as usize]
                            && (face.back == back || face.underwater_membership)
                            && visible(index)
                        {
                            emitted[index as usize] = true;
                            emit(WorldSurfaceEncounter::Face(index));
                        }
                    }
                }
            }
        }
    }

    pub(super) fn visit_decal_faces<E>(
        &self,
        locator: &MapLeafLocator,
        root: i32,
        origin: Vec3,
        radius: f32,
        mut project: impl FnMut(u32) -> Result<bool, E>,
    ) -> Result<(), E> {
        let mut stack = vec![root];
        let mut visited = vec![false; locator.nodes.len()];
        let mut created = vec![false; self.faces.len()];
        while let Some(child) = stack.pop() {
            if child < 0 {
                let leaf = (-i64::from(child) - 1) as usize;
                if let Some(range) = self.leaf_ranges.get(leaf)
                    && let Some(faces) = self.leaf_faces.get(range.clone())
                {
                    for &index in faces {
                        let index = usize::from(index);
                        let Some(face) = self.faces.get(index) else {
                            continue;
                        };
                        if created[index] || face.displacement.is_some() {
                            continue;
                        }
                        let Some(plane) = locator.planes.get(usize::from(face.plane)) else {
                            continue;
                        };
                        if decal_plane_distance(origin, plane).abs() < 4.0 {
                            created[index] = project(index as u32)?;
                        }
                    }
                }
                for &index in self.leaf_displacements.get(leaf).into_iter().flatten() {
                    if let Some(created) = created.get_mut(index as usize)
                        && !*created
                    {
                        *created = project(index)?;
                    }
                }
                continue;
            }
            let Ok(index) = usize::try_from(child) else {
                continue;
            };
            let Some(node) = locator.nodes.get(index) else {
                continue;
            };
            if visited[index] {
                continue;
            }
            visited[index] = true;
            let Some(plane) = usize::try_from(node.plane_index)
                .ok()
                .and_then(|index| locator.planes.get(index))
            else {
                continue;
            };
            let distance = decal_plane_distance(origin, plane);
            if distance > radius {
                stack.push(node.children[0]);
            } else if distance < -radius {
                stack.push(node.children[1]);
            } else {
                if distance > -4.0
                    && distance < 4.0
                    && let Some(range) = self.node_faces.get(index)
                {
                    for index in range.clone() {
                        if let Some(face) = self.faces.get(index as usize)
                            && face.displacement.is_none()
                        {
                            created[index as usize] |= project(index)?;
                        }
                    }
                }
                stack.push(node.children[1]);
                stack.push(node.children[0]);
            }
        }
        Ok(())
    }
}

fn decal_plane_distance(origin: Vec3, plane: &super::super::MapPlane) -> f32 {
    (origin[0] * plane.normal[0] + origin[1] * plane.normal[1] - plane.dist)
        + origin[2] * plane.normal[2]
}

#[cfg(test)]
#[path = "visibility_order/tests.rs"]
mod tests;
