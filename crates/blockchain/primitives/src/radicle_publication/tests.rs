use super::*;

#[test]
fn mailbox_is_bounded_nonblocking_and_preserves_proposal_retries() {
    let mailbox = RepositoryPublicationMailbox::default();
    let ready = batch(32);
    mailbox.set_ready(Some(ready.clone())).unwrap();
    assert_eq!(mailbox.ready_prefix(usize::MAX), Some(ready.clone()));
    assert_eq!(mailbox.ready_prefix(usize::MAX), Some(ready.clone()));
    assert!(mailbox.ready_prefix(PUBLICATION_BODY_LEN + 100).is_none());
    assert_eq!(
        mailbox
            .ready_prefix(PUBLICATION_BODY_LEN + 101)
            .unwrap()
            .attestations,
        ready.attestations[..1]
    );
    let held = mailbox.0.lock().unwrap();
    assert!(mailbox.ready_prefix(usize::MAX).is_none());
    drop(held);
    assert!(mailbox.set_ready(Some(batch(33))).is_err());
    assert_eq!(mailbox.ready_prefix(usize::MAX), Some(ready));
    mailbox.set_ready(None).unwrap();
    assert!(mailbox.ready_prefix(usize::MAX).is_none());
}

pub(crate) fn body() -> PublicationBody {
    PublicationBody {
        chain_id: 0x0102030405060708,
        genesis_hash: B256::repeat_byte(1),
        repo_id: FixedBytes::repeat_byte(2),
        descriptor_hash: B256::repeat_byte(3),
        owner_bls_public: FixedBytes::repeat_byte(4),
        players_commitment: B256::repeat_byte(5),
        epoch: 0x1112131415161718,
        material_version: 0x2122232425262728,
        material_hash: B256::repeat_byte(6),
        finalized_anchor_height: 0x3132333435363738,
        finalized_anchor_hash: B256::repeat_byte(7),
        sequence: 0x4142434445464748,
        previous_receipt_hash: B256::repeat_byte(8),
        action: PublicationAction::Review,
        proposal_id: U256::from_be_bytes([12; 32]),
        patch_id: FixedBytes::repeat_byte(13),
        revision_id: FixedBytes::repeat_byte(14),
        expected_target_head: FixedBytes::repeat_byte(15),
        package_manifest_hash: B256::repeat_byte(16),
    }
}

pub(crate) fn batch(count: usize) -> PublicationBatch {
    PublicationBatch {
        body: body(),
        attestations: (0..count)
            .map(|index| PublicationAttestation {
                participant_index: index as u32,
                signature: FixedBytes::repeat_byte(index as u8 + 1),
            })
            .collect(),
    }
}

#[test]
fn exact_opb1_layout_and_receipt_hash() {
    let body = body();
    let bytes = body.to_bytes();
    assert_eq!(bytes.len(), 477);
    assert_eq!(&bytes[..12], b"OPB1\x01\x02\x03\x04\x05\x06\x07\x08");
    assert_eq!(&bytes[96..192], &[4; 96]);
    assert_eq!(&bytes[224..232], &0x1112131415161718_u64.to_be_bytes());
    assert_eq!(&bytes[312..320], &0x4142434445464748_u64.to_be_bytes());
    assert_eq!(bytes[352], 2);
    assert_eq!(&bytes[353..385], &[12; 32]);
    assert_eq!(&bytes[445..], &[16; 32]);
    assert_eq!(PublicationBody::from_bytes(&bytes).unwrap(), body);
    // Independent Python hashlib vector over the fixed P0 field layout.
    assert_eq!(
        hex::encode(body.receipt_hash()),
        "45c65647bce33a6c97600c64c3c9fd1b8068b0d308bc20e43411dddf86ff6b83"
    );
    for action in [
        PublicationAction::Bootstrap,
        PublicationAction::BindMaterial,
        PublicationAction::Review,
        PublicationAction::Merge,
    ] {
        let mut next = body.clone();
        next.action = action;
        assert_eq!(PublicationBody::from_bytes(&next.to_bytes()).unwrap(), next);
    }
}

#[test]
fn malformed_body_and_noncanonical_lengths_reject_without_panics() {
    let bytes = body().to_bytes();
    for length in 0..PUBLICATION_BODY_LEN {
        assert!(PublicationBody::from_bytes(&bytes[..length]).is_err());
    }
    let mut extra = bytes.to_vec();
    extra.push(0);
    assert!(PublicationBody::from_bytes(&extra).is_err());
    for offset in [0, 3, 352] {
        let mut invalid = bytes;
        invalid[offset] = 0xff;
        assert!(PublicationBody::from_bytes(&invalid).is_err());
    }
}

#[test]
fn bounded_batch_exact_sizes_and_all_valid_counts_roundtrip() {
    for count in 1..=32 {
        let batch = batch(count);
        let bytes = batch.to_bytes().unwrap();
        assert_eq!(bytes.len(), 477 + 1 + count * 100);
        assert_eq!(bytes[477], count as u8);
        assert_eq!(PublicationBatch::from_bytes(&bytes).unwrap(), batch);
    }
    assert_eq!(
        batch(32).to_bytes().unwrap().len(),
        MAX_PUBLICATION_BATCH_LEN
    );
    assert_eq!(MAX_PUBLICATION_BATCH_LEN, 3678);
    assert!(batch(0).to_bytes().is_err());
    assert!(batch(33).to_bytes().is_err());
}

#[test]
fn batch_rejects_duplicate_unsorted_count_and_trailing_or_truncated_bytes() {
    let valid = batch(3).to_bytes().unwrap();
    for length in 0..valid.len() {
        assert!(PublicationBatch::from_bytes(&valid[..length]).is_err());
    }
    let mut trailing = valid.clone();
    trailing.push(0);
    assert!(PublicationBatch::from_bytes(&trailing).is_err());
    for count in [0, 1, 2, 4, 33, 255] {
        let mut invalid = valid.clone();
        invalid[477] = count;
        assert!(PublicationBatch::from_bytes(&invalid).is_err());
    }
    for index in [0, 2] {
        let mut invalid = batch(3);
        invalid.attestations[1].participant_index = index;
        assert!(invalid.to_bytes().is_err());
        let mut bytes = valid.clone();
        bytes[578..582].copy_from_slice(&index.to_be_bytes());
        assert!(PublicationBatch::from_bytes(&bytes).is_err());
    }
}
