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

use outbe_paynote::test_support as paynote_support;

use crate::{
    api,
    errors::NodFactoryError,
    precompile::INodFactory,
    runtime,
    sol_ext::{IReferenceCurrency, IERC20},
};
use outbe_vaultrouter::api::IVaultRouter;

fn nod_context(nod_id: WwdEntityId, snapshot: U256) -> B256 {
    outbe_paynote::api::settlement_context(
        outbe_paynote::api::SettlementDomain::Nod,
        B256::from(nod_id.to_u256()),
        U256::ONE,
        snapshot,
    )
    .unwrap()
}

/// The chain ID `World`'s storage provider reports; PayNote folds it into
/// every commitment, so fixtures must be built under the same one.
const CHAIN_ID: u64 = 1;

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

/// The Nod's derived cost, in the `u128` minor units a PayNote spend carries.
fn cost_of(input: &NodIssueParams) -> u128 {
    let cost =
        outbe_nod::api::settlement_cost_minor(input.entry_price_minor, input.gratis_load_minor)
            .expect("derive the Nod cost");
    u128::try_from(cost).expect("test Nod cost fits a PayNote spend amount")
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

    fn settle_and_mine(
        &mut self,
        nod_id: WwdEntityId,
        caller: Address,
        nonce: u64,
        auth: ModifyAuth,
        paynote_proof: &[u8],
    ) -> Result<U256, PrecompileError> {
        self.settle(nod_id, caller, paynote_proof)?;
        self.mine_gratis(api::MineGratisRequest {
            caller,
            nod_id,
            nonce,
            auth,
        })
    }

    fn settle(
        &mut self,
        nod_id: WwdEntityId,
        caller: Address,
        proof: &[u8],
    ) -> Result<(), PrecompileError> {
        self.enter(|storage, scope, parent| {
            api::settle_nod_with_paynote(&storage, scope, parent, caller, nod_id, proof)
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

    /// Seeds one note bound to `nod_id` at an explicit snapshot and returns the
    /// proof plus the nullifier that spend would book.
    fn fund_bound(
        &mut self,
        asset: Address,
        nod_id: WwdEntityId,
        snapshot: U256,
        note_amount: U256,
        spend_amount: U256,
    ) -> (Vec<u8>, B256) {
        let fixture = paynote_support::note_and_spend_proof(
            CHAIN_ID,
            asset,
            nod_context(nod_id, snapshot),
            note_amount,
            spend_amount,
        );
        paynote_support::seed_pool(&mut self.provider, CHAIN_ID, &[fixture.commitment]);
        let nullifier = outbe_protocol::codec::field_to_b256(&fixture.public.nullifier).unwrap();
        (fixture.proof, nullifier)
    }

    /// Quotes `nod_id` and binds the proof to that snapshot. Panics if the nod
    /// cannot be quoted, so a missing rate cannot hide as a context mismatch.
    fn fund_note<T>(
        &mut self,
        asset: Address,
        nod_id: WwdEntityId,
        note_amount: T,
        spend_amount: T,
    ) -> (Vec<u8>, B256)
    where
        U256: alloy_primitives::ruint::UintTryFrom<T>,
    {
        let note_amount = U256::from(note_amount);
        let spend_amount = U256::from(spend_amount);
        let snapshot = self.enter(|storage, scope, parent| {
            runtime::quote_settlement(&storage, scope, parent, nod_id, asset)
                .expect("note is quoted against a settleable nod")
                .2
        });
        self.fund_bound(asset, nod_id, snapshot, note_amount, spend_amount)
    }

    /// Registers `NOTE_ASSET` for the Nod's reference currency and mints a note
    /// that exactly covers its cost, returning the spend proof `settle_nod`
    /// needs.
    fn covering_proof(&mut self, nod_id: WwdEntityId, input: &NodIssueParams) -> Vec<u8> {
        self.register_reference_currency_asset(NOTE_ASSET);
        let cost = cost_of(input);
        self.fund_note(NOTE_ASSET, nod_id, cost, cost).0
    }

    /// Stamps the bucket's call directly. The scan that decides *when* to stamp
    /// is covered in `outbe_nod::called_tests`; what matters here is the gate
    /// `settle_nod` applies once it is stamped.
    fn mark_called(&mut self, nod_id: WwdEntityId, at: u64) {
        self.enter(|storage, scope, parent| {
            let item = nod_api::get_item(&storage, scope, parent, nod_id)
                .unwrap()
                .unwrap();
            NodContract::new(storage)
                .bucket_called_at
                .write(&item.bucket_key, at)
                .unwrap();
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

// ---- PayNote-discharged cost ---------------------------------------------
//
// A Nod's cost is paid by spending a note, not by a transfer. The value itself
// reached the reserve vault when the note was deposited, so what these tests
// pin is the proof obligation: the right owner, the right asset, enough
// covered, and exactly one spend per note.

const NOTE_ASSET: Address = Address::new([0x71; 20]);
const EUR_ASSET: Address = Address::new([0x72; 20]);
const SIX_DECIMALS: u64 = 1_000_000;

fn assert_covering_paynote_mines_nod(input: NodIssueParams) {
    let mut world = World::new();
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&input);
    let (proof, _nullifier) = world.fund_note(NOTE_ASSET, nod_id, cost, cost);
    world.provider.clear_events(NOD_FACTORY_ADDRESS);
    let nonce = world.pow_nonce(nod_id);

    let minted = world
        .settle_and_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .unwrap();
    assert_eq!(minted, input.gratis_load_minor);
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_none());

    let paid: Vec<_> = world
        .provider
        .get_ordered_events()
        .iter()
        .filter(|event| event.address == NOD_FACTORY_ADDRESS)
        .filter_map(|event| INodFactory::NodPaid::decode_log_data(&event.data).ok())
        .collect();
    assert_eq!(paid.len(), 1);
    assert_eq!(paid[0].owner, input.owner);
    assert_eq!(
        paid[0].asset, NOTE_ASSET,
        "the log must name the asset the note carried"
    );
    assert_eq!(paid[0].paymentMinor, U256::from(cost_of(&input)));

    let spent = world
        .enter(|storage, _, _| outbe_paynote::api::is_spent(&storage, paid[0].nullifier).unwrap());
    assert!(spent, "mining must burn the note it was paid with");
}

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
mod note_regressions;
mod paid_entitlement;
