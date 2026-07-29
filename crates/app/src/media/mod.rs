pub mod audio_output;
pub mod audio_playback;
pub mod backdrop;
pub mod file_preview_decode;
pub mod pixel;
pub mod preview_model;
pub mod size_analyzer_render;
pub mod sounds;
pub mod text;
pub mod text_measure;
pub mod thumbhash;
pub mod thumbnail_animation;
pub mod thumbnail_demand;
pub mod thumbnail_worker;

#[cfg(test)]
mod layering_tests {
    use std::path::Path;

    /// `media` sits below `features`: it decodes and renders the values a
    /// feature displays, and [`preview_model`] holds the shapes they meet on.
    /// An import pointing the other way makes the decoder depend on the screen
    /// its output happens to be shown on, which is what put `PreviewData` and
    /// its thirty companions inside `features::file_preview` to begin with.
    ///
    /// Asserted against the sources because nothing else can: a cycle within
    /// one crate is legal Rust, so the compiler has no opinion here.
    #[test]
    fn media_never_imports_a_feature() {
        fn walk(dir: &Path, self_path: &Path, out: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, self_path, out);
                } else if path.extension().is_some_and(|ext| ext == "rs")
                    // Only this file, which states the rule and so names the
                    // thing it forbids. Every other `mod.rs` is still walked —
                    // `file_preview_decode/mod.rs` is where the imports this
                    // guards against actually lived.
                    && path != self_path
                    && let Ok(source) = std::fs::read_to_string(&path)
                {
                    for (number, line) in source.lines().enumerate() {
                        if line.contains("crate::features") {
                            out.push(format!("{}:{}", path.display(), number + 1));
                        }
                    }
                }
            }
        }

        let media = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("media");
        assert!(media.is_dir(), "the media directory must be walked, not silently skipped");

        let mut offenders = Vec::new();
        walk(&media, &media.join("mod.rs"), &mut offenders);
        assert!(
            offenders.is_empty(),
            "media must not depend on features: {offenders:#?}"
        );
    }
}
