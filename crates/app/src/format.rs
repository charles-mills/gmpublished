use crate::bridge::DownloadCountFormat;

use crate::i18n::I18n;

const BYTE_UNIT_BASE: f64 = 1024.0;
const BYTE_UNIT_KEYS: [&str; 5] = [
    "byte-unit-b",
    "byte-unit-kb",
    "byte-unit-mb",
    "byte-unit-gb",
    "byte-unit-tb",
];

/// Formats a byte count with binary 1024-byte units and existing app labels.
#[must_use]
pub fn format_bytes(bytes: u64, i18n: &I18n) -> String {
    let (value, unit_key) = byte_value_and_unit(bytes, i18n);
    let unit = i18n.tr(unit_key);
    i18n.trn(
        "byte-format",
        &[("arg0", value.as_str()), ("arg1", unit.as_str())],
    )
}

fn byte_value_and_unit(bytes: u64, i18n: &I18n) -> (String, &'static str) {
    let mut value = bytes as f64;
    let mut unit_index = 0;

    while value >= BYTE_UNIT_BASE && unit_index < BYTE_UNIT_KEYS.len() - 1 {
        value /= BYTE_UNIT_BASE;
        unit_index += 1;
    }

    let formatted = if unit_index == 0 {
        DownloadCountFormatter::from_format_and_locale(
            DownloadCountFormat::Automatic,
            Some(i18n.locale_id()),
        )
        .format_count(bytes)
    } else {
        // Round to two decimals and trim trailing zeros (and a bare trailing
        // dot), e.g. "531.29 MB", "1.5 MB", "1 MB".
        let rounded = format!("{value:.2}");
        let trimmed = rounded.trim_end_matches('0').trim_end_matches('.');
        trimmed.to_owned()
    };
    (formatted, BYTE_UNIT_KEYS[unit_index])
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct DownloadCountFormatter {
    separator: Option<char>,
}

impl Default for DownloadCountFormatter {
    fn default() -> Self {
        Self::with_separator(',')
    }
}

impl DownloadCountFormatter {
    #[must_use]
    pub(crate) fn from_format_and_locale(
        format: DownloadCountFormat,
        locale: Option<&str>,
    ) -> Self {
        match format {
            DownloadCountFormat::Automatic => {
                let language = crate::i18n::resolve_locale_id(locale);
                Self::for_language(language)
            }
            DownloadCountFormat::Comma => Self::with_separator(','),
            DownloadCountFormat::Period => Self::with_separator('.'),
            DownloadCountFormat::Space => Self::with_separator(' '),
            DownloadCountFormat::Plain => Self::plain(),
        }
    }

    #[must_use]
    pub(crate) fn format_count(self, count: u64) -> String {
        let Some(separator) = self.separator else {
            return count.to_string();
        };

        let digits = count.to_string();
        let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
        for (index, digit) in digits.chars().enumerate() {
            if index > 0 && (digits.len() - index).is_multiple_of(3) {
                formatted.push(separator);
            }
            formatted.push(digit);
        }

        formatted
    }

    const fn plain() -> Self {
        Self { separator: None }
    }

    const fn with_separator(separator: char) -> Self {
        Self {
            separator: Some(separator),
        }
    }

    fn for_language(language: &str) -> Self {
        match language {
            "de" | "es" | "nl" | "pt-BR" | "tr" => Self::with_separator('.'),
            "fr" | "pl" | "ru" | "uk" => Self::with_separator(' '),
            _ => Self::with_separator(','),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_binary_byte_units() {
        let i18n = I18n::for_locale(Some("en"));

        assert_eq!(format_bytes(0, &i18n), "0 B");
        assert_eq!(format_bytes(999, &i18n), "999 B");
        assert_eq!(format_bytes(1_000, &i18n), "1,000 B");
        assert_eq!(format_bytes(1_024, &i18n), "1 KB");
        assert_eq!(format_bytes(12_500, &i18n), "12.21 KB");
        assert_eq!(format_bytes(1_572_864, &i18n), "1.5 MB");
        // filesize.js v6 two-decimal rounding with trailing zeros trimmed.
        assert_eq!(format_bytes(557_046_046, &i18n), "531.24 MB");
        assert_eq!(format_bytes(1_048_576, &i18n), "1 MB");
    }

    #[test]
    fn formats_byte_units_with_locale_grouping_and_labels() {
        let i18n = I18n::for_locale(Some("fr"));

        assert_eq!(format_bytes(1_000, &i18n), "1 000 o");
        assert_eq!(format_bytes(1_048_576, &i18n), "1 Mo");
    }

    #[test]
    fn formats_download_counts_with_explicit_grouping() {
        assert_eq!(
            DownloadCountFormatter::from_format_and_locale(DownloadCountFormat::Comma, None)
                .format_count(1_234_567),
            "1,234,567"
        );
        assert_eq!(
            DownloadCountFormatter::from_format_and_locale(DownloadCountFormat::Period, None)
                .format_count(1_234_567),
            "1.234.567"
        );
        assert_eq!(
            DownloadCountFormatter::from_format_and_locale(DownloadCountFormat::Space, None)
                .format_count(1_234_567),
            "1 234 567"
        );
        assert_eq!(
            DownloadCountFormatter::from_format_and_locale(DownloadCountFormat::Plain, None)
                .format_count(1_234_567),
            "1234567"
        );
    }

    #[test]
    fn infers_download_count_grouping_from_supported_language() {
        assert_eq!(
            DownloadCountFormatter::from_format_and_locale(
                DownloadCountFormat::Automatic,
                Some("en-US")
            )
            .format_count(12_345),
            "12,345"
        );
        assert_eq!(
            DownloadCountFormatter::from_format_and_locale(
                DownloadCountFormat::Automatic,
                Some("de-DE")
            )
            .format_count(12_345),
            "12.345"
        );
        assert_eq!(
            DownloadCountFormatter::from_format_and_locale(
                DownloadCountFormat::Automatic,
                Some("fr-FR")
            )
            .format_count(12_345),
            "12 345"
        );
    }
}
