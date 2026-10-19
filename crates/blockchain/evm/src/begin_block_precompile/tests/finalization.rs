use super::*;

fn metadata_for_parent(parent_number: u64, parent_hash: B256) -> CertifiedParentAccountingMetadata {
    let mut metadata = metadata();
    metadata.finalized_block_number = parent_number;
    metadata.finalized_block_hash = parent_hash;
    metadata.finalized_view = parent_number.saturating_add(1);
    metadata.parent_view = parent_number;
    metadata
}

fn read_progress(provider: &mut HashMapStorageProvider) -> u64 {
    provider.enter(|storage| {
        let ctx = runtime_ctx(storage);
        outbe_cycle::read_last_accounted_block_number(&ctx).unwrap()
    })
}

fn record_progress(provider: &mut HashMapStorageProvider, block_number: u64) {
    provider.enter(|storage| {
        let ctx = runtime_ctx(storage);
        outbe_cycle::record_phase1_progress(&ctx, block_number).unwrap();
    });
}

fn finalized_dispatch_context(
    state_root: B256,
    canonical_vrf_proof_hash: B256,
) -> PreloadedSystemTxContext {
    PreloadedSystemTxContext {
        proposer: VALIDATOR,
        finalized_summary: Some(AccountedParentArtifact {
            summary: outbe_primitives::reshare_artifact::ExecutionSummaryArtifact {
                validator_fee_sum: U256::ZERO,
            },
            timestamp: 1_699_999_990,
            state_root: Some(state_root),
        }),
        allow_boundary_proposer: false,
        canonical_vrf_proof_hash,
    }
}

fn dispatch_phase1(
    provider: &mut HashMapStorageProvider,
    metadata: CertifiedParentAccountingMetadata,
) -> Result<Bytes> {
    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CertifiedFinality,
    );
    provider.enter(|storage| {
        let input = SystemTxInputV2::CertifiedParentAccounting { metadata }
            .encode()
            .unwrap();
        with_preloaded_system_tx_context(
            finalized_dispatch_context(B256::repeat_byte(0x91), B256::repeat_byte(0xEF)),
            || dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO),
        )
    })
}

#[test]
fn dispatch_finalization_uses_preloaded_summary_not_calldata_money() {
    let mut provider = configured_storage(2, 1_700_000_000);
    let preloaded_fee = U256::from(1_234u64);
    let parent_timestamp = 1_699_999_990u64;
    let parent_day = outbe_primitives::time::timestamp_to_date_key(parent_timestamp);
    let vrf_hash = B256::repeat_byte(0xAB);
    let meta = metadata();
    let fb_hash = meta.finalized_block_hash;

    let context = PreloadedSystemTxContext {
        proposer: VALIDATOR,
        finalized_summary: Some(AccountedParentArtifact {
            summary: outbe_primitives::reshare_artifact::ExecutionSummaryArtifact {
                validator_fee_sum: preloaded_fee,
            },
            timestamp: parent_timestamp,
            state_root: Some(B256::repeat_byte(0x92)),
        }),
        allow_boundary_proposer: false,
        canonical_vrf_proof_hash: vrf_hash,
    };

    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CertifiedFinality,
    );
    provider.enter(|storage| {
        storage
            .increase_balance(outbe_primitives::addresses::REWARDS_ADDRESS, preloaded_fee)
            .unwrap();
        let input = SystemTxInputV2::CertifiedParentAccounting {
            metadata: meta.clone(),
        }
        .encode()
        .unwrap();
        with_preloaded_system_tx_context(context.clone(), || {
            dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO)
        })
        .unwrap();
    });

    assert_eq!(read_progress(&mut provider), 1);
    provider.enter(|storage| {
        let rewards = storage.contract::<outbe_rewards::schema::Rewards>();
        assert_eq!(rewards.pending_fees.read(&fb_hash).unwrap(), preloaded_fee);
        assert_eq!(
            rewards.daily_fee_sum_raw.read(&parent_day).unwrap(),
            preloaded_fee
        );
        assert_eq!(rewards.late_voter_count.read(&fb_hash).unwrap(), 1);
        assert_ne!(
            rewards
                .metadata_fingerprint_for_block
                .read(&fb_hash)
                .unwrap(),
            B256::ZERO
        );
    });

    // Supported identical replay succeeds without duplicating fee escrow or participation.
    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CertifiedFinality,
    );
    provider.enter(|storage| {
        let input = SystemTxInputV2::CertifiedParentAccounting {
            metadata: meta.clone(),
        }
        .encode()
        .unwrap();
        with_preloaded_system_tx_context(context, || {
            dispatch(storage.clone(), &input, SYSTEM_ADDRESS, U256::ZERO)
        })
        .unwrap();

        let rewards = storage.contract::<outbe_rewards::schema::Rewards>();
        assert_eq!(rewards.pending_fees.read(&fb_hash).unwrap(), preloaded_fee);
        assert_eq!(
            rewards.daily_fee_sum_raw.read(&parent_day).unwrap(),
            preloaded_fee
        );
        assert_eq!(rewards.late_voter_count.read(&fb_hash).unwrap(), 1);
    });
}

#[test]
fn phase1_reorg_same_height_uses_parent_state_progress_not_abandoned_post_state() {
    const CHILD_BLOCK: u64 = 27;
    const PARENT_BLOCK: u64 = CHILD_BLOCK - 1;
    const REQUIRED_PREVIOUS: u64 = PARENT_BLOCK - 1;

    let mut base = configured_storage(CHILD_BLOCK, 1_700_000_000);
    record_progress(&mut base, REQUIRED_PREVIOUS);
    let parent_state = base.storage.clone();

    let mut branch_a = provider_from_storage(CHILD_BLOCK, 1_700_000_000, parent_state.clone());
    dispatch_phase1(
        &mut branch_a,
        metadata_for_parent(PARENT_BLOCK, B256::repeat_byte(0xA1)),
    )
    .expect("branch A phase1 commits");
    assert_eq!(read_progress(&mut branch_a), PARENT_BLOCK);

    let mut branch_b_from_parent = provider_from_storage(CHILD_BLOCK, 1_700_000_000, parent_state);
    dispatch_phase1(
        &mut branch_b_from_parent,
        metadata_for_parent(PARENT_BLOCK, B256::repeat_byte(0xB2)),
    )
    .expect("same-height branch B must commit from its own parent state");
    assert_eq!(read_progress(&mut branch_b_from_parent), PARENT_BLOCK);

    let mut branch_b_from_abandoned_post_state =
        provider_from_storage(CHILD_BLOCK, 1_700_000_000, branch_a.storage.clone());
    let err = dispatch_phase1(
        &mut branch_b_from_abandoned_post_state,
        metadata_for_parent(PARENT_BLOCK, B256::repeat_byte(0xB2)),
    )
    .expect_err("branch B must not use abandoned branch A post-state");
    assert!(
        err.to_string()
            .contains("CertifiedParentAccounting progress gap"),
        "expected progress-gap failure, got {err}"
    );
}

#[test]
fn dispatch_finalization_counts_duplicate_missed_proposer_events_by_index() {
    let mut provider = configured_storage(2, 1_700_000_000);
    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CertifiedFinality,
    );
    provider.enter(|storage| {
        let mut metadata = metadata();
        metadata.missed_proposers = vec![
            outbe_primitives::consensus_metadata::MissedProposerEvent {
                view: 1,
                validator: VALIDATOR,
            },
            outbe_primitives::consensus_metadata::MissedProposerEvent {
                view: 2,
                validator: VALIDATOR,
            },
        ];
        let input = SystemTxInputV2::CertifiedParentAccounting { metadata }
            .encode()
            .unwrap();
        with_preloaded_system_tx_context(
            finalized_dispatch_context(B256::repeat_byte(0x93), B256::ZERO),
            || dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO),
        )
        .unwrap();
    });

    provider.enter(|storage| {
        let si = outbe_slashindicator::contract::SlashIndicator::new(storage);
        assert_eq!(si.proposer_miss_count.read(&VALIDATOR).unwrap(), 2);
    });
}

#[test]
fn finalization_rejects_non_parent_metadata_before_summary_use() {
    let mut provider = configured_storage(3, 1_700_000_000);
    provider.enter(|storage| {
        let input = SystemTxInputV2::CertifiedParentAccounting {
            metadata: metadata(),
        }
        .encode()
        .unwrap();
        let err = dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO).unwrap_err();
        assert!(err
            .to_string()
            .contains("CertifiedParentAccounting metadata must target immediate parent"));
    });
}

#[test]
fn accounting_progress_address_is_ee04_and_user_calls_do_not_dispatch() {
    use outbe_primitives::addresses::ACCOUNTING_PROGRESS_ADDRESS;
    let expected: Address = "0x000000000000000000000000000000000000EE04"
        .parse()
        .expect("valid 0xEE04 address literal");
    assert_eq!(
        ACCOUNTING_PROGRESS_ADDRESS, expected,
        "INV1: ACCOUNTING_PROGRESS_ADDRESS must be exactly 0x...EE04",
    );
    assert!(
        !crate::precompiles::outbe_precompile_addresses().contains(&ACCOUNTING_PROGRESS_ADDRESS),
        "ACCOUNTING_PROGRESS_ADDRESS must NOT be a registered dispatching precompile \
         (outbe_precompile_addresses) - it is a system-only marker; user CALLs must not dispatch",
    );
}

#[test]
fn accounting_progress_address_is_eip161_preserved() {
    use outbe_primitives::addresses::ACCOUNTING_PROGRESS_ADDRESS;
    assert!(
        crate::executor::marker_addresses::OUTBE_RUNTIME_MARKER_ADDRESSES
            .contains(&ACCOUNTING_PROGRESS_ADDRESS),
        "INV3: ACCOUNTING_PROGRESS_ADDRESS must be in the executor's EIP-161 marker \
         allowlist (OUTBE_RUNTIME_MARKER_ADDRESSES) so slot 0 survives state-root cleanup",
    );
}
