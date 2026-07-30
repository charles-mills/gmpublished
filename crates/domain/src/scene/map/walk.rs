//! Shared collision used by the preview's walk mode: brush hulls, displacement
//! triangles and prop hulls, plus the swept-AABB traces over them.
//!
//! Brushes are expanded by the player's half-extents rather than the trace
//! being swept as a box, which is what lets a hit be found by the same
//! plane-clipping the engine uses. Preview-quality throughout: enough to walk
//! a map and be stopped by the right surfaces, not a physics engine.

use super::{
    BTreeSet, BrushIndex, BrushSide, HashMap, MapBounds, MapBsp, MapLeaf, MapNode, MapPlane,
    MapPropVisibility, NodeChild, PROP_AABB_MAX_DEPTH, PROP_AABB_MAX_EXTENT, PROP_AABB_MAX_LEAVES,
    cluster_in_range, contents_flags, displacement_vertices, texture_flags,
    vector_is_finite_nonzero, walk_to_leaf,
};
use crate::math::Vec3;

#[derive(Clone, Debug, PartialEq)]
pub struct MapWalkCollision {
    pub(super) brushes: Vec<MapWalkBrush>,
    pub(super) water_brushes: Vec<MapWalkBrush>,
    pub(super) displacements: Vec<MapWalkDisplacement>,
    pub(super) props: MapWalkPropCollision,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaterVolume {
    pub surface_z: f32,
}

/// A convex collision hull: vertices plus triangle indices into them.
/// Deliberately independent of `.phy`/IVP parser details.
#[derive(Clone, Debug, PartialEq)]
pub struct ConvexHull {
    pub vertices: Vec<Vec3>,
    pub triangles: Vec<[usize; 3]>,
}

#[derive(Clone, Copy, Debug)]
pub struct MapWalkPropCollisionSource<'a> {
    pub ledges: &'a [ConvexHull],
    pub origin: Vec3,
    /// Source QAngle order: pitch, yaw, roll.
    pub angles: Vec3,
    pub scale: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapWalkPropModel {
    pub(super) brushes: Vec<MapWalkPropLocalBrush>,
}

#[derive(Clone, Copy, Debug)]
pub struct MapWalkPropModelPlacement<'a> {
    pub model: &'a MapWalkPropModel,
    pub origin: Vec3,
    /// Source QAngle order: pitch, yaw, roll.
    pub angles: Vec3,
    pub scale: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct MapWalkBrush {
    pub(super) planes: Vec<MapWalkBrushPlane>,
    pub(super) bounds_min: Vec3,
    pub(super) bounds_max: Vec3,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct MapWalkBrushPlane {
    pub(super) plane: MapPlane,
    pub(super) is_sky: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct MapWalkDisplacement {
    pub(super) triangles: Vec<MapWalkTriangle>,
    pub(super) bounds_min: Vec3,
    pub(super) bounds_max: Vec3,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct MapWalkPropLocalBrush {
    pub(super) planes: Vec<MapPlane>,
    pub(super) bounds_min: Vec3,
    pub(super) bounds_max: Vec3,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct MapWalkPropCollision {
    pub(super) brushes: Vec<MapWalkBrush>,
    pub(super) grid: MapWalkPropGrid,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct MapWalkPropGrid {
    pub(super) cells: HashMap<[i32; 3], Vec<usize>>,
    pub(super) overflow: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct MapWalkTriangle {
    pub(super) vertices: [Vec3; 3],
    pub(super) normal: Vec3,
    pub(super) bounds_min: Vec3,
    pub(super) bounds_max: Vec3,
}

impl MapWalkBrush {
    pub(super) const fn bounds(&self) -> MapBounds {
        MapBounds {
            min: self.bounds_min,
            max: self.bounds_max,
        }
    }
}

impl MapWalkDisplacement {
    pub(super) const fn bounds(&self) -> MapBounds {
        MapBounds {
            min: self.bounds_min,
            max: self.bounds_max,
        }
    }
}

impl MapWalkPropLocalBrush {
    pub(super) const fn bounds(&self) -> MapBounds {
        MapBounds {
            min: self.bounds_min,
            max: self.bounds_max,
        }
    }
}

impl MapWalkTriangle {
    pub(super) const fn bounds(&self) -> MapBounds {
        MapBounds {
            min: self.bounds_min,
            max: self.bounds_max,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapTrace {
    pub fraction: f32,
    pub end_position: Vec3,
    pub normal: Vec3,
    pub start_solid: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct TraceCandidate {
    pub(super) fraction: f32,
    pub(super) normal: Vec3,
    pub(super) start_solid: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct MapRayHit {
    pub(super) fraction: f32,
    pub(super) is_sky: bool,
}

pub(super) const TRACE_PLANE_EPSILON: f32 = 0.03125;
pub(super) const TRACE_INSIDE_EPSILON: f32 = 0.125;
pub(super) const TRACE_AXIS_EPSILON: f32 = 1.0e-6;
pub(super) const SKY_TRACE_DISTANCE: f32 = 131_072.0;
pub(super) const SKYBOX_COMPLETION_AABB_EXPANSION: f32 = 64.0;
pub(super) const SKYBOX_COMPLETION_MAX_WORLD_VOLUME_FRACTION: f32 = 0.5;
pub(super) const PROP_GRID_CELL_SIZE: f32 = 256.0;
pub(super) const PROP_GRID_MAX_CELLS_PER_BRUSH: usize = 512;
pub(super) const PROP_GRID_MAX_QUERY_CELLS: usize = 4096;
pub(super) const PROP_PLANE_NORMAL_EPSILON: f32 = 1.0e-4;
pub(super) const PROP_PLANE_DIST_EPSILON: f32 = 0.05;

impl MapWalkCollision {
    pub fn empty() -> Self {
        Self {
            brushes: Vec::new(),
            water_brushes: Vec::new(),
            displacements: Vec::new(),
            props: MapWalkPropCollision::default(),
        }
    }

    pub(super) fn from_bsp_excluding(
        bsp: &MapBsp,
        excluded_brushes: &BTreeSet<BrushIndex>,
    ) -> Option<Self> {
        let (brushes, water_brushes) = walk_brushes_from_bsp(bsp, excluded_brushes);
        let displacements = walk_displacements_from_bsp(bsp);
        (!brushes.is_empty() || !water_brushes.is_empty() || !displacements.is_empty()).then_some(
            Self {
                brushes,
                water_brushes,
                displacements,
                props: MapWalkPropCollision::default(),
            },
        )
    }

    pub fn is_empty(&self) -> bool {
        self.brushes.is_empty()
            && self.water_brushes.is_empty()
            && self.displacements.is_empty()
            && self.props.brushes.is_empty()
    }

    /// Test support: a collision set holding a single solid axis-aligned
    /// box, so downstream crates can exercise walk movement without baking
    /// a real BSP.
    #[cfg(feature = "test-support")]
    pub fn solid_box_for_tests(min: Vec3, max: Vec3) -> Self {
        let planes = [
            ([1.0, 0.0, 0.0], max[0]),
            ([-1.0, 0.0, 0.0], -min[0]),
            ([0.0, 1.0, 0.0], max[1]),
            ([0.0, -1.0, 0.0], -min[1]),
            ([0.0, 0.0, 1.0], max[2]),
            ([0.0, 0.0, -1.0], -min[2]),
        ]
        .into_iter()
        .map(|(normal, dist)| MapPlane {
            normal: Vec3::from(normal),
            dist,
        })
        .collect();
        Self {
            brushes: vec![
                walk_brush_from_planes(planes, contents_flags::SOLID)
                    .expect("axis-aligned box brush is always valid"),
            ],
            water_brushes: Vec::new(),
            displacements: Vec::new(),
            props: MapWalkPropCollision::default(),
        }
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn with_solid_box_for_tests(mut self, min: Vec3, max: Vec3) -> Self {
        self.brushes
            .push(axis_aligned_box_brush(min, max, contents_flags::SOLID));
        self
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn with_water_box_for_tests(mut self, min: Vec3, max: Vec3) -> Self {
        self.water_brushes
            .push(axis_aligned_box_brush(min, max, contents_flags::WATER));
        self
    }

    #[must_use]
    pub fn with_prop_collisions<'a>(
        mut self,
        props: impl IntoIterator<Item = MapWalkPropCollisionSource<'a>>,
    ) -> Self {
        self.props = MapWalkPropCollision::from_sources(props);
        self
    }

    #[must_use]
    pub fn with_prop_collision_models<'a>(
        mut self,
        props: impl IntoIterator<Item = MapWalkPropModelPlacement<'a>>,
    ) -> Self {
        self.props = MapWalkPropCollision::from_model_placements(props);
        self
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn without_prop_collisions_for_tests(mut self) -> Self {
        self.props = MapWalkPropCollision::default();
        self
    }

    pub fn brush_count(&self) -> usize {
        self.brushes.len()
    }

    pub fn displacement_count(&self) -> usize {
        self.displacements.len()
    }

    pub fn prop_hull_count(&self) -> usize {
        self.props.brushes.len()
    }

    pub fn prop_collision_memory_bytes(&self) -> usize {
        self.props.memory_bytes()
    }

    pub fn ray_hits_sky(&self, start: Vec3, direction: Vec3) -> bool {
        if !self.has_sky_sides() {
            return false;
        }
        let Some(direction) = direction
            .normalize()
            .filter(|d| vector_is_finite_nonzero(*d))
        else {
            return false;
        };
        let end = start + (direction * SKY_TRACE_DISTANCE);
        let ray_bounds = bounds_from_points([start, end]).unwrap_or(MapBounds {
            min: start,
            max: end,
        });
        let mut best: Option<MapRayHit> = None;

        for brush in &self.brushes {
            if !bounds_intersect(ray_bounds, brush.bounds()) {
                continue;
            }
            let Some(hit) = trace_brush_ray(brush, start, end) else {
                continue;
            };
            if best.is_none_or(|best| hit.fraction < best.fraction) {
                best = Some(hit);
            }
        }

        best.is_none_or(|hit| hit.is_sky)
    }

    pub(super) fn has_sky_sides(&self) -> bool {
        self.brushes
            .iter()
            .flat_map(|brush| &brush.planes)
            .any(|plane| plane.is_sky)
    }

    pub fn trace_aabb(&self, start: Vec3, end: Vec3, half_extents: Vec3) -> MapTrace {
        let mut best = TraceCandidate {
            fraction: 1.0,
            normal: Vec3::splat(0.0),
            start_solid: false,
        };
        let sweep = swept_bounds(start, end, half_extents);

        for brush in &self.brushes {
            let expanded = expand_bounds(brush.bounds(), half_extents);
            if !bounds_intersect(sweep, expanded) {
                continue;
            }
            if let Some(hit) = trace_brush_aabb(brush, start, end, half_extents) {
                best = merge_trace_candidate(best, hit);
            }
        }

        for displacement in &self.displacements {
            let expanded = expand_bounds(displacement.bounds(), half_extents);
            if !bounds_intersect(sweep, expanded) {
                continue;
            }
            for triangle in &displacement.triangles {
                let expanded = expand_bounds(triangle.bounds(), half_extents);
                if !bounds_intersect(sweep, expanded) {
                    continue;
                }
                if let Some(hit) = trace_triangle_aabb(triangle, start, end, half_extents) {
                    best = merge_trace_candidate(best, hit);
                }
            }
        }

        for brush_index in self.props.query(sweep) {
            let Some(brush) = self.props.brushes.get(brush_index) else {
                continue;
            };
            let expanded = expand_bounds(brush.bounds(), half_extents);
            if !bounds_intersect(sweep, expanded) {
                continue;
            }
            if let Some(hit) = trace_brush_aabb(brush, start, end, half_extents) {
                best = merge_trace_candidate(best, hit);
            }
        }

        MapTrace {
            fraction: best.fraction.clamp(0.0, 1.0),
            end_position: start.lerp(end, best.fraction.clamp(0.0, 1.0)),
            normal: best.normal,
            start_solid: best.start_solid,
        }
    }

    pub fn aabb_embedded(&self, center: Vec3, half_extents: Vec3) -> bool {
        self.brushes
            .iter()
            .any(|brush| brush_contains_aabb_center(brush, center, half_extents))
    }

    /// The trace's own notion of a solid start: not strictly outside any
    /// plane of some brush. Strictly wider than [`Self::aabb_embedded`] —
    /// it also flags the boundary shell (e.g. a spawn hull at
    /// mathematically exact floor contact, which is where mappers place
    /// info_player_start), and `trace_aabb` refuses to move a hull that
    /// starts there. Unstick logic must use THIS predicate; checking only
    /// `aabb_embedded` leaves "not embedded yet untraceable" deadlocks.
    pub fn aabb_trace_solid(&self, center: Vec3, half_extents: Vec3) -> bool {
        self.trace_aabb(center, center, half_extents).start_solid
    }

    pub fn water_at(&self, point: Vec3) -> Option<WaterVolume> {
        self.water_brushes
            .iter()
            .filter(|brush| bounds_contains_point(brush.bounds(), point))
            .filter(|brush| {
                brush.planes.iter().all(|side| {
                    point.dot(side.plane.normal) - side.plane.dist <= TRACE_INSIDE_EPSILON
                })
            })
            .filter_map(|brush| {
                brush
                    .planes
                    .iter()
                    .filter(|side| side.plane.normal[2] > 0.7)
                    .map(|side| {
                        let plane = side.plane;
                        (plane.dist - plane.normal[0] * point[0] - plane.normal[1] * point[1])
                            / plane.normal[2]
                    })
                    .max_by(f32::total_cmp)
            })
            .max_by(f32::total_cmp)
            .map(|surface_z| WaterVolume { surface_z })
    }
}

pub(super) fn merge_trace_candidate(left: TraceCandidate, right: TraceCandidate) -> TraceCandidate {
    if right.start_solid && !left.start_solid {
        return TraceCandidate {
            fraction: left.fraction.min(right.fraction),
            normal: right.normal,
            start_solid: true,
        };
    }
    if right.fraction < left.fraction {
        right
    } else {
        left
    }
}

pub(super) fn walk_brushes_from_bsp(
    bsp: &MapBsp,
    excluded_brushes: &BTreeSet<BrushIndex>,
) -> (Vec<MapWalkBrush>, Vec<MapWalkBrush>) {
    let mut brushes = Vec::new();
    let mut water_brushes = Vec::new();
    for (brush_index, brush) in bsp.brushes.iter().enumerate() {
        if excluded_brushes.contains(&BrushIndex::new(brush_index)) {
            continue;
        }
        let solid = brush.contents & (contents_flags::SOLID | contents_flags::PLAYERCLIP) != 0;
        let water = brush.contents & (contents_flags::WATER | contents_flags::SLIME) != 0;
        if !solid && !water {
            continue;
        }
        let Some(side_start) = usize::try_from(brush.first_side).ok() else {
            continue;
        };
        let Some(side_count) = usize::try_from(brush.side_count).ok() else {
            continue;
        };
        let Some(side_end) = side_start.checked_add(side_count) else {
            continue;
        };
        let Some(sides) = bsp.brush_sides.get(side_start..side_end) else {
            continue;
        };
        let planes = sides
            .iter()
            .filter_map(|side| {
                let plane = bsp.planes.get(usize::from(side.plane))?;
                let normal = Vec3::from(plane.normal).normalize_or_zero();
                vector_is_finite_nonzero(normal).then_some(MapWalkBrushPlane {
                    plane: MapPlane {
                        normal,
                        dist: plane.dist,
                    },
                    is_sky: brush_side_is_sky(side, bsp),
                })
            })
            .collect::<Vec<_>>();
        let Some(brush) = walk_brush_from_brush_planes(planes, brush.contents) else {
            continue;
        };
        if water {
            water_brushes.push(brush);
        } else {
            brushes.push(brush);
        }
    }
    (brushes, water_brushes)
}

/// Only the box builders above and the tests that exercise them reach this;
/// a decoded BSP arrives as brush sides and takes
/// [`walk_brush_from_brush_planes`] instead.
#[cfg(feature = "test-support")]
pub(super) fn walk_brush_from_planes(planes: Vec<MapPlane>, flags: i32) -> Option<MapWalkBrush> {
    walk_brush_from_brush_planes(
        planes
            .into_iter()
            .map(|plane| MapWalkBrushPlane {
                plane,
                is_sky: false,
            })
            .collect(),
        flags,
    )
}

pub(super) fn walk_brush_from_brush_planes(
    planes: Vec<MapWalkBrushPlane>,
    flags: i32,
) -> Option<MapWalkBrush> {
    if flags
        & (contents_flags::SOLID
            | contents_flags::PLAYERCLIP
            | contents_flags::WATER
            | contents_flags::SLIME)
        == 0
        || planes.len() < 4
    {
        return None;
    }
    let bounds_planes = planes.iter().map(|side| side.plane).collect::<Vec<_>>();
    let Some(MapBounds {
        min: bounds_min,
        max: bounds_max,
    }) = brush_bounds_from_planes(&bounds_planes)
    else {
        log::debug!("bsp walk brush skipped: unable to derive finite bounds");
        return None;
    };
    Some(MapWalkBrush {
        planes,
        bounds_min,
        bounds_max,
    })
}

#[cfg(feature = "test-support")]
fn axis_aligned_box_brush(min: Vec3, max: Vec3, flags: i32) -> MapWalkBrush {
    let planes = [
        ([1.0, 0.0, 0.0], max[0]),
        ([-1.0, 0.0, 0.0], -min[0]),
        ([0.0, 1.0, 0.0], max[1]),
        ([0.0, -1.0, 0.0], -min[1]),
        ([0.0, 0.0, 1.0], max[2]),
        ([0.0, 0.0, -1.0], -min[2]),
    ]
    .into_iter()
    .map(|(normal, dist)| MapPlane {
        normal: Vec3::from(normal),
        dist,
    })
    .collect();
    walk_brush_from_planes(planes, flags).expect("axis-aligned box brush is always valid")
}

pub(super) fn brush_side_is_sky(side: &BrushSide, bsp: &MapBsp) -> bool {
    let flags = usize::try_from(side.texinfo)
        .ok()
        .and_then(|index| bsp.texinfos.get(index))
        .map(|texinfo| texinfo.flags);
    brush_side_sky_from_texture_flags(side, flags)
}

pub(super) fn brush_side_sky_from_texture_flags(side: &BrushSide, flags: Option<i32>) -> bool {
    side.bevel == 0
        && flags.is_some_and(|flags| flags & (texture_flags::SKY | texture_flags::SKY2D) != 0)
}

pub(super) fn walk_displacements_from_bsp(bsp: &MapBsp) -> Vec<MapWalkDisplacement> {
    let mut displacements = Vec::new();
    for (face_index, face) in bsp.faces.iter().enumerate() {
        let Some(displacement) = bsp.face_displacement(face) else {
            continue;
        };
        if bsp
            .face_texinfo(face)
            .is_some_and(|texinfo| texinfo.flags & texture_flags::WARP != 0)
        {
            continue;
        }
        let Ok(vertices) = displacement_vertices(bsp, face, displacement) else {
            log::debug!("bsp walk displacement {face_index} skipped: invalid tessellation");
            continue;
        };
        let triangles = vertices
            .chunks_exact(3)
            .filter_map(|chunk| {
                let vertices = [chunk[0].position, chunk[1].position, chunk[2].position];
                MapWalkTriangle::new(vertices)
            })
            .collect::<Vec<_>>();
        let Some(MapBounds {
            min: bounds_min,
            max: bounds_max,
        }) = bounds_from_triangles(&triangles)
        else {
            continue;
        };
        displacements.push(MapWalkDisplacement {
            triangles,
            bounds_min,
            bounds_max,
        });
    }
    displacements
}

impl MapWalkPropModel {
    pub fn from_hulls(hulls: &[ConvexHull]) -> Self {
        let mut brushes = Vec::new();
        for hull in hulls {
            let Some(brush) = local_prop_brush_from_hull(hull, true) else {
                continue;
            };
            brushes.push(brush);
        }
        Self { brushes }
    }

    pub fn is_empty(&self) -> bool {
        self.brushes.is_empty()
    }
}

impl MapWalkPropCollision {
    pub(super) fn from_sources<'a>(
        props: impl IntoIterator<Item = MapWalkPropCollisionSource<'a>>,
    ) -> Self {
        let models = props
            .into_iter()
            .filter_map(|prop| {
                let model = MapWalkPropModel::from_hulls(prop.ledges);
                (!model.is_empty()).then_some((model, prop.origin, prop.angles, prop.scale))
            })
            .collect::<Vec<_>>();
        Self::from_model_placements(models.iter().map(|(model, origin, angles, scale)| {
            MapWalkPropModelPlacement {
                model,
                origin: *origin,
                angles: *angles,
                scale: *scale,
            }
        }))
    }

    pub(super) fn from_model_placements<'a>(
        props: impl IntoIterator<Item = MapWalkPropModelPlacement<'a>>,
    ) -> Self {
        let mut brushes = Vec::new();
        for prop in props {
            if !prop.scale.is_finite() || prop.scale <= 0.0 {
                continue;
            }
            for local in &prop.model.brushes {
                let Some(brush) = prop_brush_from_local(local, prop) else {
                    continue;
                };
                brushes.push(brush);
            }
        }
        let grid = MapWalkPropGrid::from_brushes(&brushes);
        Self { brushes, grid }
    }

    pub(super) fn query(&self, bounds: MapBounds) -> Vec<usize> {
        if self.brushes.is_empty() {
            return Vec::new();
        }
        let Some((min_cell, max_cell)) = prop_grid_range(bounds) else {
            return (0..self.brushes.len()).collect();
        };
        let Some(cell_count) = prop_grid_cell_count(min_cell, max_cell) else {
            return (0..self.brushes.len()).collect();
        };
        if cell_count > PROP_GRID_MAX_QUERY_CELLS {
            return (0..self.brushes.len()).collect();
        }

        let mut candidates = BTreeSet::<usize>::new();
        candidates.extend(self.grid.overflow.iter().copied());
        for x in min_cell[0]..=max_cell[0] {
            for y in min_cell[1]..=max_cell[1] {
                for z in min_cell[2]..=max_cell[2] {
                    if let Some(indices) = self.grid.cells.get(&[x, y, z]) {
                        candidates.extend(indices.iter().copied());
                    }
                }
            }
        }
        candidates.into_iter().collect()
    }

    pub(super) fn memory_bytes(&self) -> usize {
        let brush_bytes = self
            .brushes
            .iter()
            .map(|brush| {
                std::mem::size_of::<MapWalkBrush>()
                    + brush.planes.len() * std::mem::size_of::<MapWalkBrushPlane>()
            })
            .sum::<usize>();
        let cell_bytes = self
            .grid
            .cells
            .values()
            .map(|indices| indices.len() * std::mem::size_of::<usize>())
            .sum::<usize>();
        brush_bytes
            .saturating_add(cell_bytes)
            .saturating_add(self.grid.cells.len() * std::mem::size_of::<([i32; 3], Vec<usize>)>())
            .saturating_add(self.grid.overflow.len() * std::mem::size_of::<usize>())
    }
}

impl MapWalkPropGrid {
    pub(super) fn from_brushes(brushes: &[MapWalkBrush]) -> Self {
        let mut grid = Self::default();
        for (index, brush) in brushes.iter().enumerate() {
            let bounds = brush.bounds();
            let Some((min_cell, max_cell)) = prop_grid_range(bounds) else {
                grid.overflow.push(index);
                continue;
            };
            let Some(cell_count) = prop_grid_cell_count(min_cell, max_cell) else {
                grid.overflow.push(index);
                continue;
            };
            if cell_count > PROP_GRID_MAX_CELLS_PER_BRUSH {
                grid.overflow.push(index);
                continue;
            }
            for x in min_cell[0]..=max_cell[0] {
                for y in min_cell[1]..=max_cell[1] {
                    for z in min_cell[2]..=max_cell[2] {
                        grid.cells.entry([x, y, z]).or_default().push(index);
                    }
                }
            }
        }
        grid
    }
}

pub(super) fn local_prop_brush_from_hull(
    hull: &ConvexHull,
    include_bevels: bool,
) -> Option<MapWalkPropLocalBrush> {
    let local_planes = prop_planes_from_hull(hull, include_bevels)?;
    let MapBounds {
        min: bounds_min,
        max: bounds_max,
    } = bounds_from_points_iter(hull.vertices.iter().copied())?;
    Some(MapWalkPropLocalBrush {
        planes: local_planes,
        bounds_min,
        bounds_max,
    })
}

pub(super) fn prop_brush_from_local(
    local: &MapWalkPropLocalBrush,
    prop: MapWalkPropModelPlacement<'_>,
) -> Option<MapWalkBrush> {
    let planes = local
        .planes
        .iter()
        .copied()
        .filter_map(|plane| transform_prop_plane(plane, prop.origin, prop.angles, prop.scale))
        .map(|plane| MapWalkBrushPlane {
            plane,
            is_sky: false,
        })
        .collect::<Vec<_>>();
    if planes.len() < 4 {
        return None;
    }
    let MapBounds {
        min: bounds_min,
        max: bounds_max,
    } = bounds_from_points_iter(
        prop_bounds_corners(local.bounds())
            .into_iter()
            .map(|point| transform_prop_point(point, prop.origin, prop.angles, prop.scale)),
    )?;
    Some(MapWalkBrush {
        planes,
        bounds_min,
        bounds_max,
    })
}

pub(super) fn prop_planes_from_hull(
    hull: &ConvexHull,
    include_bevels: bool,
) -> Option<Vec<MapPlane>> {
    if hull.vertices.len() < 4 || hull.triangles.is_empty() {
        return None;
    }
    let centroid = (hull
        .vertices
        .iter()
        .copied()
        .fold(Vec3::ZERO, |sum, vertex| sum + vertex))
        * (1.0 / hull.vertices.len() as f32);
    let mut planes = Vec::new();
    for triangle in &hull.triangles {
        let vertices = [
            *hull.vertices.get(triangle[0])?,
            *hull.vertices.get(triangle[1])?,
            *hull.vertices.get(triangle[2])?,
        ];
        let normal = (vertices[1] - vertices[0])
            .cross(vertices[2] - vertices[0])
            .normalize_or_zero();
        if !vector_is_finite_nonzero(normal) {
            continue;
        }
        let mut plane = MapPlane {
            normal,
            dist: vertices[0].dot(normal),
        };
        if centroid.dot(plane.normal) - plane.dist > 0.0 {
            plane.normal *= -1.0;
            plane.dist = -plane.dist;
        }
        push_unique_prop_plane(&mut planes, plane);
    }

    if include_bevels {
        add_prop_bevel_planes(hull, &mut planes);
    }

    (planes.len() >= 4).then_some(planes)
}

pub(super) fn add_prop_bevel_planes(hull: &ConvexHull, planes: &mut Vec<MapPlane>) {
    let Some(MapBounds {
        min: bounds_min,
        max: bounds_max,
    }) = bounds_from_points_iter(hull.vertices.iter().copied())
    else {
        return;
    };
    for (normal, dist) in [
        ([1.0, 0.0, 0.0], bounds_max[0]),
        ([-1.0, 0.0, 0.0], -bounds_min[0]),
        ([0.0, 1.0, 0.0], bounds_max[1]),
        ([0.0, -1.0, 0.0], -bounds_min[1]),
        ([0.0, 0.0, 1.0], bounds_max[2]),
        ([0.0, 0.0, -1.0], -bounds_min[2]),
    ] {
        push_unique_prop_plane(
            planes,
            MapPlane {
                normal: Vec3::from(normal),
                dist,
            },
        );
    }

    // QBSP-style hull expansion needs more than face planes for swept AABBs:
    // add axial bounds plus edge x axis bevels so a box cannot pass through
    // a non-axial convex corner between two sampled positions.
    let mut edges = BTreeSet::<(usize, usize)>::new();
    for triangle in &hull.triangles {
        for (left, right) in [
            (triangle[0], triangle[1]),
            (triangle[1], triangle[2]),
            (triangle[2], triangle[0]),
        ] {
            if left == right {
                continue;
            }
            edges.insert((left.min(right), left.max(right)));
        }
    }
    let axes = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for (left, right) in edges {
        let Some(left) = hull.vertices.get(left).copied() else {
            continue;
        };
        let Some(right) = hull.vertices.get(right).copied() else {
            continue;
        };
        let edge = right - left;
        if !vector_is_finite_nonzero(edge) {
            continue;
        }
        for axis in axes {
            let normal = edge.cross(Vec3::from(axis)).normalize_or_zero();
            if !vector_is_finite_nonzero(normal) {
                continue;
            }
            for normal in [normal, (normal * -1.0)] {
                let dist = hull
                    .vertices
                    .iter()
                    .map(|vertex| (*vertex).dot(normal))
                    .fold(f32::NEG_INFINITY, f32::max);
                if dist.is_finite() {
                    push_unique_prop_plane(planes, MapPlane { normal, dist });
                }
            }
        }
    }
}

pub(super) fn push_unique_prop_plane(planes: &mut Vec<MapPlane>, plane: MapPlane) {
    if !vector_is_finite_nonzero(plane.normal) || !plane.dist.is_finite() {
        return;
    }
    if planes.iter().any(|existing| {
        (0..3).all(|axis| {
            (existing.normal[axis] - plane.normal[axis]).abs() <= PROP_PLANE_NORMAL_EPSILON
        }) && (existing.dist - plane.dist).abs() <= PROP_PLANE_DIST_EPSILON
    }) {
        return;
    }
    planes.push(plane);
}

pub(super) fn transform_prop_plane(
    plane: MapPlane,
    origin: Vec3,
    angles: Vec3,
    scale: f32,
) -> Option<MapPlane> {
    let normal = (plane.normal.rotate_source(angles)).normalize_or_zero();
    if !vector_is_finite_nonzero(normal) {
        return None;
    }
    let dist = plane.dist * scale + origin.dot(normal);
    dist.is_finite().then_some(MapPlane { normal, dist })
}

pub(super) fn transform_prop_point(point: Vec3, origin: Vec3, angles: Vec3, scale: f32) -> Vec3 {
    ((point * scale).rotate_source(angles)) + origin
}

pub(super) fn prop_bounds_corners(bounds: MapBounds) -> [Vec3; 8] {
    let MapBounds { min, max } = bounds;
    [
        min,
        Vec3::new(max[0], min[1], min[2]),
        Vec3::new(min[0], max[1], min[2]),
        Vec3::new(min[0], min[1], max[2]),
        Vec3::new(max[0], max[1], min[2]),
        Vec3::new(max[0], min[1], max[2]),
        Vec3::new(min[0], max[1], max[2]),
        max,
    ]
}

pub(super) fn prop_grid_range(bounds: MapBounds) -> Option<([i32; 3], [i32; 3])> {
    Some((
        [
            prop_grid_cell(bounds.min[0])?,
            prop_grid_cell(bounds.min[1])?,
            prop_grid_cell(bounds.min[2])?,
        ],
        [
            prop_grid_cell(bounds.max[0])?,
            prop_grid_cell(bounds.max[1])?,
            prop_grid_cell(bounds.max[2])?,
        ],
    ))
}

pub(super) fn prop_grid_cell(value: f32) -> Option<i32> {
    if !value.is_finite() {
        return None;
    }
    let cell = (value / PROP_GRID_CELL_SIZE).floor();
    if cell < i32::MIN as f32 || cell > i32::MAX as f32 {
        return None;
    }
    Some(cell as i32)
}

pub(super) fn prop_grid_cell_count(min_cell: [i32; 3], max_cell: [i32; 3]) -> Option<usize> {
    let mut count = 1_usize;
    for axis in 0..3 {
        if max_cell[axis] < min_cell[axis] {
            return None;
        }
        let span = i64::from(max_cell[axis]) - i64::from(min_cell[axis]) + 1;
        count = count.checked_mul(usize::try_from(span).ok()?)?;
    }
    Some(count)
}

impl MapWalkTriangle {
    pub(super) fn new(vertices: [Vec3; 3]) -> Option<Self> {
        let normal = (vertices[1] - vertices[0])
            .cross(vertices[2] - vertices[0])
            .normalize_or_zero();
        if !vector_is_finite_nonzero(normal)
            || !vertices
                .iter()
                .flat_map(|vertex| vertex.to_array())
                .all(f32::is_finite)
        {
            return None;
        }
        let MapBounds {
            min: bounds_min,
            max: bounds_max,
        } = bounds_from_points(vertices)?;
        Some(Self {
            vertices,
            normal,
            bounds_min,
            bounds_max,
        })
    }
}

pub(super) fn trace_brush_aabb(
    brush: &MapWalkBrush,
    start: Vec3,
    end: Vec3,
    half_extents: Vec3,
) -> Option<TraceCandidate> {
    let mut enter_fraction = -1.0_f32;
    let mut leave_fraction = 1.0_f32;
    let mut enter_normal = Vec3::ZERO;
    let mut starts_outside = false;

    for side in &brush.planes {
        let plane = side.plane;
        let expanded_dist = plane.dist + plane.normal.dot_abs(half_extents);
        let start_dist = start.dot(plane.normal) - expanded_dist;
        let end_dist = end.dot(plane.normal) - expanded_dist;

        // Quake semantics: ANY positive distance counts as starting
        // outside. Hit traces back the mover off to exactly
        // TRACE_PLANE_EPSILON from the plane, so resting contact sits AT
        // epsilon — a strict `> epsilon` here classified every standing
        // player as start_solid, freezing all movement and ground checks.
        if start_dist > 0.0 {
            starts_outside = true;
        }
        if start_dist > 0.0 && end_dist > 0.0 {
            return None;
        }
        if start_dist <= 0.0 && end_dist <= 0.0 {
            continue;
        }

        let denominator = start_dist - end_dist;
        if denominator.abs() <= TRACE_AXIS_EPSILON {
            continue;
        }

        if start_dist > end_dist {
            let fraction = (start_dist - TRACE_PLANE_EPSILON) / denominator;
            if fraction > enter_fraction {
                enter_fraction = fraction;
                enter_normal = plane.normal;
            }
        } else {
            let fraction = (start_dist + TRACE_PLANE_EPSILON) / denominator;
            leave_fraction = leave_fraction.min(fraction);
        }

        if enter_fraction > leave_fraction {
            return None;
        }
    }

    if !starts_outside {
        return Some(TraceCandidate {
            fraction: 0.0,
            normal: Vec3::splat(0.0),
            start_solid: true,
        });
    }

    // A start within epsilon of a plane yields a slightly negative enter
    // fraction; clamp it to a fraction-zero hit (Quake does the same) —
    // rejecting it would let the mover pass through the face it is
    // touching.
    (enter_fraction > -1.0 && enter_fraction <= 1.0).then_some(TraceCandidate {
        fraction: enter_fraction.max(0.0),
        normal: enter_normal,
        start_solid: false,
    })
}

pub(super) fn trace_brush_ray(brush: &MapWalkBrush, start: Vec3, end: Vec3) -> Option<MapRayHit> {
    let mut enter_fraction = -1.0_f32;
    let mut leave_fraction = 1.0_f32;
    let mut enter_is_sky = false;
    let mut starts_outside = false;

    for side in &brush.planes {
        let plane = side.plane;
        let start_dist = start.dot(plane.normal) - plane.dist;
        let end_dist = end.dot(plane.normal) - plane.dist;

        if start_dist > 0.0 {
            starts_outside = true;
        }
        if start_dist > 0.0 && end_dist > 0.0 {
            return None;
        }
        if start_dist <= 0.0 && end_dist <= 0.0 {
            continue;
        }

        let denominator = start_dist - end_dist;
        if denominator.abs() <= TRACE_AXIS_EPSILON {
            continue;
        }

        if start_dist > end_dist {
            let fraction = start_dist / denominator;
            if fraction > enter_fraction {
                enter_fraction = fraction;
                enter_is_sky = side.is_sky;
            }
        } else {
            leave_fraction = leave_fraction.min(start_dist / denominator);
        }

        if enter_fraction > leave_fraction {
            return None;
        }
    }

    if !starts_outside {
        return Some(MapRayHit {
            fraction: 0.0,
            is_sky: false,
        });
    }

    (0.0..=1.0).contains(&enter_fraction).then_some(MapRayHit {
        fraction: enter_fraction,
        is_sky: enter_is_sky,
    })
}

pub(super) fn brush_contains_aabb_center(
    brush: &MapWalkBrush,
    center: Vec3,
    half_extents: Vec3,
) -> bool {
    bounds_contains_point(expand_bounds(brush.bounds(), half_extents), center)
        && brush.planes.iter().all(|side| {
            let plane = side.plane;
            let expanded_dist = plane.dist + plane.normal.dot_abs(half_extents);
            center.dot(plane.normal) - expanded_dist < -TRACE_INSIDE_EPSILON
        })
}

pub(super) fn trace_triangle_aabb(
    triangle: &MapWalkTriangle,
    start: Vec3,
    end: Vec3,
    half_extents: Vec3,
) -> Option<TraceCandidate> {
    let velocity = end - start;
    if velocity.length_squared() <= TRACE_AXIS_EPSILON {
        return None;
    }

    let mut enter_fraction = 0.0_f32;
    let mut leave_fraction = 1.0_f32;
    let mut hit_normal = Vec3::ZERO;
    let mut start_overlaps = true;

    let edges = [
        ((triangle.vertices[1]) - (triangle.vertices[0])),
        ((triangle.vertices[2]) - (triangle.vertices[1])),
        ((triangle.vertices[0]) - (triangle.vertices[2])),
    ];
    let box_axes = [
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::new(0.0, 0.0, 1.0),
    ];
    let mut axes = Vec::with_capacity(13);
    axes.extend_from_slice(&box_axes);
    axes.push(triangle.normal);
    for edge in edges {
        for axis in box_axes {
            axes.push(edge.cross(axis));
        }
    }

    for axis in axes {
        let Some(axis) = axis.normalize().filter(|a| vector_is_finite_nonzero(*a)) else {
            continue;
        };
        let center_projection = start.dot(axis);
        let radius = axis.dot_abs(half_extents);
        let a_min = center_projection - radius;
        let a_max = center_projection + radius;
        let (b_min, b_max) = project_triangle(triangle, axis);
        let velocity_projection = velocity.dot(axis);
        let overlaps_at_start = intervals_overlap(a_min, a_max, b_min, b_max);
        start_overlaps &= overlaps_at_start;

        if velocity_projection.abs() <= TRACE_AXIS_EPSILON {
            if !overlaps_at_start {
                return None;
            }
            continue;
        }

        let (axis_enter, axis_leave, normal) = if velocity_projection > 0.0 {
            (
                (b_min - a_max) / velocity_projection,
                (b_max - a_min) / velocity_projection,
                (axis * -1.0),
            )
        } else {
            (
                (b_max - a_min) / velocity_projection,
                (b_min - a_max) / velocity_projection,
                axis,
            )
        };

        if axis_enter > enter_fraction {
            enter_fraction = axis_enter;
            hit_normal = normal;
        }
        leave_fraction = leave_fraction.min(axis_leave);

        if enter_fraction - leave_fraction > TRACE_AXIS_EPSILON {
            return None;
        }
    }

    if start_overlaps || !(0.0..=1.0).contains(&enter_fraction) {
        return None;
    }

    // Back the hit off by the plane epsilon so resting contact stays
    // strictly separated: an exact-contact endpoint makes the NEXT trace
    // start overlapping on every axis, which this sweep reports as "no
    // hit" — the mover would sink through the terrain it is standing on.
    let backoff = TRACE_PLANE_EPSILON / velocity.length_squared().sqrt();
    Some(TraceCandidate {
        fraction: (enter_fraction - backoff).max(0.0),
        normal: hit_normal,
        start_solid: false,
    })
}

pub(super) fn brush_bounds_from_planes(planes: &[MapPlane]) -> Option<MapBounds> {
    let mut points = Vec::new();
    for first in 0..planes.len() {
        for second in first + 1..planes.len() {
            for third in second + 1..planes.len() {
                let a = planes[first];
                let b = planes[second];
                let c = planes[third];
                let Some(point) = plane_intersection(a, b, c) else {
                    continue;
                };
                if planes
                    .iter()
                    .all(|plane| point.dot(plane.normal) - plane.dist <= TRACE_INSIDE_EPSILON)
                {
                    points.push(point);
                }
            }
        }
    }
    bounds_from_points_iter(points.into_iter())
}

pub(super) fn plane_intersection(
    first: MapPlane,
    second: MapPlane,
    third: MapPlane,
) -> Option<Vec3> {
    let second_cross_third = second.normal.cross(third.normal);
    let denominator = first.normal.dot(second_cross_third);
    if denominator.abs() <= TRACE_AXIS_EPSILON {
        return None;
    }
    let numerator = second_cross_third * first.dist
        + third.normal.cross(first.normal) * second.dist
        + first.normal.cross(second.normal) * third.dist;
    let point = numerator / denominator;
    point
        .to_array()
        .iter()
        .all(|value| value.is_finite())
        .then_some(point)
}

pub(super) fn bounds_from_triangles(triangles: &[MapWalkTriangle]) -> Option<MapBounds> {
    bounds_from_points_iter(triangles.iter().flat_map(|triangle| triangle.vertices))
}

pub(super) fn bounds_from_points<const N: usize>(points: [Vec3; N]) -> Option<MapBounds> {
    bounds_from_points_iter(points.into_iter())
}

pub(super) fn bounds_from_points_iter(mut points: impl Iterator<Item = Vec3>) -> Option<MapBounds> {
    let first = points.next()?;
    if !first.is_finite() {
        return None;
    }
    let mut min = first;
    let mut max = first;
    for point in points {
        if !point.is_finite() {
            return None;
        }
        for axis in 0..3 {
            min[axis] = min[axis].min(point[axis]);
            max[axis] = max[axis].max(point[axis]);
        }
    }
    Some(MapBounds { min, max })
}

#[derive(Debug, Default)]
pub(super) struct BoundsBuilder {
    pub(super) min: Vec3,
    pub(super) max: Vec3,
    pub(super) has_points: bool,
}

impl BoundsBuilder {
    pub(super) fn push(&mut self, point: Vec3) {
        if !point.is_finite() {
            return;
        }
        if !self.has_points {
            self.min = point;
            self.max = point;
            self.has_points = true;
            return;
        }
        for (axis, value) in point.into_iter().enumerate() {
            self.min[axis] = self.min[axis].min(value);
            self.max[axis] = self.max[axis].max(value);
        }
    }

    pub(super) fn finish(self) -> Option<MapBounds> {
        self.has_points.then_some(MapBounds {
            min: self.min,
            max: self.max,
        })
    }
}

pub(super) fn swept_bounds(start: Vec3, end: Vec3, half_extents: Vec3) -> MapBounds {
    let min = std::array::from_fn(|axis| start[axis].min(end[axis]) - half_extents[axis]);
    let max = std::array::from_fn(|axis| start[axis].max(end[axis]) + half_extents[axis]);
    MapBounds {
        min: Vec3::from(min),
        max: Vec3::from(max),
    }
}

pub(super) fn expand_bounds(bounds: MapBounds, half_extents: Vec3) -> MapBounds {
    MapBounds {
        min: Vec3::from(std::array::from_fn(|axis| {
            bounds.min[axis] - half_extents[axis]
        })),
        max: Vec3::from(std::array::from_fn(|axis| {
            bounds.max[axis] + half_extents[axis]
        })),
    }
}

pub(super) fn bounds_intersect(left: MapBounds, right: MapBounds) -> bool {
    (0..3).all(|axis| left.min[axis] <= right.max[axis] && left.max[axis] >= right.min[axis])
}

pub(super) fn bounds_contains_point(bounds: MapBounds, point: Vec3) -> bool {
    (0..3).all(|axis| point[axis] >= bounds.min[axis] && point[axis] <= bounds.max[axis])
}

pub(super) fn bounds_volume(bounds: MapBounds) -> f32 {
    let extents =
        std::array::from_fn::<_, 3, _>(|axis| (bounds.max[axis] - bounds.min[axis]).max(0.0));
    extents[0] * extents[1] * extents[2]
}

pub(super) fn bsp_world_bounds(bsp: &MapBsp) -> Option<MapBounds> {
    bsp.models.first().and_then(|model| {
        let min = model.mins;
        let max = model.maxs;
        min.iter()
            .chain(max.iter())
            .all(|value| value.is_finite())
            .then_some(MapBounds {
                min: Vec3::from(min),
                max: Vec3::from(max),
            })
    })
}

pub(super) fn project_triangle(triangle: &MapWalkTriangle, axis: Vec3) -> (f32, f32) {
    let mut min = (triangle.vertices[0]).dot(axis);
    let mut max = min;
    for vertex in triangle.vertices.iter().copied().skip(1) {
        let projection = vertex.dot(axis);
        min = min.min(projection);
        max = max.max(projection);
    }
    (min, max)
}

pub(super) fn intervals_overlap(a_min: f32, a_max: f32, b_min: f32, b_max: f32) -> bool {
    a_min <= b_max && a_max >= b_min
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct MapLeafLocator {
    pub(super) planes: Vec<MapPlane>,
    pub(super) nodes: Vec<MapNode>,
    pub(super) leaves: Vec<MapLeaf>,
}

impl MapLeafLocator {
    pub(super) fn from_bsp(bsp: &MapBsp) -> Self {
        Self {
            planes: bsp
                .planes
                .iter()
                .map(|plane| MapPlane {
                    normal: Vec3::from(plane.normal),
                    dist: plane.dist,
                })
                .collect(),
            nodes: bsp
                .nodes
                .iter()
                .map(|node| MapNode {
                    plane_index: node.plane,
                    children: node.children,
                })
                .collect(),
            leaves: bsp
                .leaves
                .iter()
                .map(|leaf| MapLeaf {
                    cluster: leaf.cluster,
                    mins: leaf.mins,
                    maxs: leaf.maxs,
                })
                .collect(),
        }
    }

    pub(super) fn leaf_at(&self, point: Vec3) -> Option<usize> {
        walk_to_leaf(
            point,
            self.nodes.len(),
            |index| {
                self.nodes
                    .get(index)
                    .map(|node| (node.plane_index, node.children))
            },
            |index| {
                let plane = self.planes.get(usize::try_from(index).ok()?)?;
                Some((plane.normal, plane.dist))
            },
        )
    }

    pub(super) fn clusters_for_aabb(
        &self,
        bounds_min: Vec3,
        bounds_max: Vec3,
        cluster_count: u32,
    ) -> MapPropVisibility {
        let Some(MapBounds {
            min: bounds_min,
            max: bounds_max,
        }) = normalized_prop_aabb(bounds_min, bounds_max)
        else {
            log::debug!(
                "bsp prop visibility AABB invalid {bounds_min:?}..{bounds_max:?}: using Always"
            );
            return MapPropVisibility::Always;
        };
        let extent = [
            bounds_max[0] - bounds_min[0],
            bounds_max[1] - bounds_min[1],
            bounds_max[2] - bounds_min[2],
        ];
        if extent.iter().any(|axis| *axis > PROP_AABB_MAX_EXTENT) {
            log::debug!(
                "bsp prop visibility AABB huge {bounds_min:?}..{bounds_max:?}: using Always"
            );
            return MapPropVisibility::Always;
        }
        if self.nodes.is_empty() {
            log::debug!("bsp prop visibility AABB walk missing BSP nodes: using Always");
            return MapPropVisibility::Always;
        }

        let mut stack = vec![(0_usize, 0_usize)];
        let mut visited_leaves = 0_usize;
        let mut clusters = BTreeSet::<u32>::new();
        while let Some((node_index, depth)) = stack.pop() {
            if depth > PROP_AABB_MAX_DEPTH {
                log::debug!(
                    "bsp prop visibility AABB walk exceeded depth cap {PROP_AABB_MAX_DEPTH}: using Always"
                );
                return MapPropVisibility::Always;
            }
            let Some(node) = self.nodes.get(node_index) else {
                log::debug!(
                    "bsp prop visibility AABB walk node {node_index} missing: using Always"
                );
                return MapPropVisibility::Always;
            };
            let Some(plane_index) = usize::try_from(node.plane_index).ok() else {
                log::debug!(
                    "bsp prop visibility AABB walk invalid plane {}: using Always",
                    node.plane_index
                );
                return MapPropVisibility::Always;
            };
            let Some(plane) = self.planes.get(plane_index) else {
                log::debug!(
                    "bsp prop visibility AABB walk plane {plane_index} missing: using Always"
                );
                return MapPropVisibility::Always;
            };
            let side = aabb_plane_side(bounds_min, bounds_max, *plane);
            for child in side.children(node.children) {
                let Some(child) = NodeChild::decode(child) else {
                    log::debug!(
                        "bsp prop visibility AABB walk invalid child {child}: using Always"
                    );
                    return MapPropVisibility::Always;
                };
                match child {
                    NodeChild::Leaf(leaf_index) => {
                        visited_leaves = visited_leaves.saturating_add(1);
                        if visited_leaves > PROP_AABB_MAX_LEAVES {
                            log::debug!(
                                "bsp prop visibility AABB walk exceeded leaf cap {PROP_AABB_MAX_LEAVES}: using Always"
                            );
                            return MapPropVisibility::Always;
                        }
                        let Some(leaf) = self.leaves.get(leaf_index) else {
                            log::debug!(
                                "bsp prop visibility AABB walk leaf {leaf_index} missing: using Always"
                            );
                            return MapPropVisibility::Always;
                        };
                        if cluster_in_range(leaf.cluster, cluster_count) {
                            clusters.insert(u32::try_from(leaf.cluster).unwrap_or(0));
                        }
                    }
                    NodeChild::Node(node_index) => {
                        stack.push((node_index, depth.saturating_add(1)));
                    }
                }
            }
        }

        if clusters.is_empty() {
            log::debug!(
                "bsp prop visibility AABB {bounds_min:?}..{bounds_max:?} found no valid clusters: using Always"
            );
            MapPropVisibility::Always
        } else {
            MapPropVisibility::Clusters(clusters.into_iter().collect())
        }
    }
}

pub(super) fn normalized_prop_aabb(bounds_min: Vec3, bounds_max: Vec3) -> Option<MapBounds> {
    let mut min = [0.0; 3];
    let mut max = [0.0; 3];
    for axis in 0..3 {
        if !bounds_min[axis].is_finite() || !bounds_max[axis].is_finite() {
            return None;
        }
        min[axis] = bounds_min[axis].min(bounds_max[axis]);
        max[axis] = bounds_min[axis].max(bounds_max[axis]);
    }
    (min != max).then_some(MapBounds {
        min: Vec3::from(min),
        max: Vec3::from(max),
    })
}

/// Which side(s) of a plane an AABB occupies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PlaneSide {
    Front,
    Back,
    Both,
}

impl PlaneSide {
    /// The child links to descend into, dropping the back child when both
    /// sides point at the same one.
    pub(super) fn children(self, children: [i32; 2]) -> impl Iterator<Item = i32> {
        let [front, back] = children;
        let (first, second) = match self {
            Self::Front => (front, None),
            Self::Back => (back, None),
            Self::Both => (front, (back != front).then_some(back)),
        };
        std::iter::once(first).chain(second)
    }
}

pub(super) fn aabb_plane_side(bounds_min: Vec3, bounds_max: Vec3, plane: MapPlane) -> PlaneSide {
    let center = [
        (bounds_min[0] + bounds_max[0]) * 0.5,
        (bounds_min[1] + bounds_max[1]) * 0.5,
        (bounds_min[2] + bounds_max[2]) * 0.5,
    ];
    let half = [
        (bounds_max[0] - bounds_min[0]) * 0.5,
        (bounds_max[1] - bounds_min[1]) * 0.5,
        (bounds_max[2] - bounds_min[2]) * 0.5,
    ];
    let distance = Vec3::from(center).dot(plane.normal) - plane.dist;
    let radius = half[0] * plane.normal[0].abs()
        + half[1] * plane.normal[1].abs()
        + half[2] * plane.normal[2].abs();
    if distance > radius {
        PlaneSide::Front
    } else if distance < -radius {
        PlaneSide::Back
    } else {
        PlaneSide::Both
    }
}
