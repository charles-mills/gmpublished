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
