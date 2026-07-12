//! VMT material documents — a KeyValues dialect: one shader name, then a
//! block of `$parameter` pairs and proxy/patch groups.
//!
//! This module *parses* materials. Resolving them (search paths, patch
//! include chasing, fallbacks) is deliberately the caller's policy; the
//! [`VmtDocument::patch`] accessor only detects and normalizes what a
//! `patch` material declares.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use crate::Limits;
use crate::keyvalues::{self, KvDocument, KvError, KvValue, Parser};

/// A parsed material: shader name plus its KeyValues body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmtDocument<'a> {
    /// The shader as written, e.g. `LightmappedGeneric` or `patch`.
    pub shader: Cow<'a, str>,
    /// The material body. Direct string pairs are the shader parameters;
    /// nested blocks are groups (proxies, `insert`/`replace`, ...).
    pub kv: KvDocument<'a>,
}

/// VMT parse failure.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum VmtError {
    /// The document has no shader token.
    MissingShader,
    /// The KeyValues layer rejected the input.
    Kv(KvError),
}

impl fmt::Display for VmtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingShader => write!(f, "vmt document has no shader token"),
            Self::Kv(error) => write!(f, "vmt keyvalues error: {error}"),
        }
    }
}

impl std::error::Error for VmtError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MissingShader => None,
            Self::Kv(error) => Some(error),
        }
    }
}

impl From<KvError> for VmtError {
    fn from(error: KvError) -> Self {
        Self::Kv(error)
    }
}

/// Parse a material. The body's braces are optional, matching engine
/// tolerance (`Shader { ... }` and `Shader $key value` both parse).
pub fn parse<'a>(text: &'a str, limits: &Limits) -> Result<VmtDocument<'a>, VmtError> {
    if text.len() as u64 > limits.max_input_bytes {
        return Err(KvError::InputTooLarge {
            len: text.len() as u64,
            max: limits.max_input_bytes,
        }
        .into());
    }
    let tokens = keyvalues::tokenize(text, limits)?;
    let mut parser = Parser::new(&tokens, limits);
    let shader = parser.next_word().ok_or(VmtError::MissingShader)?;
    parser.consume_open();
    // Depth 1: the first unmatched `}` ends the material body whether or
    // not the opening brace was present.
    let kv = parser.parse_block(1)?;
    Ok(VmtDocument {
        shader: Cow::Borrowed(shader),
        kv,
    })
}

/// Convenience: the normalized `$basetexture` of a material, if any.
/// Parse errors map to `None` — use [`parse`] when you need to tell
/// "unparseable" from "has no `$basetexture`".
#[must_use]
pub fn basetexture(text: &str, limits: &Limits) -> Option<String> {
    parse(text, limits).ok().and_then(|doc| doc.basetexture())
}

impl<'a> VmtDocument<'a> {
    /// First matching shader parameter (direct string pair,
    /// ASCII case-insensitive), e.g. `value("$basetexture")`.
    #[must_use]
    pub fn value(&self, key: &str) -> Option<&str> {
        self.kv.get_str(key)
    }

    /// The normalized `$basetexture` path: backslashes to slashes,
    /// leading slashes stripped, a trailing `.vtf` removed.
    pub fn basetexture(&self) -> Option<String> {
        self.value("$basetexture").and_then(normalize_texture_path)
    }

    /// Whether this is a `patch` material.
    #[must_use]
    pub fn is_patch(&self) -> bool {
        self.shader.eq_ignore_ascii_case("patch")
    }

    /// Patch metadata, if this is a `patch` material with an `include`.
    #[must_use]
    pub fn patch(&self) -> Option<VmtPatch<'a>> {
        if !self.is_patch() {
            return None;
        }
        let include = normalize_vmt_path(self.value("include")?);
        Some(VmtPatch {
            include,
            overrides: self.patch_overrides(),
        })
    }

    /// The `insert`/`replace` overrides, deduplicated case-insensitively
    /// with `replace` winning over `insert`, sorted by lowercased key.
    fn patch_overrides(&self) -> Vec<VmtOverride<'a>> {
        let mut overrides = BTreeMap::<String, VmtOverride<'a>>::new();
        for group_name in ["insert", "replace"] {
            for group in self.kv.blocks(group_name) {
                for pair in &group.pairs {
                    if let KvValue::String(value) = &pair.value {
                        overrides.insert(
                            pair.key.to_ascii_lowercase(),
                            VmtOverride {
                                key: pair.key.clone(),
                                value: value.clone(),
                            },
                        );
                    }
                }
            }
        }
        overrides.into_values().collect()
    }
}

/// What a `patch` material declares: the target and its overrides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmtPatch<'a> {
    /// Normalized include path, e.g. `materials/base/wall.vmt`.
    pub include: String,
    /// Merged `insert`/`replace` parameters (`replace` wins).
    pub overrides: Vec<VmtOverride<'a>>,
}

/// One patch override parameter, key case preserved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmtOverride<'a> {
    /// The parameter key as written.
    pub key: Cow<'a, str>,
    /// The parameter value.
    pub value: Cow<'a, str>,
}

impl VmtPatch<'_> {
    /// First matching override (ASCII case-insensitive).
    #[must_use]
    pub fn value(&self, key: &str) -> Option<&str> {
        self.overrides
            .iter()
            .find(|param| param.key.eq_ignore_ascii_case(key))
            .map(|param| &*param.value)
    }

    /// The normalized `$basetexture` override, if any.
    pub fn basetexture(&self) -> Option<String> {
        self.value("$basetexture").and_then(normalize_texture_path)
    }
}

/// Normalize a texture reference: VMT path rules plus a trailing `.vtf`
/// (any case) removed. Returns `None` for an empty result.
#[must_use]
pub fn normalize_texture_path(value: &str) -> Option<String> {
    let mut value = normalize_vmt_path(value);
    if value
        .get(value.len().saturating_sub(4)..)
        .is_some_and(|extension| extension.eq_ignore_ascii_case(".vtf"))
    {
        value.truncate(value.len() - 4);
    }
    (!value.is_empty()).then_some(value)
}

/// Normalize a VMT-referenced path: trim, backslashes to forward slashes,
/// leading slashes stripped. Case is preserved.
#[must_use]
pub fn normalize_vmt_path(value: &str) -> String {
    value
        .trim()
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basetexture(text: &str) -> Option<String> {
        super::basetexture(text, &Limits::default())
    }

    fn parse(text: &str) -> Result<VmtDocument<'_>, VmtError> {
        super::parse(text, &Limits::default())
    }

    #[test]
    fn basetexture_handles_real_world_keyvalue_shapes() {
        let cases = [
            (
                r#"
                "LightmappedGeneric"
                {
                    "$basetexture" "brick/wall01"
                }
                "#,
                Some("brick/wall01"),
            ),
            (
                r"
                VertexlitGeneric
                {
                    $basetexture models\props_c17\door01
                }
                ",
                Some("models/props_c17/door01"),
            ),
            (
                r#"
                "VertexLitGeneric"
                {
                    // "$basetexture" "wrong/path"
                    "$baseTexture" "models/props_junk/Traffic Cone.vtf"
                }
                "#,
                Some("models/props_junk/Traffic Cone"),
            ),
            (
                "'LightmappedGeneric'\n{\n\t'$basetexture'\t'custom folder/painted wall'\n}",
                Some("custom folder/painted wall"),
            ),
            (
                r#"
                UnlitGeneric
                {
                    "$detail" "detail/noise"
                    "$basetexture" "vgui/icons/spawn"
                }
                "#,
                Some("vgui/icons/spawn"),
            ),
            (
                r#"
                VertexlitGeneric
                {
                    $baseTexture    "models/weapons/v_smg1/smg1_sheet"
                    "$basetexture" "later/ignored"
                }
                "#,
                Some("models/weapons/v_smg1/smg1_sheet"),
            ),
            (
                "LightmappedGeneric { $basetexture materials/dev/dev_measurewall01.vTf }",
                Some("materials/dev/dev_measurewall01"),
            ),
            (
                r#"
                LightmappedGeneric
                {
                    "$surfaceprop" "metal" // "$basetexture" "comment/decoy"
                    "$basetexture" "metal/trim"
                }
                "#,
                Some("metal/trim"),
            ),
            (
                r#"
                LightmappedGeneric
                {
                    "$surfaceprop" "metal"
                }
                "#,
                None,
            ),
        ];

        for (input, expected) in cases {
            assert_eq!(basetexture(input).as_deref(), expected, "input: {input}");
        }
    }

    #[test]
    fn parse_exposes_shader_params_and_groups() {
        let document = parse(
            r#"
            Patch
            {
                include "materials/base\wall.vmt"
                insert
                {
                    "$surfaceprop" "brick"
                    nested { "$ignored" "yes" }
                }
                replace
                {
                    "$basetexture" "brick\wall02"
                }
            }
            "#,
        )
        .expect("patch document should parse");

        assert_eq!(document.shader, "Patch");
        assert_eq!(document.value("include"), Some("materials/base\\wall.vmt"));
        let patch = document.patch().expect("patch metadata");
        assert_eq!(patch.include, "materials/base/wall.vmt");
        assert_eq!(patch.basetexture().as_deref(), Some("brick/wall02"));
        assert_eq!(patch.value("$surfaceprop"), Some("brick"));
        // Nested groups inside insert/replace never leak into overrides.
        assert!(patch.value("$ignored").is_none());
    }

    #[test]
    fn patch_replace_wins_over_insert() {
        let document = parse(
            r#"
            patch
            {
                include "materials/base.vmt"
                insert { "$basetexture" "insert/value" }
                replace { "$basetexture" "replace/value.vtf" }
            }
            "#,
        )
        .expect("patch document should parse");

        assert_eq!(
            document
                .patch()
                .and_then(|patch| patch.basetexture())
                .as_deref(),
            Some("replace/value")
        );
    }

    #[test]
    fn braceless_body_and_missing_shader() {
        let document = parse("UnlitGeneric $basetexture some/tex").expect("braceless");
        assert_eq!(document.basetexture().as_deref(), Some("some/tex"));

        assert!(matches!(
            parse("// only a comment"),
            Err(VmtError::MissingShader)
        ));
        assert!(matches!(parse(""), Err(VmtError::MissingShader)));
    }

    #[test]
    fn conditional_suffixes_do_not_corrupt_pairing() {
        // The reference parser mis-paired after a bare [$X360]; the
        // keyvalues core strips it. Deliberate (improving) divergence.
        let document = parse(
            r#"
            LightmappedGeneric
            {
                "$basetexture" "pc/tex" [$WIN32]
                "$surfaceprop" "metal"
            }
            "#,
        )
        .expect("parse");
        assert_eq!(document.basetexture().as_deref(), Some("pc/tex"));
        assert_eq!(document.value("$surfaceprop"), Some("metal"));
    }
}
