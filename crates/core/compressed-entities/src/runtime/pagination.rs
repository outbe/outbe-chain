mod query;

use query::{
    entity_for_query, merged_candidates, record_matches_query, validate_page_request,
    validate_parent_page, verified_matches_query,
};

use super::*;
use crate::api::IdPage;

struct IndexDeltas {
    added: BTreeSet<WwdEntityId>,
    removed: BTreeSet<WwdEntityId>,
}

struct ParentIndex {
    cursor: Option<WwdEntityId>,
    seen: BTreeSet<WwdEntityId>,
    observed_removed: BTreeSet<WwdEntityId>,
    exhausted: bool,
}

pub(super) struct Pagination<'a, 'storage, P> {
    pub(super) storage: StorageHandle<'storage>,
    pub(super) scope: &'a ExecutionScope,
    pub(super) parent: &'a P,
    pub(super) query: QueryRef,
    pub(super) request: IdPageRequest,
}

impl<P: ParentBodySource> Pagination<'_, '_, P> {
    pub(super) fn read(self) -> Result<VerifiedBodyPage> {
        validate_page_request(self.query, self.request)?;
        let deltas = self.pending_deltas()?;
        let target = usize::try_from(self.request.limit)
            .map_err(|_| revert("page limit is not representable"))?;
        let index = self.scan_parent(&deltas, target)?;
        self.body_page(&deltas, &index, target)
    }

    fn pending_deltas(&self) -> Result<IndexDeltas> {
        let state = State::new(self.storage.clone());
        let mut added = BTreeSet::new();
        let mut removed = BTreeSet::new();

        for (record, status) in state.index_deltas()? {
            self.scope
                .deduct_explicit_gas(&self.storage, INDEX_RECORD_SCAN_GAS)?;
            if !record_matches_query(&record, self.query) {
                continue;
            }
            if self
                .request
                .after
                .is_some_and(|after| record.entity_id <= after)
            {
                continue;
            }
            match status {
                DeltaStatus::Added => {
                    added.insert(record.entity_id);
                }
                DeltaStatus::Removed => {
                    removed.insert(record.entity_id);
                }
                DeltaStatus::NoChangeTouched => {}
                DeltaStatus::NeverTouched => {
                    return Err(fatal("zero index delta escaped state validation"));
                }
            }
        }

        Ok(IndexDeltas { added, removed })
    }

    fn scan_parent(&self, deltas: &IndexDeltas, target: usize) -> Result<ParentIndex> {
        let mut index = ParentIndex {
            cursor: self.request.after,
            seen: BTreeSet::new(),
            observed_removed: BTreeSet::new(),
            exhausted: false,
        };
        loop {
            let page = self
                .parent
                .list(
                    self.query,
                    IdPageRequest {
                        after: index.cursor,
                        limit: self.request.limit,
                    },
                )
                .map_err(PrecompileError::from)?;
            validate_parent_page(self.query, index.cursor, self.request.limit, &page)?;
            self.observe_page(deltas, &mut index, &page)?;
            index.exhausted = page.next_after.is_none();
            if !index.exhausted {
                index.cursor = page.next_after;
            }
            let candidates = merged_candidates(&index.seen, &deltas.added, &deltas.removed);
            let has_lookahead = candidates.len() > target;
            let proof_reached = if has_lookahead {
                let lookahead = candidates[target];
                page.ids.last().is_some_and(|last| *last >= lookahead)
            } else {
                false
            };
            if index.exhausted || proof_reached {
                break;
            }
        }
        if index.exhausted {
            let relevant_removed: BTreeSet<_> = deltas
                .removed
                .iter()
                .copied()
                .filter(|id| self.request.after.is_none_or(|after| *id > after))
                .collect();
            if index.observed_removed != relevant_removed {
                return Err(PrecompileError::BodyReadCorruption(
                    "Removed ID is missing from finalized-parent index".into(),
                ));
            }
        }
        Ok(index)
    }

    fn observe_page(
        &self,
        deltas: &IndexDeltas,
        index: &mut ParentIndex,
        page: &IdPage,
    ) -> Result<()> {
        for id in &page.ids {
            self.scope
                .deduct_explicit_gas(&self.storage, PARENT_ID_GAS)?;
            if deltas.added.contains(id) {
                return Err(PrecompileError::BodyReadCorruption(format!(
                    "Added ID {id} already exists in finalized-parent index"
                )));
            }
            if !index.seen.insert(*id) {
                return Err(PrecompileError::BodyReadCorruption(format!(
                    "duplicate finalized-parent ID {id}"
                )));
            }
            if deltas.removed.contains(id) {
                index.observed_removed.insert(*id);
            }
        }
        if let Some(last) = page.ids.last() {
            let skipped_removed = deltas.removed.iter().any(|removed_id| {
                self.request.after.is_none_or(|after| *removed_id > after)
                    && *removed_id <= *last
                    && !index.observed_removed.contains(removed_id)
            });
            if skipped_removed {
                return Err(PrecompileError::BodyReadCorruption(
                    "Removed ID was skipped by the ordered finalized-parent index".into(),
                ));
            }
        }
        Ok(())
    }

    fn body_page(
        &self,
        deltas: &IndexDeltas,
        index: &ParentIndex,
        target: usize,
    ) -> Result<VerifiedBodyPage> {
        let candidates = merged_candidates(&index.seen, &deltas.added, &deltas.removed);
        let has_more = candidates.len() > target || !index.exhausted;
        let selected: Vec<_> = candidates.into_iter().take(target).collect();
        let next_after = if has_more {
            Some(*selected.last().ok_or_else(|| {
                fatal("parent continuation or merged lookahead produced an empty result page")
            })?)
        } else {
            None
        };
        let mut bodies = Vec::with_capacity(selected.len());
        for id in selected {
            let entity = entity_for_query(self.query, id);
            let body =
                read(self.storage.clone(), self.scope, self.parent, entity)?.ok_or_else(|| {
                    PrecompileError::BodyReadCorruption(format!(
                        "listed compressed entity {id} is canonically absent"
                    ))
                })?;
            if !verified_matches_query(&body, self.query) {
                return Err(PrecompileError::BodyReadCorruption(format!(
                    "listed compressed entity {id} violates query predicate"
                )));
            }
            bodies.push(body);
        }
        Ok(VerifiedBodyPage::new(bodies, next_after))
    }
}
