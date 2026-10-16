use super::{
    BspError, LightmapSource, Limits, MapBsp, MapData, MapPakFile, SourceLightmapRequirement,
    bsp_version, selected_lightmap_samples,
};

pub struct MapAsset {
    pub(super) bsp: MapBsp,
    pub(super) version: u32,
    pub(super) pakfile: MapPakFile,
}

pub fn decode_map(bytes: &[u8]) -> Result<MapAsset, BspError> {
    decode_map_impl(bytes, None)
}

pub fn decode_map_with_pool(bytes: &[u8], pool: &rayon::ThreadPool) -> Result<MapAsset, BspError> {
    decode_map_impl(bytes, Some(pool))
}

fn decode_map_impl(bytes: &[u8], pool: Option<&rayon::ThreadPool>) -> Result<MapAsset, BspError> {
    let version = bsp_version(bytes)?;
    if !matches!(version, 19 | 20) {
        return Err(BspError::UnsupportedVersion { version });
    }
    let limits = Limits {
        max_input_bytes: u64::MAX,
        ..Limits::default()
    };
    let bsp = MapBsp::parse(bytes, &limits, pool)?;
    let pakfile = MapPakFile::from_pak_bytes(bsp.pakfile_bytes.clone());
    Ok(MapAsset {
        bsp,
        version,
        pakfile,
    })
}

impl MapAsset {
    pub fn material_names(&self) -> impl ExactSizeIterator<Item = Option<&str>> {
        self.bsp.texdatas.iter().map(|texdata| {
            usize::try_from(texdata.name_index)
                .ok()
                .and_then(|index| self.bsp.texdata_strings.get(index))
                .map(String::as_str)
        })
    }

    pub fn pakfile(&self) -> &MapPakFile {
        &self.pakfile
    }

    pub fn lighting_source(&self) -> Option<LightmapSource> {
        selected_lightmap_samples(&self.bsp).1
    }

    pub fn prepare(self, requirements: &[SourceLightmapRequirement]) -> Result<MapData, BspError> {
        self.prepare_impl(requirements, None)
    }

    pub fn prepare_with_pool(
        self,
        requirements: &[SourceLightmapRequirement],
        pool: &rayon::ThreadPool,
    ) -> Result<MapData, BspError> {
        self.prepare_impl(requirements, Some(pool))
    }

    fn prepare_impl(
        self,
        requirements: &[SourceLightmapRequirement],
        pool: Option<&rayon::ThreadPool>,
    ) -> Result<MapData, BspError> {
        if requirements.len() != self.bsp.texdatas.len() {
            return Err(BspError::MaterialRequirementCount {
                expected: self.bsp.texdatas.len(),
                actual: requirements.len(),
            });
        }
        match pool {
            Some(pool) => pool.install(|| super::prepare_map(self, true, Some(requirements))),
            None => super::prepare_map(self, true, Some(requirements)),
        }
    }
}

#[cfg(test)]
mod tests;
