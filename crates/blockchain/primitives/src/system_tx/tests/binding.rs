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
    use super::fixtures;
    use crate::{
        error::PrecompileError,
        reshare_artifact::{
            encode_outbe_block_artifacts, ConsensusHeaderArtifact, OutbeBlockArtifacts,
        },
        system_tx_context::{SystemTxPayloadValidationError, SystemTxValidationContext},
        tee_attestation_v1::CodecError,
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
        let validator = |payload: &[u8],
                         ctx: &SystemTxValidationContext<'_>|
         -> Result<(), SystemTxPayloadValidationError> {
            match tx.step_index() {
                0 => {
                    validate_parent_accounting_payload(payload, ctx.parent_hash)?;
                }
                1 => {
                    validate_late_finalize_credits_payload(payload, ctx.artifacts)?;
                }
                2 => {
                    validate_boundary_outcome_payload(payload, ctx.artifacts)?;
                }
                3 => {
                    validate_tee_bootstrap_payload(payload, ctx.block_number)?;
                }
                5 => {
                    validate_publication_payload(payload, ctx.artifacts)?;
                }
                _ => unreachable!("fixture must exercise one of the five payload validators"),
            }
            Ok(())
        };
        validator(tx.payload(), &ctx).unwrap();
        let mut trailing = tx.payload().to_vec();
        trailing.push(0);
        let result = validator(&trailing, &ctx);
        match tx.step_index() {
            0 => assert!(matches!(
                result,
                Err(SystemTxPayloadValidationError::ParentAccounting(
                    ParentAccountingBindingError::PayloadDecode(PrecompileError::Revert(_))
                ))
            )),
            1 => assert!(matches!(
                result,
                Err(SystemTxPayloadValidationError::LateCredits(
                    LateCreditsBindingError::PayloadDecode(PrecompileError::Revert(_))
                ))
            )),
            2 => assert!(matches!(
                result,
                Err(SystemTxPayloadValidationError::Boundary(
                    BoundaryBindingError::PayloadDecode(PrecompileError::Revert(_))
                ))
            )),
            3 => assert!(matches!(
                result,
                Err(SystemTxPayloadValidationError::TeeBootstrap {
                    block_number: 2,
                    source: CodecError::TrailingBytes(1)
                })
            )),
            5 => assert!(matches!(
                result,
                Err(SystemTxPayloadValidationError::Publication(
                    PublicationBindingError::PayloadDecode(PrecompileError::Revert(_))
                ))
            )),
            _ => unreachable!("fixture must exercise one of the five payload validators"),
        }
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
    assert!(matches!(
        validate_parent_accounting_payload(&Bytes::new(), ctx.parent_hash),
        Err(ParentAccountingBindingError::PayloadDecode(
            PrecompileError::Revert(_)
        ))
    ));
}
