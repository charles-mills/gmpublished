use super::map_preview::{MaterialTable, parallel_collect};
use super::{
    Arc, DetailSprite, HashMap, HashSet, MAP_DETAIL_SPRITE_PLACEMENT_CAP, MapDetailSprite,
    MapOverlay, MaterialResolution, MaterialResolver, MaterialSlot, OverlayPrimitive, RenderMode,
    ResolvedMaterialTextures, Skybox, SkyboxFace,
};
use gmpublished_domain::scene::map::{
    MapDecal, MapReceiverMesh, MapSurfaceMarkSource, MapSurfaceProjector,
};

pub(super) fn resolve_texture_jobs<T: Sync + Send, R: Send>(
    jobs: &[T],
    resolver: &MaterialResolver,
    resolve: impl Fn(&T, &MaterialResolver) -> R + Sync,
) -> Vec<R> {
    if resolver.texture_loading() == crate::bridge::materials::TextureLoading::OnDemand {
        return parallel_collect(jobs, |job| {
            resolver
                .check_cancelled()
                .is_ok()
                .then(|| resolve(job, resolver))
        })
        .into_iter()
        .flatten()
        .collect();
    }
    let mut requests = crate::bridge::materials::TextureRequestPlan::default();
    for job in jobs {
        if resolver.check_cancelled().is_err() {
            return Vec::new();
        }
        let (_, plan) = resolver.plan_texture_requests(|planning| resolve(job, planning));
        requests.append(plan);
    }
    if resolver.check_cancelled().is_err() {
        return Vec::new();
    }
    let cost = requests.cost();
    let admissions = resolver.admit_texture_requests(requests);
    debug_assert_eq!(cost, admissions.cost());
    log::debug!("texture request plan: {cost:?}");
    let admitted = resolver.with_texture_admissions(admissions);
    parallel_collect(jobs, |job| {
        if admitted.check_cancelled().is_err() {
            None
        } else {
            Some(resolve(job, &admitted))
        }
    })
    .into_iter()
    .flatten()
    .collect()
}

pub(super) fn resolve_map_material_slots_parallel(
    material_names: &[String],
    resolver: &MaterialResolver,
) -> MaterialTable {
    let resolved = resolve_texture_jobs(material_names, resolver, |name, resolver| {
        resolver.resolve_with_base2(&[], name)
    });
    map_material_resolution_from_results(material_names, resolved)
}

#[cfg(test)]
pub(super) fn resolve_map_material_slots_serial(
    material_names: &[String],
    resolver: &MaterialResolver,
) -> MaterialTable {
    let resolved = material_names
        .iter()
        .map(|name| resolver.resolve_with_base2(&[], name))
        .collect::<Vec<_>>();
    map_material_resolution_from_results(material_names, resolved)
}

pub(super) fn map_material_resolution_from_results(
    material_names: &[String],
    resolved_materials: Vec<MaterialResolution<ResolvedMaterialTextures>>,
) -> MaterialTable {
    let mut table = MaterialTable::default();
    for (name, resolved) in material_names.iter().zip(resolved_materials) {
        let texture = resolved
            .material()
            .and_then(|material| material.texture.as_ref().map(Arc::clone));
        let texture2 = resolved
            .material()
            .and_then(|material| material.texture2.as_ref().map(Arc::clone));
        let render_mode = resolved
            .material()
            .map_or(RenderMode::Opaque, |material| material.render_mode);
        table.push(
            name,
            MaterialSlot {
                tint_base: None,
                resolution: resolved.outcome(),
                canonical_name: resolved.canonical_name().map(str::to_owned),
                shader: resolved
                    .material()
                    .map_or_else(Default::default, |material| material.shader.clone()),
                name: name.clone(),
                texture,
                texture2,
                render_mode,
                alpha_policy: render_mode.texture_alpha_policy(),
            },
        );
    }

    table
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
    table: &mut MaterialTable,
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
    let material_index =
        push_map_material_slot(detail_material_name, &resolved, RenderMode::Cutout, table);

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
        fast_eligible: sprite.fast_eligible,
        leaf_index: sprite.leaf_index,
        origin: sprite.origin,
        angles: sprite.angles,
        orientation: sprite.orientation,
        underwater_leaf: sprite.underwater_leaf,
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
    table: &mut MaterialTable,
) -> OverlayBakeResult {
    let pre_resolved = pre_resolve_overlay_materials(overlays, table, resolver);
    let mut resolved_overlays = Vec::new();
    let mut skipped = HashSet::new();
    for overlay in overlays {
        let Some(material_index) = overlay_material_index(overlay, resolver, &pre_resolved, table)
        else {
            skipped.insert(overlay.source);
            continue;
        };
        resolved_overlays.push(overlay_primitive(overlay, material_index));
    }
    let skipped_count = u32::try_from(skipped.len()).unwrap_or(u32::MAX);
    if skipped_count > 0 {
        log::debug!("map preview overlays skipped {skipped_count} missing/unresolved materials");
    }

    OverlayBakeResult {
        overlays: resolved_overlays,
        skipped_count,
    }
}

pub(super) fn resolve_map_decals(
    decals: &[MapDecal],
    projector: &MapSurfaceProjector<'_>,
    receiver_allowed: &[bool],
    resolver: &MaterialResolver,
    table: &mut MaterialTable,
    lightmaps: Option<&gmpublished_domain::scene::map::SourceLightmapLayout>,
) -> (OverlayBakeResult, OverlayBakeResult) {
    let mut registry = super::map_preview::StaticDecalRegistry::default();
    let mut material_groups = HashMap::new();
    let pre_resolved = pre_resolve_surface_mark_materials(
        decals.iter().map(|decal| decal.material_name.as_str()),
        table,
        resolver,
    );
    let mut world = OverlayBakeResult {
        overlays: Vec::new(),
        skipped_count: 0,
    };
    let mut sky = OverlayBakeResult {
        overlays: Vec::new(),
        skipped_count: 0,
    };
    for decal in decals {
        let source = MapSurfaceMarkSource::Decal {
            entity_index: decal.id,
            permanence: decal.permanence,
        };
        let Some(material_index) = surface_mark_material_index(
            source,
            &decal.material_name,
            resolver,
            &pre_resolved,
            table,
        ) else {
            world.skipped_count = world.skipped_count.saturating_add(1);
            continue;
        };
        let Some(slot) = table.get(material_index) else {
            continue;
        };
        let Some(surface) = slot.shader.surface() else {
            log::debug!(
                "map preview surface mark {source:?} skipped: unsupported decal vertex format"
            );
            world.skipped_count = world.skipped_count.saturating_add(1);
            continue;
        };
        let next_group = material_groups.len();
        let material_group = *material_groups
            .entry(
                slot.canonical_name
                    .as_deref()
                    .unwrap_or(&slot.name)
                    .to_owned(),
            )
            .or_insert(next_group);
        let Some(size) = slot.shader.surface().and_then(|surface| {
            surface
                .decal_parameters
                .projection_dimensions(slot.shader.mapping_dimensions(slot.texture.as_deref())?)
        }) else {
            log::debug!("map preview surface mark {source:?} skipped: invalid mapping dimensions");
            world.skipped_count = world.skipped_count.saturating_add(1);
            continue;
        };
        let fragments = match projector.project_decal(decal, size, receiver_allowed) {
            Ok(fragments) => fragments,
            Err(error) => {
                log::debug!("map preview surface mark {source:?} projection failed: {error}");
                world.skipped_count = world.skipped_count.saturating_add(1);
                continue;
            }
        };
        for fragment in fragments {
            let mut primitive = overlay_primitive(&fragment, material_index);
            let page = lightmaps
                .and_then(|layout| layout.surfaces.get(fragment.receiver_range.face as usize))
                .map_or(
                    gmpublished_domain::scene::map::SourceLightmapPage::White,
                    |allocation| allocation.page,
                );
            primitive.decal_sort =
                Some(registry.register(material_group, surface, page, decal.permanence));
            if matches!(fragment.receiver_mesh, MapReceiverMesh::Skybox(_)) {
                sky.overlays.push(primitive);
            } else {
                world.overlays.push(primitive);
            }
        }
    }
    (world, sky)
}

pub(super) fn authored_surface_mark_count(overlays: &[OverlayPrimitive]) -> u32 {
    u32::try_from(
        overlays
            .iter()
            .map(|overlay| overlay.source)
            .collect::<HashSet<_>>()
            .len(),
    )
    .unwrap_or(u32::MAX)
}

pub(super) fn pre_resolve_overlay_materials(
    overlays: &[MapOverlay],
    table: &MaterialTable,
    resolver: &MaterialResolver,
) -> HashMap<String, MaterialResolution<ResolvedMaterialTextures>> {
    pre_resolve_surface_mark_materials(
        overlays
            .iter()
            .map(|overlay| overlay.material_name.as_str()),
        table,
        resolver,
    )
}

fn pre_resolve_surface_mark_materials<'a>(
    material_names: impl Iterator<Item = &'a str>,
    table: &MaterialTable,
    resolver: &MaterialResolver,
) -> HashMap<String, MaterialResolution<ResolvedMaterialTextures>> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for name in material_names {
        if !table.contains(name) && seen.insert(name) {
            names.push(name.to_owned());
        }
    }
    resolve_texture_jobs(&names, resolver, |name, resolver| {
        (name.clone(), resolver.resolve_with_base2(&[], name))
    })
    .into_iter()
    .collect()
}

pub(super) fn overlay_material_index(
    overlay: &MapOverlay,
    resolver: &MaterialResolver,
    pre_resolved: &HashMap<String, MaterialResolution<ResolvedMaterialTextures>>,
    table: &mut MaterialTable,
) -> Option<usize> {
    surface_mark_material_index(
        overlay.source,
        &overlay.material_name,
        resolver,
        pre_resolved,
        table,
    )
}

fn surface_mark_material_index(
    source: MapSurfaceMarkSource,
    material_name: &str,
    resolver: &MaterialResolver,
    pre_resolved: &HashMap<String, MaterialResolution<ResolvedMaterialTextures>>,
    table: &mut MaterialTable,
) -> Option<usize> {
    if let Some(index) = table.index_of(material_name) {
        if table.get(index).is_some_and(|slot| slot.texture.is_some()) {
            return Some(index);
        }
        log::debug!(
            "map preview surface mark {source:?} skipped: material {material_name} unresolved"
        );
        return None;
    }

    let resolved = pre_resolved.get(material_name).map_or_else(
        || resolver.resolve_with_base2(&[], material_name),
        Clone::clone,
    );
    let Some(material) = resolved.material() else {
        log::debug!(
            "map preview surface mark {source:?} skipped: material {material_name} missing"
        );
        return None;
    };
    if material.texture.is_none() {
        log::debug!(
            "map preview surface mark {source:?} skipped: material {material_name} has no base texture"
        );
        return None;
    }
    let index = push_map_material_slot(material_name, &resolved, material.render_mode, table);
    table.index_by_name(material_name, index);
    Some(index)
}

pub(super) fn push_map_material_slot(
    name: &str,
    resolution: &MaterialResolution<ResolvedMaterialTextures>,
    render_mode: RenderMode,
    table: &mut MaterialTable,
) -> usize {
    let resolved = resolution.material();
    let texture = resolved.and_then(|material| material.texture.as_ref().map(Arc::clone));
    let texture2 = resolved.and_then(|material| material.texture2.as_ref().map(Arc::clone));
    let render_mode = resolved.map_or(render_mode, |material| {
        if material.render_mode.draws() {
            render_mode
        } else {
            material.render_mode
        }
    });
    table.push_unindexed(MaterialSlot {
        tint_base: None,
        resolution: resolution.outcome(),
        canonical_name: resolution.canonical_name().map(str::to_owned),
        shader: resolved.map_or_else(Default::default, |material| material.shader.clone()),
        name: name.to_owned(),
        texture,
        texture2,
        render_mode,
        alpha_policy: render_mode.texture_alpha_policy(),
    })
}

pub(super) fn overlay_primitive(overlay: &MapOverlay, material_index: usize) -> OverlayPrimitive {
    use crate::media::preview_model::{ModelVertex, vertex_lighting};
    OverlayPrimitive {
        source: overlay.source,
        decal_sort: None,
        vertices: overlay
            .vertices
            .iter()
            .map(|vertex| {
                let surface = vertex.surface;
                ModelVertex {
                    position: surface.position,
                    normal: surface.normal,
                    uv: vertex.uv,
                    lightmap_uv: surface.lightmap_uv,
                    lightmap_bump_step: surface.lightmap_bump_step,
                    lightmap_page: surface.lightmap_page.map_or(0, |page| page.0 + 1),
                    color: super::Vec3::splat(1.0),
                    blend_alpha: 0.0,
                    opacity: 1.0,
                    illumination: super::Vec3::splat(1.0),
                    lighting_mode: (if surface.lightmap_page.is_some() {
                        vertex_lighting::RECEIVER_LIGHTMAP
                    } else {
                        vertex_lighting::RECEIVER_CONSTANT
                    }) | if matches!(
                        overlay.source,
                        gmpublished_domain::scene::map::MapSurfaceMarkSource::Decal { .. }
                    ) {
                        vertex_lighting::BATCHED_STATIC_DECAL
                    } else {
                        0
                    },
                    tangent_s: surface.tangent_s,
                    tangent_t: surface.tangent_t,
                    tangent_sign: surface
                        .normal
                        .cross(surface.tangent_s)
                        .dot(surface.tangent_t)
                        .signum(),
                    lighting_context: 0,
                    surface_mark: 0,
                }
            })
            .collect(),
        indices: overlay.indices.clone(),
        material_index,
        visibility: overlay.visibility.clone(),
        owner: overlay.owner,
        receiver_mesh: overlay.receiver_mesh,
        receiver_range: overlay.receiver_range,
        receiver_material_index: overlay.receiver_material_index,
        render_order: overlay.render_order,
        fade: overlay.fade,
        fade_origin: overlay.fade_origin,
    }
}

pub(super) fn resolve_skybox(
    skyname: &str,
    resolver: &MaterialResolver,
    paint: Option<&gmpublished_domain::scene::map::MapSkyPaint>,
) -> Option<Skybox> {
    let mut faces = std::array::from_fn(|_| None);
    let mut materials = std::array::from_fn(|_| None);
    for face in SkyboxFace::ALL {
        let path = skybox_face_material_path(skyname, face);
        let Some(material) = resolver.resolve_sky_material_at_path(&path, paint) else {
            log::debug!(
                "map skybox {skyname}: missing LDR base texture for {}",
                face.suffix()
            );
            continue;
        };
        materials[face.index()] = Some(material.clone());
        let Some(texture) = material.texture else {
            continue;
        };
        faces[face.index()] = Some(texture);
    }

    if materials.iter().any(Option::is_some) {
        Some(Skybox {
            faces,
            materials,
            paint: paint.cloned(),
        })
    } else {
        None
    }
}

pub(super) fn resolve_sun_materials(
    suns: &[gmpublished_domain::scene::map::MapSun],
    resolver: &MaterialResolver,
    table: &mut MaterialTable,
) -> Vec<[Option<usize>; 2]> {
    suns.iter()
        .map(|sun| {
            [&sun.material, &sun.overlay_material].map(|name| {
                let index = table.index_of(name).unwrap_or_else(|| {
                    let material = resolver.resolve_with_base2(&[], name);
                    table.push(
                        name,
                        MaterialSlot {
                            tint_base: None,
                            resolution: material.outcome(),
                            canonical_name: material.canonical_name().map(str::to_owned),
                            name: name.clone(),
                            shader: material
                                .material()
                                .map_or_else(Default::default, |material| material.shader.clone()),
                            texture: material
                                .material()
                                .and_then(|material| material.texture.as_ref().map(Arc::clone)),
                            texture2: material
                                .material()
                                .and_then(|material| material.texture2.as_ref().map(Arc::clone)),
                            render_mode: material
                                .material()
                                .map_or(RenderMode::Additive, |material| material.render_mode),
                            alpha_policy: material
                                .material()
                                .map_or(RenderMode::Additive, |material| material.render_mode)
                                .texture_alpha_policy(),
                        },
                    )
                });
                if table
                    .get(index)
                    .is_some_and(|material| !material.render_mode.draws())
                {
                    None
                } else if table
                    .get(index)
                    .is_some_and(|material| material.render_mode == RenderMode::Modulate)
                {
                    log::warn!("sun material {name}: gamma-space Sun composition is unsupported");
                    None
                } else {
                    Some(index)
                }
            })
        })
        .collect()
}

pub(super) fn skybox_face_material_path(skyname: &str, face: SkyboxFace) -> String {
    format!("materials/skybox/{skyname}{}.vmt", face.suffix())
}

pub(super) fn sky_log_status(skyname: Option<&str>) -> String {
    skyname.map_or_else(|| "sky none".to_owned(), |skyname| format!("sky {skyname}"))
}
