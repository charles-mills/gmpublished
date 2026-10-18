use std::collections::HashMap;

use crate::model::SurfacePhysics;
use crate::qc::lexer::Lexer;

use super::PhyError;

#[derive(Default)]
struct Definition {
    name: String,
    base: Option<String>,
    density: Option<f32>,
    thickness: Option<f32>,
}

pub fn parse_surface_properties(
    text: &str,
    existing: &[SurfacePhysics],
) -> Result<Vec<SurfacePhysics>, PhyError> {
    let invalid = |detail| PhyError::BadParameter {
        what: "surface properties",
        detail,
    };
    let mut lexer = Lexer::new(Default::default());
    lexer.load_from_memory(text, "surfaceproperties");
    let mut definitions = Vec::new();
    while let Some(name) = lexer.next_token(true).map_err(|e| invalid(e.to_string()))? {
        let open = lexer
            .expect_token(true)
            .map_err(|e| invalid(e.to_string()))?;
        if open.text != "{" {
            return Err(invalid(format!(
                "expected a block for surface {:?}",
                name.text
            )));
        }
        let mut definition = Definition {
            name: name.text,
            ..Default::default()
        };
        loop {
            let key = lexer
                .expect_token(true)
                .map_err(|e| invalid(e.to_string()))?;
            if key.text == "}" {
                break;
            }
            let value = lexer
                .expect_token(true)
                .map_err(|e| invalid(e.to_string()))?;
            if value.text == "{" || value.text == "}" {
                return Err(invalid(format!(
                    "expected a value for surface key {:?}",
                    key.text
                )));
            }
            match key.text.to_ascii_lowercase().as_str() {
                "base" => definition.base = Some(value.text),
                "density" | "thickness" => {
                    let number: f32 = value
                        .text
                        .parse()
                        .map_err(|_| invalid(format!("invalid {} {:?}", key.text, value.text)))?;
                    if !number.is_finite()
                        || (key.text.eq_ignore_ascii_case("density") && number < 0.0)
                    {
                        return Err(invalid(format!("invalid {} {:?}", key.text, value.text)));
                    }
                    if key.text.eq_ignore_ascii_case("density") {
                        definition.density = Some(number);
                    } else {
                        definition.thickness = Some(number);
                    }
                }
                _ => {}
            }
        }
        definitions.push(definition);
    }
    let indices: HashMap<_, _> = definitions
        .iter()
        .enumerate()
        .map(|(i, d)| (d.name.to_ascii_lowercase(), i))
        .collect();
    let mut resolved = existing.to_vec();
    let mut state = vec![0_u8; definitions.len()];
    for index in 0..definitions.len() {
        resolve(index, &definitions, &indices, &mut state, &mut resolved, 0)?;
    }
    Ok(resolved)
}

fn resolve(
    index: usize,
    definitions: &[Definition],
    indices: &HashMap<String, usize>,
    state: &mut [u8],
    resolved: &mut Vec<SurfacePhysics>,
    depth: usize,
) -> Result<(), PhyError> {
    if state[index] == 2 {
        return Ok(());
    }
    if depth >= 128 {
        return Err(PhyError::BadParameter {
            what: "surface properties",
            detail: "surface inheritance exceeds 128 levels".into(),
        });
    }
    if state[index] == 1 {
        return Err(PhyError::BadParameter {
            what: "surface properties",
            detail: format!("surface inheritance cycle at {:?}", definitions[index].name),
        });
    }
    state[index] = 1;
    let definition = &definitions[index];
    let base = definition.base.as_deref().unwrap_or("default");
    if let Some(&base_index) = indices.get(&base.to_ascii_lowercase()) {
        if base_index != index {
            resolve(base_index, definitions, indices, state, resolved, depth + 1)?;
        } else if definition.base.is_some() {
            return Err(PhyError::BadParameter {
                what: "surface properties",
                detail: format!("surface {:?} inherits itself", definition.name),
            });
        }
    }
    let inherited = resolved
        .iter()
        .rev()
        .find(|p| p.name.eq_ignore_ascii_case(base));
    if inherited.is_none() && definition.base.is_some() && !base.eq_ignore_ascii_case("default") {
        return Err(PhyError::BadParameter {
            what: "surface properties",
            detail: format!("unknown base surface {base:?}"),
        });
    }
    let property = SurfacePhysics {
        name: definition.name.clone(),
        density: definition
            .density
            .unwrap_or_else(|| inherited.map_or(2000.0, |p| p.density)),
        thickness: definition
            .thickness
            .unwrap_or_else(|| inherited.map_or(0.0, |p| p.thickness)),
    };
    if let Some(old) = resolved
        .iter_mut()
        .find(|p| p.name.eq_ignore_ascii_case(&property.name))
    {
        *old = property;
    } else {
        resolved.push(property);
    }
    state[index] = 2;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_resolve_default_and_explicit_bases_across_files() {
        let initial = parse_surface_properties(
            r#"
            "default" { "density" "2000" "friction" "0.8" }
            metal { density 2700 thickness 0.2 }
        "#,
            &[],
        )
        .unwrap();
        let merged = parse_surface_properties(
            r#"
            custom { base metal_heavy thickness 0.5 impacthard "Metal.Impact" }
            metal_heavy { base metal density 7500 }
            empty { }
        "#,
            &initial,
        )
        .unwrap();
        let custom = merged.iter().find(|p| p.name == "custom").unwrap();
        assert_eq!(custom.density, 7500.0);
        assert_eq!(custom.thickness, 0.5);
        let heavy = merged.iter().find(|p| p.name == "metal_heavy").unwrap();
        assert_eq!(heavy.thickness, 0.2);
        let empty = merged.iter().find(|p| p.name == "empty").unwrap();
        assert_eq!(empty.density, 2000.0);
        assert_eq!(empty.thickness, 0.0);
    }

    #[test]
    fn invalid_surface_mass_and_inheritance_are_rejected() {
        for text in [
            "bad { density NaN }",
            "bad { density -1 }",
            "bad { thickness inf }",
            "bad { base absent }",
            "bad { base bad }",
            "a { base b } b { base a }",
            "bad { density }",
            "bad { density 1000",
            "bad density 1000",
        ] {
            assert!(parse_surface_properties(text, &[]).is_err(), "{text}");
        }
    }

    #[test]
    fn deep_surface_inheritance_returns_an_error() {
        let text: String = (0..129)
            .map(|i| format!("surface_{i} {{ base surface_{} }}\n", i + 1))
            .chain(std::iter::once("surface_129 { }".into()))
            .collect();
        assert!(
            parse_surface_properties(&text, &[])
                .unwrap_err()
                .to_string()
                .contains("128 levels")
        );
    }
}
