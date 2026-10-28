use super::*;
use crate::radicle_publication::tests::batch;

#[test]
fn proposer_uses_maximum_ready_prefix_after_reserving_final_fields() {
    use crate::radicle_publication::{RepositoryPublicationMailbox, PUBLICATION_BODY_LEN};
    let mailbox = RepositoryPublicationMailbox::default();
    mailbox.set_ready(Some(batch(32))).unwrap();
    for count in [0, 1, 7, 31, 32] {
        let mut artifacts = OutbeBlockArtifacts {
            radicle_publication_enabled: true,
            consensus_header_artifact: Some(ConsensusHeaderArtifact::DealerLog(Bytes::new())),
            ..Default::default()
        };
        let mut final_fields = artifacts.clone();
        final_fields.execution_summary = Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::MAX,
        });
        final_fields.timestamp_millis_part = 999;
        let final_base_len = encode_outbe_block_artifacts(&final_fields).unwrap().len();
        let available = 3 + PUBLICATION_BODY_LEN + 1 + count * 100;
        let padding =
            OUTBE_MAX_EXTRA_DATA_SIZE - final_base_len - ENCLAVE_WITNESS_RESERVED_BYTES - available;
        artifacts.consensus_header_artifact =
            Some(ConsensusHeaderArtifact::DealerLog(vec![1; padding].into()));
        let encoded = encode_proposal_block_artifacts(artifacts.clone(), &mailbox).unwrap();
        assert_eq!(
            encoded,
            encode_proposal_block_artifacts(artifacts.clone(), &mailbox).unwrap()
        );
        let mut decoded = decode_outbe_block_artifacts(&encoded).unwrap();
        assert_eq!(
            decoded
                .radicle_publication
                .as_ref()
                .map_or(0, |batch| batch.attestations.len()),
            count
        );
        assert_eq!(
            decoded.consensus_header_artifact,
            artifacts.consensus_header_artifact
        );
        decoded.execution_summary = final_fields.execution_summary;
        decoded.timestamp_millis_part = final_fields.timestamp_millis_part;
        let sealed = encode_outbe_block_artifacts(&decoded).unwrap();
        assert!(sealed.len() <= OUTBE_MAX_EXTRA_DATA_SIZE);
        if count > 0 {
            assert_eq!(
                sealed.len() + ENCLAVE_WITNESS_RESERVED_BYTES,
                OUTBE_MAX_EXTRA_DATA_SIZE
            );
            let mut with_max_reserved_witness = decoded.clone();
            with_max_reserved_witness.enclave_witness =
                Some(Bytes::from(vec![0xAA; ENCLAVE_WITNESS_RESERVED_BYTES - 3]));
            let sealed_with_witness =
                encode_outbe_block_artifacts(&with_max_reserved_witness).unwrap();
            assert_eq!(sealed_with_witness.len(), OUTBE_MAX_EXTRA_DATA_SIZE);
        }
    }
    assert!(
        encode_proposal_block_artifacts(OutbeBlockArtifacts::default(), &mailbox)
            .unwrap()
            .is_empty()
    );
    let enabled = OutbeBlockArtifacts {
        radicle_publication_enabled: true,
        ..Default::default()
    };
    assert_eq!(
        decode_outbe_block_artifacts(&encode_proposal_block_artifacts(enabled, &mailbox).unwrap())
            .unwrap()
            .radicle_publication,
        Some(batch(32))
    );
}

#[test]
fn publication_is_an_independent_record_alongside_each_consensus_artifact() {
    for consensus in [
        None,
        Some(ConsensusHeaderArtifact::BoundaryOutcome(
            super::tests::roundtrip_boundary_fixture(),
        )),
        Some(ConsensusHeaderArtifact::DealerLog(Bytes::from_static(
            b"dealer-log",
        ))),
        Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
            epoch: 7,
            outcome: Bytes::from_static(b"outcome"),
        }),
    ] {
        let artifacts = OutbeBlockArtifacts {
            radicle_publication_enabled: true,
            radicle_publication: Some(batch(32)),
            consensus_header_artifact: consensus,
            ..Default::default()
        };
        let encoded = encode_outbe_block_artifacts(&artifacts).unwrap();
        assert_eq!(&encoded[..6], b"OART\x01\x01");
        assert_eq!(decode_outbe_block_artifacts(&encoded).unwrap(), artifacts);
    }
}

#[test]
fn full_publication_batch_fits_with_maximum_other_mandatory_records() {
    let credits = LateFinalizeCreditsArtifact {
        batches: (0..LATE_FINALIZE_MAX_BATCHES)
            .map(|index| PerBlockCredit {
                fb_number: u64::MAX - 2 + index as u64,
                fb_hash: B256::repeat_byte(255),
                epoch: u64::MAX,
                view: u64::MAX,
                parent_view: u64::MAX - 1,
                committee_set_hash: B256::repeat_byte(255),
                signer_bitmap: vec![255; LATE_FINALIZE_MAX_BITMAP_LEN],
                aggregate_signature: [255; LATE_FINALIZE_SIG_LEN],
            })
            .collect(),
    };
    let mut artifacts = OutbeBlockArtifacts {
        radicle_publication_enabled: true,
        execution_summary: Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::MAX,
        }),
        timestamp_millis_part: 999,
        late_finalize_credits: Some(credits),
        ..Default::default()
    };
    let mandatory = encode_outbe_block_artifacts(&artifacts).unwrap();
    artifacts.radicle_publication = Some(batch(32));
    let encoded = encode_outbe_block_artifacts(&artifacts).unwrap();
    assert_eq!(encoded.len(), mandatory.len() + 3681);
    assert!(encoded.len() <= OUTBE_MAX_EXTRA_DATA_SIZE);
    assert_eq!(decode_outbe_block_artifacts(&encoded).unwrap(), artifacts);
    let sanitized = sanitize_prefinal_outbe_block_artifacts(&encoded).unwrap();
    let sanitized = decode_outbe_block_artifacts(&sanitized).unwrap();
    assert!(sanitized.radicle_publication_enabled);
    assert_eq!(sanitized.radicle_publication, artifacts.radicle_publication);
    assert_eq!(
        sanitized.late_finalize_credits,
        artifacts.late_finalize_credits
    );
    assert!(sanitized.execution_summary.is_none());
}

#[test]
fn v1_envelope_preserves_explicit_publication_mode() {
    assert!(
        encode_outbe_block_artifacts(&OutbeBlockArtifacts::default())
            .unwrap()
            .is_empty()
    );
    let mut artifacts = OutbeBlockArtifacts {
        timestamp_millis_part: 999,
        ..Default::default()
    };
    let mut expected = b"OART\x01\x00\x01\x05\x00\x08".to_vec();
    expected.extend_from_slice(&999_u64.to_be_bytes());
    assert_eq!(
        encode_outbe_block_artifacts(&artifacts).unwrap().as_ref(),
        expected
    );
    artifacts.radicle_publication = Some(batch(1));
    assert!(encode_outbe_block_artifacts(&artifacts).is_err());
    artifacts.radicle_publication_enabled = true;
    let encoded = encode_outbe_block_artifacts(&artifacts).unwrap();
    assert_eq!(&encoded[..6], b"OART\x01\x01");
    assert_eq!(decode_outbe_block_artifacts(&encoded).unwrap(), artifacts);

    let enabled_empty = OutbeBlockArtifacts {
        radicle_publication_enabled: true,
        ..Default::default()
    };
    let encoded = encode_outbe_block_artifacts(&enabled_empty).unwrap();
    assert_eq!(encoded.as_ref(), b"OART\x01\x01\x00");
    assert_eq!(
        decode_outbe_block_artifacts(&encoded).unwrap(),
        enabled_empty
    );
}

#[test]
fn v1_envelope_rejects_unknown_flags_and_unsupported_versions() {
    for flags in [2, 3, 255] {
        let bytes = [b'O', b'A', b'R', b'T', 1, flags, 0];
        assert_eq!(
            decode_outbe_block_artifacts(&bytes)
                .unwrap_err()
                .to_string(),
            format!("revert: unsupported block artifact flags: {flags}")
        );
    }
    for version in [0, 2, 5, 11, 12, 255] {
        let bytes = [b'O', b'A', b'R', b'T', version, 0, 0];
        assert_eq!(
            decode_outbe_block_artifacts(&bytes)
                .unwrap_err()
                .to_string(),
            format!("revert: unsupported block artifact version: {version}")
        );
    }
}

#[test]
fn publication_record_rejects_disabled_mode_duplicate_record_and_bad_envelope() {
    let batch = batch(32);
    let encoded = encode_radicle_publication_artifact(&batch).unwrap();
    assert_eq!(encoded.len(), 3688);
    assert_eq!(encoded[6], 1);
    assert_eq!(encoded[7], 0x09);
    assert_eq!(
        decode_radicle_publication_artifact(&encoded).unwrap(),
        Some(batch)
    );
    for flags in [0, 2, 3] {
        let mut invalid = encoded.to_vec();
        invalid[5] = flags;
        assert!(decode_outbe_block_artifacts(&invalid).is_err());
    }
    let mut duplicate = encoded.to_vec();
    duplicate[6] = 2;
    duplicate.extend_from_slice(&encoded[7..]);
    assert!(decode_outbe_block_artifacts(&duplicate).is_err());
    assert!(decode_outbe_block_artifacts(&encoded[..encoded.len() - 1]).is_err());
    let mut trailing = encoded.to_vec();
    trailing.push(0);
    assert!(decode_outbe_block_artifacts(&trailing).is_err());
}
