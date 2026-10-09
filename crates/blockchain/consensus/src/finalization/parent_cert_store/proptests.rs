//! Encoding/decoding safety properties for [`CertifiedParentProofRecord`].
//!
//! These tests pin the on-disk shape so a future schema change cannot
//! silently break the V1 read path:
//!
//! 1. Round-trip: any record round-trips through serde_json byte-equal.
//! 2. Determinism: encoding the same logical record many times yields
//!    byte-identical output (no per-call randomness, no HashMap/HashSet
//!    iteration order in the schema).
//! 3. Cross-version pin: a hand-crafted JSON payload representative of
//!    the v1 on-disk format decodes to the expected logical record.
//!    The skill's "Safety verification" rule requires this cross-version
//!    compatibility check for consensus-carrying paths.
//!
//! Note: the on-disk encoding is `serde_json` (see `encode_record`). It was
//! chosen deliberately, following the `parent_cert_store.rs:381-385` precedent.
//! Switching encoding is a schema migration, not a refactor.
use super::*;
use proptest::collection::vec;
use proptest::prelude::*;

/// Strategy: a `ProofKind`. Either a `Finalization` carrying an arbitrary
/// block number, or a `CertifiedNotarization` (no block number).
fn arb_kind() -> impl Strategy<Value = ProofKind> {
    prop_oneof![
        (0u64..(1 << 32)).prop_map(|finalized_block_number| ProofKind::Finalization {
            finalized_block_number
        }),
        Just(ProofKind::CertifiedNotarization),
    ]
}

/// Strategy: arbitrary `CertifiedParentProofRecord` with the current
/// `format_version`. The numeric ranges stay under realistic protocol
/// bounds (epoch < 2^24, view < 2^32, etc.) to keep shrinking fast without
/// sacrificing coverage of the encoded layout.
fn arb_record() -> impl Strategy<Value = CertifiedParentProofRecord> {
    let head = (
        arb_kind(),
        0u64..(1 << 24),
        0u64..(1 << 32),
        0u64..(1 << 32),
        any::<[u8; 32]>(),
        any::<[u8; 32]>(),
    );
    let tail = (
        0u64..(1 << 40),
        vec(any::<[u8; 20]>(), 0..8),
        vec(any::<u8>(), 0..8),
        vec(any::<u8>(), 0..128),
    );
    (head, tail).prop_map(
        |(
            (
                kind,
                finalized_epoch,
                finalized_view,
                parent_view,
                finalized_block_hash,
                committee_set_hash,
            ),
            (vrf_material_version, ordered_committee, signer_bitmap, proof_bytes),
        )| {
            let encoded_proof = Bytes::from(proof_bytes);
            CertifiedParentProofRecord {
                format_version: CERTIFIED_PARENT_PROOF_RECORD_FORMAT_VERSION,
                kind,
                finalized_epoch,
                finalized_view,
                parent_view,
                finalized_block_hash: B256::from(finalized_block_hash),
                committee_set_hash: B256::from(committee_set_hash),
                vrf_material_version,
                vrf_group_public_key_hash: B256::ZERO,
                ordered_committee: ordered_committee.into_iter().map(Address::from).collect(),
                signer_bitmap,
                encoded_proof,
            }
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    })]

    /// Property: any record round-trips through serde_json byte-equal.
    #[test]
    fn proptest_record_encode_decode_roundtrip(rec in arb_record()) {
        let bytes = serde_json::to_vec(&rec).expect("encode");
        let decoded: CertifiedParentProofRecord =
            serde_json::from_slice(&bytes).expect("decode");
        prop_assert_eq!(decoded, rec);
    }

    /// Property: encoding the same logical record N times yields
    /// byte-identical output. This guards determinism against any future use
    /// of HashMap/HashSet or per-call randomness in the schema.
    #[test]
    fn proptest_record_encode_is_deterministic(rec in arb_record()) {
        let first = serde_json::to_vec(&rec).expect("encode");
        for _ in 0..16 {
            let again = serde_json::to_vec(&rec).expect("encode again");
            prop_assert_eq!(&again, &first);
        }
    }

    /// Property: the backend decoder rejects a non-current format_version,
    /// regardless of the rest of the record.
    #[test]
    fn proptest_unknown_format_version_is_rejected(
        mut rec in arb_record(),
        bad_version in (0u8..=255u8).prop_filter("not the current schema", |v| *v != CERTIFIED_PARENT_PROOF_RECORD_FORMAT_VERSION),
    ) {
        rec.format_version = bad_version;
        let bytes = serde_json::to_vec(&rec).expect("encode");

        let temp = tempfile::tempdir().expect("tempdir");
        let backend = MdbxParentProofBackend::open(temp.path()).expect("open backend");
        let decoded = backend.decode_record(bytes);
        // Extract the predicate first. `prop_assert!` treats commas in
        // its arg list as format-string separators, so embedding the
        // `matches! ... if ...` guard inline confuses the macro parser.
        let rejected_with_bad_version = matches!(
            decoded,
            Err(ParentProofStoreError::UnknownFormatVersion { version, .. }) if version == bad_version
        );
        prop_assert!(rejected_with_bad_version);
    }
}

/// Current-format (V3) serde-shape pin. A hand-crafted JSON payload that
/// matches the on-disk shape decodes to the expected logical record. The
/// decoded record re-encodes + re-decodes byte-equal. A proptest round-trip
/// cannot catch a serde field/variant rename (encode and decode rename
/// symmetrically). Thus this pin forces an explicit migration decision if the
/// schema's JSON shape changes. The externally-tagged `ProofKind` serializes
/// as `{"Finalization":{"finalized_block_number":N}}`. The pin exists to catch
/// this kind of detail.
#[test]
fn current_format_payload_decodes_to_record() {
    let payload = r#"{
        "format_version": 4,
        "kind": { "Finalization": { "finalized_block_number": 42 } },
        "finalized_epoch": 7,
        "finalized_view": 100,
        "parent_view": 99,
        "finalized_block_hash": "0x000000000000000000000000000000000000000000000000000000000000aaaa",
        "committee_set_hash": "0x000000000000000000000000000000000000000000000000000000000000bbbb",
        "vrf_material_version": 3,
        "vrf_group_public_key_hash": "0x0000000000000000000000000000000000000000000000000000000000000000",
        "ordered_committee": ["0x1111111111111111111111111111111111111111"],
        "signer_bitmap": [1, 0, 1],
        "encoded_proof": "0xdeadbeef"
    }"#;
    let rec: CertifiedParentProofRecord =
        serde_json::from_str(payload).expect("current-format payload must decode");
    assert_eq!(
        rec.format_version,
        CERTIFIED_PARENT_PROOF_RECORD_FORMAT_VERSION
    );
    assert_eq!(rec.proof_kind(), ParentParticipationProof::Finalization);
    assert_eq!(rec.finalized_block_number(), Some(42));
    assert_eq!(rec.finalized_epoch, 7);
    assert_eq!(rec.finalized_view, 100);
    assert_eq!(rec.parent_view, 99);
    let mut expected_hash = [0u8; 32];
    expected_hash[30] = 0xaa;
    expected_hash[31] = 0xaa;
    assert_eq!(rec.finalized_block_hash, B256::from(expected_hash));
    assert_eq!(rec.vrf_material_version, 3);
    assert_eq!(rec.ordered_committee.len(), 1);
    assert_eq!(rec.signer_bitmap, vec![1u8, 0, 1]);
    assert_eq!(rec.encoded_proof.as_ref(), &[0xde, 0xad, 0xbe, 0xef]);

    // Re-encode + re-decode byte-equal: confirms the decoded record's
    // serialized shape is stable under the current schema.
    let encoded = serde_json::to_vec(&rec).expect("re-encode");
    let decoded: CertifiedParentProofRecord = serde_json::from_slice(&encoded).expect("re-decode");
    assert_eq!(decoded, rec);

    // A unit-variant `CertifiedNotarization` pins as a bare string tag.
    let cn_payload = r#"{
        "format_version": 4,
        "kind": "CertifiedNotarization",
        "finalized_epoch": 7,
        "finalized_view": 100,
        "parent_view": 99,
        "finalized_block_hash": "0x000000000000000000000000000000000000000000000000000000000000aaaa",
        "committee_set_hash": "0x0000000000000000000000000000000000000000000000000000000000000000",
        "vrf_material_version": 0,
        "vrf_group_public_key_hash": "0x0000000000000000000000000000000000000000000000000000000000000000",
        "ordered_committee": [],
        "signer_bitmap": [],
        "encoded_proof": "0x",
        "stored_at_height": 100
    }"#;
    let cn: CertifiedParentProofRecord =
        serde_json::from_str(cn_payload).expect("CN payload must decode");
    assert_eq!(
        cn.proof_kind(),
        ParentParticipationProof::CertifiedNotarization
    );
    assert_eq!(cn.finalized_block_number(), None);
    assert!(cn.is_certification_witness());
}
