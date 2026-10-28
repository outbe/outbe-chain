use alloy_primitives::{address, Address, Bytes, B256, U256};

use super::{
    decode_boundary_artifact, decode_consensus_header_artifact, decode_outbe_block_artifacts,
    encode_boundary_artifact, encode_consensus_header_artifact, encode_outbe_block_artifacts,
    sanitize_prefinal_outbe_block_artifacts, tee_expired_target_exclusions_hash,
    ConsensusHeaderArtifact, ExecutionSummaryArtifact, OutbeBlockArtifacts,
};
use crate::consensus::{DkgBoundaryArtifact, ReshareResult};

fn boundary_with_expiry_exclusions(exclusions: Vec<Address>) -> DkgBoundaryArtifact {
    let exclusions_hash = tee_expired_target_exclusions_hash(&exclusions).unwrap();
    DkgBoundaryArtifact {
        epoch: 1,
        dkg_cycle: 1,
        freeze_height: 100,
        planned_activation_height: 120,
        target_set_hash: B256::with_last_byte(0x11),
        vrf_material_version: 1,
        vrf_group_public_key: alloy_primitives::keccak256([]),
        vrf_group_public_key_bytes: Bytes::new(),
        committee_set_hash: B256::with_last_byte(0x22),
        is_validator_set_change: true,
        outcome: Bytes::new(),
        is_full_dkg: false,
        reshare: ReshareResult {
            new_active_set: Vec::new(),
            active_set_hash: B256::ZERO,
        },
        tee_expired_target_exclusions: exclusions,
        tee_expired_target_exclusions_hash: exclusions_hash,
    }
}

#[test]
fn boundary_roundtrip_binds_ordered_tee_expiry_exclusions() {
    let exclusions = vec![
        address!("0x1111111111111111111111111111111111111111"),
        address!("0x2222222222222222222222222222222222222222"),
    ];
    let artifact = boundary_with_expiry_exclusions(exclusions.clone());
    let encoded = encode_boundary_artifact(&artifact).unwrap();
    let decoded = decode_boundary_artifact(&encoded).unwrap().unwrap();
    assert_eq!(decoded, artifact);
    assert_eq!(decoded.tee_expired_target_exclusions, exclusions);
}

#[test]
fn boundary_wire_ends_after_tee_expiry_exclusions() {
    let artifact = boundary_with_expiry_exclusions(Vec::new());
    let encoded = encode_boundary_artifact(&artifact).unwrap();

    // OART envelope (7 bytes) + boundary record header (3 bytes) precede the
    // payload. With empty variable-length collections, the canonical boundary
    // payload ends at the expiry-exclusions count and is exactly 214 bytes.
    let payload_len = u16::from_be_bytes([encoded[8], encoded[9]]) as usize;
    assert_eq!(payload_len, 214);
    assert_eq!(encoded.len(), 7 + 3 + payload_len);
}

#[test]
fn boundary_wire_rejects_data_after_tee_expiry_exclusions() {
    let artifact = boundary_with_expiry_exclusions(Vec::new());
    let mut encoded = encode_boundary_artifact(&artifact).unwrap().to_vec();
    let payload_len = u16::from_be_bytes([encoded[8], encoded[9]]);
    encoded[8..10].copy_from_slice(&(payload_len + 1).to_be_bytes());
    encoded.push(0);

    assert!(decode_boundary_artifact(&encoded).is_err());
}

#[test]
fn boundary_rejects_duplicate_oversized_and_tampered_tee_expiry_exclusions() {
    let validator = address!("0x1111111111111111111111111111111111111111");
    assert!(tee_expired_target_exclusions_hash(&[validator, validator]).is_err());

    let oversized: Vec<_> = (0..=crate::validators::MAX_TEE_EXPIRED_TARGET_EXCLUSIONS)
        .map(|index| {
            let word = U256::from(index + 1).to_be_bytes::<32>();
            Address::from_slice(&word[12..])
        })
        .collect();
    assert!(tee_expired_target_exclusions_hash(&oversized).is_err());

    let artifact = boundary_with_expiry_exclusions(vec![validator]);
    let mut encoded = encode_boundary_artifact(&artifact).unwrap().to_vec();
    // The sole exclusion address is the final value in the payload.
    let address_last_byte = encoded.len() - 1;
    encoded[address_last_byte] ^= 0x01;
    assert!(decode_boundary_artifact(&encoded).is_err());
}

pub(super) fn roundtrip_boundary_fixture() -> DkgBoundaryArtifact {
    DkgBoundaryArtifact {
        epoch: 7,
        dkg_cycle: 1,
        freeze_height: 100,
        planned_activation_height: 200,
        target_set_hash: B256::with_last_byte(0x41),
        vrf_material_version: 1,
        vrf_group_public_key: B256::with_last_byte(0x42),
        vrf_group_public_key_bytes: Bytes::from_static(b"\x22\x22\x22"),
        committee_set_hash: B256::with_last_byte(0x4F),
        is_validator_set_change: true,
        outcome: Bytes::from_static(b"dkg-outcome"),
        is_full_dkg: true,
        tee_expired_target_exclusions: Vec::new(),
        tee_expired_target_exclusions_hash: B256::ZERO,
        reshare: ReshareResult {
            new_active_set: vec![
                address!("0x1111111111111111111111111111111111111111"),
                address!("0x2222222222222222222222222222222222222222"),
            ],
            active_set_hash: B256::with_last_byte(0x41),
        },
    }
}

#[test]
fn roundtrip_block_artifacts_with_execution_summary_and_boundary() {
    let boundary = roundtrip_boundary_fixture();
    let summary = ExecutionSummaryArtifact {
        validator_fee_sum: U256::from(3u64),
    };

    let encoded = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        radicle_publication_enabled: false,
        radicle_publication: None,
        execution_summary: Some(summary),
        consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary.clone())),
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        enclave_witness: None,
    })
    .unwrap();
    let decoded = decode_outbe_block_artifacts(&encoded).unwrap();

    assert_eq!(decoded.execution_summary, Some(summary));
    assert_eq!(
        decoded.consensus_header_artifact,
        Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary))
    );
}

#[test]
fn roundtrip_boundary_header_artifact_wrapper() {
    let result = roundtrip_boundary_fixture();

    let encoded = encode_boundary_artifact(&result).unwrap();
    let decoded = decode_boundary_artifact(&encoded).unwrap();
    assert_eq!(decoded, Some(result));
}

#[test]
fn roundtrip_dealer_log_header_artifact() {
    let encoded = encode_consensus_header_artifact(&ConsensusHeaderArtifact::DealerLog(
        Bytes::from_static(b"dealer-log"),
    ))
    .unwrap();
    let decoded = decode_consensus_header_artifact(&encoded).unwrap();
    assert_eq!(
        decoded,
        Some(ConsensusHeaderArtifact::DealerLog(Bytes::from_static(
            b"dealer-log"
        )))
    );
    assert_eq!(decode_boundary_artifact(&encoded).unwrap(), None);
}

#[test]
fn roundtrip_committee_preannounce_header_artifact() {
    let artifact = ConsensusHeaderArtifact::CommitteePreAnnounce {
        epoch: 7,
        outcome: Bytes::from_static(b"dkg-outcome-bytes"),
    };
    let encoded = encode_consensus_header_artifact(&artifact).unwrap();
    assert_eq!(
        decode_consensus_header_artifact(&encoded).unwrap(),
        Some(artifact)
    );
    // A pre-announce carries a committee for a follower. It is NOT an
    // activating boundary.
    assert_eq!(decode_boundary_artifact(&encoded).unwrap(), None);
}

#[test]
fn committee_preannounce_preserves_epoch_with_empty_outcome() {
    let artifact = ConsensusHeaderArtifact::CommitteePreAnnounce {
        epoch: 0xABCD,
        outcome: Bytes::new(),
    };
    let encoded = encode_consensus_header_artifact(&artifact).unwrap();
    match decode_consensus_header_artifact(&encoded).unwrap() {
        Some(ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, outcome }) => {
            assert_eq!(epoch, 0xABCD);
            assert!(outcome.is_empty());
        }
        other => panic!("expected CommitteePreAnnounce, got {other:?}"),
    }
}

#[test]
fn empty_extra_data_has_no_artifacts() {
    let decoded = decode_outbe_block_artifacts(&[]).unwrap();
    assert_eq!(decoded, OutbeBlockArtifacts::default());
    assert_eq!(decode_boundary_artifact(&[]).unwrap(), None);
}

#[test]
fn unknown_non_empty_extra_data_is_rejected() {
    assert!(decode_boundary_artifact(b"NOPEnot-reshare").is_err());
}

#[test]
fn legacy_finalized_parent_header_tag_is_rejected() {
    let mut buf = Vec::new();
    buf.extend_from_slice(super::MAGIC);
    buf.push(super::VERSION);
    buf.push(0u8); // Publication is off.
    buf.push(1u8);
    buf.push(0x04);
    buf.extend_from_slice(&0u16.to_be_bytes());
    let err = decode_outbe_block_artifacts(&buf).unwrap_err();
    assert!(format!("{err}").contains("unsupported block artifact tag: 4"));
}

#[test]
fn truncated_payload_is_rejected() {
    let mut encoded = encode_boundary_artifact(&DkgBoundaryArtifact {
        epoch: 1,
        dkg_cycle: 1,
        freeze_height: 10,
        planned_activation_height: 20,
        target_set_hash: B256::ZERO,
        vrf_material_version: 1,
        vrf_group_public_key: B256::ZERO,
        vrf_group_public_key_bytes: Bytes::new(),
        committee_set_hash: B256::ZERO,
        is_validator_set_change: false,
        outcome: Bytes::from_static(b"x"),
        is_full_dkg: false,
        tee_expired_target_exclusions: Vec::new(),
        tee_expired_target_exclusions_hash: B256::ZERO,
        reshare: ReshareResult {
            new_active_set: vec![],
            active_set_hash: B256::ZERO,
        },
    })
    .unwrap();
    let truncated_len = encoded.len() - 1;
    encoded.truncate(truncated_len);
    assert!(decode_outbe_block_artifacts(&encoded).is_err());
}

// ---- TC-8: codec coverage (round-trip variety, size limit, tag rejection) ----

use crate::consensus::OUTBE_MAX_EXTRA_DATA_SIZE;
use proptest::prelude::*;

/// Build a `DkgBoundaryArtifact` with caller-controlled variable-length
/// fields so tests can sweep the field matrix without re-listing every
/// fixed field at each call site.
fn make_boundary(
    validator_count: usize,
    outcome_len: usize,
    vrf_bytes_len: usize,
    is_validator_set_change: bool,
    is_full_dkg: bool,
) -> DkgBoundaryArtifact {
    let new_active_set = (0..validator_count)
        .map(|i| {
            let mut raw = [0u8; 20];
            raw[19] = (i & 0xff) as u8;
            raw[18] = ((i >> 8) & 0xff) as u8;
            Address::from(raw)
        })
        .collect::<Vec<_>>();
    DkgBoundaryArtifact {
        epoch: 7,
        dkg_cycle: 1,
        freeze_height: 100,
        planned_activation_height: 200,
        target_set_hash: B256::with_last_byte(0x41),
        vrf_material_version: 3,
        vrf_group_public_key: B256::with_last_byte(0x42),
        vrf_group_public_key_bytes: Bytes::from(vec![0x22u8; vrf_bytes_len]),
        committee_set_hash: B256::with_last_byte(0x4F),
        is_validator_set_change,
        outcome: Bytes::from(vec![0xABu8; outcome_len]),
        is_full_dkg,
        tee_expired_target_exclusions: Vec::new(),
        tee_expired_target_exclusions_hash: B256::ZERO,
        reshare: ReshareResult {
            new_active_set,
            active_set_hash: B256::with_last_byte(0x41),
        },
    }
}

/// Assert byte-for-byte determinism: decode(encode(x)) == x AND
/// encode(decode(encode(x))) == encode(x).
fn assert_roundtrip(artifacts: &OutbeBlockArtifacts) {
    let encoded = encode_outbe_block_artifacts(artifacts).expect("encode");
    let decoded = decode_outbe_block_artifacts(&encoded).expect("decode");
    assert_eq!(&decoded, artifacts, "decoded value must equal original");
    let re_encoded = encode_outbe_block_artifacts(&decoded).expect("re-encode");
    assert_eq!(
        re_encoded, encoded,
        "encode(decode(encoded)) must be byte-identical"
    );
}

#[test]
fn roundtrip_matrix_summary_x_consensus_header() {
    // {execution_summary present/absent} x {none / BoundaryOutcome / DealerLog},
    // crossed with boundary-field boundary values and the timestamp field.
    let summaries = [
        None,
        Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        }),
        Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::from(1u64),
        }),
        Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::MAX,
        }),
    ];

    let consensus_headers: Vec<Option<ConsensusHeaderArtifact>> = vec![
        None,
        // empty variable fields
        Some(ConsensusHeaderArtifact::BoundaryOutcome(make_boundary(
            0, 0, 0, false, false,
        ))),
        // non-empty variable fields, flags set
        Some(ConsensusHeaderArtifact::BoundaryOutcome(make_boundary(
            3, 11, 5, true, true,
        ))),
        // DealerLog: empty and non-empty
        Some(ConsensusHeaderArtifact::DealerLog(Bytes::new())),
        Some(ConsensusHeaderArtifact::DealerLog(Bytes::from(vec![
            0x07u8;
            4096
        ]))),
    ];

    let timestamps = [0u64, 1, 999, u64::MAX];
    for summary in &summaries {
        for header in &consensus_headers {
            for &ts in &timestamps {
                assert_roundtrip(&OutbeBlockArtifacts {
                    radicle_publication_enabled: false,
                    radicle_publication: None,
                    execution_summary: *summary,
                    consensus_header_artifact: header.clone(),
                    timestamp_millis_part: ts,
                    late_finalize_credits: None,
                    enclave_witness: None,
                });
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn proptest_roundtrip_block_artifacts(
        // U256 from 32 arbitrary bytes
        fee_bytes in proptest::collection::vec(any::<u8>(), 32..=32),
        has_summary in any::<bool>(),
        // 0 = none, 1 = BoundaryOutcome, 2 = DealerLog
        header_kind in 0u8..3,
        validator_count in 0usize..8,
        outcome in proptest::collection::vec(any::<u8>(), 0..64),
        vrf_bytes in proptest::collection::vec(any::<u8>(), 0..64),
        dealer_log in proptest::collection::vec(any::<u8>(), 0..256),
        is_vsc in any::<bool>(),
        is_full in any::<bool>(),
        timestamp_millis_part in any::<u64>(),
    ) {
        let execution_summary = has_summary.then(|| ExecutionSummaryArtifact {
            validator_fee_sum: U256::from_be_slice(&fee_bytes),
        });

        let consensus_header_artifact = match header_kind {
            1 => {
                let mut b = make_boundary(validator_count, 0, 0, is_vsc, is_full);
                b.outcome = Bytes::from(outcome.clone());
                b.vrf_group_public_key_bytes = Bytes::from(vrf_bytes.clone());
                Some(ConsensusHeaderArtifact::BoundaryOutcome(b))
            }
            2 => Some(ConsensusHeaderArtifact::DealerLog(Bytes::from(dealer_log.clone()))),
            _ => None,
        };

        let artifacts = OutbeBlockArtifacts {
            radicle_publication_enabled: false,
            radicle_publication: None,
            execution_summary,
            consensus_header_artifact,
            timestamp_millis_part,
            late_finalize_credits: None,
            enclave_witness: None,
        };

        let encoded = encode_outbe_block_artifacts(&artifacts).expect("encode");
        let decoded = decode_outbe_block_artifacts(&encoded).expect("decode");
        prop_assert_eq!(&decoded, &artifacts);
        let re_encoded = encode_outbe_block_artifacts(&decoded).expect("re-encode");
        prop_assert_eq!(re_encoded, encoded);
    }
}

#[test]
fn encode_rejects_artifacts_over_extra_data_budget() {
    // A single boundary record whose payload sits just under the per-record
    // u16 cap (65535) but whose total framed length (7-byte envelope +
    // 3-byte record header + payload) exceeds OUTBE_MAX_EXTRA_DATA_SIZE.
    //
    // Fixed boundary payload prefix is 214 bytes. The rest is the outcome
    // blob. We size the outcome so the total framed length is just over the
    // 64 KiB budget while the record payload stays <= 65535.
    // (180 base fields + 32-byte expiry-exclusions commitment + 2-byte
    // exclusions count.)
    const FIXED_PREFIX: usize = 8 + 8 + 8 + 8 + 32 + 8 + 32 + 32 + 1 + 1 + 32 + 2 + 4 + 4 + 32 + 2;
    const ENVELOPE: usize = 7; // Magic, version, flags, and record count.
    const RECORD_HEADER: usize = 1 + 2; // tag + u16 length

    // Oversize: record payload at the per-record u16 cap (65535). Framed
    // (envelope + record header + payload) this is 65544 > 64 KiB, so the
    // u16 cap passes but the total-length budget check rejects it.
    let oversize_payload_len = u16::MAX as usize;
    let oversize_total = ENVELOPE + RECORD_HEADER + oversize_payload_len;
    assert!(
        oversize_total > OUTBE_MAX_EXTRA_DATA_SIZE,
        "test premise: 65535-byte record must exceed 64 KiB budget once framed ({oversize_total})"
    );
    let oversize_outcome_len = oversize_payload_len - FIXED_PREFIX; // validator_count=0, vrf_bytes=0
    let oversize =
        OutbeBlockArtifacts {
            radicle_publication_enabled: false,
            radicle_publication: None,
            execution_summary: None,
            consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(
                make_boundary(0, oversize_outcome_len, 0, false, false),
            )),
            timestamp_millis_part: 0,
            late_finalize_credits: None,
            enclave_witness: None,
        };
    let err = encode_outbe_block_artifacts(&oversize)
        .expect_err("encoding past the extra_data budget must be rejected");
    assert!(
        format!("{err}").contains("exceed extra_data budget"),
        "unexpected error: {err}"
    );

    // At the limit: size the payload so the framed total is exactly
    // OUTBE_MAX_EXTRA_DATA_SIZE (the budget check is strictly greater-than,
    // so equal must be accepted).
    let limit_payload_len = OUTBE_MAX_EXTRA_DATA_SIZE - ENVELOPE - RECORD_HEADER;
    let under_outcome_len = limit_payload_len - FIXED_PREFIX;
    let under_total = ENVELOPE + RECORD_HEADER + FIXED_PREFIX + under_outcome_len;
    assert_eq!(under_total, OUTBE_MAX_EXTRA_DATA_SIZE);
    let at_limit =
        OutbeBlockArtifacts {
            radicle_publication_enabled: false,
            radicle_publication: None,
            execution_summary: None,
            consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(
                make_boundary(0, under_outcome_len, 0, false, false),
            )),
            timestamp_millis_part: 0,
            late_finalize_credits: None,
            enclave_witness: None,
        };
    let encoded = encode_outbe_block_artifacts(&at_limit).expect("at-limit artifact must encode");
    assert_eq!(encoded.len(), OUTBE_MAX_EXTRA_DATA_SIZE);
    // And it still round-trips.
    let decoded = decode_outbe_block_artifacts(&encoded).expect("decode at-limit");
    assert_eq!(decoded, at_limit);
}

#[test]
fn decode_rejects_reserved_tag_0x04() {
    // The v1 header precedes one record with the reserved tag 0x04.
    let mut buf = Vec::new();
    buf.extend_from_slice(super::MAGIC);
    buf.push(super::VERSION);
    buf.push(0u8); // Publication is off.
    buf.push(1u8); // one record
    buf.push(0x04u8); // legacy tag
    let payload: &[u8] = b"legacy-finalized-parent-metadata";
    buf.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    buf.extend_from_slice(payload);

    let err = decode_outbe_block_artifacts(&buf)
        .expect_err("legacy tag 0x04 must be rejected by the active codec");
    assert!(
        format!("{err}").contains("unsupported block artifact tag: 4"),
        "unexpected error: {err}"
    );

    // Sanity positives: a valid 0x02 (BoundaryOutcome) and 0x03 (DealerLog)
    // record decode Ok, proving the rejection is specific to 0x04.
    let boundary_encoded = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        radicle_publication_enabled: false,
        radicle_publication: None,
        execution_summary: None,
        consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(make_boundary(
            2, 4, 4, true, false,
        ))),
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        enclave_witness: None,
    })
    .expect("boundary encode");
    // Byte 7 is the first record tag after the v1 header.
    assert_eq!(boundary_encoded[7], super::BOUNDARY_TAG);
    assert!(decode_outbe_block_artifacts(&boundary_encoded).is_ok());

    let dealer_encoded = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        radicle_publication_enabled: false,
        radicle_publication: None,
        execution_summary: None,
        consensus_header_artifact: Some(ConsensusHeaderArtifact::DealerLog(Bytes::from_static(
            b"dealer-log",
        ))),
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        enclave_witness: None,
    })
    .expect("dealer encode");
    assert_eq!(dealer_encoded[7], super::DEALER_LOG_TAG);
    assert!(decode_outbe_block_artifacts(&dealer_encoded).is_ok());
}

fn sample_credit(fb_number: u64, last: u8) -> super::PerBlockCredit {
    super::PerBlockCredit {
        fb_number,
        fb_hash: B256::with_last_byte(last),
        epoch: 3,
        view: fb_number + 10,
        parent_view: fb_number + 9,
        committee_set_hash: B256::with_last_byte(0xC0),
        signer_bitmap: vec![0b0000_0111],
        aggregate_signature: [last; super::LATE_FINALIZE_SIG_LEN],
    }
}

#[test]
fn late_credits_codec_roundtrip() {
    let artifact = super::LateFinalizeCreditsArtifact {
        batches: vec![sample_credit(10, 0xAA), sample_credit(11, 0xBB)],
    };
    let encoded = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        late_finalize_credits: Some(artifact.clone()),
        ..Default::default()
    })
    .expect("encode late credits");
    assert_eq!(encoded[4], super::VERSION, "artifact version is 1");
    assert!(encoded.len() <= super::OUTBE_MAX_EXTRA_DATA_SIZE);
    let decoded = decode_outbe_block_artifacts(&encoded).expect("decode");
    assert_eq!(decoded.late_finalize_credits, Some(artifact));
}

#[test]
fn late_credits_coexist_with_execution_summary_and_timestamp() {
    let original = OutbeBlockArtifacts {
        radicle_publication_enabled: false,
        radicle_publication: None,
        execution_summary: Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::from(99u64),
        }),
        consensus_header_artifact: None,
        timestamp_millis_part: 777,
        late_finalize_credits: Some(super::LateFinalizeCreditsArtifact {
            batches: vec![sample_credit(5, 0x55)],
        }),
        enclave_witness: Some(Bytes::from_static(b"opaque-witness")),
    };
    let encoded = encode_outbe_block_artifacts(&original).expect("encode");
    assert_eq!(
        decode_outbe_block_artifacts(&encoded).expect("decode"),
        original
    );
}

#[test]
fn late_credits_reject_noncanonical_order_and_duplicates() {
    // Descending fb_number: not strictly ascending -> rejected.
    let descending = super::LateFinalizeCreditsArtifact {
        batches: vec![sample_credit(11, 0xBB), sample_credit(10, 0xAA)],
    };
    assert!(encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        late_finalize_credits: Some(descending),
        ..Default::default()
    })
    .is_err());
    // Duplicate target (same fb_number, fb_hash) -> rejected.
    let duplicate = super::LateFinalizeCreditsArtifact {
        batches: vec![sample_credit(10, 0xAA), sample_credit(10, 0xAA)],
    };
    assert!(encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        late_finalize_credits: Some(duplicate),
        ..Default::default()
    })
    .is_err());
}

#[test]
fn late_credits_decode_rejects_more_than_k_batches() {
    // The wire cap is the inclusion window `K`. The decoder rejects a count
    // header above `K` before it parses any per-credit body. This bounds
    // adversarial decode/snapshot/BLS-verify work to the protocol window.
    // An honest proposer never emits more than `K` batches.
    assert_eq!(
        super::LATE_FINALIZE_MAX_BATCHES as u64,
        crate::consensus::LATE_FINALIZE_WINDOW_K,
        "wire batch cap must equal K"
    );
    let over_k = (crate::consensus::LATE_FINALIZE_WINDOW_K + 1) as u16;
    let payload = over_k.to_be_bytes().to_vec();
    assert!(
        super::late_finalize::decode_payload(&payload).is_err(),
        "a batch count above K must be rejected at decode before body parsing"
    );
}

#[test]
fn late_credits_full_batch_within_64kib() {
    let batches: Vec<_> = (0..super::LATE_FINALIZE_MAX_BATCHES as u64)
        .map(|i| sample_credit(i, (i % 256) as u8))
        .collect();
    let artifact = super::LateFinalizeCreditsArtifact { batches };
    let encoded = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        late_finalize_credits: Some(artifact.clone()),
        ..Default::default()
    })
    .expect("encode full batch");
    assert!(
        encoded.len() <= super::OUTBE_MAX_EXTRA_DATA_SIZE,
        "full {} -batch artifact must fit extra_data budget, got {}",
        super::LATE_FINALIZE_MAX_BATCHES,
        encoded.len()
    );
    assert_eq!(
        decode_outbe_block_artifacts(&encoded)
            .expect("decode")
            .late_finalize_credits,
        Some(artifact)
    );
}

#[test]
fn late_credits_empty_batch_not_emitted() {
    // An empty batch must not produce a record (it is a no-op artifact).
    let encoded = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        late_finalize_credits: Some(super::LateFinalizeCreditsArtifact::default()),
        ..Default::default()
    })
    .expect("encode empty");
    assert!(
        encoded.is_empty(),
        "empty late-credits artifact emits no bytes"
    );
}

#[test]
fn post_reset_block_one_extra_data_vector_is_pinned() {
    let encoded = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        execution_summary: Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        }),
        ..Default::default()
    })
    .unwrap();
    let mut expected = b"OART\x01\x00\x01\x01\x00\x20".to_vec();
    expected.extend_from_slice(&[0_u8; 32]);
    assert_eq!(encoded.as_ref(), expected);
}

#[test]
fn proposer_sanitization_strips_all_execution_produced_fields() {
    let consensus = ConsensusHeaderArtifact::DealerLog(Bytes::from_static(b"dealer"));
    let input = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        execution_summary: Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::from(7),
        }),
        consensus_header_artifact: Some(consensus.clone()),
        timestamp_millis_part: 321,
        enclave_witness: Some(Bytes::from_static(b"stale-witness")),
        ..Default::default()
    })
    .unwrap();

    let sanitized = sanitize_prefinal_outbe_block_artifacts(&input).unwrap();
    let decoded = decode_outbe_block_artifacts(&sanitized).unwrap();
    assert_eq!(decoded.consensus_header_artifact, Some(consensus));
    assert_eq!(decoded.execution_summary, None);
    assert_eq!(decoded.timestamp_millis_part, 0);
    assert_eq!(decoded.enclave_witness, None);
}

#[test]
fn retired_ce_tag_is_rejected_instead_of_becoming_authority() {
    for length in [0u16, 35, 36, 37] {
        let mut encoded = b"OART\x01\x00\x01\x08".to_vec();
        encoded.extend_from_slice(&length.to_be_bytes());
        encoded.extend(std::iter::repeat_n(0, length as usize));
        assert!(decode_outbe_block_artifacts(&encoded).is_err());
        assert!(sanitize_prefinal_outbe_block_artifacts(&encoded).is_err());
    }
}
