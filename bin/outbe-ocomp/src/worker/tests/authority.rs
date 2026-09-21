use super::*;

fn plan() -> PlanCommitmentV1 {
    PlanCommitmentV1 {
        protocol_bundle_hash: B256::repeat_byte(1),
        job_id: B256::repeat_byte(2),
        attempt: 0,
        input_manifest_hash: B256::repeat_byte(4),
        wwd: 20_260_724,
        lysis_limit_minor: U256::from(99_000_000_u64),
        logical_evaluation_time: 1_784_765_900,
        tribute_count: 257,
        max_tributes_per_work_shard: 256,
        primary_work_unit_count: 2,
        primary_work_unit_root: B256::repeat_byte(5),
        planner_spec_version: 1,
        reducer_spec_version: 1,
    }
}

#[test]
fn changed_frozen_plan_context_is_rejected_even_when_job_and_manifest_bindings_match() {
    let limits = poc_schema_limits();
    let committed = plan();
    let expected = ExpectedPlanBindingsV1 {
        plan_hash: committed.plan_hash(&limits).unwrap(),
        protocol_bundle_hash: committed.protocol_bundle_hash,
        job_id: committed.job_id,
        attempt: committed.attempt,
        input_manifest_hash: committed.input_manifest_hash,
        wwd: committed.wwd,
        tribute_count: committed.tribute_count,
        planner_spec_version: committed.planner_spec_version,
        reducer_spec_version: committed.reducer_spec_version,
    };
    require_plan_binding(&committed, expected, &limits).unwrap();

    let mut changed = committed;
    changed.lysis_limit_minor += U256::from(1);
    assert!(require_plan_binding(&changed, expected, &limits).is_err());
}
