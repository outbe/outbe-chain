use super::{codec::*, tape::*};

fn sample() -> EnclaveWitnessTapeV1 {
    EnclaveWitnessTapeV1 {
        prefix: vec![EnclaveWitnessEntryV1::DcapVerificationFinishedV1 {
            request_hash: alloy_primitives::B256::ZERO,
            outcome: vec![1, 2],
            attestation_tag: vec![3; 64],
        }],
        txs: vec![],
    }
}

fn recompress(raw: &[u8]) -> Vec<u8> {
    let frame = zstd::bulk::compress(raw, 3).unwrap();
    let mut payload = vec![1];
    payload.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    payload.extend(frame);
    payload
}

#[test]
fn round_trips_framed_tape_without_requiring_compressed_byte_identity() {
    let tape = sample();
    let payload = encode_witness_payload_v1(&tape).unwrap();
    assert_eq!(decode_witness_payload_v1(&payload).unwrap(), tape);
    let raw = postcard::to_allocvec(&tape).unwrap();
    assert_eq!(decode_witness_payload_v1(&recompress(&raw)).unwrap(), tape);
}

#[test]
fn rejects_short_bad_version_and_bad_raw_lengths_before_decompression() {
    for payload in [&[][..], &[1; 9][..]] {
        assert!(decode_witness_payload_v1(payload).is_err());
    }
    let mut payload = encode_witness_payload_v1(&sample()).unwrap().to_vec();
    payload[0] = 2;
    assert!(matches!(
        decode_witness_payload_v1(&payload),
        Err(WitnessCodecError::UnsupportedVersion)
    ));
    for length in [0u32, 262_145] {
        let mut payload = encode_witness_payload_v1(&sample()).unwrap().to_vec();
        payload[1..5].copy_from_slice(&length.to_le_bytes());
        assert!(decode_witness_payload_v1(&payload).is_err());
    }
}

#[test]
fn rejects_skippable_corrupt_concatenated_dictionary_and_trailing_frames() {
    let good = encode_witness_payload_v1(&sample()).unwrap().to_vec();
    let mut wrong_magic = good.clone();
    wrong_magic[5..9].copy_from_slice(&[0x50, 0x2a, 0x4d, 0x18]);
    assert!(decode_witness_payload_v1(&wrong_magic).is_err());
    let mut dictionary = good.clone();
    dictionary[9] |= 1;
    assert!(decode_witness_payload_v1(&dictionary).is_err());
    let mut trailing = good.clone();
    trailing.push(0);
    assert!(decode_witness_payload_v1(&trailing).is_err());
    let mut concatenated = good.clone();
    concatenated.extend_from_slice(&good[5..]);
    assert!(decode_witness_payload_v1(&concatenated).is_err());
}

#[test]
fn rejects_postcard_remainder_empty_tape_and_invalid_segments() {
    let tape = sample();
    let mut raw = postcard::to_allocvec(&tape).unwrap();
    raw.push(0);
    assert!(decode_witness_payload_v1(&recompress(&raw)).is_err());
    let empty = EnclaveWitnessTapeV1 {
        prefix: vec![],
        txs: vec![],
    };
    assert!(encode_witness_payload_v1(&empty).is_err());
    let entry = sample().prefix[0].clone();
    for txs in [
        vec![TxWitnessV1 {
            tx_index: 2,
            entries: vec![],
        }],
        vec![
            TxWitnessV1 {
                tx_index: 2,
                entries: vec![entry.clone()],
            },
            TxWitnessV1 {
                tx_index: 2,
                entries: vec![entry.clone()],
            },
        ],
        vec![
            TxWitnessV1 {
                tx_index: 3,
                entries: vec![entry.clone()],
            },
            TxWitnessV1 {
                tx_index: 2,
                entries: vec![entry.clone()],
            },
        ],
    ] {
        let bad = EnclaveWitnessTapeV1 {
            prefix: vec![],
            txs,
        };
        assert!(encode_witness_payload_v1(&bad).is_err());
    }
}

#[test]
fn hostile_postcard_vector_length_fails_within_the_bounded_input() {
    // A u32::MAX prefix length followed by no elements must fail without an
    // unbounded reserve or panic; postcard's Vec visitor uses its cautious hint.
    let payload = recompress(&[0xff, 0xff, 0xff, 0xff, 0x0f]);
    assert!(decode_witness_payload_v1(&payload).is_err());
}

#[test]
fn bound_includes_prefix_and_rejects_payload_over_outer_tlv_limit() {
    assert_eq!(
        compressed_witness_bound(0),
        5 + zstd::zstd_safe::compress_bound(0)
    );
    let mut oversized = vec![1];
    oversized.extend_from_slice(&1u32.to_le_bytes());
    oversized.resize(65_527, 0);
    assert!(matches!(
        decode_witness_payload_v1(&oversized),
        Err(WitnessCodecError::PayloadTooLarge)
    ));
}
