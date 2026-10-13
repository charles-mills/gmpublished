use std::fmt::Write as _;

use super::{DecompileError, Reader, invalid, quoted};

pub(super) fn prelude(reader: &Reader<'_>, sequences: &[usize]) -> Result<String, DecompileError> {
    let names = names(reader)?;
    let n = names.len();
    if n == 0 {
        for &record in sequences {
            entry(reader, record, 0)?;
        }
        return Ok(String::new());
    }
    let offset = reader.count(252)?;
    let stored = reader.bytes(offset, n * n)?;
    if stored.iter().any(|value| *value as usize > n) {
        return Err(invalid("Transition graph references an absent node"));
    }
    let mut direct = vec![0i16; n * n];
    for &record in sequences {
        let [from, to, reverse] = entry(reader, record, n)?;
        if from != to {
            direct[(from - 1) * n + to - 1] = to as i16;
            if reverse != 0 {
                direct[(to - 1) * n + from - 1] = from as i16;
            }
        }
    }
    let mut selected = None;
    for multistage in [false, true] {
        let mut routes = direct.clone();
        if multistage {
            expand(&mut routes, n);
        }
        if routes
            .iter()
            .zip(stored)
            .all(|(candidate, original)| *original == 0 || *candidate == i16::from(*original))
            && (0..n).all(|from| {
                (0..n).all(|to| {
                    routes[from * n + to] == i16::from(stored[from * n + to])
                        || from != to && stored[from * n + to] == 0 && stored[to * n + from] == 0
                })
            })
        {
            selected = Some((routes, multistage));
            break;
        }
    }
    let (routes, multistage) = selected
        .ok_or_else(|| invalid("Authored transition graph cannot be represented by standard QC"))?;
    let mut output = String::new();
    for name in &names {
        writeln!(output, "$skiptransition {}", quoted(name)?).unwrap();
    }
    if multistage {
        output.push_str("$calctransitions\n");
    }
    for from in 0..n {
        for to in from + 1..n {
            if stored[from * n + to] == 0
                && stored[to * n + from] == 0
                && (routes[from * n + to] != 0 || routes[to * n + from] != 0)
            {
                writeln!(
                    output,
                    "$skiptransition {} {}",
                    quoted(&names[from])?,
                    quoted(&names[to])?
                )
                .unwrap();
            }
        }
    }
    Ok(output)
}

pub(super) fn sequence(reader: &Reader<'_>, record: usize) -> Result<String, DecompileError> {
    let names = names(reader)?;
    let [from, to, reverse] = entry(reader, record, names.len())?;
    Ok(if from == 0 {
        String::new()
    } else if from == to && reverse == 0 {
        format!("    node {}\n", quoted(&names[from - 1])?)
    } else {
        format!(
            "    {} {} {}\n",
            if reverse == 0 {
                "transition"
            } else {
                "rtransition"
            },
            quoted(&names[from - 1])?,
            quoted(&names[to - 1])?
        )
    })
}

fn names(reader: &Reader<'_>) -> Result<Vec<String>, DecompileError> {
    let count = reader.count(248)?;
    if count > 255 {
        return Err(invalid("Excessive transition node count"));
    }
    let mut names: Vec<String> = Vec::new();
    for entry in reader.table(248, 256, 4)? {
        let name = reader.string(0, entry)?;
        if name.is_empty()
            || names
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(name))
        {
            return Err(invalid("Empty or duplicate transition node names"));
        }
        names.push(name.into());
    }
    Ok(names)
}

fn entry(reader: &Reader<'_>, record: usize, nodes: usize) -> Result<[usize; 3], DecompileError> {
    let from = reader.count(record + 112)?;
    let to = reader.count(record + 116)?;
    let flags = reader.count(record + 120)?;
    if from > nodes
        || to > nodes
        || (from == 0) != (to == 0)
        || flags > 1
        || from == 0 && flags != 0
    {
        return Err(invalid("Invalid sequence transition nodes or flags"));
    }
    if reader.i32(record + 12)? & 0x800 != 0 && from != 0 {
        return Err(invalid(
            "Transition nodes on a declared sequence cannot be reconstructed",
        ));
    }
    Ok([from, to, flags])
}

fn expand(routes: &mut [i16], n: usize) {
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
        for route in &mut *routes {
            *route = route.abs();
        }
        if !changed {
            break;
        }
    }
}

#[cfg(test)]
#[path = "transitions_tests.rs"]
mod tests;
