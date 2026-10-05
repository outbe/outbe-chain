//! A called Gem settles on the ERC-20 rail up to and including its deadline and is refused
//! a second later, when its status already reads Forfeited.

use super::*;

fn gem_state(world: &mut World) -> u8 {
    world
        .view(
            GEM_ADDRESS,
            IGem::getGemStatusCall {
                gemId: world.gem_id,
            },
        )
        .state
}

#[test]
fn a_called_gem_settles_at_its_deadline_and_is_refused_a_second_later() {
    // At the deadline the payment goes through exactly as quoted.
    let (mut world, deadline) = World::build(Factory::Gem, OWNER, true, Some(TIMESTAMP));
    world.ctx.block.timestamp = U256::from(deadline);
    assert_eq!(gem_state(&mut world), outbe_gem::GemState::Called as u8);
    let out = world.settle();
    assert!(
        matches!(out.status, SubCallStatus::Success),
        "{:?}",
        out.status
    );
    assert_eq!(world.balances(), world.paid_balances());
    world.assert_settled();
    assert_eq!(gem_state(&mut world), outbe_gem::GemState::Settled as u8);

    // One second later the same payment is refused and nothing moves.
    let (mut world, deadline) = World::build(Factory::Gem, OWNER, true, Some(TIMESTAMP));
    world.ctx.block.timestamp = U256::from(deadline + 1);
    let balances = world.balances();
    let allowances = world.allowances();
    let logs = world.ctx.journaled_state.logs().to_vec();
    let out = world.settle();
    assert!(
        matches!(out.status, SubCallStatus::Revert(_)),
        "{:?}",
        out.status
    );
    assert_eq!(world.balances(), balances);
    assert_eq!(world.allowances(), allowances);
    assert_eq!(world.ctx.journaled_state.logs(), logs);
    let gem = world.view(
        GEM_ADDRESS,
        IGem::getGemStatusCall {
            gemId: world.gem_id,
        },
    );
    assert_eq!(
        gem.state,
        outbe_gem::GemState::Forfeited as u8,
        "the public status projects Forfeited before any cleanup"
    );
    assert_eq!(gem.owner, OWNER);
}
