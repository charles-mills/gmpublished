//! Map material-slot ownership, resolution bookkeeping, and diagnostics.

use super::{
    Arc, BTreeSet, HashMap, HashSet, MAP_FALLBACK_TEXTURE_DIMENSION, MaterialSlot, RenderMode,
};

#[derive(Debug)]
pub(in super::super) struct PropMaterialResolveJob {
    pub(in super::super) key: String,
    pub(in super::super) name: String,
    pub(in super::super) material_dirs: Vec<String>,
}

/// The map's material slots and the name index into them.
///
/// The material slots a baked map references by index, plus the name lookup
/// used to share a slot between meshes. Telemetry counts are derived from the
/// slots on demand so they cannot disagree with them.
#[derive(Debug, Default)]
pub(in super::super) struct MaterialTable {
    slots: Vec<MaterialSlot>,
    indexes: HashMap<String, usize>,
}

impl MaterialTable {
    pub(in super::super) fn push(&mut self, name: &str, slot: MaterialSlot) -> usize {
        let index = self.slots.len();
        self.slots.push(slot);
        self.indexes.insert(name.to_owned(), index);
        index
    }

    /// Appends without indexing it by name. Used where a slot must not be
    /// reachable by later lookups — the detail material is pushed with a
    /// hard-coded render mode an overlay of the same name must not inherit.
    pub(in super::super) fn push_unindexed(&mut self, slot: MaterialSlot) -> usize {
        let index = self.slots.len();
        self.slots.push(slot);
        index
    }

    pub(in super::super) fn index_of(&self, name: &str) -> Option<usize> {
        self.indexes.get(name).copied()
    }

    pub(in super::super) fn contains(&self, name: &str) -> bool {
        self.indexes.contains_key(name)
    }

    #[cfg(test)]
    pub(in super::super) fn name_indexes(&self) -> &HashMap<String, usize> {
        &self.indexes
    }

    pub(in super::super) fn get(&self, index: usize) -> Option<&MaterialSlot> {
        self.slots.get(index)
    }

    pub(in super::super) fn resolved_count(&self) -> u32 {
        count_of(self.slots.iter().filter(|slot| slot.texture.is_some()))
    }

    pub(in super::super) fn water_fallback_count(&self) -> u32 {
        count_of(self.slots.iter().filter(|slot| {
            slot.texture
                .as_ref()
                .is_some_and(|texture| texture.is_water_fallback())
        }))
    }

    pub(in super::super) fn into_slots(self) -> Vec<MaterialSlot> {
        self.slots
    }

    /// Makes an already-pushed slot reachable by name.
    pub(in super::super) fn index_by_name(&mut self, name: &str, index: usize) {
        self.indexes.insert(name.to_owned(), index);
    }

    pub(in super::super) fn slots(&self) -> &[MaterialSlot] {
        &self.slots
    }

    pub(in super::super) fn len(&self) -> usize {
        self.slots.len()
    }
}

fn count_of<T>(items: impl Iterator<Item = T>) -> u32 {
    u32::try_from(items.count()).unwrap_or(u32::MAX)
}

pub(in super::super) fn water_fallback_log_suffix(count: u32) -> String {
    if count == 0 {
        String::new()
    } else {
        format!(", water {count}")
    }
}

pub(in super::super) fn format_mib(bytes: usize) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}

pub(in super::super) fn texture_payload_log_suffix(materials: &[MaterialSlot]) -> String {
    let (bc, rgba) = texture_payload_counts(materials);
    format!(" (BC {bc}, RGBA {rgba})")
}

pub(in super::super) fn texture_payload_counts(materials: &[MaterialSlot]) -> (usize, usize) {
    let mut seen = HashSet::new();
    let mut bc = 0_usize;
    let mut rgba = 0_usize;
    for texture in materials
        .iter()
        .flat_map(|material| [material.texture.as_ref(), material.texture2.as_ref()])
        .flatten()
    {
        if !seen.insert(Arc::as_ptr(texture)) {
            continue;
        }
        if texture.is_bc() {
            bc += 1;
        } else {
            rgba += 1;
        }
    }
    (bc, rgba)
}

pub(in super::super) fn render_mode_log_suffix(materials: &[MaterialSlot]) -> String {
    let translucent = materials
        .iter()
        .filter(|material| material.render_mode == RenderMode::Translucent)
        .count();
    let additive = materials
        .iter()
        .filter(|material| material.render_mode == RenderMode::Additive)
        .count();
    format!(", translucent {translucent}, additive {additive}")
}

pub(in super::super) fn log_unresolved_materials(materials: &[MaterialSlot]) {
    let (names, total) = unresolved_material_names_for_debug(materials);
    if total > 0 {
        log::debug!(
            "map unresolved materials {}/{}: {}",
            names.len(),
            total,
            names.join(", ")
        );
    }
}

pub(in super::super) fn unresolved_material_names_for_debug(
    materials: &[MaterialSlot],
) -> (Vec<String>, usize) {
    let names = materials
        .iter()
        .filter(|material| material.texture.is_none())
        .map(|material| material.name.clone())
        .collect::<BTreeSet<_>>();
    let total = names.len();
    (names.into_iter().take(20).collect(), total)
}

pub(in super::super) fn material_dimensions(
    materials: &[MaterialSlot],
    index: usize,
) -> (u32, u32) {
    materials
        .get(index)
        .and_then(|material| material.texture.as_ref())
        .map_or(
            (
                MAP_FALLBACK_TEXTURE_DIMENSION,
                MAP_FALLBACK_TEXTURE_DIMENSION,
            ),
            |texture| texture.original_dimensions(),
        )
}

pub(in super::super) fn normalize_map_uv(
    tex_s: f32,
    tex_t: f32,
    width: u32,
    height: u32,
) -> [f32; 2] {
    [tex_s / width.max(1) as f32, tex_t / height.max(1) as f32]
}
