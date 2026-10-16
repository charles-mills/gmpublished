pub(super) fn sprites<T>(items: &mut [T], distance: impl Fn(&T) -> f32) {
    if items.len() < 2 {
        return;
    }
    for parent in (0..=(items.len() - 2) / 2).rev() {
        adjust_heap(items, parent, items.len(), &distance);
    }
    for end in (1..items.len()).rev() {
        items.swap(0, end);
        adjust_heap(items, 0, end, &distance);
    }
}

fn adjust_heap<T>(items: &mut [T], start: usize, end: usize, distance: &impl Fn(&T) -> f32) {
    let mut hole = start;
    while hole < (end - 1) / 2 {
        let right = 2 * (hole + 1);
        let child = if distance_bits(&items[right], distance)
            > distance_bits(&items[right - 1], distance)
        {
            right - 1
        } else {
            right
        };
        items.swap(hole, child);
        hole = child;
    }
    if end.is_multiple_of(2) && hole == (end - 2) / 2 {
        let left = 2 * hole + 1;
        items.swap(hole, left);
        hole = left;
    }
    while hole > start {
        let parent = (hole - 1) / 2;
        if distance_bits(&items[parent], distance) <= distance_bits(&items[hole], distance) {
            break;
        }
        items.swap(hole, parent);
        hole = parent;
    }
}

fn distance_bits<T>(item: &T, distance: &impl Fn(&T) -> f32) -> i32 {
    distance(item).to_bits() as i32
}

pub(super) fn owners<T>(items: &mut [T], depth: impl Fn(&T) -> f32) {
    for gap in [4, 2, 1] {
        let mut index = 0;
        while index + gap < items.len() {
            if depth(&items[index]) > depth(&items[index + gap]) {
                items.swap(index, index + gap);
                index = index.saturating_sub(gap);
            } else {
                index += gap;
            }
        }
    }
    items.reverse();
}

#[cfg(test)]
mod tests {
    use super::{owners, sprites};

    #[test]
    fn sprite_heap_preserves_native_equal_child_choice() {
        let mut items = [(0, 1.0), (1, 1.0), (2, 1.0), (3, 1.0)];
        sprites(&mut items, |item| item.1);
        assert_eq!(items.map(|item| item.0), [1, 3, 0, 2]);
    }

    #[test]
    fn sprite_heap_sorts_far_to_near_without_secondary_key() {
        let mut items = [(0, 4.0), (1, 1.0), (2, 4.0), (3, 2.0), (4, 1.0)];
        sprites(&mut items, |item| item.1);
        assert_eq!(items.map(|item| item.0), [2, 0, 3, 1, 4]);
    }

    #[test]
    fn owner_equal_depth_draws_reverse_encounter_order() {
        let mut items = [(0, 1.0), (1, 1.0), (2, 1.0), (3, 1.0)];
        owners(&mut items, |item| item.1);
        assert_eq!(items.map(|item| item.0), [3, 2, 1, 0]);
    }

    #[test]
    fn owner_gaps_reorder_ties_before_reverse_consumption() {
        let mut items = [(0, 2.0), (1, 5.0), (2, 2.0), (3, 4.0), (4, 1.0)];
        owners(&mut items, |item| item.1);
        assert_eq!(items.map(|item| item.0), [1, 3, 0, 2, 4]);
    }

    #[test]
    fn empty_and_single_item_lists_are_valid() {
        sprites::<f32>(&mut [], |item| *item);
        owners::<f32>(&mut [], |item| *item);
        let mut item = [1.0];
        sprites(&mut item, |item| *item);
        owners(&mut item, |item| *item);
        assert_eq!(item, [1.0]);
    }
}
