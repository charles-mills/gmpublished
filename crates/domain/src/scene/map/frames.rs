//! Source displacement grid normals and authored texture-coordinate frames.
use super::{BspError, MapBsp};
use crate::math::Vec3;
use vformats::bsp::{DispInfo, DispSubNeighbor, TexInfo};

pub(super) fn texture_frame(normal: Vec3, plane: Vec3, texinfo: &TexInfo) -> (Vec3, Vec3) {
    let axis = |v: [f32; 4]| Vec3::new(v[0], v[1], v[2]);
    let authored_s = axis(texinfo.texture_vecs[0]);
    let authored_t = axis(texinfo.texture_vecs[1]);
    let t = authored_t.normalize_or_zero();
    let s = normal.cross(t).normalize_or_zero();
    let t = s.cross(normal).normalize_or_zero();
    let s = if plane.dot(authored_s.cross(authored_t)) > 0.0 {
        -s
    } else {
        s
    };
    (s, t)
}

/// The runtime edge rebuild uses a different tangent operation order from
/// the displacement's core frame. Keep the normal-generation stage with each
/// vertex rather than infer it from grid coordinates or normal length.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum NormalStage {
    #[default]
    Core,
    TJunction,
    Corner,
    SharedCorner,
    SharedEdge,
    InterpolatedEdge,
}

#[derive(Default)]
pub(super) struct DisplacementNormalGrid {
    pub normals: Vec<Vec3>,
    pub stages: Vec<NormalStage>,
    pub tangent_s: Vec<Vec3>,
}

pub(super) fn displacement_frame(
    normal: Vec3,
    plane: Vec3,
    texinfo: &TexInfo,
    stage: NormalStage,
    shared_s: Option<Vec3>,
) -> (Vec3, Vec3) {
    let (core_s, t) = texture_frame(normal, plane, texinfo);
    let s = if matches!(
        stage,
        NormalStage::SharedEdge | NormalStage::SharedCorner | NormalStage::TJunction
    ) {
        shared_s.unwrap_or(core_s)
    } else {
        core_s
    };
    // Native direction-cube controls identify this operation order at shared
    // edges, shared corners and split-edge T-junctions. Lone corners and
    // interpolated-edge stages retain the core order; do not infer the
    // private runtime operation from a vertex merely lying on the boundary.
    let t = if matches!(
        stage,
        NormalStage::SharedEdge | NormalStage::SharedCorner | NormalStage::TJunction
    ) {
        // The runtime cross product is deliberately not normalized. When
        // neighboring texture frames oppose each other, shared S can have a
        // large component along N; normalizing T would invent its magnitude.
        s.cross(normal)
    } else {
        t
    };
    (s, t)
}

#[derive(Default)]
struct Grid {
    steps: usize,
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    stages: Vec<NormalStage>,
    tangent_s: Vec<Vec3>,
}
impl Grid {
    fn index(&self, point: [usize; 2]) -> usize {
        point[1] * (self.steps + 1) + point[0]
    }
    fn corners(&self) -> [[usize; 2]; 4] {
        let n = self.steps;
        [[0, 0], [0, n], [n, n], [n, 0]]
    }
    fn closest_corner(&self, position: Vec3) -> Option<usize> {
        self.corners()
            .into_iter()
            .map(|p| self.index(p))
            .filter_map(|i| {
                self.positions
                    .get(i)
                    .map(|p| (i, (*p - position).length_squared()))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .filter(|(_, d)| *d <= 0.01)
            .map(|(i, _)| i)
    }
}

pub(super) fn displacement_normals(bsp: &MapBsp) -> Result<Vec<DisplacementNormalGrid>, BspError> {
    let mut grids = Vec::with_capacity(bsp.displacements.len());
    for (index, disp) in bsp.displacements.iter().enumerate() {
        let Some(face) = bsp
            .faces
            .iter()
            .find(|face| usize::try_from(face.displacement).ok() == Some(index))
        else {
            grids.push(Grid::default());
            continue;
        };
        if !(2..=4).contains(&disp.power) {
            return Err(BspError::Malformed {
                message: "unsupported displacement power",
            });
        }
        let steps = 1_usize << disp.power;
        let vertices = super::mesh::displacement_grid(bsp, face, disp, steps)?;
        let positions = vertices.into_iter().map(|v| v.position).collect::<Vec<_>>();
        if positions.len() != (steps + 1) * (steps + 1) {
            return Err(BspError::Malformed {
                message: "incomplete displacement vertex grid",
            });
        }
        let normals = grid_normals(&positions, steps);
        let tangent_s = normals
            .iter()
            .map(|&normal| {
                bsp.face_texinfo(face).map_or(Vec3::ZERO, |texinfo| {
                    texture_frame(normal, bsp.face_plane_normal(face), texinfo).0
                })
            })
            .collect();
        grids.push(Grid {
            steps,
            stages: vec![NormalStage::Core; normals.len()],
            positions,
            normals,
            tangent_s,
        });
    }
    blend_t_junctions(&mut grids, &bsp.displacements);
    blend_corners(&mut grids, &bsp.displacements);
    blend_edges(&mut grids, &bsp.displacements);
    Ok(grids
        .into_iter()
        .map(|g| DisplacementNormalGrid {
            normals: g.normals,
            stages: g.stages,
            tangent_s: g.tangent_s,
        })
        .collect())
}

fn grid_normals(positions: &[Vec3], steps: usize) -> Vec<Vec3> {
    let side = steps + 1;
    let mut normals = vec![Vec3::ZERO; positions.len()];
    let mut counts = vec![0_u8; positions.len()];
    // Core displacement normals use both unit triangle normals from every
    // adjacent quad, including its opposite half. They use a fixed diagonal,
    // independently of the checkerboard render tessellation, and retain the
    // average's length rather than normalizing it afterward.
    for y in 0..steps {
        for x in 0..steps {
            let a = y * side + x;
            let b = a + 1;
            let c = a + side;
            let d = c + 1;
            let first = (positions[b] - positions[a])
                .cross(positions[c] - positions[a])
                .normalize_or_zero();
            let second = (positions[d] - positions[b])
                .cross(positions[c] - positions[b])
                .normalize_or_zero();
            let sum = first + second;
            for i in [a, b, c, d] {
                normals[i] += sum;
                counts[i] += 2;
            }
        }
    }
    normals
        .into_iter()
        .zip(counts)
        .map(|(n, c)| n / f32::from(c.max(1)))
        .collect()
}

fn valid_neighbor(grids: &[Grid], neighbor: DispSubNeighbor) -> Option<usize> {
    let index = usize::from(neighbor.neighbor);
    grids
        .get(index)
        .filter(|g| !g.normals.is_empty())
        .map(|_| index)
}
fn normal(grids: &[Grid], grid: usize, vertex: usize) -> Vec3 {
    grids[grid].normals[vertex]
}
fn set_normal(grids: &mut [Grid], grid: usize, vertex: usize, n: Vec3, stage: NormalStage) {
    grids[grid].normals[vertex] = n;
    grids[grid].stages[vertex] = stage;
}
fn edge_point(steps: usize, edge: usize, free: usize) -> [usize; 2] {
    match edge {
        0 => [0, free],
        1 => [free, steps],
        2 => [steps, free],
        _ => [free, 0],
    }
}

fn blend_t_junctions(grids: &mut [Grid], infos: &[DispInfo]) {
    for (i, info) in infos.iter().enumerate() {
        if grids[i].normals.is_empty() {
            continue;
        }
        for (edge, neighbors) in info.edge_neighbors.iter().enumerate() {
            let (Some(a), Some(b)) = (
                valid_neighbor(grids, neighbors[0]),
                valid_neighbor(grids, neighbors[1]),
            ) else {
                continue;
            };
            let mid = grids[i].index(edge_point(grids[i].steps, edge, grids[i].steps / 2));
            let position = grids[i].positions[mid];
            let (Some(av), Some(bv)) = (
                grids[a].closest_corner(position),
                grids[b].closest_corner(position),
            ) else {
                continue;
            };
            let n = (normal(grids, i, mid) + normal(grids, a, av) + normal(grids, b, bv))
                .normalize_or_zero();
            let s = (grids[i].tangent_s[mid] + grids[a].tangent_s[av] + grids[b].tangent_s[bv])
                .normalize_or_zero();
            for (g, v) in [(i, mid), (a, av), (b, bv)] {
                set_normal(grids, g, v, n, NormalStage::TJunction);
                grids[g].tangent_s[v] = s;
            }
        }
    }
}
fn blend_corners(grids: &mut [Grid], infos: &[DispInfo]) {
    for (i, info) in infos.iter().enumerate() {
        if grids[i].normals.is_empty() {
            continue;
        }
        // Retain authored list ordering, including repeated edge neighbors,
        // matching the compiler's sequential corner smoothing operation.
        let neighbors = info
            .corner_neighbors
            .iter()
            .flat_map(|c| {
                c.neighbors
                    .iter()
                    .take(usize::from(c.count).min(4))
                    .copied()
            })
            .chain(info.edge_neighbors.iter().flatten().map(|s| s.neighbor))
            .map(usize::from)
            .filter(|&j| grids.get(j).is_some_and(|g| !g.normals.is_empty()))
            .collect::<Vec<_>>();
        for point in grids[i].corners() {
            let vertex = grids[i].index(point);
            let position = grids[i].positions[vertex];
            let sharing = neighbors
                .iter()
                .filter_map(|&j| grids[j].closest_corner(position).map(|v| (j, v)))
                .collect::<Vec<_>>();
            let n = sharing
                .iter()
                .fold(normal(grids, i, vertex), |n, &(j, v)| {
                    n + normal(grids, j, v)
                })
                .normalize_or_zero();
            let stage = if sharing.is_empty() {
                NormalStage::Corner
            } else {
                NormalStage::SharedCorner
            };
            let s = sharing
                .iter()
                .fold(grids[i].tangent_s[vertex], |s, &(j, v)| {
                    s + grids[j].tangent_s[v]
                })
                .normalize_or_zero();
            set_normal(grids, i, vertex, n, stage);
            if !sharing.is_empty() {
                grids[i].tangent_s[vertex] = s;
            }
            for (j, v) in sharing {
                set_normal(grids, j, v, n, stage);
                grids[j].tangent_s[v] = s;
            }
        }
    }
}

fn span(steps: usize, edge: usize, span: u8) -> Option<([usize; 2], [usize; 2])> {
    if span > 2 {
        return None;
    }
    let start = if span == 2 { steps / 2 } else { 0 };
    let end = if span == 1 { steps / 2 } else { steps };
    let a = edge_point(steps, edge, start);
    let b = edge_point(steps, edge, end);
    Some(if edge >= 2 { (b, a) } else { (a, b) })
}

fn edge_pairs(
    steps: usize,
    edge: usize,
    neighbor_steps: usize,
    sub: DispSubNeighbor,
) -> Vec<([usize; 2], [usize; 2])> {
    if sub.orientation > 3 || (sub.span != 0 && sub.neighbor_span != 0) {
        return Vec::new();
    }
    let Some((start, end)) = span(steps, edge, sub.span) else {
        return Vec::new();
    };
    let neighbor_edge = (edge + 2 + usize::from(sub.orientation)) & 3;
    let Some((nb_end, nb_start)) = span(neighbor_steps, neighbor_edge, sub.neighbor_span) else {
        return Vec::new();
    };
    let free = usize::from(edge.is_multiple_of(2));
    let length = start[free].abs_diff(end[free]);
    let nb_length = nb_start[1 - neighbor_edge % 2].abs_diff(nb_end[1 - neighbor_edge % 2]);
    let increment = (length / nb_length.max(1)).max(1);
    let mut result = Vec::new();
    for value in (start[free].min(end[free])..=start[free].max(end[free])).step_by(increment) {
        let fixed =
            ((value as i32 - start[free] as i32) * 65536) / (end[free] as i32 - start[free] as i32);
        let nb = std::array::from_fn(|dim| {
            (nb_start[dim] as i32 + ((nb_end[dim] as i32 - nb_start[dim] as i32) * fixed) / 65536)
                as usize
        });
        result.push((edge_point(steps, edge, value), nb));
    }
    result
}
fn blend_edges(grids: &mut [Grid], infos: &[DispInfo]) {
    for (i, info) in infos.iter().enumerate() {
        if grids[i].normals.is_empty() {
            continue;
        }
        for (edge, subs) in info.edge_neighbors.iter().enumerate() {
            for &sub in subs {
                let Some(j) = valid_neighbor(grids, sub) else {
                    continue;
                };
                let pairs = edge_pairs(grids[i].steps, edge, grids[j].steps, sub);
                for k in 1..pairs.len() {
                    let (point, nb_point) = pairs[k];
                    let vertex = grids[i].index(point);
                    if k + 1 < pairs.len() {
                        let nb_vertex = grids[j].index(nb_point);
                        let n = (normal(grids, i, vertex) + normal(grids, j, nb_vertex))
                            .normalize_or_zero();
                        // Runtime blends pre-smoothing S independently of N;
                        // rebuilding S from the blended normal loses its
                        // authored shared-frame component along final N.
                        let s = (grids[i].tangent_s[vertex] + grids[j].tangent_s[nb_vertex])
                            .normalize_or_zero();
                        set_normal(grids, i, vertex, n, NormalStage::SharedEdge);
                        set_normal(grids, j, nb_vertex, n, NormalStage::SharedEdge);
                        grids[i].tangent_s[vertex] = s;
                        grids[j].tangent_s[nb_vertex] = s;
                    }
                    let previous = pairs[k - 1].0;
                    let prev_vertex = grids[i].index(previous);
                    let free = usize::from(edge.is_multiple_of(2));
                    let a = previous[free];
                    let b = point[free];
                    for value in a + 1..b {
                        let t = (value - a) as f32 / (b - a) as f32;
                        let n = (normal(grids, i, prev_vertex) * (1.0 - t)
                            + normal(grids, i, vertex) * t)
                            .normalize_or_zero();
                        let v = grids[i].index(edge_point(grids[i].steps, edge, value));
                        set_normal(grids, i, v, n, NormalStage::InterpolatedEdge);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quad_normal_uses_both_halves_for_every_corner_and_preserves_average_length() {
        let positions = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(1.0, 1.0, 1.0),
        ];
        let second = Vec3::new(-1.0, -1.0, 1.0).normalize_or_zero();
        let expected = (Vec3::new(0.0, 0.0, 1.0) + second) * 0.5;
        let actual = grid_normals(&positions, 1);
        for normal in actual {
            assert!((normal - expected).length() < 1e-6);
            assert!(normal.length() < 0.9);
        }
    }

    #[test]
    fn texture_frame_keeps_authored_t_direction_and_independent_handedness() {
        let normal = Vec3::new(0.0, 0.6, 0.8) * 0.75;
        let mut tex = TexInfo {
            texture_vecs: [[2.0, 0.0, 0.0, 19.0], [0.0, 3.0, 0.0, -4.0]],
            lightmap_vecs: [[0.0; 4]; 2],
            flags: 0,
            texdata: 0,
        };
        let (s, t) = texture_frame(normal, Vec3::new(0.0, 0.0, 1.0), &tex);
        assert!((s - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-6);
        assert!((t - Vec3::new(0.0, 0.8, -0.6)).length() < 1e-6);
        assert!(s.dot(normal).abs() < 1e-6 && t.dot(normal).abs() < 1e-6);
        tex.texture_vecs[0][0] = -2.0;
        let (mirrored_s, mirrored_t) = texture_frame(normal, Vec3::new(0.0, 0.0, 1.0), &tex);
        assert_eq!(mirrored_s, -s);
        assert_eq!(mirrored_t, t);
    }

    #[test]
    fn shared_edge_rebuild_preserves_normal_s_and_core_frames() {
        let normal = Vec3::new(0.3, -0.2, 0.8);
        let plane = Vec3::new(0.0, 0.0, 1.0);
        let mut tex = TexInfo {
            texture_vecs: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            lightmap_vecs: [[0.0; 4]; 2],
            flags: 0,
            texdata: 0,
        };
        let core = texture_frame(normal, plane, &tex);
        for stage in [
            NormalStage::SharedEdge,
            NormalStage::SharedCorner,
            NormalStage::TJunction,
        ] {
            let shared = displacement_frame(normal, plane, &tex, stage, None);
            assert_eq!(shared.0, core.0);
            assert!((shared.1 + core.1 * normal.length()).length() < 1e-6);
        }
        for stage in [
            NormalStage::Core,
            NormalStage::Corner,
            NormalStage::InterpolatedEdge,
        ] {
            assert_eq!(displacement_frame(normal, plane, &tex, stage, None), core);
        }
        tex.texture_vecs[0][0] = -1.0;
        let mirrored = displacement_frame(normal, plane, &tex, NormalStage::SharedEdge, None);
        let core = texture_frame(normal, plane, &tex);
        assert_eq!(mirrored.0, core.0);
        assert!((mirrored.1 - core.1 * normal.length()).length() < 1e-6);
    }

    #[test]
    fn shared_edge_blends_core_s_independently_of_final_normal() {
        let make = |n, s| Grid {
            steps: 4,
            positions: vec![Vec3::ZERO; 25],
            normals: vec![n; 25],
            stages: vec![NormalStage::Core; 25],
            tangent_s: vec![s; 25],
        };
        let a_n = Vec3::new(0.0, 0.0, 0.5);
        let b_n = Vec3::new(0.6, 0.0, 0.8);
        let a_s = Vec3::new(1.0, 0.0, 0.0);
        let b_s = Vec3::new(0.8, 0.0, -0.6);
        let mut grids = vec![make(a_n, a_s), make(b_n, b_s)];
        let mut infos = vec![
            super::super::tests::displacement_test_bsp(2, [Vec3::ZERO; 4], 0)
                .displacements[0];
            2
        ];
        infos[0].edge_neighbors[0][0] = DispSubNeighbor {
            neighbor: 1,
            orientation: 0,
            span: 0,
            neighbor_span: 0,
        };
        blend_edges(&mut grids, &infos);
        let expected_n = (a_n + b_n).normalize_or_zero();
        let expected_s = (a_s + b_s).normalize_or_zero();
        for (grid, point) in [(0, [0, 2]), (1, [4, 2])] {
            let index = grids[grid].index(point);
            assert_eq!(grids[grid].normals[index], expected_n);
            assert_eq!(grids[grid].tangent_s[index], expected_s);
        }
        assert!(expected_s.dot(expected_n).abs() > 0.1);
        let tex = TexInfo {
            texture_vecs: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            lightmap_vecs: [[0.0; 4]; 2],
            flags: 0,
            texdata: 0,
        };
        let shared = displacement_frame(
            expected_n,
            Vec3::new(0.0, 0.0, 1.0),
            &tex,
            NormalStage::SharedEdge,
            Some(expected_s),
        );
        assert_eq!(shared.0, expected_s);
        assert_eq!(shared.1, expected_s.cross(expected_n));
        assert!(shared.1.length() < 0.999);
        let untouched = grids[0].index([1, 2]);
        assert_eq!(grids[0].normals[untouched], a_n);
        assert_eq!(grids[0].tangent_s[untouched], a_s);
        for stage in [
            NormalStage::Core,
            NormalStage::Corner,
            NormalStage::InterpolatedEdge,
        ] {
            assert_eq!(
                displacement_frame(a_n, Vec3::new(0.0, 0.0, 1.0), &tex, stage, Some(expected_s)),
                texture_frame(a_n, Vec3::new(0.0, 0.0, 1.0), &tex)
            );
        }
    }

    #[test]
    fn shared_edge_t_preserves_raw_cross_magnitude_for_opposing_texture_frames() {
        let tex = TexInfo {
            texture_vecs: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            lightmap_vecs: [[0.0; 4]; 2],
            flags: 0,
            texdata: 0,
        };
        let normal = Vec3::new(0.0, 0.0, 1.0);
        let shared_s = Vec3::new(0.6, 0.0, 0.8);
        let (s, t) = displacement_frame(
            normal,
            normal,
            &tex,
            NormalStage::SharedEdge,
            Some(shared_s),
        );
        assert_eq!(s, shared_s);
        assert_eq!(t, Vec3::new(0.0, -0.6, 0.0));
        assert_eq!(t.length(), 0.6);
    }

    #[test]
    fn authored_edge_spans_rotation_and_power_mismatch_map_exact_grid_indices() {
        for orientation in 0..4 {
            let sub = DispSubNeighbor {
                neighbor: 1,
                orientation,
                span: 0,
                neighbor_span: 0,
            };
            let pairs = edge_pairs(16, 0, 4, sub);
            assert_eq!(pairs.len(), 5);
            assert_eq!(
                pairs.iter().map(|(p, _)| p[1]).collect::<Vec<_>>(),
                [0, 4, 8, 12, 16]
            );
            let expected = match orientation {
                0 => vec![[4, 0], [4, 1], [4, 2], [4, 3], [4, 4]],
                1 => vec![[0, 0], [1, 0], [2, 0], [3, 0], [4, 0]],
                2 => vec![[0, 4], [0, 3], [0, 2], [0, 1], [0, 0]],
                _ => vec![[4, 4], [3, 4], [2, 4], [1, 4], [0, 4]],
            };
            assert_eq!(
                pairs.into_iter().map(|(_, p)| p).collect::<Vec<_>>(),
                expected
            );
        }
        let sub = DispSubNeighbor {
            neighbor: 1,
            orientation: 0,
            span: 1,
            neighbor_span: 0,
        };
        let pairs = edge_pairs(8, 0, 8, sub);
        assert_eq!(pairs.first().unwrap(), &([0, 0], [8, 0]));
        assert_eq!(pairs.last().unwrap(), &([0, 4], [8, 8]));
        assert_eq!(pairs.len(), 5);
    }

    #[test]
    fn unequal_power_edge_interpolates_high_resolution_normals_between_shared_vertices() {
        let make = |steps, n| Grid {
            steps,
            positions: vec![Vec3::ZERO; (steps + 1) * (steps + 1)],
            normals: vec![n; (steps + 1) * (steps + 1)],
            stages: vec![NormalStage::Core; (steps + 1) * (steps + 1)],
            tangent_s: vec![Vec3::new(1.0, 0.0, 0.0); (steps + 1) * (steps + 1)],
        };
        let mut grids = vec![
            make(8, Vec3::new(0.0, 0.0, 1.0)),
            make(4, Vec3::new(1.0, 0.0, 0.0)),
        ];
        let mut infos = vec![
            super::super::tests::displacement_test_bsp(2, [Vec3::ZERO; 4], 0)
                .displacements[0];
            2
        ];
        infos[0].edge_neighbors[0][0] = DispSubNeighbor {
            neighbor: 1,
            orientation: 0,
            span: 0,
            neighbor_span: 0,
        };
        blend_edges(&mut grids, &infos);
        let shared = Vec3::new(1.0, 0.0, 1.0).normalize_or_zero();
        assert_eq!(grids[0].normals[grids[0].index([0, 2])], shared);
        let halfway = (Vec3::new(0.0, 0.0, 1.0) + shared).normalize_or_zero();
        assert!((grids[0].normals[grids[0].index([0, 1])] - halfway).length() < 1e-6);
        assert_eq!(grids[1].normals[grids[1].index([4, 1])], shared);
        assert_eq!(
            grids[0].stages[grids[0].index([0, 2])],
            NormalStage::SharedEdge
        );
        assert_eq!(
            grids[1].stages[grids[1].index([4, 1])],
            NormalStage::SharedEdge
        );
        assert_eq!(
            grids[0].stages[grids[0].index([0, 1])],
            NormalStage::InterpolatedEdge
        );
        assert_eq!(grids[0].stages[grids[0].index([1, 2])], NormalStage::Core);
        assert_eq!(grids[0].stages[grids[0].index([0, 0])], NormalStage::Core);
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    #[test]
    fn split_edge_midpoint_blends_exactly_the_two_authored_neighbor_corners() {
        let make = |x_offset, y_offset, y_scale, n| Grid {
            steps: 4,
            positions: (0..=4)
                .flat_map(|y| {
                    (0..=4).map(move |x| {
                        Vec3::new(
                            x_offset + x as f32 / 4.0,
                            y_offset + y as f32 * y_scale / 4.0,
                            0.0,
                        )
                    })
                })
                .collect(),
            normals: vec![n; 25],
            stages: vec![NormalStage::Core; 25],
            tangent_s: vec![Vec3::new(1.0, 0.0, 0.0); 25],
        };
        let mut grids = vec![
            make(0.0, 0.0, 1.0, Vec3::new(0.0, 0.0, 1.0)),
            make(-1.0, 0.0, 0.5, Vec3::new(1.0, 0.0, 0.0)),
            make(-1.0, 0.5, 0.5, Vec3::new(0.0, 1.0, 0.0)),
        ];
        grids[1].tangent_s.fill(Vec3::new(0.0, 1.0, 0.0));
        grids[2].tangent_s.fill(Vec3::new(0.0, 0.0, -1.0));
        let mut infos = vec![
            super::super::tests::displacement_test_bsp(2, [Vec3::ZERO; 4], 0)
                .displacements[0];
            3
        ];
        infos[0].edge_neighbors[0] = [
            DispSubNeighbor {
                neighbor: 1,
                orientation: 0,
                span: 1,
                neighbor_span: 0,
            },
            DispSubNeighbor {
                neighbor: 2,
                orientation: 0,
                span: 2,
                neighbor_span: 0,
            },
        ];
        blend_t_junctions(&mut grids, &infos);
        let expected = Vec3::splat(1.0).normalize_or_zero();
        let expected_s = Vec3::new(1.0, 1.0, -1.0).normalize_or_zero();
        for (g, p) in [(0, [0, 2]), (1, [4, 4]), (2, [4, 0])] {
            assert_eq!(grids[g].normals[grids[g].index(p)], expected);
            assert_eq!(grids[g].tangent_s[grids[g].index(p)], expected_s);
            assert_eq!(grids[g].stages[grids[g].index(p)], NormalStage::TJunction);
        }
        assert_eq!(
            grids[0].normals[grids[0].index([1, 2])],
            Vec3::new(0.0, 0.0, 1.0)
        );
    }

    #[test]
    fn corner_provenance_distinguishes_authored_sharing_from_lone_normalization() {
        let make = |offset| Grid {
            steps: 2,
            positions: (0..=2)
                .flat_map(|y| (0..=2).map(move |x| Vec3::new(offset + x as f32, y as f32, 0.0)))
                .collect(),
            normals: vec![Vec3::new(0.0, 0.0, 1.0); 9],
            stages: vec![NormalStage::Core; 9],
            tangent_s: vec![Vec3::new(1.0, 0.0, 0.0); 9],
        };
        let mut grids = vec![make(0.0), make(2.0)];
        grids[1].tangent_s.fill(Vec3::new(0.0, 0.6, 0.8));
        let mut infos = vec![
            super::super::tests::displacement_test_bsp(2, [Vec3::ZERO; 4], 0)
                .displacements[0];
            2
        ];
        infos[0].edge_neighbors[2][0].neighbor = 1;
        infos[1].edge_neighbors[0][0].neighbor = 0;
        blend_corners(&mut grids, &infos);
        let expected_s = Vec3::new(1.0, 0.6, 0.8).normalize_or_zero();
        for (g, p) in [(0, [2, 0]), (0, [2, 2]), (1, [0, 0]), (1, [0, 2])] {
            assert_eq!(
                grids[g].stages[grids[g].index(p)],
                NormalStage::SharedCorner
            );
            assert!((grids[g].tangent_s[grids[g].index(p)] - expected_s).length() < 1e-6);
        }
        assert_eq!(grids[0].stages[grids[0].index([0, 0])], NormalStage::Corner);
        assert_eq!(
            grids[0].tangent_s[grids[0].index([0, 0])],
            Vec3::new(1.0, 0.0, 0.0)
        );
        assert_eq!(grids[0].stages[grids[0].index([1, 1])], NormalStage::Core);
    }

    #[test]
    fn brush_vertices_use_compiled_corner_indices_without_normalizing_or_reordering() {
        let corners = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, 4.0, 0.0),
            Vec3::new(4.0, 4.0, 0.0),
            Vec3::new(4.0, 0.0, 0.0),
        ];
        let mut bsp = super::super::tests::displacement_test_bsp(2, corners, 0);
        bsp.vertex_normals = vec![Vec3::new(0.0, 0.6, 0.8) * 0.75, Vec3::new(0.0, 0.0, 1.0)];
        bsp.vertex_normal_indices = vec![1, 0, 1, 0];
        bsp.face_normal_offsets = vec![0];
        let tex = TexInfo {
            texture_vecs: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            lightmap_vecs: [[0.0; 4]; 2],
            flags: 0,
            texdata: 0,
        };
        let mut meshes = super::super::mesh::BuildMeshes::default();
        let mesh = meshes.get_or_insert(0, super::super::mesh::GeometryPartition::Visible, false);
        super::super::mesh::append_brush_face(
            &bsp,
            &bsp.faces[0],
            0,
            &tex,
            Vec3::new(0.0, 0.0, 1.0),
            mesh,
            None,
        )
        .unwrap();
        for (i, v) in mesh.vertices.iter().enumerate() {
            assert_eq!(v.vertex.position, corners[i]);
            assert_eq!(
                v.vertex.normal,
                bsp.vertex_normals[usize::from(bsp.vertex_normal_indices[i])]
            );
            assert!(v.vertex.normal.dot(v.vertex.tangent_s).abs() < 1e-6);
            assert!(v.vertex.normal.dot(v.vertex.tangent_t).abs() < 1e-6);
        }
        assert_eq!(mesh.indices, [0, 1, 2, 0, 2, 3]);
    }
}
