//! Intex settlement on the ERC-20 rail through real factory, router, token, vault and NFT frames.
#[path = "../../../../testing/support/factory_erc20_settlement.rs"]
mod settlement;

use alloy_primitives::{Address, Bytes, FixedBytes, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};
use outbe_intex::precompile::IIntex;
use outbe_intex::{CreateSeriesParams, IntexCallTrigger, SeriesId};
use outbe_intexfactory::precompile::IIntexFactory;
use outbe_primitives::{
    addresses::{INTEX_ADDRESS, INTEX_FACTORY_ADDRESS, INTEX_NFT1155_ADDRESS},
    storage::StorageHandle,
    time::WorldwideDay,
};
use outbe_vaultrouter::api::IVaultRouter::StablesSource;
use settlement::{Quote, Rail, World, ASSET, OWNER, TIMESTAMP};

sol! {
    interface INftFixture {
        function mint1155(address account, uint256 id, uint256 amount) external;
        function balanceOf(address account, uint256 id) external view returns (uint256);
    }
}

const SERIES: [u8; 14] = *b"20241220-USD-U";
const UNITS: u64 = 2;

#[derive(Clone, Copy, Debug)]
struct Intex;

impl Rail for Intex {
    const FACTORY: Address = INTEX_FACTORY_ADDRESS;
    const SOURCE: StablesSource = StablesSource::IntexCostAmount;
    const EXTRA_COUNTERPARTIES: &'static [Address] = &[INTEX_NFT1155_ADDRESS];

    fn seed(self, storage: &StorageHandle<'_>, issuance_currency: u16) -> (U256, u64) {
        outbe_intex::api::create_series(
            storage,
            CreateSeriesParams {
                series_id: SeriesId::from(FixedBytes(SERIES)),
                worldwide_day: WorldwideDay::new(20_241_220),
                issued_units: 10,
                promis_load_minor: 1_500_000,
                entry_price_minor: U256::from(2_000_000),
                call_price_minor: U256::from(4_560_000),
                call_trigger: IntexCallTrigger {
                    call_window_seconds: 1,
                    call_threshold_seconds: 1,
                    call_notice_period_seconds: 1,
                },
                issued_at: TIMESTAMP as u32,
                issuance_currency,
                reference_currency: 840,
            },
        )
        .unwrap();
        (U256::ZERO, 0)
    }

    fn prepare(self, world: &mut World<Self>) {
        world.ok(
            OWNER,
            INTEX_NFT1155_ADDRESS,
            INftFixture::mint1155Call {
                account: OWNER,
                id: U256::from_be_slice(&SERIES),
                amount: U256::from(UNITS),
            },
        );
    }

    fn quote(self, world: &mut World<Self>) -> Quote {
        let quote = world.view(
            INTEX_FACTORY_ADDRESS,
            IIntexFactory::quoteSettlementCall {
                seriesId: FixedBytes(SERIES),
                asset: ASSET,
                units: U256::from(UNITS),
            },
        );
        Quote {
            currency: quote.settlementCurrency,
            cost: quote.paymentMinor,
            snapshot_id: quote.snapshotId,
        }
    }

    fn settle_call(self, _id: U256, snapshot_id: U256) -> Bytes {
        IIntexFactory::settleIntexCall {
            seriesId: FixedBytes(SERIES),
            owner: OWNER,
            units: U256::from(UNITS),
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
            .filter_map(|log| IIntexFactory::IntexSettled::decode_log_data(&log.data).ok())
            .collect();
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].owner, OWNER);
        assert_eq!(settled[0].units, U256::from(UNITS));
    }

    fn assert_kept_by_owner(self, world: &mut World<Self>) {
        let settled = world.view(
            INTEX_ADDRESS,
            IIntex::balanceOfCall {
                account: OWNER,
                id: U256::from_be_slice(&SERIES),
            },
        );
        assert_eq!(settled, U256::from(UNITS));
    }
}

#[test]
fn an_issuance_currency_payment_moves_the_converted_cost_at_the_quoted_snapshot() {
    settlement::issuance_currency_payment_moves_the_converted_cost(Intex);
}

#[test]
fn erc20_settlement_moves_exactly_the_quoted_cost_into_the_reserve() {
    settlement::quoted_cost_moves_into_the_reserve_once(Intex);
}

#[test]
fn a_third_party_pays_and_the_units_stay_with_the_owner() {
    settlement::third_party_pays_and_the_entity_stays_with_the_owner(Intex);
}

#[test]
fn payment_failures_restore_balances_allowances_and_logs() {
    settlement::payment_failures_restore_balances_allowances_and_logs(Intex);
}

#[test]
fn optional_empty_token_returns_are_accepted() {
    settlement::optional_empty_token_returns_are_accepted(Intex);
}

#[test]
fn the_payer_cannot_reenter_settlement_during_transfer() {
    settlement::the_payer_cannot_reenter_settlement_during_transfer(Intex);
}
