use std::{collections::HashSet, fmt::Write as _};

use mdlc::{bone_math, smd::SmdPose};
use vformats::{
    keyvalues::{KvDocument, KvPair, KvValue},
    mdl::Mdl,
    phy,
};

use super::{DecompileError, ExportFiles, invalid, quoted, skeleton_smd};

#[derive(Default)]
pub(super) struct PhysicsReport {
    pub preserved: bool,
    pub warnings: Vec<String>,
}

pub(super) fn write_physics(
    qc: &mut String,
    export: &mut ExportFiles<'_>,
    _raw_mdl: &[u8],
    raw_phy: Option<&[u8]>,
    model: &Mdl,
    first_frame: Option<&[SmdPose]>,
) -> Result<PhysicsReport, DecompileError> {
    let Some(raw_phy) = raw_phy else {
        return Ok(PhysicsReport::default());
    };
    let limits = vformats::Limits {
        max_input_bytes: super::MAX_INPUT_BYTES,
        max_entries: 65_536,
        ..vformats::Limits::default()
    };
    let physics = phy::parse(raw_phy, &limits)
        .map_err(|error| invalid(format!("Cannot read collision data: {error}")))?;
    if physics.header.mdl_checksum != model.checksum {
        return Err(invalid("The MDL and PHY checksums do not match"));
    }
    if physics.solids.is_empty() {
        return Err(invalid("The PHY contains no collision solids"));
    }
    if physics.stats.declared_solids != physics.solids.len()
        || !physics.stats.skip_reasons.is_empty()
    {
        return Err(invalid(
            "The collision reader could not preserve all PHY geometry",
        ));
    }
    let text = physics
        .text
        .ok_or_else(|| invalid("The PHY has no solid metadata"))?;
    validate_collision_text(text.bytes)?;
    let metadata = text
        .keyvalues(&limits)
        .ok_or_else(|| invalid("Invalid PHY keyvalues"))?;
    let solids: Vec<_> = metadata.blocks("solid").collect();
    if solids.len() != physics.solids.len() {
        return Err(invalid(
            "The PHY solid metadata does not match its geometry",
        ));
    }
    let bindings = solid_bindings(raw_phy, physics.solids.len())?;
    let joints = bindings.iter().any(Option::is_some);
    if joints && bindings.iter().any(Option::is_none) {
        return Err(invalid("The PHY mixes bound and unbound collision solids"));
    }
    if !joints && physics.solids.len() != 1 {
        return Err(invalid(
            "Multiple unbound PHY solids cannot be represented by collisionmodel",
        ));
    }
    for (index, solid) in solids.iter().enumerate() {
        if integer(solid, "index")? != index {
            return Err(invalid("The PHY solid indices are not sequential"));
        }
        if let Some(bone) = bindings[index] {
            let bone = model
                .bones
                .get(bone)
                .ok_or_else(|| invalid("A collision solid references an absent model bone"))?;
            if !string(solid, "name")?.eq_ignore_ascii_case(&bone.name) {
                return Err(invalid("A PHY solid name does not match its bound bone"));
            }
        }
    }
    let max_pieces = physics
        .solids
        .iter()
        .map(|solid| solid.ledges.len())
        .max()
        .unwrap_or(0);
    if max_pieces > 1024 {
        return Err(DecompileError::SizeLimit);
    }
    let edit = metadata
        .blocks("editparams")
        .next()
        .ok_or_else(|| invalid("The PHY has no editparams metadata"))?;
    let mass = number(edit, "totalmass")?;
    if mass <= 0.0 {
        return Err(invalid("Collision mass must be positive"));
    }
    let mass_biases = collision_mass_biases(&solids, mass)?;
    let mut commands = String::new();
    if !joints {
        writeln!(
            commands,
            "$surfaceprop {}",
            quoted(string(solids[0], "surfaceprop")?)?
        )
        .unwrap();
    }
    write!(
        commands,
        "$collision{} \"collision.smd\"\n{{\n    $mass {mass}\n",
        if joints { "joints" } else { "model" }
    )
    .unwrap();
    if let Some(root) = edit.get_str("rootname").filter(|name| !name.is_empty()) {
        if !model
            .bones
            .iter()
            .any(|bone| bone.name.eq_ignore_ascii_case(root))
        {
            return Err(invalid("The collision root bone is absent from the model"));
        }
        writeln!(commands, "    $rootbone {}", quoted(root)?).unwrap();
    }
    if !joints && (physics.solids[0].ledges.len() > 1 || edit.get_str("concave") == Some("1")) {
        commands.push_str("    $concave\n");
    }
    if joints && max_pieces > 1 {
        commands.push_str("    $concaveperjoint\n");
    }
    if max_pieces > 20 {
        writeln!(commands, "    $maxconvexpieces {max_pieces}").unwrap();
    }
    for (index, solid) in solids.iter().enumerate() {
        let name = string(solid, "name")?;
        let surface = string(solid, "surfaceprop")?;
        if joints {
            writeln!(
                commands,
                "    $jointsurfaceprop {} {}",
                quoted(name)?,
                quoted(surface)?
            )
            .unwrap();
        }
        for (key, command) in [
            ("damping", "damping"),
            ("rotdamping", "rotdamping"),
            ("inertia", "inertia"),
        ] {
            let value = number(solid, key)?;
            if value < 0.0 {
                return Err(invalid("A collision parameter is negative"));
            }
            if joints {
                writeln!(commands, "    $joint{command} {} {value}", quoted(name)?).unwrap();
            } else {
                writeln!(commands, "    ${command} {value}").unwrap();
            }
        }
        if let Some(drag) = optional_number(solid, "drag")? {
            if solids
                .iter()
                .any(|other| optional_number(other, "drag").ok().flatten() != Some(drag))
            {
                return Err(invalid(
                    "Per-solid drag overrides cannot be represented in QC",
                ));
            }
            if index == 0 {
                writeln!(commands, "    $drag {drag}").unwrap();
            }
        }
        if joints {
            let bias = mass_biases[index];
            writeln!(commands, "    $jointmassbias {} {bias}", quoted(name)?).unwrap();
        }
    }
    if joints {
        write_constraints(&mut commands, &metadata, &solids, model, &bindings)?;
    } else if metadata.blocks("ragdollconstraint").next().is_some()
        || metadata.blocks("collisionrules").next().is_some()
    {
        return Err(invalid(
            "An unbound collision solid contains ragdoll constraints",
        ));
    }
    if let Some(animated) = metadata.blocks("animatedfriction").next() {
        let min = number(animated, "animfrictionmin")?;
        let max = number(animated, "animfrictionmax")?;
        if min.fract() != 0.0
            || max.fract() != 0.0
            || min < i32::MIN as f32
            || max > i32::MAX as f32
        {
            return Err(invalid(
                "Animated collision friction must use integer bounds",
            ));
        }
        writeln!(
            commands,
            "    $animatedfriction {} {} {} {} {}",
            min as i32,
            max as i32,
            number(animated, "animfrictiontimehold")?,
            number(animated, "animfrictiontimeout")?,
            number(animated, "animfrictiontimein")?
        )
        .unwrap();
    }
    commands.push_str("}\n");
    let mut geometry = skeleton_smd(model)?;
    if !joints
        && model.flags & vformats::mdl::FLAG_STATIC_PROP == 0
        && let Some(frame) = first_frame
    {
        geometry = collision_skeleton(model, frame)?;
    }
    geometry.push_str("triangles\n");
    let rest = model_worlds(model)?;
    let material = model
        .textures
        .first()
        .ok_or_else(|| invalid("The model has no collision material reference"))?;
    super::validate_material(material)?;
    for (solid_index, solid) in physics.solids.iter().enumerate() {
        for (piece, ledge) in solid.ledges.iter().enumerate() {
            // Distinct smooth normals keep touching convex pieces separate during $concave welding.
            let height = 1.0 - 2.0 * (piece as f32 + 0.5) / solid.ledges.len().max(1) as f32;
            let radius = (1.0 - height * height).sqrt();
            let angle = piece as f32 * std::f32::consts::PI * (3.0 - 5.0f32.sqrt());
            let normal = [radius * angle.cos(), radius * angle.sin(), height];
            for triangle in &ledge.triangles {
                writeln!(geometry, "{material}").unwrap();
                for vertex in triangle {
                    let raw = *ledge.ivp_vertices.get(*vertex).ok_or_else(|| {
                        invalid("A collision triangle references an absent vertex")
                    })?;
                    // IVP meters -> compiler source basis: inverse of (-y, -z, x) * 0.0254.
                    let mut position = [raw[2] / 0.0254, -raw[0] / 0.0254, -raw[1] / 0.0254];
                    let bone = bindings[solid_index].unwrap_or(0);
                    if joints {
                        position = transform(&rest[bone], position);
                    }
                    writeln!(
                        geometry,
                        "{bone} {} {} {} {} {} {} 0 0 1 {bone} 1",
                        position[0], position[1], position[2], normal[0], normal[1], normal[2]
                    )
                    .unwrap();
                }
                if geometry.len() > super::MAX_OUTPUT_BYTES {
                    return Err(DecompileError::SizeLimit);
                }
            }
        }
    }
    geometry.push_str("end\n");
    let mut extra = String::new();
    for pair in &metadata.pairs {
        if ![
            "solid",
            "ragdollconstraint",
            "collisionrules",
            "animatedfriction",
            "editparams",
        ]
        .iter()
        .any(|known| pair.key.eq_ignore_ascii_case(known))
        {
            write_collision_pair(&mut extra, pair, 0)?;
        }
    }
    if !extra.is_empty() {
        commands.push_str("$collisiontext\n{\n");
        commands.push_str(&extra);
        commands.push_str("}\n");
    }
    export.write("collision.smd", geometry.as_bytes())?;
    qc.push_str(&commands);
    Ok(PhysicsReport {
        preserved: true,
        warnings: Vec::new(),
    })
}

fn write_collision_pair(
    text: &mut String,
    pair: &KvPair<'_>,
    depth: usize,
) -> Result<(), DecompileError> {
    if depth > 64 || text.len() > 1_048_576 {
        return Err(DecompileError::SizeLimit);
    }
    let quote = |value: &str| {
        if matches!(value, "{" | "}") || value.chars().any(char::is_control) {
            return Err(invalid("Collision text cannot be represented safely in QC"));
        }
        quoted(value)
    };
    write!(text, "{} ", quote(&pair.key)?).unwrap();
    match &pair.value {
        KvValue::String(value) => writeln!(text, "{}", quote(value)?).unwrap(),
        KvValue::Block(block) => {
            text.push_str("{\n");
            for child in &block.pairs {
                write_collision_pair(text, child, depth + 1)?;
            }
            text.push_str("}\n");
        }
    }
    Ok(())
}

fn validate_collision_text(bytes: &[u8]) -> Result<(), DecompileError> {
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    if bytes.len() > 1_048_576 {
        return Err(DecompileError::SizeLimit);
    }
    if bytes.contains(&0) {
        return Err(invalid("Collision text contains embedded nulls"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("Collision text is not UTF-8"))?;
    let tokens = super::metadata::keyvalue_tokens(text)?;
    let mut levels = vec![true];
    for token in tokens {
        match token.as_str() {
            "{" => {
                if *levels.last().unwrap() || levels.len() > 64 {
                    return Err(invalid("Invalid collision text block"));
                }
                levels.push(true);
            }
            "}" => {
                if levels.len() == 1 || !levels.pop().unwrap() {
                    return Err(invalid("Unbalanced collision text block"));
                }
                *levels.last_mut().unwrap() = true;
            }
            _ => {
                let key = levels.last_mut().unwrap();
                *key = !*key;
            }
        }
    }
    if levels != [true] {
        return Err(invalid("Truncated collision text block or value"));
    }
    Ok(())
}

fn collision_skeleton(model: &Mdl, frame: &[SmdPose]) -> Result<String, DecompileError> {
    if frame.len() != model.bones.len() {
        return Err(invalid(
            "The collision reference pose does not match the skeleton",
        ));
    }
    let mut text = String::from("version 1\nnodes\n");
    for (index, bone) in model.bones.iter().enumerate() {
        writeln!(text, "{index} {} {}", quoted(&bone.name)?, bone.parent).unwrap();
    }
    text.push_str("end\nskeleton\ntime 0\n");
    for (index, pose) in frame.iter().enumerate() {
        if pose.bone != index as i32
            || !pose
                .position
                .iter()
                .chain(&pose.rotation)
                .all(|value| value.is_finite())
        {
            return Err(invalid("Invalid collision reference pose"));
        }
        writeln!(
            text,
            "{index} {} {} {} {} {} {}",
            pose.position[0],
            pose.position[1],
            pose.position[2],
            pose.rotation[0],
            pose.rotation[1],
            pose.rotation[2]
        )
        .unwrap();
    }
    text.push_str("end\n");
    Ok(text)
}

fn model_worlds(model: &Mdl) -> Result<Vec<bone_math::Matrix3x4>, DecompileError> {
    let mut worlds = vec![None; model.bones.len()];
    for index in 0..model.bones.len() {
        let mut chain = Vec::new();
        let mut current = index;
        let mut seen = HashSet::new();
        while worlds[current].is_none() {
            if !seen.insert(current) {
                return Err(invalid("The collision skeleton contains a parent cycle"));
            }
            chain.push(current);
            let parent = model.bones[current].parent;
            if parent == -1 {
                break;
            }
            current = usize::try_from(parent)
                .ok()
                .filter(|parent| *parent < model.bones.len())
                .ok_or_else(|| invalid("The collision skeleton references an absent parent"))?;
        }
        for current in chain.into_iter().rev() {
            let bone = &model.bones[current];
            let local = bone_math::local_transform(bone.position, bone.rotation);
            worlds[current] = Some(if bone.parent == -1 {
                local
            } else {
                bone_math::concat(&worlds[bone.parent as usize].unwrap(), &local)
            });
        }
    }
    Ok(worlds.into_iter().map(Option::unwrap).collect())
}

fn collision_mass_biases(
    solids: &[&KvDocument<'_>],
    mass: f32,
) -> Result<Vec<f32>, DecompileError> {
    let mut biases = Vec::with_capacity(solids.len());
    let mut volumes = Vec::with_capacity(solids.len());
    let mut masses = Vec::with_capacity(solids.len());
    for solid in solids {
        let bias = optional_number(solid, "massbias")?.unwrap_or(1.0);
        let volume = number(solid, "volume")?;
        let mass = number(solid, "mass")?;
        if bias < 0.0 || volume <= 0.0 || mass < 1.0 {
            return Err(invalid("Invalid collision solid mass or volume"));
        }
        biases.push(bias);
        volumes.push(volume);
        masses.push(mass);
    }
    let matches = |biases: &[f32]| {
        let total = volumes
            .iter()
            .zip(biases)
            .map(|(volume, bias)| f64::from(*volume) * f64::from(*bias))
            .sum::<f64>();
        total > 0.0
            && total.is_finite()
            && volumes
                .iter()
                .zip(biases)
                .zip(&masses)
                .all(|((volume, bias), original)| {
                    let predicted = ((f64::from(*volume) * f64::from(*bias) / total
                        * f64::from(mass)) as f32)
                        .max(1.0);
                    (predicted - original).abs() <= 0.001f32.max(original.abs() * 0.00001)
                })
    };
    if !matches(&biases) {
        biases = masses
            .iter()
            .zip(&volumes)
            .map(|(mass, volume)| mass / volume)
            .collect();
        if !biases.iter().all(|bias| bias.is_finite()) || !matches(&biases) {
            return Err(invalid(
                "Collision solid masses cannot be reproduced from the stored total mass",
            ));
        }
    }
    Ok(biases)
}

fn transform(matrix: &bone_math::Matrix3x4, point: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|axis| {
        let row = axis * 4;
        matrix[row] * point[0]
            + matrix[row + 1] * point[1]
            + matrix[row + 2] * point[2]
            + matrix[row + 3]
    })
}

fn write_constraints(
    qc: &mut String,
    metadata: &KvDocument<'_>,
    solids: &[&KvDocument<'_>],
    model: &Mdl,
    bindings: &[Option<usize>],
) -> Result<(), DecompileError> {
    for (index, solid) in solids.iter().enumerate() {
        let mut parent = model.bones[bindings[index].unwrap()].parent;
        let mut expected = None;
        while parent >= 0 {
            if let Some(other) = bindings
                .iter()
                .position(|bone| *bone == Some(parent as usize))
            {
                expected = Some(other);
                break;
            }
            parent = model.bones[parent as usize].parent;
        }
        let actual = solid
            .get_str("parent")
            .map(|name| {
                solids
                    .iter()
                    .position(|other| {
                        other
                            .get_str("name")
                            .is_some_and(|other| other.eq_ignore_ascii_case(name))
                    })
                    .ok_or_else(|| invalid("A collision parent solid is absent"))
            })
            .transpose()?;
        // Shipping ragdolls can name the root solid itself as its parent without a constraint.
        let actual = actual.filter(|parent| *parent != index);
        if actual != expected {
            return Err(invalid(
                "Collision parenting differs from the model skeleton and cannot be reconstructed safely",
            ));
        }
    }
    let mut children = HashSet::new();
    for constraint in metadata.blocks("ragdollconstraint") {
        let parent = integer(constraint, "parent")?;
        let child = integer(constraint, "child")?;
        let solid = solids
            .get(child)
            .ok_or_else(|| invalid("A ragdoll constraint references an absent child"))?;
        let parent_solid = solids
            .get(parent)
            .ok_or_else(|| invalid("A ragdoll constraint references an absent parent"))?;
        if !children.insert(child)
            || solid.get_str("parent").is_none_or(|name| {
                parent_solid
                    .get_str("name")
                    .is_none_or(|parent| !name.eq_ignore_ascii_case(parent))
            })
        {
            return Err(invalid("A ragdoll constraint has inconsistent parenting"));
        }
        for axis in ["x", "y", "z"] {
            let min = number(constraint, &format!("{axis}min"))?;
            let max = number(constraint, &format!("{axis}max"))?;
            let friction = number(constraint, &format!("{axis}friction"))? * 5.0;
            if min > max || friction < 0.0 || !friction.is_finite() {
                return Err(invalid("Invalid ragdoll joint limits"));
            }
            writeln!(
                qc,
                "    $jointconstrain {} {axis} limit {min} {max} {friction}",
                quoted(string(solid, "name")?)?
            )
            .unwrap();
        }
    }
    if solids.iter().enumerate().any(|(index, solid)| {
        solid.get_str("parent").is_some_and(|parent| {
            solid
                .get_str("name")
                .is_none_or(|name| !name.eq_ignore_ascii_case(parent))
        }) && !children.contains(&index)
    }) {
        return Err(invalid("A ragdoll joint is missing its constraints"));
    }
    for rules in metadata.blocks("collisionrules") {
        if rules.get_str("selfcollisions") == Some("0") {
            qc.push_str("    $noselfcollisions\n");
            continue;
        }
        for pair in rules
            .pairs
            .iter()
            .filter(|pair| pair.key.eq_ignore_ascii_case("collisionpair"))
        {
            let (a, b) = pair
                .value
                .as_str()
                .and_then(|pair| pair.split_once(','))
                .ok_or_else(|| invalid("Invalid collision pair"))?;
            let a = a
                .trim()
                .parse::<usize>()
                .ok()
                .and_then(|index| solids.get(index))
                .ok_or_else(|| invalid("Invalid collision pair index"))?;
            let b = b
                .trim()
                .parse::<usize>()
                .ok()
                .and_then(|index| solids.get(index))
                .ok_or_else(|| invalid("Invalid collision pair index"))?;
            writeln!(
                qc,
                "    $jointcollide {} {}",
                quoted(string(a, "name")?)?,
                quoted(string(b, "name")?)?
            )
            .unwrap();
        }
    }
    Ok(())
}

fn string<'a>(block: &'a KvDocument<'_>, name: &str) -> Result<&'a str, DecompileError> {
    block
        .get_str(name)
        .ok_or_else(|| invalid(format!("PHY metadata is missing {name}")))
}
fn number(block: &KvDocument<'_>, name: &str) -> Result<f32, DecompileError> {
    optional_number(block, name)?.ok_or_else(|| invalid(format!("PHY metadata is missing {name}")))
}
fn optional_number(block: &KvDocument<'_>, name: &str) -> Result<Option<f32>, DecompileError> {
    block
        .get_str(name)
        .map(|value| {
            value
                .parse::<f32>()
                .ok()
                .filter(|value| value.is_finite())
                .ok_or_else(|| invalid(format!("Invalid PHY metadata number: {name}")))
        })
        .transpose()
}
fn integer(block: &KvDocument<'_>, name: &str) -> Result<usize, DecompileError> {
    string(block, name)?
        .parse()
        .map_err(|_| invalid(format!("Invalid PHY metadata index: {name}")))
}

fn solid_bindings(raw: &[u8], solid_count: usize) -> Result<Vec<Option<usize>>, DecompileError> {
    let read = |at: usize| -> Result<i32, DecompileError> {
        Ok(i32::from_le_bytes(
            raw.get(at..at.checked_add(4).ok_or(DecompileError::SizeLimit)?)
                .ok_or_else(|| invalid("Truncated PHY binding data"))?
                .try_into()
                .unwrap(),
        ))
    };
    let positive =
        |value: i32| usize::try_from(value).map_err(|_| invalid("Negative PHY binding offset"));
    let mut base = positive(read(0)?)?;
    let mut bindings = Vec::with_capacity(solid_count);
    for _ in 0..solid_count {
        let size = positive(read(base)?)?
            .checked_add(4)
            .ok_or(DecompileError::SizeLimit)?;
        let end = base
            .checked_add(size)
            .filter(|end| *end <= raw.len())
            .ok_or_else(|| invalid("Invalid PHY solid range"))?;
        let body = base + 32;
        let root = body
            .checked_add(positive(read(body + 32)?)?)
            .ok_or(DecompileError::SizeLimit)?;
        let mut pending = vec![root];
        let mut seen = HashSet::new();
        let mut binding = None;
        while let Some(node) = pending.pop() {
            if node < root
                || node.checked_add(24).is_none_or(|last| last > end)
                || !seen.insert(node)
                || seen.len() > 16_385
            {
                return Err(invalid("Invalid PHY ledge tree"));
            }
            let right = read(node)?;
            if right == 0 {
                let ledge = node
                    .checked_add_signed(read(node + 4)? as isize)
                    .filter(|ledge| {
                        *ledge >= body + 48
                            && ledge.checked_add(16).is_some_and(|last| last <= root)
                    })
                    .ok_or_else(|| invalid("Invalid PHY ledge binding offset"))?;
                let client = positive(read(ledge + 4)?)?;
                if binding
                    .replace(client)
                    .is_some_and(|previous| previous != client)
                {
                    return Err(invalid(
                        "Convex pieces in one collision solid use different bones",
                    ));
                }
            } else {
                let right = positive(right)?;
                if right < 28 {
                    return Err(invalid("Invalid PHY tree child offset"));
                }
                pending.push(node.checked_add(right).ok_or(DecompileError::SizeLimit)?);
                pending.push(node + 28);
            }
        }
        bindings.push(
            binding
                .ok_or_else(|| invalid("A collision solid has no bound ledges"))?
                .checked_sub(1),
        );
        base = end;
    }
    Ok(bindings)
}

#[cfg(test)]
#[path = "physics_tests.rs"]
mod tests;
