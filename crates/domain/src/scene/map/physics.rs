//! PHYSCOLLIDE contains dphysmodel_t records, each followed by its VCollide
//! sections and keyvalues. The sections use the same format as a .phy file.
//! Reuse the bounded IVP decoder instead of deriving approximate physics from
//! visible faces or applying a guessed shrink margin to BSP planes.
use super::{ConvexHull, ModelIndex};
use crate::math::Vec3;

fn i32_at(bytes: &[u8], offset: usize) -> Option<i32> {
    Some(i32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn model_data(bytes: &[u8], wanted: ModelIndex) -> Option<(&[u8], i32)> {
    let mut offset = 0_usize;
    while offset < bytes.len() {
        let index = i32_at(bytes, offset)?;
        if index == -1 {
            return None;
        }
        let index = usize::try_from(index).ok()?;
        let data_size = usize::try_from(i32_at(bytes, offset + 4)?).ok()?;
        let key_size = usize::try_from(i32_at(bytes, offset + 8)?).ok()?;
        let solids = i32_at(bytes, offset + 12)?;
        if solids < 0 {
            return None;
        }
        let start = offset.checked_add(16)?;
        let end = start.checked_add(data_size)?;
        let next = end.checked_add(key_size)?;
        let data = bytes.get(start..end)?;
        if next > bytes.len() {
            return None;
        }
        if index == wanted.get() {
            return Some((data, solids));
        }
        offset = next;
    }
    None
}

pub(super) fn model_hulls(bytes: &[u8], model: ModelIndex) -> Option<Vec<ConvexHull>> {
    let (data, solids) = model_data(bytes, model)?;
    let mut phy = Vec::with_capacity(16 + data.len());
    for value in [16_i32, 0, solids, 0] {
        phy.extend_from_slice(&value.to_le_bytes());
    }
    phy.extend_from_slice(data);
    let decoded = vformats::phy::parse_lossy(&phy, &vformats::Limits::default()).ok()?;
    if !decoded.stats.skip_reasons.is_empty() {
        log::warn!(
            "bsp model {model} VPhysics decode degraded: {:?}",
            decoded.stats.skip_reasons
        );
    }
    let hulls = decoded
        .solids
        .into_iter()
        .flat_map(|solid| solid.ledges)
        .map(|ledge| ConvexHull {
            vertices: ledge.vertices.into_iter().map(Vec3::from).collect(),
            triangles: ledge.triangles,
        })
        .collect::<Vec<_>>();
    (!hulls.is_empty()).then_some(hulls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physics_records_select_model_and_validate_lengths_and_sentinel() {
        let mut bytes = Vec::new();
        for value in [0_i32, 4, 3, 1] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(b"blobkey");
        for value in [3_i32, 5, 0, 2] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(b"other");
        for value in [-1_i32, -1, 0, 0] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        assert_eq!(
            model_data(&bytes, ModelIndex::new(3)),
            Some((b"other".as_slice(), 2))
        );
        assert!(model_data(&bytes, ModelIndex::new(1)).is_none());
        for len in 0..28 {
            assert!(model_data(&bytes[..len], ModelIndex::new(3)).is_none());
        }
        bytes[4..8].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(model_data(&bytes, ModelIndex::new(3)).is_none());
        assert!(model_hulls(&bytes, ModelIndex::new(0)).is_none());
    }
}
