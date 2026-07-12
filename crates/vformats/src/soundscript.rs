//! Soundscript files (`scripts/game_sounds_*.txt`) and the
//! `game_sounds_manifest` — a KeyValues dialect — plus the engine
//! semantics needed to interpret entries: wave-reference character
//! prefixes, `SNDLVL_*` decibel constants, and volume clamping.

use std::borrow::Cow;
use std::collections::BTreeMap;

use crate::Limits;
use crate::keyvalues::{self, KvDocument, KvError, KvValue, Parser, Token};

/// The engine's `SNDLVL_NORM`, in decibels.
pub const DEFAULT_SOUND_LEVEL_DB: f32 = 75.0;

/// One soundscript entry's fields, verbatim; interpret with
/// [`parse_sound_level_db`] and [`parse_volume`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SoundScript<'a> {
    /// `channel`, e.g. `CHAN_STATIC`.
    pub channel: Option<Cow<'a, str>>,
    /// `volume`, e.g. `0.8` or `VOL_NORM`.
    pub volume: Option<Cow<'a, str>>,
    /// `pitch`, e.g. `100` or `95,105`.
    pub pitch: Option<Cow<'a, str>>,
    /// `soundlevel`, e.g. `SNDLVL_75dB`.
    pub sound_level: Option<Cow<'a, str>>,
    /// `wave` values, including every `rndwave` alternative, in order.
    pub waves: Vec<Cow<'a, str>>,
}

/// Parse a soundscript file into entries keyed by lowercased name.
///
/// Source KeyValues allows duplicate sibling names and lookups walk from
/// the first child, so the **first** entry with a given name wins across
/// a mounted script set; later duplicate *fields* inside one entry still
/// replace earlier fields.
pub fn parse_sound_scripts<'a>(
    text: &'a str,
    limits: &Limits,
) -> Result<BTreeMap<String, SoundScript<'a>>, KvError> {
    let mut scripts = BTreeMap::new();
    scan_top_level_blocks(text, limits, |name, body| {
        scripts
            .entry(name.to_ascii_lowercase())
            .or_insert_with(|| script_from_block(body));
    })?;
    Ok(scripts)
}

/// Top-level scan matching engine tolerance: an entry is a word
/// immediately followed by `{`; lone garbage tokens between entries are
/// skipped without disturbing the pairing (real script sets contain
/// byte-order marks and stray tokens).
fn scan_top_level_blocks<'a>(
    text: &'a str,
    limits: &Limits,
    mut on_block: impl FnMut(&'a str, &KvDocument<'a>),
) -> Result<(), KvError> {
    if text.len() as u64 > limits.max_input_bytes {
        return Err(KvError::InputTooLarge {
            len: text.len() as u64,
            max: limits.max_input_bytes,
        });
    }
    let tokens = keyvalues::tokenize(text, limits)?;
    let mut parser = Parser::new(&tokens, limits);
    while let Some(token) = parser.next_token() {
        let Token::Word { text: name, .. } = token else {
            if token == Token::Open {
                parser.skip_balanced();
            }
            continue;
        };
        if !parser.consume_open() {
            continue;
        }
        let body = parser.parse_block(1)?;
        on_block(name, &body);
    }
    Ok(())
}

fn script_from_block<'a>(body: &keyvalues::KvDocument<'a>) -> SoundScript<'a> {
    let mut script = SoundScript::default();
    for pair in &body.pairs {
        match &pair.value {
            KvValue::String(value) => {
                let value = value.clone();
                if pair.key.eq_ignore_ascii_case("wave") {
                    script.waves.push(value);
                } else if pair.key.eq_ignore_ascii_case("channel") {
                    script.channel = Some(value);
                } else if pair.key.eq_ignore_ascii_case("volume") {
                    script.volume = Some(value);
                } else if pair.key.eq_ignore_ascii_case("pitch") {
                    script.pitch = Some(value);
                } else if pair.key.eq_ignore_ascii_case("soundlevel") {
                    script.sound_level = Some(value);
                }
            }
            KvValue::Block(group) => {
                if pair.key.eq_ignore_ascii_case("rndwave") {
                    for inner in &group.pairs {
                        // Nested (not a let-chain): MSRV 1.85.
                        if inner.key.eq_ignore_ascii_case("wave") {
                            if let KvValue::String(value) = &inner.value {
                                script.waves.push(value.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    script
}

/// Parse a `game_sounds_manifest` file: every `precache_file` value
/// across all top-level blocks, in order.
pub fn parse_manifest_files<'a>(
    text: &'a str,
    limits: &Limits,
) -> Result<Vec<Cow<'a, str>>, KvError> {
    let mut files = Vec::new();
    scan_top_level_blocks(text, limits, |_, body| {
        for inner in &body.pairs {
            // Nested (not a let-chain): MSRV 1.85.
            if inner.key.eq_ignore_ascii_case("precache_file") {
                if let KvValue::String(value) = &inner.value {
                    files.push(value.clone());
                }
            }
        }
    })?;
    Ok(files)
}

/// Whether a wave reference points at an audio file directly rather than
/// at another soundscript entry.
#[must_use]
pub fn is_raw_wave_reference(reference: &str) -> bool {
    let reference = strip_source_wave_prefixes(reference).to_ascii_lowercase();
    [".wav", ".mp3", ".ogg"]
        .iter()
        .any(|extension| reference.ends_with(extension))
}

/// The archive path a wave reference resolves to: prefix characters
/// stripped, normalized, lowercased, rooted under `sound/`. `None` for
/// empty or traversal (`..`) paths.
#[must_use]
pub fn sound_wave_archive_path(wave: &str) -> Option<String> {
    let stripped = strip_source_wave_prefixes(wave);
    let normalized = normalize_source_path(stripped)?;
    if normalized.starts_with("sound/") {
        Some(normalized)
    } else {
        Some(format!("sound/{normalized}"))
    }
}

/// Normalize a manifest script path; `None` unless it lives under
/// `scripts/`.
#[must_use]
pub fn normalize_script_path(path: &str) -> Option<String> {
    let path = normalize_source_path(path)?;
    path.starts_with("scripts/").then_some(path)
}

/// Strip the engine's wave-prefix characters (`*` streaming, `#` music,
/// `@` omnidirectional, `)`/`(` spatial, `^` distance, `!` sentence, ...)
/// from the front of a wave reference.
#[must_use]
pub fn strip_source_wave_prefixes(value: &str) -> &str {
    value
        .trim_start()
        .trim_start_matches(|ch| {
            matches!(
                ch,
                '*' | '#' | '@' | '>' | '<' | '^' | ')' | '!' | '?' | '&' | '~' | '`' | '+' | '%'
            )
        })
        .trim_start()
}

/// Interpret a `soundlevel` value as decibels: the named `SNDLVL_*`
/// constants, `SNDLVL_<n>dB`, or a bare number; anything else falls back
/// to [`DEFAULT_SOUND_LEVEL_DB`].
#[must_use]
pub fn parse_sound_level_db(value: Option<&str>) -> f32 {
    let Some(value) = value else {
        return DEFAULT_SOUND_LEVEL_DB;
    };
    let normalized = value.trim().to_ascii_uppercase();
    match normalized.as_str() {
        "SNDLVL_NORM" => DEFAULT_SOUND_LEVEL_DB,
        "SNDLVL_NONE" => 0.0,
        "SNDLVL_IDLE" => 60.0,
        "SNDLVL_STATIC" => 66.0,
        "SNDLVL_TALKING" => 80.0,
        _ => {
            let stripped = normalized.strip_prefix("SNDLVL_").unwrap_or(&normalized);
            stripped
                .strip_suffix("DB")
                .unwrap_or(stripped)
                .parse::<f32>()
                .unwrap_or(DEFAULT_SOUND_LEVEL_DB)
        }
    }
}

/// Interpret a `volume` value: parsed and clamped to `0.0..=1.0`,
/// defaulting to `1.0` for missing or non-numeric values.
#[must_use]
pub fn parse_volume(value: Option<&str>) -> f32 {
    value
        .and_then(|value| value.trim().parse::<f32>().ok())
        .filter(|value| value.is_finite())
        .unwrap_or(1.0)
        .clamp(0.0, 1.0)
}

fn normalize_source_path(path: &str) -> Option<String> {
    let path = path.trim().replace('\\', "/");
    let path = path.trim_matches('/');
    let mut normalized = Vec::new();
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return None;
        }
        normalized.push(segment);
    }
    let path = normalized.join("/");
    (!path.is_empty()).then(|| path.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> BTreeMap<String, SoundScript<'_>> {
        parse_sound_scripts(text, &Limits::default()).expect("parse")
    }

    #[test]
    fn parses_plain_soundscript_entry() {
        let scripts = parse(
            r"
            DoorSound.DefaultMove
            {
                channel CHAN_STATIC
                volume 0.8
                pitch 100
                soundlevel SNDLVL_75dB
                wave doors\door1_move.wav
            }
            ",
        );
        let entry = &scripts["doorsound.defaultmove"];
        assert_eq!(entry.channel.as_deref(), Some("CHAN_STATIC"));
        assert_eq!(entry.volume.as_deref(), Some("0.8"));
        assert_eq!(entry.pitch.as_deref(), Some("100"));
        assert_eq!(entry.sound_level.as_deref(), Some("SNDLVL_75dB"));
        assert_eq!(entry.waves, vec!["doors\\door1_move.wav"]);
    }

    #[test]
    fn parses_rndwave_lists() {
        let scripts = parse(
            r#"
            DoorSound.DefaultOpen
            {
                rndwave
                {
                    wave "doors/dooropen1.wav"
                    wave "doors/dooropen2.wav"
                }
            }
            "#,
        );
        assert_eq!(
            scripts["doorsound.defaultopen"].waves,
            vec!["doors/dooropen1.wav", "doors/dooropen2.wav"]
        );
    }

    #[test]
    fn handles_comments_quotes_missing_fields_and_duplicate_names() {
        let scripts = parse(
            r#"
            // duplicate key: first soundscript wins, matching KeyValues lookup order.
            "DoorSound.DefaultClose" { "wave" "doors/first.wav" }
            "DoorSound.DefaultClose" { "wave" "doors/second.wav" }
            Sparse.Entry { soundlevel "SNDLVL_NORM" }
            "#,
        );
        assert_eq!(
            scripts["doorsound.defaultclose"].waves,
            vec!["doors/first.wav"]
        );
        assert!(scripts["sparse.entry"].waves.is_empty());
        assert_eq!(
            parse_sound_level_db(scripts["sparse.entry"].sound_level.as_deref()),
            75.0
        );
    }

    #[test]
    fn parses_manifest_precache_files() {
        let files = parse_manifest_files(
            r#"
            game_sounds_manifest
            {
                "precache_file" "scripts/game_sounds.txt"
                precache_file scripts/game_sounds_doors.txt
                ignored { precache_file scripts/not-real.txt }
            }
            "#,
            &Limits::default(),
        )
        .expect("parse");
        assert_eq!(
            files,
            vec!["scripts/game_sounds.txt", "scripts/game_sounds_doors.txt"]
        );
    }

    #[test]
    fn strips_wave_prefixes_and_normalizes_case_insensitive_sound_paths() {
        assert_eq!(
            sound_wave_archive_path(r"*#@Doors\Door1_Move.WAV").as_deref(),
            Some("sound/doors/door1_move.wav")
        );
        assert_eq!(
            sound_wave_archive_path("sound/UCSounds/Door.wav").as_deref(),
            Some("sound/ucsounds/door.wav")
        );
        assert_eq!(sound_wave_archive_path("../escape.wav"), None);
        assert!(is_raw_wave_reference("doors/door1_move.wav"));
        assert!(!is_raw_wave_reference("DoorSound.DefaultMove"));

        assert_eq!(
            normalize_script_path(r"Scripts\Game_Sounds_Doors.txt").as_deref(),
            Some("scripts/game_sounds_doors.txt")
        );
        assert_eq!(
            normalize_script_path("sound/not_a_script.txt"),
            None,
            "manifest entries outside scripts/ are rejected"
        );
        assert_eq!(normalize_script_path("../scripts/evil.txt"), None);
    }

    #[test]
    fn garbage_bytes_do_not_panic() {
        let bytes = b"\xff\xfe DoorSound.Bad { wave \"doors/bad.wav\" ";
        let text = String::from_utf8_lossy(bytes);
        let scripts = parse(&text);
        assert!(scripts.contains_key("doorsound.bad"));
    }

    #[test]
    fn sound_level_and_volume_semantics() {
        assert_eq!(parse_sound_level_db(None), 75.0);
        assert_eq!(parse_sound_level_db(Some("SNDLVL_NONE")), 0.0);
        assert_eq!(parse_sound_level_db(Some("SNDLVL_85dB")), 85.0);
        assert_eq!(parse_sound_level_db(Some("90")), 90.0);
        assert_eq!(parse_sound_level_db(Some("garbage")), 75.0);
        assert_eq!(parse_volume(Some("0.5")), 0.5);
        assert_eq!(parse_volume(Some("7")), 1.0);
        assert_eq!(parse_volume(Some("VOL_NORM")), 1.0);
        assert_eq!(parse_volume(None), 1.0);
    }
}
