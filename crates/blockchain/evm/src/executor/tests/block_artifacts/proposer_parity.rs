//! Proposer and validator execution produce byte-equal receipts, roots and header artifacts,
//! including late finalize credit settlement.

use super::*;

#[test]
fn active_lifecycle_proposer_and_replay_match_receipts_roots_and_header_artifacts() {
    let run = |replay: bool| {
        let signer = test_evm_signer();
        let proposer = signer.address();
        let user = test_regular_tx()
            .try_into_recovered()
            .expect("regular tx signer recovers");
        let user_sender = Address(*user.signer());
        let mut state =
            state_with_active_proposer_and_funded_account_without_ocomp(proposer, user_sender);
        let chain_spec = test_chain_spec();
        let install = test_ocomp_fork_install(&chain_spec, &[(proposer, dummy_pubkey(0xA2))]);
        let config = OutbeEvmConfig::new_with_runtime_body_readers(
            chain_spec.clone(),
            RuntimeBodyReaders::new(Arc::new(MemoryStorage::new())),
        )
        .with_evm_signer(signer)
        .with_ocomp_lifecycle_activation(OcompLifecycleActivation::at_block(1))
        .with_ocomp_fork_install(install.clone());
        let begin = begin_system_txs_for_test(
            &config,
            BeginBlockFixture {
                block_number: 1,
                parent_hash: B256::ZERO,
                extra_data: &Bytes::new(),
                parent_consensus_metadata: None,
                proposer,
                bootstrap: BootstrapFixture::StandardForBlock,
            },
        );
        let end = config
            .build_end_system_txs(1, CHAIN_ID, begin.len(), Some(proposer))
            .expect("terminal system tx builds");

        let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
        let mut ctx = block_one_execution_ctx(Some(begin.len() + 1 + end.len()), Bytes::new());
        ctx.proposer_evm_address = Some(proposer);
        if replay {
            ctx.expected_begin_system_txs = begin.clone();
            ctx.expected_end_system_txs = end.clone();
        }
        let mut executor = config.create_executor(evm, ctx);

        executor
            .apply_pre_execution_changes()
            .expect("active pre-execution succeeds");
        for tx in begin {
            executor
                .execute_transaction(tx)
                .expect("begin system tx executes");
        }
        executor
            .execute_transaction(user)
            .expect("ordinary tx executes before CE sealing");
        executor
            .execute_transaction(end.into_iter().next().unwrap())
            .expect("terminal request executes after ordinary txs");

        let ce_root = executor
            .compressed_entities_seal_output()
            .expect("terminal request seals compressed entities")
            .new_root;
        executor
            .prepare_final_header_artifacts(0)
            .expect("final header artifacts encode");
        let final_extra_data = executor.final_extra_data.clone();
        let (evm, result) = executor.finish().expect("active block finishes");
        drop(evm);
        let state_root = post_state_root(&state.bundle_state);
        assert_persisted_fork_install(&mut state, chain_spec.chain().id(), &install)
            .expect("assert persisted fork install fixture succeeds");

        (
            result.receipts,
            result.gas_used,
            ce_root,
            final_extra_data,
            state_root,
        )
    };

    let proposer = run(false);
    let replay = run(true);
    assert_eq!(proposer, replay);
    assert_eq!(proposer.0.len(), 8);
}

/// Determinism gate: a block carries a valid BLS late-finalize credit. The block executes on two
/// inputs: the proposer's encoded `extra_data`, and the bytes that a validator decodes and
/// re-encodes. Both runs reach **identical** post-state and receipts. This proves that the
/// begin-zone late-credit verify+record path is deterministic across proposer and validator.
/// The codec round-trip pins artifact byte-identity here. The localnet harness covers full
/// proposer/validator lockstep end-to-end.
///
/// The block executes at `N+K`, so the begin-zone `settle_matured` is not a no-op. The begin-zone
/// actually **pays** a pre-seeded matured escrow (block `N`, non-zero fee, one credited voter at
/// `k=1`). The resulting fee-share **balance delta** (voter + drained `REWARDS`) must match
/// byte-for-byte on both the proposer and validator paths. This closes the gap where a zero
/// `validator_fee_sum` made settlement prove nothing.
#[test]
fn proposer_validator_same_state_root() {
    use outbe_primitives::reshare_artifact::decode_outbe_block_artifacts;

    // Mirror production startup, which installs the consensus chain id into the namespace
    // source of truth BEFORE anything signs or verifies. The executor below constructs
    // `OutbeEvmConfig`, which now installs it for every constructor. Install it here too, so
    // that the finalize aggregate signed below uses the same `finalize_namespace` that the
    // verify path reads. Otherwise the late-finalize BLS check fails on a namespace mismatch
    // (`b"outbe" || 0` at sign time vs `b"outbe" || CHAIN_ID` at verify time). CHAIN_ID
    // matches the Outbe Devnet identity that `test_chain_spec()` uses throughout this shared
    // lib-test process.
    outbe_consensus::proof::init_consensus_chain_id(CHAIN_ID).unwrap();

    let epoch = 0u64;
    // Real BLS committee of 4 (committee addresses are the late-credit voters).
    let (keys, addrs, snapshot, csh) = late_credit_committee(epoch);

    // Execute at block N+K so the begin-zone `settle_matured` is not a no-op.
    let window_k = outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K;
    let settle_block = window_k + 1; // K+1 = 4: first block where N=1 matures.
    let progress_marker = settle_block - 2; // CPA progress gate: last_accounted.

    // Live credit for the finalized parent (fb = settle_block - 1, distance 1), signers 0..2.
    // The credit targets the finalized parent (`parent_hash`). This is the same block that the
    // block-(N+K) CPA escrows. Thus `on_finalized_metadata` writes its canonical binding
    // (number->{fb_hash, epoch, committee_set_hash}), and the credit authenticates against it.
    // The CPA metadata carries no base voters, so the recording path records only the signers
    // of the late credit. This exercises the parity of the *recording* path.
    let (fb_number, view, parent_view) = (settle_block - 1, 9u64, 8u64);
    let parent_hash = B256::with_last_byte(0xAA);
    let fb_hash = parent_hash;

    // Pre-seeded MATURED escrow for block N = settle_block - K, with a non-zero fee and one
    // credited voter at k=1. `settle_matured(settle_block, K)` settles this block, so the
    // begin-zone actually PAYS. This proves that the fee-share balance delta is identical on
    // both paths. A distinct fb_hash and a dedicated voter address keep this concern isolated
    // from the live recording credit above.
    let settle_target = settle_block - window_k; // = 1
    let settle_fb_hash = B256::with_last_byte(0x11);
    let settle_voter = Address::with_last_byte(0x77);
    let settle_committee = 4u64;
    let settle_fee = U256::from(4_000u64) * outbe_primitives::units::NATIVE_UNITS_PER_PROTOCOL_UNIT;
    // payout_i = fee * w(1) / (committee * w_max) = 4000e12 * 100 / 400 = 1000e12.
    let expected_payout = settle_fee * outbe_rewards::constants::decay_weight(1)
        / outbe_rewards::constants::fixed_denominator(settle_committee);
    let artifact = signed_late_credit_artifact(
        &keys,
        &LateCreditBinding {
            fb_number,
            fb_hash,
            epoch,
            view,
            parent_view,
            csh,
        },
    )
    .expect("sign independent late credit fixture");

    // Proposer encodes; validator decodes the same bytes and re-encodes.
    let extra_proposer = encode_outbe_block_artifacts(&artifact).unwrap();
    let decoded = decode_outbe_block_artifacts(extra_proposer.as_ref()).unwrap();
    let extra_validator = encode_outbe_block_artifacts(&decoded).unwrap();
    assert_eq!(
        extra_proposer, extra_validator,
        "codec round-trip must be byte-identical (proposer encode == validator re-encode)"
    );

    // Execute block N+K with the begin-zone. Capture these values:
    // - the recorded late-credit state for the live credit,
    // - the fee-share balance of the settled voter,
    // - the drained REWARDS balance,
    // - the receipt shape.
    let run = |extra_data: Bytes| -> (usize, Vec<u64>, u32, Vec<Address>, U256, U256, u64) {
        let signer = test_evm_signer();
        let proposer = signer.address();
        let snapshot = snapshot.clone();
        // Register the committee members, so that the window-close absentee pass can slash
        // them. All four are absent for the settled block (which credited only
        // `settle_voter`). At a single miss this is counter-only (no felony). It adds no
        // balance effect, only the parity-checked miss counters.
        let mut seeded: Vec<(Address, [u8; 48])> = vec![(proposer, dummy_pubkey(0xA2))];
        for member in &snapshot.committee {
            seeded.push((member.address, member.consensus_pubkey));
        }
        let mut state = state_with_active_validators_seeded(&seeded, move |storage| {
            // The N+K CPA (on_finalized_metadata) writes the escrow binding of the live
            // credit. This closure pre-seeds the committee snapshot for the BLS verify of
            // the credit.
            seed_late_credit_escrow(
                storage,
                &LateCreditEscrow {
                    snapshot: &snapshot,
                    epoch,
                    settle_target,
                    settle_fb_hash,
                    settle_fee,
                    settle_committee,
                    settle_voter,
                    progress_marker,
                    csh,
                },
            )
            .expect("seed late credit escrow fixture succeeds");
        });
        let bridge = ConsensusExecutionBridge::new();
        bridge.record_execution_summary_with_state_root(
            fb_number,
            parent_hash,
            ExecutionSummaryArtifact {
                validator_fee_sum: U256::ZERO,
            },
            1,
            B256::repeat_byte(0x91),
        );
        let config = OutbeEvmConfig::new_with_bridge(test_chain_spec(), bridge)
            .with_evm_signer(signer.clone());
        let mut metadata = test_metadata();
        metadata.finalized_block_number = fb_number;
        metadata.finalized_block_hash = parent_hash;
        // Canonical binding that the CPA escrows. It must match the credit.
        metadata.finalized_epoch = epoch;
        metadata.finalized_view = view;
        metadata.parent_view = parent_view;
        metadata.committee_set_hash = csh;
        let evm_env = test_evm_env(settle_block, REWARDS_ADDRESS);
        let evm = config.evm_with_env(&mut state, evm_env);
        let mut ctx = execution_ctx(Some(0), extra_data.clone());
        ctx.inner.parent_hash = parent_hash;
        ctx.parent_consensus_metadata = Some(metadata.clone());
        let mut executor = config.create_executor(evm, ctx);

        // Phase 1 is disabled (no CPA cert seeded). Late-finalize verify runs on the valid
        // credit + seeded snapshot.
        super::with_phase1_verify_disabled(|| {
            executor
                .apply_pre_execution_changes()
                .expect("pre-exec ok for a valid credit + seeded snapshot");
        });
        let system_txs = begin_system_txs_for_test(
            &config,
            BeginBlockFixture {
                block_number: settle_block,
                parent_hash,
                extra_data: &extra_data,
                parent_consensus_metadata: Some(metadata),
                proposer,
                bootstrap: BootstrapFixture::StandardForBlock,
            },
        );
        for tx in system_txs {
            executor
                .execute_transaction(tx)
                .expect("begin-zone system tx executes");
        }
        let receipts_len = executor.receipts().len();
        let gas: Vec<u64> = executor
            .receipts()
            .iter()
            .map(|r| r.cumulative_gas_used)
            .collect();
        drop(executor);

        // Read the recorded live-credit voters + the settled fee-share balances.
        let read_ctx = BlockContext::new(settle_block, settle_block, CHAIN_ID, proposer, vec![]);
        let mut provider =
            outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read_ctx);
        let (count, voters, voter_balance, rewards_balance, absentee_miss) =
            StorageHandle::enter(&mut provider, |storage| {
                read_late_credit_settlement(
                    storage,
                    &LateCreditRead {
                        fb_hash,
                        addrs: &addrs,
                        settle_voter,
                    },
                )
            })
            .expect("read recorded late-credit + settlement state");
        (
            receipts_len,
            gas,
            count,
            voters,
            voter_balance,
            rewards_balance,
            absentee_miss,
        )
    };

    let proposer_out = run(extra_proposer);
    let validator_out = run(extra_validator);

    assert_eq!(
        proposer_out, validator_out,
        "proposer and validator must reach identical late-credit + settlement state"
    );
    // Recording parity: the live credit's three signers were recorded.
    assert_eq!(
        proposer_out.2, 3,
        "three voters recorded for the in-window credit"
    );
    assert_eq!(proposer_out.3, addrs[0..3].to_vec());
    // Settlement actually PAID: the k=1 voter received its decay-weighted fee-share, and
    // settlement drained the settled escrow from REWARDS.
    assert_eq!(
        proposer_out.4, expected_payout,
        "settled k=1 voter must receive fee * w(1) / D"
    );
    assert!(
        !expected_payout.is_zero(),
        "the strengthened test must prove a non-zero balance delta"
    );
    assert_eq!(
        proposer_out.5,
        U256::ZERO,
        "REWARDS is drained: payout transferred + residue burned"
    );
    // Window-close slash parity: the window-close pass records the miss of the absent
    // committee member (slash fired). The miss is byte-identical on the proposer and
    // validator paths. The tuple equality above already compares it.
    assert_eq!(
        proposer_out.6, 1,
        "absent committee voter is slashed (miss recorded) at window close on both paths"
    );
}

struct LateCreditEscrow<'a> {
    snapshot: &'a outbe_consensus::proof::CommitteeSnapshot,
    epoch: u64,
    settle_target: u64,
    settle_fb_hash: B256,
    settle_fee: U256,
    settle_committee: u64,
    settle_voter: Address,
    progress_marker: u64,
    csh: B256,
}

fn late_credit_committee(
    epoch: u64,
) -> (
    Vec<commonware_cryptography::bls12381::PrivateKey>,
    Vec<Address>,
    outbe_consensus::proof::CommitteeSnapshot,
    B256,
) {
    use commonware_codec::Encode as _;
    use commonware_cryptography::{bls12381, Signer as _};
    use commonware_math::algebra::Random as _;
    use outbe_consensus::proof::{committee_set_hash_v2, CommitteeEntry, CommitteeSnapshot};
    let keys: Vec<bls12381::PrivateKey> = (0..4)
        .map(|_| {
            bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
                rand_commonware::rngs::SysRng,
            ))
        })
        .collect();
    let addrs: Vec<Address> = (0..4).map(|i| Address::with_last_byte(i + 0x40)).collect();
    let snapshot = CommitteeSnapshot {
        committee: keys
            .iter()
            .zip(&addrs)
            .map(|(k, a)| {
                let mut pk = [0u8; 48];
                pk.copy_from_slice(&k.public_key().encode());
                CommitteeEntry {
                    address: *a,
                    consensus_pubkey: pk,
                }
            })
            .collect(),
        vrf_material_version: 1,
        vrf_group_public_key_bytes: vec![0x11; 96],
        vrf_public_polynomial_hash: alloy_primitives::B256::ZERO,
    };
    let csh = committee_set_hash_v2(epoch, &snapshot);

    (keys, addrs, snapshot, csh)
}

fn seed_late_credit_escrow(
    storage: StorageHandle<'_>,
    fixture: &LateCreditEscrow,
) -> eyre::Result<()> {
    let LateCreditEscrow {
        snapshot,
        epoch,
        settle_target,
        settle_fb_hash,
        settle_fee,
        settle_committee,
        settle_voter,
        progress_marker,
        csh,
    } = *fixture;
    outbe_validatorset::write_committee_snapshot(storage.clone(), epoch, snapshot)?;

    // Pre-seed the matured escrow (block N) and its k=1 voter. Fund REWARDS to back the
    // payout + residue burn. Advance the accounting marker, so that the N+K CPA progress
    // gate passes.
    let seed_ctx = BlockRuntimeContext::new(
        BlockContext::new(settle_target, 1, CHAIN_ID, Address::ZERO, vec![]),
        storage,
    );
    outbe_rewards::late_settlement::escrow_block_fee(
        &seed_ctx,
        settle_target,
        settle_fb_hash,
        settle_fee,
        settle_committee as u32,
        epoch,
        0, // canonical_view (block N is pre-seeded + settled, not live-credited)
        0, // canonical_parent_view
        csh,
        &[],
    )?;
    seed_ctx
        .storage
        .contract::<outbe_rewards::schema::Rewards>()
        .pending_reward_day
        .write(&settle_fb_hash, 19700101)?;
    outbe_rewards::late_settlement::record_late_credit(&seed_ctx, settle_fb_hash, settle_voter, 1)?;
    seed_ctx
        .storage
        .increase_balance(REWARDS_ADDRESS, settle_fee)?;
    outbe_accounting::record_phase1_progress(&seed_ctx, progress_marker)?;

    Ok(())
}

fn assert_persisted_fork_install(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    chain_id: u64,
    install: &outbe_metadosis::config::OcompForkInstallV1,
) -> eyre::Result<()> {
    let mut provider =
        super::DirectStorageProvider::new(state, BlockContext::empty_for_tests(1, 1, chain_id));
    let storage = StorageHandle::new(&mut provider);
    assert!(
        outbe_metadosis::api::is_active_ocomp_fork_install(storage, install)
            .expect("read persisted block-1 fork installation"),
        "block-1 lifecycle must persist the exact fork installation"
    );

    Ok(())
}

struct LateCreditBinding {
    fb_number: u64,
    fb_hash: B256,
    epoch: u64,
    view: u64,
    parent_view: u64,
    csh: B256,
}
fn signed_late_credit_artifact(
    keys: &[commonware_cryptography::bls12381::PrivateKey],
    binding: &LateCreditBinding,
) -> eyre::Result<OutbeBlockArtifacts> {
    use outbe_primitives::reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit};
    let LateCreditBinding {
        fb_number,
        fb_hash,
        epoch,
        view,
        parent_view,
        csh,
    } = *binding;
    let (aggregate_signature, signer_bitmap) = late_credit_signature(keys, binding)?;
    let artifact = OutbeBlockArtifacts {
        execution_summary: None,
        consensus_header_artifact: None,
        timestamp_millis_part: 0,
        late_finalize_credits: Some(LateFinalizeCreditsArtifact {
            batches: vec![PerBlockCredit {
                fb_number,
                fb_hash,
                epoch,
                view,
                parent_view,
                committee_set_hash: csh,
                signer_bitmap,
                aggregate_signature,
            }],
        }),
        compressed_entities_root: None,
    };

    Ok(artifact)
}

struct LateCreditRead<'a> {
    fb_hash: B256,
    addrs: &'a [Address],
    settle_voter: Address,
}
fn read_late_credit_settlement(
    storage: StorageHandle<'_>,
    fixture: &LateCreditRead<'_>,
) -> Result<(u32, Vec<Address>, U256, U256, u64), outbe_primitives::error::PrecompileError> {
    let LateCreditRead {
        fb_hash,
        addrs,
        settle_voter,
    } = *fixture;
    let r = outbe_rewards::contract::Rewards::new(storage.clone());
    let count = r.late_voter_count.read(&fb_hash)?;
    let at = r.late_voter_at.get_nested(&fb_hash);
    let mut voters = Vec::new();
    for i in 0..count {
        voters.push(at.read(&i)?);
    }
    // The real BLS late-credit phase also contributes to GEM,
    // attributed to the authenticated parent's timestamp (1).
    let reward_day = r.pending_reward_day.read(&fb_hash)?;
    assert_eq!(reward_day, 19700101);
    let participation = r.daily_participation.get_nested(&reward_day);
    for voter in &addrs[..3] {
        assert_eq!(participation.read(voter)?, 1);
    }
    assert_eq!(participation.read(&addrs[3])?, 0);
    assert_eq!(r.daily_total_participation.read(&reward_day)?, 4);
    let voter_balance = storage.balance(settle_voter)?;
    let rewards_balance = storage.balance(REWARDS_ADDRESS)?;
    // `addrs[3]` is a committee member absent for the settled block
    // and not in the live in-window credit -> a pure window-close
    // absentee. Its miss count must match on both paths.
    let si = outbe_slashindicator::contract::SlashIndicator::new(storage.clone());
    let absentee_miss = si.get_voter_miss_count(addrs[3])?;
    Ok::<_, outbe_primitives::error::PrecompileError>((
        count,
        voters,
        voter_balance,
        rewards_balance,
        absentee_miss,
    ))
}

fn late_credit_signature(
    keys: &[commonware_cryptography::bls12381::PrivateKey],
    binding: &LateCreditBinding,
) -> eyre::Result<([u8; 96], Vec<u8>)> {
    use commonware_codec::Encode as _;
    use commonware_consensus::simplex::types::Proposal;
    use commonware_consensus::types::{Epoch, Round, View};
    use commonware_cryptography::bls12381::{
        self,
        primitives::{ops::aggregate, variant::MinPk},
    };
    use commonware_cryptography::Signer as _;
    use outbe_consensus::digest::Digest as OutbeDigest;
    use outbe_consensus::proof::finalize_namespace;
    let LateCreditBinding {
        fb_hash,
        epoch,
        view,
        parent_view,
        ..
    } = *binding;
    let proposal = Proposal::new(
        Round::new(Epoch::new(epoch), View::new(view)),
        View::new(parent_view),
        OutbeDigest(fb_hash),
    );
    let msg = proposal.encode().to_vec();
    // Finalize votes bind the ordered committee. Build the canonical `Set` from the same
    // committee that the snapshot/verifier uses.
    let committee_set: commonware_utils::ordered::Set<bls12381::PublicKey> =
        commonware_utils::ordered::Set::from_iter_dedup(keys.iter().map(|k| k.public_key()));
    let sigs: Vec<bls12381::Signature> = [0usize, 1, 2]
        .iter()
        .map(|&i| keys[i].sign(&finalize_namespace(&committee_set), &msg))
        .collect();
    let agg = aggregate::combine_signatures::<MinPk, _>(
        commonware_utils::iter::NonEmpty::try_new(sigs.iter().map(|s| s.as_ref()))
            .ok_or_else(|| eyre::eyre!("three signatures must be nonempty"))?,
    );
    let mut aggregate_signature = [0u8; 96];
    aggregate_signature.copy_from_slice(&agg.encode());
    let mut signer_bitmap = vec![0u8; 4usize.div_ceil(8)];
    for i in [0usize, 1, 2] {
        signer_bitmap[i / 8] |= 1u8 << (i % 8);
    }
    Ok((aggregate_signature, signer_bitmap))
}
