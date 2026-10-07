//! The direct settlement rail converts through the trailing window and refuses
//! a payment whose target leg that window never priced, before any write.

use super::*;

/// The euro trades live and the dollar leg is priced in the window, but the
/// window holds no euro price. The rail refuses the direct euro settlement as
/// oracle unavailable, and the provider records no write or event.
#[test]
fn a_direct_settlement_in_a_currency_the_window_never_priced_is_refused_without_writes() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xa7));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SIX_DECIMALS));
    world.publish_coen_spot(978, U256::from(SIX_DECIMALS));
    let required = world.enter(|storage, _, _| {
        outbe_oracle::api::current_vwap_snapshot(storage.clone())
            .unwrap()
            .to_u256()
    });
    let slots = world.provider.storage.clone();
    let events = world.provider.get_ordered_events().to_vec();
    world.provider.clear_mutation_failure();

    let error = settle_erc20(&mut world, nod_id, input.owner, EUR_ASSET, required).unwrap_err();
    assert!(
        error.to_string().contains("oracle nominal unavailable"),
        "{error}"
    );

    assert_eq!(
        world.provider.clear_mutation_failure(),
        0,
        "the refusal reaches the provider with no write or event"
    );
    assert_eq!(world.provider.storage, slots);
    assert_eq!(world.provider.get_ordered_events(), events);
    assert!(!is_settled(&mut world, nod_id));
}
