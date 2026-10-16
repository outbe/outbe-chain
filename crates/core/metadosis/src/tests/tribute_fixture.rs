use alloy_primitives::{Address, U256};
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{TributeContract, TributeData};

pub(super) struct FixtureTribute {
    pub(super) owner: Address,
    pub(super) wwd: WorldwideDay,
    pub(super) nominal: U256,
}

pub(super) fn issue_sealed_tribute(tribute: &mut TributeContract<'_>, input: FixtureTribute) {
    let FixtureTribute {
        owner,
        wwd,
        nominal,
    } = input;
    tribute.unseal_day(wwd).unwrap();
    tribute
        .issue(&TributeData {
            tribute_id: outbe_nod::identity::generate_nod_id(owner, wwd).unwrap(),
            owner,
            worldwide_day: wwd,
            issuance_amount_minor: nominal,
            issuance_currency: 840,
            nominal_amount_minor: nominal,
            reference_currency: 840,
            tribute_price_minor: U256::from(2),
        })
        .unwrap();
    tribute.seal_day(wwd).unwrap();
}
