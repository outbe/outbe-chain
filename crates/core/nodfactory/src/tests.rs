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
    addresses::{COMPRESSED_ENTITIES_ADDRESS, NOD_ADDRESS, NOD_FACTORY_ADDRESS},
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
use outbe_protocol::Codec as _;
use outbe_protocol::OutbeV1;
use outbe_vaultrouter::api::IVaultRouter;

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

fn seed_compressed_entities_genesis(storage: &StorageHandle<'_>) {
    storage
        .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(4))
        .unwrap();
    storage
        .sstore(
            COMPRESSED_ENTITIES_ADDRESS,
            U256::from(1),
            U256::from_be_slice(
                outbe_compressed_entities::sealed_root(B256::ZERO)
                    .unwrap()
                    .as_slice(),
            ),
        )
        .unwrap();
}

fn params(owner: Address) -> NodIssueParams {
    NodIssueParams {
        owner,
        gratis_load_minor: U256::from(1_000),
        worldwide_day: WorldwideDay::new(20_241_220),
        league_id: 1,
        floor_price_minor: U256::from(540),
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

#[test]
fn nod_pow_binds_owner_and_zero_sequence() {
    let owner = Address::repeat_byte(0x11);
    let other = Address::repeat_byte(0x22);
    let nod_id = NodContract::generate_nod_id(owner, WorldwideDay::new(20_241_201)).unwrap();
    let nonce = 42;
    let bound = runtime::compute_pow_hash(nod_id, owner, nonce);
    assert_ne!(bound, runtime::compute_pow_hash(nod_id, other, nonce));
    assert_ne!(
        bound,
        outbe_common::pow::compute_pow_hash(nod_id.to_u256(), nonce)
    );
    let solved = find_valid_nonce(nod_id, owner);
    assert!(runtime::validate_pow(nod_id, owner, solved).is_ok());
    assert_ne!(
        runtime::compute_pow_hash(nod_id, owner, solved),
        runtime::compute_pow_hash(nod_id, other, solved)
    );
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
            seed_compressed_entities_genesis(&storage);
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
        self.enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
                api::MineGratisRequest {
                    caller,
                    nod_id,
                    nonce,
                    auth,
                },
            )
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

    fn publish_coen_rate(&mut self, iso_code: u16, rate: U256) {
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

    fn set_asset_decimals(&mut self, asset: Address, decimals: u8) {
        self.provider.stub_sub_call_at_selector(
            asset,
            IERC20::decimalsCall::SELECTOR,
            Bytes::from(IERC20::decimalsCall::abi_encode_returns(&decimals)),
        );
    }

    /// Seeds the PayNote pool with one note and returns a spend proof over it
    /// alongside the nullifier that spend would book.
    fn fund_note(
        &mut self,
        asset: Address,
        owner: Address,
        note_amount: u128,
        spend_amount: u128,
    ) -> (Vec<u8>, B256) {
        self.fund_note_u256(
            asset,
            owner,
            U256::from(note_amount),
            U256::from(spend_amount),
        )
    }

    fn fund_note_u256(
        &mut self,
        asset: Address,
        owner: Address,
        note_amount: U256,
        spend_amount: U256,
    ) -> (Vec<u8>, B256) {
        let fixture = paynote_support::note_and_spend_proof(
            CHAIN_ID,
            asset,
            owner,
            note_amount,
            spend_amount,
        );
        paynote_support::seed_pool(&mut self.provider, CHAIN_ID, &[fixture.commitment]);
        let nullifier = B256::from_slice(&OutbeV1::field_to_be_bytes(&fixture.public.nullifier));
        (fixture.proof, nullifier)
    }

    /// Registers `NOTE_ASSET` for the Nod's reference currency and mints a note
    /// that exactly covers its cost, returning the spend proof `settle_nod`
    /// needs.
    fn covering_proof(&mut self, input: &NodIssueParams) -> Vec<u8> {
        self.register_reference_currency_asset(NOTE_ASSET);
        let cost = cost_of(input);
        self.fund_note(NOTE_ASSET, input.owner, cost, cost).0
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
                .utc_day_vwap_value
                .get_nested(&day)
                .write(&index, item.floor_price_minor + U256::from(1))
                .unwrap();
            if oracle.utc_day_vwap_last_finalized.read().unwrap() < day {
                oracle.utc_day_vwap_last_finalized.write(day).unwrap();
            }
        });
    }
}

#[test]
fn issue_is_immediately_readable_and_keeps_product_event_order() {
    let mut world = World::new();
    let input = params(address!("1111111111111111111111111111111111111111"));
    let nod_id = world.issue(&input);
    let item = world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .unwrap();
    assert_eq!(item.owner, input.owner);
    assert_eq!(
        world
            .enter(|storage, scope, parent| {
                nod_api::list_by_owner(&storage, scope, parent, input.owner)
            })
            .unwrap()
            .len(),
        1
    );

    let events: Vec<_> = world
        .provider
        .get_ordered_events()
        .iter()
        .filter(|event| event.address == NOD_ADDRESS || event.address == NOD_FACTORY_ADDRESS)
        .map(|event| (event.address, event.data.topics()[0]))
        .collect();
    assert_eq!(
        events,
        [
            (NOD_ADDRESS, INod::NodBodyStored::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::NodBucketBodyStored::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::Transfer::SIGNATURE_HASH),
            (NOD_FACTORY_ADDRESS, INodFactory::NodIssued::SIGNATURE_HASH),
        ]
    );
}

#[test]
fn second_same_block_issue_reuses_the_pending_bucket_without_parent_projection() {
    let mut world = World::new();
    let first = params(Address::repeat_byte(0x18));
    let second = params(Address::repeat_byte(0x19));
    let first_id = world.issue(&first);
    let second_id = world.issue(&second);
    assert_ne!(first_id, second_id);

    let bucket_key = NodContract::bucket_key(
        first.worldwide_day,
        first.floor_price_minor,
        first.reference_currency,
    );
    let bucket_id = WwdEntityId::from_day_and_digest(first.worldwide_day, bucket_key.0);
    let bucket = world
        .enter(|storage, scope, parent| nod_api::get_bucket(&storage, scope, parent, bucket_id))
        .unwrap()
        .unwrap();
    assert_eq!(bucket.entry_price_minor, first.entry_price_minor);
    assert_eq!(
        world
            .enter(|storage, _, _| NodContract::new(storage).bucket_nod_count.read(&bucket_key))
            .unwrap(),
        2
    );
    assert_eq!(
        world
            .provider
            .get_ordered_events()
            .iter()
            .filter(|event| {
                event.address == NOD_ADDRESS
                    && event.data.topics().first()
                        == Some(&INod::NodBucketBodyStored::SIGNATURE_HASH)
            })
            .count(),
        1,
        "only the first member creates the bucket body"
    );
    assert_eq!(
        world
            .enter(|storage, scope, parent| nod_api::list_all(&storage, scope, parent))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn invalid_and_duplicate_issuance_leave_one_canonical_item() {
    let mut world = World::new();
    let mut invalid = params(Address::ZERO);
    let error = world
        .enter(|storage, scope, parent| api::issue_nod(&storage, scope, parent, &invalid))
        .unwrap_err();
    assert!(matches!(
        error,
        PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::InvalidOwner.to_string()
    ));

    invalid.owner = Address::repeat_byte(0x22);
    let nod_id = world.issue(&invalid);
    assert!(world
        .enter(|storage, scope, parent| api::issue_nod(&storage, scope, parent, &invalid))
        .is_err());
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_some());
}

#[test]
fn failed_authorization_preserves_the_loaded_nod() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x33));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    let proof = world.covering_proof(&input);
    world.settle(nod_id, input.owner, &proof).unwrap();
    let nonce = world.pow_nonce(nod_id);
    world
        .enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
                api::MineGratisRequest {
                    caller: Address::repeat_byte(0x44),
                    nod_id,
                    nonce,
                    auth: dummy_auth(),
                },
            )
        })
        .unwrap_err();
    // Dummy MAC is rejected regardless of who submits; the paid Nod remains.
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_some());
}

#[test]
fn invalid_gratis_mac_rolls_back_the_nod_burn() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x45));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    let proof = world.covering_proof(&input);
    world.settle(nod_id, input.owner, &proof).unwrap();
    let nonce = world.pow_nonce(nod_id);

    world
        .enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
                api::MineGratisRequest {
                    caller: input.owner,
                    nod_id,
                    nonce,
                    auth: dummy_auth(),
                },
            )
        })
        .unwrap_err();
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_some());
}

#[test]
fn qualified_mine_deletes_item_and_last_bucket_then_emits_burn() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x55));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.provider.clear_events(NOD_ADDRESS);
    world.provider.clear_events(NOD_FACTORY_ADDRESS);
    let proof = world.covering_proof(&input);
    world.settle(nod_id, input.owner, &proof).unwrap();
    let nonce = world.pow_nonce(nod_id);
    let minted = world
        .enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
                api::MineGratisRequest {
                    caller: input.owner,
                    nod_id,
                    nonce,
                    auth: mine_auth(input.owner, input.gratis_load_minor),
                },
            )
        })
        .unwrap();
    assert_eq!(minted, input.gratis_load_minor);
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_none());
    let bucket_key = NodContract::bucket_key(
        input.worldwide_day,
        input.floor_price_minor,
        input.reference_currency,
    );
    let bucket_id = WwdEntityId::from_day_and_digest(input.worldwide_day, bucket_key.0);
    assert!(world
        .enter(|storage, scope, parent| { nod_api::get_bucket(&storage, scope, parent, bucket_id) })
        .unwrap()
        .is_none());

    let signatures: Vec<_> = world
        .provider
        .get_ordered_events()
        .iter()
        .filter(|event| event.address == NOD_ADDRESS || event.address == NOD_FACTORY_ADDRESS)
        .map(|event| (event.address, event.data.topics()[0]))
        .collect();
    assert_eq!(
        signatures,
        [
            (NOD_ADDRESS, INod::NodBodyStored::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::NodBucketBodyStored::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::MetadataUpdate::SIGNATURE_HASH),
            (NOD_FACTORY_ADDRESS, INodFactory::NodPaid::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::NodBodyDeleted::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::NodBucketBodyDeleted::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::Transfer::SIGNATURE_HASH),
            (
                NOD_FACTORY_ADDRESS,
                INodFactory::NodExercised::SIGNATURE_HASH
            ),
            (NOD_FACTORY_ADDRESS, INodFactory::NodBurned::SIGNATURE_HASH),
        ]
    );
}

/// Mining stays available to a Nod that qualified after issuance, with a
/// payment step after qualification.
#[test]
fn a_nod_qualifying_after_issuance_still_mines() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x5a));
    let nod_id = world.issue(&input);

    world.qualify(nod_id);

    let proof = world.covering_proof(&input);
    world.settle(nod_id, input.owner, &proof).unwrap();
    let nonce = world.pow_nonce(nod_id);
    let minted = world
        .enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
                api::MineGratisRequest {
                    caller: input.owner,
                    nod_id,
                    nonce,
                    auth: mine_auth(input.owner, input.gratis_load_minor),
                },
            )
        })
        .unwrap();
    assert_eq!(minted, input.gratis_load_minor);
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_none());
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

#[test]
fn a_cost_that_does_not_divide_evenly_is_floored_and_the_note_matches_it() {
    // 500.001 six-decimal units: the chain charges 500, the figure
    // `settlementCostMinor` advertises. Rounding up would demand 501.
    let mut world = World::new();
    let input = NodIssueParams {
        entry_price_minor: U256::from(500_001u64),
        ..params(Address::repeat_byte(0x61))
    };
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&input);
    assert_eq!(cost, 500);
    let (proof, _nullifier) = world.fund_note(NOTE_ASSET, input.owner, cost, cost);
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
}

#[test]
fn a_note_in_a_wider_asset_pays_the_cost_scaled_to_its_decimals() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x61));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    world.set_asset_decimals(NOTE_ASSET, 18);
    let cost = U256::from(cost_of(&input)) * U256::from(1_000_000_000_000u64);
    let (proof, _nullifier) = world.fund_note_u256(NOTE_ASSET, input.owner, cost, cost);
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
}

#[test]
fn a_covering_paynote_mines_a_paid_nod_and_books_the_nullifier() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x61));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&input);
    let (proof, _nullifier) = world.fund_note(NOTE_ASSET, input.owner, cost, cost);
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
    assert_eq!(paid[0].amountCovered, U256::from(cost_of(&input)));

    let spent = world
        .enter(|storage, _, _| outbe_paynote::api::is_spent(&storage, paid[0].nullifier).unwrap());
    assert!(spent, "mining must burn the note it was paid with");
}

/// The surplus of an over-covering spend has already reached the reserve vault
/// and nothing returns it, so the spend must equal the cost exactly.
#[test]
fn a_paynote_over_the_cost_leaves_the_nod_and_the_note_intact() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x66));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&input);
    let (proof, nullifier) = world.fund_note(NOTE_ASSET, input.owner, cost + 1, cost + 1);
    let nonce = world.pow_nonce(nod_id);

    let error = world
        .settle_and_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::PayNoteCostMismatch {
                covered: U256::from(cost + 1),
                required: U256::from(cost),
            }
            .to_string()),
        "unexpected error: {error:?}"
    );
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_some());
    let spent =
        world.enter(|storage, _, _| outbe_paynote::api::is_spent(&storage, nullifier).unwrap());
    assert!(!spent, "a rejected over-cover must not consume the note");
}

#[test]
fn a_paynote_short_of_the_cost_leaves_the_nod_and_the_note_intact() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x62));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&input);
    let (proof, _nullifier) = world.fund_note(NOTE_ASSET, input.owner, cost, cost - 1);
    let nonce = world.pow_nonce(nod_id);

    let error = world
        .settle_and_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::PayNoteCostMismatch {
                covered: U256::from(cost - 1),
                required: U256::from(cost),
            }
            .to_string()),
        "unexpected error: {error:?}"
    );
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_some());
}

/// `consume` books the nullifier before the cover check runs, so this is the
/// test that proves settlement is one rollback unit: rejected settlement must
/// leave the note spendable rather than destroying it for nothing.
#[test]
fn rejected_settlement_unbooks_the_nullifier_it_had_already_spent() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x63));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&input);
    let (proof, nullifier) = world.fund_note(NOTE_ASSET, input.owner, cost, cost - 1);
    let nonce = world.pow_nonce(nod_id);

    world
        .settle_and_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .unwrap_err();

    let spent =
        world.enter(|storage, _, _| outbe_paynote::api::is_spent(&storage, nullifier).unwrap());
    assert!(!spent, "reverted settlement must not consume the note");
}

#[test]
fn a_paynote_naming_another_owner_cannot_pay_this_nod() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x64));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&input);
    let stranger = Address::repeat_byte(0x65);
    let (proof, _nullifier) = world.fund_note(NOTE_ASSET, stranger, cost, cost);
    let nonce = world.pow_nonce(nod_id);

    let error = world
        .settle_and_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::PayNoteOwnerMismatch {
                expected: input.owner,
                actual: stranger,
            }
            .to_string()),
        "unexpected error: {error:?}"
    );
}

#[test]
fn a_stranger_cannot_pay_a_nod_with_a_proof_naming_themselves() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x66));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&input);
    let stranger = Address::repeat_byte(0x67);
    let (proof, nullifier) = world.fund_note(NOTE_ASSET, stranger, cost, cost);

    let error = world.settle(nod_id, stranger, &proof).unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::PayNoteOwnerMismatch {
                expected: input.owner,
                actual: stranger,
            }
            .to_string()),
        "unexpected error: {error:?}"
    );
    assert!(
        !world.enter(|storage, _, _| outbe_paynote::api::is_spent(&storage, nullifier).unwrap())
    );
}

#[test]
fn a_stranger_can_relay_a_paynote_proof_naming_the_nod_owner() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x68));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&input);
    let stranger = Address::repeat_byte(0x69);
    let (proof, nullifier) = world.fund_note(NOTE_ASSET, input.owner, cost, cost);

    world.settle(nod_id, stranger, &proof).unwrap();

    let item = world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .unwrap();
    assert!(item.is_settled);
    assert_eq!(item.owner, input.owner);
    assert!(world.enter(|storage, _, _| outbe_paynote::api::is_spent(&storage, nullifier).unwrap()));
    let paid = world
        .provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| INodFactory::NodPaid::decode_log_data(&event.data).ok())
        .last()
        .expect("NodPaid event");
    assert_eq!(paid.owner, input.owner);
    assert_eq!(paid.nodId, nod_id.to_u256());
}

#[test]
fn a_stranger_can_mine_with_the_owners_auth() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x6a));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    let proof = world.covering_proof(&input);
    world.settle(nod_id, input.owner, &proof).unwrap();
    let nonce = world.pow_nonce(nod_id);
    let stranger = Address::repeat_byte(0x6b);

    let minted = world
        .enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
                api::MineGratisRequest {
                    caller: stranger,
                    nod_id,
                    nonce,
                    auth: mine_auth(input.owner, input.gratis_load_minor),
                },
            )
        })
        .unwrap();
    assert_eq!(minted, input.gratis_load_minor);
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_none());
    let exercised = world
        .provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| INodFactory::NodExercised::decode_log_data(&event.data).ok())
        .last()
        .expect("NodExercised event");
    assert_eq!(exercised.owner, input.owner);
    assert_eq!(exercised.nodId, nod_id.to_u256());
}

#[test]
fn a_paynote_in_the_wrong_asset_cannot_pay_this_nod() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x66));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let other_asset = Address::repeat_byte(0x67);
    world.register_settlement_asset(other_asset, 978);
    let cost = cost_of(&input);
    let (proof, _nullifier) = world.fund_note(other_asset, input.owner, cost, cost);
    let nonce = world.pow_nonce(nod_id);

    let error = world
        .settle_and_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::SettlementCurrencyMismatch { iso_code: 978 }.to_string()),
        "unexpected error: {error:?}"
    );
}

#[test]
fn any_asset_registered_for_the_reference_currency_pays_the_nod() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x6a));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    // The registry lists interchangeable assets for the currency; the payer
    // picks which one their note carries, and it need not be the first.
    let second_asset = Address::repeat_byte(0x6b);
    world.register_reference_currency_assets(vec![NOTE_ASSET, second_asset]);
    let cost = cost_of(&input);
    let (proof, nullifier) = world.fund_note(second_asset, input.owner, cost, cost);
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
    assert!(world.enter(|storage, _, _| outbe_paynote::api::is_spent(&storage, nullifier).unwrap()));
}

#[test]
fn one_note_cannot_pay_two_nods() {
    let mut world = World::new();
    let first = params(Address::repeat_byte(0x68));
    let first_id = world.issue(&first);
    world.qualify(first_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&first);
    let (proof, _nullifier) = world.fund_note(NOTE_ASSET, first.owner, cost, cost);

    let first_nonce = world.pow_nonce(first_id);
    world
        .settle_and_mine(
            first_id,
            first.owner,
            first_nonce,
            mine_auth(first.owner, first.gratis_load_minor),
            &proof,
        )
        .unwrap();

    let second = NodIssueParams {
        worldwide_day: WorldwideDay::new(20_241_221),
        ..params(first.owner)
    };
    let second_id = world.issue(&second);
    world.qualify(second_id);
    let second_nonce = world.pow_nonce(second_id);
    let error = world
        .settle_and_mine(
            second_id,
            second.owner,
            second_nonce,
            mine_auth(second.owner, second.gratis_load_minor),
            &proof,
        )
        .unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(ref reason) if reason.contains("nullifier")),
        "replaying a spent note must revert, got: {error:?}"
    );
}

#[test]
fn a_paynote_can_cover_a_nod_cost_above_u128() {
    let mut world = World::new();
    // Above u128, yet inside the price ladder the call index bins by.
    let cost = (U256::from(1) << 129) + U256::from(17);
    let input = NodIssueParams {
        gratis_load_minor: U256::from(1_000_000),
        entry_price_minor: cost,
        ..params(Address::repeat_byte(0x6a))
    };
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let (proof, nullifier) = world.fund_note_u256(NOTE_ASSET, input.owner, cost, cost);
    let nonce = world.pow_nonce(nod_id);

    let minted = world
        .settle_and_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .expect("U256 PayNote covers U256 Nod cost");
    assert_eq!(minted, input.gratis_load_minor);
    assert!(world.enter(|storage, _, _| outbe_paynote::api::is_spent(&storage, nullifier).unwrap()));
    let paid = world
        .provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| INodFactory::NodPaid::decode_log_data(&event.data).ok())
        .last()
        .expect("NodPaid event");
    assert_eq!(paid.amountCovered, cost);
}

#[test]
fn settlement_charges_zk_verification_base_gas() {
    assert_eq!(
        crate::precompile::base_gas(&INodFactory::settleNodWithPayNoteCall::SELECTOR),
        outbe_primitives::storage::gas::ZK_VERIFY_GAS
    );
    assert_eq!(
        crate::precompile::base_gas(&INodFactory::materializationHeadCall::SELECTOR),
        outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS
    );
    assert_eq!(
        crate::precompile::base_gas(&[]),
        outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS
    );
}

#[test]
fn certified_generation_has_no_public_installation_selector() {
    let mut world = World::new();
    let selector_hash = alloy_primitives::keccak256("installCertifiedGeneration(bytes)".as_bytes());
    let calldata = selector_hash[..4].to_vec();
    let storage_before = world.provider.storage.clone();
    let events_before = world.provider.get_ordered_events().to_vec();

    let result = world.enter(|storage, scope, parent| {
        crate::precompile::dispatch(
            storage,
            scope,
            parent,
            &calldata,
            Address::repeat_byte(0x91),
            U256::ZERO,
        )
    });

    assert!(result.is_err());
    assert_eq!(world.provider.storage, storage_before);
    assert_eq!(world.provider.get_ordered_events(), events_before);
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

/// Settlement at the deadline remains valid; the paid Nod can then be mined.
#[test]
fn a_called_nod_still_mines_at_the_settlement_deadline() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x55));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);

    let called_at = 1_700_000_000;
    world.mark_called(nod_id, called_at);
    world.set_timestamp(called_at + u64::from(CALL_NOTICE_PERIOD));
    assert_eq!(public_nod_data(&mut world, nod_id).effectiveState, 2);

    let proof = world.covering_proof(&input);
    world.settle(nod_id, input.owner, &proof).unwrap();
    let nonce = world.pow_nonce(nod_id);
    let minted = world
        .enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
                api::MineGratisRequest {
                    caller: input.owner,
                    nod_id,
                    nonce,
                    auth: mine_auth(input.owner, input.gratis_load_minor),
                },
            )
        })
        .unwrap();
    assert_eq!(minted, input.gratis_load_minor);
}

/// A called Nod settles inside its notice period whether or not its bucket has
/// qualified: the call alone opens settlement.
#[test]
fn a_called_nod_settles_without_qualifying() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x56));
    let nod_id = world.issue(&input);
    let proof = world.covering_proof(&input);
    assert_eq!(
        world
            .settle(nod_id, input.owner, &proof)
            .unwrap_err()
            .to_string(),
        PrecompileError::from(NodFactoryError::NodNotQualified).to_string()
    );

    let called_at = 1_700_000_000;
    world.mark_called(nod_id, called_at);
    world.set_timestamp(called_at + 1);
    assert!(!public_nod_data(&mut world, nod_id).isQualified);
    world.settle(nod_id, input.owner, &proof).unwrap();
    assert!(public_nod_data(&mut world, nod_id).isSettled);
}

/// Past the deadline the Nod is forfeit. The daily sweep burns it, but this gate
/// closes the window between the deadline and the sweep reaching it.
#[test]
fn settlement_is_rejected_once_the_deadline_has_passed() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x55));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);

    let called_at = 1_700_000_000;
    world.mark_called(nod_id, called_at);
    world.set_timestamp(called_at + u64::from(CALL_NOTICE_PERIOD) + 1);

    let data = public_nod_data(&mut world, nod_id);
    assert_eq!(data.effectiveState, 4);
    assert!(data.isQualified);
    assert!(!data.isSettled);
    assert_eq!(
        data.settlementDeadline,
        called_at + u64::from(CALL_NOTICE_PERIOD)
    );

    let error = world.settle(nod_id, input.owner, &[]).unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::CallDeadlineExpired.to_string()),
        "unexpected error: {error:?}"
    );
    // The Nod survives for the sweep to burn; the gate only refuses to mine it.
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_some());
}

#[test]
fn settlement_preserves_entitlement_and_failed_mining_can_retry_after_deadline() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x81));
    let nod_id = world.issue(&input);
    let proof = world.covering_proof(&input);
    assert!(
        world.settle(nod_id, input.owner, &proof).is_err(),
        "unqualified"
    );
    world.qualify(nod_id);
    let called_at = 1_700_000_000;
    world.mark_called(nod_id, called_at);
    world.set_timestamp(called_at + u64::from(CALL_NOTICE_PERIOD));
    world.settle(nod_id, input.owner, &proof).unwrap();
    let stored = world.enter(|storage, scope, parent| {
        let item = nod_api::get_item(&storage, scope, parent, nod_id)
            .unwrap()
            .unwrap();
        assert!(item.is_settled);
        assert_eq!(item.owner, input.owner);
        let bucket = nod_api::get_bucket(
            &storage,
            scope,
            parent,
            WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key),
        )
        .unwrap()
        .unwrap();
        assert_eq!(bucket.settled_nods, 1);
        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.total_supply().unwrap(), 1);
        assert_eq!(nod.bucket_nod_count.read(&item.bucket_key).unwrap(), 0);
        assert_eq!(
            nod.bucket_called_at.read(&item.bucket_key).unwrap(),
            called_at
        );
        outbe_nod::canonical_item(&item)
    });
    let before = world.provider.storage.clone();
    let events = world.provider.get_ordered_events().to_vec();
    assert!(
        world.settle(nod_id, input.owner, &proof).is_err(),
        "duplicate settlement"
    );
    world.set_timestamp(called_at + u64::from(CALL_NOTICE_PERIOD) + 365 * 86_400);
    assert_eq!(public_nod_data(&mut world, nod_id).effectiveState, 3);
    let nonce = world.pow_nonce(nod_id);
    for (caller, candidate, auth) in [
        (Address::repeat_byte(0x82), nonce, dummy_auth()),
        (
            input.owner,
            (0..100_000)
                .find(|n| runtime::validate_pow(nod_id, input.owner, *n).is_err())
                .unwrap(),
            dummy_auth(),
        ),
        (input.owner, nonce, dummy_auth()),
    ] {
        assert!(world
            .enter(|storage, scope, parent| api::mine_gratis(
                &storage,
                scope,
                parent,
                api::MineGratisRequest {
                    caller,
                    nod_id,
                    nonce: candidate,
                    auth
                }
            ))
            .is_err());
        assert_eq!(world.provider.storage, before);
        assert_eq!(world.provider.get_ordered_events(), events);
        world.enter(|storage, scope, parent| {
            let item = nod_api::get_item(&storage, scope, parent, nod_id)
                .unwrap()
                .unwrap();
            assert_eq!(outbe_nod::canonical_item(&item), stored);
        });
    }
    let minted = world
        .enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
                api::MineGratisRequest {
                    caller: input.owner,
                    nod_id,
                    nonce,
                    auth: mine_auth(input.owner, input.gratis_load_minor),
                },
            )
        })
        .unwrap();
    assert_eq!(minted, input.gratis_load_minor);
    assert!(world
        .enter(|storage, scope, parent| api::mine_gratis(
            &storage,
            scope,
            parent,
            api::MineGratisRequest {
                caller: input.owner,
                nod_id,
                nonce,
                auth: dummy_auth()
            }
        ))
        .is_err());
}

#[test]
fn settlement_failure_rolls_back_payment_change_and_body_updates() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x83));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = cost_of(&input);
    let (proof, nullifier) = world.fund_note(NOTE_ASSET, input.owner, cost * 2, cost);
    // Force a state failure after consume has booked the nullifier and appended change.
    world.enter(|storage, scope, parent| {
        let item = nod_api::get_item(&storage, scope, parent, nod_id)
            .unwrap()
            .unwrap();
        NodContract::new(storage)
            .bucket_nod_count
            .write(&item.bucket_key, 0)
            .unwrap();
    });
    let before = world.provider.storage.clone();
    let events = world.provider.get_ordered_events().to_vec();
    assert!(world.settle(nod_id, input.owner, &proof).is_err());
    assert_eq!(world.provider.storage, before);
    assert_eq!(world.provider.get_ordered_events(), events);
    world.enter(|storage, scope, parent| {
        assert!(!outbe_paynote::api::is_spent(&storage, nullifier).unwrap());
        let item = nod_api::get_item(&storage, scope, parent, nod_id)
            .unwrap()
            .unwrap();
        assert!(!item.is_settled);
        NodContract::new(storage)
            .bucket_nod_count
            .write(&item.bucket_key, 1)
            .unwrap();
    });
    world.settle(nod_id, input.owner, &proof).unwrap();
}

#[test]
fn unpaid_mining_and_retired_payment_selector_are_rejected() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x84));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    let error = world
        .enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
                api::MineGratisRequest {
                    caller: input.owner,
                    nod_id,
                    nonce: 0,
                    auth: dummy_auth(),
                },
            )
        })
        .unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(reason) if reason == NodFactoryError::NodNotSettled.to_string())
    );
    let old_selector =
        alloy_primitives::keccak256("mineGratis(uint256,uint64,bytes32,uint64,bytes)");
    assert!(world
        .enter(|storage, scope, parent| crate::precompile::dispatch(
            storage,
            scope,
            parent,
            &old_selector[..4],
            input.owner,
            U256::ZERO
        ))
        .is_err());
    assert_eq!(
        crate::precompile::base_gas(&INodFactory::mineGratisCall::SELECTOR),
        outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS
    );
}

#[test]
fn fidelity_persistence_failure_preserves_paid_entitlement_and_mint_nonce() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x85));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    let proof = world.covering_proof(&input);
    world.settle(nod_id, input.owner, &proof).unwrap();
    let before = world.provider.storage.clone();
    let events = world.provider.get_ordered_events().to_vec();
    let nonce = world.pow_nonce(nod_id);
    world
        .provider
        .fail_mutation_at_address(outbe_primitives::addresses::FIDELITY_ADDRESS);
    let result = world.enter(|storage, scope, parent| {
        api::mine_gratis(
            &storage,
            scope,
            parent,
            api::MineGratisRequest {
                caller: input.owner,
                nod_id,
                nonce,
                auth: mine_auth(input.owner, input.gratis_load_minor),
            },
        )
    });
    assert!(result.is_err());
    world.provider.clear_mutation_failure();
    assert_eq!(world.provider.storage, before);
    assert_eq!(world.provider.get_ordered_events(), events);
    world.enter(|storage, scope, parent| {
        assert!(
            nod_api::get_item(&storage, scope, parent, nod_id)
                .unwrap()
                .unwrap()
                .is_settled
        );
        api::mine_gratis(
            &storage,
            scope,
            parent,
            api::MineGratisRequest {
                caller: input.owner,
                nod_id,
                nonce,
                auth: mine_auth(input.owner, input.gratis_load_minor),
            },
        )
        .unwrap();
    });
}

#[test]
fn erc20_settlement_enforces_eligibility_before_payment_and_accepts_zero_cost() {
    let mut world = World::new();
    let mut input = params(Address::repeat_byte(0x91));
    input.entry_price_minor = U256::ZERO;
    let nod_id = world.issue(&input);
    world.register_reference_currency_asset(NOTE_ASSET);
    let settle = |world: &mut World, caller, asset| {
        world.enter(|storage, scope, parent| {
            api::settle_nod(&storage, scope, parent, caller, nod_id, asset)
        })
    };
    let stranger = Address::repeat_byte(0x92);
    assert_eq!(
        settle(&mut world, stranger, NOTE_ASSET)
            .unwrap_err()
            .to_string(),
        PrecompileError::from(NodFactoryError::NodNotQualified).to_string()
    );
    world.qualify(nod_id);
    let foreign = Address::repeat_byte(0x99);
    world.register_settlement_asset(foreign, 978);
    let error = settle(&mut world, stranger, foreign).unwrap_err();
    assert_eq!(
        error.to_string(),
        PrecompileError::from(NodFactoryError::SettlementCurrencyMismatch { iso_code: 978 })
            .to_string()
    );
    assert!(
        !world
            .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
            .unwrap()
            .unwrap()
            .is_settled
    );

    // No token transfer stubs: zero cost must require neither funds nor approvals.
    settle(&mut world, stranger, NOTE_ASSET).unwrap();
    assert_eq!(
        settle(&mut world, input.owner, NOTE_ASSET)
            .unwrap_err()
            .to_string(),
        PrecompileError::from(NodFactoryError::NodAlreadySettled).to_string()
    );
    let paid = world
        .provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| INodFactory::NodPaid::decode_log_data(&event.data).ok())
        .last()
        .unwrap();
    assert_eq!(paid.owner, input.owner);
    assert_eq!(paid.nullifier, B256::ZERO);
    assert_eq!(paid.amountCovered, U256::ZERO);
}

#[test]
fn erc20_selector_has_no_zk_surcharge_and_old_paynote_selector_is_rejected() {
    assert_eq!(
        crate::precompile::base_gas(&INodFactory::settleNodCall::SELECTOR),
        outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS
    );
    alloy_sol_types::sol! { function settleNod(uint256 nodId, bytes payNoteProof) external; }
    let data = settleNodCall {
        nodId: U256::ONE,
        payNoteProof: Bytes::new(),
    }
    .abi_encode();
    let mut world = World::new();
    assert!(world
        .enter(|storage, scope, parent| crate::precompile::dispatch(
            storage,
            scope,
            parent,
            &data,
            Address::repeat_byte(1),
            U256::ZERO,
        ))
        .is_err());
}

#[test]
fn erc20_settlement_uses_the_existing_inclusive_deadline() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x93));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let called_at = 1_700_000_000;
    world.mark_called(nod_id, called_at);
    let deadline = called_at + u64::from(CALL_NOTICE_PERIOD);
    let foreign = Address::repeat_byte(0x94);
    world.register_settlement_asset(foreign, 978);
    for (timestamp, expected) in [
        (deadline + 1, NodFactoryError::CallDeadlineExpired),
        (
            deadline,
            NodFactoryError::SettlementCurrencyMismatch { iso_code: 978 },
        ),
    ] {
        world.set_timestamp(timestamp);
        let error = world
            .enter(|storage, scope, parent| {
                api::settle_nod(&storage, scope, parent, input.owner, nod_id, foreign)
            })
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            PrecompileError::from(expected).to_string()
        );
    }
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

#[test]
fn the_issuance_currency_settles_through_the_coen_pivot() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xa1));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SIX_DECIMALS));
    world.publish_coen_rate(978, U256::from(SIX_DECIMALS));
    let reference_cost = cost_of(&input);
    let issuance_cost = reference_cost / 2;
    let (proof, _nullifier) = world.fund_note(EUR_ASSET, input.owner, issuance_cost, issuance_cost);

    world.settle(nod_id, input.owner, &proof).unwrap();

    let paid = paid_event(&world);
    assert_eq!(paid.asset, EUR_ASSET);
    assert_eq!(paid.amountCovered, U256::from(issuance_cost));
    assert!(is_settled(&mut world, nod_id));
}

#[test]
fn quote_agrees_with_what_settling_charges_on_both_rails() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xa2));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_settlement_asset(NOTE_ASSET, 840);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SIX_DECIMALS));
    world.publish_coen_rate(978, U256::from(SIX_DECIMALS));

    let (ref_iso, ref_amount, iss_iso, iss_amount) = world.enter(|storage, scope, parent| {
        let (ref_iso, ref_amount) =
            api::quote_settlement(&storage, scope, parent, nod_id, NOTE_ASSET).unwrap();
        let (iss_iso, iss_amount) =
            api::quote_settlement(&storage, scope, parent, nod_id, EUR_ASSET).unwrap();
        (ref_iso, ref_amount, iss_iso, iss_amount)
    });
    assert_eq!(ref_iso, 840);
    assert_eq!(iss_iso, 978);
    assert_eq!(ref_amount, U256::from(cost_of(&input)));
    assert_eq!(iss_amount, ref_amount / U256::from(2u64));

    let spend = u128::try_from(iss_amount).unwrap();
    let (proof, _) = world.fund_note(EUR_ASSET, input.owner, spend, spend);
    world.settle(nod_id, input.owner, &proof).unwrap();
    assert_eq!(paid_event(&world).amountCovered, iss_amount);
}

#[test]
fn issuance_erc20_admission_uses_the_quoted_converted_cost() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xa3));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SIX_DECIMALS));
    world.publish_coen_rate(978, U256::from(SIX_DECIMALS));
    world.provider.stub_sub_call_at_selector(
        EUR_ASSET,
        IERC20::transferFromCall::SELECTOR,
        Bytes::from(IERC20::transferFromCall::abi_encode_returns(&true)),
    );
    world.provider.stub_sub_call_at_selector(
        EUR_ASSET,
        IERC20::approveCall::SELECTOR,
        Bytes::from(IERC20::approveCall::abi_encode_returns(&true)),
    );
    world.provider.stub_sub_call_at_selector(
        EUR_ASSET,
        IERC20::balanceOfCall::SELECTOR,
        Bytes::from(IERC20::balanceOfCall::abi_encode_returns(&U256::ZERO)),
    );

    let quoted = world
        .enter(|storage, scope, parent| {
            api::quote_settlement(&storage, scope, parent, nod_id, EUR_ASSET)
        })
        .unwrap();
    assert_eq!(quoted.0, 978);
    assert_eq!(quoted.1, U256::from(cost_of(&input) / 2));

    // Admission and conversion ran; the fixed balance stub cannot show a delta.
    let error = world
        .enter(|storage, scope, parent| {
            api::settle_nod(&storage, scope, parent, input.owner, nod_id, EUR_ASSET)
        })
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        PrecompileError::from(NodFactoryError::SettlementAmountMismatch).to_string()
    );
    assert!(!is_settled(&mut world, nod_id));
}

#[test]
fn settle_rejects_an_asset_with_no_registered_vault() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0xa4));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    world.provider.stub_sub_call_at_selector(
        outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall::SELECTOR,
        Bytes::from(IVaultRouter::assetVaultsCountCall::abi_encode_returns(
            &U256::ZERO,
        )),
    );
    let cost = cost_of(&input);
    let (proof, _) = world.fund_note(NOTE_ASSET, input.owner, cost, cost);

    let paynote_error = world.settle(nod_id, input.owner, &proof).unwrap_err();
    assert_eq!(
        paynote_error.to_string(),
        PrecompileError::from(NodFactoryError::SettlementAssetNotRegistered { asset: NOTE_ASSET })
            .to_string()
    );
    let erc20_error = world
        .enter(|storage, scope, parent| {
            api::settle_nod(&storage, scope, parent, input.owner, nod_id, NOTE_ASSET)
        })
        .unwrap_err();
    assert_eq!(
        erc20_error.to_string(),
        PrecompileError::from(NodFactoryError::SettlementAssetNotRegistered { asset: NOTE_ASSET })
            .to_string()
    );
    assert!(!is_settled(&mut world, nod_id));
}

#[test]
fn issuance_rail_rejects_a_stale_leg_without_settling() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xa5));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SIX_DECIMALS));
    world.publish_coen_rate(978, U256::from(SIX_DECIMALS));
    let now = 1_700_000_000u64;
    world.enter(|storage, _, _| {
        outbe_oracle::api::set_exchange_rate(
            storage.clone(),
            Address::ZERO,
            outbe_oracle::api::AddressPair::new_coen_to(978),
            U256::from(SIX_DECIMALS),
            1,
            now - outbe_oracle::constants::FX_RATE_MAX_AGE_SECONDS - 1,
        )
        .unwrap();
    });
    let spend = cost_of(&input) / 2;
    let (proof, _) = world.fund_note(EUR_ASSET, input.owner, spend, spend);

    let error = world.settle(nod_id, input.owner, &proof).unwrap_err();
    assert!(
        error.to_string().contains("stale"),
        "unexpected error: {error}"
    );
    assert!(!is_settled(&mut world, nod_id));
}

#[test]
fn issuance_rail_rejects_a_missing_cross_rate() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xa6));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SIX_DECIMALS));
    let spend = cost_of(&input) / 2;
    let (proof, _) = world.fund_note(EUR_ASSET, input.owner, spend, spend);

    let error = world.settle(nod_id, input.owner, &proof).unwrap_err();
    assert!(
        error.to_string().contains("not registered"),
        "unexpected error: {error}"
    );
    assert!(!is_settled(&mut world, nod_id));
}

#[test]
fn quote_settlement_dispatch() {
    let mut world = World::new();
    let input = dual_currency_params(Address::repeat_byte(0xa7));
    let nod_id = world.issue(&input);
    world.register_settlement_asset(EUR_ASSET, 978);
    world.publish_coen_rate(840, U256::from(2 * SIX_DECIMALS));
    world.publish_coen_rate(978, U256::from(SIX_DECIMALS));

    let out = world
        .enter(|storage, scope, parent| {
            crate::precompile::dispatch(
                storage,
                scope,
                parent,
                &INodFactory::quoteSettlementCall {
                    nodId: nod_id.to_u256(),
                    asset: EUR_ASSET,
                }
                .abi_encode(),
                input.owner,
                U256::ZERO,
            )
        })
        .unwrap();
    let ret = INodFactory::quoteSettlementCall::abi_decode_returns(&out).unwrap();
    assert_eq!(ret.settlementCurrency, 978);
    assert_eq!(ret.payableUnits, U256::from(cost_of(&input) / 2));
}
