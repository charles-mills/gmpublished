//! Languages accepted by the Steamworks UGC title and description APIs.
use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize,
)]
pub enum WorkshopLanguage {
    #[serde(rename = "arabic")]
    Arabic,
    #[serde(rename = "bulgarian")]
    Bulgarian,
    #[serde(rename = "schinese")]
    SimplifiedChinese,
    #[serde(rename = "tchinese")]
    TraditionalChinese,
    #[serde(rename = "czech")]
    Czech,
    #[serde(rename = "danish")]
    Danish,
    #[serde(rename = "dutch")]
    Dutch,
    #[default]
    #[serde(rename = "english")]
    English,
    #[serde(rename = "finnish")]
    Finnish,
    #[serde(rename = "french")]
    French,
    #[serde(rename = "german")]
    German,
    #[serde(rename = "greek")]
    Greek,
    #[serde(rename = "hungarian")]
    Hungarian,
    #[serde(rename = "indonesian")]
    Indonesian,
    #[serde(rename = "italian")]
    Italian,
    #[serde(rename = "japanese")]
    Japanese,
    #[serde(rename = "koreana")]
    Korean,
    #[serde(rename = "malay")]
    Malay,
    #[serde(rename = "norwegian")]
    Norwegian,
    #[serde(rename = "polish")]
    Polish,
    #[serde(rename = "portuguese")]
    Portuguese,
    #[serde(rename = "brazilian")]
    BrazilianPortuguese,
    #[serde(rename = "romanian")]
    Romanian,
    #[serde(rename = "russian")]
    Russian,
    #[serde(rename = "spanish")]
    Spanish,
    #[serde(rename = "latam")]
    LatinAmericanSpanish,
    #[serde(rename = "swedish")]
    Swedish,
    #[serde(rename = "thai")]
    Thai,
    #[serde(rename = "turkish")]
    Turkish,
    #[serde(rename = "ukrainian")]
    Ukrainian,
    #[serde(rename = "vietnamese")]
    Vietnamese,
}

impl WorkshopLanguage {
    pub const ALL: [Self; 31] = [
        Self::Arabic,
        Self::Bulgarian,
        Self::SimplifiedChinese,
        Self::TraditionalChinese,
        Self::Czech,
        Self::Danish,
        Self::Dutch,
        Self::English,
        Self::Finnish,
        Self::French,
        Self::German,
        Self::Greek,
        Self::Hungarian,
        Self::Indonesian,
        Self::Italian,
        Self::Japanese,
        Self::Korean,
        Self::Malay,
        Self::Norwegian,
        Self::Polish,
        Self::Portuguese,
        Self::BrazilianPortuguese,
        Self::Romanian,
        Self::Russian,
        Self::Spanish,
        Self::LatinAmericanSpanish,
        Self::Swedish,
        Self::Thai,
        Self::Turkish,
        Self::Ukrainian,
        Self::Vietnamese,
    ];
    pub const fn api_code(self) -> &'static str {
        match self {
            Self::Arabic => "arabic",
            Self::Bulgarian => "bulgarian",
            Self::SimplifiedChinese => "schinese",
            Self::TraditionalChinese => "tchinese",
            Self::Czech => "czech",
            Self::Danish => "danish",
            Self::Dutch => "dutch",
            Self::English => "english",
            Self::Finnish => "finnish",
            Self::French => "french",
            Self::German => "german",
            Self::Greek => "greek",
            Self::Hungarian => "hungarian",
            Self::Indonesian => "indonesian",
            Self::Italian => "italian",
            Self::Japanese => "japanese",
            Self::Korean => "koreana",
            Self::Malay => "malay",
            Self::Norwegian => "norwegian",
            Self::Polish => "polish",
            Self::Portuguese => "portuguese",
            Self::BrazilianPortuguese => "brazilian",
            Self::Romanian => "romanian",
            Self::Russian => "russian",
            Self::Spanish => "spanish",
            Self::LatinAmericanSpanish => "latam",
            Self::Swedish => "swedish",
            Self::Thai => "thai",
            Self::Turkish => "turkish",
            Self::Ukrainian => "ukrainian",
            Self::Vietnamese => "vietnamese",
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            Self::Arabic => "Arabic",
            Self::Bulgarian => "Bulgarian",
            Self::SimplifiedChinese => "Chinese (Simplified)",
            Self::TraditionalChinese => "Chinese (Traditional)",
            Self::Czech => "Czech",
            Self::Danish => "Danish",
            Self::Dutch => "Dutch",
            Self::English => "English",
            Self::Finnish => "Finnish",
            Self::French => "French",
            Self::German => "German",
            Self::Greek => "Greek",
            Self::Hungarian => "Hungarian",
            Self::Indonesian => "Indonesian",
            Self::Italian => "Italian",
            Self::Japanese => "Japanese",
            Self::Korean => "Korean",
            Self::Malay => "Malay",
            Self::Norwegian => "Norwegian",
            Self::Polish => "Polish",
            Self::Portuguese => "Portuguese",
            Self::BrazilianPortuguese => "Portuguese (Brazil)",
            Self::Romanian => "Romanian",
            Self::Russian => "Russian",
            Self::Spanish => "Spanish (Spain)",
            Self::LatinAmericanSpanish => "Spanish (Latin America)",
            Self::Swedish => "Swedish",
            Self::Thai => "Thai",
            Self::Turkish => "Turkish",
            Self::Ukrainian => "Ukrainian",
            Self::Vietnamese => "Vietnamese",
        }
    }
    pub fn from_locale(locale: &str) -> Self {
        let locale = locale.replace('_', "-").to_ascii_lowercase();
        match locale.as_str() {
            "kr" => return Self::Korean,
            "zh-hans" | "zh-hans-cn" => return Self::SimplifiedChinese,
            "zh-hant" | "zh-hant-tw" | "zh-hk" => return Self::TraditionalChinese,
            _ => {}
        }
        match locale.as_str() {
            "ar" => Self::Arabic,
            "bg" => Self::Bulgarian,
            "zh-cn" => Self::SimplifiedChinese,
            "zh-tw" => Self::TraditionalChinese,
            "cs" => Self::Czech,
            "da" => Self::Danish,
            "nl" => Self::Dutch,
            "en" => Self::English,
            "fi" => Self::Finnish,
            "fr" => Self::French,
            "de" => Self::German,
            "el" => Self::Greek,
            "hu" => Self::Hungarian,
            "id" => Self::Indonesian,
            "it" => Self::Italian,
            "ja" => Self::Japanese,
            "ko" => Self::Korean,
            "ms" => Self::Malay,
            "no" => Self::Norwegian,
            "pl" => Self::Polish,
            "pt" => Self::Portuguese,
            "pt-br" => Self::BrazilianPortuguese,
            "ro" => Self::Romanian,
            "ru" => Self::Russian,
            "es" => Self::Spanish,
            "es-419" => Self::LatinAmericanSpanish,
            "sv" => Self::Swedish,
            "th" => Self::Thai,
            "tr" => Self::Turkish,
            "uk" => Self::Ukrainian,
            "vi" => Self::Vietnamese,

            _ => locale
                .split('-')
                .next()
                .filter(|base| *base != locale)
                .map_or(Self::English, Self::from_locale),
        }
    }
}
impl std::fmt::Display for WorkshopLanguage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_app_catalogs_and_preserves_regional_variants() {
        for (locale, code) in [
            ("en", "english"),
            ("de", "german"),
            ("es", "spanish"),
            ("fr", "french"),
            ("kr", "koreana"),
            ("nl", "dutch"),
            ("pl", "polish"),
            ("pt-BR", "brazilian"),
            ("ru", "russian"),
            ("tr", "turkish"),
            ("uk", "ukrainian"),
            ("zh-cn", "schinese"),
            ("ko-KR", "koreana"),
            ("es-419", "latam"),
            ("pt", "portuguese"),
            ("zh-TW", "tchinese"),
            ("unknown", "english"),
        ] {
            assert_eq!(WorkshopLanguage::from_locale(locale).api_code(), code);
        }
        let codes: std::collections::HashSet<_> = WorkshopLanguage::ALL
            .into_iter()
            .map(WorkshopLanguage::api_code)
            .collect();
        assert_eq!(codes.len(), 31);
    }
}
