use crate::model::CompiledModelDesc;

struct Control {
    component: i32,
    controller: i32,
    min: f32,
    max: f32,
}

pub(crate) fn append(
    compiled: &CompiledModelDesc,
    bytes: &mut Vec<u8>,
    header: usize,
) -> Result<(), String> {
    if compiled.desc.model.bone_flex_drivers.is_empty() {
        return Ok(());
    }
    let mut groups: Vec<(usize, Vec<Control>)> = Vec::new();
    for driver in &compiled.desc.model.bone_flex_drivers {
        if driver.component > 2
            || !driver.min.is_finite()
            || !driver.max.is_finite()
            || driver.min == driver.max
        {
            return Err(format!(
                "Invalid bone flex driver range or component for {:?}",
                driver.bone
            ));
        }
        let bone = compiled
            .desc
            .bones
            .iter()
            .position(|bone| bone.name.eq_ignore_ascii_case(&driver.bone))
            .ok_or_else(|| format!("Unknown bone flex driver bone {:?}", driver.bone))?;
        let controller = compiled
            .desc
            .flex_controllers
            .iter()
            .position(|controller| controller.name.eq_ignore_ascii_case(&driver.controller))
            .ok_or_else(|| format!("Unknown bone flex controller {:?}", driver.controller))?;
        if compiled
            .resolved_jiggle_bones
            .iter()
            .any(|item| item.bone == bone as i32)
            || compiled
                .resolved_quat_interp_bones
                .iter()
                .any(|item| item.bone == bone as i32)
        {
            return Err(format!(
                "Procedural bone {:?} cannot drive a flex controller",
                driver.bone
            ));
        }
        // Source remaps driver axes only for its canonical Y-up root transform.
        let component = if compiled.desc.bones[bone].parent.is_none()
            && compiled.desc.model.source_rotation.is_some_and(|rotation| {
                rotation[0] == std::f32::consts::FRAC_PI_2
                    && rotation[1] == 0.0
                    && rotation[2] == std::f32::consts::FRAC_PI_2
            }) {
            [1, 2, 0][driver.component as usize]
        } else {
            driver.component as i32
        };
        let control = Control {
            component,
            controller: controller as i32,
            min: driver.min,
            max: driver.max,
        };
        let group = match groups.iter().position(|group| group.0 == bone) {
            Some(index) => &mut groups[index].1,
            None => {
                groups.push((bone, Vec::new()));
                &mut groups.last_mut().unwrap().1
            }
        };
        match group
            .iter()
            .position(|item| item.controller == control.controller)
        {
            Some(index) => group[index] = control,
            None => group.push(control),
        }
    }
    let start = bytes
        .len()
        .checked_add(3)
        .map(|length| length & !3)
        .ok_or("Bone flex driver offset overflows")?;
    let records = groups
        .len()
        .checked_mul(24)
        .ok_or("Bone flex driver count overflows")?;
    let controls = groups
        .iter()
        .try_fold(0usize, |length, group| length.checked_add(group.1.len()))
        .and_then(|length| length.checked_mul(16))
        .ok_or("Bone flex control count overflows")?;
    let end = start
        .checked_add(records)
        .and_then(|length| length.checked_add(controls))
        .filter(|&length| length <= i32::MAX as usize)
        .ok_or("Bone flex drivers exceed the model format limit")?;
    bytes.resize(end, 0);
    let integer = |bytes: &mut [u8], at: usize, value: i32| {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes())
    };
    integer(bytes, header + 0x18, groups.len() as i32);
    integer(
        bytes,
        header + 0x1c,
        i32::try_from(
            start
                .checked_sub(header)
                .ok_or("Invalid bone flex driver header offset")?,
        )
        .map_err(|_| "Bone flex driver relative offset overflows")?,
    );
    let mut control_offset = start + records;
    for (index, (bone, controls)) in groups.into_iter().enumerate() {
        let record = start + index * 24;
        integer(bytes, record, bone as i32);
        integer(bytes, record + 4, controls.len() as i32);
        integer(bytes, record + 8, (control_offset - record) as i32);
        for control in controls {
            integer(bytes, control_offset, control.component);
            integer(bytes, control_offset + 4, control.controller);
            bytes[control_offset + 8..control_offset + 12]
                .copy_from_slice(&control.min.to_le_bytes());
            bytes[control_offset + 12..control_offset + 16]
                .copy_from_slice(&control.max.to_le_bytes());
            control_offset += 16;
        }
    }
    integer(bytes, 76, end as i32);
    Ok(())
}
