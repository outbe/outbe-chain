use std::collections::BTreeSet;

use outbe_compressed_entities::{IdPageRequest, WwdEntityId, MAX_ID_PAGE_LIMIT};
use outbe_offchain_storage::AtomicWriteBatch;
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{
    projection::TributeProjectionSession, RetainedTributePin, RetainedTributeReader,
    TributeRepositoryReader,
};

use super::ProjectionError;

pub(super) fn collect_ids_for_retired_day(
    tribute_reader: &TributeRepositoryReader,
    worldwide_day: WorldwideDay,
    tribute_ids: &mut BTreeSet<WwdEntityId>,
) -> Result<(), ProjectionError> {
    let mut after = None;
    loop {
        let page = tribute_reader.list_ids_by_day(
            worldwide_day,
            IdPageRequest {
                after,
                limit: MAX_ID_PAGE_LIMIT,
            },
        )?;
        tribute_ids.extend(page.ids);
        let Some(next) = page.next_after else {
            break;
        };
        after = Some(next);
    }
    Ok(())
}

pub(super) struct RetiredPartition<'a> {
    pub(super) tribute_ids: &'a [WwdEntityId],
    pub(super) worldwide_day: WorldwideDay,
    pub(super) retention_pin: Option<RetainedTributePin>,
}

pub(super) fn plan_retired_partition(
    tributes: &mut TributeProjectionSession,
    retained_tribute_reader: &RetainedTributeReader,
    partition: RetiredPartition<'_>,
    batch: &mut AtomicWriteBatch,
) -> Result<(), ProjectionError> {
    for tribute_id in partition.tribute_ids {
        let belongs_to_partition = tributes
            .current(*tribute_id)?
            .is_some_and(|tribute| tribute.worldwide_day == partition.worldwide_day);
        if belongs_to_partition {
            let planned = match partition.retention_pin {
                Some(pin) => {
                    tributes.retain_then_delete(retained_tribute_reader, pin, *tribute_id)?
                }
                None => tributes.delete(*tribute_id)?,
            };
            batch.extend(planned.operations().iter().cloned());
        }
    }
    Ok(())
}
