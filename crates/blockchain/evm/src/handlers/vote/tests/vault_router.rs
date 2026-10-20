//! VaultRouter vote target wiring: validator votes change vaults and routes.

use super::*;
use alloy_primitives::Bytes;
use alloy_sol_types::SolCall;
use outbe_primitives::addresses::{CREDIS_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_vaultrouter::api::IVaultRouter;
use outbe_vaultrouter::{VaultRouterContract, VaultRouterVoteAction};
use outbe_vote::handlers::TargetElectorate;

const VAULT: Address = Address::repeat_byte(0x57);
const ASSET: Address = Address::repeat_byte(0x58);
const USD_ISO_CODE: u16 = 840;

fn selector(signature: &str) -> [u8; 4] {
    alloy_primitives::keccak256(signature.as_bytes())[..4]
        .try_into()
        .unwrap()
}

fn word(value: U256) -> Bytes {
    Bytes::from(value.to_be_bytes::<32>().to_vec())
}

/// Stubs `VAULT` as an ownerless vault of `ASSET`, a USD asset.
fn stub_registrable_vault(provider: &mut HashMapStorageProvider) {
    provider.stub_sub_call_at_selector(
        VAULT,
        selector("asset()"),
        word(U256::from_be_bytes(ASSET.into_word().0)),
    );
    provider.stub_sub_call_at_selector(VAULT, selector("owner()"), word(U256::ZERO));
    provider.stub_sub_call_at_selector(
        ASSET,
        selector("isoCode()"),
        word(U256::from(USD_ISO_CODE)),
    );
    provider.enable_sub_call_stub();
}

fn add_vault_payload() -> String {
    VaultRouterVoteAction::AddVault {
        vault: VAULT,
        asset: ASSET,
        iso_code: USD_ISO_CODE,
    }
    .to_json()
}

/// Creates a proposal of `VALIDATOR_A`, has `voters` approve it, and runs the
/// tally after its deadline. Returns the proposal id and its final status.
fn vote_on(
    storage: StorageHandle<'_>,
    payload: &str,
    voters: &[Address],
) -> (U256, ProposalStatus) {
    let mut vote = Vote::new(storage.clone());
    let proposal_id = vote
        .create_proposal(VALIDATOR_A, VAULT_ROUTER_ADDRESS, payload, 7, registry())
        .unwrap();
    for voter in voters {
        vote.cast_vote_approve(proposal_id, *voter, true, 8)
            .unwrap();
    }
    vote.process_begin_block(
        &finalize_context(storage, 7 + VOTING_WINDOW_BLOCKS + 1, VALIDATOR_A),
        registry(),
    )
    .unwrap();
    let status = vote
        .proposals
        .get(proposal_id)
        .unwrap()
        .unwrap()
        .proposal_status()
        .unwrap();
    (proposal_id, status)
}

fn approved_action(storage: StorageHandle<'_>, proposal_id: U256) -> (u8, Address) {
    let out = outbe_vaultrouter::precompile::dispatch(
        storage,
        &IVaultRouter::approvedVaultActionCall {
            proposalId: proposal_id,
        }
        .abi_encode(),
        VALIDATOR_D,
        U256::ZERO,
    )
    .unwrap();
    let record = IVaultRouter::approvedVaultActionCall::abi_decode_returns(&out).unwrap();
    (record.action as u8, record.vault)
}

#[test]
fn vault_router_is_an_active_validator_vote_target() {
    let target = registry()
        .lookup(VAULT_ROUTER_ADDRESS)
        .expect("VaultRouter vote target must be active");
    assert_eq!(target.target_module(), VAULT_ROUTER_ADDRESS);
    assert_eq!(target.admission(), TargetAdmission::ActiveValidatorOnly);
    assert_eq!(target.electorate(), TargetElectorate::Active);
}

#[test]
fn only_active_validators_propose_valid_vault_router_changes() {
    let mut provider = HashMapStorageProvider::new(1);
    let storage = StorageHandle::new(&mut provider);
    setup_active_validators(storage.clone());
    let mut vote = Vote::new(storage);
    assert!(vote
        .create_proposal(
            VALIDATOR_D,
            VAULT_ROUTER_ADDRESS,
            &add_vault_payload(),
            7,
            registry()
        )
        .is_err());
    let zero_vault = format!(
        r#"{{"action":"removeVault","vault":"{:#x}"}}"#,
        Address::ZERO
    );
    assert!(vote
        .create_proposal(
            VALIDATOR_A,
            VAULT_ROUTER_ADDRESS,
            &zero_vault,
            7,
            registry()
        )
        .is_err());
    assert_eq!(vote.proposal_count.read().unwrap(), U256::ZERO);
}

#[test]
fn passed_add_vault_vote_then_apply_approved_registers_the_vault() {
    let mut provider = HashMapStorageProvider::new(1);
    stub_registrable_vault(&mut provider);
    let storage = StorageHandle::new(&mut provider);
    setup_active_validators(storage.clone());
    let (proposal_id, status) = vote_on(
        storage.clone(),
        &add_vault_payload(),
        &[VALIDATOR_A, VALIDATOR_B],
    );
    assert_eq!(status, ProposalStatus::Approved);

    // The tally only records the action. The vault is not registered yet.
    let router = VaultRouterContract::new(storage.clone());
    assert_eq!(router.asset_vault_set(ASSET).len().unwrap(), 0);
    assert_eq!(
        approved_action(storage.clone(), proposal_id),
        (IVaultRouter::VaultAction::AddVault as u8, VAULT)
    );

    // Anyone applies it in a normal call, where sub-calls work.
    outbe_vaultrouter::precompile::dispatch(
        storage.clone(),
        &IVaultRouter::applyApprovedCall {
            proposalId: proposal_id,
        }
        .abi_encode(),
        VALIDATOR_D,
        U256::ZERO,
    )
    .unwrap();
    assert_eq!(
        router.asset_vault_set(ASSET).read_all().unwrap(),
        vec![VAULT]
    );
    assert_eq!(
        router
            .reference_currency_vault_set(USD_ISO_CODE)
            .read_all()
            .unwrap(),
        vec![VAULT]
    );
    assert_eq!(
        approved_action(storage, proposal_id),
        (IVaultRouter::VaultAction::Unknown as u8, Address::ZERO)
    );
}

#[test]
fn failed_vault_router_vote_changes_nothing() {
    let mut provider = HashMapStorageProvider::new(1);
    let storage = StorageHandle::new(&mut provider);
    setup_active_validators(storage.clone());
    let (proposal_id, status) = vote_on(storage.clone(), &add_vault_payload(), &[VALIDATOR_A]);
    assert_eq!(status, ProposalStatus::Expired);
    assert_eq!(
        approved_action(storage.clone(), proposal_id),
        (IVaultRouter::VaultAction::Unknown as u8, Address::ZERO)
    );
    let router = VaultRouterContract::new(storage);
    assert!(router.pending_vault_actions.read(&VAULT).unwrap().is_zero());
    assert_eq!(router.assets.len().unwrap(), 0);
}

#[test]
fn passed_liquidity_target_vote_applies_in_the_tally() {
    let mut provider = HashMapStorageProvider::new(1);
    let storage = StorageHandle::new(&mut provider);
    setup_active_validators(storage.clone());
    let payload = VaultRouterVoteAction::AddLiquidityTarget {
        target: CREDIS_FACTORY_ADDRESS,
        target_type: IVaultRouter::StablesTarget::Credis as u8,
    }
    .to_json();
    let (_, status) = vote_on(storage.clone(), &payload, &[VALIDATOR_A, VALIDATOR_B]);
    assert_eq!(status, ProposalStatus::Approved);
    assert!(matches!(
        outbe_vaultrouter::runtime::registered_liquidity_target(&storage, CREDIS_FACTORY_ADDRESS)
            .unwrap(),
        IVaultRouter::StablesTarget::Credis
    ));

    // Removing a route that does not exist makes the proposal an Error.
    let payload = VaultRouterVoteAction::RemoveLiquiditySource {
        source: CREDIS_FACTORY_ADDRESS,
    }
    .to_json();
    let (_, status) = vote_on(storage, &payload, &[VALIDATOR_A, VALIDATOR_B]);
    assert_eq!(status, ProposalStatus::Error);
}
