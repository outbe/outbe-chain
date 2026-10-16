//! Nod settlement on the ERC-20 rail through real factory, router, token and vault frames.
#[path = "../../../../testing/support/factory_erc20_settlement.rs"]
mod settlement;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_gratis::enclave_client::test_enclave;
use outbe_nod::{precompile::INod, NodContract, NodIssueParams};
use outbe_nodfactory::precompile::INodFactory;
use outbe_oracle::test_support::record_finalized_day_vwap;
use outbe_primitives::wwd_entity_id::WwdEntityId;
use outbe_primitives::{
    addresses::{NOD_ADDRESS, NOD_FACTORY_ADDRESS},
    chain::CHAIN_ID,
    storage::{StorageHandle, SubCallStatus},
    time::{first_full_day, WorldwideDay},
};
use outbe_tee::protocol::GratisOp;
use outbe_tee_enclave::gratis::{derive_modify_key, modify_mac};
use outbe_vaultrouter::api::IVaultRouter::StablesSource;
use settlement::{Quote, Rail, World, ASSET, FACTORY_FLOAT, OWNER, THIRD_PARTY};

const GRATIS_LOAD: u64 = 1_000;

#[derive(Clone, Copy, Debug)]
struct Nod {
    cost: U256,
}

const NOD: Nod = Nod {
    cost: U256::from_limbs([500, 0, 0, 0]),
};

impl Rail for Nod {
    const FACTORY: Address = NOD_FACTORY_ADDRESS;
    const SOURCE: StablesSource = StablesSource::NodCostAmount;

    fn seed(self, storage: &StorageHandle<'_>, issuance_currency: u16) -> (U256, u64) {
        test_enclave::install();
        let params = NodIssueParams {
            owner: OWNER,
            gratis_load_minor: U256::from(GRATIS_LOAD),
            worldwide_day: WorldwideDay::new(20_241_220),
            league_id: 1,
            entry_price_minor: self.cost * U256::from(1_000),
            issuance_currency,
            reference_currency: 840,
        };
        let nod = outbe_nodfactory::api::issue_nod(
            storage,
            &outbe_nod::test_support::encrypted_fixture(&params, CHAIN_ID),
        )
        .unwrap();
        // A finalized day above the floor qualifies the Nod's bucket.
        let floor = outbe_nod::pricing::floor_price_minor(params.entry_price_minor).unwrap();
        let bucket =
            outbe_nod::identity::bucket_key(params.worldwide_day, params.entry_price_minor, 840);
        let issued_at = NodContract::new(storage.clone())
            .bucket_issued_at
            .read(&bucket)
            .unwrap();
        record_finalized_day_vwap(storage, 840, first_full_day(issued_at), floor + U256::ONE)
            .unwrap();
        (nod.to_u256(), 0)
    }

    fn quote(self, world: &mut World<Self>) -> Quote {
        let quote = world.view(
            NOD_FACTORY_ADDRESS,
            INodFactory::quoteSettlementCall {
                nodId: world.id,
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
        INodFactory::settleNodCall {
            nodId: id,
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
            .filter_map(|log| INodFactory::NodSettled::decode_log_data(&log.data).ok())
            .collect();
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].owner, OWNER);
        assert_eq!(settled[0].asset, ASSET);
        assert_eq!(settled[0].paymentMinor, world.cost);
    }

    fn assert_kept_by_owner(self, world: &mut World<Self>) {
        let nod = world.view(NOD_ADDRESS, INod::getNodCall { nodId: world.id });
        assert!(nod.isSettled);
        assert_eq!(nod.owner, OWNER);
    }
}

fn assert_unsettled(world: &mut World<Nod>) {
    let nod = world.view(NOD_ADDRESS, INod::getNodCall { nodId: world.id });
    assert!(!nod.isSettled);
}

#[test]
fn an_issuance_currency_payment_moves_the_converted_cost_at_the_quoted_snapshot() {
    settlement::issuance_currency_payment_moves_the_converted_cost(NOD);
}

#[test]
fn erc20_settlement_moves_exactly_the_quoted_cost_into_the_reserve() {
    settlement::quoted_cost_moves_into_the_reserve_once(NOD);
}

#[test]
fn a_third_party_pays_and_the_nod_stays_with_the_owner() {
    settlement::third_party_pays_and_the_entity_stays_with_the_owner(NOD);
}

#[test]
fn payment_failures_restore_balances_allowances_and_logs() {
    settlement::payment_failures_restore_balances_allowances_and_logs(NOD);
}

#[test]
fn optional_empty_token_returns_are_accepted() {
    settlement::optional_empty_token_returns_are_accepted(NOD);
}

#[test]
fn the_payer_cannot_reenter_settlement_during_transfer() {
    settlement::the_payer_cannot_reenter_settlement_during_transfer(NOD);
}

#[test]
fn erc20_settlement_moves_exact_full_width_cost_and_preserves_mining() {
    // Above u128, yet inside the price ladder the call index bins by.
    let rail = Nod {
        cost: (U256::ONE << 129) + U256::from(500),
    };
    let mut world = World::new(rail, OWNER, true);
    assert_eq!(world.cost, rail.cost);
    let out = world.settle();
    world.succeeds(out);
    assert_eq!(world.balances(), world.paid_balances());
    rail.assert_settled(&world);
    let balances = world.balances();
    assert!(!matches!(world.settle().status, SubCallStatus::Success));
    assert_eq!(world.balances(), balances);

    let nod = WwdEntityId::from(world.id);
    let nonce = (0..1_000_000)
        .find(|n| outbe_nodfactory::runtime::validate_pow(nod, OWNER, *n).is_ok())
        .unwrap();
    let key = derive_modify_key(&test_enclave::state_key(), OWNER).unwrap();
    let mac = modify_mac(
        &key,
        &outbe_tee_enclave::gratis::ModifyOperation {
            account: OWNER,
            op: GratisOp::Mint,
            amount: U256::from(GRATIS_LOAD),
            op_nonce: 0,
            chain_id: B256::from(U256::from(CHAIN_ID)),
        },
    );
    let minted = world.ok(
        OWNER,
        NOD_FACTORY_ADDRESS,
        INodFactory::mineGratisCall {
            nodId: world.id,
            nonce,
            mac: B256::from(mac),
            opNonce: 0,
        },
    );
    let view_key =
        outbe_tee_enclave::gratis::derive_view_key(&test_enclave::state_key(), OWNER).unwrap();
    assert_eq!(
        outbe_tee::gratis_decrypt::decrypt_gratis_balance(&view_key, OWNER, &minted).unwrap(),
        U256::from(GRATIS_LOAD)
    );
}

#[test]
fn insufficient_allowance_and_balance_revert_without_payment() {
    for (allowance, payer) in [(U256::ZERO, OWNER), (U256::MAX, THIRD_PARTY)] {
        let mut world = World::new(NOD, payer, true);
        let funded = world.cost * U256::from(2);
        if payer != OWNER {
            // Empty the payer through an authorized transfer before attempting settlement.
            world.ok(
                payer,
                ASSET,
                settlement::IFixture::approveCall {
                    spender: OWNER,
                    amount: funded,
                },
            );
            world.ok(
                OWNER,
                ASSET,
                settlement::IFixture::transferFromCall {
                    from: payer,
                    to: OWNER,
                    amount: funded,
                },
            );
        }
        world.ok(
            payer,
            ASSET,
            settlement::IFixture::approveCall {
                spender: NOD_FACTORY_ADDRESS,
                amount: allowance,
            },
        );
        let balances = world.balances();
        assert!(!matches!(world.settle().status, SubCallStatus::Success));
        assert_eq!(world.balances(), balances);
        assert_eq!(balances[1], U256::from(FACTORY_FLOAT));
        assert_unsettled(&mut world);
    }
}
