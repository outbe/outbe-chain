use super::payload;
use super::*;

pub(in crate::ocomp::retention) fn encode_record(record: PinRecordV1) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(PIN_RECORD_MAX_BYTES);
    encoded.extend_from_slice(&JOURNAL_MAGIC);
    encoded.extend_from_slice(&PIN_RECORD_VERSION.to_be_bytes());
    encoded.extend_from_slice(&record.generation.to_be_bytes());
    encoded.push(state_tag(record.state));
    encode_candidate(&mut encoded, record_candidate(record));
    encode_state_payload(&mut encoded, record.state);
    append_checksum(encoded)
}

fn encode_state_payload(encoded: &mut Vec<u8>, state: PinStateV1) {
    encode_finalized_prefix(encoded, state);
    match state {
        PinStateV1::AwaitingJobFinalization { .. } | PinStateV1::Finalized { .. } => {}
        PinStateV1::Exported { export, .. } => encode_export_authority(encoded, export),
        PinStateV1::Terminal {
            source_generation,
            export,
            terminal_height,
            release_height,
            ..
        }
        | PinStateV1::GcPending {
            source_generation,
            export,
            terminal_height,
            release_height,
            ..
        } => {
            encoded.extend_from_slice(&source_generation.to_be_bytes());
            authority::encode_optional_export(encoded, export);
            encoded.extend_from_slice(&terminal_height.to_be_bytes());
            encoded.extend_from_slice(&release_height.to_be_bytes());
        }
        PinStateV1::Released {
            job_id,
            source_generation,
            observed_height,
            export,
            ..
        } => {
            encoded.extend_from_slice(job_id.as_slice());
            encoded.extend_from_slice(&source_generation.to_be_bytes());
            authority::encode_optional_export(encoded, export);
            encoded.extend_from_slice(&observed_height.to_be_bytes());
        }
    }
}

fn encode_finalized_prefix(encoded: &mut Vec<u8>, state: PinStateV1) {
    if let PinStateV1::Finalized {
        job_id,
        finality_recorded_height,
        open_height,
        deadline_height,
        ..
    }
    | PinStateV1::Exported {
        job_id,
        finality_recorded_height,
        open_height,
        deadline_height,
        ..
    }
    | PinStateV1::Terminal {
        job_id,
        finality_recorded_height,
        open_height,
        deadline_height,
        ..
    }
    | PinStateV1::GcPending {
        job_id,
        finality_recorded_height,
        open_height,
        deadline_height,
        ..
    } = state
    {
        encoded.extend_from_slice(job_id.as_slice());
        encode_finalized_window(
            encoded,
            finality_recorded_height,
            open_height,
            deadline_height,
        );
    }
}

fn state_tag(state: PinStateV1) -> u8 {
    match state {
        PinStateV1::AwaitingJobFinalization { .. } => 1,
        PinStateV1::Finalized { .. } => 2,
        PinStateV1::Exported { .. } => 3,
        PinStateV1::Terminal { .. } => 4,
        PinStateV1::Released { .. } => 5,
        PinStateV1::GcPending { .. } => 6,
    }
}
fn encode_candidate(encoded: &mut Vec<u8>, candidate: CandidatePinV1) {
    encoded.extend_from_slice(&candidate.block_number.to_be_bytes());
    encoded.extend_from_slice(candidate.block_hash.as_slice());
    encoded.extend_from_slice(candidate.state_root.as_slice());
    encoded.extend_from_slice(candidate.intent_id.as_slice());
    encoded.extend_from_slice(&candidate.wwd.to_be_bytes());
    encoded.extend_from_slice(candidate.ce_sealed_root.as_slice());
    encoded.extend_from_slice(candidate.protocol_bundle_hash.as_slice());
    encoded.extend_from_slice(candidate.input_lease_id.as_slice());
}

fn encode_finalized_window(
    encoded: &mut Vec<u8>,
    finality_recorded_height: u64,
    open_height: u64,
    deadline_height: u64,
) {
    encoded.extend_from_slice(&finality_recorded_height.to_be_bytes());
    encoded.extend_from_slice(&open_height.to_be_bytes());
    encoded.extend_from_slice(&deadline_height.to_be_bytes());
}

pub(super) fn decode_record(encoded: &[u8]) -> Result<PinRecordV1, RetentionError> {
    let body = checked_body(
        encoded,
        JOURNAL_MAGIC.len() + 2 + 8 + 1 + 32,
        "truncated header",
    )?;
    let mut reader = JournalReader::new(body);
    read_version(&mut reader, PIN_RECORD_VERSION)?;
    let generation = read_generation(&mut reader, "zero generation")?;
    let tag = reader.take::<1>()?[0];
    let candidate = decode_candidate(&mut reader)?;
    let state = match tag {
        1 => PinStateV1::AwaitingJobFinalization { candidate },
        2 => payload::FinalizedFields::read(&mut reader)?.finalized(candidate),
        3 => {
            let fields = payload::FinalizedFields::read(&mut reader)?;
            fields.exported(candidate, decode_export_authority(&mut reader)?)
        }
        4 => payload::decode_terminal(&mut reader, candidate, authority::TerminalKind::Terminal)?,
        5 => payload::decode_released(&mut reader, candidate)?,
        6 => payload::decode_terminal(&mut reader, candidate, authority::TerminalKind::GcPending)?,
        _ => return Err(RetentionError::MalformedJournal("unknown state tag")),
    };
    reader.finish()?;
    Ok(PinRecordV1 { generation, state })
}

fn decode_candidate(reader: &mut JournalReader<'_>) -> Result<CandidatePinV1, RetentionError> {
    Ok(CandidatePinV1 {
        block_number: u64::from_be_bytes(reader.take::<8>()?),
        block_hash: B256::new(reader.take::<32>()?),
        state_root: B256::new(reader.take::<32>()?),
        intent_id: B256::new(reader.take::<32>()?),
        wwd: u32::from_be_bytes(reader.take::<4>()?),
        ce_sealed_root: B256::new(reader.take::<32>()?),
        protocol_bundle_hash: B256::new(reader.take::<32>()?),
        input_lease_id: B256::new(reader.take::<32>()?),
    })
}
