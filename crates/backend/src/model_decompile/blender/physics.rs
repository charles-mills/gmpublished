use std::{path::Path, sync::atomic::AtomicBool};

use mdlc::bone_math;
use serde_json::json;

use super::super::{LoadedModel, physics, read_input, sibling};
use super::{DecompileError, Document, check_cancelled, invalid, position};

pub(super) fn write(
    output: &mut Document,
    loaded: &LoadedModel,
    path: &Path,
    cancelled: &AtomicBool,
) -> Result<(), DecompileError> {
    let Ok(path) = sibling(path, "phy") else {
        return Ok(());
    };
    let raw = read_input(&path)?;
    let collision = physics::collision_geometry(&loaded.model, &raw, cancelled)?;
    let metadata = json!({
        "sourceUnits": "inches",
        "sourcePhysicsKeyvalues": collision.metadata,
        "sourceCollisionBindings": collision.meshes.iter().map(|mesh| json!({
            "solid":mesh.solid,"piece":mesh.piece,"bone":mesh.bone
        })).collect::<Vec<_>>(),
    });
    output.extras["sourcePhysics"] = metadata.clone();
    let skin = if collision.meshes.iter().any(|mesh| mesh.bone.is_some()) {
        let matrices = collision
            .worlds
            .iter()
            .map(|world| {
                let inverse = bone_math::invert(world);
                let axes = [1, 2, 0];
                let mut matrix = [0.0; 16];
                for row in 0..3 {
                    for column in 0..3 {
                        matrix[column * 4 + row] = inverse[axes[row] * 4 + axes[column]];
                    }
                    matrix[12 + row] = inverse[axes[row] * 4 + 3] * super::INCHES_TO_METERS;
                }
                matrix[15] = 1.0;
                matrix
            })
            .collect::<Vec<_>>();
        let accessor = output.floats(&matrices, "MAT4", false, None)?;
        let index = output.skins.len();
        output.skins.push(json!({"name":"Collision","inverseBindMatrices":accessor,"skeleton":0,"joints":(1..=loaded.model.bones.len()).collect::<Vec<_>>() }));
        Some(index)
    } else {
        None
    };
    let palette = [
        [0.08, 0.65, 1.0, 0.35],
        [1.0, 0.45, 0.08, 0.35],
        [0.25, 0.9, 0.4, 0.35],
        [0.8, 0.25, 1.0, 0.35],
    ];
    let materials = palette.map(|color| {
        let index = output.materials.len();
        output.materials.push(json!({"name":"Collision","pbrMetallicRoughness":{"baseColorFactor":color,"metallicFactor":0.0,"roughnessFactor":1.0},"alphaMode":"BLEND"}));
        index
    });
    let mut nodes = Vec::new();
    for mesh in &collision.meshes {
        check_cancelled(cancelled)?;
        let name = format!(
            "collision / {} / Piece {}",
            mesh.bone
                .map_or("model", |bone| loaded.model.bones[bone].name.as_str()),
            mesh.piece + 1
        );
        let positions = mesh
            .vertices
            .iter()
            .copied()
            .map(position)
            .collect::<Vec<_>>();
        let mut attributes = json!({"POSITION":output.floats(&positions,"VEC3",true,Some(34962))?});
        if let Some(bone) = mesh.bone {
            let bone =
                u16::try_from(bone).map_err(|_| invalid("A collision bone exceeds glTF limits"))?;
            let joints = positions
                .iter()
                .flat_map(|_| [bone, 0, 0, 0].into_iter().flat_map(u16::to_le_bytes))
                .collect::<Vec<_>>();
            attributes["JOINTS_0"] =
                json!(output.integers(&joints, 5123, positions.len(), "VEC4", 34962)?);
            attributes["WEIGHTS_0"] = json!(output.floats(
                &vec![[1.0, 0.0, 0.0, 0.0]; positions.len()],
                "VEC4",
                false,
                Some(34962)
            )?);
        }
        let index_bytes = mesh
            .indices
            .iter()
            .flat_map(|index| index.to_le_bytes())
            .collect::<Vec<_>>();
        let primitive = json!({"attributes":attributes,"indices":output.integers(&index_bytes,5125,mesh.indices.len(),"SCALAR",34963)?,"mode":4,"material":materials[nodes.len()%materials.len()]});
        let mesh_index = output.meshes.len();
        output
            .meshes
            .push(json!({"name":name,"primitives":[primitive]}));
        let mut node = json!({"name":name,"mesh":mesh_index,"extras":{"gmpublishedCollision":true,"sourceCollision":true,"sourceCollisionSolid":mesh.solid,"sourceCollisionPiece":mesh.piece,"sourceCollisionBone":mesh.bone}});
        if mesh.bone.is_some() {
            node["skin"] = json!(skin.unwrap());
        }
        nodes.push(output.nodes.len());
        output.nodes.push(node);
    }
    let root = output.nodes.len();
    output.nodes.push(json!({"name":"Collision","children":nodes,"extras":{"gmpublishedCollision":true,"sourceCollision":true,"gmpublishedSourceMetadata":serde_json::to_string(&metadata).map_err(|error|invalid(error.to_string()))?}}));
    output
        .scenes
        .push(json!({"name":"Collision","nodes":[0,root],"extras":{"sourceCollision":true}}));
    if collision.has_constraints {
        output.warnings.push("Collision meshes remain bound to their model bones. Source ragdoll constraints are preserved as custom data; Blender physics constraints are not generated.".into());
    }
    Ok(())
}
