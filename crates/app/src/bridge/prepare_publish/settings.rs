//! Ignore-pattern settings mutation and UI projection.

use super::{
    ConfigService, IgnorePatternMutation, IgnorePatternMutationResult, IgnoredPattern, Settings,
    whitelist,
};

pub fn apply_ignore_pattern_mutation(
    config: ConfigService<'_>,
    mutation: IgnorePatternMutation,
) -> IgnorePatternMutationResult {
    let mut changed = false;
    let mut save_error = None;
    if let Err(error) = config.update_settings_snapshot(|settings| match mutation {
        IgnorePatternMutation::Add(pattern) => {
            let pattern = pattern.trim();
            if !pattern.is_empty()
                && !settings
                    .backend
                    .ignore_globs
                    .iter()
                    .any(|glob| glob == pattern)
            {
                settings.backend.ignore_globs.push(pattern.to_owned());
                changed = true;
            }
        }
        IgnorePatternMutation::Remove(pattern) => {
            let before = settings.backend.ignore_globs.len();
            settings
                .backend
                .ignore_globs
                .retain(|glob| glob != &pattern);
            changed = settings.backend.ignore_globs.len() != before;
        }
    }) {
        save_error = Some(error.to_string());
    }
    let settings = config.settings_snapshot();

    IgnorePatternMutationResult {
        changed,
        ignored_patterns: ignored_patterns_from_settings(&settings),
        save_error,
    }
}

pub fn ignored_patterns_from_settings(settings: &Settings) -> Vec<IgnoredPattern> {
    let mut patterns = Vec::with_capacity(
        settings
            .backend
            .ignore_globs
            .len()
            .saturating_add(whitelist::DEFAULT_IGNORE.len()),
    );
    patterns.extend(
        settings
            .backend
            .ignore_globs
            .iter()
            .map(|pattern| IgnoredPattern {
                pattern: pattern.clone(),
                default_pattern: false,
            }),
    );
    let mut default_patterns = whitelist::DEFAULT_IGNORE.to_vec();
    default_patterns.sort_unstable();
    patterns.extend(default_patterns.into_iter().map(|pattern| IgnoredPattern {
        pattern: pattern.to_owned(),
        default_pattern: true,
    }));
    patterns
}
