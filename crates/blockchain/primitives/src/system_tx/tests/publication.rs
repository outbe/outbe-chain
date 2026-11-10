use super::*;
use crate::{
    reshare_artifact::{encode_outbe_block_artifacts, OutbeBlockArtifacts},
    system_tx::binding::validate_system_layout,
    test_utils::sample_radicle_publication_batch as batch,
    OutbeBlockBody, OutbeHeader,
};

#[test]
fn publication_calldata_is_bounded_and_canonical() {
    let input = SystemTx::RadiclePublication { batch: batch(32) };
    let encoded = input.encode().unwrap();
    assert_eq!(&encoded[..5], b"OSP2\x02");
    assert_eq!(encoded.len(), 3683);
    assert_eq!(SystemTx::decode(&encoded).unwrap(), input);
    let mut trailing = encoded.to_vec();
    trailing.push(0);
    assert!(SystemTx::decode(&trailing).is_err());
    assert!(SystemTx::decode(&encoded[..encoded.len() - 1]).is_err());
    let mut old = encoded.to_vec();
    old[4] = 1;
    assert!(SystemTx::decode(&old).is_err());
    assert!(input.revert_fails_block());
}

fn transaction(input: SystemTx, ordinal: u8) -> TransactionSigned {
    build_unsigned_system_tx(ordinal, 2, CHAIN_ID, input.encode().unwrap())
        .unwrap()
        .into_signed(Signature::test_signature())
        .into()
}

#[test]
fn publication_layout_binds_exact_header_batch_after_validator_liveness() {
    let artifacts = OutbeBlockArtifacts {
        radicle_publication_enabled: true,
        radicle_publication: Some(batch(32)),
        ..Default::default()
    };
    let header = OutbeHeader::new(alloy_consensus::Header {
        number: 2,
        extra_data: encode_outbe_block_artifacts(&artifacts).unwrap(),
        ..Default::default()
    });
    let selectors = expected_begin_block_selectors_for_publication(2, false, false, true);
    assert_eq!(
        &selectors[2..5],
        &[
            VALIDATOR_LIVENESS_SELECTOR,
            RADICLE_PUBLICATION_SELECTOR,
            VOTE_SELECTOR
        ]
    );
    let mut inputs = vec![
        accounting(),
        late_credits(),
        SystemTx::ValidatorLiveness,
        SystemTx::RadiclePublication { batch: batch(32) },
    ];
    inputs.extend(begin_stage_inputs());
    let mut body = OutbeBlockBody {
        transactions: inputs
            .into_iter()
            .enumerate()
            .map(|(index, input)| transaction(input, index as u8))
            .collect(),
        ..Default::default()
    };
    for input in end_stage_inputs() {
        let ordinal = input.nonce_ordinal().unwrap();
        body.transactions.push(transaction(input, ordinal));
    }
    assert!(validate_system_layout(&body, &header).is_ok());

    let publication_index = 3;
    let accepted = body.transactions.remove(publication_index);
    assert!(validate_system_layout(&body, &header).is_err());

    let mut different = batch(32);
    different.body.package_manifest_hash = B256::repeat_byte(99);
    body.transactions.insert(
        publication_index,
        transaction(
            SystemTx::RadiclePublication { batch: different },
            publication_index as u8,
        ),
    );
    assert!(validate_system_layout(&body, &header).is_err());

    body.transactions[publication_index] = accepted;
    let mut no_batch = header.clone();
    no_batch.inner.extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        radicle_publication_enabled: true,
        ..Default::default()
    })
    .unwrap();
    assert!(validate_system_layout(&body, &no_batch).is_err());
    body.transactions.remove(publication_index);
    assert!(validate_system_layout(&body, &no_batch).is_ok());
    no_batch.inner.extra_data = Bytes::new();
    assert!(validate_system_layout(&body, &no_batch).is_ok());
}

#[test]
fn publication_cursor_advances_after_validator_liveness_when_batch_is_present() {
    let liveness = SystemTxPhase::ValidatorLiveness { body_index: 2 };
    assert_eq!(
        liveness.advance_after_commit(false, false),
        SystemTxPhase::Vote { body_index: 3 }
    );
    let publication = liveness.advance_after_commit_with_publication(false, false, true);
    assert_eq!(
        publication,
        SystemTxPhase::RadiclePublication { body_index: 3 }
    );
    assert_eq!(
        publication.expected_selector(),
        Some(RADICLE_PUBLICATION_SELECTOR)
    );
    assert_eq!(
        publication.advance_after_commit_with_publication(false, false, true),
        SystemTxPhase::Vote { body_index: 4 }
    );
}
