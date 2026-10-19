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
