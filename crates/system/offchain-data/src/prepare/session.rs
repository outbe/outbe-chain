//! Simulate ordered domain changes before the projector writes a block.

use super::sources::ProjectionSessions;
use super::*;
use outbe_primitives::time::WorldwideDay;

pub(super) struct BlockProjection<'a> {
    projector: &'a OffchainDataProjection,
    sources: ProjectionSessions,
    pub(super) day_retirements: Vec<DayRetirement>,
    retired_days: BTreeSet<u32>,
    seen_tributes: BTreeSet<WwdEntityId>,
    seen_nods: BTreeSet<WwdEntityId>,
    seen_buckets: BTreeSet<WwdEntityId>,
}

impl<'a> BlockProjection<'a> {
    pub(super) fn new(projector: &'a OffchainDataProjection, sources: ProjectionSessions) -> Self {
        Self {
            projector,
            sources,
            day_retirements: Vec::new(),
            retired_days: BTreeSet::new(),
            seen_tributes: BTreeSet::new(),
            seen_nods: BTreeSet::new(),
            seen_buckets: BTreeSet::new(),
        }
    }
    pub(super) fn plan_event(
        &mut self,
        event: ProjectionEvent,
    ) -> Result<AtomicWriteBatch, ProjectionError> {
        let planned = match event {
            ProjectionEvent::TributeStored(event) => {
                self.require_live_tribute(event.identity)?;
                self.require_tribute_transition(event.identity, event.previous_commitment)?;
                self.sources.tributes.store(
                    event.identity,
                    event.stored_body,
                    Some(event.source.to_storage_metadata()?),
                )?
            }
            ProjectionEvent::TributeDeleted(event) => {
                self.require_live_tribute(event.identity)?;
                self.require_tribute_transition(event.identity, event.previous_commitment)?;
                self.sources.tributes.delete(event.identity)?
            }
            ProjectionEvent::TributePartitionRetired { worldwide_day } => {
                return self.plan_retirement(worldwide_day);
            }
            ProjectionEvent::NodStored(event) => {
                self.require_nod_transition(event.identity, event.previous_commitment)?;
                self.sources.nods.store_item(
                    event.identity,
                    event.stored_body,
                    Some(event.source.to_storage_metadata()?),
                )?
            }
            ProjectionEvent::NodDeleted(event) => {
                self.require_nod_transition(event.identity, event.previous_commitment)?;
                self.sources.nods.delete_item(event.identity)?
            }
            ProjectionEvent::BucketStored(event) => {
                self.require_bucket_transition(event.identity, event.previous_commitment)?;
                self.sources.nods.store_bucket(
                    event.identity,
                    event.stored_body,
                    Some(event.source.to_storage_metadata()?),
                )?
            }
            ProjectionEvent::BucketDeleted(event) => {
                self.require_bucket_transition(event.identity, event.previous_commitment)?;
                self.sources.nods.delete_bucket(event.identity)?
            }
        };
        Ok(planned)
    }

    fn require_live_tribute(&self, tribute_id: WwdEntityId) -> Result<(), ProjectionError> {
        reject_tribute_after_retirement(
            self.projector.day_route.is_some() || self.projector.partition_retirement,
            &self.retired_days,
            tribute_id,
        )?;
        if self.projector.partition_retirement
            && outbe_tribute::read_tribute_day_mark(
                self.projector.reader.as_ref(),
                tribute_id.worldwide_day().value(),
            )?
            .is_some()
        {
            return Err(ProjectionError::TributeStoredAfterDayRetirement { tribute_id });
        }
        Ok(())
    }
    fn require_tribute_transition(
        &mut self,
        tribute_id: WwdEntityId,
        previous: B256,
    ) -> Result<(), ProjectionError> {
        let old = self.sources.tributes.current(tribute_id)?;
        validate_tribute_transition(
            tribute_id,
            old,
            previous,
            self.seen_tributes.insert(tribute_id),
        )
    }
    fn require_nod_transition(
        &mut self,
        nod_id: WwdEntityId,
        previous: B256,
    ) -> Result<(), ProjectionError> {
        let old = self.sources.nods.current_item(nod_id)?;
        validate_nod_transition(nod_id, old, previous, self.seen_nods.insert(nod_id))
    }
    fn require_bucket_transition(
        &mut self,
        bucket_id: WwdEntityId,
        previous: B256,
    ) -> Result<(), ProjectionError> {
        let old = self.sources.nods.current_bucket(bucket_id)?;
        validate_bucket_transition(
            bucket_id,
            old,
            previous,
            self.seen_buckets.insert(bucket_id),
        )
    }
    fn plan_retirement(
        &mut self,
        worldwide_day: WorldwideDay,
    ) -> Result<AtomicWriteBatch, ProjectionError> {
        let mut batch = AtomicWriteBatch::new();
        let retention_pin = self
            .projector
            .tribute_retention_selector
            .as_ref()
            .map(|selector| selector.active_pin_for(worldwide_day))
            .transpose()
            .map_err(|reason| ProjectionError::RetentionSelector {
                worldwide_day,
                reason,
            })?
            .flatten();
        if let Some(pin) = retention_pin {
            if pin.worldwide_day != worldwide_day {
                return Err(ProjectionError::RetentionPinDayMismatch {
                    requested: worldwide_day,
                    selected: pin.worldwide_day,
                });
            }
        }
        if self.projector.day_route.is_some() || self.projector.partition_retirement {
            record_day_retirement(
                &mut self.day_retirements,
                &mut self.retired_days,
                worldwide_day.value(),
                retention_pin,
            );
            if self.projector.partition_retirement && retention_pin.is_some() {
                super::super::retirement::plan_retired_partition(
                    &mut self.sources.tributes,
                    &self.sources.retained_tribute_reader,
                    super::super::retirement::RetiredPartition {
                        tribute_ids: &self.sources.tribute_ids,
                        worldwide_day,
                        retention_pin,
                    },
                    &mut batch,
                )?;
            }
        } else {
            super::super::retirement::plan_retired_partition(
                &mut self.sources.tributes,
                &self.sources.retained_tribute_reader,
                super::super::retirement::RetiredPartition {
                    tribute_ids: &self.sources.tribute_ids,
                    worldwide_day,
                    retention_pin,
                },
                &mut batch,
            )?;
        }
        Ok(batch)
    }
}
