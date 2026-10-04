use alloy_primitives::{Address, U256};
use outbe_compressed_entities::derive_poseidon_entity_id;
use outbe_lysis::program_v1::{
    artifacts::{decode_amount_run, encode_amount_run},
    execute,
    phases::{amount_map, fidelity_map, finalize_fi_fraction_table},
    ObservationValueV1, ObservedTributeV1, ProgramInputV1, TributeInputV1,
};
use outbe_ocomp_protocol::profile::poc_schema_limits;
use outbe_primitives::time::WorldwideDay;

fn observation() -> ObservedTributeV1 {
    let owner = Address::repeat_byte(1);
    let day = WorldwideDay::new(20260724);
    ObservedTributeV1 {
        tribute: TributeInputV1 {
            tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
            owner,
            worldwide_day: day,
            issuance_currency: 840,
            reference_currency: 840,
            nominal_amount_minor: U256::from(1_000_000),
            tribute_price_minor: U256::from(19),
            exclude_from_intex_issuance: false,
        },
        first_league: ObservationValueV1::Value(1),
        second_league: ObservationValueV1::Value(1),
        entry_price_minor: ObservationValueV1::Value(U256::from(19)),
        nod_target_available: true,
    }
}

#[test]
fn sequential_lysis_issues_a_positive_load_with_a_zero_floored_reference_cost() {
    let observed = observation();
    let result = execute(ProgramInputV1 {
        worldwide_day: observed.tribute.worldwide_day,
        logical_evaluation_time: 1_784_765_900,
        lysis_limit_minor: U256::from(25_629),
        tributes: vec![observed],
    })
    .expect("positive dust remains an issuable right");
    assert_eq!(result.nod_actions.len(), 1);
    assert_eq!(result.nod_actions[0].gratis_load_minor, U256::from(25_629));
    assert_eq!(result.nod_actions[0].entry_price_minor, U256::from(19));
    assert_eq!(result.nod_actions[0].settlement_cost_minor, U256::ZERO);
}

#[test]
fn phased_lysis_preserves_positive_dust_through_the_canonical_amount_artifact() {
    let tributes = vec![observation()];
    let budget = U256::from(25_629);
    let fidelity = fidelity_map(0, &tributes).unwrap();
    let fractions = finalize_fi_fraction_table(&fidelity.aggregate, budget).unwrap();
    let amount = amount_map(0, &tributes, &fidelity.observations, &fractions)
        .expect("worker amount map must share the exact floor");
    assert_eq!(amount.ordered_records.len(), 1);
    assert_eq!(amount.ordered_records[0].gratis_load_minor, budget);
    assert_eq!(amount.ordered_records[0].settlement_cost_minor, U256::ZERO);
    let limits = poc_schema_limits();
    let encoded = encode_amount_run(&amount, &limits).unwrap();
    assert_eq!(decode_amount_run(&encoded, &limits).unwrap(), amount);
}
