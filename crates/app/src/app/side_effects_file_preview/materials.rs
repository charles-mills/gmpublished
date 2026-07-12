use super::map_preview::{PropMaterialState, parallel_collect};
use super::{
    Arc, DetailSprite, HashMap, HashSet, MAP_DETAIL_SPRITE_PLACEMENT_CAP, MapDetailSprite,
    MapOverlay, MaterialResolver, MaterialSlot, OverlayPrimitive, OverlayVertex, RenderMode,
    ResolvedMaterialTextures, SKYBOX_FACE_DIMENSION_CAP, Skybox, SkyboxFace,
};

#[derive(Debug)]
pub(super) struct MapMaterialResolution {
    pub(super) materials: Vec<MaterialSlot>,
    pub(super) material_indexes: HashMap<String, usize>,
    pub(super) resolved_material_count: u32,
    pub(super) water_fallback_material_count: u32,
}

pub(super) fn resolve_map_material_slots_parallel(
    material_names: &[String],
    resolver: &MaterialResolver,
) -> MapMaterialResolution {
    let resolved = parallel_collect(material_names, |_, name| {
        resolver.resolve_with_base2(&[], name)
    });
    map_material_resolution_from_results(material_names, resolved)
}

#[cfg(test)]
pub(super) fn resolve_map_material_slots_serial(
    material_names: &[String],
    resolver: &MaterialResolver,
) -> MapMaterialResolution {
    let resolved = material_names
        .iter()
        .map(|name| resolver.resolve_with_base2(&[], name))
        .collect::<Vec<_>>();
    map_material_resolution_from_results(material_names, resolved)
}

pub(super) fn map_material_resolution_from_results(
    material_names: &[String],
    resolved_materials: Vec<Option<ResolvedMaterialTextures>>,
) -> MapMaterialResolution {
    let mut resolved_material_count = 0_u32;
    let mut water_fallback_material_count = 0_u32;
    let mut material_indexes = HashMap::<String, usize>::new();
    let mut materials = Vec::with_capacity(material_names.len());
    for (name, resolved) in material_names.iter().zip(resolved_materials) {
        let texture = resolved
            .as_ref()
            .and_then(|material| material.texture.as_ref().map(Arc::clone));
        let texture2 = resolved
            .as_ref()
            .and_then(|material| material.texture2.as_ref().map(Arc::clone));
        let force_opaque = resolved
            .as_ref()
            .is_none_or(|material| material.force_opaque);
        let render_mode = resolved
            .as_ref()
            .map_or(RenderMode::Opaque, |material| material.render_mode);
        if texture.is_some() {
            resolved_material_count = resolved_material_count.saturating_add(1);
        }
        if texture
            .as_ref()
            .is_some_and(|texture| texture.is_water_fallback())
        {
            water_fallback_material_count = water_fallback_material_count.saturating_add(1);
        }
        material_indexes.insert(name.clone(), materials.len());
        materials.push(MaterialSlot {
            name: name.clone(),
            texture,
            texture2,
            force_opaque,
            render_mode,
        });
    }

    MapMaterialResolution {
        materials,
        material_indexes,
        resolved_material_count,
        water_fallback_material_count,
    }
}

#[derive(Debug)]
pub(super) struct OverlayBakeResult {
    pub(super) overlays: Vec<OverlayPrimitive>,
    pub(super) skipped_count: u32,
}

pub(super) fn resolve_detail_sprites(
    sprites: &[MapDetailSprite],
    map_skybox_sprites: &[MapDetailSprite],
    detail_material_name: &str,
    resolver: &MaterialResolver,
    mut material_state: PropMaterialState<'_>,
) -> (Vec<DetailSprite>, Vec<DetailSprite>) {
    if sprites.is_empty() && map_skybox_sprites.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let visible_count = sprites.len().min(MAP_DETAIL_SPRITE_PLACEMENT_CAP);
    let skybox_count = map_skybox_sprites
        .len()
        .min(MAP_DETAIL_SPRITE_PLACEMENT_CAP.saturating_sub(visible_count));
    let dropped_count = sprites
        .len()
        .saturating_add(map_skybox_sprites.len())
        .saturating_sub(visible_count.saturating_add(skybox_count));
    if dropped_count > 0 {
        log::debug!(
            "map preview detail sprite placement cap {MAP_DETAIL_SPRITE_PLACEMENT_CAP}: dropped {dropped_count}"
        );
    }
    let resolved = resolver.resolve_with_base2(&[], detail_material_name);
    let material_index = push_map_material_slot(
        detail_material_name,
        resolved.as_ref(),
        false,
        RenderMode::Cutout,
        &mut material_state,
    );

    let visible = sprites
        .iter()
        .take(visible_count)
        .map(|sprite| map_detail_sprite_to_preview(sprite, material_index))
        .collect();
    let skybox = map_skybox_sprites
        .iter()
        .take(skybox_count)
        .map(|sprite| map_detail_sprite_to_preview(sprite, material_index))
        .collect();
    (visible, skybox)
}

pub(super) fn map_detail_sprite_to_preview(
    sprite: &MapDetailSprite,
    material_index: usize,
) -> DetailSprite {
    DetailSprite {
        origin: sprite.origin,
        upper_left: sprite.upper_left,
        lower_right: sprite.lower_right,
        tex_upper_left: sprite.tex_upper_left,
        tex_lower_right: sprite.tex_lower_right,
        material_index,
        visibility: sprite.visibility,
    }
}

pub(super) fn resolve_map_overlays(
    overlays: &[MapOverlay],
    resolver: &MaterialResolver,
    mut material_state: PropMaterialState<'_>,
) -> OverlayBakeResult {
    let pre_resolved = pre_resolve_overlay_materials(overlays, &material_state, resolver);
    let mut resolved_overlays = Vec::new();
    let mut skipped_count = 0_u32;
    for overlay in overlays {
        let Some(material_index) =
            overlay_material_index(overlay, resolver, &pre_resolved, &mut material_state)
        else {
            skipped_count = skipped_count.saturating_add(1);
            continue;
        };
        resolved_overlays.push(overlay_primitive(overlay, material_index));
    }
    if skipped_count > 0 {
        log::debug!("map preview overlays skipped {skipped_count} missing/unresolved materials");
    }

    OverlayBakeResult {
        overlays: resolved_overlays,
        skipped_count,
    }
}

/// Resolve unique overlay materials across worker threads before the serial
/// index-assignment pass — the same shape as the prop pre-resolve; slot order
/// stays first-encounter and counters only move in the serial pass.
pub(super) fn pre_resolve_overlay_materials(
    overlays: &[MapOverlay],
    material_state: &PropMaterialState<'_>,
    resolver: &MaterialResolver,
) -> HashMap<String, Option<ResolvedMaterialTextures>> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for overlay in overlays {
        if material_state
            .material_indexes
            .contains_key(&overlay.material_name)
        {
            continue;
        }
        if seen.insert(overlay.material_name.clone()) {
            names.push(overlay.material_name.clone());
        }
    }
    parallel_collect(&names, |_, name| {
        (name.clone(), resolver.resolve_with_base2(&[], name))
    })
    .into_iter()
    .collect()
}

pub(super) fn overlay_material_index(
    overlay: &MapOverlay,
    resolver: &MaterialResolver,
    pre_resolved: &HashMap<String, Option<ResolvedMaterialTextures>>,
    material_state: &mut PropMaterialState<'_>,
) -> Option<usize> {
    if let Some(index) = material_state
        .material_indexes
        .get(&overlay.material_name)
        .copied()
    {
        if material_state
            .materials
            .get(index)
            .is_some_and(|slot| slot.texture.is_some())
        {
            return Some(index);
        }
        log::debug!(
            "map preview overlay {} skipped: material {} unresolved",
            overlay.id,
            overlay.material_name
        );
        return None;
    }

    let resolved = pre_resolved.get(&overlay.material_name).map_or_else(
        || resolver.resolve_with_base2(&[], &overlay.material_name),
        Clone::clone,
    );
    let Some(resolved) = resolved else {
        log::debug!(
            "map preview overlay {} skipped: material {} missing",
            overlay.id,
            overlay.material_name
        );
        return None;
    };
    if resolved.texture.is_none() {
        log::debug!(
            "map preview overlay {} skipped: material {} has no base texture",
            overlay.id,
            overlay.material_name
        );
        return None;
    }
    let index = push_map_material_slot(
        &overlay.material_name,
        Some(&resolved),
        resolved.force_opaque,
        resolved.render_mode,
        material_state,
    );
    material_state
        .material_indexes
        .insert(overlay.material_name.clone(), index);
    Some(index)
}

pub(super) fn push_map_material_slot(
    name: &str,
    resolved: Option<&ResolvedMaterialTextures>,
    force_opaque: bool,
    render_mode: RenderMode,
    material_state: &mut PropMaterialState<'_>,
) -> usize {
    let texture = resolved.and_then(|material| material.texture.as_ref().map(Arc::clone));
    let texture2 = resolved.and_then(|material| material.texture2.as_ref().map(Arc::clone));
    if texture.is_some() {
        *material_state.resolved_material_count =
            material_state.resolved_material_count.saturating_add(1);
    }
    if texture
        .as_ref()
        .is_some_and(|texture| texture.is_water_fallback())
    {
        *material_state.water_fallback_material_count = material_state
            .water_fallback_material_count
            .saturating_add(1);
    }
    let index = material_state.materials.len();
    material_state.materials.push(MaterialSlot {
        name: name.to_owned(),
        texture,
        texture2,
        force_opaque,
        render_mode,
    });
    index
}

pub(super) fn overlay_primitive(overlay: &MapOverlay, material_index: usize) -> OverlayPrimitive {
    let u = overlay.u;
    let v = overlay.v;
    // Overlay corners are authored V-first (Hammer uv0..uv3 = bottom-left,
    // top-left, top-right, bottom-right), so texcoords walk V before U.
    // U-first renders every decal transposed (rotated 90° + mirrored).
    let uvs = [[u[0], v[0]], [u[0], v[1]], [u[1], v[1]], [u[1], v[0]]];
    OverlayPrimitive {
        vertices: std::array::from_fn(|index| OverlayVertex {
            position: overlay.positions[index],
            normal: overlay.normal,
            uv: uvs[index],
        }),
        material_index,
        visibility: overlay.visibility,
    }
}

pub(super) fn resolve_skybox(skyname: &str, resolver: &MaterialResolver) -> Option<Skybox> {
    let mut faces = std::array::from_fn(|_| None);
    for face in SkyboxFace::ALL {
        let path = skybox_face_material_path(skyname, face);
        let Some(texture) = resolver.resolve_base_texture_at_path(&path) else {
            log::debug!(
                "map skybox {skyname}: missing LDR base texture for {}",
                face.suffix()
            );
            continue;
        };
        if texture.width > SKYBOX_FACE_DIMENSION_CAP || texture.height > SKYBOX_FACE_DIMENSION_CAP {
            log::debug!(
                "map skybox {skyname}: dropped {} face {}x{} over {} cap",
                face.suffix(),
                texture.width,
                texture.height,
                SKYBOX_FACE_DIMENSION_CAP
            );
            continue;
        }
        faces[face.index()] = Some(Arc::new(texture.without_mip_chain()));
    }

    if faces.iter().any(Option::is_some) {
        Some(Skybox { faces })
    } else {
        None
    }
}

pub(super) fn skybox_face_material_path(skyname: &str, face: SkyboxFace) -> String {
    format!("materials/skybox/{skyname}{}.vmt", face.suffix())
}

pub(super) fn sky_log_status(skyname: Option<&str>) -> String {
    skyname.map_or_else(|| "sky none".to_owned(), |skyname| format!("sky {skyname}"))
}
