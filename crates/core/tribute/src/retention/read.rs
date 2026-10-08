use super::*;

pub(super) fn current_body(
    storage: &StorageReaderHandle,
    tribute_id: WwdEntityId,
    expected_commitment: B256,
) -> Result<(Option<Vec<u8>>, bool), TributeRepositoryError> {
    let mut selected = None;
    let mut current_mismatch = false;
    if let Some(current) =
        storage.get_record(namespace(TRIBUTES_NAMESPACE)?, &primary_key(tribute_id)?)?
    {
        let commitment = commitment_for_stored_bytes(tribute_id, current.value.as_bytes())?;
        if commitment == expected_commitment {
            selected = Some(current.value.as_bytes().to_vec());
        } else {
            current_mismatch = true;
        }
    }
    Ok((selected, current_mismatch))
}

pub(super) fn select_retained_body(
    storage: &StorageReaderHandle,
    pin: RetainedTributePin,
    tribute_id: WwdEntityId,
    expected_commitment: B256,
    selected: &mut Option<Vec<u8>>,
) -> Result<(), TributeRepositoryError> {
    let retained_key = retained_key(pin, tribute_id, expected_commitment)?;
    if let Some(retained) =
        storage.get_record(namespace(OCOMP_RETAINED_TRIBUTES_NAMESPACE)?, &retained_key)?
    {
        if retained.metadata.is_some() {
            return Err(TributeRepositoryError::RetainedMetadata {
                job_id: pin.input_lease_id,
                tribute_id,
            });
        }
        let commitment = commitment_for_stored_bytes(tribute_id, retained.value.as_bytes())?;
        if commitment != expected_commitment {
            return Err(TributeRepositoryError::RetainedCommitmentMismatch {
                job_id: pin.input_lease_id,
                tribute_id,
            });
        }
        validate_retained_index_record(storage, &retained_key, pin)?;
        if selected
            .as_ref()
            .is_some_and(|current| current.as_slice() != retained.value.as_bytes())
        {
            return Err(TributeRepositoryError::ConflictingRetainedBody {
                job_id: pin.input_lease_id,
                tribute_id,
            });
        }
        *selected = Some(retained.value.as_bytes().to_vec());
    } else {
        let conflicting = storage.scan_prefix(
            namespace(OCOMP_RETAINED_TRIBUTES_NAMESPACE)?,
            ScanRequest::new(&retained_identity_prefix(pin, tribute_id), None, 1)?,
        )?;
        if !conflicting.entries.is_empty() {
            return Err(TributeRepositoryError::ConflictingRetainedBody {
                job_id: pin.input_lease_id,
                tribute_id,
            });
        }
    }
    Ok(())
}

pub(super) fn retained_day_body(
    reader: &RetainedTributeView,
    pin: RetainedTributePin,
    tribute_id: WwdEntityId,
    expected_commitment: B256,
) -> Result<Option<Vec<u8>>, TributeRepositoryError> {
    let Some(storage) = retained_day_reader(reader, pin)? else {
        return Ok(None);
    };
    let Some(record) =
        storage.get_record(namespace(TRIBUTES_NAMESPACE)?, &primary_key(tribute_id)?)?
    else {
        return Ok(None);
    };
    let commitment = commitment_for_stored_bytes(tribute_id, record.value.as_bytes())?;
    if commitment != expected_commitment {
        return Err(TributeRepositoryError::RetainedCommitmentMismatch {
            job_id: pin.input_lease_id,
            tribute_id,
        });
    }
    Ok(Some(record.value.as_bytes().to_vec()))
}

fn retained_day_reader(
    reader: &RetainedTributeView,
    pin: RetainedTributePin,
) -> Result<Option<StorageReaderHandle>, TributeRepositoryError> {
    let Some(days) = &reader.days else {
        return Ok(None);
    };
    let Some(TributeDayMark::Retained(lease)) =
        day_mark::read_tribute_day_mark(reader.storage.as_ref(), pin.worldwide_day.value())?
    else {
        return Ok(None);
    };
    if lease != pin.input_lease_id {
        return Ok(None);
    }
    Ok(days
        .tribute_if_present(pin.worldwide_day.value())?
        .map(|storage| storage as StorageReaderHandle))
}
