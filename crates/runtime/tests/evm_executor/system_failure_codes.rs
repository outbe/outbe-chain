use super::*;

fn revert_result() -> TxResult<OutbeEvmTypes> {
    evm2::TxResultExt {
        stop: InstrStop::Revert,
        ..Default::default()
    }
}

fn halt_result(stop: InstrStop) -> TxResult<OutbeEvmTypes> {
    evm2::TxResultExt {
        stop,
        ..Default::default()
    }
}

#[test]
fn revert_maps_to_201() {
    assert_eq!(system_tx_failure_code_for_result(&revert_result()), 201);
}

#[test]
fn out_of_gas_maps_to_202() {
    for stop in [
        InstrStop::OutOfGas,
        InstrStop::MemoryOOG,
        InstrStop::MemoryLimitOOG,
        InstrStop::PrecompileOOG,
        InstrStop::InvalidOperandOOG,
        InstrStop::ReentrancySentryOOG,
    ] {
        let r = halt_result(stop);
        assert_eq!(system_tx_failure_code_for_result(&r), 202, "{stop:?}");
    }
}

#[test]
fn other_halt_maps_to_299() {
    assert_eq!(
        system_tx_failure_code_for_result(&halt_result(InstrStop::PrecompileError)),
        299
    );
}
