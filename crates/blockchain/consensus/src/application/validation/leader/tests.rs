use alloy_primitives::B256;
use outbe_primitives::{
    block::BlockOrder, test_utils::TEST_BLOCK_ORDER, OutbeBlockBody, OutbeHeader,
};

#[test]
fn layout_uses_injected_order_and_rejects_empty_order_after_genesis() {
    let body = OutbeBlockBody::default();
    let mut header = OutbeHeader::default();
    super::validate_layout(&body, &header, &BlockOrder::default()).expect("genesis needs no order");
    for number in [1, 2] {
        header.inner.number = number;
        let error = super::validate_layout(&body, &header, &BlockOrder::default())
            .expect_err("post-genesis validation must fail closed with no order");
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
    super::validate_layout(&block.body, &block.header, &TEST_BLOCK_ORDER)
        .expect("the injected test order accepts its complete fixture");
    let missing_begin = BlockOrder {
        begin_block: &[],
        ..TEST_BLOCK_ORDER
    };
    assert!(super::validate_layout(&block.body, &block.header, &missing_begin).is_err());
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
        assert!(super::validate_layout(&block.body, &block.header, &TEST_BLOCK_ORDER).is_err());
    }
    let block = crate::test_fixtures::block_with_system_tx(&signer)
        .into_inner()
        .into_block();
    let (layout, artifacts) =
        super::validate_layout(&block.body, &block.header, &TEST_BLOCK_ORDER).unwrap();
    super::validate_parent_accounting(&layout, &block.header).unwrap();
    super::validate_boundary_outcome(&layout, &artifacts).unwrap();
    let mut wrong_parent = block.header;
    wrong_parent.inner.parent_hash = parent_hash;
    assert!(super::validate_parent_accounting(&layout, &wrong_parent).is_err());
}
