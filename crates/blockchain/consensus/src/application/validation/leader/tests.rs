use alloy_primitives::B256;
use outbe_primitives::{
    system_tx::binding::{LayoutBindingError, SystemTxLeaderPolicy},
    OutbeBlockBody, OutbeHeader,
};

#[test]
fn layout_uses_injected_policy_and_fails_closed_by_default() {
    let body = OutbeBlockBody::default();
    let mut header = OutbeHeader::default();
    super::validate_layout(&body, &header, &SystemTxLeaderPolicy::default())
        .expect("genesis needs no policy");
    for number in [1, 2] {
        header.inner.number = number;
        let error = super::validate_layout(&body, &header, &SystemTxLeaderPolicy::default())
            .expect_err("post-genesis validation must fail closed with no policy");
        assert!(
            matches!(error, crate::errors::BlockValidationError::LayoutBinding(
            outbe_primitives::system_tx::binding::LayoutBindingError::Set(
                outbe_primitives::system_tx::SystemTxError::EmptyBlockOrder { block_number }
            )
        ) if block_number == number)
        );
    }
    let signer = outbe_primitives::signer::OutbeEvmSigner::from_secret_bytes([7; 32]).unwrap();
    let block = crate::test_fixtures::block_with_system_tx(&signer)
        .into_inner()
        .into_block();
    super::validate_layout(
        &block.body,
        &block.header,
        &crate::test_fixtures::test_leader_policy(),
    )
    .expect("the injected test policy accepts its complete fixture");
    let rejecting_policy = SystemTxLeaderPolicy {
        validate_layout: |_, _| {
            Err(LayoutBindingError::Set(
                outbe_primitives::system_tx::SystemTxError::V2Block1MissingBoundaryOutcome,
            ))
        },
        ..crate::test_fixtures::test_leader_policy()
    };
    assert!(matches!(
        super::validate_layout(&block.body, &block.header, &rejecting_policy),
        Err(crate::errors::BlockValidationError::LayoutBinding(
            LayoutBindingError::Set(
                outbe_primitives::system_tx::SystemTxError::V2Block1MissingBoundaryOutcome
            )
        ))
    ));
    let (layout, _) = super::validate_layout(
        &block.body,
        &block.header,
        &crate::test_fixtures::test_leader_policy(),
    )
    .unwrap();
    let named_policy = SystemTxLeaderPolicy {
        step_name_or_stage: |_| "injected_step_name",
        ..crate::test_fixtures::test_leader_policy()
    };
    assert!(matches!(
        super::validate_envelopes(
            &layout,
            &block.header,
            outbe_primitives::chain::CHAIN_ID + 1,
            &named_policy
        ),
        Err(crate::errors::BlockValidationError::SignatureHashMismatch {
            kind: "injected_step_name",
            ordinal: 0,
        })
    ));
    let mut genesis_body = block.body;
    genesis_body.transactions.truncate(1);
    header.inner.number = 0;
    assert!(matches!(
        super::validate_layout(&genesis_body, &header, &SystemTxLeaderPolicy::default()),
        Err(crate::errors::BlockValidationError::LayoutBinding(
            LayoutBindingError::Set(
                outbe_primitives::system_tx::SystemTxError::ActiveSystemTxSetMismatch { .. }
            )
        ))
    ));
}

#[test]
fn layout_rejects_malformed_parent_payload_and_preserves_parent_binding() {
    let signer = outbe_primitives::signer::OutbeEvmSigner::from_secret_bytes([7; 32]).unwrap();
    let parent_hash = B256::repeat_byte(0x41);
    for payload in [alloy_primitives::Bytes::new(), {
        let mut bytes = crate::test_fixtures::finalized_metadata(parent_hash)
            .encode()
            .unwrap()
            .to_vec();
        bytes.push(0);
        bytes.into()
    }] {
        let mut inputs = crate::test_fixtures::steady_begin_inputs(parent_hash);
        inputs[0] = outbe_primitives::system_tx::SystemTx::Consensus {
            step_index: outbe_primitives::system_tx::CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
            payload,
        };
        let block = crate::test_fixtures::block_with_system_inputs(
            &signer,
            crate::test_fixtures::SystemBlockFixture {
                block_number: 2,
                parent_hash,
                extra_data: Default::default(),
                inputs,
                chain_id: outbe_primitives::chain::CHAIN_ID,
            },
        )
        .into_inner()
        .into_block();
        assert!(super::validate_layout(
            &block.body,
            &block.header,
            &crate::test_fixtures::test_leader_policy()
        )
        .is_err());
    }
    let block = crate::test_fixtures::block_with_system_tx(&signer)
        .into_inner()
        .into_block();
    let (layout, artifacts) = super::validate_layout(
        &block.body,
        &block.header,
        &crate::test_fixtures::test_leader_policy(),
    )
    .unwrap();
    super::validate_parent_accounting(&layout, &block.header).unwrap();
    super::validate_boundary_outcome(&layout, &artifacts).unwrap();
    let mut wrong_parent = block.header;
    wrong_parent.inner.parent_hash = parent_hash;
    assert!(super::validate_parent_accounting(&layout, &wrong_parent).is_err());
}
