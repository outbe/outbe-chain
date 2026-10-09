//! Real StablecoinFactory target: approval, expiry, error and fatal finalization paths.

use super::*;

#[test]
fn real_factory_vote_approval_creates_token_refunds_once_and_preserves_surplus() {
    let issuer = Address::repeat_byte(0x11);
    let raw = payload(issuer);
    let raw = core::str::from_utf8(&raw).unwrap();
    let forced_surplus = U256::from(7u64);
    let issuer_start = U256::from(11u64);
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_balance(VOTE_ADDRESS, STABLECOIN_CREATE_BOND + forced_surplus);
    provider.set_balance(issuer, issuer_start);
    let proposal_id;
    {
        let storage = StorageHandle::new(&mut provider);
        setup_active_validators(storage.clone());
        let mut vote = Vote::new(storage.clone());
        let factory = StablecoinFactoryContract::new(storage.clone());
        let (expected_id, expected_token) = factory.predict_token_address(issuer, "EXUSD").unwrap();
        proposal_id = vote
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
        vote.cast_vote_approve(proposal_id, VALIDATOR_A, true, 8)
            .unwrap();
        vote.cast_vote_approve(proposal_id, VALIDATOR_B, true, 8)
            .unwrap();

        let deadline = 7 + VOTING_WINDOW_BLOCKS;
        vote.process_begin_block(
            &finalize_context(storage.clone(), deadline + 1, issuer),
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
        assert_eq!(
            vote.proposal_bond(proposal_id).unwrap().settlement,
            BondSettlement::Refunded
        );
        assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), forced_surplus);
        assert_eq!(
            storage.balance(issuer).unwrap(),
            issuer_start + STABLECOIN_CREATE_BOND
        );
        assert_eq!(factory.token_count().unwrap(), U256::from(1u64));
        assert_eq!(
            factory.registered_token_id(expected_token).unwrap(),
            Some(expected_id)
        );
        assert!(!factory.reservations.exists(proposal_id).unwrap());

        vote.process_begin_block(
            &finalize_context(storage.clone(), deadline + 2, issuer),
            registry(),
        )
        .unwrap();
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), forced_surplus);
        assert_eq!(
            storage.balance(issuer).unwrap(),
            issuer_start + STABLECOIN_CREATE_BOND
        );
        assert_eq!(factory.token_count().unwrap(), U256::from(1u64));
    }

    assert_eq!(
        provider
            .get_events(VOTE_ADDRESS)
            .iter()
            .filter(|event| {
                event.topics().first() == Some(&IVote::ProposalBondRefunded::SIGNATURE_HASH)
            })
            .count(),
        1
    );
    assert_eq!(proposal_id, U256::from(1u64));
}

#[test]
fn pfs_010_05_expiry_releases_identity_and_pending_cap_and_burns_once() {
    let issuer = Address::repeat_byte(0x11);
    let raw = payload(issuer);
    let raw = core::str::from_utf8(&raw).unwrap();
    let forced_surplus = U256::from(7u64);
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_block_number(8);
    provider.set_balance(VOTE_ADDRESS, STABLECOIN_CREATE_BOND + forced_surplus);
    let proposal_id;
    {
        let storage = StorageHandle::new(&mut provider);
        setup_active_validators(storage.clone());
        let mut vote = Vote::new(storage.clone());
        proposal_id = vote
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
        vote.cast_vote_approve(proposal_id, VALIDATOR_A, true, 8)
            .unwrap();
        vote.cast_vote_approve(proposal_id, VALIDATOR_B, true, 8)
            .unwrap();

        let mut validator_set = ValidatorSet::new(storage.clone());
        validator_set
            .deactivate_validator(VALIDATOR_OWNER, VALIDATOR_B)
            .unwrap();
        register_active_validator(storage.clone(), VALIDATOR_D, 4);

        let promis_limit_before = outbe_promislimit::PromisLimitContract::new(storage.clone())
            .get_total_unallocated()
            .unwrap();
        let deadline = 7 + VOTING_WINDOW_BLOCKS;
        vote.process_begin_block(
            &finalize_context(storage.clone(), deadline + 1, issuer),
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
        // The expired bond is a native burn: no capacity credit of any kind.
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            promis_limit_before
        );
        assert_eq!(
            vote.proposal_bond(proposal_id).unwrap().settlement,
            BondSettlement::Burned
        );
        assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), forced_surplus);
        assert_eq!(storage.balance(issuer).unwrap(), U256::ZERO);
        let factory = StablecoinFactoryContract::new(storage.clone());
        assert_eq!(factory.token_count().unwrap(), U256::ZERO);
        assert!(!factory.reservations.exists(proposal_id).unwrap());

        vote.process_begin_block(
            &finalize_context(storage.clone(), deadline + 2, issuer),
            registry(),
        )
        .unwrap();
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), forced_surplus);
    }

    provider.set_balance(VOTE_ADDRESS, STABLECOIN_CREATE_BOND + forced_surplus);
    {
        let storage = StorageHandle::new(&mut provider);
        let mut vote = Vote::new(storage.clone());
        let retry_id = vote
            .create_proposal_with_value(
                outbe_vote::ProposalSubmission {
                    proposer: issuer,
                    target_module: STABLECOIN_FACTORY_ADDRESS,
                    payload: raw,
                    created_height: 7 + VOTING_WINDOW_BLOCKS + 2,
                    attached_value: STABLECOIN_CREATE_BOND,
                },
                registry(),
            )
            .unwrap();
        assert_eq!(retry_id, proposal_id + U256::from(1u64));
        assert_eq!(vote.pending_proposal_count_by_proposer(issuer).unwrap(), 1);
        assert!(StablecoinFactoryContract::new(storage)
            .reservations
            .exists(retry_id)
            .unwrap());
    }

    assert_eq!(
        provider
            .get_events(VOTE_ADDRESS)
            .iter()
            .filter(|event| {
                event.topics().first() == Some(&IVote::ProposalBondBurned::SIGNATURE_HASH)
            })
            .count(),
        1
    );
    assert!(provider.get_events(STABLECOIN_FACTORY_ADDRESS).is_empty());
}

#[test]
fn pfs_010_06_execution_error_refunds_bond_once_and_retains_reservation() {
    let issuer = Address::repeat_byte(0x11);
    let raw = payload(issuer);
    let raw = core::str::from_utf8(&raw).unwrap();
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_balance(VOTE_ADDRESS, STABLECOIN_CREATE_BOND);
    let reserved_token;
    {
        let storage = StorageHandle::new(&mut provider);
        setup_active_validators(storage.clone());
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
        let mut corrupted_payload = vote.proposals.get(proposal_id).unwrap().unwrap();
        corrupted_payload.payload = "{".into();
        vote.proposals.update(&corrupted_payload).unwrap();
        vote.cast_vote_approve(proposal_id, VALIDATOR_A, true, 8)
            .unwrap();
        vote.cast_vote_approve(proposal_id, VALIDATOR_B, true, 8)
            .unwrap();

        let deadline = 7 + VOTING_WINDOW_BLOCKS;
        vote.process_begin_block(
            &finalize_context(storage.clone(), deadline + 1, issuer),
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
            ProposalStatus::Error
        );
        // Vote refunds the bond once and closes the liability. The
        // target-owned reservation is admission state and stays.
        assert_eq!(
            vote.proposal_bond(proposal_id).unwrap().settlement,
            BondSettlement::Refunded
        );
        assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
        assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(storage.balance(issuer).unwrap(), STABLECOIN_CREATE_BOND);
        let factory = StablecoinFactoryContract::new(storage);
        assert_eq!(factory.token_count().unwrap(), U256::ZERO);
        let reservation = factory.reservations.get(proposal_id).unwrap().unwrap();
        reserved_token = reservation.token;
        assert_eq!(
            factory
                .pending_token_id
                .read(&reservation.token_id)
                .unwrap(),
            proposal_id
        );
        assert_eq!(
            factory
                .pending_ticker
                .read(&reservation.ticker_hash)
                .unwrap(),
            proposal_id
        );
        assert_eq!(
            factory.pending_address.read(&reservation.token).unwrap(),
            proposal_id
        );
        // Error is terminal: it no longer occupies the proposer's pending cap.
        assert_eq!(vote.pending_proposal_count_by_proposer(issuer).unwrap(), 0);
    }
    assert_eq!(
        provider
            .get_events(VOTE_ADDRESS)
            .iter()
            .filter(|event| {
                event.topics().first() == Some(&IVote::ProposalBondRefunded::SIGNATURE_HASH)
            })
            .count(),
        1
    );
    assert_eq!(
        provider
            .get_events(VOTE_ADDRESS)
            .iter()
            .filter(|event| {
                event.topics().first() == Some(&IVote::ProposalBondBurned::SIGNATURE_HASH)
            })
            .count(),
        0
    );
    assert!(provider.get_events(STABLECOIN_FACTORY_ADDRESS).is_empty());
    assert!(provider
        .get_account_info(reserved_token)
        .is_none_or(|account| account
            .code
            .as_ref()
            .is_none_or(|code| code.original_bytes().is_empty())));
}

#[test]
fn real_factory_vote_rejects_global_ticker_collision_before_allocation() {
    let issuer_a = Address::repeat_byte(0x11);
    let issuer_b = Address::repeat_byte(0x22);
    let raw_a = payload_with_ticker(issuer_a, "GLB");
    let raw_b = payload_with_ticker(issuer_b, "GLB");
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_balance(VOTE_ADDRESS, STABLECOIN_CREATE_BOND * U256::from(2u64));
    let storage = StorageHandle::new(&mut provider);
    let mut vote = Vote::new(storage.clone());
    let first = vote
        .create_proposal_with_value(
            outbe_vote::ProposalSubmission {
                proposer: issuer_a,
                target_module: STABLECOIN_FACTORY_ADDRESS,
                payload: core::str::from_utf8(&raw_a).unwrap(),
                created_height: 7,
                attached_value: STABLECOIN_CREATE_BOND,
            },
            registry(),
        )
        .unwrap();
    assert!(vote
        .create_proposal_with_value(
            outbe_vote::ProposalSubmission {
                proposer: issuer_b,
                target_module: STABLECOIN_FACTORY_ADDRESS,
                payload: core::str::from_utf8(&raw_b).unwrap(),
                created_height: 7,
                attached_value: STABLECOIN_CREATE_BOND,
            },
            registry(),
        )
        .is_err());
    assert_eq!(vote.proposal_count.read().unwrap(), first);
    assert_eq!(vote.bond_liabilities().unwrap(), STABLECOIN_CREATE_BOND);
    assert!(!StablecoinFactoryContract::new(storage)
        .reservations
        .exists(first + U256::from(1u64))
        .unwrap());
}

#[test]
fn factory_target_approved_expired_error_and_fatal_paths_are_distinct() {
    let issuer = Address::repeat_byte(0x11);
    let raw = payload(issuer);
    let target = registry().lookup(STABLECOIN_FACTORY_ADDRESS).unwrap();

    let mut approved_provider = HashMapStorageProvider::new(1);
    let approved_storage = StorageHandle::new(&mut approved_provider);
    target
        .reserve(
            approved_storage.clone(),
            U256::from(1u64),
            &raw,
            context(issuer),
        )
        .unwrap();
    let approved_ctx = BlockRuntimeContext::new(
        BlockContext::new(7, 1_700_000_000, 1, issuer, vec![issuer]),
        approved_storage.clone(),
    );
    assert_eq!(
        target
            .handle_tally(
                &approved_ctx,
                U256::from(1u64),
                &raw,
                context(issuer),
                ProposalStatus::Approved,
            )
            .unwrap(),
        TargetExecutionOutcome::Applied
    );
    assert_eq!(
        StablecoinFactoryContract::new(approved_storage)
            .token_count()
            .unwrap(),
        U256::from(1u64)
    );

    let mut expired_provider = HashMapStorageProvider::new(1);
    let expired_storage = StorageHandle::new(&mut expired_provider);
    target
        .reserve(
            expired_storage.clone(),
            U256::from(2u64),
            &raw,
            context(issuer),
        )
        .unwrap();
    let expired_ctx = BlockRuntimeContext::new(
        BlockContext::new(7, 1_700_000_000, 1, issuer, vec![issuer]),
        expired_storage.clone(),
    );
    assert_eq!(
        target
            .handle_tally(
                &expired_ctx,
                U256::from(2u64),
                &raw,
                context(issuer),
                ProposalStatus::Expired,
            )
            .unwrap(),
        TargetExecutionOutcome::Applied
    );
    assert!(!StablecoinFactoryContract::new(expired_storage)
        .reservations
        .exists(U256::from(2u64))
        .unwrap());

    let mut error_provider = HashMapStorageProvider::new(1);
    let error_storage = StorageHandle::new(&mut error_provider);
    target
        .reserve(
            error_storage.clone(),
            U256::from(3u64),
            &raw,
            context(issuer),
        )
        .unwrap();
    let error_ctx = BlockRuntimeContext::new(
        BlockContext::new(7, 1_700_000_000, 1, issuer, vec![issuer]),
        error_storage.clone(),
    );
    assert!(matches!(
        target
            .handle_tally(
                &error_ctx,
                U256::from(3u64),
                b"{",
                context(issuer),
                ProposalStatus::Approved,
            )
            .unwrap(),
        TargetExecutionOutcome::Error { .. }
    ));
    assert!(StablecoinFactoryContract::new(error_storage)
        .reservations
        .exists(U256::from(3u64))
        .unwrap());

    let mut fatal_provider = HashMapStorageProvider::new(1);
    let fatal_storage = StorageHandle::new(&mut fatal_provider);
    target
        .reserve(
            fatal_storage.clone(),
            U256::from(4u64),
            &raw,
            context(issuer),
        )
        .unwrap();
    let fatal_factory = StablecoinFactoryContract::new(fatal_storage.clone());
    let fatal_reservation = fatal_factory
        .reservations
        .get(U256::from(4u64))
        .unwrap()
        .unwrap();
    fatal_factory
        .pending_address
        .clear(&fatal_reservation.token)
        .unwrap();
    let fatal_ctx = BlockRuntimeContext::new(
        BlockContext::new(7, 1_700_000_000, 1, issuer, vec![issuer]),
        fatal_storage,
    );
    assert!(matches!(
        target.handle_tally(
            &fatal_ctx,
            U256::from(4u64),
            &raw,
            context(issuer),
            ProposalStatus::Approved,
        ),
        Err(PrecompileError::Fatal(_))
    ));
}

#[test]
fn pfs_010_08_fatal_creation_rolls_back_the_containing_block() {
    let issuer = Address::repeat_byte(0x11);
    let raw = payload(issuer);
    let raw_text = core::str::from_utf8(&raw).unwrap();
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_balance(VOTE_ADDRESS, STABLECOIN_CREATE_BOND);

    let (proposal_id, reservation) = {
        let storage = StorageHandle::new(&mut provider);
        setup_active_validators(storage.clone());
        let mut vote = Vote::new(storage.clone());
        let proposal_id = vote
            .create_proposal_with_value(
                outbe_vote::ProposalSubmission {
                    proposer: issuer,
                    target_module: STABLECOIN_FACTORY_ADDRESS,
                    payload: raw_text,
                    created_height: 7,
                    attached_value: STABLECOIN_CREATE_BOND,
                },
                registry(),
            )
            .unwrap();
        vote.cast_vote_approve(proposal_id, VALIDATOR_A, true, 8)
            .unwrap();
        vote.cast_vote_approve(proposal_id, VALIDATOR_B, true, 8)
            .unwrap();

        let factory = StablecoinFactoryContract::new(storage);
        let reservation = factory.reservations.get(proposal_id).unwrap().unwrap();
        factory.pending_address.clear(&reservation.token).unwrap();
        (proposal_id, reservation)
    };
    let vote_events_before = provider.get_events(VOTE_ADDRESS).len();
    let factory_events_before = provider.get_events(STABLECOIN_FACTORY_ADDRESS).len();

    {
        let storage = StorageHandle::new(&mut provider);
        let deadline = 7 + VOTING_WINDOW_BLOCKS;
        let ctx = finalize_context(storage.clone(), deadline + 1, issuer);
        let result = ctx
            .with_checkpoint(|| Vote::new(storage.clone()).process_begin_block(&ctx, registry()));
        assert!(matches!(result, Err(PrecompileError::Fatal(_))));

        let vote = Vote::new(storage.clone());
        assert_eq!(
            vote.proposals
                .get(proposal_id)
                .unwrap()
                .unwrap()
                .proposal_status()
                .unwrap(),
            ProposalStatus::Pending
        );
        assert_eq!(vote.pending_proposal_count_by_proposer(issuer).unwrap(), 1);
        assert_eq!(
            vote.proposal_bond(proposal_id).unwrap().settlement,
            BondSettlement::Unsettled
        );
        assert_eq!(vote.bond_liabilities().unwrap(), STABLECOIN_CREATE_BOND);
        assert_eq!(
            storage.balance(VOTE_ADDRESS).unwrap(),
            STABLECOIN_CREATE_BOND
        );
        assert_eq!(storage.balance(issuer).unwrap(), U256::ZERO);

        let factory = StablecoinFactoryContract::new(storage.clone());
        assert_eq!(factory.token_count().unwrap(), U256::ZERO);
        assert_eq!(
            factory
                .pending_token_id
                .read(&reservation.token_id)
                .unwrap(),
            proposal_id
        );
        assert_eq!(
            factory
                .pending_ticker
                .read(&reservation.ticker_hash)
                .unwrap(),
            proposal_id
        );
        assert!(factory
            .pending_address
            .read(&reservation.token)
            .unwrap()
            .is_zero());
        assert_eq!(
            factory.reservations.get(proposal_id).unwrap(),
            Some(reservation.clone())
        );
        assert_eq!(
            storage.sload(reservation.token, U256::ZERO).unwrap(),
            U256::ZERO
        );
    }

    assert_eq!(provider.get_events(VOTE_ADDRESS).len(), vote_events_before);
    assert_eq!(
        provider.get_events(STABLECOIN_FACTORY_ADDRESS).len(),
        factory_events_before
    );
    assert!(provider
        .get_account_info(reservation.token)
        .is_none_or(|account| account
            .code
            .as_ref()
            .is_none_or(|code| code.original_bytes().is_empty())));
}
