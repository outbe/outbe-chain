//! Paying for a lifecycle holding on either rail, in any currency it accepts.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall;

use super::chain::{finalized_checkpoint, verify_checkpoint};
use super::entity::{Item, Rail, Target, Terms};
use super::markets::{coen_rate, currency};
use crate::features::settlement::{assert_mined_success, fund_and_approve};
use crate::internal::{addresses, eth};
use crate::world::settlement_currency::{self, SettlementCurrency};
use crate::world::World;

/// What settling a holding costs in one asset, and the pricing snapshot an ERC20
/// payment has to name: zero on the reference rail, which needs no conversion.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Quote {
    pub(crate) currency: u16,
    pub(crate) payable: U256,
    pub(crate) snapshot: U256,
}

/// A payment that landed, and the blocks either side of it on the primary.
#[derive(Clone, Debug)]
pub(crate) struct Payment {
    pub(crate) target: Target,
    pub(crate) vault: SettlementCurrency,
    pub(crate) payable: U256,
    pub(crate) before: u64,
    pub(crate) after: u64,
}

pub(crate) fn quote(world: &World, target: &Target, asset: Address) -> Quote {
    let url = world.rpc.url(world.validators.primary_port());
    match &target.item {
        Item::Gem(id) => {
            let quote = eth::read_call(
                &url,
                addresses::GEM_FACTORY_ADDR,
                &eth::IGemFactory::quoteSettlementCall { gemId: *id, asset },
            )
            .unwrap_or_else(|| panic!("gem {id} does not quote a payment in {asset}"));
            Quote {
                currency: quote.settlementCurrency,
                payable: quote.amountMinor,
                snapshot: quote.snapshotId,
            }
        }
        Item::Series { id, units } => {
            let quote = eth::read_call(
                &url,
                addresses::INTEX_FACTORY_ADDR,
                &eth::IIntexFactory::quoteSettlementCall {
                    seriesId: *id,
                    asset,
                    amount: U256::from(*units),
                },
            )
            .unwrap_or_else(|| panic!("series {id} does not quote a payment in {asset}"));
            Quote {
                currency: quote.settlementCurrency,
                payable: quote.amountMinor,
                snapshot: quote.snapshotId,
            }
        }
        Item::Nod(id) => {
            let quote = eth::read_call(
                &url,
                addresses::NOD_FACTORY_ADDR,
                &eth::INodFactory::quoteSettlementCall { nodId: *id, asset },
            )
            .unwrap_or_else(|| panic!("Nod {id} does not quote a payment in {asset}"));
            Quote {
                currency: quote.settlementCurrency,
                payable: quote.amountMinor,
                snapshot: quote.snapshotId,
            }
        }
    }
}

/// The factory a holding is settled through.
pub(crate) fn factory(target: &Target) -> Address {
    match target.item {
        Item::Gem(_) => addresses::GEM_FACTORY_ADDR,
        Item::Series { .. } => addresses::INTEX_FACTORY_ADDR,
        Item::Nod(_) => addresses::NOD_FACTORY_ADDR,
    }
}

/// The third party who pays by ERC20: a validator, never the holding's owner.
pub(crate) fn third_party_key(world: &World) -> String {
    world
        .validators
        .get(1)
        .evm_key()
        .expect("validator-1 key pays as a third party")
}

/// Pay for `target` in `iso` by ERC20 from a third party or by the owner's PayNote, at a
/// quote that must be exactly what `terms` cost in `iso`.
pub(crate) fn pay(world: &World, target: &Target, rail: Rail, iso: u16, terms: Terms) -> Payment {
    assert!(
        target.accepts(iso),
        "{:?} pays in USD or its issuance currency {}, not {iso}",
        target.item,
        target.issuance_currency
    );
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let vault = currency(world, iso);
    let before = eth::block_number(&url).expect("head before the payment");
    let checked_quote = || {
        let quoted = quote(world, target, vault.asset);
        assert_eq!(
            (quoted.currency, quoted.payable),
            (iso, cost(terms, iso)),
            "{:?} quoted a currency or cost its terms do not give in {iso}",
            target.item
        );
        quoted
    };
    let mut quoted = checked_quote();
    let mut requoted = false;
    let outcome = loop {
        let outcome = match rail {
            Rail::Erc20 => {
                let payer_key = third_party_key(world);
                let payer = eth::address_of(&payer_key).expect("third-party payer address");
                fund_and_approve(
                    world,
                    vault.asset,
                    &payer_key,
                    payer,
                    factory(target),
                    quoted.payable,
                );
                settle_erc20(&url, target, &payer_key, vault.asset, quoted.snapshot)
            }
            Rail::PayNote => {
                let proof = crate::features::paynote::deposit_and_prove(
                    world,
                    port,
                    &target.owner_key,
                    target.owner,
                    vault.asset,
                    quoted.payable,
                    note_context(target, quoted.snapshot),
                );
                settle_paynote(&url, target, &target.owner_key, proof)
            }
        };
        if outcome.success {
            break outcome;
        }
        // The pricing snapshot rolls over on the hour: quote again once.
        let fresh = checked_quote();
        assert!(
            !requoted && fresh.snapshot != quoted.snapshot,
            "{rail:?} payment for {:?} reverted: {}",
            target.item,
            outcome.receipt
        );
        quoted = fresh;
        requoted = true;
    };
    assert_mined_success(&outcome, "lifecycle payment");
    assert_paid_event(
        target,
        rail,
        vault.asset,
        iso,
        quoted.payable,
        &outcome.receipt,
    );
    Payment {
        target: target.clone(),
        vault,
        payable: quoted.payable,
        before,
        after: receipt_block(&outcome.receipt),
    }
}

/// The settlement a PayNote proof is bound to: the holding, its units, and the quote's snapshot.
pub(crate) fn note_context(target: &Target, snapshot: U256) -> B256 {
    use crate::features::paynote::{gem_context, intex_context, nod_context};
    match &target.item {
        Item::Gem(id) => gem_context(*id, snapshot),
        Item::Series { id, units } => intex_context(&id.0, U256::from(*units), snapshot),
        Item::Nod(id) => nod_context(*id, snapshot),
    }
}

/// Entry price times load, at least one reference minor unit, converted through the
/// controlled COEN quotes and floored once to six decimals.
fn cost(terms: Terms, iso: u16) -> U256 {
    let scale = U256::from(1_000_000);
    (terms.entry_price * terms.load).max(scale) * coen_rate(iso)
        / (coen_rate(settlement_currency::USD_ISO) * scale)
}

fn settle_erc20(
    url: &str,
    target: &Target,
    payer_key: &str,
    asset: Address,
    snapshot: U256,
) -> eth::MinedCallOutcome {
    match &target.item {
        Item::Gem(id) => send(
            url,
            payer_key,
            addresses::GEM_FACTORY_ADDR,
            &eth::IGemFactory::settleGemCall {
                gemId: *id,
                asset,
                snapshotId: snapshot,
            },
        ),
        Item::Series { id, units } => send(
            url,
            payer_key,
            addresses::INTEX_FACTORY_ADDR,
            &eth::IIntexFactory::settleIntexCall {
                seriesId: *id,
                intexOwner: target.owner,
                amount: U256::from(*units),
                asset,
                snapshotId: snapshot,
            },
        ),
        Item::Nod(id) => send(
            url,
            payer_key,
            addresses::NOD_FACTORY_ADDR,
            &eth::INodFactory::settleNodCall {
                nodId: *id,
                asset,
                snapshotId: snapshot,
            },
        ),
    }
}

fn settle_paynote(
    url: &str,
    target: &Target,
    payer_key: &str,
    proof: Vec<u8>,
) -> eth::MinedCallOutcome {
    match &target.item {
        Item::Gem(id) => send(
            url,
            payer_key,
            addresses::GEM_FACTORY_ADDR,
            &eth::IGemFactory::settleGemWithPayNoteCall {
                gemId: *id,
                payNoteProof: proof.into(),
            },
        ),
        Item::Series { id, units } => send(
            url,
            payer_key,
            addresses::INTEX_FACTORY_ADDR,
            &eth::IIntexFactory::settleIntexWithPayNoteCall {
                seriesId: *id,
                intexOwner: target.owner,
                amount: U256::from(*units),
                payNoteProof: proof.into(),
            },
        ),
        Item::Nod(id) => send(
            url,
            payer_key,
            addresses::NOD_FACTORY_ADDR,
            &eth::INodFactory::settleNodWithPayNoteCall {
                nodId: *id,
                payNoteProof: proof.into(),
            },
        ),
    }
}

fn send<C: SolCall>(url: &str, key: &str, to: Address, call: &C) -> eth::MinedCallOutcome {
    eth::send_call_outcome(url, to, key, call, None)
        .unwrap_or_else(|error| panic!("submit {}: {error:#}", C::SIGNATURE))
}

/// The factory's own record of the payment names the holding, what it cost and how.
fn assert_paid_event(
    target: &Target,
    rail: Rail,
    asset: Address,
    currency: u16,
    payable: U256,
    receipt: &serde_json::Value,
) {
    match &target.item {
        Item::Gem(id) => {
            let settled = eth::receipt_event::<eth::IGemFactory::GemSettled>(
                receipt,
                addresses::GEM_FACTORY_ADDR,
            );
            assert_eq!(
                (
                    settled.gemId,
                    settled.owner,
                    settled.amountMinor,
                    settled.settlementCurrency
                ),
                (*id, target.owner, payable, currency),
                "GemSettled does not record this payment"
            );
        }
        // The series' record names the units; their cost shows only in the vault.
        Item::Series { id, units } => {
            let settled = eth::receipt_event::<eth::IIntexFactory::Settled>(
                receipt,
                addresses::INTEX_FACTORY_ADDR,
            );
            assert_eq!(
                (settled.seriesId, settled.intexOwner, settled.amount),
                (*id, target.owner, U256::from(*units)),
                "Settled does not record this payment"
            );
        }
        Item::Nod(id) => {
            let paid = eth::receipt_event::<eth::INodFactory::NodPaid>(
                receipt,
                addresses::NOD_FACTORY_ADDR,
            );
            assert_eq!(
                (paid.owner, paid.nodId, paid.asset, paid.amountMinor),
                (target.owner, *id, asset, payable),
                "NodPaid does not record this payment"
            );
            assert_eq!(
                paid.nullifier == B256::ZERO,
                rail == Rail::Erc20,
                "only a PayNote payment spends a nullifier"
            );
        }
    }
}

/// Every payment credited exactly its quote to its currency's vault, on every
/// validator, and nothing else touched that vault in between.
pub(crate) fn assert_payments_settled(world: &World, payments: &[Payment]) {
    let checkpoint = finalized_checkpoint(world);
    for payment in payments {
        assert!(
            checkpoint.height >= payment.after,
            "the payment is not finalized yet"
        );
        for port in world.validators.committee_ports() {
            let url = world.rpc.url(port);
            let at = |height| {
                settlement_currency::vault_balance_at(
                    &url,
                    payment.vault.asset,
                    payment.vault.vault,
                    height,
                )
                .expect("finalized vault balance")
            };
            assert_eq!(
                at(payment.after) - at(payment.before),
                payment.payable,
                "{:?} did not credit its quote to the vault on port {port}",
                payment.target.item
            );
        }
    }
    verify_checkpoint(world, checkpoint);
}

/// The transaction is mined and reverts with gas to spare: a guard refused it.
pub(crate) fn assert_mined_refusal<C: SolCall>(world: &World, key: &str, to: Address, call: &C) {
    let url = world.rpc.url(world.validators.primary_port());
    let outcome = send(&url, key, to, call);
    assert!(
        !outcome.success,
        "{} was not refused: {}",
        C::SIGNATURE,
        outcome.transaction_hash
    );
    let gas_used = outcome.receipt["gasUsed"]
        .as_str()
        .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
        .expect("receipt gas used");
    assert!(
        gas_used < eth::REVERT_FRIENDLY_GAS_LIMIT,
        "{} ran out of gas instead of reverting",
        C::SIGNATURE
    );
}

/// The call reverts with exactly `expected`, the product error's own text.
pub(crate) fn assert_refused<C: SolCall>(
    world: &World,
    from: Address,
    to: Address,
    call: &C,
    expected: impl std::fmt::Display,
) {
    let url = world.rpc.url(world.validators.primary_port());
    let height = eth::block_number(&url).expect("head for the refused call");
    let reason = eth::read_call_revert_reason_at(&url, to, from, call, height)
        .unwrap_or_else(|error| panic!("{} did not revert: {error:#}", C::SIGNATURE));
    assert_eq!(
        reason,
        expected.to_string(),
        "{} refused for the wrong reason",
        C::SIGNATURE
    );
}

pub(crate) fn receipt_block(receipt: &serde_json::Value) -> u64 {
    receipt["blockNumber"]
        .as_str()
        .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
        .expect("receipt block number")
}
