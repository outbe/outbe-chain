//! Paying for a lifecycle holding by ERC20, from its owner or a third party, in any
//! currency it accepts.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;

use super::chain::{finalized_checkpoint, settlement_read, verify_checkpoint};
use super::entity::{Item, Payer, Target, Terms};
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
                payable: quote.paymentMinor,
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
                    units: U256::from(*units),
                },
            )
            .unwrap_or_else(|| panic!("series {id} does not quote a payment in {asset}"));
            Quote {
                currency: quote.settlementCurrency,
                payable: quote.paymentMinor,
                snapshot: quote.snapshotId,
            }
        }
        Item::Nod(id) => {
            let quote = settlement_read(|| {
                eth::read_call_result(
                    &url,
                    addresses::NOD_FACTORY_ADDR,
                    &eth::INodFactory::quoteSettlementCall { nodId: *id, asset },
                )
            })
            .unwrap_or_else(|error| {
                panic!("Nod {id} does not quote a payment in {asset}: {error}")
            });
            Quote {
                currency: quote.settlementCurrency,
                payable: quote.paymentMinor,
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

/// Pay for `target` in `iso` by ERC20 from `payer`, at a quote that must be exactly what
/// `terms` cost in `iso`.
pub(crate) fn pay(world: &World, target: &Target, payer: Payer, iso: u16, terms: Terms) -> Payment {
    assert!(
        target.accepts(iso),
        "{:?} pays in USD or its issuance currency {}, not {iso}",
        target.item,
        target.issuance_currency
    );
    let url = world.rpc.url(world.validators.primary_port());
    let vault = currency(world, iso);
    let payer_key = match payer {
        Payer::Owner => target.owner_key.clone(),
        Payer::ThirdParty => third_party_key(world),
    };
    let payer_address = eth::address_of(&payer_key).expect("payer address");
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
        fund_and_approve(
            world,
            crate::features::settlement::SettlementFunding {
                asset: vault.asset,
                owner_key: &payer_key,
                owner: payer_address,
                spender: factory(target),
                amount: quoted.payable,
            },
        );
        let outcome = settle_erc20(&url, target, &payer_key, vault.asset, quoted.snapshot);
        if outcome.success {
            break outcome;
        }
        // The pricing snapshot rolls over on the hour: quote again once.
        let fresh = checked_quote();
        assert!(
            !requoted && fresh.snapshot != quoted.snapshot,
            "{payer:?} payment for {:?} reverted: {}",
            target.item,
            outcome.receipt
        );
        quoted = fresh;
        requoted = true;
    };
    assert_mined_success(&outcome, "lifecycle payment");
    assert_paid_event(target, vault.asset, iso, quoted.payable, &outcome.receipt);
    Payment {
        target: target.clone(),
        vault,
        payable: quoted.payable,
        before,
        after: receipt_block(&outcome.receipt),
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
                owner: target.owner,
                units: U256::from(*units),
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

fn send<C: SolCall>(url: &str, key: &str, to: Address, call: &C) -> eth::MinedCallOutcome {
    eth::send_call_outcome(url, to, key, call, None)
        .unwrap_or_else(|error| panic!("submit {}: {error:#}", C::SIGNATURE))
}

/// The factory's own record of the payment names the holding, what it cost and how.
fn assert_paid_event(
    target: &Target,
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
                    settled.asset,
                    settled.paymentMinor,
                    settled.settlementCurrency
                ),
                (*id, target.owner, asset, payable, currency),
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
                (settled.seriesId, settled.owner, settled.units),
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
                (paid.owner, paid.nodId, paid.asset, paid.paymentMinor),
                (target.owner, *id, asset, payable),
                "NodPaid does not record this payment"
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
