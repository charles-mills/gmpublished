use crate::model::ModelDesc;

pub(crate) const SOLID: i32 = 0x0000_0001;
pub(crate) const GRATE: i32 = 0x0000_0008;
pub(crate) const MONSTER: i32 = 0x0200_0000;
pub(crate) const LADDER: i32 = 0x2000_0000;

fn parents(desc: &ModelDesc) -> Result<Vec<Option<usize>>, String> {
    let indices = desc.bone_index();
    let parents: Vec<_> = desc
        .bones
        .iter()
        .map(|bone| {
            bone.parent
                .as_deref()
                .map(|parent| {
                    indices.get(parent).copied().ok_or_else(|| {
                        format!("Bone properties reference absent parent bone {parent:?}")
                    })
                })
                .transpose()
        })
        .collect::<Result<_, _>>()?;
    Ok(parents)
}

pub(crate) fn bone_contents(desc: &ModelDesc) -> Result<Vec<i32>, String> {
    let parents = parents(desc)?;
    let mut named = std::collections::HashMap::new();
    for (bone, mask) in &desc.model.joint_contents {
        named.insert(bone.to_ascii_lowercase(), *mask);
    }
    let overrides: Vec<_> = desc
        .bones
        .iter()
        .map(|bone| named.get(&bone.name.to_ascii_lowercase()).copied())
        .collect();
    resolve(desc.model.contents.unwrap_or(SOLID), &parents, &overrides).map_err(str::to_owned)
}

pub(crate) fn bone_surface_props(desc: &ModelDesc) -> Result<Vec<Option<&str>>, String> {
    let parents = parents(desc)?;
    let overrides: Vec<_> = desc
        .bones
        .iter()
        .map(|bone| bone.surface_prop.as_deref().map(Some))
        .collect();
    resolve(desc.model.surface_prop.as_deref(), &parents, &overrides).map_err(str::to_owned)
}

fn resolve<T: Copy>(
    global: T,
    parents: &[Option<usize>],
    overrides: &[Option<T>],
) -> Result<Vec<T>, &'static str> {
    if parents.len() != overrides.len()
        || parents
            .iter()
            .flatten()
            .any(|&parent| parent >= parents.len())
    {
        return Err("Invalid bone property parent table");
    }
    let mut state = vec![0u8; parents.len()];
    let mut masks = vec![global; parents.len()];
    let mut pending = Vec::new();
    for start in 0..parents.len() {
        let mut next = Some(start);
        while let Some(index) = next {
            match state[index] {
                2 => break,
                1 => return Err("Bone property hierarchy contains a parent cycle"),
                _ => {}
            }
            state[index] = 1;
            pending.push(index);
            next = parents[index];
        }
        while let Some(index) = pending.pop() {
            let inherited = parents[index].map_or(global, |parent| masks[parent]);
            masks[index] = overrides[index].unwrap_or(inherited);
            state[index] = 2;
        }
    }
    Ok(masks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inheritance_handles_child_first_tables_and_checks_cycles_even_with_overrides() {
        assert_eq!(
            resolve(SOLID, &[Some(2), None, Some(1)], &[None, None, Some(GRATE)]).unwrap(),
            [GRATE, SOLID, GRATE]
        );
        assert!(resolve(SOLID, &[Some(1)], &[None]).is_err());
        assert!(resolve(SOLID, &[None], &[]).is_err());
        assert!(resolve(SOLID, &[Some(1), Some(0)], &[Some(GRATE), Some(LADDER)]).is_err());
        assert_eq!(
            resolve(
                Some("wood"),
                &[Some(2), None, Some(1)],
                &[None, None, Some(Some("flesh"))]
            )
            .unwrap(),
            [Some("flesh"), Some("wood"), Some("flesh")]
        );
        assert_eq!(
            resolve(Some("wood"), &[None, Some(0)], &[Some(Some("")), None]).unwrap(),
            [Some(""), Some("")]
        );
        let count = 50_000;
        let mut parents: Vec<_> = (1..=count).map(Some).collect();
        parents[count - 1] = None;
        let mut overrides = vec![None; count];
        overrides[count - 1] = Some(MONSTER | LADDER);
        assert_eq!(
            resolve(SOLID, &parents, &overrides).unwrap(),
            vec![MONSTER | LADDER; count]
        );
    }
}
