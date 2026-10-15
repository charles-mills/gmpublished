use super::{ContentPath, MaterialResolver, SourceRef};

impl MaterialResolver {
    /// VHV indices belong to one BSP; identically named files in other content
    /// sources must never become a missing map's lighting fallback.
    pub(crate) fn packed_prop_lighting(&self, index: usize, hdr: bool) -> Option<Vec<u8>> {
        let name = if hdr {
            format!("sp_hdr_{index}.vhv")
        } else {
            format!("sp_{index}.vhv")
        };
        let path = ContentPath::new(&name)?;
        let source = self.config.pakfile.as_deref()?;
        Self::tier_bytes(&SourceRef::Pakfile(source), &path)
    }

    #[cfg(test)]
    pub(crate) fn prop_lighting_fixture(
        packed: Vec<(&str, Vec<u8>)>,
        addon: Vec<(&str, Vec<u8>)>,
    ) -> Self {
        use super::tests::{zip_fixture, zip_stored_entry};
        use crate::{
            bridge::{archive::PreviewArchiveSource, gma::PreviewArchive},
            test_support::GmaFixtureBuilder,
        };
        use gmpublished_domain::scene::map::MapPakFile;
        use std::sync::Arc;
        let mut builder = GmaFixtureBuilder::new("Prop Lighting Fixture");
        for (name, bytes) in addon {
            builder = builder.entry(name, bytes);
        }
        let archive = PreviewArchive::from_gma(builder.build()).unwrap();
        let pakfile = MapPakFile::from_pak_bytes(zip_fixture(
            packed
                .into_iter()
                .map(|(name, bytes)| zip_stored_entry(name, bytes)),
        ));
        Self::with_pakfile_source(
            PreviewArchiveSource::from_gma(Arc::new(archive)),
            None,
            pakfile,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prop_lighting_is_scoped_to_its_map_and_hdr_variant() {
        let resolver = MaterialResolver::prop_lighting_fixture(
            vec![("sp_0.vhv", vec![1]), ("sp_hdr_0.vhv", vec![2])],
            vec![
                ("sp_0.vhv", vec![9]),
                ("sp_1.vhv", vec![8]),
                ("sp_hdr_1.vhv", vec![7]),
            ],
        );
        assert_eq!(resolver.packed_prop_lighting(0, false), Some(vec![1]));
        assert_eq!(resolver.packed_prop_lighting(0, true), Some(vec![2]));
        assert_eq!(resolver.packed_prop_lighting(1, false), None);
        assert_eq!(resolver.packed_prop_lighting(1, true), None);
    }
}
