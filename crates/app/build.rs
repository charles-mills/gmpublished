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
    compress_bundled_catalogs()?;
    Ok(())
}

/// Locale ids, in the order [`CATALOGS`](../src/i18n/mod.rs) declares them.
/// The generated table is keyed by id rather than position, so this order is
/// cosmetic — unlike `FONT_SOURCES`, adding or reordering an entry here cannot
/// silently mis-map a catalog.
const CATALOG_LOCALE_IDS: &[&str] = &[
    "en", "de", "es", "fr", "kr", "nl", "pl", "pt-BR", "ru", "tr", "uk", "zh-cn",
];

/// Concatenates the Fluent catalogs and stores one LZMA blob plus an
/// id-keyed table of the byte ranges needed to recover each one at runtime.
///
/// The catalogs are ~232 KiB of highly repetitive UTF-8 and compress by about
/// 80%, which is worth having in a binary that ships as an ~8 MB AppImage.
fn compress_bundled_catalogs() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("no manifest dir")?);
    let mut concatenated = Vec::new();
    let mut segments = String::new();

    for id in CATALOG_LOCALE_IDS {
        let path = manifest_dir.join("i18n").join(format!("{id}.ftl"));
        println!("cargo:rerun-if-changed={}", path.display());

        let bytes = fs::read(&path)?;
        let _ = writeln!(
            segments,
            "    ({:?}, {}, {}),",
            id,
            concatenated.len(),
            bytes.len()
        );
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
    fs::write(out_dir.join("bundled_catalogs.lzma"), encoder.finish()?)?;

    fs::write(
        out_dir.join("catalog_segments.rs"),
        format!(
            "const CATALOG_SEGMENTS: &[(&str, usize, usize)] = &[\n{segments}];\n\
             const CATALOGS_UNCOMPRESSED_LEN: usize = {};\n",
            concatenated.len()
        ),
    )?;

    Ok(())
}

/// The bundled faces, and the constant each is reachable by at runtime.
///
/// The generated constants are what `assets.rs` uses, so reordering this list
/// moves the *names* with the faces rather than silently repointing a
/// hardcoded index at a different font.
const FONT_SOURCES: &[(&str, &str)] = &[
    ("INTER_REGULAR", "ui/fonts/Inter-Regular.ttf"),
    ("INTER_SEMI_BOLD", "ui/fonts/Inter-SemiBold.ttf"),
    ("INTER_BOLD", "ui/fonts/Inter-Bold.ttf"),
    ("CJK_SC_REGULAR", "ui/fonts/GMPCJKSCUI-Regular.otf"),
    ("CJK_KR_REGULAR", "ui/fonts/GMPCJKKRUI-Regular.otf"),
];

/// Concatenates the bundled font faces and stores one LZMA blob plus the
/// byte ranges needed to recover each original file at runtime.
fn compress_bundled_fonts() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("no manifest dir")?);
    let mut concatenated = Vec::new();
    let mut segments = String::new();

    let mut constants = String::new();
    for (index, (name, relative_path)) in FONT_SOURCES.iter().enumerate() {
        let path = manifest_dir.join(relative_path);
        println!("cargo:rerun-if-changed={}", path.display());

        let bytes = fs::read(path)?;
        let _ = writeln!(
            segments,
            "    FontSegment {{ start: {}, len: {} }},",
            concatenated.len(),
            bytes.len()
        );
        let _ = writeln!(constants, "pub const {name}: usize = {index};");
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
            "/// Where one bundled face sits in the decompressed blob. Named\n\
             /// fields because `start` and `len` are both `usize` and a swap\n\
             /// would yield a valid-looking range over the wrong bytes.\n\
             struct FontSegment {{\n    start: usize,\n    len: usize,\n}}\n\n\
             const FONT_SEGMENTS: &[FontSegment] = &[\n{segments}];\n\
             pub const FONT_COUNT: usize = {};\n\
             const FONTS_UNCOMPRESSED_LEN: usize = {};\n\n{constants}",
            FONT_SOURCES.len(),
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
