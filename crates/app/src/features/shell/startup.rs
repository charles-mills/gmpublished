use std::time::Duration;

use serde_json::Value;
use thiserror::Error;

use super::UpdateRelease;

const GITHUB_LATEST_RELEASE_API_URL: &str =
    "https://api.github.com/repos/charles-mills/gmpublished/releases/latest";
const GITHUB_RELEASE_TAG_URL_PREFIX: &str =
    "https://github.com/charles-mills/gmpublished/releases/tag/";
const UPDATE_CHECK_USER_AGENT: &str = concat!("gmpublished/", env!("CARGO_PKG_VERSION"));
// Fail fast and silent: a slow GitHub round-trip should never hold the
// update badge hostage. Sub-timeouts sit inside the global budget so none
// of them is dead config.
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(3);
const UPDATE_CHECK_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const UPDATE_CHECK_RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Error)]
pub enum UpdateCheckError {
    #[error("failed to fetch latest GitHub release")]
    Request(#[source] ureq::Error),
    #[error("failed to read latest GitHub release response")]
    ResponseRead(#[source] ureq::Error),
}

/// Fetches the latest GitHub release, returning it only when newer than current.
pub fn fetch_latest_update(
    current_version: &str,
) -> Result<Option<UpdateRelease>, UpdateCheckError> {
    let agent = update_check_agent();
    let mut response = agent
        .get(GITHUB_LATEST_RELEASE_API_URL)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", UPDATE_CHECK_USER_AGENT)
        .call()
        .map_err(UpdateCheckError::Request)?;

    let body = response
        .body_mut()
        .read_to_string()
        .map_err(UpdateCheckError::ResponseRead)?;

    Ok(update_release_from_json(&body, current_version))
}

fn update_check_agent() -> ureq::Agent {
    crate::net::build_agent(
        UPDATE_CHECK_TIMEOUT,
        UPDATE_CHECK_CONNECT_TIMEOUT,
        UPDATE_CHECK_RESPONSE_TIMEOUT,
    )
}

fn update_release_from_json(json: &str, current_version: &str) -> Option<UpdateRelease> {
    let value = serde_json::from_str::<Value>(json).ok()?;
    update_release_from_value(&value, current_version)
}

fn update_release_from_value(value: &Value, current_version: &str) -> Option<UpdateRelease> {
    let tag = trimmed_json_string(value, "tag_name")?;
    let release_version = extract_numeric_version_suffix(&tag)?;
    if !is_newer_than_current(current_version, release_version) {
        return None;
    }

    let url = release_url_from_value(value, &tag)?;
    Some(UpdateRelease::new(tag, url))
}

fn trimmed_json_string(value: &Value, key: &str) -> Option<String> {
    let text = value.get(key)?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }

    Some(text.to_owned())
}

fn release_url_from_value(value: &Value, tag: &str) -> Option<String> {
    let fallback_url = fallback_release_url(tag)?;
    if let Some(url) = trimmed_json_string(value, "html_url")
        && url == fallback_url
    {
        return Some(url);
    }

    Some(fallback_url)
}

fn fallback_release_url(tag: &str) -> Option<String> {
    if !is_safe_release_tag_for_url(tag) {
        return None;
    }

    Some(format!("{GITHUB_RELEASE_TAG_URL_PREFIX}{tag}"))
}

fn is_safe_release_tag_for_url(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '/'))
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct NumericVersion {
    major: u64,
    minor: u64,
    patch: u64,
}

impl NumericVersion {
    const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

fn extract_numeric_version_suffix(tag: &str) -> Option<NumericVersion> {
    let trimmed = tag.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut suffix_start = 0;
    for (index, character) in trimmed.char_indices().rev() {
        if !character.is_ascii_digit() && character != '.' {
            suffix_start = index + character.len_utf8();
            break;
        }
    }

    if !has_safe_version_suffix_boundary(trimmed, suffix_start) {
        return None;
    }

    parse_numeric_version(&trimmed[suffix_start..])
}

fn has_safe_version_suffix_boundary(tag: &str, suffix_start: usize) -> bool {
    if suffix_start == 0 {
        return true;
    }

    let Some(boundary) = tag[..suffix_start].chars().last() else {
        return false;
    };

    matches!(boundary, 'v' | 'V' | '-' | '_' | '/')
}

fn parse_numeric_version(version: &str) -> Option<NumericVersion> {
    let version = version.trim();
    if version.is_empty() || version.starts_with('.') || version.ends_with('.') {
        return None;
    }

    let mut parts = [0_u64; 3];
    let mut part_count = 0_usize;
    for part in version.split('.') {
        if part_count == parts.len()
            || part.is_empty()
            || !part.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }

        parts[part_count] = part.parse::<u64>().ok()?;
        part_count += 1;
    }

    if part_count == 0 {
        return None;
    }

    Some(NumericVersion::new(parts[0], parts[1], parts[2]))
}

fn is_newer_than_current(current_version: &str, release_version: NumericVersion) -> bool {
    let Some(current_version) = parse_numeric_version(current_version) else {
        return false;
    };

    release_version > current_version
}

#[cfg(test)]
mod tests {
    use super::{
        NumericVersion, extract_numeric_version_suffix, is_newer_than_current,
        update_release_from_json,
    };

    #[test]
    fn extracts_numeric_version_suffix_from_release_tags() {
        assert_eq!(
            extract_numeric_version_suffix("v1.2.3"),
            Some(NumericVersion::new(1, 2, 3))
        );
        assert_eq!(
            extract_numeric_version_suffix("gmpublished-v1.2.3"),
            Some(NumericVersion::new(1, 2, 3))
        );
        assert_eq!(
            extract_numeric_version_suffix("release-2.4"),
            Some(NumericVersion::new(2, 4, 0))
        );
        assert_eq!(extract_numeric_version_suffix("v1.2.3-beta.1"), None);
        assert_eq!(extract_numeric_version_suffix("v1.2.3-beta1"), None);
        assert_eq!(extract_numeric_version_suffix("v1.2.3+1"), None);
        assert_eq!(extract_numeric_version_suffix("v1.2.beta"), None);
        assert_eq!(extract_numeric_version_suffix("1.2.3.4"), None);
        assert_eq!(extract_numeric_version_suffix(""), None);
    }

    #[test]
    fn compares_numeric_versions_deterministically() {
        assert!(is_newer_than_current("0.1.0", NumericVersion::new(0, 1, 1)));
        assert!(!is_newer_than_current(
            "0.1.0",
            NumericVersion::new(0, 1, 0)
        ));
        assert!(!is_newer_than_current(
            "0.1.1",
            NumericVersion::new(0, 1, 0)
        ));
        assert!(!is_newer_than_current("1.2", NumericVersion::new(1, 2, 0)));
        assert!(is_newer_than_current("1.2", NumericVersion::new(1, 2, 1)));
        assert!(!is_newer_than_current(
            "current",
            NumericVersion::new(9, 9, 9)
        ));
    }

    #[test]
    fn parses_minimal_github_release_json_into_update_state() {
        let release = update_release_from_json(
            r#"{"tag_name":"v0.1.1","html_url":"https://github.com/charles-mills/gmpublished/releases/tag/v0.1.1"}"#,
            "0.1.0",
        );

        assert_eq!(
            release,
            Some(super::UpdateRelease::new(
                "v0.1.1".to_owned(),
                "https://github.com/charles-mills/gmpublished/releases/tag/v0.1.1".to_owned(),
            ))
        );
    }

    #[test]
    fn ignores_malformed_or_non_update_release_json() {
        assert_eq!(update_release_from_json("not json", "0.1.0"), None);
        assert_eq!(
            update_release_from_json(r#"{"html_url":"https://example.com"}"#, "0.1.0"),
            None
        );
        assert_eq!(
            update_release_from_json(r#"{"tag_name":"v0.1.0"}"#, "0.1.0"),
            None
        );
        assert_eq!(
            update_release_from_json(r#"{"tag_name":"v0.1.1-beta.1"}"#, "0.1.0"),
            None
        );
        assert_eq!(
            update_release_from_json(r#"{"tag_name":"v0.1.1"}"#, "0.1.0"),
            Some(super::UpdateRelease::new(
                "v0.1.1".to_owned(),
                "https://github.com/charles-mills/gmpublished/releases/tag/v0.1.1".to_owned(),
            ))
        );
    }

    #[test]
    fn falls_back_when_release_json_url_is_unexpected() {
        assert_eq!(
            update_release_from_json(
                r#"{"tag_name":"v0.1.1","html_url":"https://example.com/releases/v0.1.1"}"#,
                "0.1.0",
            ),
            Some(super::UpdateRelease::new(
                "v0.1.1".to_owned(),
                "https://github.com/charles-mills/gmpublished/releases/tag/v0.1.1".to_owned(),
            ))
        );
    }
}
