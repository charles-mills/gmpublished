use super::*;

fn surface(bumped: bool) -> MapRadiositySurfaces {
    let samples = (0..if bumped { 8 } else { 2 })
        .map(|index| ColorRgbExp {
            r: if index == 0 { 255 } else { 0 },
            g: if index == if bumped { 4 } else { 1 } {
                255
            } else {
                0
            },
            b: 0,
            exponent: 0,
        })
        .collect();
    MapRadiositySurfaces {
        source: LightmapSource::Ldr,
        world_head_node: -1,
        world_face_range: 0..1,
        planes: Vec::new(),
        nodes: Vec::new(),
        leaves: Vec::new(),
        leaf_faces: Vec::new(),
        faces: vec![Face {
            plane: 0,
            side: 0,
            on_node: 1,
            first_edge: 0,
            edge_count: 3,
            texinfo: 0,
            displacement: -1,
            styles: [0, 32, 255, 255],
            light_offset: 0,
            area: 1.0,
            lightmap_mins: [0; 2],
            lightmap_size: [0; 2],
            original_face: -1,
        }],
        face_vertices: vec![Vec::new()],
        texinfos: vec![TexInfo {
            texture_vecs: [[0.0; 4]; 2],
            lightmap_vecs: [[0.0; 4]; 2],
            flags: if bumped { 0x0800 } else { 0 },
            texdata: 0,
        }],
        texdatas: Vec::new(),
        texture_names: Vec::new(),
        samples,
        displacements: Vec::new(),
        triangles: Vec::new(),
        triangle_tree: BoundsTree::new(std::iter::empty()),
    }
}

#[test]
fn first_style_sampling_does_not_accumulate_switchable_layers() {
    for bumped in [false, true] {
        let surface = surface(bumped);
        let mut styles = [1.0; 128];
        styles[32] = 0.5;
        assert_eq!(
            surface.sample_luxel(0, [0; 2], &styles, MapRadiosityStyles::First),
            Some(Vec3::new(1.0, 0.0, 0.0))
        );
        assert_eq!(
            surface.sample_luxel(0, [0; 2], &styles, MapRadiosityStyles::All),
            Some(Vec3::new(1.0, 0.5, 0.0))
        );
        styles[32] = f32::NAN;
        assert!(
            surface
                .sample_luxel(0, [0; 2], &styles, MapRadiosityStyles::First)
                .is_some()
        );
        assert!(
            surface
                .sample_luxel(0, [0; 2], &styles, MapRadiosityStyles::All)
                .is_none()
        );
    }
}

#[test]
fn surface_queries_reject_invalid_luxels_and_payload_offsets() {
    let mut surface = surface(false);
    assert!(
        surface
            .sample_luxel(0, [1, 0], &[1.0; 128], MapRadiosityStyles::First)
            .is_none()
    );
    for offset in [-1, 1, 8] {
        surface.faces[0].light_offset = offset;
        assert!(
            surface
                .sample_luxel(0, [0; 2], &[1.0; 128], MapRadiosityStyles::First)
                .is_none()
        );
    }
}

#[test]
fn both_zero_extents_use_projected_vertices_but_single_zero_extent_keeps_the_rectangle() {
    let mut surfaces = surface(false);
    surfaces.texinfos[0].lightmap_vecs = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]];
    surfaces.face_vertices[0] = vec![
        Vec3::ZERO,
        Vec3::new(16.0, 0.0, 0.0),
        Vec3::new(0.0, 16.0, 0.0),
    ];
    let point = Vec3::new(8.0, 8.0, 0.0);
    assert_eq!(
        surfaces
            .face_hit(0, point, 0.5)
            .unwrap()
            .unwrap()
            .lightmap_coords,
        [8.0, 8.0]
    );
    assert!(
        surfaces
            .face_hit(0, Vec3::new(17.0, 8.0, 0.0), 0.5)
            .unwrap()
            .is_none()
    );
    assert!(
        surfaces
            .face_hit(0, Vec3::new(-1.0, 8.0, 0.0), 0.5)
            .unwrap()
            .is_none()
    );
    surfaces.faces[0].lightmap_size = [0, 16];
    assert!(surfaces.face_hit(0, point, 0.5).unwrap().is_none());
    surfaces.faces[0].lightmap_size = [16, 0];
    assert!(surfaces.face_hit(0, point, 0.5).unwrap().is_none());
}

#[test]
fn authored_face_averages_use_reverse_style_prefix_not_luxel_mean() {
    let mut surface = surface(true);
    surface.faces[0].light_offset = 8;
    surface.samples.splice(
        0..0,
        [
            ColorRgbExp {
                r: 0,
                g: 128,
                b: 0,
                exponent: 0,
            },
            ColorRgbExp {
                r: 64,
                g: 0,
                b: 0,
                exponent: 0,
            },
        ],
    );
    let mut styles = [1.0; 128];
    styles[32] = 0.25;
    assert_eq!(
        surface.sample_face_average(0, &styles, MapRadiosityStyles::First),
        Some(Vec3::new(64.0 / 255.0, 0.0, 0.0))
    );
    assert_eq!(
        surface.sample_face_average(0, &styles, MapRadiosityStyles::All),
        Some(Vec3::new(64.0 / 255.0, 32.0 / 255.0, 0.0))
    );
    assert_eq!(
        surface.sample_luxel(0, [0; 2], &styles, MapRadiosityStyles::First),
        Some(Vec3::new(1.0, 0.0, 0.0))
    );
    surface.faces[0].light_offset = 0;
    assert!(
        surface
            .sample_face_average(0, &styles, MapRadiosityStyles::First)
            .is_none()
    );
}

fn leaf_with_two_faces() -> Leaf {
    let mut bytes = vec![0_u8; 1036 + 32];
    bytes[..4].copy_from_slice(b"VBSP");
    bytes[4..8].copy_from_slice(&20_i32.to_le_bytes());
    let directory = 8 + 10 * 16;
    bytes[directory..directory + 4].copy_from_slice(&1036_i32.to_le_bytes());
    bytes[directory + 4..directory + 8].copy_from_slice(&32_i32.to_le_bytes());
    bytes[directory + 8..directory + 12].copy_from_slice(&1_i32.to_le_bytes());
    bytes[1036 + 22..1036 + 24].copy_from_slice(&2_u16.to_le_bytes());
    vformats::bsp::parse(&bytes, &vformats::Limits::default())
        .unwrap()
        .leafs(&vformats::Limits::default())
        .unwrap()
        .remove(0)
}

#[test]
fn missed_leaf_face_does_not_erase_an_earlier_valid_surface_hit() {
    for (side, reverse_plane) in [(0, false), (1, false), (1, true)] {
        let mut surface = surface(false);
        surface.world_face_range = 0..2;
        surface.leaves = vec![leaf_with_two_faces()];
        surface.leaf_faces = vec![0, 1];
        let mut first = surface.faces[0];
        first.on_node = 0;
        first.side = side;
        first.lightmap_size = [1; 2];
        let mut second = first;
        second.plane = 1;
        second.side = 0;
        second.lightmap_mins = [10; 2];
        surface.faces = vec![first, second];
        surface.planes = vec![
            Plane {
                normal: [if reverse_plane { -1.0 } else { 1.0 }, 0.0, 0.0],
                dist: if reverse_plane { -8.0 } else { 8.0 },
                axis_type: 0,
            },
            Plane {
                normal: [1.0, 0.0, 0.0],
                dist: 12.0,
                axis_type: 0,
            },
        ];
        surface.texinfos[0].lightmap_vecs = [[0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        surface.texdatas.push(TexData {
            reflectivity: [0.2; 3],
            name_index: 0,
            width: 1,
            height: 1,
        });
        let hit = surface
            .trace_surface(
                Vec3::new(20.0, 0.0, 0.0),
                Vec3::ZERO,
                &[MapRadiosityMaterialFlags::default()],
            )
            .unwrap();
        if reverse_plane {
            assert!(hit.is_none());
            continue;
        }
        let hit = hit.unwrap();
        assert_eq!(hit.face_index, 0);
        assert_eq!(hit.fraction, 0.6);
    }
}
