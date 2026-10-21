use super::*;
use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
use outbe_vaultrouter::{api::IVaultRouter, VaultRouterContract};
use outbe_vote::{handlers::TargetElectorate, ProposalSubmission};

fn vault_payload(operation: &str, vault: Address) -> String {
    match operation {
        "addVault" => serde_json::json!({"operation":operation,"vault":vault,"asset":Address::repeat_byte(0x77),"isoCode":840}).to_string(),
        _ => serde_json::json!({"operation":operation,"vault":vault}).to_string(),
    }
}

fn route_payload(operation: &str, address: Address) -> String {
    match operation {
        "addLiquiditySource" | "addLiquidityTarget" => {
            serde_json::json!({"operation":operation,"address":address,"type":1}).to_string()
        }
        _ => serde_json::json!({"operation":operation,"address":address}).to_string(),
    }
}

fn approve(vote: &mut Vote<'_>, id: U256) {
    for validator in [VALIDATOR_A, VALIDATOR_B] {
        vote.cast_vote_approve(id, validator, true, 8).unwrap();
    }
}

fn status(vote: &Vote<'_>, id: U256) -> ProposalStatus {
    vote.proposals
        .get(id)
        .unwrap()
        .unwrap()
        .proposal_status()
        .unwrap()
}

#[test]
fn vault_router_registration_enforces_validator_only_zero_value_admission() {
    let target = registry().lookup(VAULT_ROUTER_ADDRESS).unwrap();
    assert_eq!(target.admission(), TargetAdmission::ActiveValidatorOnly);
    assert_eq!(target.electorate(), TargetElectorate::Active);
    let mut provider = HashMapStorageProvider::new(1);
    let storage = StorageHandle::new(&mut provider);
    setup_active_validators(storage.clone());
    let mut vote = Vote::new(storage);
    let payload = vault_payload("addVault", Address::repeat_byte(0x11));
    assert!(vote
        .create_proposal(
            Address::repeat_byte(0x44),
            VAULT_ROUTER_ADDRESS,
            &payload,
            7,
            registry()
        )
        .is_err());
    assert!(vote
        .create_proposal_with_value(
            ProposalSubmission {
                proposer: VALIDATOR_A,
                target_module: VAULT_ROUTER_ADDRESS,
                payload: &payload,
                created_height: 7,
                attached_value: U256::ONE
            },
            registry()
        )
        .is_err());
    assert_eq!(vote.proposal_count.read().unwrap(), U256::ZERO);
    let id = vote
        .create_proposal(VALIDATOR_A, VAULT_ROUTER_ADDRESS, &payload, 7, registry())
        .unwrap();
    assert_eq!(vote.proposal_electorate.read(&id).unwrap(), 0);
}

#[test]
fn real_tally_stages_vault_actions_without_subcalls_and_nonapproved_has_no_effects() {
    let vault = Address::repeat_byte(0x11);
    let rejected_vault = Address::repeat_byte(0x12);
    let mut provider = HashMapStorageProvider::new(1);
    {
        let storage = StorageHandle::new(&mut provider);
        setup_active_validators(storage.clone());
        let mut vote = Vote::new(storage.clone());
        let add = vote
            .create_proposal(
                VALIDATOR_A,
                VAULT_ROUTER_ADDRESS,
                &vault_payload("addVault", vault),
                7,
                registry(),
            )
            .unwrap();
        let not_approved = vote
            .create_proposal(
                VALIDATOR_B,
                VAULT_ROUTER_ADDRESS,
                &vault_payload("removeVault", rejected_vault),
                7,
                registry(),
            )
            .unwrap();
        approve(&mut vote, add);
        vote.cast_vote_approve(not_approved, VALIDATOR_A, false, 8)
            .unwrap();
        vote.cast_vote_approve(not_approved, VALIDATOR_B, false, 8)
            .unwrap();
        let height = 7 + VOTING_WINDOW_BLOCKS + 1;
        vote.process_begin_block(
            &finalize_context(storage.clone(), height, VALIDATOR_A),
            registry(),
        )
        .unwrap();
        assert_eq!(status(&vote, add), ProposalStatus::Approved);
        assert_eq!(status(&vote, not_approved), ProposalStatus::Expired);
        let router = VaultRouterContract::new(storage.clone());
        let record = router.approved_vault_actions.get(add).unwrap().unwrap();
        assert_eq!(
            (record.vault, record.kind, record.approved_at),
            (vault, 1, height)
        );
        assert_eq!(router.pending_vault_proposals.read(&vault).unwrap(), add);
        assert!(router
            .approved_vault_actions
            .get(not_approved)
            .unwrap()
            .is_none());
        assert_eq!(
            router
                .pending_vault_proposals
                .read(&rejected_vault)
                .unwrap(),
            U256::ZERO
        );
        assert_eq!(router.assets.len().unwrap(), 0);
        vote.process_begin_block(
            &finalize_context(storage, height + 1, VALIDATOR_A),
            registry(),
        )
        .unwrap();
    }
    assert!(provider.sub_call_inputs().is_empty());
    assert!(provider.get_events(VAULT_ROUTER_ADDRESS).is_empty());
}

#[test]
fn same_block_supersession_follows_real_tally_order_not_proposal_id() {
    let vault = Address::repeat_byte(0x11);
    let mut provider = HashMapStorageProvider::new(1);
    {
        let storage = StorageHandle::new(&mut provider);
        setup_active_validators(storage.clone());
        let mut vote = Vote::new(storage.clone());
        // Expiring the first proposal swap-removes the last ID into index zero.
        // The following block therefore tallies the larger ID before the smaller.
        let dummy_payload = vault_payload("removeVault", Address::repeat_byte(0x12));
        let dummy = vote
            .create_proposal_with_voting_window(
                ProposalSubmission::new(VALIDATOR_A, VAULT_ROUTER_ADDRESS, &dummy_payload, 7),
                1,
                registry(),
            )
            .unwrap();
        let lower = vote
            .create_proposal(
                VALIDATOR_B,
                VAULT_ROUTER_ADDRESS,
                &vault_payload("removeVault", vault),
                7,
                registry(),
            )
            .unwrap();
        let higher = vote
            .create_proposal(
                VALIDATOR_C,
                VAULT_ROUTER_ADDRESS,
                &vault_payload("addVault", vault),
                7,
                registry(),
            )
            .unwrap();
        approve(&mut vote, lower);
        approve(&mut vote, higher);
        vote.process_begin_block(
            &finalize_context(storage.clone(), 9, VALIDATOR_A),
            registry(),
        )
        .unwrap();
        assert_eq!(status(&vote, dummy), ProposalStatus::Expired);
        assert_eq!(
            vote.pending_proposal_ids.read_all().unwrap(),
            vec![higher, lower]
        );
        let height = 7 + VOTING_WINDOW_BLOCKS + 1;
        vote.process_begin_block(
            &finalize_context(storage.clone(), height, VALIDATOR_A),
            registry(),
        )
        .unwrap();
        assert_eq!(status(&vote, lower), ProposalStatus::Approved);
        assert_eq!(status(&vote, higher), ProposalStatus::Approved);
        let router = VaultRouterContract::new(storage);
        assert_eq!(router.pending_vault_proposals.read(&vault).unwrap(), lower);
        assert!(router.approved_vault_actions.get(higher).unwrap().is_none());
        let record = router.approved_vault_actions.get(lower).unwrap().unwrap();
        assert_eq!((record.kind, record.approved_at), (2, height));
    }
    assert!(provider.sub_call_inputs().is_empty());
}

#[test]
fn route_actions_mutate_and_emit_while_conflicts_do_not_stop_other_tallies() {
    let address = Address::repeat_byte(0x31);
    let missing = Address::repeat_byte(0x32);
    let mut provider = HashMapStorageProvider::new(1);
    {
        let storage = StorageHandle::new(&mut provider);
        setup_active_validators(storage.clone());
        let mut vote = Vote::new(storage.clone());
        let conflict = vote
            .create_proposal(
                VALIDATOR_A,
                VAULT_ROUTER_ADDRESS,
                &route_payload("removeLiquiditySource", missing),
                7,
                registry(),
            )
            .unwrap();
        let add_source = vote
            .create_proposal(
                VALIDATOR_B,
                VAULT_ROUTER_ADDRESS,
                &route_payload("addLiquiditySource", address),
                7,
                registry(),
            )
            .unwrap();
        let add_target = vote
            .create_proposal(
                VALIDATOR_C,
                VAULT_ROUTER_ADDRESS,
                &route_payload("addLiquidityTarget", address),
                7,
                registry(),
            )
            .unwrap();
        for id in [conflict, add_source, add_target] {
            approve(&mut vote, id);
        }
        let height = 7 + VOTING_WINDOW_BLOCKS + 1;
        vote.process_begin_block(
            &finalize_context(storage.clone(), height, VALIDATOR_A),
            registry(),
        )
        .unwrap();
        assert_eq!(status(&vote, conflict), ProposalStatus::Error);
        assert_eq!(status(&vote, add_source), ProposalStatus::Approved);
        assert_eq!(status(&vote, add_target), ProposalStatus::Approved);
        let router = VaultRouterContract::new(storage.clone());
        assert_eq!(router.liquidity_source_types.read(&address).unwrap(), 1);
        assert_eq!(router.liquidity_target_types.read(&address).unwrap(), 1);
        let remove_source = vote
            .create_proposal(
                VALIDATOR_A,
                VAULT_ROUTER_ADDRESS,
                &route_payload("removeLiquiditySource", address),
                height,
                registry(),
            )
            .unwrap();
        let remove_target = vote
            .create_proposal(
                VALIDATOR_B,
                VAULT_ROUTER_ADDRESS,
                &route_payload("removeLiquidityTarget", address),
                height,
                registry(),
            )
            .unwrap();
        for id in [remove_source, remove_target] {
            for validator in [VALIDATOR_A, VALIDATOR_B] {
                vote.cast_vote_approve(id, validator, true, height + 1)
                    .unwrap();
            }
        }
        vote.process_begin_block(
            &finalize_context(storage, height + VOTING_WINDOW_BLOCKS + 1, VALIDATOR_A),
            registry(),
        )
        .unwrap();
        assert_eq!(status(&vote, remove_source), ProposalStatus::Approved);
        assert_eq!(status(&vote, remove_target), ProposalStatus::Approved);
        assert_eq!(router.liquidity_sources.len().unwrap(), 0);
        assert_eq!(router.liquidity_targets.len().unwrap(), 0);
        assert_eq!(router.liquidity_source_types.read(&address).unwrap(), 0);
        assert_eq!(router.liquidity_target_types.read(&address).unwrap(), 0);
    }
    let events = provider.get_events(VAULT_ROUTER_ADDRESS);
    assert_eq!(events.len(), 4);
    for (event, signature) in events.iter().zip([
        IVaultRouter::LiquiditySourceAdded::SIGNATURE_HASH,
        IVaultRouter::LiquidityTargetAdded::SIGNATURE_HASH,
        IVaultRouter::LiquiditySourceRemoved::SIGNATURE_HASH,
        IVaultRouter::LiquidityTargetRemoved::SIGNATURE_HASH,
    ]) {
        assert_eq!(event.topics().first(), Some(&signature));
    }
    assert!(provider.sub_call_inputs().is_empty());
}
