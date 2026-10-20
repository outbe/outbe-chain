//! Gem settlement on the ERC-20 rail through real factory, router, token and vault frames.
#[path = "../../../../testing/support/factory_erc20_settlement.rs"]
mod settlement;

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_gem::{precompile::IGem, GemAddParams};
use outbe_gemfactory::precompile::IGemFactory;
use outbe_primitives::{
    addresses::{GEM_ADDRESS, GEM_FACTORY_ADDRESS},
    storage::{StorageHandle, SubCallStatus},
};
use outbe_vaultrouter::api::IVaultRouter::StablesSource;
use settlement::{Quote, Rail, World, ASSET, OWNER, TIMESTAMP};

/// `called_at` stamps the Gem's group as called at that time.
#[derive(Clone, Copy, Debug)]
struct Gem {
    called_at: Option<u64>,
}

const GEM: Gem = Gem { called_at: None };

impl Rail for Gem {
    const FACTORY: Address = GEM_FACTORY_ADDRESS;
    const SOURCE: StablesSource = StablesSource::GemCostAmount;

    fn seed(self, storage: &StorageHandle<'_>, issuance_currency: u16) -> (U256, u64) {
        let profile = outbe_gem::config::read(storage).unwrap();
        let entry_price = U256::from(2_000_000);
        let gem_id = outbe_gem::api::add_gem(
            storage,
            GemAddParams {
                owner: OWNER,
                promis_load_minor: U256::from(1_500_000),
                entry_price_minor: entry_price,
                issuance_currency,
                reference_currency: 840,
                issued_at: TIMESTAMP,
                call_terms: Some(outbe_gem::CallTerms {
                    call_price_minor: outbe_primitives::markup::marked_up(
                        entry_price,
                        profile.call_rate,
                    )
                    .unwrap(),
                    reference_currency: 840,
                    call_rate: profile.call_rate,
                    call_window_seconds: profile.call_window_seconds,
                    call_threshold_seconds: profile.call_threshold_seconds,
                    call_notice_period_seconds: profile.call_notice_period_seconds,
                }),
            },
        )
        .unwrap();
        let Some(called_at) = self.called_at else {
            return (gem_id, 0);
        };
        let group = outbe_gem::api::get_item(storage, gem_id)
            .unwrap()
            .unwrap()
            .group_key;
        outbe_gem::GemContract::new(storage.clone())
            .group_called_at
            .write(&group, called_at)
            .unwrap();
        let gem = outbe_gem::api::get_view(storage, gem_id).unwrap().unwrap();
        assert_eq!(
            gem.effective_state(TIMESTAMP),
            outbe_gem::GemState::Called as u8
        );
        (gem_id, gem.settlement_deadline())
    }

    fn quote(self, world: &mut World<Self>) -> Quote {
        let quote = world.view(
            GEM_FACTORY_ADDRESS,
            IGemFactory::quoteSettlementCall {
                gemId: world.id,
                asset: ASSET,
            },
        );
        Quote {
            currency: quote.settlementCurrency,
            cost: quote.paymentMinor,
            snapshot_id: quote.snapshotId,
        }
    }

    fn settle_call(self, id: U256, snapshot_id: U256) -> Bytes {
        IGemFactory::settleGemCall {
            gemId: id,
            asset: ASSET,
            snapshotId: snapshot_id,
        }
        .abi_encode()
        .into()
    }

    fn assert_settled(self, world: &World<Self>) {
        let settled: Vec<_> = world
            .evm
            .logs()
            .iter()
            .filter_map(|log| IGemFactory::GemSettled::decode_log_data(&log.data).ok())
            .collect();
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].owner, OWNER);
        assert_eq!(settled[0].asset, ASSET);
        assert_eq!(settled[0].paymentMinor, world.cost);
    }

    fn assert_kept_by_owner(self, world: &mut World<Self>) {
        let gem = world.view(GEM_ADDRESS, IGem::getGemCall { gemId: world.id });
        assert_eq!(gem.owner, OWNER);
        assert_eq!(gem.state, outbe_gem::GemState::Settled as u8);
    }
}

fn gem_state(world: &mut World<Gem>) -> u8 {
    world
        .view(GEM_ADDRESS, IGem::getGemCall { gemId: world.id })
        .state
}

#[test]
fn an_issuance_currency_payment_moves_the_converted_cost_at_the_quoted_snapshot() {
    settlement::issuance_currency_payment_moves_the_converted_cost(GEM);
}

#[test]
fn erc20_settlement_moves_exactly_the_quoted_cost_into_the_reserve() {
    settlement::quoted_cost_moves_into_the_reserve_once(GEM);
}

#[test]
fn a_third_party_pays_and_the_units_stay_with_the_owner() {
    settlement::third_party_pays_and_the_entity_stays_with_the_owner(GEM);
}

#[test]
fn payment_failures_restore_balances_allowances_and_logs() {
    settlement::payment_failures_restore_balances_allowances_and_logs(GEM);
}

#[test]
fn optional_empty_token_returns_are_accepted() {
    settlement::optional_empty_token_returns_are_accepted(GEM);
}

#[test]
fn the_payer_cannot_reenter_settlement_during_transfer() {
    settlement::the_payer_cannot_reenter_settlement_during_transfer(GEM);
}

/// A called Gem settles up to and including its deadline and is refused a second later,
/// when its status already reads Forfeited.
#[test]
fn a_called_gem_settles_at_its_deadline_and_is_refused_a_second_later() {
    let called = Gem {
        called_at: Some(TIMESTAMP),
    };
    let (mut world, deadline) = World::build(called, OWNER, true, 840);
    world.evm.ctx.block.timestamp = U256::from(deadline);
    assert_eq!(gem_state(&mut world), outbe_gem::GemState::Called as u8);
    let out = world.settle();
    world.succeeds(out);
    assert_eq!(world.balances(), world.paid_balances());
    called.assert_settled(&world);
    assert_eq!(gem_state(&mut world), outbe_gem::GemState::Settled as u8);

    let (mut world, deadline) = World::build(called, OWNER, true, 840);
    world.evm.ctx.block.timestamp = U256::from(deadline + 1);
    let balances = world.balances();
    let allowances = world.allowances();
    let logs = world.evm.logs().to_vec();
    let out = world.settle();
    assert!(
        matches!(out.status, SubCallStatus::Revert(_)),
        "{:?}",
        out.status
    );
    assert_eq!(world.balances(), balances);
    assert_eq!(world.allowances(), allowances);
    assert_eq!(world.evm.logs(), logs);
    let gem = world.view(GEM_ADDRESS, IGem::getGemCall { gemId: world.id });
    assert_eq!(
        gem.state,
        outbe_gem::GemState::Forfeited as u8,
        "the public status projects Forfeited before any cleanup"
    );
    assert_eq!(gem.owner, OWNER);
}
