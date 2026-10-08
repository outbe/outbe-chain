//! Decode finalized logs and load the domain projection sessions.

use super::super::FinalizedReceipt;
use super::*;
use outbe_nod::projection::NodProjectionSession;
use outbe_tribute::projection::TributeProjectionSession;

type DecodedReceipts<'a> = Vec<(&'a FinalizedReceipt, Vec<ProjectionEvent>)>;

pub(super) fn decode_receipts(
    block: &FinalizedBlock,
) -> Result<DecodedReceipts<'_>, ProjectionError> {
    let mut decoded_receipts = Vec::with_capacity(block.receipts.len());
    for receipt in &block.receipts {
        let mut events = Vec::new();
        for log in &receipt.logs {
            let Some(signature) = log.data.topics().first().copied() else {
                continue;
            };
            let source = ProjectionSource {
                block_number: block.number,
                block_hash: block.hash,
                tx_hash: receipt.tx_hash,
                transaction_index: receipt.transaction_index,
                log_index: log.log_index,
                emitter: log.emitter,
                event_signature: signature,
            };
            let recognized = is_projection_pair(log.emitter, signature);
            if !receipt.success {
                if recognized {
                    return Err(ProjectionError::ProjectionLogInFailedReceipt(Box::new(
                        source,
                    )));
                }
                continue;
            }
            if let Some(event) = decode_event(source, &log.data)? {
                events.push(event);
            }
        }
        decoded_receipts.push((receipt, events));
    }
    Ok(decoded_receipts)
}

pub(super) struct ProjectionSessions {
    pub(super) tributes: TributeProjectionSession,
    pub(super) nods: NodProjectionSession,
    pub(super) retained_tribute_reader: RetainedTributeReader,
    pub(super) tribute_ids: Vec<WwdEntityId>,
}

impl ProjectionSessions {
    pub(super) fn load(
        projector: &OffchainDataProjection,
        decoded_receipts: &DecodedReceipts<'_>,
    ) -> Result<Self, ProjectionError> {
        let (tribute_reader, nod_reader) = match &projector.day_route {
            Some(route) => (
                TributeRepositoryReader::with_days(
                    route.durable_reader.clone(),
                    route.durable_writer.clone(),
                    route.databases.clone(),
                ),
                outbe_nod::nod_reader(route.durable_reader.clone())
                    .with_days(route.durable_writer.clone(), route.databases.clone()),
            ),
            None => (
                TributeRepositoryReader::new(projector.reader.clone()),
                outbe_nod::nod_reader(projector.reader.clone()),
            ),
        };
        let retained_tribute_reader = RetainedTributeReader::new(projector.reader.clone());
        let mut tribute_ids = BTreeSet::new();
        let mut nod_ids = BTreeSet::new();
        let mut bucket_ids = BTreeSet::new();

        for (_, events) in decoded_receipts {
            for event in events {
                match event.identity() {
                    None => {}
                    Some(EntityIdentity::Tribute(id)) => {
                        tribute_ids.insert(id);
                    }
                    Some(EntityIdentity::Nod(id)) => {
                        nod_ids.insert(id);
                    }
                    Some(EntityIdentity::Bucket(key)) => {
                        bucket_ids.insert(key);
                    }
                }
                collect_retired_ids(projector, &tribute_reader, event, &mut tribute_ids)?;
            }
        }

        let tribute_ids: Vec<_> = tribute_ids.into_iter().collect();
        let nod_ids: Vec<_> = nod_ids.into_iter().collect();
        let bucket_ids: Vec<_> = bucket_ids.into_iter().collect();
        let tributes = tribute_reader.projection_session(&tribute_ids)?;
        for tribute_id in &tribute_ids {
            validate_existing_record("Tribute", tributes.current_with_metadata(*tribute_id)?)?;
        }
        let nods = nod_reader.projection_session(&nod_ids, &bucket_ids)?;
        for nod_id in &nod_ids {
            validate_existing_record("Nod", nods.current_item_with_metadata(*nod_id)?)?;
        }
        for bucket_id in &bucket_ids {
            validate_existing_record("Nod bucket", nods.current_bucket_with_metadata(*bucket_id)?)?;
        }
        Ok(Self {
            tributes,
            nods,
            retained_tribute_reader,
            tribute_ids,
        })
    }
}

fn collect_retired_ids(
    projector: &OffchainDataProjection,
    tribute_reader: &TributeRepositoryReader,
    event: &ProjectionEvent,
    tribute_ids: &mut BTreeSet<WwdEntityId>,
) -> Result<(), ProjectionError> {
    if projector.day_route.is_some() {
        return Ok(());
    }
    let ProjectionEvent::TributePartitionRetired { worldwide_day } = event else {
        return Ok(());
    };
    let needs_copy = !projector.partition_retirement
        || projector
            .tribute_retention_selector
            .as_ref()
            .map(|selector| selector.active_pin_for(*worldwide_day))
            .transpose()
            .map_err(|reason| ProjectionError::RetentionSelector {
                worldwide_day: *worldwide_day,
                reason,
            })?
            .flatten()
            .is_some();
    if needs_copy {
        super::super::retirement::collect_ids_for_retired_day(
            tribute_reader,
            *worldwide_day,
            tribute_ids,
        )?;
    }
    Ok(())
}
