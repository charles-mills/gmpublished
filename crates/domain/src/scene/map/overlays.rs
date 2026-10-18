use super::projection::{SurfaceProjection, append_surface_polygon};
use super::{
    BspError, GeometryPartition, MapBsp, MapPropVisibility, MapSurfaceVertex, Overlay,
    is_preview_material_visible, normalize_material_name, vector_is_finite_nonzero,
};
use crate::math::Vec3;
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq)]
pub struct MapOverlay {
    pub source: MapSurfaceMarkSource,
    pub decal_creation_order: Option<usize>,
    pub material_name: String,
    pub render_order: u16,
    pub fade: Option<super::MapOverlayFade>,
    pub fade_origin: Vec3,
    pub receiver_mesh: super::MapReceiverMesh,
    pub receiver_range: super::MapMeshIndexRange,
    pub receiver_material_index: usize,
    pub owner: super::MapSurfaceOwner,
    pub vertices: Vec<MapSurfaceVertex>,
    pub indices: Vec<u32>,
    pub visibility: MapPropVisibility,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MapSurfaceMarkSource {
    Overlay {
        index: usize,
        id: i32,
    },
    Decal {
        entity_index: usize,
        permanence: super::MapDecalPermanence,
    },
}

pub(super) fn partitioned_map_overlays(
    bsp: &MapBsp,
    projector: &super::MapSurfaceProjector<'_>,
) -> Result<(Vec<MapOverlay>, Vec<MapOverlay>), BspError> {
    let mut visible = Vec::new();
    let mut skybox = Vec::new();
    for (overlay_index, overlay) in bsp.overlays.iter().enumerate() {
        let Some(texinfo) = usize::try_from(overlay.texinfo)
            .ok()
            .and_then(|index| bsp.texinfos.get(index))
        else {
            continue;
        };
        let Some(material_name) = normalize_material_name(bsp.texinfo_name(texinfo))
            .filter(|name| is_preview_material_visible(name))
        else {
            continue;
        };
        let Some(projection) = overlay_projection(overlay) else {
            log::debug!("bsp overlay {} skipped: invalid projection", overlay.id);
            continue;
        };
        let faces = overlay
            .faces()
            .iter()
            .filter_map(|face| u32::try_from(*face).ok())
            .collect::<BTreeSet<_>>();
        for face in faces {
            for receiver in projector.receivers_for_face(face) {
                if receiver.owner != super::MapSurfaceOwner::World {
                    continue;
                }
                let mut mapped = MapOverlay {
                    source: MapSurfaceMarkSource::Overlay {
                        index: overlay_index,
                        id: overlay.id,
                    },
                    decal_creation_order: None,
                    material_name: material_name.clone(),
                    render_order: overlay.render_order(),
                    fade: bsp
                        .overlay_fades
                        .get(overlay_index)
                        .copied()
                        .filter(|fade| fade.min_distance_squared >= 0.0),
                    fade_origin: receiver.point_to_local(Vec3::from(overlay.origin)),
                    receiver_mesh: receiver.receiver_mesh,
                    receiver_range: receiver.range,
                    receiver_material_index: receiver.mesh.material_index,
                    owner: receiver.owner,
                    vertices: Vec::new(),
                    indices: Vec::new(),
                    visibility: receiver.visibility.clone(),
                };
                if let Some(displacement) = &receiver.displacement {
                    for polygon in
                        projection.project_displacement(displacement, receiver.triangles())
                    {
                        append_surface_polygon(&mut mapped.vertices, &mut mapped.indices, polygon)?;
                    }
                } else {
                    for triangle in receiver.triangles() {
                        let polygon = projection
                            .clip_quad_to_triangle(&triangle)
                            .into_iter()
                            .map(|mut vertex| {
                                vertex.surface.position += receiver.plane_normal * 0.1;
                                vertex
                            })
                            .collect();
                        append_surface_polygon(&mut mapped.vertices, &mut mapped.indices, polygon)?;
                    }
                }
                if mapped.indices.is_empty() {
                    continue;
                }
                match receiver.partition {
                    GeometryPartition::Visible => visible.push(mapped),
                    GeometryPartition::Skybox => skybox.push(mapped),
                }
            }
        }
    }
    Ok((visible, skybox))
}

fn overlay_projection(overlay: &Overlay) -> Option<SurfaceProjection> {
    let basis = OverlayBasis::from_overlay(overlay);
    let [u_axis, v_axis] = basis.axes()?;
    let projection = SurfaceProjection {
        origin: basis.origin,
        u_axis,
        v_axis,
        corners: basis.uv_points.map(|point| [point[0], point[1]]),
        u: overlay.u,
        v: overlay.v,
    };
    projection.is_valid().then_some(projection)
}

#[derive(Clone, Copy, Debug)]
pub(super) struct OverlayBasis {
    pub(super) basis_normal: Vec3,
    pub(super) uv_points: [Vec3; 4],
    pub(super) origin: Vec3,
}

impl OverlayBasis {
    fn from_overlay(overlay: &Overlay) -> Self {
        Self {
            basis_normal: Vec3::from(overlay.basis_normal),
            uv_points: overlay.uv_points.map(Vec3::from),
            origin: Vec3::from(overlay.origin),
        }
    }

    fn axes(self) -> Option<[Vec3; 2]> {
        let normal = self.basis_normal.normalize_or_zero();
        // VBSP packs U into the first three z values; the fourth flips V.
        let u_axis = Vec3::new(
            self.uv_points[0][2],
            self.uv_points[1][2],
            self.uv_points[2][2],
        )
        .normalize_or_zero();
        if !vector_is_finite_nonzero(normal) || !vector_is_finite_nonzero(u_axis) {
            return None;
        }
        let mut v_axis = normal.cross(u_axis).normalize_or_zero();
        if self.uv_points[3][2] == 1.0 {
            v_axis *= -1.0;
        }
        vector_is_finite_nonzero(v_axis).then_some([u_axis, v_axis])
    }
}

#[cfg(test)]
pub(super) fn overlay_quad_positions(overlay: OverlayBasis) -> Option<[Vec3; 4]> {
    let [u_axis, v_axis] = overlay.axes()?;
    Some(
        overlay
            .uv_points
            .map(|point| overlay.origin + u_axis * point[0] + v_axis * point[1]),
    )
}
