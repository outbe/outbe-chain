//! The refusals that guard a lifecycle, each read as the exact revert text of the
//! product's own error: paying on wrong terms, mining unpaid or twice, moving.

use alloy_primitives::{Address, FixedBytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_gemfactory::errors::GemFactoryError;
use outbe_intexfactory::errors::IntexFactoryError;
use outbe_nodfactory::errors::NodFactoryError;
use outbe_vaultrouter::settlement::SettlementError;
use std::fmt::Display;

use super::entity::{Item, Target};
use super::markets::{currency, EUR_ISO, MYR_ISO};
use super::payment::{assert_refused, factory, quote, third_party_key};
use crate::internal::{addresses, eth};
use crate::world::World;

/// An issued holding refuses a stale pricing snapshot and an asset in a third currency.
pub(crate) fn assert_payment_guards(world: &World, target: &Target) {
    let required = quote(world, target, currency(world, MYR_ISO).asset).snapshot;
    assert!(
        !required.is_zero(),
        "an issuance-currency quote names its pricing snapshot"
    );
    match &target.item {
        Item::Gem(id) => refuse_wrong_terms(world, target, required, |asset| {
            erc20_gem(*id, asset, U256::ZERO)
        }),
        Item::Series { id, units } => refuse_wrong_terms(world, target, required, |asset| {
            erc20_series(target, *id, *units, asset, U256::ZERO)
        }),
        Item::Nod(id) => refuse_wrong_terms(world, target, required, |asset| {
            erc20_nod(*id, asset, U256::ZERO)
        }),
    }
}

/// Pays with snapshot zero in MYR, which names the wrong snapshot, and in EUR, which the
/// holding does not accept.
fn refuse_wrong_terms<C: SolCall>(
    world: &World,
    target: &Target,
    required: U256,
    payment: impl Fn(Address) -> C,
) {
    let payer = third_party(world);
    assert_refused(
        world,
        payer,
        factory(target),
        &payment(currency(world, MYR_ISO).asset),
        SettlementError::SnapshotMismatch {
            authorized: U256::ZERO,
            required,
        },
    );
    assert_refused(
        world,
        payer,
        factory(target),
        &payment(currency(world, EUR_ISO).asset),
        SettlementError::CurrencyMismatch { iso_code: EUR_ISO },
    );
}

/// A holding nobody paid for cannot be mined.
pub(crate) fn assert_unpaid_unminable(world: &World, target: &Target) {
    refuse_mining(
        world,
        target,
        &GemFactoryError::GemNotSettled,
        &IntexFactoryError::InsufficientSettled,
        &NodFactoryError::NodNotSettled,
    );
}

/// A holding mined once is gone, so mining it again finds nothing.
pub(crate) fn assert_mined_unminable(world: &World, target: &Target) {
    refuse_mining(
        world,
        target,
        &GemFactoryError::GemNotFound,
        &IntexFactoryError::InsufficientSettled,
        &NodFactoryError::NodNotFound,
    );
}

/// The owner mines the holding and reads the refusal its product names.
fn refuse_mining(
    world: &World,
    target: &Target,
    gem: &dyn Display,
    series: &dyn Display,
    nod: &dyn Display,
) {
    let (owner, to) = (target.owner, factory(target));
    match &target.item {
        Item::Gem(id) => assert_refused(world, owner, to, &mine_gem(*id), gem),
        Item::Series { id, units } => {
            assert_refused(world, owner, to, &mine_series(target, *id, *units), series)
        }
        Item::Nod(id) => assert_refused(world, owner, to, &mine_nod(*id), nod),
    }
}

/// Gems and Nods are soulbound: even their owner cannot move them.
pub(crate) fn assert_soulbound(world: &World, target: &Target) {
    let to = third_party(world);
    match &target.item {
        Item::Gem(id) => assert_refused(
            world,
            target.owner,
            addresses::GEM_ADDR,
            &eth::IGem::transferFromCall {
                from: target.owner,
                to,
                gemId: *id,
            },
            outbe_gem::errors::GemError::NonTransferable,
        ),
        Item::Series { .. } => unreachable!("an issued Intex trades freely until it is called"),
        Item::Nod(id) => assert_refused(
            world,
            target.owner,
            addresses::NOD_ADDR,
            &eth::INod::transferFromCall {
                from: target.owner,
                to,
                nodId: *id,
            },
            outbe_nod::errors::NodError::NonTransferable,
        ),
    }
}

fn third_party(world: &World) -> Address {
    eth::address_of(&third_party_key(world)).expect("third-party payer address")
}

fn erc20_gem(id: U256, asset: Address, snapshot: U256) -> eth::IGemFactory::settleGemCall {
    eth::IGemFactory::settleGemCall {
        gemId: id,
        asset,
        snapshotId: snapshot,
    }
}

fn mine_gem(id: U256) -> eth::IGemFactory::minePromisCall {
    eth::IGemFactory::minePromisCall {
        gemId: id,
        nonce: 0,
        mac: B256::ZERO,
        opNonce: 0,
    }
}

fn erc20_series(
    target: &Target,
    id: FixedBytes<14>,
    units: u32,
    asset: Address,
    snapshot: U256,
) -> eth::IIntexFactory::settleIntexCall {
    eth::IIntexFactory::settleIntexCall {
        seriesId: id,
        owner: target.owner,
        units: U256::from(units),
        asset,
        snapshotId: snapshot,
    }
}

fn mine_series(
    target: &Target,
    id: FixedBytes<14>,
    units: u32,
) -> eth::IIntexFactory::minePromisCall {
    eth::IIntexFactory::minePromisCall {
        seriesId: id,
        owner: target.owner,
        units: U256::from(units),
        nonce: 0,
        mac: B256::ZERO,
        opNonce: 0,
    }
}

fn erc20_nod(id: U256, asset: Address, snapshot: U256) -> eth::INodFactory::settleNodCall {
    eth::INodFactory::settleNodCall {
        nodId: id,
        asset,
        snapshotId: snapshot,
    }
}

fn mine_nod(id: U256) -> eth::INodFactory::mineGratisCall {
    eth::INodFactory::mineGratisCall {
        nodId: id,
        nonce: 0,
        mac: B256::ZERO,
        opNonce: 0,
    }
}
