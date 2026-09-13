use super::*;
use revm::context::result::ResultGas;

fn revert_result() -> ExecutionResult<HaltReason> {
    ExecutionResult::Revert {
        gas: ResultGas::default(),
        logs: Vec::new(),
        output: Default::default(),
    }
}

fn halt_result(reason: HaltReason) -> ExecutionResult<HaltReason> {
    ExecutionResult::Halt {
        reason,
        gas: ResultGas::default(),
        logs: Vec::new(),
    }
}

#[test]
fn revert_maps_to_201() {
    assert_eq!(system_tx_failure_code_for_result(&revert_result()), 201);
}

#[test]
fn only_the_exact_ocomp_deadline_revert_is_a_failed_carrier_receipt() {
    let deadline = ExecutionResult::Revert {
        gas: ResultGas::default(),
        logs: Vec::new(),
        output: outbe_metadosis::deadline_passed_result_vote_revert_data(),
    };
    assert!(is_ocomp_deadline_passed_revert(&deadline));
    assert!(!is_ocomp_deadline_passed_revert(&revert_result()));
    assert!(!is_ocomp_deadline_passed_revert(&halt_result(
        HaltReason::OutOfGas(OutOfGasError::Precompile)
    )));
}

#[test]
fn only_typed_materialization_race_and_proof_rejections_are_failed_receipts() {
    use outbe_nodfactory::materialization::{
        materialization_revert_data, NodMaterializationRejectionV1,
    };

    for rejection in [
        NodMaterializationRejectionV1::StaleQueueSequence,
        NodMaterializationRejectionV1::StaleCursor,
        NodMaterializationRejectionV1::AttemptLimit,
        NodMaterializationRejectionV1::InvalidBatchShape,
        NodMaterializationRejectionV1::InvalidProof,
        NodMaterializationRejectionV1::DuplicateNod,
    ] {
        let result = ExecutionResult::Revert {
            gas: ResultGas::default(),
            logs: Vec::new(),
            output: materialization_revert_data(rejection),
        };
        assert!(is_nod_materialization_soft_revert(&result));
    }

    assert!(!is_nod_materialization_soft_revert(&revert_result()));
    assert!(!is_nod_materialization_soft_revert(
        &ExecutionResult::Revert {
            gas: ResultGas::default(),
            logs: Vec::new(),
            output: materialization_revert_data(NodMaterializationRejectionV1::UnauthorizedSigner,),
        }
    ));
    assert!(!is_nod_materialization_soft_revert(&halt_result(
        HaltReason::OutOfGas(OutOfGasError::Precompile),
    )));
}

#[test]
fn out_of_gas_maps_to_202() {
    for variant in [
        OutOfGasError::Basic,
        OutOfGasError::MemoryLimit,
        OutOfGasError::Memory,
        OutOfGasError::Precompile,
        OutOfGasError::InvalidOperand,
        OutOfGasError::ReentrancySentry,
    ] {
        let r = halt_result(HaltReason::OutOfGas(variant));
        assert_eq!(system_tx_failure_code_for_result(&r), 202, "{variant:?}");
    }
}

#[test]
fn other_halt_maps_to_299() {
    assert_eq!(
        system_tx_failure_code_for_result(&halt_result(HaltReason::PrecompileError)),
        299
    );
}

#[test]
fn codes_are_in_phase_band() {
    for code in [
        system_tx_failure_code_for_result(&revert_result()),
        system_tx_failure_code_for_result(&halt_result(HaltReason::OutOfGas(
            OutOfGasError::Memory,
        ))),
        system_tx_failure_code_for_result(&halt_result(HaltReason::PrecompileError)),
    ] {
        assert!(
            (200..=299).contains(&code),
            "phase failure code {code} outside 200..=299 band"
        );
    }
}
