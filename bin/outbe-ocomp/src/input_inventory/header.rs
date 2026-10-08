use super::*;

pub(super) fn encode_header(header: &InventoryHeaderV1) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(header_len());
    encoded.extend_from_slice(&HEADER_MAGIC);
    encoded.extend_from_slice(header.subject.protocol_bundle_hash.as_slice());
    encoded.extend_from_slice(header.subject.job_id.as_slice());
    encoded.extend_from_slice(&header.subject.attempt.to_be_bytes());
    encoded.extend_from_slice(
        &header
            .subject
            .checkpoint
            .finalized_block_number
            .to_be_bytes(),
    );
    encoded.extend_from_slice(header.subject.checkpoint.finalized_block_hash.as_slice());
    encoded.extend_from_slice(header.subject.checkpoint.finalized_state_root.as_slice());
    encoded.extend_from_slice(header.subject.checkpoint.finalized_ce_root.as_slice());
    encoded.extend_from_slice(&header.subject.checkpoint.ce_schema_version.to_be_bytes());
    encoded.extend_from_slice(&header.subject.worldwide_day.value().to_be_bytes());
    encoded.extend_from_slice(header.subject.sealed_tribute_collection_root.as_slice());
    encoded.extend_from_slice(&header.subject.expected_tribute_count.to_be_bytes());
    encoded.extend_from_slice(&header.subject.expected_nominal_total.to_be_bytes::<32>());
    encoded.extend_from_slice(&header.unique_owner_count.to_be_bytes());
    encoded.extend_from_slice(header.owner_file_digest.as_slice());
    encoded.extend_from_slice(header.iso_bitmap_digest.as_slice());
    encoded.extend_from_slice(header.body_file_digest.as_slice());
    encoded.extend_from_slice(&header.exact_body_bytes.to_be_bytes());
    encoded
}

pub(super) fn decode_header(encoded: &[u8]) -> Result<InventoryHeaderV1, TributeInventoryError> {
    if encoded.len() != header_len() || encoded[..8] != HEADER_MAGIC {
        return Err(TributeInventoryError::Corrupt("inventory header"));
    }
    let mut offset = 8;
    let mut take = |count: usize| {
        let start = offset;
        offset += count;
        &encoded[start..offset]
    };
    let protocol_bundle_hash = B256::from_slice(take(32));
    let job_id = B256::from_slice(take(32));
    let attempt = u32::from_be_bytes(take(4).try_into().expect("fixed header slice"));
    let checkpoint = CheckpointIdentityV1 {
        finalized_block_number: u64::from_be_bytes(take(8).try_into().expect("fixed header slice")),
        finalized_block_hash: B256::from_slice(take(32)),
        finalized_state_root: B256::from_slice(take(32)),
        finalized_ce_root: B256::from_slice(take(32)),
        ce_schema_version: u16::from_be_bytes(take(2).try_into().expect("fixed header slice")),
    };
    let worldwide_day = WorldwideDay::new(u32::from_be_bytes(
        take(4).try_into().expect("fixed header slice"),
    ));
    let sealed_tribute_collection_root = B256::from_slice(take(32));
    let expected_tribute_count =
        u32::from_be_bytes(take(4).try_into().expect("fixed header slice"));
    let expected_nominal_total = U256::from_be_slice(take(32));
    let unique_owner_count = u64::from_be_bytes(take(8).try_into().expect("fixed header slice"));
    let owner_file_digest = B256::from_slice(take(32));
    let iso_bitmap_digest = B256::from_slice(take(32));
    let body_file_digest = B256::from_slice(take(32));
    let exact_body_bytes = u64::from_be_bytes(take(8).try_into().expect("fixed header slice"));
    let header = InventoryHeaderV1 {
        subject: TributeInventorySubjectV1 {
            protocol_bundle_hash,
            job_id,
            attempt,
            checkpoint,
            worldwide_day,
            sealed_tribute_collection_root,
            expected_tribute_count,
            expected_nominal_total,
        },
        unique_owner_count,
        owner_file_digest,
        iso_bitmap_digest,
        body_file_digest,
        exact_body_bytes,
    };
    if !header.subject.worldwide_day.is_valid() {
        return Err(TributeInventoryError::Corrupt("inventory worldwide day"));
    }
    Ok(header)
}

pub(super) const fn header_len() -> usize {
    8 + 32 + 32 + 4 + 8 + 32 + 32 + 32 + 2 + 4 + 32 + 4 + 32 + 8 + 32 + 32 + 32 + 8
}
