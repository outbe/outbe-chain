use super::*;

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
    let block_gas_limit = intrinsic_total + 50_000;

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
    assert_eq!(plan.total_envelope_gas(), block_gas_limit);
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

    let error = SystemTxVisibleGasPlan::new(intrinsic - 1, &[cycle])
        .expect_err("system envelope cannot exceed the block gas limit");

    assert_eq!(
        error,
        SystemTxError::VisibleGasPlanExceedsBlock {
            required_gas: intrinsic,
            block_gas_limit: intrinsic - 1,
        }
    );
}
