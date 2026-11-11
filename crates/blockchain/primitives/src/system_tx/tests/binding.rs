use crate as primitives;
use crate::system_tx::{binding::*, SystemTxLayout};
use alloy_primitives::B256;
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
                &crate::test_utils::TEST_BLOCK_ORDER,
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
            &crate::test_utils::TEST_BLOCK_ORDER
        ),
        Err(LayoutBindingError::Artifacts(_))
    ));
    let order = fixture("wrong_order");
    assert!(matches!(
        validate_system_layout(
            &order.body,
            &order.header,
            &crate::test_utils::TEST_BLOCK_ORDER
        ),
        Err(LayoutBindingError::Layout(_))
    ));
    let set = fixture("block1_requires_boundary");
    assert!(matches!(
        validate_system_layout(&set.body, &set.header, &crate::test_utils::TEST_BLOCK_ORDER),
        Err(LayoutBindingError::Set(_))
    ));
}

#[test]
fn parent_errors_carry_block_height_and_both_hashes() {
    let missing = fixture("parent_missing");
    assert!(matches!(
        validate_parent_accounting_binding(&missing.layout(), &missing.header),
        Err(ParentAccountingBindingError::Missing { block_number: 2 })
    ));
    let mismatch = fixture("parent_hash_mismatch");
    let Err(ParentAccountingBindingError::HashMismatch { expected, actual }) =
        validate_parent_accounting_binding(&mismatch.layout(), &mismatch.header)
    else {
        panic!("expected parent hash mismatch");
    };
    assert_eq!(expected, B256::repeat_byte(0x41));
    assert_eq!(actual, B256::repeat_byte(0x42));
}

#[test]
fn boundary_parity_checks_end_inputs_after_a_matching_begin_input() {
    let case = fixture("boundary_match_then_bad_input");
    let layout = SystemTxLayout {
        begin: vec![&case.body.transactions[0]],
        user: vec![],
        end: vec![&case.body.transactions[1]],
    };
    assert!(matches!(
        validate_boundary_outcome_binding(&layout, &case.artifacts),
        Err(BoundaryBindingError::Decode(_))
    ));
}

#[test]
fn boundary_rejects_the_first_failure_in_system_transaction_order() {
    let mismatch = fixture("boundary_mismatch");
    let malformed = fixture("no_boundary_skips_malformed_inputs");
    let bad_artifact = &mismatch.body.transactions[0];
    let bad_input = &malformed.body.transactions[0];
    let mut layout = SystemTxLayout {
        begin: vec![bad_input, bad_artifact],
        user: vec![],
        end: vec![],
    };
    assert!(matches!(
        validate_boundary_outcome_binding(&layout, &mismatch.artifacts),
        Err(BoundaryBindingError::Decode(_))
    ));
    layout.begin.reverse();
    assert!(matches!(
        validate_boundary_outcome_binding(&layout, &mismatch.artifacts),
        Err(BoundaryBindingError::Mismatch)
    ));
}

#[test]
fn payload_validators_reject_trailing_bytes_and_cross_artifact_envelopes() {
    use super::{fixtures, TEST_BLOCK_ORDER};
    use crate::{
        reshare_artifact::{
            encode_outbe_block_artifacts, ConsensusHeaderArtifact, OutbeBlockArtifacts,
        },
        system_tx_context::SystemTxValidationContext,
    };
    use alloy_primitives::Bytes;
    let boundary = fixtures::boundary();
    let boundary_artifact = crate::reshare_artifact::decode_boundary_artifact(boundary.payload())
        .unwrap()
        .unwrap();
    let publication = fixtures::publication();
    let publication_batch =
        crate::radicle_publication::PublicationBatch::from_bytes(publication.payload()).unwrap();
    let artifacts = OutbeBlockArtifacts {
        consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(
            boundary_artifact,
        )),
        radicle_publication: Some(publication_batch),
        radicle_publication_enabled: true,
        ..Default::default()
    };
    let ctx = SystemTxValidationContext {
        regime: crate::block::BlockHeightRegime::SteadyState,
        block_number: 2,
        parent_hash: alloy_primitives::B256::repeat_byte(0x41),
        artifacts: &artifacts,
    };
    for tx in [
        fixtures::accounting(),
        fixtures::late_credits(),
        boundary,
        fixtures::bootstrap(),
        publication,
    ] {
        let validator = TEST_BLOCK_ORDER.step_for_tx(&tx).unwrap().validate_payload;
        validator(tx.payload(), &ctx).unwrap();
        let mut trailing = tx.payload().to_vec();
        trailing.push(0);
        assert!(validator(&trailing, &ctx).is_err(), "{:?}", tx.step_id());
    }
    let contaminated = encode_outbe_block_artifacts(&artifacts).unwrap();
    assert!(matches!(
        validate_boundary_outcome_payload(&contaminated, &artifacts),
        Err(BoundaryBindingError::NonCanonical)
    ));
    assert!(matches!(
        validate_late_finalize_credits_payload(&contaminated, &artifacts),
        Err(LateCreditsBindingError::NonCanonical)
    ));
    assert!(validate_parent_accounting_payload(&Bytes::new(), ctx.parent_hash).is_err());
}

#[test]
fn injected_payload_validation_runs_on_begin_and_end_steps() {
    use crate::{
        block::{begin_step, end_step, BlockOrder, ProtocolModule},
        system_tx_context::{SystemTxPayloadValidationError, SystemTxValidationContext},
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
    const REJECT_BEGIN: &[crate::block::ProtocolStep] = &[begin_step::<Reject>(); 9];
    const REJECT_END: &[crate::block::ProtocolStep] = &[end_step::<Reject>(); 5];
    let mut case = fixture("steady_disabled");
    for end in [false, true] {
        let order = if end {
            BlockOrder {
                end_block: REJECT_END,
                ..crate::test_utils::TEST_BLOCK_ORDER
            }
        } else {
            BlockOrder {
                begin_block: REJECT_BEGIN,
                ..crate::test_utils::TEST_BLOCK_ORDER
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
            &crate::test_utils::TEST_BLOCK_ORDER
        ),
        Err(LayoutBindingError::ParentAccounting(
            ParentAccountingBindingError::HashMismatch { .. }
        ))
    ));
}
