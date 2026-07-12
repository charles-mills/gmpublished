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

/// Translates a backend [`UiError`](crate::backend::ui_error::UiError)
/// through the Fluent catalogs: `ERR_FOO_BAR` looks up `err-foo-bar` (and
/// `err-foo-bar-detail` when the error carries detail text), falling back to
/// the raw error string when no entry exists.
pub fn translated_error(i18n: &I18n, error: &crate::backend::ui_error::UiError) -> String {
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
        error.to_string()
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
    use std::collections::BTreeSet;

    use super::{CATALOGS, I18n, catalog_source, resolve_locale_id};

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
