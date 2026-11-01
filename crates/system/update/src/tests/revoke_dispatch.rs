use super::{block_ctx, TEST_CHAIN_ID};
use alloy_primitives::{address, Address, B256, U256};
use alloy_sol_types::{SolError, SolEvent};
use outbe_primitives::addresses::{TEE_REGISTRY_ADDRESS, UPDATE_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1;
use outbe_teeregistry::TeeRegistry;
use outbe_vote::constants::VOTING_WINDOW_BLOCKS;
use outbe_vote::handlers::{
    TargetExecutionOutcome, VoteTarget, VoteTargetContext, VoteTargetRegistry,
};
use outbe_vote::schema::{ProposalStatus, Vote};

use crate::payload::REVOKE_ENCLAVE_BINDING_ACTION_V1;
use crate::precompile::IUpdate;
use crate::vote_target::UpdateVoteTarget;

static UPDATE_VOTE_TARGET: UpdateVoteTarget = UpdateVoteTarget;
static VOTE_HANDLERS: &[&dyn VoteTarget] = &[&UPDATE_VOTE_TARGET];
static VOTE_TARGET_REGISTRY: VoteTargetRegistry = VoteTargetRegistry::new(VOTE_HANDLERS);

const PROPOSER: Address = address!("0x1111111111111111111111111111111111111111");
const VOTER_A: Address = address!("0x2222222222222222222222222222222222222222");
const VOTER_B: Address = address!("0x3333333333333333333333333333333333333333");
const VALID_UNTIL: u64 = 1_700_000_000;

fn setup_validators(storage: StorageHandle) {
    let owner = address!("0xffffffffffffffffffffffffffffffffffffffff");
    let minimum = U256::from(1_000u64);
    for (addr, seed) in [(PROPOSER, 1u8), (VOTER_A, 2), (VOTER_B, 3)] {
        let mut validator_set = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        validator_set.config_owner.write(owner).unwrap();
        validator_set.set_config_max_validators(100).unwrap();
        validator_set
            .test_seed_authoritative_min_stake(minimum)
            .unwrap();
        let mut public_key = [0u8; 48];
        public_key[0] = seed;
        validator_set
            .register_validator(owner, addr, &public_key)
            .unwrap();
        validator_set
            .test_seed_authoritative_bonded(addr, minimum)
            .unwrap();
        validator_set
            .activate_validator_via_boundary_for_test(addr)
            .unwrap();
    }
}

fn seed_active_binding(
    storage: StorageHandle,
    node_id_hash: B256,
    binding_id: B256,
    enclave_id: B256,
) {
    let registry = TeeRegistry::new(storage);
    registry
        .validator_v1_node_hash
        .write(&PROPOSER, node_id_hash)
        .unwrap();
    registry
        .v1_node_intent_hash
        .write(&node_id_hash, B256::repeat_byte(0x44))
        .unwrap();
    registry
        .v1_node_enclave_id
        .write(&node_id_hash, enclave_id)
        .unwrap();
    registry
        .v1_node_binding_id
        .write(&node_id_hash, binding_id)
        .unwrap();
    registry
        .v1_node_valid_until
        .write(&node_id_hash, VALID_UNTIL)
        .unwrap();
}

fn revoke_payload(node_id_hash: B256, binding_id: Option<B256>) -> String {
    let mut value = serde_json::json!({
        "action": REVOKE_ENCLAVE_BINDING_ACTION_V1,
        "node_id_hash": format!("{node_id_hash:#x}"),
    });
    if let Some(binding_id) = binding_id {
        value["binding_id"] = serde_json::json!(format!("{binding_id:#x}"));
    }
    value.to_string()
}

fn encoded_error(error: impl SolError) -> String {
    alloy_primitives::hex::encode_prefixed(error.abi_encode())
}

fn handle_approved(
    storage: StorageHandle,
    block_number: u64,
    proposal_id: U256,
    payload: &str,
) -> Result<TargetExecutionOutcome> {
    UpdateVoteTarget.handle_approved(
        &block_ctx(storage, block_number),
        proposal_id,
        payload.as_bytes(),
        VoteTargetContext {
            proposer: PROPOSER,
            attached_value: U256::ZERO,
            block_number,
            chain_id: TEST_CHAIN_ID,
        },
    )
}

fn approve_proposal(
    vote: &mut Vote<'_>,
    proposer: Address,
    payload: &str,
    created_height: u64,
    first_vote_height: u64,
    second_vote_height: u64,
) -> U256 {
    let proposal_id = vote
        .create_proposal(
            proposer,
            UPDATE_ADDRESS,
            payload,
            created_height,
            &VOTE_TARGET_REGISTRY,
        )
        .unwrap();
    vote.cast_vote_approve(proposal_id, VOTER_A, true, first_vote_height)
        .unwrap();
    vote.cast_vote_approve(proposal_id, VOTER_B, true, second_vote_height)
        .unwrap();
    proposal_id
}

fn proposal_status(vote: &Vote<'_>, proposal_id: U256) -> ProposalStatus {
    vote.proposals
        .get(proposal_id)
        .unwrap()
        .unwrap()
        .proposal_status()
        .unwrap()
}

fn process_begin_block(storage: StorageHandle, block_number: u64) {
    let ctx = block_ctx(storage.clone(), block_number);
    Vote::new(storage)
        .process_begin_block(&ctx, &VOTE_TARGET_REGISTRY)
        .unwrap();
}

fn revocation_events(provider: &HashMapStorageProvider) -> Vec<&alloy_primitives::Log> {
    provider
        .get_ordered_events()
        .iter()
        .filter(|log| {
            log.address == TEE_REGISTRY_ADDRESS
                && log.topics().first()
                    == Some(&ITeeRegistryV1::EnclaveBindingRevokedV1::SIGNATURE_HASH)
        })
        .collect()
}

fn governance_provider() -> HashMapStorageProvider {
    let mut provider =
        HashMapStorageProvider::new_with_chain_identity(TEST_CHAIN_ID, B256::repeat_byte(0x01));
    provider.set_block_number(1);
    provider
}

#[test]
fn vote_target_revokes_enclave_binding() {
    let node_id_hash = B256::repeat_byte(0x11);
    let binding_id = B256::repeat_byte(0x22);
    let enclave_id = B256::repeat_byte(0x33);
    let current = 100u64;
    let deadline = current + VOTING_WINDOW_BLOCKS + 1;
    let mut provider = governance_provider();

    let proposal_id = StorageHandle::enter(&mut provider, |storage| {
        setup_validators(storage.clone());
        seed_active_binding(storage.clone(), node_id_hash, binding_id, enclave_id);
        let registry = TeeRegistry::new(storage.clone());
        assert!(registry.is_validator_enclave_ready_v1(PROPOSER).unwrap());

        let mismatch = revoke_payload(node_id_hash, Some(B256::repeat_byte(0x99)));
        let outcome = handle_approved(storage.clone(), current, U256::from(1), &mismatch).unwrap();
        assert_eq!(
            outcome,
            TargetExecutionOutcome::Error {
                reason: encoded_error(ITeeRegistryV1::EnclaveBindingIdMismatch {}),
            }
        );
        assert!(registry.is_validator_enclave_ready_v1(PROPOSER).unwrap());
        assert_eq!(
            registry.v1_node_valid_until.read(&node_id_hash).unwrap(),
            VALID_UNTIL
        );

        let payload = revoke_payload(node_id_hash, Some(binding_id));
        let mut vote = Vote::new(storage);
        approve_proposal(
            &mut vote,
            PROPOSER,
            &payload,
            current,
            current + 1,
            current + 2,
        )
    });

    provider.set_block_number(deadline);
    StorageHandle::enter(&mut provider, |storage| {
        process_begin_block(storage.clone(), deadline);
        let vote = Vote::new(storage.clone());
        assert_eq!(
            proposal_status(&vote, proposal_id),
            ProposalStatus::Approved
        );
        let registry = TeeRegistry::new(storage);
        assert!(!registry.is_validator_enclave_ready_v1(PROPOSER).unwrap());
        assert_eq!(registry.v1_node_valid_until.read(&node_id_hash).unwrap(), 0);
        assert!(registry.v1_revoked_enclave.read(&enclave_id).unwrap());
        assert_eq!(
            registry.last_enclave_retirement_height_v1().unwrap(),
            deadline
        );
    });
    let events = revocation_events(&provider);
    assert_eq!(events.len(), 1);
    let decoded = ITeeRegistryV1::EnclaveBindingRevokedV1::decode_log(events[0]).unwrap();
    assert_eq!(decoded.nodeIdHash, node_id_hash);
    assert_eq!(decoded.bindingId, binding_id);
    assert_eq!(decoded.proposalId, proposal_id);
    assert_eq!(decoded.blockNumber, deadline);

    // The per-validator pending cap is one. These proposals are admitted after
    // the revocation closes, with a voting deadline of `deadline`, so the next
    // block executes them. The provider height is that later block: a conflict
    // that writes the retirement height would move it off `deadline`.
    provider.set_block_number(deadline + 1);
    let stale_created = current + 1;
    StorageHandle::enter(&mut provider, |storage| {
        let mut vote = Vote::new(storage.clone());
        let already_revoked = approve_proposal(
            &mut vote,
            PROPOSER,
            &revoke_payload(node_id_hash, Some(binding_id)),
            stale_created,
            deadline,
            deadline,
        );
        let mismatched = approve_proposal(
            &mut vote,
            VOTER_A,
            &revoke_payload(node_id_hash, Some(B256::repeat_byte(0x99))),
            stale_created,
            deadline,
            deadline,
        );
        let missing = approve_proposal(
            &mut vote,
            VOTER_B,
            &revoke_payload(B256::repeat_byte(0xab), None),
            stale_created,
            deadline,
            deadline,
        );
        process_begin_block(storage.clone(), deadline + 1);
        for proposal_id in [already_revoked, mismatched, missing] {
            assert_eq!(proposal_status(&vote, proposal_id), ProposalStatus::Error);
        }
        let registry = TeeRegistry::new(storage);
        assert!(!registry.is_validator_enclave_ready_v1(PROPOSER).unwrap());
        assert_eq!(registry.v1_node_valid_until.read(&node_id_hash).unwrap(), 0);
        assert!(registry.v1_revoked_enclave.read(&enclave_id).unwrap());
        assert_eq!(
            registry.last_enclave_retirement_height_v1().unwrap(),
            deadline
        );
    });
    assert_eq!(revocation_events(&provider).len(), 1);

    assert_zero_node_payload_is_fatal();
    assert_wide_isv_prod_id_is_fatal(node_id_hash, binding_id, enclave_id);
}

fn assert_zero_node_payload_is_fatal() {
    let mut provider = HashMapStorageProvider::new(TEST_CHAIN_ID);
    provider.clear_mutation_failure();
    let payload = revoke_payload(B256::ZERO, None);
    let outcome = StorageHandle::enter(&mut provider, |storage| {
        handle_approved(storage, 1, U256::from(9), &payload)
    });
    match outcome {
        Err(PrecompileError::RevertBytes(bytes)) => {
            assert_eq!(
                &bytes[..4],
                &IUpdate::StoredProposalPayloadInvalid::SELECTOR
            );
            let decoded =
                IUpdate::StoredProposalPayloadInvalid::abi_decode(&bytes).expect("selector");
            assert_eq!(decoded.value, "invalid vote payload");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(provider.clear_mutation_failure(), 0);
}

fn assert_wide_isv_prod_id_is_fatal(node_id_hash: B256, binding_id: B256, enclave_id: B256) {
    let mut provider = governance_provider();
    StorageHandle::enter(&mut provider, |storage| {
        seed_active_binding(storage.clone(), node_id_hash, binding_id, enclave_id);
        TeeRegistry::new(storage)
            .v1_node_isv_prod_id
            .write(&node_id_hash, u64::from(u16::MAX) + 1)
            .unwrap();
    });
    provider.clear_mutation_failure();
    let payload = revoke_payload(node_id_hash, Some(binding_id));
    let outcome = StorageHandle::enter(&mut provider, |storage| {
        handle_approved(storage, 1, U256::from(9), &payload)
    });
    assert!(
        matches!(outcome, Err(PrecompileError::Revert(_))),
        "{outcome:?}"
    );
    assert_eq!(provider.clear_mutation_failure(), 0);
    StorageHandle::enter(&mut provider, |storage| {
        let registry = TeeRegistry::new(storage);
        assert_eq!(
            registry.v1_node_valid_until.read(&node_id_hash).unwrap(),
            VALID_UNTIL
        );
        assert!(!registry.v1_revoked_enclave.read(&enclave_id).unwrap());
    });
    assert!(revocation_events(&provider).is_empty());
}
