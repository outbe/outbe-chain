use super::*;

/// Phase 7b glue: `run_late_finalize_credits` at block `N+K` closes the
/// matured window - pays the escrowed voters, marks `fee_settled`, and routes
/// the unpaid residue through the active-profile carry-over sink.
/// Uses an empty credit artifact so the assertion isolates the
/// `settle_matured` + residue-recycle wiring (the BLS batch path is covered
/// by the verifier and `late_settlement` unit tests).
#[test]
fn late_finalize_window_close_settles_and_recycles_residue() {
    use outbe_primitives::addresses::REWARDS_ADDRESS;

    const V0: Address = address!("0x00000000000000000000000000000000000000A0");
    const V1: Address = address!("0x00000000000000000000000000000000000000A1");
    const V2: Address = address!("0x00000000000000000000000000000000000000A2");
    const V3: Address = address!("0x00000000000000000000000000000000000000A3");
    let fb_hash = B256::repeat_byte(0xAB);
    let timestamp = 1_700_000_000u64;

    // Block N+K = 13 settles block N = 10 (K = LATE_FINALIZE_WINDOW_K = 3).
    let mut provider = configured_storage(13, timestamp);
    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CertifiedFinality,
    );
    provider.enter(|storage| {
        let ctx = runtime_ctx(storage);

        let committee_size = 4u32;
        let pool = U256::from(4_000u64); // divisible by committee
        ctx.storage.increase_balance(REWARDS_ADDRESS, pool).unwrap();
        // Escrow block 10; only 3 of 4 voters credited at k=0 (one absent).
        outbe_rewards::late_settlement::escrow_block_fee(
            &ctx,
            10,
            fb_hash,
            pool,
            committee_size,
            0, // epoch
            0, // view
            0, // parent_view
            B256::ZERO,
            &[V0, V1, V2],
        )
        .unwrap();

        // Empty artifact: the mandatory phase still closes block 10's window.
        run_late_finalize_credits(&ctx, &LateFinalizeCreditsArtifact::default()).unwrap();

        let each = pool / U256::from(committee_size); // 1000, unchanged by exclusion
        assert_eq!(ctx.storage.balance(V0).unwrap(), each);
        assert_eq!(ctx.storage.balance(V1).unwrap(), each);
        assert_eq!(ctx.storage.balance(V2).unwrap(), each);
        assert_eq!(
            ctx.storage.balance(V3).unwrap(),
            U256::ZERO,
            "absent voter earns nothing"
        );
        // distributed (3*each) left REWARDS; residue (each) burned for parity.
        assert_eq!(
            ctx.storage.balance(REWARDS_ADDRESS).unwrap(),
            U256::ZERO,
            "REWARDS fully drained (3 paid + residue burned)"
        );
        assert!(
            ctx.storage
                .contract::<outbe_rewards::schema::Rewards>()
                .fee_settled
                .read(&fb_hash)
                .unwrap(),
            "window marked settled"
        );

        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(ctx.storage.clone())
                .get_total_unallocated()
                .unwrap(),
            each,
            "late-settlement residue is credited exactly once to carry-over"
        );
    });
}

/// at the inclusion-window close, a registered committee member
/// that never voted within `K` (`committee \ credited`) gets a voter miss
/// recorded in BOTH counters; a credited member does not. Proves the relocated,
/// now-punitive miss accounting runs against the FINAL credited set.
#[test]
fn window_close_records_miss_for_absent_committee_voter_only() {
    use outbe_validatorset::{CommitteeEntry, CommitteeSnapshot};

    const V0: Address = address!("0x00000000000000000000000000000000000000B0");
    const V1: Address = address!("0x00000000000000000000000000000000000000B1");
    let timestamp = 1_700_000_000u64;
    let epoch = 0u64;
    let fb_hash = B256::repeat_byte(0xE8);

    // Block N+K = 13 closes block N = 10 (K = LATE_FINALIZE_WINDOW_K = 3).
    let mut provider = configured_storage(13, timestamp);
    provider.enable_metadosis_mutation_frames(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CertifiedFinality,
        2,
    );
    provider.enter(|storage| {
        // Register both committee members so the strict registered-validator
        // contract of `record_finalized_participation` accepts them.
        {
            let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
            vs.register_validator(OWNER, V0, &[0xB0; 48]).unwrap();
            vs.activate_validator_via_boundary_for_test(V0).unwrap();
            vs.register_validator(OWNER, V1, &[0xB1; 48]).unwrap();
            vs.activate_validator_via_boundary_for_test(V1).unwrap();
        }

        // Committee snapshot [V0, V1] under (epoch, csh); escrow must bind csh.
        let snapshot = CommitteeSnapshot {
            committee: vec![
                CommitteeEntry {
                    address: V0,
                    consensus_pubkey: [0xB0; 48],
                },
                CommitteeEntry {
                    address: V1,
                    consensus_pubkey: [0xB1; 48],
                },
            ],
            vrf_material_version: 1,
            vrf_group_public_key_bytes: vec![0x11; 96],
            vrf_public_polynomial_hash: alloy_primitives::B256::ZERO,
        };
        let csh = outbe_validatorset::committee_set_hash_v2(epoch, &snapshot);
        outbe_validatorset::write_committee_snapshot(storage.clone(), epoch, &snapshot).unwrap();

        let ctx = runtime_ctx(storage);
        // Escrow block 10: committee of 2; only V0 credited at k=0 (V1 absent).
        ctx.storage
            .increase_balance(
                outbe_primitives::addresses::REWARDS_ADDRESS,
                U256::from(2_000u64),
            )
            .unwrap();
        outbe_rewards::late_settlement::escrow_block_fee(
            &ctx,
            10,
            fb_hash,
            U256::from(2_000u64),
            2,
            epoch,
            0,
            0,
            csh,
            &[V0],
        )
        .unwrap();

        // Close block 10's window: the absentee pass runs before settle.
        run_late_finalize_credits(&ctx, &LateFinalizeCreditsArtifact::default()).unwrap();

        let si = outbe_slashindicator::contract::SlashIndicator::new(ctx.storage.clone());
        assert_eq!(
            si.get_voter_miss_count(V1).unwrap(),
            1,
            "absent committee voter is counted missed at window close"
        );
        assert_eq!(
            si.get_voter_miss_count(V0).unwrap(),
            0,
            "credited voter is not counted missed"
        );
        let vs = outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone());
        assert_eq!(vs.participation(V1).unwrap().missed_votes, 1);
        assert_eq!(vs.participation(V0).unwrap().missed_votes, 0);

        // Replay the closed window: settle freed the escrow and the per-fb_hash
        // guards short-circuit, so re-running must not double-count.
        run_late_finalize_credits(&ctx, &LateFinalizeCreditsArtifact::default()).unwrap();
        assert_eq!(
            si.get_voter_miss_count(V1).unwrap(),
            1,
            "replay must not double-count the absentee miss"
        );
        assert_eq!(vs.participation(V1).unwrap().missed_votes, 1);
    });
}

/// Determinism: the window-close absentee pass is computed purely from committed
/// chain state (committee snapshot + `late_voter_*`) in committee order, with no
/// proposer-chosen input - so two independent executions of the same closed
/// window reach byte-identical slashing state (the proposer/validator guarantee).
/// Multiple absentees exercise ordering.
#[test]
fn window_close_absentee_pass_is_deterministic_and_correct() {
    use outbe_validatorset::{CommitteeEntry, CommitteeSnapshot};

    const C0: Address = address!("0x00000000000000000000000000000000000000C0");
    const C1: Address = address!("0x00000000000000000000000000000000000000C1");
    const C2: Address = address!("0x00000000000000000000000000000000000000C2");
    const C3: Address = address!("0x00000000000000000000000000000000000000C3");
    let epoch = 0u64;
    let fb_hash = B256::repeat_byte(0xD7);
    let members = [C0, C1, C2, C3];

    let run = || -> Vec<(u64, u64)> {
        let mut provider = configured_storage(13, 1_700_000_000);
        provider.enable_metadosis_mutation_frame(
            outbe_primitives::storage::MetadosisMutationPurposeTag::CertifiedFinality,
        );
        let mut out = Vec::new();
        provider.enter(|storage| {
            {
                let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
                for (i, a) in members.iter().enumerate() {
                    vs.test_register_validator_without_pop(*a, &[0xC0u8 + i as u8; 48])
                        .unwrap();
                    vs.activate_validator_via_boundary_for_test(*a).unwrap();
                }
            }
            let snapshot = CommitteeSnapshot {
                committee: members
                    .iter()
                    .enumerate()
                    .map(|(i, a)| CommitteeEntry {
                        address: *a,
                        consensus_pubkey: [0xC0u8 + i as u8; 48],
                    })
                    .collect(),
                vrf_material_version: 1,
                vrf_group_public_key_bytes: vec![0x11; 96],
                vrf_public_polynomial_hash: alloy_primitives::B256::ZERO,
            };
            let csh = outbe_validatorset::committee_set_hash_v2(epoch, &snapshot);
            outbe_validatorset::write_committee_snapshot(storage.clone(), epoch, &snapshot)
                .unwrap();

            let ctx = runtime_ctx(storage);
            ctx.storage
                .increase_balance(
                    outbe_primitives::addresses::REWARDS_ADDRESS,
                    U256::from(4_000u64),
                )
                .unwrap();
            // Credit C0 and C2 at k=0; C1 and C3 absent.
            outbe_rewards::late_settlement::escrow_block_fee(
                &ctx,
                10,
                fb_hash,
                U256::from(4_000u64),
                4,
                epoch,
                0,
                0,
                csh,
                &[C0, C2],
            )
            .unwrap();

            run_late_finalize_credits(&ctx, &LateFinalizeCreditsArtifact::default()).unwrap();

            let si = outbe_slashindicator::contract::SlashIndicator::new(ctx.storage.clone());
            let vs = outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone());
            for a in members {
                out.push((
                    si.get_voter_miss_count(a).unwrap(),
                    vs.participation(a).unwrap().missed_votes,
                ));
            }
        });
        out
    };

    let first = run();
    let second = run();
    assert_eq!(
        first, second,
        "window-close absentee pass must be deterministic across executions"
    );
    // C0, C2 credited -> no miss; C1, C3 absent -> miss in both counters.
    assert_eq!(
        first,
        vec![(0, 0), (1, 1), (0, 0), (1, 1)],
        "only the two absent committee members are missed"
    );
}

/// Boundary ordering: a block carrying certified `BoundaryOutcome` prepares
/// its per-epoch counter reset before the receipt-visible
/// `LateFinalizeCredits` body tx. The later BoundaryOutcome advances the
/// epoch/set/snapshot without resetting the freshly recorded absentee miss.
#[test]
fn window_close_miss_survives_epoch_boundary_reset() {
    use outbe_validatorset::{CommitteeEntry, CommitteeSnapshot};

    const A: Address = address!("0x00000000000000000000000000000000000000E0"); // credited
    const B: Address = address!("0x00000000000000000000000000000000000000E1"); // absent
    let epoch = 0u64;
    let fb_hash = B256::repeat_byte(0xE9);

    // Block 13 is an epoch boundary: configured_storage sets epoch_length=10,
    // epoch_start=0, so `is_epoch_boundary(13)` is true (13 >= 0 + 10).
    let mut provider = configured_storage(13, 1_700_000_000);
    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CertifiedFinality,
    );
    provider.enter(|storage| {
        {
            let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
            vs.register_validator(OWNER, A, &[0xE0; 48]).unwrap();
            vs.activate_validator_via_boundary_for_test(A).unwrap();
            vs.register_validator(OWNER, B, &[0xE1; 48]).unwrap();
            vs.activate_validator_via_boundary_for_test(B).unwrap();
        }
        let snapshot = CommitteeSnapshot {
            committee: vec![
                CommitteeEntry {
                    address: A,
                    consensus_pubkey: [0xE0; 48],
                },
                CommitteeEntry {
                    address: B,
                    consensus_pubkey: [0xE1; 48],
                },
            ],
            vrf_material_version: 1,
            vrf_group_public_key_bytes: vec![0x11; 96],
            vrf_public_polynomial_hash: alloy_primitives::B256::ZERO,
        };
        let csh = outbe_validatorset::committee_set_hash_v2(epoch, &snapshot);
        outbe_validatorset::write_committee_snapshot(storage.clone(), epoch, &snapshot).unwrap();

        let ctx = runtime_ctx(storage);
        ctx.storage
            .increase_balance(
                outbe_primitives::addresses::REWARDS_ADDRESS,
                U256::from(2_000u64),
            )
            .unwrap();
        outbe_rewards::late_settlement::escrow_block_fee(
            &ctx,
            10,
            fb_hash,
            U256::from(2_000u64),
            2,
            epoch,
            0,
            0,
            csh,
            &[A],
        )
        .unwrap();

        // B carries 5 misses accumulated earlier in the epoch.
        {
            let si = outbe_slashindicator::contract::SlashIndicator::new(ctx.storage.clone());
            si.voter_miss_count.write(&B, 5).unwrap();
        }

        // Real begin-zone order for a block that actually carries the
        // certified boundary: boundary-conditioned pre-block reset first...
        crate::executor::prepare_boundary_epoch_counters(
            ctx.storage.clone(),
            &boundary_noop(),
            ctx.block.block_number,
        )
        .unwrap();
        // ...then the begin-zone window-close increments.
        run_late_finalize_credits(&ctx, &LateFinalizeCreditsArtifact::default()).unwrap();

        let si = outbe_slashindicator::contract::SlashIndicator::new(ctx.storage.clone());
        assert_eq!(
            si.get_voter_miss_count(B).unwrap(),
            1,
            "absentee miss is recorded AFTER the reset (survives), not lost"
        );
        assert_eq!(si.get_voter_miss_count(A).unwrap(), 0);
    });
}

fn dummy_credit(fb_number: u64) -> outbe_primitives::reshare_artifact::PerBlockCredit {
    outbe_primitives::reshare_artifact::PerBlockCredit {
        fb_number,
        fb_hash: B256::repeat_byte(0xCD),
        epoch: 0,
        view: 9,
        parent_view: 8,
        committee_set_hash: B256::repeat_byte(0xEF),
        signer_bitmap: vec![0x01],
        aggregate_signature: [0u8; 96],
    }
}

/// #2 defense-in-depth: a body credit whose target is outside the K-block
/// inclusion window is FATAL (rejected before the snapshot read / BLS verify,
/// so no snapshot seeding is needed). distance = 13 - 5 = 8 > K = 3.
#[test]
fn late_finalize_out_of_window_credit_is_fatal() {
    let mut provider = configured_storage(13, 1_700_000_000);
    provider.enter(|storage| {
        let ctx = runtime_ctx(storage);
        let artifact = LateFinalizeCreditsArtifact {
            batches: vec![dummy_credit(5)],
        };
        let err = run_late_finalize_credits(&ctx, &artifact).unwrap_err();
        assert!(
            matches!(err, PrecompileError::Fatal(_)),
            "out-of-window credit must be Fatal, got {err:?}"
        );
        assert!(
            err.to_string().contains("outside inclusion window"),
            "{err}"
        );
    });
}

/// bad/unverifiable proof: an in-window, escrow-authenticated credit whose
/// committee snapshot does not exist is FATAL (the block aborts - never a soft
/// receipt). distance = 13 - 11 = 2 (in window); the escrow binding matches so
/// authentication passes and the snapshot lookup is reached and fails.
#[test]
fn late_finalize_unverifiable_credit_is_fatal() {
    let mut provider = configured_storage(13, 1_700_000_000);
    provider.enter(|storage| {
        let ctx = runtime_ctx(storage);
        let credit = dummy_credit(11);
        // Seed an escrow binding that matches the credit so it passes
        // authentication and reaches the (missing) snapshot lookup.
        outbe_rewards::late_settlement::escrow_block_fee(
            &ctx,
            credit.fb_number,
            credit.fb_hash,
            U256::from(1_000u64),
            4,
            credit.epoch,
            credit.view,
            credit.parent_view,
            credit.committee_set_hash,
            &[],
        )
        .unwrap();
        let artifact = LateFinalizeCreditsArtifact {
            batches: vec![credit],
        };
        let err = run_late_finalize_credits(&ctx, &artifact).unwrap_err();
        assert!(
            matches!(err, PrecompileError::Fatal(_)),
            "unverifiable credit must be Fatal, got {err:?}"
        );
        assert!(
            err.to_string().contains("missing committee snapshot"),
            "{err}"
        );
    });
}

/// a credit referencing a finalized block with no
/// escrow is rejected (the in-window distance passes, but there is nothing to
/// authenticate against).
#[test]
fn no_escrow_credit_rejected() {
    let mut provider = configured_storage(13, 1_700_000_000);
    provider.enter(|storage| {
        let ctx = runtime_ctx(storage);
        let artifact = LateFinalizeCreditsArtifact {
            batches: vec![dummy_credit(11)], // in window, but no escrow seeded
        };
        let err = run_late_finalize_credits(&ctx, &artifact).unwrap_err();
        assert!(err.to_string().contains("no escrow for fb_number"), "{err}");
    });
}

/// Seed an escrow binding `(11 -> fb_hash 0xCD, epoch 7, csh 0xEF)` and run a
/// credit that mismatches one field - each must be FATAL.
fn assert_auth_mismatch_fatal(
    mut mutate: impl FnMut(&mut outbe_primitives::reshare_artifact::PerBlockCredit),
    needle: &str,
) {
    let mut provider = configured_storage(13, 1_700_000_000);
    provider.enter(|storage| {
        let ctx = runtime_ctx(storage);
        // Canonical escrow for fb_number 11 (view/parent_view match dummy_credit).
        outbe_rewards::late_settlement::escrow_block_fee(
            &ctx,
            11,
            B256::repeat_byte(0xCD),
            U256::from(1_000u64),
            4,
            7, // epoch
            9, // view (dummy_credit default)
            8, // parent_view (dummy_credit default)
            B256::repeat_byte(0xEF),
            &[],
        )
        .unwrap();
        // Also escrow fb_number 12 (a different block) so a spoofed fb_number
        // hits a populated-but-wrong binding rather than an empty one.
        outbe_rewards::late_settlement::escrow_block_fee(
            &ctx,
            12,
            B256::repeat_byte(0xAA),
            U256::from(1_000u64),
            4,
            7, // epoch
            9, // view
            8, // parent_view
            B256::repeat_byte(0xEF),
            &[],
        )
        .unwrap();
        let mut credit = dummy_credit(11); // fb_hash 0xCD, epoch 0, csh 0xEF
        credit.epoch = 7; // match canonical unless the test overrides
        mutate(&mut credit);
        let artifact = LateFinalizeCreditsArtifact {
            batches: vec![credit],
        };
        let err = run_late_finalize_credits(&ctx, &artifact).unwrap_err();
        assert!(err.to_string().contains(needle), "{err}");
    });
}

/// BUG-2: spoofing `fb_number` (to shrink `k`) hits a wrong fb_hash binding.
#[test]
fn fb_number_mismatch_rejected() {
    // Real block (fb_hash 0xCD) is escrowed at 11; proposer claims fb_number
    // 12 (in window, distance 1) where a different block (0xAA) is escrowed.
    assert_auth_mismatch_fatal(|c| c.fb_number = 12, "fb_hash mismatch");
}

/// BUG-5: wrong epoch is rejected.
#[test]
fn wrong_epoch_rejected() {
    assert_auth_mismatch_fatal(|c| c.epoch = 9, "epoch mismatch");
}

/// BUG-5: wrong committee_set_hash is rejected.
#[test]
fn wrong_committee_set_hash_rejected() {
    assert_auth_mismatch_fatal(
        |c| c.committee_set_hash = B256::repeat_byte(0x99),
        "committee_set_hash mismatch",
    );
}

/// Review #1b (full binding): a credit with correct fb_number/fb_hash/epoch/
/// committee_set_hash but a non-canonical `view` is rejected at body auth -
/// closing the cross-view equivocation credit the pre-exec BLS verify (which
/// only ties the credit's view to its signatures) would otherwise let through.
#[test]
fn wrong_view_rejected() {
    assert_auth_mismatch_fatal(|c| c.view = 99, "view mismatch");
}

/// Review #1b (full binding): a non-canonical `parent_view` is rejected.
#[test]
fn wrong_parent_view_rejected() {
    assert_auth_mismatch_fatal(|c| c.parent_view = 99, "parent_view mismatch");
}
