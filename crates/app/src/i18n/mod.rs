//! Fluent-backed runtime localization for the Iced UI.

use std::rc::Rc;
use std::{fmt, sync::OnceLock};

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource, FluentValue};
use fluent_langneg::{NegotiationStrategy, negotiate_languages};

const FALLBACK_LOCALE: &str = "en";

#[derive(Clone, Copy)]
struct LocaleCatalog {
    id: &'static str,
    tag: &'static str,
    source: &'static str,
}

fn catalog_source(id: &str) -> &'static str {
    CATALOGS
        .iter()
        .find(|catalog| catalog.id == id)
        .map(|catalog| catalog.source)
        .expect("every bundled locale id must have a catalog")
}

const CATALOGS: &[LocaleCatalog] = &[
    LocaleCatalog {
        id: "en",
        tag: "en",
        source: include_str!("../../i18n/en.ftl"),
    },
    LocaleCatalog {
        id: "de",
        tag: "de",
        source: include_str!("../../i18n/de.ftl"),
    },
    LocaleCatalog {
        id: "es",
        tag: "es",
        source: include_str!("../../i18n/es.ftl"),
    },
    LocaleCatalog {
        id: "fr",
        tag: "fr",
        source: include_str!("../../i18n/fr.ftl"),
    },
    LocaleCatalog {
        id: "kr",
        tag: "ko",
        source: include_str!("../../i18n/kr.ftl"),
    },
    LocaleCatalog {
        id: "nl",
        tag: "nl",
        source: include_str!("../../i18n/nl.ftl"),
    },
    LocaleCatalog {
        id: "pl",
        tag: "pl",
        source: include_str!("../../i18n/pl.ftl"),
    },
    LocaleCatalog {
        id: "pt-BR",
        tag: "pt-BR",
        source: include_str!("../../i18n/pt-BR.ftl"),
    },
    LocaleCatalog {
        id: "ru",
        tag: "ru",
        source: include_str!("../../i18n/ru.ftl"),
    },
    LocaleCatalog {
        id: "tr",
        tag: "tr",
        source: include_str!("../../i18n/tr.ftl"),
    },
    LocaleCatalog {
        id: "uk",
        tag: "uk",
        source: include_str!("../../i18n/uk.ftl"),
    },
    LocaleCatalog {
        id: "zh-cn",
        tag: "zh-CN",
        source: include_str!("../../i18n/zh-cn.ftl"),
    },
];

pub struct I18n {
    locale: &'static LocaleCatalog,
    bundles: Rc<Bundles>,
}

/// The built Fluent state for one locale, behind an `Rc` so cloning an
/// `I18n` is a refcount bump instead of re-decompressing and rebuilding it.
struct Bundles {
    bundle: FluentBundle<FluentResource>,
    fallback: FluentBundle<FluentResource>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanguageInfo {
    pub(crate) id: &'static str,
    pub(crate) name: String,
}

impl I18n {
    pub(crate) fn from_user_or_system(locale_hint: Option<&str>) -> Self {
        let system_locale = locale_hint.is_none().then(sys_locale::get_locale).flatten();
        Self::for_locale(locale_hint.or(system_locale.as_deref()))
    }

    pub(crate) fn for_locale(locale_hint: Option<&str>) -> Self {
        let locale = resolve_locale(locale_hint);
        Self {
            locale,
            bundles: Rc::new(Bundles {
                bundle: build_bundle(locale),
                fallback: build_bundle(fallback_locale()),
            }),
        }
    }

    pub(crate) fn locale_id(&self) -> &'static str {
        self.locale.id
    }

    pub(crate) fn select_locale(&mut self, locale_hint: Option<&str>) -> bool {
        let next = Self::for_locale(locale_hint);
        let changed = self.locale.id != next.locale.id;
        *self = next;
        changed
    }

    pub(crate) fn tr(&self, key: &str) -> String {
        self.format(key, None)
    }

    pub(crate) fn trn(&self, key: &str, args: &[(&str, &str)]) -> String {
        let mut fluent_args = FluentArgs::with_capacity(args.len());
        for (name, value) in args {
            fluent_args.set(*name, FluentValue::try_number(value));
        }
        self.format(key, Some(&fluent_args))
    }

    fn format(&self, key: &str, args: Option<&FluentArgs<'_>>) -> String {
        format_from_bundle(&self.bundles.bundle, key, args)
            .or_else(|| format_from_bundle(&self.bundles.fallback, key, args))
            .unwrap_or_else(|| key.to_owned())
    }
}

pub fn available_languages() -> &'static [LanguageInfo] {
    static LANGUAGES: OnceLock<Vec<LanguageInfo>> = OnceLock::new();
    LANGUAGES
        .get_or_init(|| {
            CATALOGS
                .iter()
                .map(|catalog| LanguageInfo {
                    id: catalog.id,
                    name: language_name_from_source(catalog),
                })
                .collect()
        })
        .as_slice()
}

impl Clone for I18n {
    fn clone(&self) -> Self {
        Self {
            locale: self.locale,
            bundles: Rc::clone(&self.bundles),
        }
    }
}

impl fmt::Debug for I18n {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("I18n")
            .field("locale", &self.locale.id)
            .finish_non_exhaustive()
    }
}

impl PartialEq for I18n {
    fn eq(&self, other: &Self) -> bool {
        self.locale.id == other.locale.id
    }
}

impl Eq for I18n {}

/// Translates a backend [`UiError`](crate::bridge::ui_error::UiError)
/// through the Fluent catalogs: `ERR_FOO_BAR` looks up `err-foo-bar` (and
/// `err-foo-bar-detail` when the error carries detail text).
///
/// An unmapped key falls back to `err-unknown`, never to the raw wire string —
/// `UiError`'s `Display` is the `KEY:detail` composite, so returning it here
/// put text like `ERR_GMOD_PATH_MISSING` in front of users.
pub fn translated_error(i18n: &I18n, error: &crate::bridge::ui_error::UiError) -> String {
    let key = format!(
        "err-{}",
        error
            .key
            .as_str()
            .trim_start_matches("ERR_")
            .to_ascii_lowercase()
            .replace('_', "-")
    );

    let translated = error.detail.as_ref().map_or_else(
        || i18n.tr(&key),
        |detail| {
            let detail_key = format!("{key}-detail");
            let detailed = i18n.trn(&detail_key, &[("arg0", detail.as_ref())]);
            if detailed == detail_key {
                i18n.tr(&key)
            } else {
                detailed
            }
        },
    );

    if translated == key {
        // `tr` echoes the key back on a miss, so this is "no catalog entry".
        // Log the key that needs one; show the user something readable.
        log::debug!("no catalog entry for {key}; falling back to err-unknown");
        i18n.tr("err-unknown")
    } else {
        translated
    }
}

pub fn resolve_locale_id(locale_hint: Option<&str>) -> &'static str {
    resolve_locale(locale_hint).id
}

fn resolve_locale(locale_hint: Option<&str>) -> &'static LocaleCatalog {
    let Some(requested) = requested_locale(locale_hint) else {
        return fallback_locale();
    };
    let Ok(requested) = parse_negotiation_tag(&requested) else {
        return fallback_locale();
    };

    let available = available_negotiation_tags();
    let default = available.first();
    let negotiated = negotiate_languages(
        &[requested],
        &available,
        default,
        NegotiationStrategy::Filtering,
    );
    let Some(selected) = negotiated.first() else {
        return fallback_locale();
    };
    let Some(index) = available
        .iter()
        .position(|available| available == *selected)
    else {
        return fallback_locale();
    };
    CATALOGS.get(index).unwrap_or_else(|| fallback_locale())
}

fn requested_locale(locale_hint: Option<&str>) -> Option<String> {
    let normalized = locale_hint?.trim().replace('_', "-").to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }

    Some(match normalized.as_str() {
        "kr" | "ko" | "ko-kr" => "ko".to_owned(),
        "zh" | "zh-cn" | "zh-hans" | "zh-hans-cn" => "zh-CN".to_owned(),
        "pt-br" => "pt-BR".to_owned(),
        _ => normalized,
    })
}

fn available_negotiation_tags() -> Vec<unic_langid::LanguageIdentifier> {
    CATALOGS
        .iter()
        .map(|catalog| parse_negotiation_tag(catalog.tag))
        .collect::<Result<_, _>>()
        .expect("bundled locale negotiation tags must be valid")
}

fn parse_negotiation_tag(
    tag: &str,
) -> Result<unic_langid::LanguageIdentifier, unic_langid::LanguageIdentifierError> {
    unic_langid::LanguageIdentifier::from_bytes(tag.as_bytes())
}

fn fallback_locale() -> &'static LocaleCatalog {
    CATALOGS
        .iter()
        .find(|catalog| catalog.id == FALLBACK_LOCALE)
        .expect("English fallback catalog must be bundled")
}

fn build_bundle(locale: &LocaleCatalog) -> FluentBundle<FluentResource> {
    let langid = locale
        .tag
        .parse::<unic_langid::LanguageIdentifier>()
        .expect("bundled locale tags must be valid for fluent-bundle");
    let resource = FluentResource::try_new(catalog_source(locale.id).to_owned())
        .expect("bundled Fluent catalogs must parse");
    let mut bundle = FluentBundle::new(vec![langid]);
    bundle.set_use_isolating(false);
    bundle
        .add_resource(resource)
        .expect("bundled Fluent catalogs must not contain duplicate message ids");
    bundle
}

fn language_name_from_source(catalog: &LocaleCatalog) -> String {
    catalog_source(catalog.id)
        .lines()
        .find_map(|line| line.strip_prefix("language-name = "))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(catalog.id)
        .to_owned()
}

fn format_from_bundle(
    bundle: &FluentBundle<FluentResource>,
    message_id: &str,
    args: Option<&FluentArgs<'_>>,
) -> Option<String> {
    let message = bundle.get_message(message_id)?;
    let pattern = message.value()?;
    let mut errors = Vec::new();
    Some(
        bundle
            .format_pattern(pattern, args, &mut errors)
            .into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{CATALOGS, I18n, catalog_source, resolve_locale_id};

    /// `(locale, message id)` pairs where the target language genuinely renders
    /// the message as the English string, so an identical value is a correct
    /// translation rather than a missing one: loanwords ("Addons", "Skin",
    /// "Roleplay"), cognates ("Name", "Audio", "Water"), Valve/Source format
    /// names, acronyms ("CRC", "NPC"), and byte units in the languages that do
    /// not localize them.
    ///
    /// This records facts about languages, not decisions about this codebase.
    /// Anything that is untranslatable *by construction* does not belong here:
    /// brand names live in [`crate::APP_NAME`], and pure format templates are
    /// skipped by [`has_translatable_text`]. Adding an entry asserts that a
    /// speaker of that language would write the English word in this place.
    const LOCALE_KEEPS_ENGLISH: &[(&str, &str)] = &[
        ("de", "byte-rate-per-second"),
        ("de", "byte-unit-b"),
        ("de", "byte-unit-gb"),
        ("de", "byte-unit-kb"),
        ("de", "byte-unit-mb"),
        ("de", "byte-unit-tb"),
        ("de", "destination-addons"),
        ("de", "destination-downloads"),
        ("de", "downloader-workshop-id"),
        ("de", "file-preview-crc"),
        ("de", "file-preview-model-bodygroup"),
        ("de", "file-preview-model-skin"),
        ("de", "file-preview-model-skin-option"),
        ("de", "file-preview-particle-system"),
        ("de", "file-type-audio"),
        ("de", "file-type-map"),
        ("de", "file-type-txt"),
        ("de", "prepare-publish-tag-cartoon"),
        ("de", "prepare-publish-tag-comic"),
        ("de", "prepare-publish-tag-fun"),
        ("de", "prepare-publish-type-map"),
        ("de", "prepare-publish-type-npc"),
        ("de", "preview-gma-steam-workshop"),
        ("de", "search-source-steam-workshop"),
        ("de", "size-analyzer-name"),
        ("es", "byte-rate-per-second"),
        ("es", "byte-unit-b"),
        ("es", "byte-unit-gb"),
        ("es", "byte-unit-kb"),
        ("es", "byte-unit-mb"),
        ("es", "byte-unit-tb"),
        ("es", "destination-addons"),
        ("es", "downloader-status-error"),
        ("es", "downloader-workshop-id"),
        ("es", "file-preview-crc"),
        ("es", "file-type-audio"),
        ("es", "menu-zoom"),
        ("es", "prepare-publish-type-npc"),
        ("es", "preview-gma-steam-workshop"),
        ("es", "search-source-steam-workshop"),
        ("es", "settings-tab-general"),
        ("fr", "byte-rate-per-second"),
        ("fr", "destination-addons"),
        ("fr", "downloader-workshop-id"),
        ("fr", "file-preview-audio-pause"),
        ("fr", "file-preview-crc"),
        ("fr", "file-preview-map-faces"),
        ("fr", "file-preview-model-triangles"),
        ("fr", "file-type-audio"),
        ("fr", "file-type-image"),
        ("fr", "menu-zoom"),
        ("fr", "prepare-publish-addon-type"),
        ("fr", "prepare-publish-tag-fun"),
        ("fr", "settings-accessibility-color-picker-saturation"),
        ("fr", "settings-theme-classic-source"),
        ("fr", "size-analyzer-type"),
        ("kr", "byte-rate-per-second"),
        ("kr", "byte-unit-b"),
        ("kr", "byte-unit-gb"),
        ("kr", "byte-unit-kb"),
        ("kr", "byte-unit-mb"),
        ("kr", "byte-unit-tb"),
        ("kr", "file-preview-crc"),
        ("kr", "prepare-publish-type-npc"),
        ("kr", "settings-theme-classic-source"),
        ("nl", "byte-rate-per-second"),
        ("nl", "byte-unit-b"),
        ("nl", "byte-unit-gb"),
        ("nl", "byte-unit-kb"),
        ("nl", "byte-unit-mb"),
        ("nl", "byte-unit-tb"),
        ("nl", "destination-addons"),
        ("nl", "destination-downloads"),
        ("nl", "downloader"),
        ("nl", "downloader-workshop-id"),
        ("nl", "file-preview-crc"),
        ("nl", "file-preview-model-meshes"),
        ("nl", "file-preview-model-skin"),
        ("nl", "file-preview-model-skin-option"),
        ("nl", "file-type-audio"),
        ("nl", "file-type-mdl"),
        ("nl", "menu-help"),
        ("nl", "menu-zoom"),
        ("nl", "prepare-publish-addon-type"),
        ("nl", "prepare-publish-items-num"),
        ("nl", "prepare-publish-items-one"),
        ("nl", "prepare-publish-tag-1"),
        ("nl", "prepare-publish-tag-2"),
        ("nl", "prepare-publish-tag-3"),
        ("nl", "prepare-publish-tag-cartoon"),
        ("nl", "prepare-publish-tag-fun"),
        ("nl", "prepare-publish-tag-roleplay"),
        ("nl", "prepare-publish-tag-water"),
        ("nl", "prepare-publish-type-model"),
        ("nl", "prepare-publish-type-npc"),
        ("nl", "preview-gma-steam-workshop"),
        ("nl", "search-source-steam-workshop"),
        ("nl", "size-analyzer-summary-cells"),
        ("nl", "size-analyzer-type"),
        ("pl", "byte-rate-per-second"),
        ("pl", "byte-unit-b"),
        ("pl", "byte-unit-gb"),
        ("pl", "byte-unit-kb"),
        ("pl", "byte-unit-mb"),
        ("pl", "byte-unit-tb"),
        ("pl", "file-preview-crc"),
        ("pl", "file-preview-particle-system"),
        ("pl", "file-type-fgd"),
        ("pl", "file-type-folder"),
        ("pl", "file-type-mdl"),
        ("pl", "prepare-publish-tag-1"),
        ("pl", "prepare-publish-tag-2"),
        ("pl", "prepare-publish-tag-3"),
        ("pl", "prepare-publish-tag-roleplay"),
        ("pl", "prepare-publish-type-model"),
        ("pl", "prepare-publish-type-npc"),
        ("pt-BR", "byte-rate-per-second"),
        ("pt-BR", "byte-unit-b"),
        ("pt-BR", "byte-unit-gb"),
        ("pt-BR", "byte-unit-kb"),
        ("pt-BR", "byte-unit-mb"),
        ("pt-BR", "byte-unit-tb"),
        ("pt-BR", "destination-addons"),
        ("pt-BR", "destination-downloads"),
        ("pt-BR", "file-preview-crc"),
        ("pt-BR", "file-preview-map-faces"),
        ("pt-BR", "file-preview-model-skin"),
        ("pt-BR", "file-preview-model-skin-option"),
        ("pt-BR", "file-type-fgd"),
        ("pt-BR", "file-type-vcd"),
        ("pt-BR", "file-type-vmt"),
        ("pt-BR", "file-type-vtf"),
        ("pt-BR", "menu-zoom"),
        ("pt-BR", "prepare-publish-items-one"),
        ("pt-BR", "prepare-publish-tag-1"),
        ("pt-BR", "prepare-publish-tag-2"),
        ("pt-BR", "prepare-publish-tag-3"),
        ("pt-BR", "prepare-publish-tag-roleplay"),
        ("pt-BR", "prepare-publish-type-npc"),
        ("ru", "file-preview-crc"),
        ("tr", "byte-rate-per-second"),
        ("tr", "byte-unit-b"),
        ("tr", "byte-unit-gb"),
        ("tr", "byte-unit-kb"),
        ("tr", "byte-unit-mb"),
        ("tr", "byte-unit-tb"),
        ("tr", "file-preview-crc"),
        ("tr", "file-type-mdl"),
        ("tr", "prepare-publish-type-model"),
        ("tr", "prepare-publish-type-npc"),
        ("uk", "file-preview-crc"),
        ("zh-cn", "byte-rate-per-second"),
        ("zh-cn", "byte-unit-b"),
        ("zh-cn", "byte-unit-gb"),
        ("zh-cn", "byte-unit-kb"),
        ("zh-cn", "byte-unit-mb"),
        ("zh-cn", "byte-unit-tb"),
        ("zh-cn", "file-preview-crc"),
        ("zh-cn", "prepare-publish-type-npc"),
    ];

    #[test]
    fn locale_resolution_handles_exact_alias_base_and_fallback() {
        assert_eq!(resolve_locale_id(Some("pt_BR")), "pt-BR");
        assert_eq!(resolve_locale_id(Some("ko-KR")), "kr");
        assert_eq!(resolve_locale_id(Some("kr")), "kr");
        assert_eq!(resolve_locale_id(Some("zh-Hans-CN")), "zh-cn");
        assert_eq!(resolve_locale_id(Some("fr-CA")), "fr");
        assert_eq!(resolve_locale_id(Some("missing")), "en");
        assert_eq!(resolve_locale_id(None), "en");
    }

    #[test]
    fn formats_named_and_positional_args() {
        let i18n = I18n::for_locale(Some("fr-CA"));

        assert_eq!(i18n.locale_id(), "fr");
        assert_eq!(
            i18n.trn("my-workshop-count", &[("arg0", "3"), ("arg1", "12")]),
            "Affichage de 3 sur 12 addons"
        );
        assert_eq!(i18n.tr("publish-new"), "Publier un nouveau...");
        assert_eq!(
            i18n.trn(
                "downloader-progress-percent",
                &[("arg0", "75"), ("arg1", "Téléchargement")]
            ),
            "75% Téléchargement"
        );
    }

    #[test]
    fn unsupported_locale_and_missing_key_fall_back_predictably() {
        let i18n = I18n::for_locale(Some("zz-ZZ"));

        assert_eq!(i18n.locale_id(), "en");
        assert_eq!(i18n.tr("my-workshop"), "My Workshop");
        assert_eq!(
            i18n.tr("missing.translation.key"),
            "missing.translation.key"
        );
    }

    #[test]
    fn fluent_catalogs_have_matching_key_sets() {
        let english = catalog_message_ids(catalog_source("en"));
        for catalog in CATALOGS {
            let available = catalog_message_ids(catalog_source(catalog.id));
            assert_eq!(available, english, "{} FTL coverage", catalog.id);
        }
    }

    /// Key parity alone is not coverage: pasting the English block into every
    /// catalog satisfies [`fluent_catalogs_have_matching_key_sets`] while leaving
    /// the UI in English. This asserts the values actually got translated.
    #[test]
    fn translated_catalogs_do_not_leak_english_values() {
        let english = catalog_messages(catalog_source("en"));
        for catalog in CATALOGS {
            if catalog.id == "en" {
                continue;
            }
            let translated = catalog_messages(catalog_source(catalog.id));
            let leaked = english
                .iter()
                .filter(|(_, value)| has_translatable_text(value))
                .filter(|(key, _)| !LOCALE_KEEPS_ENGLISH.contains(&(catalog.id, *key)))
                .filter(|(key, value)| translated.get(*key) == Some(*value))
                .map(|(key, _)| *key)
                .collect::<Vec<_>>();

            assert!(
                leaked.is_empty(),
                "{} still uses the English value for {} message(s): {:?}\n\
                 Translate them, or record the overlap in LOCALE_KEEPS_ENGLISH.",
                catalog.id,
                leaked.len(),
                leaked
            );
        }
    }

    #[test]
    fn packed_catalogs_match_the_source_ftl_files() {
        for catalog in CATALOGS {
            let disk = std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("i18n")
                    .join(format!("{}.ftl", catalog.id)),
            )
            .expect("bundled .ftl source should exist");
            assert_eq!(
                catalog_source(catalog.id),
                disk,
                "{} packed catalog",
                catalog.id
            );
        }
    }

    #[test]
    fn numeric_args_drive_fluent_plural_selectors() {
        let i18n = I18n::for_locale(Some("pl"));

        assert_eq!(
            i18n.trn("relative-time-past-years", &[("arg0", "2")]),
            "2 lata temu"
        );
        assert_eq!(
            i18n.trn("relative-time-past-years", &[("arg0", "5")]),
            "5 lat temu"
        );
    }

    /// Message id -> value, with Fluent block values (a bare `=` followed by
    /// indented lines) folded into a single newline-joined string so they are
    /// compared as a whole rather than skipped.
    fn catalog_messages(source: &str) -> BTreeMap<&str, String> {
        let mut messages = BTreeMap::new();
        let mut current: Option<&str> = None;

        for line in source.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            if line.starts_with([' ', '\t']) {
                if let Some(key) = current {
                    let value: &mut String = messages
                        .get_mut(key)
                        .expect("current key is inserted before it is set");
                    if !value.is_empty() {
                        value.push('\n');
                    }
                    value.push_str(trimmed);
                }
                continue;
            }

            current = match line.split_once('=') {
                Some((key, value)) if is_message_id(key.trim()) => {
                    let key = key.trim();
                    messages.insert(key, value.trim().to_owned());
                    Some(key)
                }
                _ => None,
            };
        }

        messages
    }

    /// Whether a value contains anything a translator could act on. Templates
    /// built purely from placeholders and punctuation (`{$arg0} {$arg1}`,
    /// `{$arg0}% {$arg1}`) render identically in every language, so an
    /// identical value there is correct rather than a missed translation.
    fn has_translatable_text(value: &str) -> bool {
        let mut depth = 0usize;
        value.chars().any(|ch| match ch {
            '{' => {
                depth += 1;
                false
            }
            '}' => {
                depth = depth.saturating_sub(1);
                false
            }
            _ => depth == 0 && ch.is_alphabetic(),
        })
    }

    fn is_message_id(key: &str) -> bool {
        !key.is_empty()
            && key
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    }

    fn catalog_message_ids(source: &str) -> BTreeSet<&str> {
        source
            .lines()
            .filter_map(|line| line.split_once('='))
            .filter_map(|(key, _)| {
                let key = key.trim();
                (!key.is_empty()
                    && key
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'))
                .then_some(key)
            })
            .collect()
    }
}
