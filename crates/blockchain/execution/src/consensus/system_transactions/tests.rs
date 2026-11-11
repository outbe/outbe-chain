use super::*;
use alloy_consensus::Header;
use {
    outbe_evm::system_tx::TEST_BLOCK_ORDER,
    outbe_primitives::system_tx::{binding::LayoutBindingError, SystemTxError},
};

#[test]
fn empty_order_fails_closed_after_genesis() {
    let body = OutbeBlockBody::default();
    for number in [0, 1, 2, u64::MAX] {
        let header = OutbeHeader::new(Header {
            number,
            ..Default::default()
        });
        let result = validate_layout(&body, &header, &BlockOrder::default());
        if number == 0 {
            result.expect("genesis requires no system transactions");
        } else {
            let ConsensusError::Other(error) = result.unwrap_err() else {
                panic!("expected layout error")
            };
            assert!(
                matches!(error.downcast_ref::<LayoutBindingError>(), Some(LayoutBindingError::Set(SystemTxError::EmptyBlockOrder { block_number })) if *block_number == number)
            );
        }
    }
    let header = OutbeHeader::new(Header {
        number: 1,
        ..Default::default()
    });
    assert!(validate_layout(&body, &header, &TEST_BLOCK_ORDER).is_err());
}

#[test]
fn injected_payload_validator_runs_once_without_legacy_revalidation() {
    use alloy_consensus::{SignableTransaction as _, TxLegacy};
    use alloy_primitives::{Bytes, Signature, TxKind};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use {
        outbe_evm::system_tx::consensus_step,
        outbe_primitives::{
            addresses::OUTBE_SYSTEM_TX_ADDRESS,
            block::ProtocolModule,
            system_tx::SystemTx,
            system_tx_context::{SystemTxPayloadValidationError, SystemTxValidationContext},
        },
    };

    static CALLS: AtomicUsize = AtomicUsize::new(0);
    struct Payload;
    impl ProtocolModule for Payload {
        const NAME: &'static str = "test_payload";
        fn validate_payload(
            payload: &[u8],
            ctx: &SystemTxValidationContext<'_>,
        ) -> Result<(), SystemTxPayloadValidationError> {
            assert_eq!(payload, b"runtime-defined");
            assert_eq!(ctx.block_number, 2);
            CALLS.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }
    const ORDER: BlockOrder = BlockOrder {
        consensus: &[consensus_step::<Payload>()],
        begin_block: &[],
        end_block: &[],
    };
    let tx = TxLegacy {
        to: TxKind::Call(OUTBE_SYSTEM_TX_ADDRESS),
        input: SystemTx::Consensus {
            step_index: 0,
            payload: Bytes::from_static(b"runtime-defined"),
        }
        .encode()
        .unwrap(),
        ..Default::default()
    }
    .into_signed(Signature::test_signature())
    .into();
    let body = OutbeBlockBody {
        transactions: vec![tx],
        ..Default::default()
    };
    let header = OutbeHeader::new(Header {
        number: 2,
        ..Default::default()
    });
    validate_layout(&body, &header, &ORDER).expect("only the injected payload validator applies");
    assert_eq!(CALLS.load(Ordering::Relaxed), 1);
}
