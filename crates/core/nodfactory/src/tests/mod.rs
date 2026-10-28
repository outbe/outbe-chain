use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_gratis::enclave_client::test_enclave;
use outbe_gratisfactory::api::ModifyAuth;
use outbe_nod::{
    api as nod_api, constants::CALL_NOTICE_PERIOD, precompile::INod, NodContract, NodIssueParams,
};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::wwd_entity_id::WwdEntityId;
use outbe_primitives::{
    addresses::{NOD_ADDRESS, NOD_FACTORY_ADDRESS},
    error::PrecompileError,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    units::SCALE_1E6_U64,
};
use outbe_tee::protocol::GratisOp;
use outbe_tee_enclave::gratis::{derive_modify_key, modify_mac};
use outbe_vaultrouter::settlement::SettlementPayment;

use crate::{
    api,
    errors::NodFactoryError,
    precompile::INodFactory,
    runtime,
    sol_ext::{IReferenceCurrency, IERC20},
};
use outbe_vaultrouter::api::IVaultRouter;
use outbe_vaultrouter::settlement::SettlementError;

const ALL_PHASES: outbe_oracle::call_sweep::Selection = outbe_oracle::call_sweep::Selection {
    process_call: true,
    process_forfeit: true,
    process_cleanup: true,
};

#[test]
fn maintenance_noop_and_query_are_read_only_and_dispatch_is_guarded() {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(1_700_000_000u64));
    provider.set_block_number(77);
    StorageHandle::enter(&mut provider, |storage| {
        NodContract::new(storage)
            .daily_genesis_day
            .write(outbe_primitives::time::timestamp_to_date_key(1_700_000_000))
            .unwrap();
    });
    let maintain = INodFactory::maintainCall {
        process_call: false,
        process_forfeit: false,
        process_cleanup: false,
    }
    .abi_encode();
    let query = INodFactory::hasMaintenanceWorkCall {}.abi_encode();
    let before = provider.storage.clone();
    StorageHandle::enter(&mut provider, |storage| {
        assert!(crate::precompile::dispatch(
            storage.clone(),
            &maintain,
            Address::repeat_byte(9),
            U256::ZERO
        )
        .unwrap()
        .is_empty());
        assert!(crate::precompile::dispatch(
            storage,
            &maintain,
            Address::repeat_byte(8),
            U256::from(1)
        )
        .is_err());
    });
    assert_eq!(provider.storage, before);
    assert!(provider.events.is_empty());
    provider.set_static(true);
    StorageHandle::enter(&mut provider, |storage| {
        let result = crate::precompile::dispatch(
            storage.clone(),
            &query,
            Address::repeat_byte(7),
            U256::ZERO,
        )
        .unwrap();
        let flags = INodFactory::hasMaintenanceWorkCall::abi_decode_returns(&result).unwrap();
        assert!(!flags.call_pending && !flags.forfeit_pending && !flags.cleanup_pending);
        assert!(matches!(
            crate::precompile::dispatch(storage, &maintain, Address::repeat_byte(6), U256::ZERO),
            Err(PrecompileError::WriteProtection)
        ));
    });
    assert_eq!(provider.storage, before);
    assert!(provider.events.is_empty());
}

#[test]
fn cleanup_only_maintenance_inspects_eligible_queue_without_advancing_daily_calls() {
    use outbe_primitives::{
        expiry_queue,
        time::{timestamp_to_date_key, SECONDS_PER_DAY},
    };
    let now = 1_700_000_000u64;
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(now));
    provider.set_block_number(77);
    StorageHandle::enter(&mut provider, |storage| {
        let nod = NodContract::new(storage.clone());
        let day = timestamp_to_date_key(now - SECONDS_PER_DAY);
        nod.daily_genesis_day.write(day).unwrap();
        outbe_oracle::schema::OracleContract::new(storage.clone())
            .utc_day_vwap_last_finalized
            .write(day)
            .unwrap();
        let queue = outbe_nod::state::CleanupDays(&nod);
        let future = B256::repeat_byte(1);
        let eligible = B256::repeat_byte(2);
        expiry_queue::push(&queue, future, now + SECONDS_PER_DAY).unwrap();
        assert_eq!(
            runtime::has_maintenance_work(&storage).unwrap(),
            (true, true, false)
        );
        expiry_queue::push(&queue, eligible, now - SECONDS_PER_DAY).unwrap();
        assert_eq!(
            runtime::has_maintenance_work(&storage).unwrap(),
            (true, true, true)
        );
        runtime::maintain(
            &storage,
            outbe_oracle::call_sweep::Selection {
                process_call: false,
                process_forfeit: false,
                process_cleanup: true,
            },
        )
        .unwrap();
        assert_eq!(
            runtime::has_maintenance_work(&storage).unwrap(),
            (true, true, false)
        );
        assert_eq!(nod.call_sweep_day.read().unwrap(), 0);
        assert_eq!(nod.daily_budget.read().unwrap(), U256::ZERO);
        assert_eq!(nod.daily_phase.read().unwrap(), 0);
    });
}

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
            &outbe_tee_enclave::gratis::ModifyOperation {
                account: owner,
                op: GratisOp::Mint,
                amount,
                op_nonce: 0,
                chain_id: B256::from(U256::from(1)),
            },
        ),
        op_nonce: 0,
    }
}

fn decrypt_gratis(owner: Address, encrypted: &[u8]) -> U256 {
    let key =
        outbe_tee_enclave::gratis::derive_view_key(&test_enclave::state_key(), owner).unwrap();
    outbe_tee::gratis_decrypt::decrypt_gratis_balance(&key, owner, encrypted).unwrap()
}

fn gratis_balance(storage: &StorageHandle<'_>, owner: Address) -> U256 {
    decrypt_gratis(
        owner,
        &outbe_gratis::api::balance_ct(storage.clone(), owner).unwrap(),
    )
}

fn seed_production_nod_genesis(storage: &StorageHandle<'_>) {
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
    (0_u64..1_000_000)
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
                U256::from(SCALE_1E6_U64),
            )],
        )
        .unwrap();
}

struct World {
    provider: HashMapStorageProvider,
}

impl World {
    fn new() -> Self {
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_block_number(1);
        provider.set_timestamp(U256::from(1_700_000_000));

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
        });
        Self { provider }
    }

    fn enter<R>(&mut self, call: impl FnOnce(StorageHandle<'_>) -> R) -> R {
        StorageHandle::enter(&mut self.provider, call)
    }

    fn issue(&mut self, input: &NodIssueParams) -> WwdEntityId {
        self.enter(|storage| {
            api::issue_nod(
                &storage,
                &outbe_nod::test_support::encrypted_fixture(input, CHAIN_ID),
            )
        })
        .unwrap()
    }

    fn mine_gratis(&mut self, request: api::MineGratisRequest) -> Result<U256, PrecompileError> {
        self.enter(|storage| {
            let owner = nod_api::get_item(&storage, request.nod_id)?
                .expect("issued Nod")
                .owner;
            let before = gratis_balance(&storage, owner);
            let encrypted = api::mine_gratis(&storage, request)?;
            assert_eq!(
                encrypted,
                outbe_gratis::api::balance_ct(storage.clone(), owner)?
            );
            Ok(decrypt_gratis(owner, &encrypted) - before)
        })
    }

    fn pow_nonce(&mut self, nod_id: WwdEntityId) -> u64 {
        let owner = self.enter(|storage| {
            nod_api::get_item(&storage, nod_id)
                .unwrap()
                .expect("issued Nod")
                .owner
        });
        find_valid_nonce(nod_id, owner)
    }

    /// Settles by ERC20 in the reference currency; a free Nod moves no tokens.
    fn settle(&mut self, nod_id: WwdEntityId, caller: Address) -> Result<(), PrecompileError> {
        self.register_reference_currency_asset(SETTLEMENT_ASSET);
        self.enter(|storage| {
            api::settle_nod(
                &storage,
                nod_id,
                SettlementPayment {
                    payer: caller,
                    asset: SETTLEMENT_ASSET,
                    snapshot_id: U256::ZERO,
                },
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
        self.enter(|storage| {
            let pair = outbe_oracle::api::AddressPair::new_coen_to(iso_code);
            let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
            if oracle.pair_index_of(pair).unwrap() == 0 {
                outbe_oracle::api::register_pair(storage.clone(), pair).unwrap();
            }
            outbe_oracle::api::set_exchange_rate(
                storage.clone(),
                Address::ZERO,
                pair,
                outbe_oracle::api::RateObservation {
                    rate,
                    block_number: 1,
                    timestamp: storage.now_secs().unwrap(),
                },
            )
            .unwrap();
        });
    }

    /// Prices `COEN/<iso>` at `rate` live, for the last closed UTC day and inside
    /// the trailing VWAP window settlement converts at.
    fn publish_coen_rate(&mut self, iso_code: u16, rate: U256) {
        self.publish_coen_spot(iso_code, rate);
        self.enter(|storage| {
            use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};
            let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), iso_code)
                .unwrap()
                .expect("COEN pair registered");
            let day = previous_date_key(timestamp_to_date_key(storage.now_secs().unwrap()))
                .expect("date step");
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

    /// Stamps the group's call directly. `tests::nod_lifecycle` covers the
    /// scan that decides *when* to stamp. What matters here is the gate
    /// `settle_nod` applies once it is stamped.
    fn mark_called(&mut self, nod_id: WwdEntityId, at: u64) {
        self.enter(|storage| {
            let item = nod_api::get_item(&storage, nod_id).unwrap().unwrap();
            let nod = NodContract::new(storage);
            nod.group_called_at.write(&item.group_key, at).unwrap();
            // A free Nod's group seals no call terms; give it the notice a priced one seals.
            let notice = &nod.group_call_notice_period_seconds;
            if notice.read(&item.group_key).unwrap() == 0 {
                notice.write(&item.group_key, CALL_NOTICE_PERIOD).unwrap();
            }
        });
    }

    fn set_timestamp(&mut self, timestamp: u64) {
        self.provider.set_timestamp(U256::from(timestamp));
    }
}

const CHAIN_ID: u64 = 1;

const SETTLEMENT_ASSET: Address = Address::new([0x71; 20]);
const SETTLEMENT_ASSET_EUR: Address = Address::new([0x72; 20]);

fn public_nod_data(world: &mut World, nod_id: WwdEntityId) -> INod::NodData {
    world.enter(|storage| {
        let bytes = outbe_nod::precompile::dispatch(
            storage,
            &INod::getNodCall {
                nodId: nod_id.to_u256(),
            }
            .abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        INod::getNodCall::abi_decode_returns(&bytes).unwrap()
    })
}

fn dual_currency_params(owner: Address) -> NodIssueParams {
    NodIssueParams {
        issuance_currency: 978,
        reference_currency: 840,
        ..params(owner)
    }
}

fn paid_event(world: &World) -> INodFactory::NodSettled {
    world
        .provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| INodFactory::NodSettled::decode_log_data(&event.data).ok())
        .last()
        .expect("NodSettled event")
}

fn is_settled(world: &mut World, nod_id: WwdEntityId) -> bool {
    world
        .enter(|storage| nod_api::get_item(&storage, nod_id))
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
    world.enter(|storage| {
        api::settle_nod(
            &storage,
            nod_id,
            SettlementPayment {
                payer: owner,
                asset,
                snapshot_id: snapshot,
            },
        )
    })
}

mod costs;
mod currency_snapshots;
mod direct_fx_admission;
mod issuance;
mod nod_lifecycle;
mod paid_entitlement;
