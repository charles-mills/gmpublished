use crate::model::CompiledModelDesc;

pub(crate) struct Graph {
    pub names: Vec<String>,
    pub routes: Vec<u8>,
    pub sequences: Vec<[i32; 3]>,
}

pub(crate) fn build(compiled: &CompiledModelDesc) -> Result<Graph, String> {
    let mut names = Vec::new();
    for name in &compiled.desc.model.transition_nodes {
        node(&mut names, name)?;
    }
    let mut sequences = Vec::with_capacity(compiled.sequences.len());
    for sequence in &compiled.sequences {
        sequences.push(if let Some([entry, exit]) = &sequence.transition {
            [
                node(&mut names, entry)? as i32,
                node(&mut names, exit)? as i32,
                i32::from(sequence.transition_reversible),
            ]
        } else {
            [0; 3]
        });
    }
    let mut skips = Vec::new();
    for [from, to] in &compiled.desc.model.transition_skips {
        skips.push([node(&mut names, from)?, node(&mut names, to)?]);
    }
    let n = names.len();
    let mut routes = vec![0i16; n * n];
    for &[entry, exit, reverse] in &sequences {
        if entry == exit {
            continue;
        }
        routes[(entry as usize - 1) * n + exit as usize - 1] = exit as i16;
        if reverse != 0 {
            routes[(exit as usize - 1) * n + entry as usize - 1] = entry as i16;
        }
    }
    if compiled.desc.model.transition_multistage {
        loop {
            let mut changed = false;
            for from in 0..n {
                for to in 0..n {
                    if from == to || routes[from * n + to] != 0 {
                        continue;
                    }
                    for via in 0..n {
                        if routes[via * n + to] > 0 && routes[from * n + via] > 0 {
                            routes[from * n + to] = -routes[from * n + via];
                            changed = true;
                            break;
                        }
                    }
                }
            }
            for route in &mut routes {
                *route = route.abs();
            }
            if !changed {
                break;
            }
        }
    }
    for [from, to] in skips {
        if from != to {
            routes[(from - 1) * n + to - 1] = 0;
        }
    }
    Ok(Graph {
        names,
        routes: routes.into_iter().map(|route| route as u8).collect(),
        sequences,
    })
}

fn node(names: &mut Vec<String>, name: &str) -> Result<usize, String> {
    if name.is_empty() || name.contains('\0') {
        return Err("Transition node names must be nonempty and contain no null bytes".into());
    }
    if let Some(index) = names
        .iter()
        .position(|value| value.eq_ignore_ascii_case(name))
    {
        return Ok(index + 1);
    }
    if names.len() >= 255 {
        return Err("Source models support at most 255 transition nodes".into());
    }
    names.push(name.into());
    Ok(names.len())
}
