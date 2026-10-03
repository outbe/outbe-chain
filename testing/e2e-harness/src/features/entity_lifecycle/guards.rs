//! The refusals that guard a lifecycle, each read as the exact revert text of the
//! product's own error: paying early or on wrong terms, mining unpaid or twice, moving.

use alloy_primitives::{Address, FixedBytes, B256, U256};
use outbe_gemfactory::errors::GemFactoryError;
use outbe_intexfactory::errors::IntexFactoryError;
use outbe_nodfactory::errors::NodFactoryError;

use super::entity::{Item, Target};
use super::markets::{currency, EUR_ISO, MYR_ISO};
use super::payment::{
    assert_mined_refusal, assert_refused, factory, note_context, quote, third_party_key,
};
use crate::internal::{addresses, eth};
use crate::world::settlement_currency::USD_ISO;
use crate::world::World;

/// `IntexState::Issued`, the stored state an unqualified series refuses payment in.
const ISSUED_SERIES: u8 = 0;

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
        Item::Series { id, units } => assert_refused(
            world,
            payer,
            factory(target),
            &erc20_series(target, *id, *units, asset, U256::ZERO),
            IntexFactoryError::NotSettleable(ISSUED_SERIES),
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

/// A qualified holding refuses a stale pricing snapshot, an asset in a third currency,
/// and a PayNote for its exact cost bound to `other`.
pub(crate) fn assert_payment_guards(world: &World, target: &Target, other: &Target) {
    let payer = third_party(world);
    let myr = currency(world, MYR_ISO).asset;
    let eur = currency(world, EUR_ISO).asset;
    let required = quote(world, target, myr).snapshot;
    assert!(
        !required.is_zero(),
        "an issuance-currency quote names its pricing snapshot"
    );
    let expected = note_context(target, required);
    let actual = note_context(other, required);
    let note = foreign_note(world, target, actual);
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
                GemFactoryError::PayNoteContextMismatch { expected, actual },
            );
        }
        Item::Series { id, units } => {
            assert_refused(
                world,
                payer,
                factory(target),
                &erc20_series(target, *id, *units, myr, U256::ZERO),
                IntexFactoryError::VwapSnapshotMismatch {
                    authorized: U256::ZERO,
                    required,
                },
            );
            assert_refused(
                world,
                payer,
                factory(target),
                &erc20_series(target, *id, *units, eur, U256::ZERO),
                IntexFactoryError::SettlementCurrencyMismatch(EUR_ISO),
            );
            assert_refused(
                world,
                target.owner,
                factory(target),
                &eth::IIntexFactory::settleIntexWithPayNoteCall {
                    seriesId: *id,
                    owner: target.owner,
                    units: U256::from(*units),
                    payNoteProof: note.into(),
                },
                IntexFactoryError::PayNoteContextMismatch { expected, actual },
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
            // Nod settlement writes its compressed body first, which only a block executes,
            // so the refusal is a mined revert; a note bound to this Nod then pays it.
            assert_mined_refusal(
                world,
                &target.owner_key,
                factory(target),
                &eth::INodFactory::settleNodWithPayNoteCall {
                    nodId: *id,
                    payNoteProof: note.into(),
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
        Item::Series { id, units } => assert_refused(
            world,
            target.owner,
            factory(target),
            &mine_series(target, *id, *units),
            IntexFactoryError::InsufficientSettled,
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
        Item::Series { id, units } => assert_refused(
            world,
            target.owner,
            factory(target),
            &mine_series(target, *id, *units),
            IntexFactoryError::InsufficientSettled,
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

/// A real, unspent note for the holding's exact MYR cost, bound to `context`.
fn foreign_note(world: &World, target: &Target, context: B256) -> Vec<u8> {
    let myr = currency(world, MYR_ISO).asset;
    let key = third_party_key(world);
    crate::features::paynote::deposit_and_prove(
        world,
        world.validators.primary_port(),
        &key,
        third_party(world),
        myr,
        quote(world, target, myr).payable,
        context,
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
