use alloy_primitives::LogData;

use super::*;
use crate::state::{self, ADD_VAULT, REMOVE_VAULT};

fn stub_vault(provider: &mut HashMapStorageProvider) {
    provider.stub_sub_call_at_selector(vault(), IERC20::balanceOfCall::SELECTOR, word(U256::ZERO));
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
    state::record_vault_action(storage, U256::from(id), vault(), kind, height).unwrap();
}

fn apply(storage: &StorageHandle<'_>, id: u64) -> outbe_primitives::error::Result<Bytes> {
    dispatch(
        storage.clone(),
        &IVaultRouter::applyVaultActionCall {
            proposalId: U256::from(id),
        }
        .abi_encode(),
        stranger(),
        U256::ZERO,
    )
}

fn created(id: u64, kind: u8) -> LogData {
    IVaultRouter::VaultActionCreated {
        proposalId: U256::from(id),
        vault: vault(),
        kind: IVaultRouter::VaultAction::try_from(kind).unwrap(),
    }
    .encode_log_data()
}

fn applied(id: u64) -> LogData {
    IVaultRouter::VaultActionApplied {
        proposalId: U256::from(id),
        vault: vault(),
    }
    .encode_log_data()
}

fn expired(id: u64) -> LogData {
    IVaultRouter::VaultActionExpired {
        proposalId: U256::from(id),
        vault: vault(),
    }
    .encode_log_data()
}

fn canceled(id: u64, new_id: u64) -> LogData {
    IVaultRouter::VaultActionCanceled {
        proposalId: U256::from(id),
        vault: vault(),
        newProposalId: U256::from(new_id),
    }
    .encode_log_data()
}

fn vault_added() -> LogData {
    IVaultRouter::VaultAdded {
        isoCode: USD_ISO_CODE,
        asset: asset(),
        vault: vault(),
    }
    .encode_log_data()
}

fn vault_removed() -> LogData {
    IVaultRouter::VaultRemoved {
        isoCode: USD_ISO_CODE,
        asset: asset(),
        vault: vault(),
    }
    .encode_log_data()
}

fn pending(
    storage: &StorageHandle<'_>,
    id: u64,
) -> outbe_primitives::error::Result<IVaultRouter::getVaultActionReturn> {
    let output = dispatch(
        storage.clone(),
        &IVaultRouter::getVaultActionCall {
            proposalId: U256::from(id),
        }
        .abi_encode(),
        stranger(),
        U256::ZERO,
    )?;
    Ok(IVaultRouter::getVaultActionCall::abi_decode_returns(&output).unwrap())
}

#[test]
fn pending_view_tracks_supersession_application_and_expiry_cleanup() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    stub_vault(&mut provider);
    StorageHandle::enter(&mut provider, |storage| {
        let missing = pending(&storage, 1).unwrap();
        assert!(matches!(missing.action, IVaultRouter::VaultAction::Unknown));
        assert_eq!((missing.vault, missing.createdAt), (Address::ZERO, 0));
        stage(&storage, 1, REMOVE_VAULT, 7);
        let removal = pending(&storage, 1).unwrap();
        assert!(matches!(
            removal.action,
            IVaultRouter::VaultAction::RemoveVault
        ));
        assert_eq!((removal.vault, removal.createdAt), (vault(), 7));
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
        assert_eq!((addition.vault, addition.createdAt), (vault(), 8));
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
        assert_revert::<IVaultRouter::VaultActionIndexInconsistent>(
            &pending(&storage, 1).map(|_| ()).unwrap_err(),
        );
        router
            .pending_vault_proposals
            .write(&vault(), U256::ONE)
            .unwrap();
        let mut record = router.vault_actions.get(U256::ONE).unwrap().unwrap();
        record.kind = 3;
        router.vault_actions.delete(U256::ONE).unwrap();
        router.vault_actions.create(&record).unwrap();
        assert_revert::<IVaultRouter::InvalidVaultAction>(
            &pending(&storage, 1).map(|_| ()).unwrap_err(),
        );
    });
    assert!(provider.sub_call_inputs().is_empty());
    assert_eq!(
        provider.get_events(VAULT_ROUTER_ADDRESS),
        &vec![created(1, ADD_VAULT)]
    );
}

#[test]
fn layout_preserves_old_slots_and_appends_pending_maps() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        let router = VaultRouterContract::new(storage.clone());
        router.assets.insert(asset()).unwrap();
        router.stablecoin_sources.insert(source_account()).unwrap();
        router.stablecoin_targets.insert(target_account()).unwrap();
        for slot in [1, 4, 7] {
            assert_eq!(
                storage
                    .sload(VAULT_ROUTER_ADDRESS, U256::from(slot))
                    .unwrap(),
                U256::ONE
            );
        }
        assert_eq!(router.asset_vaults.base_slot(), U256::from(3));
        assert_eq!(router.stablecoin_source_types.base_slot(), U256::from(6));
        assert_eq!(router.stablecoin_target_types.base_slot(), U256::from(9));
        assert_eq!(router.reference_currency_vaults.base_slot(), U256::from(10));
        assert_eq!(
            router.vault_reference_currencies.base_slot(),
            U256::from(11)
        );
        assert_eq!(router.reservation_nonce.slot(), U256::from(12));
        assert_eq!(router.reservations.base_slot(), U256::from(13));
        assert_eq!(router.vault_assets.base_slot(), U256::from(35));
        assert_eq!(router.vault_actions.base_slot(), U256::from(36));
        assert_eq!(router.pending_vault_proposals.base_slot(), U256::from(39));
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
    // Each vault action has one start event and one end event. Applied comes before the
    // vault event in the same transaction.
    assert_eq!(
        provider.get_events(VAULT_ROUTER_ADDRESS),
        &vec![
            created(1, ADD_VAULT),
            applied(1),
            vault_added(),
            created(2, REMOVE_VAULT),
            applied(2),
            vault_removed(),
        ]
    );
}

#[test]
fn apply_uses_live_metadata_not_informational_proposal_declarations() {
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
    use outbe_system_common::vote_target::{TargetExecutionOutcome, VoteTarget, VoteTargetContext};

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    stub_vault(&mut provider);
    StorageHandle::enter(&mut provider, |storage| {
        let payload = format!(
            r#"{{"operation":"addVault","vault":"{:#x}","asset":"{:#x}","isoCode":978}}"#,
            vault(),
            receiver(),
        );
        let ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 0,
                timestamp: 1_700_000_000,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: 1_700_000_000,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
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
    assert_eq!(
        provider.get_events(VAULT_ROUTER_ADDRESS),
        &vec![created(1, ADD_VAULT), applied(1), vault_added()]
    );

    let mut provider = HashMapStorageProvider::new(CHAIN_ID); // No subcall stubs.
    provider.set_block_number(11 + window);
    StorageHandle::enter(&mut provider, |storage| {
        stage(&storage, 1, ADD_VAULT, 10);
        apply(&storage, 1).unwrap();
        let router = VaultRouterContract::new(storage.clone());
        assert!(!router.vault_actions.exists(U256::ONE).unwrap());
        assert_eq!(
            router.pending_vault_proposals.read(&vault()).unwrap(),
            U256::ZERO
        );
        assert!(router.assets.is_empty().unwrap());
        assert!(apply(&storage, 1).is_err());
    });
    assert_eq!(
        provider.get_events(VAULT_ROUTER_ADDRESS),
        &vec![created(1, ADD_VAULT), expired(1)]
    );
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
    assert_eq!(
        provider.get_events(VAULT_ROUTER_ADDRESS),
        &vec![created(1, ADD_VAULT), applied(1), vault_added()]
    );
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
        let record = state::vault_action(&storage, U256::ONE).unwrap();
        assert_eq!((record.kind, record.created_at), (ADD_VAULT, 10));
    });
    // Canceled of the older action comes before Created of the new action.
    assert_eq!(
        provider.get_events(VAULT_ROUTER_ADDRESS),
        &vec![
            created(99, ADD_VAULT),
            canceled(99, 2),
            created(2, REMOVE_VAULT),
            canceled(2, 1),
            created(1, ADD_VAULT),
        ]
    );
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
            assert!(router.vault_actions.exists(U256::ONE).unwrap());
        });
        // A failed application leaves no Applied event.
        assert_eq!(
            provider.get_events(VAULT_ROUTER_ADDRESS),
            &vec![created(1, ADD_VAULT)]
        );
    }
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    stub_vault(&mut provider);
    provider.stub_sub_call_at_selector(vault(), IVaultV2::ownerCall::SELECTOR, word_addr(owner()));
    StorageHandle::enter(&mut provider, |storage| {
        stage(&storage, 1, ADD_VAULT, 0);
        assert!(apply(&storage, 1).is_err());
        assert!(state::vault_action(&storage, U256::ONE).is_ok());
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
        assert_revert::<IVaultRouter::VaultActionIndexInconsistent>(
            &apply(&storage, 1).unwrap_err(),
        );
        assert!(dispatch(
            storage.clone(),
            &IVaultRouter::applyVaultActionCall {
                proposalId: U256::ONE
            }
            .abi_encode(),
            stranger(),
            U256::ONE
        )
        .is_err());
        assert!(router.vault_actions.exists(U256::ONE).unwrap());
    });
}

#[test]
fn removal_with_live_shares_preserves_pending_and_indexes_then_retries_at_zero() {
    let other_vault = address!("0x0000000000000000000000000000000000000666");
    for shares in [U256::ONE, U256::MAX] {
        for has_other_vault in [false, true] {
            let mut provider = HashMapStorageProvider::new(CHAIN_ID);
            stub_vault(&mut provider);
            provider.stub_sub_call_at_selector(
                vault(),
                IERC20::balanceOfCall::SELECTOR,
                word(shares),
            );
            StorageHandle::enter(&mut provider, |storage| {
                runtime::add_vault(storage.clone(), vault()).unwrap();
                if has_other_vault {
                    crate::test_support::register_vault(
                        &storage,
                        asset(),
                        other_vault,
                        USD_ISO_CODE,
                    )
                    .unwrap();
                }
                stage(&storage, 7, REMOVE_VAULT, 0);
            });
            let calls_before = provider.sub_call_inputs().len();
            let events_before = provider.get_events(VAULT_ROUTER_ADDRESS).len();
            StorageHandle::enter(&mut provider, |storage| {
                let record = state::vault_action(&storage, U256::from(7)).unwrap();
                let error = apply(&storage, 7).unwrap_err();
                let PrecompileError::RevertBytes(bytes) = error else {
                    panic!("expected ReserveVaultHasShares");
                };
                let error = IVaultRouter::ReserveVaultHasShares::abi_decode(&bytes).unwrap();
                assert_eq!((error.vault, error.shares), (vault(), shares));
                assert_eq!(
                    state::vault_action(&storage, U256::from(7)).unwrap(),
                    record
                );
                assert_registered_removal_indexes(&storage, has_other_vault);
            });
            assert_eq!(
                provider.get_events(VAULT_ROUTER_ADDRESS).len(),
                events_before
            );
            let calls = &provider.sub_call_inputs()[calls_before..];
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].target, vault());
            assert_eq!(
                calls[0].calldata.as_ref(),
                IERC20::balanceOfCall {
                    account: VAULT_ROUTER_ADDRESS
                }
                .abi_encode()
                .as_slice()
            );

            provider.stub_sub_call_at_selector(
                vault(),
                IERC20::balanceOfCall::SELECTOR,
                word(U256::ZERO),
            );
            StorageHandle::enter(&mut provider, |storage| {
                apply(&storage, 7).unwrap();
                let router = VaultRouterContract::new(storage.clone());
                assert!(!router.vault_actions.exists(U256::from(7)).unwrap());
                assert_eq!(
                    router.pending_vault_proposals.read(&vault()).unwrap(),
                    U256::ZERO
                );
                assert!(!router.asset_vault_set(asset()).contains(&vault()).unwrap());
                assert!(!router
                    .reference_currency_vault_set(USD_ISO_CODE)
                    .contains(&vault())
                    .unwrap());
                assert_eq!(router.vault_assets.read(&vault()).unwrap(), Address::ZERO);
                assert_eq!(router.vault_reference_currencies.read(&vault()).unwrap(), 0);
                assert_eq!(router.assets.contains(&asset()).unwrap(), has_other_vault);
                if has_other_vault {
                    assert_eq!(
                        router.asset_vault_set(asset()).at(0).unwrap(),
                        Some(other_vault)
                    );
                    assert_eq!(
                        router
                            .reference_currency_vault_set(USD_ISO_CODE)
                            .at(0)
                            .unwrap(),
                        Some(other_vault)
                    );
                }
            });
            let events = provider.get_events(VAULT_ROUTER_ADDRESS);
            assert_eq!(events[events_before - 1], created(7, REMOVE_VAULT));
            assert_eq!(events[events_before..], [applied(7), vault_removed()]);
        }
    }
}

fn assert_registered_removal_indexes(storage: &StorageHandle<'_>, has_other_vault: bool) {
    let router = VaultRouterContract::new(storage.clone());
    let count = if has_other_vault { 2 } else { 1 };
    assert_eq!(router.asset_vault_set(asset()).len().unwrap(), count);
    assert_eq!(
        router.asset_vault_set(asset()).at(0).unwrap(),
        Some(vault())
    );
    assert_eq!(
        router
            .reference_currency_vault_set(USD_ISO_CODE)
            .len()
            .unwrap(),
        count
    );
    assert_eq!(
        router
            .reference_currency_vault_set(USD_ISO_CODE)
            .at(0)
            .unwrap(),
        Some(vault())
    );
    assert_eq!(router.assets.len().unwrap(), 1);
    assert_eq!(router.assets.at(0).unwrap(), Some(asset()));
    assert_eq!(router.vault_assets.read(&vault()).unwrap(), asset());
    assert_eq!(
        router.vault_reference_currencies.read(&vault()).unwrap(),
        USD_ISO_CODE
    );
    assert_eq!(
        router.pending_vault_proposals.read(&vault()).unwrap(),
        U256::from(7)
    );
}

#[test]
fn removal_missing_or_malformed_share_query_fails_closed() {
    for response in [None, Some(Bytes::new()), Some(Bytes::from(vec![0u8; 31]))] {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        if let Some(response) = response {
            provider.stub_sub_call_at_selector(vault(), IERC20::balanceOfCall::SELECTOR, response);
        }
        StorageHandle::enter(&mut provider, |storage| {
            crate::test_support::register_vault(&storage, asset(), vault(), USD_ISO_CODE).unwrap();
            stage(&storage, 7, REMOVE_VAULT, 0);
            assert!(apply(&storage, 7).is_err());
            assert_registered_removal_indexes(&storage, false);
            assert!(state::vault_action(&storage, U256::from(7)).is_ok());
        });
        assert_eq!(
            provider.get_events(VAULT_ROUTER_ADDRESS),
            &vec![created(7, REMOVE_VAULT)]
        );
        let calls = provider.sub_call_inputs();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].target, vault());
        assert!(calls[0]
            .calldata
            .starts_with(&IERC20::balanceOfCall::SELECTOR));
    }
}

#[test]
fn expired_removal_cleans_pending_without_querying_live_shares_or_changing_registry() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(1 + outbe_chain_constants::get_governance_voting_window_blocks());
    StorageHandle::enter(&mut provider, |storage| {
        crate::test_support::register_vault(&storage, asset(), vault(), USD_ISO_CODE).unwrap();
        stage(&storage, 7, REMOVE_VAULT, 0);
        apply(&storage, 7).unwrap();
        let router = VaultRouterContract::new(storage.clone());
        assert!(router.asset_vault_set(asset()).contains(&vault()).unwrap());
        assert!(router
            .reference_currency_vault_set(USD_ISO_CODE)
            .contains(&vault())
            .unwrap());
        assert_eq!(router.vault_assets.read(&vault()).unwrap(), asset());
        assert!(!router.vault_actions.exists(U256::from(7)).unwrap());
        assert_eq!(
            router.pending_vault_proposals.read(&vault()).unwrap(),
            U256::ZERO
        );
    });
    assert!(provider.sub_call_inputs().is_empty());
    assert_eq!(
        provider.get_events(VAULT_ROUTER_ADDRESS),
        &vec![created(7, REMOVE_VAULT), expired(7)]
    );
}

#[test]
fn pending_removal_blocks_new_reservations_even_after_expiry_until_cleanup() {
    let window = outbe_chain_constants::get_governance_voting_window_blocks();
    for block in [0, window, window + 1] {
        with_reservable_vault_at_block(U256::from(100), block, |storage| {
            stage(&storage, 9, ADD_VAULT, 0);
            stage(&storage, 7, REMOVE_VAULT, 0);
            let error = reserve_in(&storage, USD_ISO_CODE).unwrap_err();
            let PrecompileError::RevertBytes(bytes) = error else {
                panic!("expected ReserveVaultRemovalPending");
            };
            let error = IVaultRouter::ReserveVaultRemovalPending::abi_decode(&bytes).unwrap();
            assert_eq!((error.vault, error.proposalId), (vault(), U256::from(7)));
            let router = VaultRouterContract::new(storage.clone());
            assert_eq!(router.reservation_nonce.read().unwrap(), U256::ZERO);
            assert!(!router.reservations.exists(U256::ONE).unwrap());
            assert!(state::vault_action(&storage, U256::from(7)).is_ok());
            if block > window {
                apply(&storage, 7).unwrap();
                assert_eq!(reserve_in(&storage, USD_ISO_CODE).unwrap(), U256::ONE);
            }
        });
    }
}

#[test]
fn reservation_admission_uses_current_action_for_the_selected_vault_only() {
    for scenario in 0..4 {
        with_reservable_vault(U256::from(100), |storage| {
            match scenario {
                0 => {} // No pending action.
                1 => stage(&storage, 1, ADD_VAULT, 0),
                2 => {
                    stage(&storage, 9, REMOVE_VAULT, 0);
                    stage(&storage, 1, ADD_VAULT, 0);
                }
                _ => state::record_vault_action(&storage, U256::ONE, vault_to(), REMOVE_VAULT, 0)
                    .unwrap(),
            }
            assert_eq!(reserve_in(&storage, USD_ISO_CODE).unwrap(), U256::ONE);
        });
    }
}

#[test]
fn reservation_admission_fails_closed_on_corrupt_pending_records() {
    for scenario in 0..3 {
        with_reservable_vault(U256::from(100), |storage| {
            stage(&storage, 7, ADD_VAULT, 0);
            let router = VaultRouterContract::new(storage.clone());
            let mut record = router.vault_actions.get(U256::from(7)).unwrap().unwrap();
            router.vault_actions.delete(U256::from(7)).unwrap();
            match scenario {
                0 => {} // Dangling pending ID.
                1 => {
                    record.kind = 3;
                    router.vault_actions.create(&record).unwrap();
                }
                _ => {
                    record.vault = vault_to();
                    router.vault_actions.create(&record).unwrap();
                    router
                        .pending_vault_proposals
                        .write(&vault_to(), U256::from(7))
                        .unwrap();
                }
            }
            let error = reserve_in(&storage, USD_ISO_CODE).unwrap_err();
            match scenario {
                0 => assert_revert::<IVaultRouter::VaultActionNotFound>(&error),
                1 => assert_revert::<IVaultRouter::InvalidVaultAction>(&error),
                _ => assert_revert::<IVaultRouter::VaultActionIndexInconsistent>(&error),
            }
            assert_eq!(router.reservation_nonce.read().unwrap(), U256::ZERO);
            assert!(!router.reservations.exists(U256::ONE).unwrap());
            assert_eq!(
                router.pending_vault_proposals.read(&vault()).unwrap(),
                U256::from(7)
            );
        });
    }
}

#[test]
fn pending_removal_does_not_block_settlement_of_existing_reservations() {
    for release in [false, true] {
        with_reservable_vault(U256::from(100), |storage| {
            let id = reserve_in(&storage, USD_ISO_CODE).unwrap();
            stage(&storage, 7, REMOVE_VAULT, 0);
            assert_revert::<IVaultRouter::ReserveVaultRemovalPending>(
                &reserve_in(&storage, USD_ISO_CODE).unwrap_err(),
            );
            if release {
                // Partial release also redeposits the unused amount into the origin vault.
                assert_eq!(
                    runtime::release_reservation(
                        storage.clone(),
                        id,
                        receiver(),
                        U256::from(4),
                        IVaultRouter::StablecoinTarget::Credis,
                    )
                    .unwrap(),
                    U256::from(4)
                );
            } else {
                assert_eq!(
                    runtime::return_reservation(storage.clone(), cca(), id).unwrap(),
                    U256::from(100)
                );
            }
            assert!(runtime::reservation_of(&storage, id)
                .unwrap()
                .asset
                .is_zero());
            assert!(state::vault_action(&storage, U256::from(7)).is_ok());
        });
    }
}

#[test]
fn vault_action_has_one_start_and_one_end_event() {
    let window = outbe_chain_constants::get_governance_voting_window_blocks();
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    stub_vault(&mut provider);
    StorageHandle::enter(&mut provider, |storage| {
        stage(&storage, 1, ADD_VAULT, 0);
        apply(&storage, 1).unwrap();
        stage(&storage, 2, REMOVE_VAULT, 0);
        stage(&storage, 3, REMOVE_VAULT, 0);
    });
    provider.set_block_number(1 + window);
    StorageHandle::enter(&mut provider, |storage| {
        apply(&storage, 3).unwrap();
    });
    let events = provider.get_events(VAULT_ROUTER_ADDRESS).clone();
    for (id, kind, end) in [
        (1, ADD_VAULT, applied(1)),
        (2, REMOVE_VAULT, canceled(2, 3)),
        (3, REMOVE_VAULT, expired(3)),
    ] {
        let topic = U256::from(id).into();
        let own: Vec<_> = events
            .iter()
            .filter(|event| event.topics().get(1) == Some(&topic))
            .cloned()
            .collect();
        assert_eq!(own, [created(id, kind), end], "proposal {id}");
    }
}

#[test]
fn vault_action_applied_comes_before_the_vault_event() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    stub_vault(&mut provider);
    StorageHandle::enter(&mut provider, |storage| {
        stage(&storage, 1, ADD_VAULT, 0);
    });
    provider.clear_events(VAULT_ROUTER_ADDRESS);
    StorageHandle::enter(&mut provider, |storage| {
        apply(&storage, 1).unwrap();
    });
    assert_eq!(
        provider.get_events(VAULT_ROUTER_ADDRESS),
        &vec![applied(1), vault_added()]
    );
}

#[test]
fn replaced_vault_action_is_canceled_before_the_new_one_is_created() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        stage(&storage, 4, ADD_VAULT, 0);
    });
    provider.clear_events(VAULT_ROUTER_ADDRESS);
    StorageHandle::enter(&mut provider, |storage| {
        stage(&storage, 5, REMOVE_VAULT, 1);
        assert!(matches!(
            pending(&storage, 4).unwrap().action,
            IVaultRouter::VaultAction::Unknown
        ));
    });
    assert_eq!(
        provider.get_events(VAULT_ROUTER_ADDRESS),
        &vec![canceled(4, 5), created(5, REMOVE_VAULT)]
    );
    let created = IVaultRouter::VaultActionCreated::decode_log_data(
        &provider.get_events(VAULT_ROUTER_ADDRESS)[1],
    )
    .unwrap();
    assert!(matches!(
        created.kind,
        IVaultRouter::VaultAction::RemoveVault
    ));
}
