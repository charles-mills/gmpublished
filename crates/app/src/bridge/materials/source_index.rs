use super::{Arc, Mutex, TextureWorkingBudget};
use vformats::{ArchiveIndexCost, ArchiveIndexLimit, ArchiveIndexLimitKind, ArchiveIndexLimits};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct ArchiveIndexStats {
    pub archives: usize,
    pub entries: usize,
    pub directory_bytes: u64,
    pub metadata_bytes: u64,
    pub rejected_archives: usize,
}

#[derive(Debug)]
pub(super) struct SourceIndexBudget {
    limits: ArchiveIndexLimits,
    working: Arc<TextureWorkingBudget>,
    state: Mutex<IndexState>,
}

#[derive(Debug, Default)]
struct IndexState {
    cost: ArchiveIndexCost,
    archives: usize,
    rejected: usize,
}

impl SourceIndexBudget {
    pub(super) fn new(limits: ArchiveIndexLimits, working: Arc<TextureWorkingBudget>) -> Arc<Self> {
        Arc::new(Self {
            limits,
            working,
            state: Mutex::new(IndexState::default()),
        })
    }

    pub(super) fn default_working() -> Arc<TextureWorkingBudget> {
        static WORKING: std::sync::OnceLock<Arc<TextureWorkingBudget>> = std::sync::OnceLock::new();
        let limits = ArchiveIndexLimits::default();
        Arc::clone(
            WORKING.get_or_init(|| {
                Arc::new(TextureWorkingBudget::new(limits.directory_bytes as usize))
            }),
        )
    }

    #[cfg(test)]
    pub(super) fn defaults() -> Arc<Self> {
        Self::new(ArchiveIndexLimits::default(), Self::default_working())
    }

    pub(super) fn limits(&self) -> ArchiveIndexLimits {
        self.limits
    }

    #[cfg(test)]
    pub(super) fn working(
        &self,
        bytes: usize,
    ) -> Result<super::texture_working::TextureWorkingLease, ArchiveIndexLimit> {
        self.working
            .request(bytes)
            .map_err(|_| ArchiveIndexLimit {
                kind: ArchiveIndexLimitKind::WorkingBytes,
                required: bytes as u64,
                limit: self.working.capacity() as u64,
            })
            .map(super::texture_working::TextureWorkingRequest::acquire)
    }

    pub(super) fn working_request(
        &self,
        bytes: usize,
    ) -> Result<super::texture_working::TextureWorkingRequest, ArchiveIndexLimit> {
        self.working.request(bytes).map_err(|_| ArchiveIndexLimit {
            kind: ArchiveIndexLimitKind::WorkingBytes,
            required: bytes as u64,
            limit: self.working.capacity() as u64,
        })
    }

    pub(super) fn reject(&self, error: ArchiveIndexLimit) -> ArchiveIndexLimit {
        let mut state = self.state.lock();
        state.rejected = state.rejected.saturating_add(1);
        error
    }

    pub(super) fn reserve(
        self: &Arc<Self>,
        cost: ArchiveIndexCost,
    ) -> Result<IndexReservation, ArchiveIndexLimit> {
        let mut state = self.state.lock();
        let total = ArchiveIndexCost {
            directory_bytes: state
                .cost
                .directory_bytes
                .checked_add(cost.directory_bytes)
                .ok_or_else(ArchiveIndexLimit::arithmetic_overflow)?,
            entries: state
                .cost
                .entries
                .checked_add(cost.entries)
                .ok_or_else(ArchiveIndexLimit::arithmetic_overflow)?,
            record_bytes: state
                .cost
                .record_bytes
                .checked_add(cost.record_bytes)
                .ok_or_else(ArchiveIndexLimit::arithmetic_overflow)?,
            string_bytes: state
                .cost
                .string_bytes
                .checked_add(cost.string_bytes)
                .ok_or_else(ArchiveIndexLimit::arithmetic_overflow)?,
            copied_bytes: state
                .cost
                .copied_bytes
                .checked_add(cost.copied_bytes)
                .ok_or_else(ArchiveIndexLimit::arithmetic_overflow)?,
        };
        self.limits.check(total)?;
        let archives = state
            .archives
            .checked_add(1)
            .ok_or_else(ArchiveIndexLimit::arithmetic_overflow)?;
        state.cost = total;
        state.archives = archives;
        drop(state);
        Ok(IndexReservation {
            budget: Arc::clone(self),
            cost: Some(cost),
        })
    }

    pub(super) fn stats(&self) -> ArchiveIndexStats {
        let state = self.state.lock();
        ArchiveIndexStats {
            archives: state.archives,
            entries: state.cost.entries,
            directory_bytes: state.cost.directory_bytes,
            metadata_bytes: state
                .cost
                .retained_bytes()
                .expect("admitted index cost fits u64"),
            rejected_archives: state.rejected,
        }
    }
}

#[derive(Debug)]
pub(super) struct IndexReservation {
    budget: Arc<SourceIndexBudget>,
    cost: Option<ArchiveIndexCost>,
}
impl IndexReservation {
    pub(super) fn commit(mut self) {
        self.cost = None;
    }
}
impl Drop for IndexReservation {
    fn drop(&mut self) {
        if let Some(cost) = self.cost {
            let mut state = self.budget.state.lock();
            state.cost.directory_bytes -= cost.directory_bytes;
            state.cost.entries -= cost.entries;
            state.cost.record_bytes -= cost.record_bytes;
            state.cost.string_bytes -= cost.string_bytes;
            state.cost.copied_bytes -= cost.copied_bytes;
            state.archives -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unbounded() -> Arc<SourceIndexBudget> {
        SourceIndexBudget::new(
            ArchiveIndexLimits {
                directory_bytes: u64::MAX,
                entries: usize::MAX,
                retained_bytes: u64::MAX,
            },
            Arc::new(TextureWorkingBudget::new(8)),
        )
    }

    #[test]
    fn aggregate_component_overflow_keeps_the_last_admitted_state() {
        for (base, additional) in [
            (
                ArchiveIndexCost {
                    directory_bytes: u64::MAX,
                    ..Default::default()
                },
                ArchiveIndexCost {
                    directory_bytes: 1,
                    ..Default::default()
                },
            ),
            (
                ArchiveIndexCost {
                    entries: usize::MAX,
                    ..Default::default()
                },
                ArchiveIndexCost {
                    entries: 1,
                    ..Default::default()
                },
            ),
            (
                ArchiveIndexCost {
                    record_bytes: u64::MAX,
                    ..Default::default()
                },
                ArchiveIndexCost {
                    record_bytes: 1,
                    ..Default::default()
                },
            ),
            (
                ArchiveIndexCost {
                    string_bytes: u64::MAX,
                    ..Default::default()
                },
                ArchiveIndexCost {
                    string_bytes: 1,
                    ..Default::default()
                },
            ),
            (
                ArchiveIndexCost {
                    copied_bytes: u64::MAX,
                    ..Default::default()
                },
                ArchiveIndexCost {
                    copied_bytes: 1,
                    ..Default::default()
                },
            ),
        ] {
            let budget = unbounded();
            budget.reserve(base).unwrap().commit();
            let admitted = budget.stats();
            assert_eq!(
                budget.reserve(additional).err().unwrap().kind,
                ArchiveIndexLimitKind::ArithmeticOverflow
            );
            assert_eq!(budget.stats(), admitted);
            assert_eq!(budget.state.lock().cost, base);
        }
    }

    #[test]
    fn retained_sum_overflow_cannot_commit_or_corrupt_a_later_rollback() {
        let budget = unbounded();
        let base = ArchiveIndexCost {
            record_bytes: u64::MAX,
            ..Default::default()
        };
        budget.reserve(base).unwrap().commit();
        assert_eq!(
            budget
                .reserve(ArchiveIndexCost {
                    string_bytes: 1,
                    ..Default::default()
                })
                .err()
                .unwrap()
                .kind,
            ArchiveIndexLimitKind::ArithmeticOverflow
        );
        {
            let _rollback = budget.reserve(ArchiveIndexCost::default()).unwrap();
        }
        assert_eq!(budget.state.lock().cost, base);
        assert_eq!(budget.stats().archives, 1);
    }

    #[test]
    fn archive_counter_overflow_rejects_atomically_and_diagnostics_saturate() {
        let budget = unbounded();
        budget.state.lock().archives = usize::MAX;
        let before = budget.stats();
        assert_eq!(
            budget
                .reserve(ArchiveIndexCost {
                    directory_bytes: 1,
                    ..Default::default()
                })
                .err()
                .unwrap()
                .kind,
            ArchiveIndexLimitKind::ArithmeticOverflow
        );
        assert_eq!(budget.stats(), before);
        budget.state.lock().rejected = usize::MAX;
        budget.reject(ArchiveIndexLimit::arithmetic_overflow());
        assert_eq!(budget.stats().rejected_archives, usize::MAX);
    }

    #[test]
    fn aggregate_reservations_are_atomic_and_failed_initializations_roll_back() {
        let budget = SourceIndexBudget::new(
            ArchiveIndexLimits {
                directory_bytes: 8,
                entries: 2,
                retained_bytes: 16,
            },
            Arc::new(TextureWorkingBudget::new(8)),
        );
        let cost = ArchiveIndexCost {
            directory_bytes: 4,
            entries: 1,
            record_bytes: 8,
            ..Default::default()
        };
        budget.reserve(cost).unwrap().commit();
        {
            let _failed = budget.reserve(cost).unwrap();
            assert_eq!(budget.stats().archives, 2);
        }
        assert_eq!(budget.stats().archives, 1);
        budget.reserve(cost).unwrap().commit();
        budget.reject(budget.reserve(cost).err().unwrap());
        assert_eq!(
            budget.stats(),
            ArchiveIndexStats {
                archives: 2,
                entries: 2,
                directory_bytes: 8,
                metadata_bytes: 16,
                rejected_archives: 1
            }
        );
    }
}
