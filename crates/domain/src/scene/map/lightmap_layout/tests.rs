use super::*;

fn material(name: &str, requirement: SourceLightmapRequirement) -> SourceLightmapMaterial<'_> {
    SourceLightmapMaterial {
        canonical_name: name,
        requirement,
    }
}

fn surface(material: usize, extents: [i32; 2]) -> SourceLightmapSurface {
    SourceLightmapSurface {
        material,
        extents,
        no_light: false,
        styles: [0, 255, 255, 255],
    }
}

#[test]
fn native_construct_all_face_page_oracle() {
    check_native_fixture(include_bytes!("testdata/construct.bin"));
}

#[test]
fn native_flatgrass_fresh_scene_all_face_page_oracle() {
    check_native_fixture(include_bytes!("testdata/flatgrass_fresh.bin"));
}

#[test]
fn native_flatgrass_after_construct_material_universe_page_oracle() {
    check_native_fixture(include_bytes!("testdata/flatgrass_after_construct.bin"));
}

#[test]
fn native_material_permutations_all_face_page_oracles() {
    check_native_fixture(include_bytes!("testdata/construct_plain.bin"));
    check_native_fixture(include_bytes!("testdata/receiver_unlit_positive.bin"));
    check_native_fixture(include_bytes!("testdata/receiver_bumped_positive.bin"));
}

fn check_native_fixture(bytes: &[u8]) {
    let mut input = FixtureReader(bytes);
    assert_eq!(&input.take::<4>(), b"SLL1");
    let profile = SourceLightmapPageProfile {
        width: u32::from(input.u16()),
        height: u32::from(input.u16()),
    };
    let material_count = input.u32() as usize;
    let face_count = input.u32() as usize;
    let expected_page_count = input.u32();
    let mut identities = Vec::with_capacity(material_count);
    for _ in 0..material_count {
        let requirement = match input.take::<1>()[0] {
            0 => SourceLightmapRequirement::None,
            1 => SourceLightmapRequirement::Lightmapped,
            2 => SourceLightmapRequirement::Bumped,
            _ => panic!("invalid native material requirement"),
        };
        let length = usize::from(input.u16());
        let name = std::str::from_utf8(&input.0[..length]).unwrap().to_owned();
        input.0 = &input.0[length..];
        identities.push((name, requirement));
    }
    let materials = identities
        .iter()
        .map(|(name, requirement)| material(name, *requirement))
        .collect::<Vec<_>>();
    let mut surfaces = Vec::with_capacity(face_count);
    let mut expected = Vec::with_capacity(face_count);
    for _ in 0..face_count {
        surfaces.push(SourceLightmapSurface {
            material: usize::from(input.u16()),
            extents: [i32::from(input.i16()), i32::from(input.i16())],
            no_light: input.take::<1>()[0] != 0,
            styles: input.take::<4>(),
        });
        expected.push(i32::from(input.i16()));
    }
    assert!(input.0.is_empty());
    let layout = SourceLightmapLayout::allocate(&surfaces, &materials, profile).unwrap();
    assert_eq!(layout.physical_page_count, expected_page_count);
    for (face, (actual, expected)) in layout.surfaces.iter().zip(expected).enumerate() {
        assert_eq!(
            actual.page.native_id(),
            expected,
            "face {face}, material {}, extents {:?}",
            materials[surfaces[face].material].canonical_name,
            surfaces[face].extents
        );
    }
}

struct FixtureReader<'a>(&'a [u8]);

impl FixtureReader<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let value = self.0[..N].try_into().unwrap();
        self.0 = &self.0[N..];
        value
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take())
    }
    fn i16(&mut self) -> i16 {
        i16::from_le_bytes(self.take())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }
}

#[test]
fn material_transition_reuses_only_the_last_open_page() {
    let materials = [
        material("a", SourceLightmapRequirement::Lightmapped),
        material("b", SourceLightmapRequirement::Lightmapped),
    ];
    let surfaces = [
        surface(0, [5, 5]),
        surface(0, [5, 5]),
        surface(0, [5, 5]),
        surface(1, [1, 5]),
    ];
    let layout = SourceLightmapLayout::allocate(
        &surfaces,
        &materials,
        SourceLightmapPageProfile {
            width: 8,
            height: 8,
        },
    )
    .unwrap();
    assert_eq!(layout.physical_page_count, 3);
    assert_eq!(
        layout
            .surfaces
            .iter()
            .map(|allocation| allocation.page.native_id())
            .collect::<Vec<_>>(),
        [0, 1, 2, 2]
    );
    assert_eq!(layout.surfaces[3].offset, [6, 0]);
}

#[test]
fn canonical_material_aliases_share_pages_and_receiver_group() {
    let materials = [
        material("shared", SourceLightmapRequirement::Lightmapped),
        material("other", SourceLightmapRequirement::Lightmapped),
        material("shared", SourceLightmapRequirement::Lightmapped),
    ];
    let layout = SourceLightmapLayout::allocate(
        &[surface(2, [1, 1]), surface(0, [1, 1])],
        &materials,
        SourceLightmapPageProfile::DX9,
    )
    .unwrap();
    assert!(
        layout
            .surfaces
            .iter()
            .all(|allocation| allocation.material_group == 0)
    );
    assert_eq!(layout.surfaces[0].offset, [0, 0]);
    assert_eq!(layout.surfaces[1].offset, [2, 0]);
    let conflict = [
        material("shared", SourceLightmapRequirement::Lightmapped),
        material("shared", SourceLightmapRequirement::Bumped),
    ];
    assert!(matches!(
        SourceLightmapLayout::allocate(&[], &conflict, SourceLightmapPageProfile::DX9),
        Err(SourceLightmapLayoutError::ConflictingMaterial { material: 1, .. })
    ));
}

#[test]
fn zero_extent_and_authored_nolight_use_typed_standard_pages() {
    let materials = [
        material("lit", SourceLightmapRequirement::Lightmapped),
        material("bump", SourceLightmapRequirement::Bumped),
    ];
    let mut authored = surface(1, [-1, -1]);
    authored.no_light = true;
    let layout = SourceLightmapLayout::allocate(
        &[surface(0, [0, 16]), surface(1, [24, 0]), authored],
        &materials,
        SourceLightmapPageProfile::DX9,
    )
    .unwrap();
    assert_eq!(layout.physical_page_count, 0);
    assert_eq!(
        layout
            .surfaces
            .iter()
            .map(|allocation| allocation.page)
            .collect::<Vec<_>>(),
        [
            SourceLightmapPage::White,
            SourceLightmapPage::WhiteBump,
            SourceLightmapPage::WhiteBump
        ]
    );
}

#[test]
fn native_page_height_boundary_rejects_the_reserved_rows() {
    let materials = [material("lit", SourceLightmapRequirement::Lightmapped)];
    let profile = SourceLightmapPageProfile {
        width: 8,
        height: 8,
    };
    assert!(SourceLightmapLayout::allocate(&[surface(0, [1, 5])], &materials, profile).is_ok());
    assert!(matches!(
        SourceLightmapLayout::allocate(&[surface(0, [1, 6])], &materials, profile),
        Err(SourceLightmapLayoutError::SurfaceTooLarge {
            face: 0,
            width: 2,
            height: 7
        })
    ));
}

#[test]
fn failed_rectangle_does_not_suppress_a_different_aspect_ratio() {
    let mut page = PagePacker::new(
        0,
        SourceLightmapPageProfile {
            width: 8,
            height: 8,
        },
    );
    assert_eq!(page.allocate(5, 6), Some([0, 0]));
    assert_eq!(page.allocate(5, 1), None);
    assert_eq!(page.allocate(3, 6), Some([5, 0]));
}
