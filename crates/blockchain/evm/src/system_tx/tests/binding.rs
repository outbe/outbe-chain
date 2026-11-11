use crate::system_tx::binding::*;
use alloy_primitives::B256;
use outbe_primitives as primitives;
#[path = "binding_cases.rs"]
mod cases;

fn fixture(name: &str) -> cases::Case {
    cases::cases()
        .into_iter()
        .find(|case| case.name == name)
        .expect("binding fixture exists")
}

#[test]
fn shared_binding_cases_preserve_acceptance_and_rejection() {
    for case in cases::cases() {
        let layout = case.layout();
        let result = match case.check {
            cases::Check::Layout => validate_system_layout(
                &case.body,
                &case.header,
                &crate::system_tx::TEST_BLOCK_ORDER,
            )
            .map(|_| ())
            .map_err(|error| error.to_string()),
            cases::Check::Parent => validate_parent_accounting_binding(&layout, &case.header)
                .map_err(|error| error.to_string()),
            cases::Check::Boundary => validate_boundary_outcome_binding(&layout, &case.artifacts)
                .map_err(|error| error.to_string()),
        };
        let accepted = matches!(
            case.name,
            "genesis_empty"
                | "steady_disabled"
                | "genesis_skips_parent"
                | "block1_skips_parent"
                | "parent_matches"
                | "maximum_height_parent_matches"
                | "no_boundary_skips_malformed_inputs"
                | "boundary_matches"
        );
        assert_eq!(result.is_ok(), accepted, "{}: {result:?}", case.name);
    }
}

#[test]
fn layout_failures_identify_the_rejected_stage() {
    let artifacts = fixture("artifacts_before_layout");
    assert!(matches!(
        validate_system_layout(
            &artifacts.body,
            &artifacts.header,
            &crate::system_tx::TEST_BLOCK_ORDER
        ),
        Err(LayoutBindingError::Artifacts(_))
    ));
    let order = fixture("wrong_order");
    assert!(matches!(
        validate_system_layout(
            &order.body,
            &order.header,
            &crate::system_tx::TEST_BLOCK_ORDER
        ),
        Err(LayoutBindingError::Layout(_))
    ));
    let steady = fixture("steady_disabled");
    let missing_begin = crate::system_tx::BlockOrder {
        begin_block: &[],
        ..crate::system_tx::TEST_BLOCK_ORDER
    };
    assert!(matches!(
        validate_system_layout(&steady.body, &steady.header, &missing_begin),
        Err(LayoutBindingError::Set(
            crate::system_tx::SystemTxError::ActiveSystemTxSetMismatch { .. }
        ))
    ));
    let set = fixture("block1_requires_boundary");
    assert!(matches!(
        validate_system_layout(&set.body, &set.header, &crate::system_tx::TEST_BLOCK_ORDER),
        Err(LayoutBindingError::Set(_))
    ));
}

#[test]
fn injected_payload_validation_runs_on_begin_and_end_steps() {
    use {
        crate::system_tx::{begin_step, end_step, BlockOrder},
        outbe_primitives::{
            block::ProtocolModule,
            system_tx_context::{SystemTxPayloadValidationError, SystemTxValidationContext},
        },
    };
    struct Reject;
    impl ProtocolModule for Reject {
        const NAME: &'static str = "reject";
        fn validate_payload(
            _: &[u8],
            _: &SystemTxValidationContext<'_>,
        ) -> Result<(), SystemTxPayloadValidationError> {
            Err(SystemTxPayloadValidationError::UnexpectedNonEmptyPayload {
                module: Self::NAME,
                len: 7,
            })
        }
    }
    const REJECT_BEGIN: &[crate::system_tx::ProtocolStep] = &[begin_step::<Reject>(); 9];
    const REJECT_END: &[crate::system_tx::ProtocolStep] = &[end_step::<Reject>(); 5];
    let mut case = fixture("steady_disabled");
    for end in [false, true] {
        let order = if end {
            BlockOrder {
                end_block: REJECT_END,
                ..crate::system_tx::TEST_BLOCK_ORDER
            }
        } else {
            BlockOrder {
                begin_block: REJECT_BEGIN,
                ..crate::system_tx::TEST_BLOCK_ORDER
            }
        };
        assert!(matches!(
            validate_system_layout(&case.body, &case.header, &order),
            Err(LayoutBindingError::UnexpectedNonEmptyPayload {
                module: "reject",
                len: 7
            })
        ));
    }
    case.header.inner.parent_hash = B256::repeat_byte(99);
    assert!(matches!(
        validate_system_layout(
            &case.body,
            &case.header,
            &crate::system_tx::TEST_BLOCK_ORDER
        ),
        Err(LayoutBindingError::ParentAccounting(
            ParentAccountingBindingError::HashMismatch { .. }
        ))
    ));
}
