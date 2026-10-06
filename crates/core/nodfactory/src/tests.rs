use outbe_compressed_entities::test_support::seed_compressed_entities_genesis;
use std::sync::Arc;

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_compressed_entities::{begin_block, ExecutionScope, WwdEntityId};
use outbe_gratis::enclave_client::test_enclave;
use outbe_gratisfactory::api::ModifyAuth;
use outbe_nod::{
    api as nod_api, constants::CALL_NOTICE_PERIOD, precompile::INod, NodContract, NodIssueParams,
    NodRepositoryReader,
};
use outbe_offchain_storage::MemoryStorage;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    addresses::{NOD_ADDRESS, NOD_FACTORY_ADDRESS},
    error::PrecompileError,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};
use outbe_tee::protocol::GratisOp;
use outbe_tee_enclave::gratis::{derive_modify_key, modify_mac};

use crate::{
    api,
    errors::NodFactoryError,
    precompile::INodFactory,
    runtime,
    sol_ext::{IReferenceCurrency, IERC20},
};
use outbe_vaultrouter::api::IVaultRouter;

fn dummy_auth() -> ModifyAuth {
    ModifyAuth {
        mac: [0; 32],
        op_nonce: 0,
    }
}

fn mine_auth(owner: Address, amount: U256) -> ModifyAuth {
    test_enclave::install();
    let modify_key = derive_modify_key(&test_enclave::state_key(), owner).unwrap();
    ModifyAuth {
        mac: modify_mac(
            &modify_key,
            owner,
            GratisOp::Mint,
            amount,
            0,
            B256::from(U256::from(1)),
        ),
        op_nonce: 0,
    }
}

fn seed_production_nod_genesis(storage: &StorageHandle<'_>) {
    seed_compressed_entities_genesis(storage).expect("CE genesis fixture");
    // These tests exercise the production call terms.
    NodContract::new(storage.clone())
        .config_profile
        .write(outbe_nod::config::PROFILE_PROD)
        .unwrap();
}

fn params(owner: Address) -> NodIssueParams {
    NodIssueParams {
        owner,
        gratis_load_minor: U256::from(1_000),
        worldwide_day: WorldwideDay::new(20_241_220),
        league_id: 1,
        entry_price_minor: U256::from(500_000),
        issuance_currency: 840,
        reference_currency: 840,
    }
}

/// A Nod with no entry price: it costs nothing, so settling it moves no tokens.
fn free(owner: Address) -> NodIssueParams {
    NodIssueParams {
        entry_price_minor: U256::ZERO,
        ..params(owner)
    }
}

/// The Nod's derived cost in reference minor units.
fn cost_of(input: &NodIssueParams) -> u128 {
    let cost =
        outbe_nod::api::settlement_cost_minor(input.entry_price_minor, input.gratis_load_minor)
            .expect("derive the Nod cost");
    u128::try_from(cost).expect("test Nod cost fits u128")
}

fn find_valid_nonce(nod_id: WwdEntityId, owner: Address) -> u64 {
    (0_u64..100_000)
        .find(|nonce| runtime::validate_pow(nod_id, owner, *nonce).is_ok())
        .expect("test identity has a nonce in the bounded search")
}

/// One observation at `rate` in the last second of the current trailing window.
fn seed_window_vwap(storage: &StorageHandle<'_>, iso_code: u16, rate: U256) {
    let snapshot = outbe_oracle::api::current_vwap_snapshot(storage.clone()).unwrap();
    outbe_oracle::schema::OracleContract::new(storage.clone())
        .write_snapshot(
            snapshot.cutoff() - 1,
            &[(
                outbe_oracle::api::AddressPair::new_coen_to(iso_code),
                rate,
                U256::from(SIX_DECIMALS),
            )],
        )
        .unwrap();
}

struct World {
    provider: HashMapStorageProvider,
    scope: ExecutionScope,
    parent: NodRepositoryReader,
}

impl World {
    fn new() -> Self {
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_block_number(1);
        provider.set_timestamp(U256::from(1_700_000_000));
        let scope = ExecutionScope::new();
        provider.stub_sub_call_at_selector(
            outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
            IVaultRouter::assetVaultsCountCall::SELECTOR,
            Bytes::from(IVaultRouter::assetVaultsCountCall::abi_encode_returns(
                &U256::ONE,
            )),
        );
        provider.stub_sub_call_at_selector(
            outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
            IVaultRouter::depositCall::SELECTOR,
            Bytes::from(IVaultRouter::depositCall::abi_encode_returns(&U256::ONE)),
        );
        StorageHandle::enter(&mut provider, |storage| {
            seed_production_nod_genesis(&storage);
            begin_block(storage, &scope).unwrap();
        });
        Self {
            provider,
            scope,
            parent: NodRepositoryReader::new(Arc::new(MemoryStorage::new())),
        }
    }

    fn enter<R>(
        &mut self,
        call: impl FnOnce(StorageHandle<'_>, &ExecutionScope, &NodRepositoryReader) -> R,
    ) -> R {
        let scope = &self.scope;
        let parent = self.parent.clone();
        StorageHandle::enter(&mut self.provider, |storage| call(storage, scope, &parent))
    }

    fn issue(&mut self, input: &NodIssueParams) -> WwdEntityId {
        self.enter(|storage, scope, parent| api::issue_nod(&storage, scope, parent, input))
            .unwrap()
    }

    fn mine_gratis(&mut self, request: api::MineGratisRequest) -> Result<U256, PrecompileError> {
        self.enter(|storage, scope, parent| api::mine_gratis(&storage, scope, parent, request))
    }

    fn pow_nonce(&mut self, nod_id: WwdEntityId) -> u64 {
        let owner = self.enter(|storage, scope, parent| {
            nod_api::get_item(&storage, scope, parent, nod_id)
                .unwrap()
                .expect("issued Nod")
                .owner
        });
        find_valid_nonce(nod_id, owner)
    }

    /// Settles by ERC20 in the reference currency; a free Nod moves no tokens.
    fn settle(&mut self, nod_id: WwdEntityId, caller: Address) -> Result<(), PrecompileError> {
        self.register_reference_currency_asset(PAYMENT_ASSET);
        self.enter(|storage, scope, parent| {
            api::settle_nod(
                &storage,
                scope,
                parent,
                caller,
                nod_id,
                PAYMENT_ASSET,
                U256::ZERO,
            )
        })
    }

    /// Publishes `asset` as a vaulted USD (840) token, the default reference rail.
    fn register_reference_currency_asset(&mut self, asset: Address) {
        self.register_settlement_asset(asset, 840);
    }

    fn register_settlement_asset(&mut self, asset: Address, iso_code: u16) {
        self.provider.stub_sub_call_at_selector(
            asset,
            IReferenceCurrency::isoCodeCall::SELECTOR,
            Bytes::from(IReferenceCurrency::isoCodeCall::abi_encode_returns(
                &iso_code,
            )),
        );
        self.set_asset_decimals(asset, 6);
    }

    fn register_reference_currency_assets(&mut self, assets: Vec<Address>) {
        for asset in assets {
            self.register_settlement_asset(asset, 840);
        }
    }

    /// Registers `COEN/<iso>` and prices it live only.
    fn publish_coen_spot(&mut self, iso_code: u16, rate: U256) {
        self.enter(|storage, _, _| {
            let pair = outbe_oracle::api::AddressPair::new_coen_to(iso_code);
            let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
            if oracle.pair_index_of(pair).unwrap() == 0 {
                outbe_oracle::api::register_pair(storage.clone(), pair).unwrap();
            }
            outbe_oracle::api::set_exchange_rate(
                storage.clone(),
                Address::ZERO,
                pair,
                rate,
                1,
                storage.timestamp().unwrap().to::<u64>(),
            )
            .unwrap();
        });
    }

    /// Prices `COEN/<iso>` at `rate` live, for the last closed UTC day and inside
    /// the trailing VWAP window settlement converts at.
    fn publish_coen_rate(&mut self, iso_code: u16, rate: U256) {
        self.publish_coen_spot(iso_code, rate);
        self.enter(|storage, _, _| {
            use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};
            let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), iso_code)
                .unwrap()
                .expect("COEN pair registered");
            let day = previous_date_key(timestamp_to_date_key(
                storage.timestamp().unwrap().to::<u64>(),
            ));
            outbe_oracle::schema::OracleContract::new(storage.clone())
                .record_utc_day_vwap(day, index, rate)
                .unwrap();
            seed_window_vwap(&storage, iso_code, rate);
        });
    }

    fn set_asset_decimals(&mut self, asset: Address, decimals: u8) {
        self.provider.stub_sub_call_at_selector(
            asset,
            IERC20::decimalsCall::SELECTOR,
            Bytes::from(IERC20::decimalsCall::abi_encode_returns(&decimals)),
        );
    }

    /// Stamps the bucket's call directly. The scan that decides *when* to stamp
    /// is covered in `outbe_nod::called_tests`; what matters here is the gate
    /// `settle_nod` applies once it is stamped.
    fn mark_called(&mut self, nod_id: WwdEntityId, at: u64) {
        self.enter(|storage, scope, parent| {
            let item = nod_api::get_item(&storage, scope, parent, nod_id)
                .unwrap()
                .unwrap();
            let nod = NodContract::new(storage);
            nod.bucket_called_at.write(&item.bucket_key, at).unwrap();
            // A free Nod's bucket seals no call terms; give it the notice a priced one seals.
            let notice = &nod.callable_bucket_call_notice_period_seconds;
            if notice.read(&item.bucket_key).unwrap() == 0 {
                notice.write(&item.bucket_key, CALL_NOTICE_PERIOD).unwrap();
            }
        });
    }

    fn set_timestamp(&mut self, timestamp: u64) {
        self.provider.set_timestamp(U256::from(timestamp));
    }

    /// Closes the bucket's first full day above its floor, which qualifies it.
    fn qualify(&mut self, nod_id: WwdEntityId) {
        self.enter(|storage, scope, parent| {
            let item = nod_api::get_item(&storage, scope, parent, nod_id)
                .unwrap()
                .unwrap();
            let floor = nod_api::get_bucket(
                &storage,
                scope,
                parent,
                WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key.0),
            )
            .unwrap()
            .unwrap()
            .floor_price_minor()
            .unwrap();
            let issued_at = NodContract::new(storage.clone())
                .callable_bucket_issued_at
                .read(&item.bucket_key)
                .unwrap();
            let pair = outbe_oracle::api::AddressPair::new_coen_to(item.reference_currency);
            let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
            let mut index = oracle.pair_index_of(pair).unwrap();
            if index == 0 {
                index = outbe_oracle::api::register_pair(storage.clone(), pair).unwrap();
            }
            let day = outbe_primitives::time::first_full_day(issued_at);
            oracle
                .record_utc_day_vwap(day, index, floor + U256::from(1))
                .unwrap();
            if oracle.utc_day_vwap_last_finalized.read().unwrap() < day {
                oracle.utc_day_vwap_last_finalized.write(day).unwrap();
            }
        });
    }
}

const PAYMENT_ASSET: Address = Address::new([0x71; 20]);
const EUR_ASSET: Address = Address::new([0x72; 20]);
const SIX_DECIMALS: u64 = 1_000_000;

mod materialization;

fn public_nod_data(world: &mut World, nod_id: WwdEntityId) -> INod::NodData {
    world.enter(|storage, scope, parent| {
        let bytes = outbe_nod::precompile::dispatch(
            storage,
            scope,
            parent,
            &INod::nodDataCall {
                nodId: nod_id.to_u256(),
            }
            .abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        INod::nodDataCall::abi_decode_returns(&bytes).unwrap()
    })
}

fn dual_currency_params(owner: Address) -> NodIssueParams {
    NodIssueParams {
        issuance_currency: 978,
        reference_currency: 840,
        ..params(owner)
    }
}

fn paid_event(world: &World) -> INodFactory::NodPaid {
    world
        .provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| INodFactory::NodPaid::decode_log_data(&event.data).ok())
        .last()
        .expect("NodPaid event")
}

fn is_settled(world: &mut World, nod_id: WwdEntityId) -> bool {
    world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .unwrap()
        .is_settled
}

/// Pays `asset` by ERC20 against a fixed balance stub: a payment that clears the
/// snapshot check stops at the balance delta instead.
fn settle_erc20(
    world: &mut World,
    nod_id: WwdEntityId,
    owner: Address,
    asset: Address,
    snapshot: U256,
) -> Result<(), PrecompileError> {
    for (selector, ret) in [
        (
            IERC20::transferFromCall::SELECTOR,
            IERC20::transferFromCall::abi_encode_returns(&true),
        ),
        (
            IERC20::approveCall::SELECTOR,
            IERC20::approveCall::abi_encode_returns(&true),
        ),
        (
            IERC20::balanceOfCall::SELECTOR,
            IERC20::balanceOfCall::abi_encode_returns(&U256::ZERO),
        ),
    ] {
        world
            .provider
            .stub_sub_call_at_selector(asset, selector, Bytes::from(ret));
    }
    world.enter(|storage, scope, parent| {
        api::settle_nod(&storage, scope, parent, owner, nod_id, asset, snapshot)
    })
}

mod costs;
mod currency_snapshots;
mod direct_fx_admission;
mod issuance;
mod paid_entitlement;
