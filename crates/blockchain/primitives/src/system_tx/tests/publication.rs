use super::*;
use crate::{
    reshare_artifact::{encode_outbe_block_artifacts, OutbeBlockArtifacts},
    system_tx::binding::validate_system_layout,
    test_utils::sample_radicle_publication_batch as batch,
    OutbeBlockBody, OutbeHeader,
};

#[test]
fn publication_calldata_is_bounded_and_canonical() {
    let input = SystemTx::Consensus {
        step_index: 5,
        payload: Bytes::from((batch(32)).to_bytes().unwrap()),
    };
    let encoded = input.encode().unwrap();
    assert_eq!(&encoded[..6], b"OSCN\x03\x05");
    assert_eq!(encoded.len(), 3684);
    assert_eq!(SystemTx::decode(&encoded).unwrap(), input);
    let mut trailing = encoded.to_vec();
    trailing.push(0);
    assert!(crate::system_tx::binding::validate_publication_payload(
        &trailing[6..],
        &OutbeBlockArtifacts {
            radicle_publication: Some(batch(32)),
            ..Default::default()
        }
    )
    .is_err());
    assert!(crate::system_tx::binding::validate_publication_payload(
        &encoded[6..encoded.len() - 1],
        &OutbeBlockArtifacts {
            radicle_publication: Some(batch(32)),
            ..Default::default()
        }
    )
    .is_err());
    let mut old = encoded.to_vec();
    old[4] = 1;
    assert!(SystemTx::decode(&old).is_err());
    assert!(
        TEST_BLOCK_ORDER
            .step_for_tx(&input)
            .unwrap()
            .revert_fails_block
    );
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
        parent_hash: B256::repeat_byte(0x41),
        extra_data: encode_outbe_block_artifacts(&artifacts).unwrap(),
        ..Default::default()
    });
    let plan = BlockSystemPlan::for_block(
        2,
        BlockSystemFlags {
            has_publication: true,
            has_boundary_outcome: false,
        },
        &TEST_BLOCK_ORDER,
    );
    assert_eq!(
        &plan.expected_prefix_steps()[2..5],
        &[
            SystemTxStepId::consensus(4),
            SystemTxStepId::consensus(5),
            SystemTxStepId::begin_block(0)
        ]
    );
    let mut inputs = vec![
        accounting(),
        late_credits(),
        SystemTx::Consensus {
            step_index: 4,
            payload: Bytes::new(),
        },
        SystemTx::Consensus {
            step_index: 5,
            payload: Bytes::from((batch(32)).to_bytes().unwrap()),
        },
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
    assert!(validate_system_layout(&body, &header, &TEST_BLOCK_ORDER).is_ok());

    let publication_index = 3;
    let accepted = body.transactions.remove(publication_index);
    assert!(validate_system_layout(&body, &header, &TEST_BLOCK_ORDER).is_err());

    let mut different = batch(32);
    different.body.package_manifest_hash = B256::repeat_byte(99);
    body.transactions.insert(
        publication_index,
        transaction(
            SystemTx::Consensus {
                step_index: 5,
                payload: Bytes::from((different).to_bytes().unwrap()),
            },
            publication_index as u8,
        ),
    );
    assert!(validate_system_layout(&body, &header, &TEST_BLOCK_ORDER).is_err());

    body.transactions[publication_index] = accepted;
    let mut no_batch = header.clone();
    no_batch.inner.extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        radicle_publication_enabled: true,
        ..Default::default()
    })
    .unwrap();
    assert!(validate_system_layout(&body, &no_batch, &TEST_BLOCK_ORDER).is_err());
    body.transactions.remove(publication_index);
    assert!(validate_system_layout(&body, &no_batch, &TEST_BLOCK_ORDER).is_ok());
    no_batch.inner.extra_data = Bytes::new();
    assert!(validate_system_layout(&body, &no_batch, &TEST_BLOCK_ORDER).is_ok());
}

#[test]
fn publication_cursor_advances_after_validator_liveness_when_batch_is_present() {
    for has_publication in [false, true] {
        let mut fsm = BlockExecutionFsm::for_block(
            2,
            BlockSystemFlags {
                has_publication,
                has_boundary_outcome: false,
            },
            &TEST_BLOCK_ORDER,
        );
        for index in [0, 1, 4] {
            fsm.on_system_tx(&fixtures::consensus(index, Bytes::new()))
                .unwrap();
        }
        if has_publication {
            assert_eq!(fsm.expected_step_id(), Some(SystemTxStepId::consensus(5)));
            assert_eq!(fsm.expected_prefix_body_index(), Some(3));
            fsm.on_system_tx(&fixtures::consensus(5, Bytes::new()))
                .unwrap();
        }
        assert_eq!(fsm.expected_step_id(), Some(SystemTxStepId::begin_block(0)));
        assert_eq!(
            fsm.expected_prefix_body_index(),
            Some(if has_publication { 4 } else { 3 })
        );
    }
}
