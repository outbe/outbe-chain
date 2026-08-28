use std::sync::Arc;

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{begin_block, ExecutionScope, WwdEntityId};
use outbe_gratis::enclave_client::test_enclave;
use outbe_gratisfactory::api::ModifyAuth;
use outbe_nod::{
    api as nod_api, constants::CALL_NOTICE_PERIOD, precompile::INod, NodContract, NodIssueParams,
    NodRepositoryReader,
};
use outbe_offchain_storage::MemoryStorage;
use outbe_primitives::{
    addresses::{COMPRESSED_ENTITIES_ADDRESS, NOD_ADDRESS, NOD_FACTORY_ADDRESS},
    error::PrecompileError,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};
use outbe_tee::protocol::GratisOp;
use outbe_tee_enclave::gratis::{derive_modify_key, modify_mac};

use outbe_paynote::test_support as paynote_support;

use crate::{api, errors::NodFactoryError, precompile::INodFactory, runtime};

/// The chain ID `World`'s storage provider reports; Paynote folds it into
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

fn find_valid_nonce(nod_id: WwdEntityId) -> u64 {
    (0_u64..100_000)
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
        provider.set_block_number(1);
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

    fn issue(&mut self, input: &NodIssueParams) -> WwdEntityId {
        self.enter(|storage, scope, parent| api::issue_nod(&storage, scope, parent, input))
            .unwrap()
    }

    fn try_mine(
        &mut self,
        nod_id: WwdEntityId,
        caller: Address,
        nonce: u64,
        auth: ModifyAuth,
        paynote_proof: &[u8],
    ) -> Result<U256, PrecompileError> {
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
                    paynote_proof,
                },
            )
        })
    }

    /// Publishes `asset` as the only asset registered under the Nod's reference
    /// currency, which is the asset a covering paynote must carry.
    fn register_reference_currency_asset(&mut self, asset: Address) {
        use outbe_vaultrouter::api::IVaultRouter;

        self.provider.stub_sub_call_at_selector(
            outbe_primitives::addresses::VAULT_ROUTER_ADDRESS,
            IVaultRouter::referenceCurrencyAssetsCall::SELECTOR,
            Bytes::from(
                IVaultRouter::referenceCurrencyAssetsCall::abi_encode_returns(&vec![asset]),
            ),
        );
    }

    /// Seeds the Paynote pool with one note and returns a spend proof over it
    /// alongside the nullifier that spend would book.
    fn fund_note(
        &mut self,
        asset: Address,
        spender: Address,
        note_amount: u128,
        spend_amount: u128,
    ) -> (Vec<u8>, B256) {
        let fixture = paynote_support::note_and_spend_proof(
            CHAIN_ID,
            asset,
            spender,
            note_amount,
            spend_amount,
        );
        paynote_support::seed_pool(&mut self.provider, CHAIN_ID, &[fixture.commitment]);
        let nullifier = B256::new(outbe_paynote::hash::field_to_be_bytes(
            fixture.public.nullifier,
        ));
        (fixture.proof, nullifier)
    }

    /// Stamps the bucket's call directly. The scan that decides *when* to stamp
    /// is covered in `outbe_nod::called_tests`; what matters here is the gate
    /// `mine_gratis` applies once it is stamped.
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

    fn qualify(&mut self, nod_id: WwdEntityId) {
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
    let bucket_id = WwdEntityId::from_day_and_digest(first.worldwide_day, bucket_key.0);
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
                api::MineGratisRequest {
                    caller: Address::repeat_byte(0x44),
                    nod_id,
                    nonce,
                    auth: dummy_auth(),
                    paynote_proof: &[],
                },
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
    let nonce = find_valid_nonce(nod_id);

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
                    paynote_proof: &[],
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
    let nonce = find_valid_nonce(nod_id);
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
                    paynote_proof: &[],
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
            (NOD_ADDRESS, INod::NodBodyDeleted::SIGNATURE_HASH),
            (NOD_ADDRESS, INod::NodBucketBodyDeleted::SIGNATURE_HASH),
            (NOD_FACTORY_ADDRESS, INodFactory::NodBurned::SIGNATURE_HASH),
        ]
    );
}

/// Mining stays available to a Nod that qualified after issuance; there is no
/// separate pre-payment step to sequence against any more.
#[test]
fn a_nod_qualifying_after_issuance_still_mines() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x5a));
    let nod_id = world.issue(&input);

    world.qualify(nod_id);

    let nonce = find_valid_nonce(nod_id);
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
                    paynote_proof: &[],
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

// ---- paynote-discharged cost ---------------------------------------------
//
// A Nod's cost is paid by spending a note, not by a transfer. The value itself
// reached the reserve vault when the note was deposited, so what these tests
// pin is the proof obligation: the right spender, the right asset, enough
// covered, and exactly one spend per note.

const NOTE_ASSET: Address = Address::new([0x71; 20]);

#[test]
fn a_covering_paynote_mines_a_paid_nod_and_books_the_nullifier() {
    let mut world = World::new();
    let input = paid_params(Address::repeat_byte(0x61));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = u128::try_from(input.cost_amount_minor).unwrap();
    let (proof, _nullifier) = world.fund_note(NOTE_ASSET, input.owner, cost, cost);
    world.provider.clear_events(NOD_FACTORY_ADDRESS);
    let nonce = find_valid_nonce(nod_id);

    let minted = world
        .try_mine(
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
        "the log must name the reference-currency asset"
    );
    assert_eq!(paid[0].amountCovered, input.cost_amount_minor);

    let spent = world
        .enter(|storage, _, _| outbe_paynote::api::is_spent(&storage, paid[0].nullifier).unwrap());
    assert!(spent, "mining must burn the note it was paid with");
}

#[test]
fn a_paynote_short_of_the_cost_leaves_the_nod_and_the_note_intact() {
    let mut world = World::new();
    let input = paid_params(Address::repeat_byte(0x62));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = u128::try_from(input.cost_amount_minor).unwrap();
    let (proof, _nullifier) = world.fund_note(NOTE_ASSET, input.owner, cost, cost - 1);
    let nonce = find_valid_nonce(nod_id);

    let error = world
        .try_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::PaynoteUndercoversCost {
                covered: cost - 1,
                required: cost,
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
/// test that proves the whole mine is one rollback unit: a rejected mine must
/// leave the note spendable rather than destroying it for nothing.
#[test]
fn a_rejected_mine_unbooks_the_nullifier_it_had_already_spent() {
    let mut world = World::new();
    let input = paid_params(Address::repeat_byte(0x63));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = u128::try_from(input.cost_amount_minor).unwrap();
    let (proof, nullifier) = world.fund_note(NOTE_ASSET, input.owner, cost, cost - 1);
    let nonce = find_valid_nonce(nod_id);

    world
        .try_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .unwrap_err();

    let spent =
        world.enter(|storage, _, _| outbe_paynote::api::is_spent(&storage, nullifier).unwrap());
    assert!(!spent, "a reverted mine must not consume the note");
}

#[test]
fn a_paynote_naming_another_spender_cannot_pay_this_nod() {
    let mut world = World::new();
    let input = paid_params(Address::repeat_byte(0x64));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = u128::try_from(input.cost_amount_minor).unwrap();
    let stranger = Address::repeat_byte(0x65);
    let (proof, _nullifier) = world.fund_note(NOTE_ASSET, stranger, cost, cost);
    let nonce = find_valid_nonce(nod_id);

    let error = world
        .try_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::PaynoteSpenderMismatch {
                expected: input.owner,
                actual: stranger,
            }
            .to_string()),
        "unexpected error: {error:?}"
    );
}

#[test]
fn a_paynote_in_the_wrong_asset_cannot_pay_this_nod() {
    let mut world = World::new();
    let input = paid_params(Address::repeat_byte(0x66));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let other_asset = Address::repeat_byte(0x67);
    let cost = u128::try_from(input.cost_amount_minor).unwrap();
    let (proof, _nullifier) = world.fund_note(other_asset, input.owner, cost, cost);
    let nonce = find_valid_nonce(nod_id);

    let error = world
        .try_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::PaynoteAssetMismatch {
                expected: NOTE_ASSET,
                actual: other_asset,
            }
            .to_string()),
        "unexpected error: {error:?}"
    );
}

#[test]
fn one_note_cannot_pay_two_nods() {
    let mut world = World::new();
    let first = paid_params(Address::repeat_byte(0x68));
    let first_id = world.issue(&first);
    world.qualify(first_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let cost = u128::try_from(first.cost_amount_minor).unwrap();
    let (proof, _nullifier) = world.fund_note(NOTE_ASSET, first.owner, cost, cost);

    world
        .try_mine(
            first_id,
            first.owner,
            find_valid_nonce(first_id),
            mine_auth(first.owner, first.gratis_load_minor),
            &proof,
        )
        .unwrap();

    let second = NodIssueParams {
        worldwide_day: WorldwideDay::new(20_241_221),
        ..paid_params(first.owner)
    };
    let second_id = world.issue(&second);
    world.qualify(second_id);
    let error = world
        .try_mine(
            second_id,
            second.owner,
            find_valid_nonce(second_id),
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
fn a_free_nod_mines_without_a_note_and_refuses_one() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x69));
    assert!(input.cost_amount_minor.is_zero());
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let (proof, _nullifier) = world.fund_note(NOTE_ASSET, input.owner, 10, 10);
    let nonce = find_valid_nonce(nod_id);

    let error = world
        .try_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &proof,
        )
        .unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::UnexpectedPaynoteProof.to_string()),
        "unexpected error: {error:?}"
    );

    let minted = world
        .try_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &[],
        )
        .unwrap();
    assert_eq!(minted, input.gratis_load_minor);
}

/// `spend_amount` is a `u128` in the circuit while a Nod cost is a `U256`, so
/// the conversion is checked rather than truncating a cost no note could cover.
#[test]
fn a_nod_cost_wider_than_a_paynote_amount_is_rejected_not_truncated() {
    let mut world = World::new();
    let cost = U256::from(u128::MAX) + U256::ONE;
    let input = NodIssueParams {
        cost_amount_minor: cost,
        ..params(Address::repeat_byte(0x6a))
    };
    let nod_id = world.issue(&input);
    world.qualify(nod_id);
    world.register_reference_currency_asset(NOTE_ASSET);
    let nonce = find_valid_nonce(nod_id);

    let error = world
        .try_mine(
            nod_id,
            input.owner,
            nonce,
            mine_auth(input.owner, input.gratis_load_minor),
            &[0x00],
        )
        .unwrap_err();
    assert!(
        matches!(error, PrecompileError::Revert(ref reason)
            if reason == &NodFactoryError::SettlementCostTooLarge { cost }.to_string()),
        "unexpected error: {error:?}"
    );
}

#[test]
fn mine_gratis_charges_zk_verification_base_gas() {
    assert_eq!(
        crate::precompile::base_gas(&INodFactory::mineGratisCall::SELECTOR),
        outbe_zkproof::constants::ZK_VERIFY_GAS
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

/// Being called opens a notice period, it does not close mining: the owner is
/// meant to settle and mine inside it. The deadline itself is still inside.
#[test]
fn a_called_nod_still_mines_at_the_settlement_deadline() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x55));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);

    let called_at = 1_700_000_000;
    world.mark_called(nod_id, called_at);
    world.set_timestamp(called_at + CALL_NOTICE_PERIOD);

    let nonce = find_valid_nonce(nod_id);
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
                    paynote_proof: &[],
                },
            )
        })
        .unwrap();
    assert_eq!(minted, input.gratis_load_minor);
}

/// Past the deadline the Nod is forfeit. The daily sweep burns it, but this gate
/// closes the window between the deadline and the sweep reaching it.
#[test]
fn mining_is_rejected_once_the_settlement_deadline_has_passed() {
    let mut world = World::new();
    let input = params(Address::repeat_byte(0x55));
    let nod_id = world.issue(&input);
    world.qualify(nod_id);

    let called_at = 1_700_000_000;
    world.mark_called(nod_id, called_at);
    world.set_timestamp(called_at + CALL_NOTICE_PERIOD + 1);

    let nonce = find_valid_nonce(nod_id);
    let error = world
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
                    paynote_proof: &[],
                },
            )
        })
        .unwrap_err();
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
