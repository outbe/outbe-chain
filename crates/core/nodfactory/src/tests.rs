use std::sync::Arc;

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{begin_block, EntityId36, ExecutionScope};
use outbe_gratis::enclave_client::test_enclave;
use outbe_gratisfactory::api::ModifyAuth;
use outbe_nod::{
    api as nod_api, precompile::INod, NodContract, NodIssueParams, NodRepositoryReader,
};
use outbe_offchain_storage::MemoryStorage;
use outbe_primitives::{
    addresses::{COMPRESSED_ENTITIES_ADDRESS, NOD_ADDRESS, NOD_FACTORY_ADDRESS},
    error::PrecompileError,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};
use outbe_tee::protocol::GratisOp;
use outbe_tee_enclave::gratis::{derive_modify_key, modify_mac};

use crate::{api, errors::NodFactoryError, precompile::INodFactory, runtime, sol_ext::IERC20};

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
        .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(3))
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
        entry_price_minor: U256::from(500),
        cost_amount_minor: U256::ZERO,
        issuance_currency: 840,
        reference_currency: 840,
    }
}

/// Same Nod, but with a cost that forces the real payment path.
fn paid_params(owner: Address) -> NodIssueParams {
    NodIssueParams {
        cost_amount_minor: U256::from(500),
        ..params(owner)
    }
}

fn word(value: U256) -> Bytes {
    Bytes::from(value.to_be_bytes::<32>().to_vec())
}

fn find_valid_nonce(nod_id: EntityId36) -> U256 {
    (0_u64..100_000)
        .map(U256::from)
        .find(|nonce| runtime::validate_pow(nod_id, *nonce).is_ok())
        .expect("test identity has a nonce in the bounded search")
}

struct World {
    provider: HashMapStorageProvider,
    scope: ExecutionScope,
    parent: NodRepositoryReader,
}

impl World {
    fn new() -> Self {
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_timestamp(U256::from(1_700_000_000));
        let scope = ExecutionScope::new();
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

    fn issue(&mut self, input: &NodIssueParams) -> EntityId36 {
        self.enter(|storage, scope, parent| api::issue_nod(&storage, scope, parent, input))
            .unwrap()
    }

    fn settle(&mut self, nod_id: EntityId36, payer: Address) -> U256 {
        self.try_settle(nod_id, payer).unwrap()
    }

    fn try_settle(&mut self, nod_id: EntityId36, payer: Address) -> Result<U256, PrecompileError> {
        self.enter(|storage, scope, parent| api::settle_nod(&storage, scope, parent, payer, nod_id))
    }

    /// Stubs the vault router's asset registry plus the ERC20 and deposit legs
    /// the settlement path drives on the resolved asset.
    fn register_settlement_asset(&mut self, asset: Address, deposited: U256) {
        use outbe_vaultrouter::api::IVaultRouter;

        self.provider.stub_sub_call_at_selector(
            outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
            IVaultRouter::referenceCurrencyAssetsCall::SELECTOR,
            Bytes::from(
                IVaultRouter::referenceCurrencyAssetsCall::abi_encode_returns(&vec![asset]),
            ),
        );
        self.provider.stub_sub_call_at_selector(
            asset,
            IERC20::transferFromCall::SELECTOR,
            word(U256::from(1)),
        );
        self.provider.stub_sub_call_at_selector(
            asset,
            IERC20::approveCall::SELECTOR,
            word(U256::from(1)),
        );
        self.provider.stub_sub_call_at_selector(
            outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
            IVaultRouter::depositCall::SELECTOR,
            word(deposited),
        );
    }

    fn is_settled(&mut self, nod_id: EntityId36) -> bool {
        self.enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
            .unwrap()
            .unwrap()
            .is_settled
    }

    fn qualify(&mut self, nod_id: EntityId36) {
        self.enter(|storage, scope, parent| {
            let item = nod_api::get_item(&storage, scope, parent, nod_id)
                .unwrap()
                .unwrap();
            NodContract::new(storage)
                .qualify_bucket(scope, parent, item.bucket_key)
                .unwrap();
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
            (NOD_FACTORY_ADDRESS, INodFactory::NodIssued::SIGNATURE_HASH),
        ]
    );
}

#[test]
fn second_same_block_issue_updates_the_pending_bucket_without_parent_projection() {
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
    let bucket_id = EntityId36::new(first.worldwide_day, bucket_key.0);
    let bucket = world
        .enter(|storage, scope, parent| nod_api::get_bucket(&storage, scope, parent, bucket_id))
        .unwrap()
        .unwrap();
    assert_eq!(bucket.total_nods, 2);
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
    let nonce = find_valid_nonce(nod_id);
    let error = world
        .enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
                Address::repeat_byte(0x44),
                nod_id,
                nonce,
                dummy_auth(),
            )
        })
        .unwrap_err();
    assert!(matches!(
        error,
        PrecompileError::Revert(ref reason) if reason == &NodFactoryError::NotOwner.to_string()
    ));
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
    world.settle(nod_id, input.owner);
    let nonce = find_valid_nonce(nod_id);

    world
        .enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
                input.owner,
                nod_id,
                nonce,
                dummy_auth(),
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
    world.settle(nod_id, input.owner);
    world.provider.clear_events(NOD_ADDRESS);
    world.provider.clear_events(NOD_FACTORY_ADDRESS);
    let nonce = find_valid_nonce(nod_id);
    let minted = world
        .enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
                input.owner,
                nod_id,
                nonce,
                mine_auth(input.owner, input.gratis_load_minor),
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
    let bucket_id = EntityId36::new(input.worldwide_day, bucket_key.0);
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
            (NOD_ADDRESS, INod::NodBodyDeleted::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::NodBucketBodyDeleted::SIGNATURE_HASH),
            (NOD_FACTORY_ADDRESS, INodFactory::NodBurned::SIGNATURE_HASH),
        ]
    );
}

#[test]
fn mine_gratis_rejects_an_unsettled_nod() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x56));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    let nonce = find_valid_nonce(nod_id);

    let error = world
        .enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
                input.owner,
                nod_id,
                nonce,
                mine_auth(input.owner, input.gratis_load_minor),
            )
        })
        .unwrap_err();
    assert!(matches!(
        error,
        PrecompileError::Revert(ref reason) if reason == &NodFactoryError::NodNotSettled.to_string()
    ));
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_some());
}

#[test]
fn settle_nod_accepts_any_payer() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x57));
    let nod_id = world.issue(&input);
    assert!(!world.is_settled(nod_id));
    world.provider.clear_events(NOD_FACTORY_ADDRESS);

    let stranger = Address::repeat_byte(0x58);
    assert_eq!(world.settle(nod_id, stranger), input.cost_amount_minor);
    assert!(world.is_settled(nod_id));

    let settled: Vec<_> = world
        .provider
        .get_ordered_events()
        .iter()
        .filter(|event| event.address == NOD_FACTORY_ADDRESS)
        .filter_map(|event| INodFactory::NodSettled::decode_log_data(&event.data).ok())
        .collect();
    assert_eq!(settled.len(), 1);
    assert_eq!(settled[0].owner, input.owner);
    assert_eq!(settled[0].payer, stranger);
    assert_eq!(settled[0].amountPaid, input.cost_amount_minor);
}

#[test]
fn settle_nod_rejects_double_settlement() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x59));
    let nod_id = world.issue(&input);
    world.settle(nod_id, input.owner);

    let error = world.try_settle(nod_id, input.owner).unwrap_err();
    assert!(matches!(
        error,
        PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::NodAlreadySettled.to_string()
    ));
}

#[test]
fn settle_nod_fails_when_the_reference_currency_has_no_registered_asset() {
    use outbe_vaultrouter::api::IVaultRouter;

    let mut world = World::new();
    let input = paid_params(Address::repeat_byte(0x5c));
    let nod_id = world.issue(&input);
    world.provider.stub_sub_call_at_selector(
        outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
        IVaultRouter::referenceCurrencyAssetsCall::SELECTOR,
        Bytes::from(IVaultRouter::referenceCurrencyAssetsCall::abi_encode_returns(&Vec::new())),
    );

    let error = world.try_settle(nod_id, input.owner).unwrap_err();
    assert!(matches!(
        error,
        PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::NoSettlementAsset {
                reference_currency: input.reference_currency,
            }
            .to_string()
    ));
    // The settled flag is written before the asset sub-calls, so the checkpoint
    // must have unwound it.
    assert!(!world.is_settled(nod_id));
}

#[test]
fn settle_nod_pays_the_cost_amount_with_the_registered_reference_currency_asset() {
    let mut world = World::new();
    let input = paid_params(Address::repeat_byte(0x5e));
    let nod_id = world.issue(&input);
    let asset = Address::repeat_byte(0x5f);
    world.register_settlement_asset(asset, input.cost_amount_minor);
    world.provider.clear_events(NOD_FACTORY_ADDRESS);

    let paid = world.settle(nod_id, Address::repeat_byte(0x60));
    assert_eq!(paid, input.cost_amount_minor);
    assert!(world.is_settled(nod_id));

    let settled: Vec<_> = world
        .provider
        .get_ordered_events()
        .iter()
        .filter(|event| event.address == NOD_FACTORY_ADDRESS)
        .filter_map(|event| INodFactory::NodSettled::decode_log_data(&event.data).ok())
        .collect();
    assert_eq!(settled.len(), 1);
    assert_eq!(
        settled[0].asset, asset,
        "the log must name the resolved asset"
    );
    assert_eq!(settled[0].amountPaid, input.cost_amount_minor);
}

#[test]
fn settle_before_qualification_then_mine_after() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x5a));
    let nod_id = world.issue(&input);

    // Paying is allowed while the bucket is still unqualified.
    world.settle(nod_id, Address::repeat_byte(0x5b));
    world.qualify(nod_id);

    let nonce = find_valid_nonce(nod_id);
    let minted = world
        .enter(|storage, scope, parent| {
            api::mine_gratis(
                &storage,
                scope,
                parent,
                input.owner,
                nod_id,
                nonce,
                mine_auth(input.owner, input.gratis_load_minor),
            )
        })
        .unwrap();
    assert_eq!(minted, input.gratis_load_minor);
    assert!(world
        .enter(|storage, scope, parent| nod_api::get_item(&storage, scope, parent, nod_id))
        .unwrap()
        .is_none());
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
