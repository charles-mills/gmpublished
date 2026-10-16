use super::parameters::{self, MaterialProxy};
use std::{borrow::Cow, collections::BTreeMap};
use vformats::keyvalues::{KvDocument, KvPair, KvValue};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct HardwareProfile {
    dx_level: u32,
    ps20b: bool,
    hdr: bool,
    srgb: bool,
    gpu_level: u32,
}

impl HardwareProfile {
    pub(super) const LDR: Self = Self {
        dx_level: 90,
        ps20b: true,
        hdr: false,
        srgb: true,
        gpu_level: 3,
    };

    #[cfg(test)]
    pub(super) const HDR: Self = Self {
        hdr: true,
        ..Self::LDR
    };

    fn blocks(self) -> Vec<&'static str> {
        let mut result = match self.gpu_level {
            0 => vec!["GPU<1"],
            1 => vec!["GPU<2", "GPU>=1"],
            _ => vec!["GPU>=1", "GPU>=2"],
        };
        if self.dx_level < 90 {
            result.push("<DX90");
        }
        if self.dx_level < 95 {
            result.push("<DX95");
        }
        result.push(if self.dx_level >= 90 && self.ps20b {
            ">=DX90_20b"
        } else {
            "<DX90_20b"
        });
        if self.dx_level <= 90 {
            result.push("<=DX90");
        }
        if self.dx_level >= 90 {
            result.push(">=DX90");
        }
        if self.dx_level > 90 {
            result.push(">DX90");
        }
        if self.hdr {
            result.extend(["hdr_dx9", "hdr"]);
        } else {
            result.push("ldr");
        }
        if self.srgb {
            result.push("srgb");
        }
        if self.dx_level >= 90 {
            result.push("dx9");
        }
        result
    }

    pub(super) fn condition(self, key: &str) -> Option<bool> {
        Some(match key {
            "srgb" | "srgb_pc" => self.srgb,
            "hdr" | "hdr_dx9" => self.hdr,
            "ldr" => !self.hdr,
            "dx9" | ">=dx90" => self.dx_level >= 90,
            "<dx90" => self.dx_level < 90,
            "<dx95" => self.dx_level < 95,
            "<=dx90" => self.dx_level <= 90,
            ">dx90" => self.dx_level > 90,
            ">=dx90_20b" => self.dx_level >= 90 && self.ps20b,
            "<dx90_20b" => self.dx_level < 90 || !self.ps20b,
            "gpu<1" => self.gpu_level < 1,
            "gpu<2" => self.gpu_level < 2,
            "gpu<3" => self.gpu_level < 3,
            "gpu>=1" => self.gpu_level >= 1,
            "gpu>=2" => self.gpu_level >= 2,
            "gpu>=3" => self.gpu_level >= 3,
            "360" | "sonyps3" | "gameconsole" | "srgb_gameconsole" => false,
            _ => return None,
        })
    }
}

pub(super) fn owned(document: &KvDocument<'_>) -> KvDocument<'static> {
    KvDocument {
        pairs: document
            .pairs
            .iter()
            .map(|pair| KvPair {
                key: Cow::Owned(pair.key.to_string()),
                value: match &pair.value {
                    KvValue::String(value) => KvValue::String(Cow::Owned(value.to_string())),
                    KvValue::Block(block) => KvValue::Block(owned(block)),
                },
            })
            .collect(),
    }
}

fn selected<'a, 'b>(
    document: &'a KvDocument<'b>,
    shader: &str,
    profile: HardwareProfile,
) -> Option<&'a KvDocument<'b>> {
    for condition in profile.blocks() {
        for key in [condition.to_owned(), format!("{shader}_{condition}")] {
            if let Some(KvValue::Block(block)) = document.get(&key) {
                return Some(block);
            }
        }
    }
    None
}

pub(super) fn resolve(
    document: &KvDocument<'_>,
    shader: &str,
    profile: HardwareProfile,
) -> (BTreeMap<String, String>, Vec<MaterialProxy>) {
    let mut parameters = parameters::string_values(document, profile);
    let block = selected(document, shader, profile);
    if let Some(block) = block {
        parameters.extend(parameters::string_values(block, profile));
    }
    // FindKey("Proxies") follows the selected block's parent chain; an own
    // proxy block shadows the inherited group as a whole.
    let proxy_source = block
        .filter(|block| block.get("Proxies").is_some())
        .unwrap_or(document);
    (parameters, parameters::proxies(proxy_source, profile))
}

/// Merge raw KeyValues before profile selection. Preserve blocks and duplicate
/// source entries, replacing only the first matching key just as FindKey does.
pub(super) fn merge(base: &mut KvDocument<'static>, patch: &KvDocument<'_>, insert: bool) {
    for pair in &patch.pairs {
        if let Some(existing) = base
            .pairs
            .iter_mut()
            .find(|existing| existing.key.eq_ignore_ascii_case(&pair.key))
        {
            match (&mut existing.value, &pair.value) {
                (KvValue::Block(base), KvValue::Block(patch)) => merge(base, patch, insert),
                _ if !insert => existing.value = owned_value(&pair.value),
                _ => {}
            }
        } else if insert {
            base.pairs.push(KvPair {
                key: Cow::Owned(pair.key.to_string()),
                value: owned_value(&pair.value),
            });
        }
    }
}

fn owned_value(value: &KvValue<'_>) -> KvValue<'static> {
    match value {
        KvValue::String(value) => KvValue::String(Cow::Owned(value.to_string())),
        KvValue::Block(block) => KvValue::Block(owned(block)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn material(text: &str) -> vformats::vmt::VmtDocument<'_> {
        vformats::vmt::parse(text, &vformats::Limits::default()).unwrap()
    }

    #[test]
    fn matching_shader_profiles_inherit_without_merging_unrelated_families() {
        for shader in [
            "LightmappedGeneric",
            "WorldVertexTransition",
            "VertexLitGeneric",
            "UnlitGeneric",
            "Water",
        ] {
            let text = format!(
                r#"{shader} {{ "$alpha" ".8" "$color" "outer" "{shader}_DX9" {{ "$color" "selected" }} WrongShader_DX9 {{ "$alpha" ".1" }} }}"#
            );
            let source = material(&text);
            let (effective, _) = resolve(&source.kv, &source.shader, HardwareProfile::LDR);
            assert_eq!(effective["$color"], "selected");
            assert_eq!(effective["$alpha"], ".8");
        }
    }

    #[test]
    fn first_supported_global_block_shadows_shader_block_and_later_conditions() {
        let source = material(
            r#"LightmappedGeneric { "$color" "outer" LightmappedGeneric_DX9 { "$color" "family" } dx9 { "$color" "dx9" } srgb { "$color" "srgb" } ">=DX90" { "$color" "dx90" } }"#,
        );
        assert_eq!(
            resolve(&source.kv, &source.shader, HardwareProfile::LDR).0["$color"],
            "dx90"
        );
        let source = material(
            r#"LightmappedGeneric { "$alpha" ".8" dx9 { "$color" "first" } dx9 { "$color" "second" } LightmappedGeneric_DX9 { "$alpha" ".1" } }"#,
        );
        let values = resolve(&source.kv, &source.shader, HardwareProfile::LDR).0;
        assert_eq!(values["$color"], "first");
        assert_eq!(values["$alpha"], ".8");
    }

    #[test]
    fn raw_profile_blocks_survive_patch_assembly_and_selected_proxies_replace_parent_group() {
        let source = material(
            r#"LightmappedGeneric { "$color" "outer" "$alpha" ".8" Proxies { Parent { value old } } LightmappedGeneric_DX9 { "$color" "selected" Proxies { Own { value own } } } }"#,
        );
        let mut source = owned(&source.kv);
        let patch = material(
            r#"Patch { replace { "$color" "patch-outer" LightmappedGeneric_DX9 { "$color" "patched-selected" } } }"#,
        );
        merge(
            &mut source,
            patch.kv.blocks("replace").next().unwrap(),
            false,
        );
        let (values, proxies) = resolve(&source, "LightmappedGeneric", HardwareProfile::LDR);
        assert_eq!(values["$color"], "patched-selected");
        assert_eq!(values["$alpha"], ".8");
        assert_eq!(proxies.len(), 1);
        assert_eq!(proxies[0].name, "own");
    }

    #[test]
    fn replace_cannot_introduce_a_new_profile_block() {
        let source =
            material(r#"LightmappedGeneric { LightmappedGeneric_DX9 { "$color" "selected" } }"#);
        let mut source = owned(&source.kv);
        let patch = material(r#"Patch { replace { ">=DX90" { "$color" "ignored" } } }"#);
        merge(
            &mut source,
            patch.kv.blocks("replace").next().unwrap(),
            false,
        );
        assert_eq!(
            resolve(&source, "LightmappedGeneric", HardwareProfile::LDR).0["$color"],
            "selected"
        );
        assert!(source.get(">=DX90").is_none());
    }

    #[test]
    fn native_hardware_predicates_and_missing_scalar_replace_are_preserved() {
        let profile = HardwareProfile::LDR;
        assert!(profile.blocks().contains(&"<DX95"));
        assert!(profile.blocks().contains(&"<=DX90"));
        assert!(!profile.blocks().contains(&">DX90"));
        assert!(!profile.blocks().contains(&"GPU>=3"));
        assert_eq!(profile.condition("gpu>=3"), Some(true));
        let source = material(r#"LightmappedGeneric { "$color" "outer" }"#);
        let mut source = owned(&source.kv);
        let patch = material(r#"Patch { replace { "$alpha" ".7" } }"#);
        merge(
            &mut source,
            patch.kv.blocks("replace").next().unwrap(),
            false,
        );
        assert!(
            !resolve(&source, "LightmappedGeneric", HardwareProfile::LDR)
                .0
                .contains_key("$alpha")
        );
    }

    #[test]
    fn insert_preserves_existing_values_and_adds_missing_nested_values() {
        let source = material(r#"LightmappedGeneric { dx9 { "$color" "selected" } }"#);
        let mut source = owned(&source.kv);
        let patch = material(r#"Patch { insert { dx9 { "$color" "ignored" "$alpha" ".7" } } }"#);
        merge(&mut source, patch.kv.blocks("insert").next().unwrap(), true);
        let values = resolve(&source, "LightmappedGeneric", HardwareProfile::LDR).0;
        assert_eq!(values["$color"], "selected");
        assert_eq!(values["$alpha"], ".7");
    }
    #[test]
    fn one_profile_controls_blocks_inline_values_and_proxies() {
        let source = material(
            r#"VertexLitGeneric {
            "$alpha" "outer" "hdr?$alpha" "hdr-outer" "ldr?$alpha" "ldr-outer"
            hdr { "$color" "hdr" "hdr?$detailblendfactor" ".7" Proxies { Control { "hdr?value" "yes" "ldr?value" "no" } } }
            ldr { "$color" "ldr" "ldr?$detailblendfactor" ".3" Proxies { Control { "hdr?value" "no" "ldr?value" "yes" } } }
        }"#,
        );
        for (profile, color, alpha, factor) in [
            (HardwareProfile::LDR, "ldr", "ldr-outer", ".3"),
            (
                HardwareProfile {
                    hdr: true,
                    ..HardwareProfile::LDR
                },
                "hdr",
                "hdr-outer",
                ".7",
            ),
        ] {
            let (values, proxies) = resolve(&source.kv, &source.shader, profile);
            assert_eq!(values["$color"], color);
            assert_eq!(values["$alpha"], alpha);
            assert_eq!(values["$detailblendfactor"], factor);
            assert_eq!(proxies[0].values["value"], "yes");
        }
    }
}
