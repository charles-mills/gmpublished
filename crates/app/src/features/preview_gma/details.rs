use std::path::Path;

use gmpublished_backend::bbcode::Document as BbCodeDocument;
use iced::widget::image;
use jiff::{Timestamp, tz::TimeZone};

use crate::bridge::domain::{AvatarRgba, PublishedFileId};
use crate::bridge::gma::{GmaHeader, GmaMetadata, PreviewArchive, workshop_id_from_filename};
use crate::format::DownloadCountFormatter;

use super::model::WorkshopMetadata;

const UNIX_TIMESTAMP_DATE_FORMAT: &str = "%Y-%m-%d %H:%M";
const MINUTE_SECONDS: i64 = 60;
const HOUR_SECONDS: i64 = 60 * MINUTE_SECONDS;
const DAY_SECONDS: i64 = 24 * HOUR_SECONDS;
const MONTH_SECONDS: i64 = 30 * DAY_SECONDS;
const YEAR_SECONDS: i64 = 365 * DAY_SECONDS;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Details {
    pub(crate) title: String,
    pub(crate) archive_path: String,
    pub(crate) author: Option<AuthorDisplay>,
    pub(crate) metadata_rows: Vec<MetadataRow>,
    pub(crate) tag_rows: Vec<TagRow>,
    pub(crate) description: BbCodeDocument,
    pub(crate) has_stats: bool,
    pub(crate) subscriptions: String,
    pub(crate) score_bucket: i32,
    pub(crate) score_label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MetadataValue {
    Bytes(u64),
    Relative(RelativeTime),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataRow {
    pub(crate) label_key: &'static str,
    pub(crate) value: MetadataValue,
    pub(crate) tooltip: String,
    pub(crate) avatar: Option<image::Handle>,
}

/// Author row projection: a resolved profile or the Steam2 placeholder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorDisplay {
    pub(crate) name: String,
    /// `None` renders the anonymous placeholder avatar.
    pub(crate) avatar: Option<image::Handle>,
    pub(crate) profile_url: Option<String>,
    /// The async profile lookup failed; a small dead glyph joins the row.
    pub(crate) failed: bool,
}

impl MetadataRow {
    fn bytes(label_key: &'static str, value: u64) -> Self {
        Self {
            label_key,
            value: MetadataValue::Bytes(value),
            tooltip: String::new(),
            avatar: None,
        }
    }

    fn timestamp(label_key: &'static str, relative: RelativeTime, absolute: String) -> Self {
        Self {
            label_key,
            value: MetadataValue::Relative(relative),
            tooltip: absolute,
            avatar: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelativeTime {
    pub(crate) key: &'static str,
    pub(crate) count: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TagRow {
    pub(crate) label: String,
}

pub fn details_for_archive(
    archive: &PreviewArchive,
    archive_path: &str,
    fallback_title: &str,
    workshop: Option<&WorkshopMetadata>,
    author_fetch_failed: bool,
    formatter: DownloadCountFormatter,
) -> Details {
    let header = &archive.index().header;
    let mut metadata_rows = Vec::new();
    let total_size = archive_total_bytes(archive);
    if total_size > 0 {
        metadata_rows.push(MetadataRow::bytes("preview-gma-size", total_size));
    }

    // Author/timestamp rows come from Workshop data only.
    if let Some(workshop) = workshop {
        push_timestamp_rows(&mut metadata_rows, workshop);
    }

    let (has_stats, subscriptions, score_bucket, score_label) = workshop.map_or_else(
        || (false, String::new(), 0, String::new()),
        |metadata| {
            (
                true,
                formatter.format_count(metadata.subscriptions),
                metadata.score_bucket,
                metadata.score_label.clone(),
            )
        },
    );

    Details {
        title: details_title(header, fallback_title, workshop),
        archive_path: archive_path.to_owned(),
        author: workshop.and_then(|workshop| author_display(workshop, author_fetch_failed)),
        metadata_rows,
        tag_rows: tag_rows(&header.metadata, workshop),
        description: BbCodeDocument::parse(&details_description(&header.metadata, workshop)),
        has_stats,
        subscriptions,
        score_bucket,
        score_label,
    }
}

pub fn infer_workshop_id_from_path(path: &Path) -> Option<PublishedFileId> {
    path.file_stem()
        .and_then(|name| name.to_str())
        .and_then(workshop_id_from_filename)
        .and_then(PublishedFileId::new)
}

fn author_display(workshop: &WorkshopMetadata, fetch_failed: bool) -> Option<AuthorDisplay> {
    let profile_url = workshop
        .steamid64
        .map(|steamid64| format!("https://steamcommunity.com/profiles/{steamid64}"));
    if let Some(author) = workshop
        .author
        .as_deref()
        .map(str::trim)
        .filter(|author| !author.is_empty())
    {
        return Some(AuthorDisplay {
            name: author.to_owned(),
            avatar: workshop.avatar.as_ref().and_then(avatar_handle_from_rgba),
            profile_url,
            failed: false,
        });
    }

    let steamid64 = workshop.steamid64?;
    Some(AuthorDisplay {
        name: super::model::steam2_rendered_id(steamid64),
        avatar: None,
        profile_url,
        failed: fetch_failed,
    })
}

fn push_timestamp_rows(rows: &mut Vec<MetadataRow>, workshop: &WorkshopMetadata) {
    if let Some((relative, absolute)) = relative_timestamp(u64::from(workshop.time_created)) {
        rows.push(MetadataRow::timestamp(
            "preview-gma-created",
            relative,
            absolute,
        ));
    }
    if workshop.time_updated != workshop.time_created
        && let Some((relative, absolute)) = relative_timestamp(u64::from(workshop.time_updated))
    {
        rows.push(MetadataRow::timestamp(
            "preview-gma-updated",
            relative,
            absolute,
        ));
    }
}

fn details_title(
    header: &GmaHeader,
    fallback_title: &str,
    workshop: Option<&WorkshopMetadata>,
) -> String {
    if let Some(title) = workshop
        .map(|metadata| metadata.title.trim())
        .filter(|title| !title.is_empty())
    {
        return title.to_owned();
    }

    // The click source's title (what the user just saw) wins over the gma
    // header so the sidebar title never pops when metadata hydrates.
    let fallback = fallback_title.trim();
    if !fallback.is_empty() {
        return fallback.to_owned();
    }

    header.title().trim().to_owned()
}

fn details_description(metadata: &GmaMetadata, workshop: Option<&WorkshopMetadata>) -> String {
    if let Some(description) = workshop
        .map(|metadata| metadata.description.trim())
        .filter(|description| !description.is_empty())
    {
        return description.to_owned();
    }

    match metadata {
        GmaMetadata::Legacy { description, .. } => description.trim().to_owned(),
        GmaMetadata::Standard { .. } => String::new(),
    }
}

fn tag_rows(metadata: &GmaMetadata, workshop: Option<&WorkshopMetadata>) -> Vec<TagRow> {
    let mut rows = Vec::new();
    if let Some(addon_type) = metadata
        .addon_type()
        .map(str::trim)
        .filter(|addon_type| !addon_type.is_empty())
    {
        push_tag(&mut rows, addon_type);
    }
    if let Some(workshop) = workshop {
        for tag in &workshop.tags {
            push_tag(&mut rows, tag);
        }
    }
    if let Some(tags) = metadata.tags() {
        for tag in tags {
            push_tag(&mut rows, tag);
        }
    }
    rows
}

fn push_tag(rows: &mut Vec<TagRow>, raw_tag: &str) {
    let tag = raw_tag.trim();
    if tag.is_empty() || rows.iter().any(|row| row.label.eq_ignore_ascii_case(tag)) {
        return;
    }
    rows.push(TagRow {
        label: tag.to_owned(),
    });
}

fn relative_timestamp(timestamp: u64) -> Option<(RelativeTime, String)> {
    let absolute = format_unix_timestamp(timestamp)?;
    Some((relative_to_now(timestamp), absolute))
}

fn format_unix_timestamp(timestamp: u64) -> Option<String> {
    if timestamp == 0 {
        return None;
    }

    let Ok(seconds) = i64::try_from(timestamp) else {
        log::warn!("Preview GMA archive timestamp {timestamp} exceeds supported range");
        return Some(timestamp.to_string());
    };

    match Timestamp::from_second(seconds) {
        Ok(timestamp) => Some(
            timestamp
                .to_zoned(TimeZone::system())
                .strftime(UNIX_TIMESTAMP_DATE_FORMAT)
                .to_string(),
        ),
        Err(error) => {
            log::warn!("invalid Preview GMA archive timestamp {timestamp}: {error}");
            Some(timestamp.to_string())
        }
    }
}

fn relative_to_now(timestamp: u64) -> RelativeTime {
    let then = i64::try_from(timestamp).unwrap_or(i64::MAX);
    let now = Timestamp::now().as_second();
    let delta = now - then;
    let magnitude = delta.unsigned_abs() as i64;

    let (value, unit) = if magnitude >= YEAR_SECONDS {
        (magnitude / YEAR_SECONDS, "years")
    } else if magnitude >= MONTH_SECONDS {
        (magnitude / MONTH_SECONDS, "months")
    } else if magnitude >= DAY_SECONDS {
        (magnitude / DAY_SECONDS, "days")
    } else if magnitude >= HOUR_SECONDS {
        (magnitude / HOUR_SECONDS, "hours")
    } else if magnitude >= MINUTE_SECONDS {
        (magnitude / MINUTE_SECONDS, "minutes")
    } else if magnitude > 0 {
        (magnitude, "seconds")
    } else {
        return RelativeTime {
            key: "relative-time-now",
            count: String::new(),
        };
    };
    let unit = if value == 1 {
        unit.trim_end_matches('s')
    } else {
        unit
    };
    let direction = if delta < 0 { "future" } else { "past" };

    RelativeTime {
        key: match (direction, unit) {
            ("future", "year") => "relative-time-future-year",
            ("future", "years") => "relative-time-future-years",
            ("future", "month") => "relative-time-future-month",
            ("future", "months") => "relative-time-future-months",
            ("future", "day") => "relative-time-future-day",
            ("future", "days") => "relative-time-future-days",
            ("future", "hour") => "relative-time-future-hour",
            ("future", "hours") => "relative-time-future-hours",
            ("future", "minute") => "relative-time-future-minute",
            ("future", "minutes") => "relative-time-future-minutes",
            ("future", "second") => "relative-time-future-second",
            ("future", "seconds") => "relative-time-future-seconds",
            ("past", "year") => "relative-time-past-year",
            ("past", "years") => "relative-time-past-years",
            ("past", "month") => "relative-time-past-month",
            ("past", "months") => "relative-time-past-months",
            ("past", "day") => "relative-time-past-day",
            ("past", "days") => "relative-time-past-days",
            ("past", "hour") => "relative-time-past-hour",
            ("past", "hours") => "relative-time-past-hours",
            ("past", "minute") => "relative-time-past-minute",
            ("past", "minutes") => "relative-time-past-minutes",
            ("past", "second") => "relative-time-past-second",
            ("past", "seconds") => "relative-time-past-seconds",
            _ => "relative-time-now",
        },
        count: value.to_string(),
    }
}

fn archive_total_bytes(archive: &PreviewArchive) -> u64 {
    archive
        .entries()
        .iter()
        .fold(0_u64, |total, entry| total.saturating_add(entry.size))
}

fn avatar_handle_from_rgba(avatar: &AvatarRgba) -> Option<image::Handle> {
    let expected = usize::try_from(avatar.width)
        .ok()?
        .checked_mul(usize::try_from(avatar.height).ok()?)?
        .checked_mul(4)?;
    if avatar.rgba.len() != expected {
        log::warn!(
            "Preview GMA author avatar has {} bytes, expected {expected}",
            avatar.rgba.len()
        );
        return None;
    }

    Some(image::Handle::from_rgba(
        avatar.width,
        avatar.height,
        avatar.rgba.as_ref().to_vec(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::gma::PreviewArchive;
    use crate::test_support::GmaFixtureBuilder;

    #[test]
    fn local_archive_details_include_size_author_timestamp_and_tags() {
        let archive = PreviewArchive::from_gma(
            GmaFixtureBuilder::new("Fixture Title")
                .entry("lua/autorun/init.lua", b"print('ok')\n".to_vec())
                .build(),
        )
        .expect("fixture archive should load");

        let details = details_for_archive(
            &archive,
            "/tmp/local.gma",
            "Fallback",
            None,
            false,
            DownloadCountFormatter::default(),
        );

        // The click-source title wins over the gma header (no title pop-in).
        assert_eq!(details.title, "Fallback");
        assert_eq!(details.archive_path, "/tmp/local.gma");
        assert!(details.metadata_rows.iter().any(|row| {
            row.label_key == "preview-gma-size" && row.value == MetadataValue::Bytes(12)
        }));
        assert!(details.author.is_none());
        assert!(
            !details
                .metadata_rows
                .iter()
                .any(|row| row.label_key == "preview-gma-created")
        );
    }

    #[test]
    fn workshop_metadata_overrides_title_description_and_stats() {
        let archive = PreviewArchive::from_gma(
            GmaFixtureBuilder::new("Local")
                .entry("lua/init.lua", b"ok".to_vec())
                .build(),
        )
        .expect("fixture archive should load");
        let mut workshop = WorkshopMetadata {
            id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            title: "Remote Title".to_owned(),
            author: Some("Ada".to_owned()),
            steamid64: Some(76_561_197_990_735_296),
            avatar: None,
            time_created: 1_717_171_717,
            time_updated: 1_717_181_717,
            description: "Remote description".to_owned(),
            tags: vec!["fun".to_owned()],
            preview_url: Some("https://example.invalid/preview.jpg".to_owned()),
            subscriptions: 12_345,
            score_bucket: 4,
            score_label: "80.00%".to_owned(),
        };

        let details = details_for_archive(
            &archive,
            "/tmp/local.gma",
            "Fallback",
            Some(&workshop),
            false,
            DownloadCountFormatter::default(),
        );

        let author = details.author.as_ref().expect("author should project");
        assert_eq!(author.name, "Ada");
        assert_eq!(
            author.profile_url.as_deref(),
            Some("https://steamcommunity.com/profiles/76561197990735296")
        );
        assert!(!author.failed);
        assert_eq!(details.title, "Remote Title");
        assert_eq!(details.description.plain_text(), "Remote description");
        assert!(details.has_stats);
        assert_eq!(details.subscriptions, "12,345");
        assert_eq!(details.score_bucket, 4);
        let period_details = details_for_archive(
            &archive,
            "/tmp/local.gma",
            "Fallback",
            Some(&workshop),
            false,
            DownloadCountFormatter::from_format_and_locale(
                crate::bridge::DownloadCountFormat::Period,
                None,
            ),
        );
        assert_eq!(period_details.subscriptions, "12.345");
        assert!(details.tag_rows.iter().any(|row| row.label == "fun"));
        assert!(
            details
                .metadata_rows
                .iter()
                .any(|row| row.label_key == "preview-gma-updated")
        );

        let long_body = "Long workshop text. ".repeat(40);
        let long_body = long_body.trim_end();
        let long_plain_text = format!("Full description\n{long_body}");
        workshop.description = format!("[h1]Full description[/h1]\n{long_body}");
        let long_details = details_for_archive(
            &archive,
            "/tmp/local.gma",
            "Fallback",
            Some(&workshop),
            false,
            DownloadCountFormatter::default(),
        );
        assert!(long_plain_text.len() > 255);
        assert_eq!(long_details.description.plain_text(), long_plain_text);
    }

    #[test]
    fn missing_owner_projects_the_steam2_placeholder_until_fetched() {
        let archive = PreviewArchive::from_gma(
            GmaFixtureBuilder::new("Local")
                .entry("lua/init.lua", b"ok".to_vec())
                .build(),
        )
        .expect("fixture archive should load");
        let workshop = WorkshopMetadata {
            id: PublishedFileId::new(123).expect("test fixture ids are always nonzero"),
            title: "Remote".to_owned(),
            author: None,
            steamid64: Some(76_561_197_990_735_296),
            avatar: None,
            time_created: 0,
            time_updated: 0,
            description: String::new(),
            tags: Vec::new(),
            preview_url: None,
            subscriptions: 0,
            score_bucket: 0,
            score_label: String::new(),
        };

        let pending = details_for_archive(
            &archive,
            "/a.gma",
            "F",
            Some(&workshop),
            false,
            DownloadCountFormatter::default(),
        );
        let author = pending.author.as_ref().expect("placeholder author");
        assert_eq!(author.name, "STEAM_1:0:15234784");
        assert!(author.avatar.is_none());
        assert!(!author.failed);

        let failed = details_for_archive(
            &archive,
            "/a.gma",
            "F",
            Some(&workshop),
            true,
            DownloadCountFormatter::default(),
        );
        assert!(failed.author.as_ref().expect("placeholder author").failed);
    }

    #[test]
    fn relative_time_uses_fluent_key_boundary() {
        let now = u64::try_from(Timestamp::now().as_second()).unwrap_or_default();
        let timestamp = now.saturating_sub(2 * DAY_SECONDS as u64);

        let relative = relative_to_now(timestamp);

        assert_eq!(relative.key, "relative-time-past-days");
        assert_eq!(relative.count, "2");
    }
}
