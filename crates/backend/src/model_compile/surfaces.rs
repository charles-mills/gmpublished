use super::{ModelCompileError, ModelCompileRequest, SurfacePhysics, load_description};
use vformats::keyvalues::{self, KvValue};

pub fn load_surface_properties(
    request: &mut ModelCompileRequest,
    read: impl FnMut(&str) -> Option<Vec<u8>>,
) -> Result<(), ModelCompileError> {
    let source =
        std::fs::canonicalize(&request.source).map_err(|source| ModelCompileError::Io {
            path: request.source.clone(),
            source,
        })?;
    let base = source
        .parent()
        .ok_or_else(|| ModelCompileError::InvalidOption("source must be a file".into()))?;
    if load_description(request, &source, base)?.physics.auto_mass {
        request.surface_properties = read_surface_properties(read)?;
    }
    Ok(())
}

fn read_surface_properties(
    mut read: impl FnMut(&str) -> Option<Vec<u8>>,
) -> Result<Vec<SurfacePhysics>, ModelCompileError> {
    let invalid = |message| ModelCompileError::Description(message);
    let manifest = read("scripts/surfaceproperties_manifest.txt");
    let required = manifest.is_some();
    let files = if let Some(bytes) = manifest {
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(invalid("Surface property manifest exceeds 8 MiB".into()));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|error| invalid(format!("Invalid surface property manifest: {error}")))?;
        let document = keyvalues::parse(text, &vformats::Limits::default())
            .map_err(|error| invalid(format!("Invalid surface property manifest: {error}")))?;
        let block = document
            .pairs
            .iter()
            .find_map(|pair| {
                if pair.key.eq_ignore_ascii_case("surfaceproperties_manifest")
                    && let KvValue::Block(block) = &pair.value
                {
                    return Some(block);
                }
                None
            })
            .ok_or_else(|| {
                invalid("Surface property manifest has no surfaceproperties_manifest block".into())
            })?;
        let mut files = Vec::new();
        for pair in &block.pairs {
            if pair.key.eq_ignore_ascii_case("file") {
                let KvValue::String(path) = &pair.value else {
                    return Err(invalid(
                        "Surface property manifest file must be a path".into(),
                    ));
                };
                let path = path.replace('\\', "/");
                if path.starts_with('/')
                    || path.contains(':')
                    || path.split('/').any(|component| component == "..")
                {
                    return Err(invalid(
                        "Surface property manifest path escapes game content".into(),
                    ));
                }
                files.push(path);
                if files.len() > 128 {
                    return Err(invalid(
                        "Surface property manifest exceeds 128 files".into(),
                    ));
                }
            }
        }
        files
    } else {
        vec!["scripts/surfaceproperties.txt".into()]
    };
    let mut properties = Vec::new();
    for file in files {
        let bytes = match read(&file) {
            Some(bytes) => bytes,
            None if !required => return Ok(Vec::new()),
            None => {
                return Err(invalid(format!(
                    "Surface property file was not found: {file}"
                )));
            }
        };
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(invalid(format!(
                "Surface property file exceeds 8 MiB: {file}"
            )));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|error| invalid(format!("Invalid surface property file {file}: {error}")))?;
        properties = mdlc::phy::parse_surface_properties(text, &properties)
            .map_err(|error| invalid(format!("Invalid surface property file {file}: {error}")))?;
    }
    Ok(properties)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_resolves_density_and_thickness_across_files() {
        let properties = read_surface_properties(|path| match path {
            "scripts/surfaceproperties_manifest.txt" => Some(br#""surfaceproperties_manifest" { "file" "scripts/base.txt" "file" "scripts/custom.txt" }"#.to_vec()),
            "scripts/base.txt" => Some(br#""default" { "density" "2000" } "metal" { "density" "7800" "thickness" "0.1" }"#.to_vec()),
            "scripts/custom.txt" => Some(br#""paintedmetal" { "base" "metal" "thickness" "0.25" }"#.to_vec()),
            _ => None,
        }).unwrap();
        let metal = properties
            .iter()
            .find(|surface| surface.name == "paintedmetal")
            .unwrap();
        assert_eq!(metal.density, 7800.0);
        assert_eq!(metal.thickness, 0.25);
    }

    #[test]
    fn missing_or_escaping_manifest_dependencies_fail() {
        for file in ["scripts/missing.txt", "../outside.txt"] {
            let manifest = format!("surfaceproperties_manifest {{ file \"{file}\" }}");
            assert!(
                read_surface_properties(|path| (path == "scripts/surfaceproperties_manifest.txt")
                    .then(|| manifest.as_bytes().to_vec()))
                .is_err()
            );
        }
    }
}
