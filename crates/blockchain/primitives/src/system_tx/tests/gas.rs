use super::*;

#[test]
fn visible_gas_plan_assigns_only_cycle_the_block_remainder() {
    let inputs = [
        input_for(SystemTxKind::CertifiedParentAccounting)
            .encode()
            .expect("CPA input encodes"),
        input_for(SystemTxKind::CycleTick)
            .encode()
            .expect("CycleTick input encodes"),
        input_for(SystemTxKind::HookEvents)
            .encode()
            .expect("HookEvents input encodes"),
    ];
    let entries = [
        (SystemTxKind::CertifiedParentAccounting, inputs[0].clone()),
        (SystemTxKind::CycleTick, inputs[1].clone()),
        (SystemTxKind::HookEvents, inputs[2].clone()),
    ];
    let intrinsic_total = inputs
        .iter()
        .map(|input| system_tx_intrinsic_gas(input).expect("intrinsic gas computes"))
        .sum::<u64>();
    let block_gas_limit = intrinsic_total + 50_000;

    let plan = SystemTxVisibleGasPlan::new(block_gas_limit, &entries)
        .expect("system gas plan fits the block");

    assert_eq!(plan.gas_limit(0), Some(plan.intrinsic_gas(0).unwrap()));
    assert_eq!(plan.ce_gas_limit(0), Some(0));
    assert_eq!(plan.ce_gas_limit(1), Some(50_000));
    assert_eq!(
        plan.gas_limit(1),
        Some(plan.intrinsic_gas(1).unwrap() + 50_000)
    );
    assert_eq!(plan.gas_limit(2), Some(plan.intrinsic_gas(2).unwrap()));
    assert_eq!(plan.total_envelope_gas(), block_gas_limit);
}

#[test]
fn visible_gas_plan_reserves_ocomp_terminal_ce_before_cycle_remainder() {
    let ocomp = input_for(SystemTxKind::OcompLifecycleBegin)
        .encode()
        .expect("OCOMP lifecycle input encodes");
    let cycle = input_for(SystemTxKind::CycleTick)
        .encode()
        .expect("CycleTick input encodes");
    let entries = [
        (SystemTxKind::OcompLifecycleBegin, ocomp.clone()),
        (SystemTxKind::CycleTick, cycle.clone()),
    ];
    let intrinsic_total = system_tx_intrinsic_gas(&ocomp).expect("OCOMP intrinsic gas computes")
        + system_tx_intrinsic_gas(&cycle).expect("Cycle intrinsic gas computes");
    let cycle_remainder = 50_000;
    let block_gas_limit = intrinsic_total + OCOMP_LIFECYCLE_CE_GAS_RESERVE + cycle_remainder;

    let plan = SystemTxVisibleGasPlan::new(block_gas_limit, &entries)
        .expect("system gas plan fits the block");

    assert_eq!(plan.protocol_precharge(0), Some(0));
    assert_eq!(plan.ce_gas_limit(0), Some(OCOMP_LIFECYCLE_CE_GAS_RESERVE));
    assert_eq!(
        plan.gas_limit(0),
        Some(plan.intrinsic_gas(0).unwrap() + OCOMP_LIFECYCLE_CE_GAS_RESERVE)
    );
    assert_eq!(plan.ce_gas_limit(1), Some(cycle_remainder));
    assert_eq!(plan.total_envelope_gas(), block_gas_limit);
}

#[test]
fn visible_gas_plan_rejects_system_intrinsic_gas_above_the_block_limit() {
    let cycle = input_for(SystemTxKind::CycleTick)
        .encode()
        .expect("CycleTick input encodes");
    let intrinsic = system_tx_intrinsic_gas(&cycle).expect("intrinsic gas computes");

    let error = SystemTxVisibleGasPlan::new(intrinsic - 1, &[(SystemTxKind::CycleTick, cycle)])
        .expect_err("system envelope cannot exceed the block gas limit");

    assert_eq!(
        error,
        SystemTxError::VisibleGasPlanExceedsBlock {
            required_gas: intrinsic,
            block_gas_limit: intrinsic - 1,
        }
    );
}
