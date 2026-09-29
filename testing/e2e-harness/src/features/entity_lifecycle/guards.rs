//! The refusals that guard a lifecycle: paying too early or in the wrong terms, and
//! mining what was not paid or is already gone. Each is read as the exact revert the
//! product's own error type renders.

use alloy_primitives::{Address, B256, U256};
use outbe_gemfactory::errors::GemFactoryError;
use outbe_nodfactory::errors::NodFactoryError;

use super::entity::{Item, Target};
use super::markets::{currency, EUR_ISO, MYR_ISO};
use super::payment::{assert_refused, factory, quote, third_party_key};
use crate::internal::eth;
use crate::world::settlement_currency::USD_ISO;
use crate::world::World;

/// An ERC20 payment in USD, the reference rail, before the holding qualified.
pub(crate) fn assert_unqualified_refused(world: &World, target: &Target) {
    let payer = third_party(world);
    let asset = currency(world, USD_ISO).asset;
    match &target.item {
        Item::Gem(id) => assert_refused(
            world,
            payer,
            factory(target),
            &erc20_gem(*id, asset, U256::ZERO),
            GemFactoryError::InvalidState,
        ),
        Item::Nod(id) => assert_refused(
            world,
            payer,
            factory(target),
            &erc20_nod(*id, asset, U256::ZERO),
            NodFactoryError::NodNotQualified,
        ),
    }
}

/// A qualified holding still refuses an ERC20 payment naming a stale pricing
/// snapshot, an asset in a currency it was never issued or referenced in, and a
/// PayNote whose proof names somebody other than the payer it must name.
pub(crate) fn assert_payment_guards(world: &World, target: &Target) {
    let payer = third_party(world);
    let myr = currency(world, MYR_ISO).asset;
    let eur = currency(world, EUR_ISO).asset;
    let required = quote(world, target, myr).snapshot;
    assert!(
        !required.is_zero(),
        "an issuance-currency quote names its pricing snapshot"
    );
    let note = foreign_note(world, target);
    match &target.item {
        Item::Gem(id) => {
            assert_refused(
                world,
                payer,
                factory(target),
                &erc20_gem(*id, myr, U256::ZERO),
                GemFactoryError::VwapSnapshotMismatch {
                    authorized: U256::ZERO,
                    required,
                },
            );
            assert_refused(
                world,
                payer,
                factory(target),
                &erc20_gem(*id, eur, U256::ZERO),
                GemFactoryError::SettlementCurrencyMismatch { iso_code: EUR_ISO },
            );
            assert_refused(
                world,
                target.owner,
                factory(target),
                &eth::IGemFactory::settleGemWithPayNoteCall {
                    gemId: *id,
                    payNoteProof: note.into(),
                },
                GemFactoryError::PayNoteOwnerMismatch {
                    expected: target.owner,
                    actual: payer,
                },
            );
        }
        Item::Nod(id) => {
            assert_refused(
                world,
                payer,
                factory(target),
                &erc20_nod(*id, myr, U256::ZERO),
                NodFactoryError::VwapSnapshotMismatch {
                    authorized: U256::ZERO,
                    required,
                },
            );
            assert_refused(
                world,
                payer,
                factory(target),
                &erc20_nod(*id, eur, U256::ZERO),
                NodFactoryError::SettlementCurrencyMismatch { iso_code: EUR_ISO },
            );
            assert_refused(
                world,
                target.owner,
                factory(target),
                &eth::INodFactory::settleNodWithPayNoteCall {
                    nodId: *id,
                    payNoteProof: note.into(),
                },
                NodFactoryError::PayNoteOwnerMismatch {
                    expected: target.owner,
                    actual: payer,
                },
            );
        }
    }
}

/// A holding nobody paid for cannot be mined.
pub(crate) fn assert_unpaid_unminable(world: &World, target: &Target) {
    match &target.item {
        Item::Gem(id) => assert_refused(
            world,
            target.owner,
            factory(target),
            &mine_gem(*id),
            GemFactoryError::InvalidState,
        ),
        Item::Nod(id) => assert_refused(
            world,
            target.owner,
            factory(target),
            &mine_nod(*id),
            NodFactoryError::NodNotSettled,
        ),
    }
}

/// A holding mined once is gone, so mining it again finds nothing.
pub(crate) fn assert_mined_unminable(world: &World, target: &Target) {
    match &target.item {
        Item::Gem(id) => assert_refused(
            world,
            target.owner,
            factory(target),
            &mine_gem(*id),
            GemFactoryError::GemNotFound,
        ),
        Item::Nod(id) => assert_refused(
            world,
            target.owner,
            factory(target),
            &mine_nod(*id),
            NodFactoryError::NodNotFound,
        ),
    }
}

fn third_party(world: &World) -> Address {
    eth::address_of(&third_party_key(world)).expect("third-party payer address")
}

/// A real, unspent note for the holding's USD cost, deposited and proven by the
/// third party rather than by whoever the settlement binds the proof to.
fn foreign_note(world: &World, target: &Target) -> Vec<u8> {
    let usd = currency(world, USD_ISO).asset;
    let key = third_party_key(world);
    crate::features::paynote::deposit_and_prove(
        world,
        world.validators.primary_port(),
        &key,
        third_party(world),
        usd,
        quote(world, target, usd).payable,
    )
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
