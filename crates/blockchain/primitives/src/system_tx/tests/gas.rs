use super::*;
use crate::protocol_schedule::OutbeProtocolSchedule;

#[test]
fn end_block_reservation_is_fixed_without_a_height_condition() {
    let reserve = OutbeProtocolSchedule::END_BLOCK_RESERVED_GAS;
    assert_eq!(reserve, 2_100_000);
    assert_eq!(reserve, 30_000_000 * 7 / 100);

    for limit in [reserve, 30_000_000, 60_000_000, u64::MAX] {
        let budget = BlockGasBudget::new(limit).unwrap();
        assert_eq!(budget.reserved_gas(), reserve);
        assert_eq!(budget.pre_end_capacity(), limit - reserve);
        assert_eq!(budget.end_capacity(0), Ok(limit));
    }
}

#[test]
fn end_block_budget_rejects_small_limits_and_invalid_accounting() {
    let reserve = OutbeProtocolSchedule::END_BLOCK_RESERVED_GAS;
    for limit in [0, reserve - 1] {
        assert_eq!(
            BlockGasBudget::new(limit),
            Err(BlockGasBudgetError::BlockGasLimitBelowReserve {
                block_gas_limit: limit,
                reserved_gas: reserve,
            })
        );
    }
    let budget = BlockGasBudget::new(reserve).unwrap();
    assert_eq!(budget.pre_end_capacity(), 0);
    assert_eq!(budget.remaining_pre_end_gas(0), Ok(0));
    assert_eq!(budget.end_capacity(0), Ok(reserve));

    for (limit, consumed) in [(reserve, 1), (30_000_000, 27_900_001), (u64::MAX, u64::MAX)] {
        let budget = BlockGasBudget::new(limit).unwrap();
        let error = BlockGasBudgetError::PreEndGasExceeded {
            consumed_gas: consumed,
            pre_end_capacity: limit - reserve,
        };
        assert_eq!(budget.remaining_pre_end_gas(consumed), Err(error));
        assert_eq!(budget.end_capacity(consumed), Err(error));
    }
    let budget = BlockGasBudget::new(reserve).unwrap();
    assert!(budget.end_capacity(u64::MAX).is_err());
}

#[test]
fn end_capacity_includes_unused_pre_end_capacity() {
    let budget = BlockGasBudget::new(30_000_000).unwrap();
    assert_eq!(budget.pre_end_capacity(), 27_900_000);
    assert_eq!(budget.remaining_pre_end_gas(18_000_000), Ok(9_900_000));
    assert_eq!(budget.end_capacity(18_000_000), Ok(12_000_000));
    assert_eq!(budget.remaining_pre_end_gas(27_900_000), Ok(0));
    assert_eq!(budget.end_capacity(27_900_000), Ok(2_100_000));
}

#[test]
fn visible_gas_plan_assigns_only_cycle_the_block_remainder() {
    let inputs = [
        accounting().encode().expect("CPA input encodes"),
        SystemTx::CycleTick
            .encode()
            .expect("CycleTick input encodes"),
        SystemTx::HookEvents
            .encode()
            .expect("HookEvents input encodes"),
    ];
    let intrinsic_total = inputs
        .iter()
        .map(|input| system_tx_intrinsic_gas(input).expect("intrinsic gas computes"))
        .sum::<u64>();
    let begin_gas_limit = intrinsic_total + 50_000;
    let block_gas_limit = begin_gas_limit + OutbeProtocolSchedule::END_BLOCK_RESERVED_GAS;

    let plan = SystemTxVisibleGasPlan::new(block_gas_limit, &inputs)
        .expect("system gas plan fits the block");

    assert_eq!(plan.gas_limit(0), Some(plan.intrinsic_gas(0).unwrap()));
    assert_eq!(plan.execution_gas_limit(0), Ok(Some(0)));
    assert_eq!(plan.execution_gas_limit(1), Ok(Some(50_000)));
    assert_eq!(
        plan.gas_limit(1),
        Some(plan.intrinsic_gas(1).unwrap() + 50_000)
    );
    assert_eq!(plan.gas_limit(2), Some(plan.intrinsic_gas(2).unwrap()));
    assert_eq!(plan.total_envelope_gas(), begin_gas_limit);
}

#[test]
fn gas_plan_rejects_duplicate_cycle_and_malformed_body() {
    let cycle = SystemTx::CycleTick.encode().unwrap();
    assert_eq!(
        SystemTxVisibleGasPlan::new(u64::MAX, &[cycle.clone(), cycle]),
        Err(SystemTxError::DuplicateCycleTickGasBudget)
    );
    assert_eq!(
        SystemTxVisibleGasPlan::new(u64::MAX, &[Bytes::from_static(b"OSC2\x02\x00")]),
        Err(SystemTxError::UnexpectedBody {
            kind: "CycleTick",
            len: 1
        })
    );
}

#[test]
fn visible_gas_plan_rejects_system_intrinsic_gas_above_the_block_limit() {
    let cycle = SystemTx::CycleTick
        .encode()
        .expect("CycleTick input encodes");
    let intrinsic = system_tx_intrinsic_gas(&cycle).expect("intrinsic gas computes");

    let reserve = OutbeProtocolSchedule::END_BLOCK_RESERVED_GAS;
    let error = SystemTxVisibleGasPlan::new(reserve + intrinsic - 1, std::slice::from_ref(&cycle))
        .expect_err("system envelope cannot exceed the block gas limit");

    assert_eq!(
        error,
        SystemTxError::BlockGasBudget(BlockGasBudgetError::PreEndGasExceeded {
            consumed_gas: intrinsic,
            pre_end_capacity: intrinsic - 1,
        })
    );
    let exact = SystemTxVisibleGasPlan::new(reserve + intrinsic, &[cycle]).unwrap();
    assert_eq!(exact.total_envelope_gas(), intrinsic);
    assert_eq!(exact.execution_gas_limit(0), Ok(Some(0)));
}

#[test]
fn begin_envelopes_reserve_gas_at_genesis_and_bootstrap_limits() {
    let inputs = [SystemTx::CycleTick.encode().unwrap()];
    let intrinsic = system_tx_intrinsic_gas(&inputs[0]).unwrap();
    let reserve = OutbeProtocolSchedule::END_BLOCK_RESERVED_GAS;
    for limit in [STEADY_BLOCK_GAS_LIMIT, BOOTSTRAP_BLOCK_GAS_LIMIT] {
        let plan = SystemTxVisibleGasPlan::new(limit, &inputs).unwrap();
        assert_eq!(plan.total_envelope_gas(), limit - reserve);
        assert_eq!(plan.gas_limit(0), Some(limit - reserve));
        assert_eq!(
            plan.execution_gas_limit(0),
            Ok(Some(limit - reserve - intrinsic))
        );
    }
}

#[test]
fn reserved_begin_plan_preserves_prebuilt_phase1_witness_bytes() {
    let signer = OutbeEvmSigner::from_secret_bytes([7; 32]).unwrap();
    let calldata = accounting().encode().unwrap();
    let inputs = [calldata.clone(), SystemTx::CycleTick.encode().unwrap()];
    for height in [2, 42] {
        for limit in [30_000_000, 60_000_000, u64::MAX] {
            let plan = SystemTxVisibleGasPlan::new(limit, &inputs).unwrap();
            let prebuilt = signer
                .sign_unsigned(
                    build_unsigned_system_tx(0, height, CHAIN_ID, calldata.clone()).unwrap(),
                )
                .unwrap();
            let planned = signer
                .sign_unsigned(
                    build_unsigned_system_tx_with_gas_limit(SystemTxEnvelopeInput {
                        ordinal: 0,
                        block_number: height,
                        chain_id: CHAIN_ID,
                        calldata: calldata.clone(),
                        gas_limit: plan.gas_limit(0).unwrap(),
                    })
                    .unwrap(),
                )
                .unwrap();
            assert_eq!(prebuilt.encoded_2718(), planned.encoded_2718());
            assert_eq!(
                validate_phase1_witness_against(
                    &planned,
                    &calldata,
                    signer.address(),
                    CHAIN_ID,
                    height,
                ),
                Ok(prebuilt.signature_hash())
            );
        }
    }
}

#[test]
fn gas_plan_checks_reserve_even_without_begin_transactions() {
    let reserve = OutbeProtocolSchedule::END_BLOCK_RESERVED_GAS;
    assert_eq!(
        SystemTxVisibleGasPlan::new(reserve - 1, &[]),
        Err(SystemTxError::BlockGasBudget(
            BlockGasBudgetError::BlockGasLimitBelowReserve {
                block_gas_limit: reserve - 1,
                reserved_gas: reserve,
            }
        ))
    );
    assert_eq!(
        SystemTxVisibleGasPlan::new(reserve, &[])
            .unwrap()
            .total_envelope_gas(),
        0
    );
}
