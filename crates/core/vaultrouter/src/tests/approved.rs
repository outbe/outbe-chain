use super::*;
use crate::state::{self, ADD_VAULT, REMOVE_VAULT};

fn stub_vault(provider: &mut HashMapStorageProvider) {
    provider.stub_sub_call_at_selector(vault(), IVaultV2::assetCall::SELECTOR, word_addr(asset()));
    provider.stub_sub_call_at_selector(vault(), IVaultV2::ownerCall::SELECTOR, word(U256::ZERO));
    provider.stub_sub_call_at_selector(
        asset(),
        IReferenceCurrency::isoCodeCall::SELECTOR,
        word(U256::from(USD_ISO_CODE)),
    );
    provider.enable_sub_call_stub();
}

fn stage(storage: &StorageHandle<'_>, id: u64, kind: u8, height: u64) {
    state::record_approved_vault_action(storage, U256::from(id), vault(), kind, height).unwrap();
}

fn apply(storage: &StorageHandle<'_>, id: u64) -> outbe_primitives::error::Result<Bytes> {
    dispatch(
        storage.clone(),
        &IVaultRouter::applyApprovedCall {
            proposalId: U256::from(id),
        }
        .abi_encode(),
        stranger(),
        U256::ZERO,
    )
}

fn pending(
    storage: &StorageHandle<'_>,
    id: u64,
) -> outbe_primitives::error::Result<IVaultRouter::approvedVaultActionReturn> {
    let output = dispatch(
        storage.clone(),
        &IVaultRouter::approvedVaultActionCall {
            proposalId: U256::from(id),
        }
        .abi_encode(),
        stranger(),
        U256::ZERO,
    )?;
    Ok(IVaultRouter::approvedVaultActionCall::abi_decode_returns(&output).unwrap())
}

#[test]
fn pending_view_tracks_supersession_application_and_expiry_cleanup() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    stub_vault(&mut provider);
    StorageHandle::enter(&mut provider, |storage| {
        let missing = pending(&storage, 1).unwrap();
        assert!(matches!(missing.action, IVaultRouter::VaultAction::Unknown));
        assert_eq!((missing.vault, missing.approvedAt), (Address::ZERO, 0));
        stage(&storage, 1, REMOVE_VAULT, 7);
        let removal = pending(&storage, 1).unwrap();
        assert!(matches!(
            removal.action,
            IVaultRouter::VaultAction::RemoveVault
        ));
        assert_eq!((removal.vault, removal.approvedAt), (vault(), 7));
        stage(&storage, 2, ADD_VAULT, 8);
        assert!(matches!(
            pending(&storage, 1).unwrap().action,
            IVaultRouter::VaultAction::Unknown
        ));
        let addition = pending(&storage, 2).unwrap();
        assert!(matches!(
            addition.action,
            IVaultRouter::VaultAction::AddVault
        ));
        assert_eq!((addition.vault, addition.approvedAt), (vault(), 8));
        apply(&storage, 2).unwrap();
        assert!(matches!(
            pending(&storage, 2).unwrap().action,
            IVaultRouter::VaultAction::Unknown
        ));
        stage(&storage, 3, REMOVE_VAULT, 8);
    });
    provider.set_block_number(9 + outbe_chain_constants::get_governance_voting_window_blocks());
    StorageHandle::enter(&mut provider, |storage| {
        // Expiry alone does not delete a record. The view reports it until cleanup.
        assert!(matches!(
            pending(&storage, 3).unwrap().action,
            IVaultRouter::VaultAction::RemoveVault
        ));
        apply(&storage, 3).unwrap();
        assert!(matches!(
            pending(&storage, 3).unwrap().action,
            IVaultRouter::VaultAction::Unknown
        ));
    });
}

#[test]
fn pending_view_does_not_hide_corrupt_actions_or_indexes() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        stage(&storage, 1, ADD_VAULT, 0);
        let router = VaultRouterContract::new(storage.clone());
        router
            .pending_vault_proposals
            .write(&vault(), U256::from(2))
            .unwrap();
        assert_revert::<IVaultRouter::ApprovedVaultActionIndexInconsistent>(
            &pending(&storage, 1).map(|_| ()).unwrap_err(),
        );
        router
            .pending_vault_proposals
            .write(&vault(), U256::ONE)
            .unwrap();
        let mut record = router
            .approved_vault_actions
            .get(U256::ONE)
            .unwrap()
            .unwrap();
        record.kind = 3;
        router.approved_vault_actions.delete(U256::ONE).unwrap();
        router.approved_vault_actions.create(&record).unwrap();
        assert_revert::<IVaultRouter::InvalidApprovedVaultAction>(
            &pending(&storage, 1).map(|_| ()).unwrap_err(),
        );
    });
    assert!(provider.sub_call_inputs().is_empty());
    assert!(provider.get_events(VAULT_ROUTER_ADDRESS).is_empty());
}

#[test]
fn layout_preserves_old_slots_and_appends_pending_maps() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        let router = VaultRouterContract::new(storage.clone());
        router.assets.insert(asset()).unwrap();
        router.liquidity_sources.insert(source_account()).unwrap();
        router.liquidity_targets.insert(target_account()).unwrap();
        for slot in [1, 4, 7] {
            assert_eq!(
                storage
                    .sload(VAULT_ROUTER_ADDRESS, U256::from(slot))
                    .unwrap(),
                U256::ONE
            );
        }
        assert_eq!(router.asset_vaults.base_slot(), U256::from(3));
        assert_eq!(router.liquidity_source_types.base_slot(), U256::from(6));
        assert_eq!(router.liquidity_target_types.base_slot(), U256::from(9));
        assert_eq!(router.reference_currency_vaults.base_slot(), U256::from(10));
        assert_eq!(
            router.vault_reference_currencies.base_slot(),
            U256::from(11)
        );
        assert_eq!(router.reservation_nonce.slot(), U256::from(12));
        assert_eq!(router.reservations.base_slot(), U256::from(13));
        assert_eq!(router.vault_assets.base_slot(), U256::from(29));
        assert_eq!(router.approved_vault_actions.base_slot(), U256::from(30));
        assert_eq!(router.pending_vault_proposals.base_slot(), U256::from(33));
        assert_eq!(
            storage.sload(VAULT_ROUTER_ADDRESS, U256::ZERO).unwrap(),
            U256::ZERO
        );
    });
}

#[test]
fn approvals_do_not_overwrite_reservations_with_the_same_id() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    stub_vault(&mut provider);
    StorageHandle::enter(&mut provider, |storage| {
        let router = VaultRouterContract::new(storage.clone());
        let reservation = LiquidityReservation {
            id: U256::ONE,
            asset: asset(),
            amount_minor: U256::from(123),
            credis_owner: owner(),
            source: source_account(),
            ..Default::default()
        };
        router.reservations.create(&reservation).unwrap();
        stage(&storage, 1, ADD_VAULT, 0);
        assert_eq!(
            router.reservations.get(U256::ONE).unwrap(),
            Some(reservation.clone())
        );
        apply(&storage, 1).unwrap();
        assert_eq!(
            router.reservations.get(U256::ONE).unwrap(),
            Some(reservation)
        );
    });
}

#[test]
fn unrelated_caller_can_apply_add_then_remove_once() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    stub_vault(&mut provider);
    StorageHandle::enter(&mut provider, |storage| {
        stage(&storage, 1, ADD_VAULT, 0);
        apply(&storage, 1).unwrap();
        let router = VaultRouterContract::new(storage.clone());
        assert!(router.asset_vault_set(asset()).contains(&vault()).unwrap());
        assert!(router
            .reference_currency_vault_set(USD_ISO_CODE)
            .contains(&vault())
            .unwrap());
        assert_eq!(
            router.pending_vault_proposals.read(&vault()).unwrap(),
            U256::ZERO
        );
        assert!(apply(&storage, 1).is_err());
        stage(&storage, 2, REMOVE_VAULT, 0);
        apply(&storage, 2).unwrap();
        assert!(router.assets.is_empty().unwrap());
        assert!(router
            .reference_currency_vault_set(USD_ISO_CODE)
            .is_empty()
            .unwrap());
        assert_eq!(router.vault_reference_currencies.read(&vault()).unwrap(), 0);
        assert!(apply(&storage, 2).is_err());
    });
    let events = provider.get_events(VAULT_ROUTER_ADDRESS);
    assert_eq!(events.len(), 2);
    IVaultRouter::VaultAdded::decode_log_data(&events[0]).unwrap();
    IVaultRouter::VaultRemoved::decode_log_data(&events[1]).unwrap();
}

#[test]
fn apply_uses_live_metadata_not_informational_proposal_declarations() {
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
    use outbe_vote::handlers::{TargetExecutionOutcome, VoteTarget, VoteTargetContext};

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    stub_vault(&mut provider);
    StorageHandle::enter(&mut provider, |storage| {
        let payload = format!(
            r#"{{"operation":"addVault","vault":"{:#x}","asset":"{:#x}","isoCode":978}}"#,
            vault(),
            receiver(),
        );
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(0, 1_700_000_000, CHAIN_ID),
            storage.clone(),
        );
        assert_eq!(
            crate::VaultRouterVoteTarget
                .handle_approved(
                    &ctx,
                    U256::ONE,
                    payload.as_bytes(),
                    VoteTargetContext {
                        proposer: owner(),
                        attached_value: U256::ZERO,
                        block_number: 0,
                        chain_id: CHAIN_ID,
                    },
                )
                .unwrap(),
            TargetExecutionOutcome::Applied
        );
        let router = VaultRouterContract::new(storage.clone());
        assert!(router.assets.is_empty().unwrap());
        apply(&storage, 1).unwrap();
        assert!(router.asset_vault_set(asset()).contains(&vault()).unwrap());
        assert!(router
            .reference_currency_vault_set(USD_ISO_CODE)
            .contains(&vault())
            .unwrap());
        assert!(router.asset_vault_set(receiver()).is_empty().unwrap());
        assert!(router
            .reference_currency_vault_set(EUR_ISO_CODE)
            .is_empty()
            .unwrap());
        assert_eq!(
            router.vault_reference_currencies.read(&vault()).unwrap(),
            USD_ISO_CODE
        );
    });
}

#[test]
fn expiry_is_strict_and_expired_cleanup_needs_no_subcalls() {
    let window = outbe_chain_constants::get_governance_voting_window_blocks();
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    stub_vault(&mut provider);
    provider.set_block_number(10 + window);
    StorageHandle::enter(&mut provider, |storage| {
        stage(&storage, 1, ADD_VAULT, 10);
        apply(&storage, 1).unwrap(); // Equality is not expired.
    });
    assert_eq!(provider.get_events(VAULT_ROUTER_ADDRESS).len(), 1);

    let mut provider = HashMapStorageProvider::new(CHAIN_ID); // No subcall stubs.
    provider.set_block_number(11 + window);
    StorageHandle::enter(&mut provider, |storage| {
        stage(&storage, 1, ADD_VAULT, 10);
        apply(&storage, 1).unwrap();
        let router = VaultRouterContract::new(storage.clone());
        assert!(!router.approved_vault_actions.exists(U256::ONE).unwrap());
        assert_eq!(
            router.pending_vault_proposals.read(&vault()).unwrap(),
            U256::ZERO
        );
        assert!(router.assets.is_empty().unwrap());
        assert!(apply(&storage, 1).is_err());
    });
    let events = provider.get_events(VAULT_ROUTER_ADDRESS);
    assert_eq!(events.len(), 1);
    let expired = IVaultRouter::ApprovedVaultActionExpired::decode_log_data(&events[0]).unwrap();
    assert_eq!((expired.proposalId, expired.vault), (U256::ONE, vault()));
}

#[test]
fn expiry_addition_is_widened_near_u64_max() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    stub_vault(&mut provider);
    provider.set_block_number(u64::MAX);
    StorageHandle::enter(&mut provider, |storage| {
        stage(&storage, 1, ADD_VAULT, u64::MAX - 1);
        apply(&storage, 1).unwrap();
        assert!(VaultRouterContract::new(storage)
            .asset_vault_set(asset())
            .contains(&vault())
            .unwrap());
    });
    IVaultRouter::VaultAdded::decode_log_data(&provider.get_events(VAULT_ROUTER_ADDRESS)[0])
        .unwrap();
}

#[test]
fn canonical_order_supersedes_across_action_kinds_and_proposal_ids() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        stage(&storage, 99, ADD_VAULT, 10);
        stage(&storage, 2, REMOVE_VAULT, 10);
        stage(&storage, 1, ADD_VAULT, 10);
        assert!(apply(&storage, 99).is_err());
        assert!(apply(&storage, 2).is_err());
        let record = state::approved_vault_action(&storage, U256::ONE).unwrap();
        assert_eq!((record.kind, record.approved_at), (ADD_VAULT, 10));
    });
    assert!(provider.get_events(VAULT_ROUTER_ADDRESS).is_empty());
}

#[test]
fn failed_validation_and_bad_token_returns_restore_pending_and_indexes() {
    for response in [
        word(U256::ZERO),
        Bytes::from_static(&[1]),
        word(U256::from(2)),
    ] {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        stub_vault(&mut provider);
        provider.stub_sub_call_at_selector(asset(), IERC20::approveCall::SELECTOR, response);
        StorageHandle::enter(&mut provider, |storage| {
            stage(&storage, 1, ADD_VAULT, 0);
            assert!(apply(&storage, 1).is_err());
            let router = VaultRouterContract::new(storage.clone());
            assert!(router.assets.is_empty().unwrap());
            assert!(router.asset_vault_set(asset()).is_empty().unwrap());
            assert!(router
                .reference_currency_vault_set(USD_ISO_CODE)
                .is_empty()
                .unwrap());
            assert_eq!(router.vault_reference_currencies.read(&vault()).unwrap(), 0);
            assert_eq!(
                router.pending_vault_proposals.read(&vault()).unwrap(),
                U256::ONE
            );
            assert!(router.approved_vault_actions.exists(U256::ONE).unwrap());
        });
        assert!(provider.get_events(VAULT_ROUTER_ADDRESS).is_empty());
    }
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    stub_vault(&mut provider);
    provider.stub_sub_call_at_selector(vault(), IVaultV2::ownerCall::SELECTOR, word_addr(owner()));
    StorageHandle::enter(&mut provider, |storage| {
        stage(&storage, 1, ADD_VAULT, 0);
        assert!(apply(&storage, 1).is_err());
        assert!(state::approved_vault_action(&storage, U256::ONE).is_ok());
    });
}

#[test]
fn inconsistent_index_and_nonpayable_apply_do_not_consume_record() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        stage(&storage, 1, ADD_VAULT, 0);
        let router = VaultRouterContract::new(storage.clone());
        router
            .pending_vault_proposals
            .write(&vault(), U256::from(2))
            .unwrap();
        assert_revert::<IVaultRouter::ApprovedVaultActionIndexInconsistent>(
            &apply(&storage, 1).unwrap_err(),
        );
        assert!(dispatch(
            storage.clone(),
            &IVaultRouter::applyApprovedCall {
                proposalId: U256::ONE
            }
            .abi_encode(),
            stranger(),
            U256::ONE
        )
        .is_err());
        assert!(router.approved_vault_actions.exists(U256::ONE).unwrap());
    });
}
