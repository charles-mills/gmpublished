use std::{env, error::Error, fmt::Write as _, fs, io, io::Write as _, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=GMPUBLISHED_STEAM_RUNTIME_DIR");

    if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    }

    if let Err(error) = copy_steam_runtime_for_local_build() {
        println!("cargo:warning=Steam runtime library was not copied: {error}");
    }

    compress_bundled_fonts()?;
    compress_locale_catalogs()?;

    Ok(())
}

const FONT_SOURCES: &[&str] = &[
    "ui/fonts/Inter-Regular.ttf",
    "ui/fonts/Inter-SemiBold.ttf",
    "ui/fonts/Inter-Bold.ttf",
    "ui/fonts/GMPCJKSCUI-Regular.otf",
    "ui/fonts/GMPCJKKRUI-Regular.otf",
];

/// Concatenates the bundled font faces and stores one LZMA blob plus the
/// byte ranges needed to recover each original file at runtime.
fn compress_bundled_fonts() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("no manifest dir")?);
    let mut concatenated = Vec::new();
    let mut segments = String::new();

    for relative_path in FONT_SOURCES {
        let path = manifest_dir.join(relative_path);
        println!("cargo:rerun-if-changed={}", path.display());

        let bytes = fs::read(path)?;
        let _ = writeln!(segments, "    ({}, {}),", concatenated.len(), bytes.len());
        concatenated.extend_from_slice(&bytes);
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("no OUT_DIR")?);
    let options = lzma_rust2::LzmaOptions::with_preset(9);
    let mut encoder = lzma_rust2::LzmaWriter::new_use_header(
        Vec::new(),
        &options,
        Some(concatenated.len() as u64),
    )?;
    encoder.write_all(&concatenated)?;
    fs::write(out_dir.join("bundled_fonts.lzma"), encoder.finish()?)?;

    fs::write(
        out_dir.join("font_segments.rs"),
        format!(
            "const FONT_SEGMENTS: &[(usize, usize)] = &[\n{segments}];\n\
             const FONTS_UNCOMPRESSED_LEN: usize = {};\n",
            concatenated.len()
        ),
    )?;

    Ok(())
}

/// Concatenates every bundled .ftl catalog and stores one LZMA blob plus a
/// segment table in OUT_DIR; the twelve catalogs are ~90% redundant with each
/// other (same keys), so one shared dictionary compresses far better than
/// twelve separate streams. `i18n/mod.rs` decompresses lazily at runtime.
fn compress_locale_catalogs() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("no manifest dir")?);
    let i18n_dir = manifest_dir.join("i18n");
    println!("cargo:rerun-if-changed={}", i18n_dir.display());

    let mut catalogs: Vec<(String, String)> = Vec::new();
    for entry in fs::read_dir(&i18n_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("ftl") {
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or("non-utf8 .ftl file name")?
            .to_owned();
        catalogs.push((id, fs::read_to_string(&path)?));
    }
    catalogs.sort_by(|a, b| a.0.cmp(&b.0));

    let mut concatenated = String::new();
    let mut segments = String::new();
    for (id, source) in &catalogs {
        let _ = writeln!(segments, "    (\"{id}\", {}),", source.len());
        concatenated.push_str(source);
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("no OUT_DIR")?);

    let options = lzma_rust2::LzmaOptions::with_preset(9);
    let mut encoder = lzma_rust2::LzmaWriter::new_use_header(
        Vec::new(),
        &options,
        Some(concatenated.len() as u64),
    )?;
    encoder.write_all(concatenated.as_bytes())?;
    fs::write(out_dir.join("i18n_catalogs.lzma"), encoder.finish()?)?;

    fs::write(
        out_dir.join("i18n_segments.rs"),
        format!(
            "/// (locale file stem, uncompressed byte length) in blob order.\n\
             const CATALOG_SEGMENTS: &[(&str, usize)] = &[\n{segments}];\n\
             const CATALOGS_UNCOMPRESSED_LEN: usize = {};\n",
            concatenated.len()
        ),
    )?;

    Ok(())
}

fn copy_steam_runtime_for_local_build() -> io::Result<()> {
    let Some(runtime_source) = steam_runtime_source_path() else {
        return Ok(());
    };
    println!("cargo:rerun-if-changed={}", runtime_source.display());

    if !runtime_source.exists() {
        return Ok(());
    }

    let Some(target_dir) = target_profile_dir() else {
        return Ok(());
    };

    fs::copy(&runtime_source, target_dir.join(steam_runtime_file_name()))?;
    Ok(())
}

fn steam_runtime_source_path() -> Option<PathBuf> {
    if let Some(runtime_dir) = env::var_os("GMPUBLISHED_STEAM_RUNTIME_DIR") {
        return Some(PathBuf::from(runtime_dir).join(steam_runtime_file_name()));
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR")?);
    let workspace_root = manifest_dir.parent()?.parent()?;
    Some(
        workspace_root
            .join("packaging")
            .join("steam")
            .join("redistributable")
            .join(steam_runtime_platform_dir())
            .join(steam_runtime_file_name()),
    )
}

fn steam_runtime_platform_dir() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

fn steam_runtime_file_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "steam_api64.dll"
    } else if cfg!(target_os = "macos") {
        "libsteam_api.dylib"
    } else {
        "libsteam_api.so"
    }
}

fn target_profile_dir() -> Option<PathBuf> {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR")?);
    let build_dir = out_dir.parent()?.parent()?;
    if build_dir.file_name().and_then(|name| name.to_str()) != Some("build") {
        return None;
    }
    build_dir.parent().map(PathBuf::from)
}
