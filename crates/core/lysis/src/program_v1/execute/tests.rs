use super::*;

#[test]
fn nod_action_uses_independent_keccak_vector_and_preserves_source_tribute() {
    let owner = alloy_primitives::Address::repeat_byte(0x42);
    let day = WorldwideDay::new(0x01020304);
    let source_id = derive_poseidon_entity_id(owner, day);
    let source = TributeInputV1 {
        tribute_id: source_id,
        owner,
        worldwide_day: day,
        issuance_currency: 840,
        nominal_amount_minor: SCALE,
        reference_currency: 978,
        tribute_price_minor: U256::ZERO,
    };
    let mut execution = prepare(day, vec![source], vec![1], SCALE).unwrap();
    let pending = execution.quote_next().unwrap();
    let action = execution.commit_next(pending, SCALE, 1, true).unwrap();
    assert_eq!(
        action.nod_id,
        U256::from_str_radix(
            "501ba769d4ff8f2faa11545b1608051d3009bb0cdd2518ef005b88ec71b5243b",
            16,
        )
        .unwrap()
    );
    assert_eq!(action.source_tribute_id, source_id);
    assert_eq!(
        source_id.to_u256(),
        U256::from_str_radix(
            "0102030405beb34363ddabb6c010e0b5883bfe90d9e06ff2eeffa66f61683c0b",
            16,
        )
        .unwrap()
    );
    assert_ne!(action.nod_id, source_id.to_u256());
}

#[test]
fn gratis_load_cap_preserves_fraction_math_and_full_precision() {
    let nod_gratis_load_max = U256::from(1000);
    for (nominal, fraction, expected) in [
        (1000, 500_000, 500),
        (2000, 500_000, 1000),
        (5000, 500_000, 1000),
        (1000, 2_000_000, 1000),
    ] {
        assert_eq!(
            calculate_gratis_load(
                U256::from(nominal),
                U256::from(fraction),
                nod_gratis_load_max,
                0
            )
            .unwrap(),
            U256::from(expected)
        );
    }
    assert_eq!(
        calculate_gratis_load(U256::MAX / SCALE, SCALE, U256::MAX / U256::from(1024), 0).unwrap(),
        U256::MAX / SCALE
    );
    assert!(matches!(
        calculate_gratis_load(U256::MAX, SCALE, nod_gratis_load_max, 0),
        Err(ProgramErrorV1::Arithmetic { .. })
    ));
    assert!(matches!(
        calculate_gratis_load(U256::ZERO, SCALE, nod_gratis_load_max, 7),
        Err(ProgramErrorV1::ZeroGratisLoad { ordinal: 7 })
    ));
}

#[test]
fn frozen_budget_cap_boundaries_and_remaining_accounting() {
    let day = WorldwideDay::new(20_260_724);
    let tributes = [1_u8, 2].map(|seed| {
        let owner = alloy_primitives::Address::repeat_byte(seed);
        TributeInputV1 {
            tribute_id: derive_poseidon_entity_id(owner, day),
            owner,
            worldwide_day: day,
            issuance_currency: 840,
            nominal_amount_minor: U256::from(1_000_000),
            reference_currency: 840,
            tribute_price_minor: U256::ZERO,
        }
    });
    for budget in [1023_u64, 1024, 1025, 1_025_023] {
        let mut tributes = tributes.to_vec();
        tributes.sort_by_key(|tribute| tribute.tribute_id);
        let mut execution = prepare(day, tributes, vec![1, 1], U256::from(budget)).unwrap();
        let fractions = execution.fractions.clone();
        if budget < 1024 {
            assert!(matches!(
                execution.quote_next(),
                Err(ProgramErrorV1::ZeroGratisLoad { ordinal: 0 })
            ));
            continue;
        }
        let cap = U256::from(budget / 1024);
        for _ in 0..2 {
            let pending = execution.quote_next().unwrap();
            assert_eq!(pending.gratis_load_minor, cap);
            execution.commit_next(pending, SCALE, 1, true).unwrap();
        }
        assert_eq!(execution.fractions, fractions);
        assert_eq!(
            execution.finish().unwrap().remaining_lysis_limit_minor,
            U256::from(budget) - cap * U256::from(2)
        );
    }
}

#[test]
fn higher_leagues_receive_higher_fractions_within_limit() {
    for (populations, nominals, allocation) in [
        ([1_u32, 1, 1], [1_000_000_u64; 3], 960_000_u64),
        ([1000, 50, 1], [999_998, 1, 1], 320_000),
        ([1, 50, 1000], [1, 1, 999_998], 320_000),
        ([1, 1, 1], [1, 1, 1], 1),
        ([1, 1, 1], [1_000_000; 3], 3),
    ] {
        let leagues = [1, 2048, 4096];
        let groups = leagues
            .into_iter()
            .zip(populations.into_iter().zip(nominals.map(U256::from)))
            .collect();
        let total = nominals.into_iter().map(U256::from).sum();
        let allocation = U256::from(allocation);
        let fractions = compute_fraction_map_from_groups(
            &groups,
            populations.into_iter().sum(),
            total,
            allocation,
        )
        .unwrap();
        assert!(fractions[&1] <= fractions[&2048]);
        assert!(fractions[&2048] <= fractions[&4096]);
        let spent = leagues
            .into_iter()
            .zip(nominals)
            .map(|(league, nominal)| U256::from(nominal) * fractions[&league] / SCALE)
            .sum::<U256>();
        assert!(spent <= allocation);
        if nominals == [1_000_000; 3] && allocation == U256::from(960_000) {
            assert!(fractions[&1] < fractions[&2048]);
            assert!(fractions[&2048] < fractions[&4096]);
        }
        if allocation == U256::from(3) {
            assert!(fractions[&1] == fractions[&2048] || fractions[&2048] == fractions[&4096]);
        }
    }
}
