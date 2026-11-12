//! Shared deterministic fixture for domain and daemon Lysis tests.
use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    time::WorldwideDay,
    tribute_encryption::{TributeAmountsV2, TributeContextV2},
    wwd_entity_id::derive_poseidon_entity_id,
};
use outbe_tee::{
    lysis::LysisRequest,
    protocol::{FidelitySnapshotEntry, FidelitySnapshotRequest},
};
const GROUP_SIG: [u8; 48] = [9; 48];

pub(crate) fn tribute_state_key() -> [u8; 32] {
    *crate::tribute_encryption::derive_tribute_state_key(&GROUP_SIG, B256::from(U256::ONE), 0)
        .unwrap()
}

pub(crate) fn fixture() -> LysisRequest {
    let day = WorldwideDay::new(20261015);
    let timestamp = day.start_timestamp().unwrap() + 3600;
    let tribute_key = tribute_state_key();
    let mut tributes = Vec::new();
    for (index, nominal, currency) in [
        (1, 1_234_u64, 840),
        (2, 7_654_321, 978),
        (3, 987_654_321, 840),
    ] {
        let owner = Address::repeat_byte(index);
        tributes.push(
            crate::tribute_encryption::encrypt_tribute(
                &tribute_key,
                TributeContextV2 {
                    chain_id: 1,
                    tribute_id: derive_poseidon_entity_id(owner, day),
                    owner,
                    worldwide_day: day,
                    issuance_currency: 840,
                    reference_currency: currency,
                    tribute_price_minor: U256::from(2_000_000),
                    offer_input_hash: B256::repeat_byte(index),
                },
                &TributeAmountsV2 {
                    nominal_amount_minor: U256::from(nominal),
                    issuance_amount_minor: U256::from(nominal * 2),
                },
            )
            .unwrap(),
        );
    }
    tributes.sort_by_key(|tribute| tribute.context.tribute_id);
    let fidelity = FidelitySnapshotRequest {
        timestamp,
        first_qualified_start: 0,
        entries: tributes
            .iter()
            .map(|tribute| FidelitySnapshotEntry {
                owner: tribute.context.owner,
                cohort_blob: vec![],
            })
            .collect(),
    };
    LysisRequest {
        chain_id: 1,
        worldwide_day: day,
        emission_day: day.value(),
        timestamp,
        lysis_limit_minor: U256::from(1_000_000),
        tributes,
        fidelity,
        entry_prices: vec![
            (840, Some(U256::from(250_000))),
            (978, Some(U256::from(125_001))),
        ],
    }
}
