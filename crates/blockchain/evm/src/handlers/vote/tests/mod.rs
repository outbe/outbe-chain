//! Vote target wiring tests: L2 registration and public bonded admission.

use super::*;
use alloy_sol_types::SolEvent;
use commonware_cryptography::bls12381::primitives::{ops, variant::MinSig};
use outbe_l2registry::L2RegistryContract;
use outbe_primitives::addresses::L2_REGISTRY_ADDRESS;
use outbe_primitives::addresses::VOTE_ADDRESS;
use outbe_primitives::block::BlockContext;
use outbe_primitives::stablecoin::{encode_canonical_stablecoin_create, StablecoinCreatePayload};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_stablecoinfactory::StablecoinFactoryContract;
use outbe_validatorset::contract::ValidatorSet;
use outbe_vote::constants::VOTING_WINDOW_BLOCKS;
use outbe_vote::precompile::IVote;
use outbe_vote::schema::{BondSettlement, Vote};

mod factory;

const VALIDATOR_OWNER: Address = Address::repeat_byte(0xff);
const VALIDATOR_A: Address = Address::repeat_byte(0xa1);
const VALIDATOR_B: Address = Address::repeat_byte(0xa2);
const VALIDATOR_C: Address = Address::repeat_byte(0xa3);
const VALIDATOR_D: Address = Address::repeat_byte(0xa4);

#[test]
fn l2_registry_is_an_active_validator_vote_target() {
    let target = registry()
        .lookup(outbe_primitives::addresses::L2_REGISTRY_ADDRESS)
        .expect("L2Registry vote target must be active");
    assert_eq!(target.admission(), TargetAdmission::ActiveValidatorOnly);
}

fn payload(issuer: Address) -> Vec<u8> {
    encode_canonical_stablecoin_create(&StablecoinCreatePayload {
        issuer,
        name: "Example Dollar".into(),
        ticker: "EXUSD".into(),
        iso4217: 840,
        decimals: 6,
        supply_cap: U256::from(1_000_000u64),
        policy_id: U256::from(1u64),
    })
    .unwrap()
}

fn payload_with_ticker(issuer: Address, ticker: &str) -> Vec<u8> {
    encode_canonical_stablecoin_create(&StablecoinCreatePayload {
        issuer,
        name: format!("{ticker} stablecoin"),
        ticker: ticker.into(),
        iso4217: 840,
        decimals: 6,
        supply_cap: U256::from(1_000_000u64),
        policy_id: U256::from(1u64),
    })
    .unwrap()
}

fn l2_register_payload(chain_id: u64, l1_address: Address) -> String {
    let (_, public_key) = ops::keypair::<_, MinSig>(&mut rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    let public_key = outbe_l2registry::public_key::encode(&public_key)
        .expect("fixture L2 key encodes as EIP-2537");
    serde_json::json!({
        "operation": "register",
        "chainId": chain_id,
        "l1Address": format!("{l1_address:#x}"),
        "publicKey": format!("0x{}", hex::encode(public_key)),
    })
    .to_string()
}

fn context(issuer: Address) -> VoteTargetContext {
    VoteTargetContext {
        proposer: issuer,
        attached_value: STABLECOIN_CREATE_BOND,
        block_number: 7,
        chain_id: 1,
    }
}

fn register_active_validator(storage: StorageHandle<'_>, validator: Address, seed: u8) {
    let mut validator_set = ValidatorSet::new(storage);
    validator_set.config_owner.write(VALIDATOR_OWNER).unwrap();
    validator_set.set_config_max_validators(100).unwrap();
    let mut public_key = [0u8; 48];
    public_key[0] = seed;
    validator_set
        .register_validator(VALIDATOR_OWNER, validator, &public_key)
        .unwrap();
    validator_set
        .activate_validator_via_boundary_for_test(validator)
        .unwrap();
}

fn setup_active_validators(storage: StorageHandle<'_>) {
    register_active_validator(storage.clone(), VALIDATOR_A, 1);
    register_active_validator(storage.clone(), VALIDATOR_B, 2);
    register_active_validator(storage, VALIDATOR_C, 3);
}

fn finalize_context(
    storage: StorageHandle<'_>,
    block_number: u64,
    issuer: Address,
) -> BlockRuntimeContext<'_> {
    BlockRuntimeContext::new(
        BlockContext::new(block_number, 1_700_000_000, 1, issuer, vec![issuer]),
        storage,
    )
}

#[test]
fn l2_registration_waits_for_deadline_then_applies_exactly_once() {
    const L2_CHAIN_ID: u64 = 4242;
    let l1_address = Address::repeat_byte(0x44);
    let payload = l2_register_payload(L2_CHAIN_ID, l1_address);
    let mut provider = HashMapStorageProvider::new(1);
    {
        let storage = StorageHandle::new(&mut provider);
        setup_active_validators(storage.clone());
        let mut vote = Vote::new(storage.clone());
        let proposal_id = vote
            .create_proposal(VALIDATOR_A, L2_REGISTRY_ADDRESS, &payload, 7, registry())
            .unwrap();
        vote.cast_vote_approve(proposal_id, VALIDATOR_A, true, 8)
            .unwrap();
        vote.cast_vote_approve(proposal_id, VALIDATOR_B, true, 8)
            .unwrap();

        let deadline = 7 + VOTING_WINDOW_BLOCKS;
        vote.process_begin_block(
            &finalize_context(storage.clone(), deadline, VALIDATOR_A),
            registry(),
        )
        .unwrap();
        assert_eq!(
            vote.proposals
                .get(proposal_id)
                .unwrap()
                .unwrap()
                .proposal_status()
                .unwrap(),
            ProposalStatus::Pending
        );
        assert!(!L2RegistryContract::new(storage.clone())
            .networks
            .exists(L2_CHAIN_ID)
            .unwrap());

        vote.process_begin_block(
            &finalize_context(storage.clone(), deadline + 1, VALIDATOR_A),
            registry(),
        )
        .unwrap();
        assert_eq!(
            vote.proposals
                .get(proposal_id)
                .unwrap()
                .unwrap()
                .proposal_status()
                .unwrap(),
            ProposalStatus::Approved
        );
        let record = L2RegistryContract::new(storage.clone())
            .load_network(L2_CHAIN_ID)
            .unwrap();
        assert_eq!(record.l1_address, l1_address);

        vote.process_begin_block(
            &finalize_context(storage.clone(), deadline + 2, VALIDATOR_A),
            registry(),
        )
        .unwrap();
        assert_eq!(
            L2RegistryContract::new(storage)
                .l1_to_chain
                .read(&l1_address)
                .unwrap(),
            L2_CHAIN_ID
        );
    }
    assert_eq!(
        provider
            .get_events(L2_REGISTRY_ADDRESS)
            .iter()
            .filter(|event| {
                event.topics().first()
                    == Some(
                        &outbe_l2registry::precompile::IL2Registry::L2NetworkRegistered::SIGNATURE_HASH,
                    )
            })
            .count(),
        1
    );
}

#[test]
fn conflicting_approved_l2_registration_becomes_error_without_blocking_tally() {
    const L2_CHAIN_ID: u64 = 4242;
    let l1_address = Address::repeat_byte(0x44);
    let payload = l2_register_payload(L2_CHAIN_ID, l1_address);
    let mut provider = HashMapStorageProvider::new(1);
    let storage = StorageHandle::new(&mut provider);
    setup_active_validators(storage.clone());
    let mut vote = Vote::new(storage.clone());
    let first = vote
        .create_proposal(VALIDATOR_A, L2_REGISTRY_ADDRESS, &payload, 7, registry())
        .unwrap();
    let second = vote
        .create_proposal(VALIDATOR_B, L2_REGISTRY_ADDRESS, &payload, 7, registry())
        .unwrap();
    for proposal_id in [first, second] {
        vote.cast_vote_approve(proposal_id, VALIDATOR_A, true, 8)
            .unwrap();
        vote.cast_vote_approve(proposal_id, VALIDATOR_B, true, 8)
            .unwrap();
    }

    let deadline = 7 + VOTING_WINDOW_BLOCKS;
    vote.process_begin_block(
        &finalize_context(storage.clone(), deadline + 1, VALIDATOR_A),
        registry(),
    )
    .unwrap();
    assert_eq!(
        vote.proposals
            .get(first)
            .unwrap()
            .unwrap()
            .proposal_status()
            .unwrap(),
        ProposalStatus::Approved
    );
    assert_eq!(
        vote.proposals
            .get(second)
            .unwrap()
            .unwrap()
            .proposal_status()
            .unwrap(),
        ProposalStatus::Error
    );
    assert_eq!(
        L2RegistryContract::new(storage)
            .l1_to_chain
            .read(&l1_address)
            .unwrap(),
        L2_CHAIN_ID
    );
}

#[test]
fn below_quorum_l2_registration_expires_without_registry_effects() {
    const L2_CHAIN_ID: u64 = 4242;
    let l1_address = Address::repeat_byte(0x44);
    let payload = l2_register_payload(L2_CHAIN_ID, l1_address);
    let mut provider = HashMapStorageProvider::new(1);
    let storage = StorageHandle::new(&mut provider);
    setup_active_validators(storage.clone());
    let mut vote = Vote::new(storage.clone());
    let proposal_id = vote
        .create_proposal(VALIDATOR_A, L2_REGISTRY_ADDRESS, &payload, 7, registry())
        .unwrap();
    vote.cast_vote_approve(proposal_id, VALIDATOR_A, true, 8)
        .unwrap();

    let deadline = 7 + VOTING_WINDOW_BLOCKS;
    vote.process_begin_block(
        &finalize_context(storage.clone(), deadline + 1, VALIDATOR_A),
        registry(),
    )
    .unwrap();
    assert_eq!(
        vote.proposals
            .get(proposal_id)
            .unwrap()
            .unwrap()
            .proposal_status()
            .unwrap(),
        ProposalStatus::Expired
    );
    assert!(!L2RegistryContract::new(storage)
        .networks
        .exists(L2_CHAIN_ID)
        .unwrap());
}

#[test]
fn factory_target_has_exact_bond_and_reserves_from_genesis() {
    let issuer = Address::repeat_byte(0x11);
    let raw = payload(issuer);
    let target = registry().lookup(STABLECOIN_FACTORY_ADDRESS).unwrap();
    assert_eq!(
        target.admission(),
        TargetAdmission::PublicBonded {
            amount: STABLECOIN_CREATE_BOND
        }
    );
    target.validate(&raw, context(issuer)).unwrap();
    assert!(target
        .validate(&raw, context(Address::repeat_byte(0x22)))
        .is_err());

    let mut provider = HashMapStorageProvider::new(1);
    let storage = StorageHandle::new(&mut provider);
    target
        .reserve(storage.clone(), U256::from(1u64), &raw, context(issuer))
        .unwrap();
    let factory = StablecoinFactoryContract::new(storage);
    assert!(factory.reservations.exists(U256::from(1u64)).unwrap());
}

#[test]
fn factory_public_admission_atomically_records_reservation_and_bond() {
    let issuer = Address::repeat_byte(0x11);
    let raw = payload(issuer);
    let raw = core::str::from_utf8(&raw).unwrap();
    let forced_surplus = U256::from(7u64);
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_balance(VOTE_ADDRESS, STABLECOIN_CREATE_BOND + forced_surplus);
    {
        let storage = StorageHandle::new(&mut provider);
        let mut vote = Vote::new(storage.clone());
        let proposal_id = vote
            .create_proposal_with_value(
                outbe_vote::ProposalSubmission {
                    proposer: issuer,
                    target_module: STABLECOIN_FACTORY_ADDRESS,
                    payload: raw,
                    created_height: 7,
                    attached_value: STABLECOIN_CREATE_BOND,
                },
                registry(),
            )
            .unwrap();
        assert_eq!(
            vote.proposal_bond(proposal_id).unwrap().settlement,
            BondSettlement::Unsettled
        );
        assert_eq!(vote.bond_liabilities().unwrap(), STABLECOIN_CREATE_BOND);
        assert!(StablecoinFactoryContract::new(storage)
            .reservations
            .exists(proposal_id)
            .unwrap());
    }
    assert_eq!(
        provider.get_balance(VOTE_ADDRESS),
        STABLECOIN_CREATE_BOND + forced_surplus
    );

    let mut mismatch_provider = HashMapStorageProvider::new(1);
    mismatch_provider.set_balance(VOTE_ADDRESS, STABLECOIN_CREATE_BOND);
    let mismatch_storage = StorageHandle::new(&mut mismatch_provider);
    let mut vote = Vote::new(mismatch_storage.clone());
    assert!(vote
        .create_proposal_with_value(
            outbe_vote::ProposalSubmission {
                proposer: Address::repeat_byte(0x22),
                target_module: STABLECOIN_FACTORY_ADDRESS,
                payload: raw,
                created_height: 7,
                attached_value: STABLECOIN_CREATE_BOND,
            },
            registry(),
        )
        .is_err());
    assert_eq!(vote.proposal_count.read().unwrap(), U256::ZERO);
    assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
    assert!(!StablecoinFactoryContract::new(mismatch_storage)
        .reservations
        .exists(U256::from(1u64))
        .unwrap());
}
