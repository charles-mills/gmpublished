use std::cmp::Reverse;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceLightmapRequirement {
    None,
    Lightmapped,
    Bumped,
}

#[derive(Clone, Copy, Debug)]
pub struct SourceLightmapMaterial<'a> {
    pub canonical_name: &'a str,
    pub requirement: SourceLightmapRequirement,
}

#[derive(Clone, Copy, Debug)]
pub struct SourceLightmapSurface {
    pub material: usize,
    pub extents: [i32; 2],
    pub no_light: bool,
    pub styles: [u8; 4],
}

impl SourceLightmapSurface {
    fn uses_white_page(self) -> bool {
        self.no_light || self.extents.contains(&0)
    }

    fn has_lightstyles(self) -> bool {
        self.styles.iter().any(|&style| style != 0 && style != 255)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SourceLightmapPage {
    Physical(u32),
    White,
    WhiteBump,
}

impl SourceLightmapPage {
    pub const fn native_id(self) -> i32 {
        match self {
            Self::Physical(index) => index as i32,
            Self::White => -1,
            Self::WhiteBump => -2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceLightmapAllocation {
    pub material_group: usize,
    pub page: SourceLightmapPage,
    pub offset: [u32; 2],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLightmapLayout {
    pub profile: SourceLightmapPageProfile,
    pub surfaces: Vec<SourceLightmapAllocation>,
    pub physical_page_count: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceLightmapPageProfile {
    pub width: u32,
    pub height: u32,
}

impl SourceLightmapPageProfile {
    pub const DX9: Self = Self {
        width: 512,
        height: 256,
    };
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SourceLightmapLayoutError {
    #[error("invalid Source lightmap page dimensions {width}x{height}")]
    InvalidProfile { width: u32, height: u32 },
    #[error("material {material} has an empty canonical identity")]
    EmptyMaterialIdentity { material: usize },
    #[error("material {material} conflicts with the earlier contract for {name}")]
    ConflictingMaterial { material: usize, name: String },
    #[error("face {face} references unavailable material {material}")]
    InvalidMaterial { face: usize, material: usize },
    #[error("face {face} has invalid lightmap extents {extents:?}")]
    InvalidExtents { face: usize, extents: [i32; 2] },
    #[error("face {face} lightmap {width}x{height} does not fit the Source page profile")]
    SurfaceTooLarge {
        face: usize,
        width: u32,
        height: u32,
    },
    #[error("Source lightmap page count exceeds the signed page identity range")]
    TooManyPages,
}

impl SourceLightmapLayout {
    /// Materials define first-touch order; surfaces follow BSP face order.
    pub fn allocate(
        surfaces: &[SourceLightmapSurface],
        materials: &[SourceLightmapMaterial<'_>],
        profile: SourceLightmapPageProfile,
    ) -> Result<Self, SourceLightmapLayoutError> {
        if profile.width == 0
            || profile.width > 2048
            || profile.height < 3
            || profile.height > 32767
        {
            return Err(SourceLightmapLayoutError::InvalidProfile {
                width: profile.width,
                height: profile.height,
            });
        }
        let mut identities = HashMap::new();
        let mut material_groups = Vec::with_capacity(materials.len());
        for (material, input) in materials.iter().enumerate() {
            if input.canonical_name.is_empty() {
                return Err(SourceLightmapLayoutError::EmptyMaterialIdentity { material });
            }
            let (group, requirement) = *identities
                .entry(input.canonical_name)
                .or_insert((material, input.requirement));
            if requirement != input.requirement {
                return Err(SourceLightmapLayoutError::ConflictingMaterial {
                    material,
                    name: input.canonical_name.to_owned(),
                });
            }
            material_groups.push(group);
        }
        for (face, surface) in surfaces.iter().enumerate() {
            if surface.material >= materials.len() {
                return Err(SourceLightmapLayoutError::InvalidMaterial {
                    face,
                    material: surface.material,
                });
            }
            if !surface.uses_white_page() && surface.extents.iter().any(|&extent| extent < 0) {
                return Err(SourceLightmapLayoutError::InvalidExtents {
                    face,
                    extents: surface.extents,
                });
            }
        }
        let mut order = (0..surfaces.len()).collect::<Vec<_>>();
        order.sort_by_key(|&face| {
            let surface = surfaces[face];
            (
                surface.uses_white_page(),
                material_groups[surface.material],
                surface.has_lightstyles(),
                Reverse(i64::from(surface.extents[0]) * i64::from(surface.extents[1])),
                face,
            )
        });
        let mut allocations = surfaces
            .iter()
            .map(|surface| SourceLightmapAllocation {
                material_group: material_groups[surface.material],
                page: SourceLightmapPage::White,
                offset: [0; 2],
            })
            .collect::<Vec<_>>();
        let mut active_pages = Vec::<PagePacker>::new();
        let mut previous_material = None;
        let mut physical_page_count = 0_u32;
        for face in order {
            let surface = surfaces[face];
            let requirement = materials[surface.material].requirement;
            if surface.uses_white_page() {
                allocations[face].page = if requirement == SourceLightmapRequirement::Bumped {
                    SourceLightmapPage::WhiteBump
                } else {
                    SourceLightmapPage::White
                };
                continue;
            }
            let material = material_groups[surface.material];
            if previous_material != Some(material) {
                let last_page = active_pages.pop();
                active_pages.clear();
                active_pages.extend(last_page);
                previous_material = Some(material);
            }
            let width = (surface.extents[0] as u32 + 1)
                .checked_mul(if requirement == SourceLightmapRequirement::Bumped {
                    4
                } else {
                    1
                })
                .ok_or(SourceLightmapLayoutError::InvalidExtents {
                    face,
                    extents: surface.extents,
                })?;
            let height = surface.extents[1] as u32 + 1;
            let existing = active_pages.iter_mut().find_map(|page| {
                page.allocate(width, height)
                    .map(|offset| SourceLightmapAllocation {
                        material_group: material,
                        page: SourceLightmapPage::Physical(page.page),
                        offset,
                    })
            });
            allocations[face] = if let Some(allocation) = existing {
                allocation
            } else {
                if physical_page_count == i32::MAX as u32 {
                    return Err(SourceLightmapLayoutError::TooManyPages);
                }
                let mut page = PagePacker::new(physical_page_count, profile);
                let offset = page.allocate(width, height).ok_or(
                    SourceLightmapLayoutError::SurfaceTooLarge {
                        face,
                        width,
                        height,
                    },
                )?;
                physical_page_count += 1;
                let allocation = SourceLightmapAllocation {
                    material_group: material,
                    page: SourceLightmapPage::Physical(page.page),
                    offset,
                };
                active_pages.push(page);
                allocation
            };
        }
        Ok(Self {
            profile,
            surfaces: allocations,
            physical_page_count,
        })
    }
}

struct PagePacker {
    page: u32,
    profile: SourceLightmapPageProfile,
    wavefront: Vec<i32>,
    largest_failed: [u32; 2],
}

impl PagePacker {
    fn new(page: u32, profile: SourceLightmapPageProfile) -> Self {
        Self {
            page,
            profile,
            wavefront: vec![-1; profile.width as usize],
            largest_failed: [profile.width + 1, profile.height + 1],
        }
    }

    fn allocate(&mut self, width: u32, height: u32) -> Option<[u32; 2]> {
        if width >= self.largest_failed[0] && height >= self.largest_failed[1] {
            return None;
        }
        let mut best = None;
        let mut best_y = self.profile.height as i32;
        let mut first_x = 0;
        let mut last_y = -2;
        if width <= self.profile.width {
            while first_x <= self.profile.width - width {
                if self.wavefront[first_x as usize] == last_y {
                    first_x += 1;
                    continue;
                }
                let (maximum_x, &maximum_y) = self.wavefront
                    [first_x as usize..(first_x + width) as usize]
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, y)| *y)?;
                last_y = maximum_y;
                if maximum_y < best_y {
                    best_y = maximum_y;
                    best = Some(first_x);
                }
                first_x += maximum_x as u32 + 1;
            }
        }
        let y = (best_y + 1) as u32;
        if best.is_none() || u64::from(y) + u64::from(height) >= u64::from(self.profile.height - 1)
        {
            if width <= self.largest_failed[0] && height <= self.largest_failed[1] {
                self.largest_failed = [width, height];
            }
            return None;
        }
        let x = best?;
        self.wavefront[x as usize..(x + width) as usize].fill(best_y + height as i32);
        Some([x, y])
    }
}

#[cfg(test)]
mod tests;
