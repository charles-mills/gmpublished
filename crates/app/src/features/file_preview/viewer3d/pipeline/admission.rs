use super::lightmaps::MissingCost;
use std::collections::HashMap;
use std::hash::Hash;

#[derive(Clone, Debug)]
pub(super) struct Cost<K> {
    allocations: HashMap<K, (u64, bool)>,
}

impl<K: Eq + Hash + Clone> Cost<K> {
    pub fn new() -> Self {
        Self {
            allocations: HashMap::new(),
        }
    }

    pub fn insert(&mut self, key: K, bytes: u64, resolved: bool) {
        self.allocations.insert(key, (bytes, resolved));
    }

    pub fn extend(&mut self, other: &Self) {
        self.allocations.extend(
            other
                .allocations
                .iter()
                .map(|(key, value)| (key.clone(), *value)),
        );
    }

    pub fn upper_bound(&self) -> Result<MissingCost, String> {
        self.total(false)
    }

    fn total(&self, resolved_only: bool) -> Result<MissingCost, String> {
        self.allocations
            .values()
            .filter(|(_, resolved)| !resolved_only || *resolved)
            .try_fold(MissingCost::default(), |mut total, (bytes, _)| {
                total.bytes = total
                    .bytes
                    .checked_add(*bytes)
                    .ok_or("sampled-image working set size overflows")?;
                total.allocations = total
                    .allocations
                    .checked_add(1)
                    .ok_or("sampled-image allocation count overflows")?;
                Ok(total)
            })
    }
}

pub(super) struct Candidate<K> {
    pub priority: bool,
    pub cost: Result<Cost<K>, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Decision {
    Admitted,
    Pending,
    Rejected(String),
}

pub(super) fn exceeds(cost: MissingCost, capacity: MissingCost) -> bool {
    cost.bytes > capacity.bytes || cost.allocations > capacity.allocations
}

pub(super) fn rejection(cost: MissingCost, capacity: MissingCost) -> String {
    format!(
        "visible texture working set requires {} bytes in {} allocations; GPU pool capacity is {} bytes in {} allocations",
        cost.bytes, cost.allocations, capacity.bytes, capacity.allocations
    )
}

pub(super) fn select<K: Eq + Hash + Clone>(
    candidates: &[Candidate<K>],
    capacity: MissingCost,
) -> Vec<Decision> {
    let mut order: Vec<_> = (0..candidates.len()).collect();
    order.sort_by_key(|&index| (!candidates[index].priority, index));
    let mut decisions = vec![Decision::Pending; candidates.len()];
    let mut accepted = Cost::new();
    for index in order {
        let cost = match &candidates[index].cost {
            Ok(cost) => cost,
            Err(reason) => {
                decisions[index] = Decision::Rejected(reason.clone());
                continue;
            }
        };
        let mut proposed = accepted.clone();
        proposed.extend(cost);
        match proposed.upper_bound() {
            Err(reason) => decisions[index] = Decision::Rejected(reason),
            Ok(total) if !exceeds(total, capacity) => {
                accepted = proposed;
                decisions[index] = Decision::Admitted;
            }
            Ok(_) => match proposed.total(true) {
                Err(reason) => decisions[index] = Decision::Rejected(reason),
                Ok(total) if exceeds(total, capacity) => {
                    decisions[index] = Decision::Rejected(rejection(total, capacity));
                }
                Ok(_) => {}
            },
        }
    }
    decisions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cost(entries: &[(&'static str, u64, bool)]) -> Cost<&'static str> {
        let mut cost = Cost::new();
        for &(key, bytes, resolved) in entries {
            cost.insert(key, bytes, resolved);
        }
        cost
    }

    fn capacity(bytes: u64, allocations: u64) -> MissingCost {
        MissingCost { bytes, allocations }
    }

    #[test]
    fn shared_physical_images_are_charged_once_across_viewers() {
        let candidates = [false, true].map(|priority| Candidate {
            priority,
            cost: Ok(cost(&[("shared-static", 64, true)])),
        });
        assert_eq!(
            select(&candidates, capacity(64, 1)),
            [Decision::Admitted, Decision::Admitted]
        );
    }

    #[test]
    fn incumbent_animation_wins_over_new_incompatible_demand_in_either_widget_order() {
        for incumbent_first in [false, true] {
            let mut candidates = vec![
                Candidate {
                    priority: false,
                    cost: Ok(cost(&[("new-map-lighting", 64, true)])),
                },
                Candidate {
                    priority: true,
                    cost: Ok(cost(&[("next-animation-frame", 64, true)])),
                },
            ];
            if incumbent_first {
                candidates.reverse();
            }
            let decisions = select(&candidates, capacity(64, 1));
            let incumbent = usize::from(!incumbent_first);
            assert_eq!(decisions[incumbent], Decision::Admitted);
            assert!(matches!(decisions[1 - incumbent], Decision::Rejected(_)));
        }
    }

    #[test]
    fn unresolved_alias_overflow_waits_without_consuming_another_views_budget() {
        let candidates = [
            Candidate {
                priority: true,
                cost: Ok(cost(&[
                    ("unresolved-a", 64, false),
                    ("unresolved-b", 64, false),
                ])),
            },
            Candidate {
                priority: false,
                cost: Ok(cost(&[("visible", 64, true)])),
            },
        ];
        assert_eq!(
            select(&candidates, capacity(64, 1)),
            [Decision::Pending, Decision::Admitted]
        );
        let resolved = [Candidate {
            priority: true,
            cost: Ok(cost(&[
                ("same-physical-image", 64, true),
                ("same-physical-image", 64, true),
            ])),
        }];
        assert_eq!(select(&resolved, capacity(64, 1)), [Decision::Admitted]);
    }

    #[test]
    fn known_overflow_is_rejected_even_when_other_aliases_remain_unresolved() {
        let candidates = [Candidate {
            priority: false,
            cost: Ok(cost(&[
                ("lightmap-a", 64, true),
                ("lightmap-b", 64, true),
                ("texture", 64, false),
            ])),
        }];
        assert!(matches!(
            select(&candidates, capacity(64, 4))[0],
            Decision::Rejected(_)
        ));
    }

    #[test]
    fn allocation_count_is_enforced_independently_of_bytes() {
        let candidates = [Candidate {
            priority: false,
            cost: Ok(cost(&[("a", 1, true), ("b", 1, true), ("c", 1, true)])),
        }];
        assert!(matches!(
            select(&candidates, capacity(1024, 2))[0],
            Decision::Rejected(_)
        ));
    }

    #[test]
    fn overflow_and_invalid_requests_do_not_poison_valid_siblings() {
        let candidates = [
            Candidate {
                priority: false,
                cost: Ok(cost(&[("a", u64::MAX, true), ("b", 1, true)])),
            },
            Candidate {
                priority: false,
                cost: Err("invalid source".into()),
            },
            Candidate {
                priority: false,
                cost: Ok(cost(&[("valid", 64, true)])),
            },
        ];
        let decisions = select(&candidates, capacity(64, 1));
        assert!(matches!(decisions[0], Decision::Rejected(_)));
        assert_eq!(decisions[1], Decision::Rejected("invalid source".into()));
        assert_eq!(decisions[2], Decision::Admitted);
    }
}
