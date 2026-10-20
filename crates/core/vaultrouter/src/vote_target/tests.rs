//! VaultRouter vote target: payload validation, approval in the tally hook,
//! and `applyApproved`.

use alloy_primitives::{address, Bytes, LogData};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_primitives::block::BlockContext;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_vote::handlers::TargetElectorate;

use super::*;
use crate::api::IVaultRouter;
use crate::precompile::dispatch;
use crate::schema::VaultRouterContract;
use crate::sol_ext::{IReferenceCurrency, IVaultV2};

const CHAIN_ID: u64 = 1;
const USD_ISO_CODE: u16 = 840;
const APPROVED_AT: u64 = 30;

const VAULT: Address = address!("0x0000000000000000000000000000000000000777");
const OTHER_VAULT: Address = address!("0x0000000000000000000000000000000000000778");
const ASSET: Address = address!("0x0000000000000000000000000000000000000888");
const SOURCE: Address = address!("0x0000000000000000000000000000000000000111");
const TARGET: Address = address!("0x0000000000000000000000000000000000000222");
const ANYONE: Address = address!("0x0000000000000000000000000000000000000b0b");

fn context() -> VoteTargetContext {
    VoteTargetContext {
        proposer: Address::repeat_byte(0xa1),
        attached_value: U256::ZERO,
        block_number: 7,
        chain_id: CHAIN_ID,
    }
}

fn add_vault(vault: Address) -> VaultRouterVoteAction {
    VaultRouterVoteAction::AddVault {
        vault,
        asset: ASSET,
        iso_code: USD_ISO_CODE,
    }
}

fn word(value: U256) -> Bytes {
    Bytes::from(value.to_be_bytes::<32>().to_vec())
}

/// Stubs `vault` as an ownerless ERC-4626 vault of `ASSET`, a USD asset.
fn stub_registrable_vault(provider: &mut HashMapStorageProvider, vault: Address) {
    provider.stub_sub_call_at_selector(
        vault,
        IVaultV2::assetCall::SELECTOR,
        word(U256::from_be_bytes(ASSET.into_word().0)),
    );
    provider.stub_sub_call_at_selector(vault, IVaultV2::ownerCall::SELECTOR, word(U256::ZERO));
    provider.stub_sub_call_at_selector(
        ASSET,
        IReferenceCurrency::isoCodeCall::SELECTOR,
        word(U256::from(USD_ISO_CODE)),
    );
    provider.enable_sub_call_stub();
}

/// Runs the tally hook for an approved `action` of `proposal_id` at `APPROVED_AT`.
fn approve(
    storage: &StorageHandle<'_>,
    proposal_id: u64,
    action: VaultRouterVoteAction,
) -> TargetExecutionOutcome {
    let ctx = BlockRuntimeContext::new(
        BlockContext::empty_for_tests(APPROVED_AT, 1_700_000_000, CHAIN_ID),
        storage.clone(),
    );
    VaultRouterVoteTarget
        .handle_approved(
            &ctx,
            U256::from(proposal_id),
            action.to_json().as_bytes(),
            context(),
        )
        .unwrap()
}

fn apply(storage: &StorageHandle<'_>, proposal_id: u64) -> Result<Bytes> {
    dispatch(
        storage.clone(),
        &IVaultRouter::applyApprovedCall {
            proposalId: U256::from(proposal_id),
        }
        .abi_encode(),
        ANYONE,
        U256::ZERO,
    )
}

/// `approvedVaultAction` of `proposal_id` as `(action, vault, approvedAt)`.
fn pending(storage: &StorageHandle<'_>, proposal_id: u64) -> (u8, Address, u64) {
    let out = dispatch(
        storage.clone(),
        &IVaultRouter::approvedVaultActionCall {
            proposalId: U256::from(proposal_id),
        }
        .abi_encode(),
        ANYONE,
        U256::ZERO,
    )
    .unwrap();
    let record = IVaultRouter::approvedVaultActionCall::abi_decode_returns(&out).unwrap();
    (record.action as u8, record.vault, record.approvedAt)
}

fn decode_events<E: SolEvent>(events: &[LogData]) -> Vec<E> {
    events
        .iter()
        .filter_map(|event| E::decode_log_data(event).ok())
        .collect()
}

fn expiry_window() -> u64 {
    outbe_chain_constants::get_governance_voting_window_blocks()
}

#[test]
fn target_is_the_vault_router_with_default_admission_and_electorate() {
    let target = VaultRouterVoteTarget;
    assert_eq!(target.target_module(), VAULT_ROUTER_ADDRESS);
    assert_eq!(
        target.admission(),
        outbe_vote::handlers::TargetAdmission::ActiveValidatorOnly
    );
    assert_eq!(target.electorate(), TargetElectorate::Active);
}

#[test]
fn every_action_round_trips_through_its_json_payload() {
    let actions = [
        add_vault(VAULT),
        VaultRouterVoteAction::RemoveVault { vault: VAULT },
        VaultRouterVoteAction::AddLiquiditySource {
            source: SOURCE,
            source_type: StablesSource::NodCostAmount as u8,
        },
        VaultRouterVoteAction::RemoveLiquiditySource { source: SOURCE },
        VaultRouterVoteAction::AddLiquidityTarget {
            target: TARGET,
            target_type: StablesTarget::Credis as u8,
        },
        VaultRouterVoteAction::RemoveLiquidityTarget { target: TARGET },
    ];
    for action in actions {
        let payload = action.to_json();
        assert_eq!(
            VaultRouterVoteAction::decode(payload.as_bytes()).unwrap(),
            action
        );
        VaultRouterVoteTarget
            .validate(payload.as_bytes(), context())
            .unwrap();
    }
    assert_eq!(
        VaultRouterVoteAction::decode(
            br#"{"action":"addLiquiditySource","address":"0x0000000000000000000000000000000000000111","type":1}"#
        )
        .unwrap(),
        VaultRouterVoteAction::AddLiquiditySource {
            source: SOURCE,
            source_type: 1
        }
    );
}

#[test]
fn validate_rejects_malformed_payloads_zero_addresses_and_unknown_types() {
    let vault = "0x0000000000000000000000000000000000000777";
    let asset = "0x0000000000000000000000000000000000000888";
    let zero = "0x0000000000000000000000000000000000000000";
    let rejects = [
        ("not json".to_owned(), "invalid proposal payload"),
        ("[]".to_owned(), "invalid VaultRouter proposal payload"),
        (
            format!(r#"{{"action":"setOwner","vault":"{vault}"}}"#),
            "invalid VaultRouter proposal payload",
        ),
        (
            format!(r#"{{"vault":"{vault}"}}"#),
            "invalid VaultRouter proposal payload",
        ),
        (
            format!(r#"{{"action":"removeVault","vault":"{vault}","asset":"{asset}"}}"#),
            "invalid VaultRouter proposal payload",
        ),
        (
            r#"{"action":"removeVault","vault":"0x1234"}"#.to_owned(),
            "invalid VaultRouter proposal payload",
        ),
        (
            format!(r#"{{"action":"addVault","vault":"{vault}","asset":"{asset}"}}"#),
            "invalid VaultRouter proposal payload",
        ),
        (
            format!(r#"{{"action":"removeVault","vault":"{zero}"}}"#),
            "zero address",
        ),
        (
            format!(r#"{{"action":"addVault","vault":"{zero}","asset":"{asset}","isoCode":840}}"#),
            "zero address",
        ),
        (
            format!(r#"{{"action":"addVault","vault":"{vault}","asset":"{zero}","isoCode":840}}"#),
            "zero address",
        ),
        (
            format!(r#"{{"action":"addVault","vault":"{vault}","asset":"{asset}","isoCode":0}}"#),
            "Invalid currency code 0",
        ),
        (
            format!(r#"{{"action":"addLiquiditySource","address":"{zero}","type":1}}"#),
            "zero address",
        ),
        (
            format!(r#"{{"action":"addLiquiditySource","address":"{vault}","type":0}}"#),
            "invalid liquidity source",
        ),
        (
            format!(r#"{{"action":"addLiquiditySource","address":"{vault}","type":9}}"#),
            "invalid liquidity source",
        ),
        (
            format!(r#"{{"action":"addLiquiditySource","address":"{vault}","type":256}}"#),
            "invalid VaultRouter proposal payload",
        ),
        (
            format!(r#"{{"action":"addLiquidityTarget","address":"{vault}","type":0}}"#),
            "invalid liquidity target",
        ),
        (
            format!(r#"{{"action":"addLiquidityTarget","address":"{vault}","type":2}}"#),
            "invalid liquidity target",
        ),
        (
            format!(r#"{{"action":"removeLiquidityTarget","address":"{zero}"}}"#),
            "zero address",
        ),
    ];
    for (payload, expected) in rejects {
        let err = VaultRouterVoteTarget
            .validate(payload.as_bytes(), context())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(expected),
            "{payload}: expected {expected:?}, got {err}"
        );
    }
}

#[test]
fn approved_vault_action_is_recorded_in_the_hook_without_sub_calls() {
    // No sub-call stubs: a sub-call in the hook would fail.
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            approve(&storage, 1, add_vault(VAULT)),
            TargetExecutionOutcome::Applied
        );

        assert_eq!(
            pending(&storage, 1),
            (VaultAction::AddVault as u8, VAULT, APPROVED_AT)
        );
        let contract = VaultRouterContract::new(storage.clone());
        assert_eq!(
            contract.pending_vault_actions.read(&VAULT).unwrap(),
            U256::ONE
        );
        assert_eq!(contract.assets.len().unwrap(), 0);
        assert_eq!(contract.asset_vault_set(ASSET).len().unwrap(), 0);
    });
    let approved = decode_events::<IVaultRouter::VaultActionApproved>(
        provider.get_events(VAULT_ROUTER_ADDRESS),
    );
    assert_eq!(approved.len(), 1);
    assert_eq!(
        (
            approved[0].proposalId,
            approved[0].vault,
            approved[0].action as u8
        ),
        (U256::ONE, VAULT, VaultAction::AddVault as u8)
    );
}

#[test]
fn apply_approved_add_vault_registers_the_vault_like_add_vault() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    stub_registrable_vault(&mut provider, VAULT);
    provider.set_block_number(APPROVED_AT + 1);
    StorageHandle::enter(&mut provider, |storage| {
        approve(&storage, 1, add_vault(VAULT));
        apply(&storage, 1).unwrap();

        let contract = VaultRouterContract::new(storage.clone());
        assert_eq!(contract.assets.read_all().unwrap(), vec![ASSET]);
        assert_eq!(
            contract.asset_vault_set(ASSET).read_all().unwrap(),
            vec![VAULT]
        );
        assert_eq!(
            contract
                .reference_currency_vault_set(USD_ISO_CODE)
                .read_all()
                .unwrap(),
            vec![VAULT]
        );
        assert_eq!(
            contract.vault_reference_currencies.read(&VAULT).unwrap(),
            USD_ISO_CODE
        );

        // The action is single-use.
        assert_eq!(pending(&storage, 1).0, VaultAction::Unknown as u8);
        assert!(contract
            .pending_vault_actions
            .read(&VAULT)
            .unwrap()
            .is_zero());
        let err = apply(&storage, 1).unwrap_err().to_string();
        assert!(err.contains("approved vault action not found: 1"), "{err}");

        // Slot 0 held the owner. Nothing writes it now.
        assert_eq!(
            storage.sload(VAULT_ROUTER_ADDRESS, U256::ZERO).unwrap(),
            U256::ZERO
        );
        assert_eq!(contract.retired_owner.read().unwrap(), Address::ZERO);
    });
    let added =
        decode_events::<IVaultRouter::VaultAdded>(provider.get_events(VAULT_ROUTER_ADDRESS));
    assert_eq!(added.len(), 1);
    assert_eq!(
        (added[0].isoCode, added[0].asset, added[0].vault),
        (USD_ISO_CODE, ASSET, VAULT)
    );
}

#[test]
fn apply_approved_remove_vault_deregisters_the_vault() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    stub_registrable_vault(&mut provider, VAULT);
    StorageHandle::enter(&mut provider, |storage| {
        approve(&storage, 1, add_vault(VAULT));
        apply(&storage, 1).unwrap();
        approve(
            &storage,
            2,
            VaultRouterVoteAction::RemoveVault { vault: VAULT },
        );
        assert_eq!(pending(&storage, 2).0, VaultAction::RemoveVault as u8);
        apply(&storage, 2).unwrap();

        let contract = VaultRouterContract::new(storage.clone());
        assert_eq!(contract.assets.len().unwrap(), 0);
        assert_eq!(contract.asset_vault_set(ASSET).len().unwrap(), 0);
        assert_eq!(
            contract
                .reference_currency_vault_set(USD_ISO_CODE)
                .len()
                .unwrap(),
            0
        );
        assert_eq!(contract.vault_reference_currencies.read(&VAULT).unwrap(), 0);
        assert_eq!(pending(&storage, 2).0, VaultAction::Unknown as u8);
    });
    let removed =
        decode_events::<IVaultRouter::VaultRemoved>(provider.get_events(VAULT_ROUTER_ADDRESS));
    assert_eq!(removed.len(), 1);
    assert_eq!(
        (removed[0].isoCode, removed[0].asset, removed[0].vault),
        (USD_ISO_CODE, ASSET, VAULT)
    );
}

#[test]
fn reverted_apply_keeps_the_action_retriable() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    stub_registrable_vault(&mut provider, VAULT);
    // The vault still has an owner, so `add_vault` rejects it.
    provider.stub_sub_call_at_selector(
        VAULT,
        IVaultV2::ownerCall::SELECTOR,
        word(U256::from_be_bytes(ANYONE.into_word().0)),
    );
    StorageHandle::enter(&mut provider, |storage| {
        approve(&storage, 1, add_vault(VAULT));
        let err = apply(&storage, 1).unwrap_err().to_string();
        assert!(err.contains("owner not renounced"), "{err}");
        assert_eq!(pending(&storage, 1).0, VaultAction::AddVault as u8);
        assert_eq!(
            VaultRouterContract::new(storage.clone())
                .pending_vault_actions
                .read(&VAULT)
                .unwrap(),
            U256::ONE
        );
    });

    provider.stub_sub_call_at_selector(VAULT, IVaultV2::ownerCall::SELECTOR, word(U256::ZERO));
    StorageHandle::enter(&mut provider, |storage| {
        apply(&storage, 1).unwrap();
        let contract = VaultRouterContract::new(storage.clone());
        assert_eq!(
            contract.asset_vault_set(ASSET).read_all().unwrap(),
            vec![VAULT]
        );
        assert_eq!(pending(&storage, 1).0, VaultAction::Unknown as u8);
    });
}

#[test]
fn expired_action_is_deleted_without_applying_and_without_reverting() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    stub_registrable_vault(&mut provider, VAULT);
    stub_registrable_vault(&mut provider, OTHER_VAULT);
    provider.set_block_number(APPROVED_AT + expiry_window() + 1);
    StorageHandle::enter(&mut provider, |storage| {
        approve(&storage, 1, add_vault(VAULT));
        apply(&storage, 1).unwrap();

        let contract = VaultRouterContract::new(storage.clone());
        assert_eq!(contract.assets.len().unwrap(), 0);
        assert_eq!(contract.asset_vault_set(ASSET).len().unwrap(), 0);
        assert_eq!(pending(&storage, 1).0, VaultAction::Unknown as u8);
        assert!(contract
            .pending_vault_actions
            .read(&VAULT)
            .unwrap()
            .is_zero());
        let err = apply(&storage, 1).unwrap_err().to_string();
        assert!(err.contains("approved vault action not found"), "{err}");
    });
    let events = provider.get_events(VAULT_ROUTER_ADDRESS);
    let expired = decode_events::<IVaultRouter::VaultActionExpired>(events);
    assert_eq!(expired.len(), 1);
    assert_eq!(
        (
            expired[0].proposalId,
            expired[0].vault,
            expired[0].action as u8
        ),
        (U256::ONE, VAULT, VaultAction::AddVault as u8)
    );
    assert!(decode_events::<IVaultRouter::VaultAdded>(events).is_empty());

    // The last block of the window still applies.
    provider.set_block_number(APPROVED_AT + expiry_window());
    StorageHandle::enter(&mut provider, |storage| {
        approve(&storage, 2, add_vault(OTHER_VAULT));
        apply(&storage, 2).unwrap();
        assert_eq!(
            VaultRouterContract::new(storage.clone())
                .asset_vault_set(ASSET)
                .read_all()
                .unwrap(),
            vec![OTHER_VAULT]
        );
    });
}

#[test]
fn newer_approval_on_a_vault_supersedes_its_pending_action() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    stub_registrable_vault(&mut provider, VAULT);
    stub_registrable_vault(&mut provider, OTHER_VAULT);
    StorageHandle::enter(&mut provider, |storage| {
        approve(&storage, 1, add_vault(VAULT));
        approve(&storage, 2, add_vault(OTHER_VAULT));
        approve(
            &storage,
            3,
            VaultRouterVoteAction::RemoveVault { vault: VAULT },
        );

        assert_eq!(pending(&storage, 1).0, VaultAction::Unknown as u8);
        assert_eq!(pending(&storage, 2).0, VaultAction::AddVault as u8);
        assert_eq!(pending(&storage, 3).0, VaultAction::RemoveVault as u8);
        let contract = VaultRouterContract::new(storage.clone());
        assert_eq!(
            contract.pending_vault_actions.read(&VAULT).unwrap(),
            U256::from(3)
        );
        assert_eq!(
            contract.pending_vault_actions.read(&OTHER_VAULT).unwrap(),
            U256::from(2)
        );

        // The stale AddVault cannot be applied after the RemoveVault vote.
        let err = apply(&storage, 1).unwrap_err().to_string();
        assert!(err.contains("approved vault action not found: 1"), "{err}");
        let err = apply(&storage, 3).unwrap_err().to_string();
        assert!(err.contains("reserve vault not found"), "{err}");
        apply(&storage, 2).unwrap();
        assert_eq!(
            contract.asset_vault_set(ASSET).read_all().unwrap(),
            vec![OTHER_VAULT]
        );
    });
    let superseded = decode_events::<IVaultRouter::VaultActionSuperseded>(
        provider.get_events(VAULT_ROUTER_ADDRESS),
    );
    assert_eq!(superseded.len(), 1);
    assert_eq!(
        (
            superseded[0].proposalId,
            superseded[0].vault,
            superseded[0].supersededBy
        ),
        (U256::ONE, VAULT, U256::from(3))
    );
}

#[test]
fn liquidity_actions_apply_on_approval() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        let actions = [
            VaultRouterVoteAction::AddLiquiditySource {
                source: SOURCE,
                source_type: StablesSource::IntexCostAmount as u8,
            },
            VaultRouterVoteAction::AddLiquidityTarget {
                target: TARGET,
                target_type: StablesTarget::Credis as u8,
            },
        ];
        for (id, action) in (1..).zip(actions) {
            assert_eq!(
                approve(&storage, id, action),
                TargetExecutionOutcome::Applied
            );
        }
        assert!(matches!(
            runtime::registered_liquidity_source(&storage, SOURCE).unwrap(),
            StablesSource::IntexCostAmount
        ));
        assert!(matches!(
            runtime::registered_liquidity_target(&storage, TARGET).unwrap(),
            StablesTarget::Credis
        ));

        let actions = [
            VaultRouterVoteAction::RemoveLiquiditySource { source: SOURCE },
            VaultRouterVoteAction::RemoveLiquidityTarget { target: TARGET },
        ];
        for (id, action) in (3..).zip(actions) {
            assert_eq!(
                approve(&storage, id, action),
                TargetExecutionOutcome::Applied
            );
        }
        let contract = VaultRouterContract::new(storage.clone());
        assert_eq!(contract.liquidity_sources.len().unwrap(), 0);
        assert_eq!(contract.liquidity_targets.len().unwrap(), 0);

        // A route that is already gone makes the proposal an Error, not a fault.
        let outcome = approve(
            &storage,
            5,
            VaultRouterVoteAction::RemoveLiquiditySource { source: SOURCE },
        );
        assert_eq!(
            outcome,
            TargetExecutionOutcome::Error {
                reason: "liquidity source not found".into()
            }
        );
    });
    let events = provider.get_events(VAULT_ROUTER_ADDRESS);
    assert_eq!(events.len(), 4);
    let added = IVaultRouter::LiquiditySourceAdded::decode_log_data(&events[0]).unwrap();
    assert_eq!(
        (added.sourceAddress, added.sourceType as u8),
        (SOURCE, StablesSource::IntexCostAmount as u8)
    );
    let added = IVaultRouter::LiquidityTargetAdded::decode_log_data(&events[1]).unwrap();
    assert_eq!(
        (added.targetAddress, added.targetType as u8),
        (TARGET, StablesTarget::Credis as u8)
    );
    let removed = IVaultRouter::LiquiditySourceRemoved::decode_log_data(&events[2]).unwrap();
    assert_eq!(
        (removed.sourceAddress, removed.sourceType as u8),
        (SOURCE, StablesSource::IntexCostAmount as u8)
    );
    let removed = IVaultRouter::LiquidityTargetRemoved::decode_log_data(&events[3]).unwrap();
    assert_eq!(
        (removed.targetAddress, removed.targetType as u8),
        (TARGET, StablesTarget::Credis as u8)
    );
}

#[test]
fn malformed_stored_payload_is_fatal_and_invalid_action_is_an_error() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(APPROVED_AT, 1_700_000_000, CHAIN_ID),
            storage.clone(),
        );
        assert!(VaultRouterVoteTarget
            .handle_approved(&ctx, U256::ONE, b"not json", context())
            .is_err());
        assert!(matches!(
            VaultRouterVoteTarget
                .handle_approved(&ctx, U256::ONE, br#"{"action":"setOwner"}"#, context())
                .unwrap(),
            TargetExecutionOutcome::Error { .. }
        ));
        assert_eq!(pending(&storage, 1).0, VaultAction::Unknown as u8);
    });
}
