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
        outbe_accounting::read_last_accounted_block_number(&ctx).unwrap()
    })
}

fn record_progress(provider: &mut HashMapStorageProvider, block_number: u64) {
    provider.enter(|storage| {
        let ctx = runtime_ctx(storage);
        outbe_accounting::record_phase1_progress(&ctx, block_number).unwrap();
    });
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
            PreloadedSystemTxContext {
                proposer: VALIDATOR,
                finalized_summary: Some(AccountedParentArtifact {
                    summary: outbe_primitives::reshare_artifact::ExecutionSummaryArtifact {
                        validator_fee_sum: U256::ZERO,
                    },
                    timestamp: 1_699_999_990,
                    state_root: Some(B256::repeat_byte(0x91)),
                }),
                allow_boundary_proposer: false,
                canonical_vrf_proof_hash: B256::repeat_byte(0xEF),
            },
            || dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO),
        )
    })
}

#[test]
fn dispatch_finalization_uses_preloaded_summary_not_calldata_money() {
    let mut provider = configured_storage(2, 1_700_000_000);
    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CertifiedFinality,
    );
    provider.enter(|storage| {
        let input = SystemTxInputV2::CertifiedParentAccounting {
            metadata: metadata(),
        }
        .encode()
        .unwrap();
        with_preloaded_system_tx_context(
            PreloadedSystemTxContext {
                proposer: VALIDATOR,
                finalized_summary: Some(AccountedParentArtifact {
                    summary: outbe_primitives::reshare_artifact::ExecutionSummaryArtifact {
                        validator_fee_sum: U256::ZERO,
                    },
                    timestamp: 1_699_999_990,
                    state_root: Some(B256::repeat_byte(0x92)),
                }),
                allow_boundary_proposer: false,
                canonical_vrf_proof_hash: B256::ZERO,
            },
            || dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO),
        )
        .unwrap();
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
            PreloadedSystemTxContext {
                proposer: VALIDATOR,
                finalized_summary: Some(AccountedParentArtifact {
                    summary: outbe_primitives::reshare_artifact::ExecutionSummaryArtifact {
                        validator_fee_sum: U256::ZERO,
                    },
                    timestamp: 1_699_999_990,
                    state_root: Some(B256::repeat_byte(0x93)),
                }),
                allow_boundary_proposer: false,
                canonical_vrf_proof_hash: B256::ZERO,
            },
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
