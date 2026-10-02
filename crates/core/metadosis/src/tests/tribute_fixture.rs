use alloy_primitives::{Address, U256};
use outbe_compressed_entities::{ExecutionScope, ParentBodySource};
use outbe_nod::NodContract;
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{TributeContract, TributeData};

pub(super) struct FixtureTribute {
    pub(super) owner: Address,
    pub(super) wwd: WorldwideDay,
    pub(super) nominal: U256,
}

pub(super) fn issue_sealed_tribute(
    tribute: &mut TributeContract<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    input: FixtureTribute,
) {
    let FixtureTribute {
        owner,
        wwd,
        nominal,
    } = input;
    tribute.initialize_fresh_ocomp_profile().unwrap();
    tribute.unseal_day(wwd).unwrap();
    tribute
        .issue(
            scope,
            parent,
            &TributeData {
                tribute_id: NodContract::generate_nod_id(owner, wwd).unwrap(),
                owner,
                worldwide_day: wwd,
                issuance_amount_minor: nominal,
                issuance_currency: 840,
                nominal_amount_minor: nominal,
                reference_currency: 840,
                exclude_from_intex_issuance: false,
                tribute_price_minor: U256::from(2),
            },
        )
        .unwrap();
    tribute.seal_day(wwd).unwrap();
}
