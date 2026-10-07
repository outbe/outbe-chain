//! The per-(series, owner) mining sequence: a counter that cannot wrap, and
//! that moves only together with a committed burn and mint.

use super::*;

/// The provider counts every write and event it is asked to apply, so a zero
/// count around the refused call proves it wrote nothing.
#[test]
fn a_mining_sequence_at_its_maximum_refuses_the_next_mining_without_writes() {
    use crate::sol_ext::IERC1155;

    let mut storage = factory_provider();
    storage.stub_sub_call_at_selector(
        crate::constants::INTEX_NFT1155_ADDRESS,
        IERC1155::balanceOfCall::SELECTOR,
        word(1),
    );
    let nonce = StorageHandle::enter(&mut storage, |s| {
        select_prod_profile(&s);
        runtime::issue(&s, sample(7)).unwrap();
        outbe_intex::api::record_settled_units(&s, sid(7), 1).unwrap();
        let promis_amount = outbe_intex::api::read_series(&s, sid(7))
            .unwrap()
            .promis_load_minor;
        let mut factory = IntexFactoryContract::new(s.clone());
        factory.write_mine_seq(sid(7), owner(), u32::MAX).unwrap();
        (0u64..)
            .find(|nonce| {
                runtime::validate_pow(owner(), promis_amount, sid(7), u32::MAX, *nonce).is_ok()
            })
            .unwrap()
    });
    let slots = storage.storage.clone();
    let events = storage.get_ordered_events().to_vec();
    let (series, counts) = StorageHandle::enter(&mut storage, |s| {
        (
            outbe_intex::api::read_series(&s, sid(7)).unwrap(),
            outbe_intex::api::unit_counts(&s, sid(7)).unwrap(),
        )
    });
    // Reset the provider's write/event counter after the fixture.
    storage.clear_mutation_failure();

    let error = StorageHandle::enter(&mut storage, |s| {
        runtime::mine_promis(
            &s,
            sid(7),
            owner(),
            U256::from(1),
            runtime::MiningProof {
                nonce,
                auth: outbe_promisfactory::api::ModifyAuth {
                    mac: [0u8; 32],
                    op_nonce: 0,
                },
            },
        )
        .unwrap_err()
    });
    assert!(
        format!("{error:?}").contains("mining sequence overflow"),
        "{error:?}"
    );

    assert_eq!(
        storage.clear_mutation_failure(),
        0,
        "the refusal must reach the provider with no write or event"
    );
    assert_eq!(storage.storage, slots, "no slot changed");
    assert_eq!(storage.get_ordered_events(), events, "no event was emitted");
    StorageHandle::enter(&mut storage, |s| {
        assert_eq!(
            IntexFactoryContract::new(s.clone())
                .read_mine_seq(sid(7), owner())
                .unwrap(),
            u32::MAX,
            "the sequence must not wrap"
        );
        let after = outbe_intex::api::read_series(&s, sid(7)).unwrap();
        assert_eq!(after.state, series.state);
        assert_eq!(after.issued_units, series.issued_units);
        assert_eq!(after.promis_load_minor, series.promis_load_minor);
        let after = outbe_intex::api::unit_counts(&s, sid(7)).unwrap();
        assert_eq!(
            [
                after.issued,
                after.active,
                after.settled,
                after.exercised,
                after.gem_factory,
                after.forfeited
            ],
            [
                counts.issued,
                counts.active,
                counts.settled,
                counts.exercised,
                counts.gem_factory,
                counts.forfeited
            ],
            "the paid unit stays settled and unexercised"
        );
    });
}
