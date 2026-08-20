use super::*;

fn contrib(n: u8) -> Address {
    Address::from([n; 20])
}

/// A future fan-in deadline relative to the harness clock (`ISSUED_AT`).
const DEADLINE_FUTURE: u64 = ISSUED_AT as u64 + 1000;

#[test]
fn distribute_pays_contributors_proportionally_with_dust_to_last() {
    with_factory(|s| {
        let owners = [contrib(1), contrib(2), contrib(3)];
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(7),
            &[
                (owners[0], U256::from(100u64)),
                (owners[1], U256::from(200u64)),
                (owners[2], U256::from(300u64)),
            ],
        )
        .unwrap();
        // A single winning chain: its arrival completes the fan-in immediately.
        outbe_intex::api::arm_proceeds(&s, WorldwideDay::new(7), &[10], DEADLINE_FUTURE).unwrap();
        // Simulate the native value arriving on the precompile via distribute{value}.
        let amount = U256::from(1000u64);
        s.increase_balance(INTEX_FACTORY_ADDRESS, amount).unwrap();

        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7.into(),
            10,
            amount,
        )
        .unwrap();

        // distribute only registers; nothing is paid until the begin-block drain.
        assert_eq!(s.balance(owners[0]).unwrap(), U256::ZERO);
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        runtime::drain_distributions(&s).unwrap();

        // floor shares (amount * nominal / total); the last owner absorbs the
        // rounding remainder, so the sum is exactly `amount`.
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(166u64)); // 1000*100/600
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(333u64)); // 1000*200/600
        assert_eq!(s.balance(owners[2]).unwrap(), U256::from(501u64)); // 1000-166-333
                                                                       // precompile fully drained, progress + contributors cleared.
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(
            outbe_intex::api::get_progress(&s, WorldwideDay::new(7)).unwrap(),
            None
        );
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);
        assert_eq!(
            outbe_intex::api::contributor_count(&s, WorldwideDay::new(7)).unwrap(),
            0
        );
    });
}

#[test]
fn distribute_waits_for_all_winning_chains_then_pays_the_sum() {
    with_factory(|s| {
        let owners = [contrib(1), contrib(2)];
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(7),
            &[
                (owners[0], U256::from(100u64)),
                (owners[1], U256::from(100u64)),
            ],
        )
        .unwrap();
        outbe_intex::api::arm_proceeds(&s, WorldwideDay::new(7), &[10, 20], DEADLINE_FUTURE)
            .unwrap();

        // Chain 10 arrives first: pot accumulates, fan-in not complete → no payout yet.
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(300u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7.into(),
            10,
            U256::from(300u64),
        )
        .unwrap();
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);
        assert!(!outbe_intex::api::proceeds_ready(&s, WorldwideDay::new(7)).unwrap());

        // Chain 20 completes the fan-in: one distribution over the summed pot.
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(500u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7.into(),
            20,
            U256::from(500u64),
        )
        .unwrap();
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        runtime::drain_distributions(&s).unwrap();
        // 800 split 100:100 → 400 each; map cleared since every chain is in.
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(400u64));
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(400u64));
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(
            outbe_intex::api::contributor_count(&s, WorldwideDay::new(7)).unwrap(),
            0
        );
    });
}

#[test]
fn distribute_deadline_forces_partial_payout_then_late_chain_supplements() {
    with_factory(|s| {
        let owners = [contrib(1), contrib(2)];
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(7),
            &[
                (owners[0], U256::from(100u64)),
                (owners[1], U256::from(100u64)),
            ],
        )
        .unwrap();
        outbe_intex::api::arm_proceeds(&s, WorldwideDay::new(7), &[10, 20], DEADLINE_FUTURE)
            .unwrap();

        // Only chain 10 arrives before the deadline.
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(200u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7.into(),
            10,
            U256::from(200u64),
        )
        .unwrap();
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);

        // Past the deadline the sweep pays out what arrived; the map is retained.
        runtime::try_settle_proceeds(&s, WorldwideDay::new(7), DEADLINE_FUTURE + 1).unwrap();
        assert!(!outbe_intex::api::proceeds_finalize_on_done(&s, WorldwideDay::new(7)).unwrap());
        runtime::drain_distributions(&s).unwrap();
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(100u64));
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(100u64));
        assert_eq!(
            outbe_intex::api::contributor_count(&s, WorldwideDay::new(7)).unwrap(),
            2
        ); // retained

        // The straggler arrives: a supplementary payout over the same map, then finalize.
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(400u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7.into(),
            20,
            U256::from(400u64),
        )
        .unwrap();
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);
        runtime::drain_distributions(&s).unwrap();
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(300u64)); // +200
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(300u64));
        assert_eq!(
            outbe_intex::api::contributor_count(&s, WorldwideDay::new(7)).unwrap(),
            0
        ); // finalized
    });
}

#[test]
fn late_top_up_during_final_round_reaches_creators() {
    use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
    with_factory(|s| {
        let owners = [contrib(1), contrib(2)];
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(7),
            &[
                (owners[0], U256::from(100u64)),
                (owners[1], U256::from(100u64)),
            ],
        )
        .unwrap();
        // A single winning chain: the first arrival completes the fan-in, so the
        // round runs finalize-on-done (its end clears the contributor map).
        outbe_intex::api::arm_proceeds(&s, WorldwideDay::new(7), &[10], DEADLINE_FUTURE).unwrap();
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(200u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7.into(),
            10,
            U256::from(200u64),
        )
        .unwrap();
        assert!(outbe_intex::api::proceeds_finalize_on_done(&s, WorldwideDay::new(7)).unwrap());

        // Pay only the first contributor: the round is mid-drain, still active.
        runtime::pay_chunk(&s, WorldwideDay::new(7), 1).unwrap();
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(100u64));
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        // Chain 10 sends the rest of its proceeds while the round still drains: it
        // only tops the pot up (an in-flight round is never overlapped).
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(400u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7.into(),
            10,
            U256::from(400u64),
        )
        .unwrap();
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        // Finishing the round finds the topped-up pot and opens a supplementary
        // round over the same map instead of finalizing (which would strand it).
        runtime::pay_chunk(&s, WorldwideDay::new(7), 1).unwrap();
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(100u64));
        assert_eq!(
            outbe_intex::api::contributor_count(&s, WorldwideDay::new(7)).unwrap(),
            2
        ); // retained
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        // The supplementary round pays the top-up to the creators, then finalizes.
        runtime::drain_distributions(&s).unwrap();
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(300u64)); // +200
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(300u64)); // +200
        assert_eq!(
            outbe_intex::api::contributor_count(&s, WorldwideDay::new(7)).unwrap(),
            0
        ); // finalized
           // The money reached creators, never the vault or the burn path.
        assert_eq!(s.balance(VAULT_ROUTER_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
    });
}

#[test]
fn distribute_paginates_across_chunks() {
    with_factory(|s| {
        let owners = [contrib(1), contrib(2), contrib(3)];
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(7),
            &[
                (owners[0], U256::from(100u64)),
                (owners[1], U256::from(200u64)),
                (owners[2], U256::from(300u64)),
            ],
        )
        .unwrap();
        let amount = U256::from(600u64);
        s.increase_balance(INTEX_FACTORY_ADDRESS, amount).unwrap();
        outbe_intex::api::start_distribution(&s, WorldwideDay::new(7), amount, U256::from(600u64))
            .unwrap();

        // Chunk 1 (limit 2): pays the first two, cursor advances, still active.
        runtime::pay_chunk(&s, WorldwideDay::new(7), 2).unwrap();
        assert_eq!(s.balance(owners[0]).unwrap(), U256::from(100u64));
        assert_eq!(s.balance(owners[1]).unwrap(), U256::from(200u64));
        assert_eq!(s.balance(owners[2]).unwrap(), U256::ZERO);
        assert_eq!(
            outbe_intex::api::get_progress(&s, WorldwideDay::new(7))
                .unwrap()
                .unwrap()
                .cursor,
            2
        );
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);

        // Chunk 2: pays the last and finalizes.
        runtime::pay_chunk(&s, WorldwideDay::new(7), 2).unwrap();
        assert_eq!(s.balance(owners[2]).unwrap(), U256::from(300u64));
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(
            outbe_intex::api::get_progress(&s, WorldwideDay::new(7)).unwrap(),
            None
        );
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);
    });
}

#[test]
fn distribute_rejects_non_origin_router() {
    with_factory(|s| {
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(7),
            &[(contrib(1), U256::from(100u64))],
        )
        .unwrap();
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(100u64))
            .unwrap();
        let err = runtime::distribute(&s, holder(), 7.into(), 10, U256::from(100u64)).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("origin router"));
    });
}

#[test]
fn distribute_no_contributors_burns() {
    use alloy_sol_types::SolEvent;
    use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    storage.stub_sub_call_at(
        crate::constants::INTEX_NFT1155_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
    storage.stub_sub_call_at(
        crate::constants::ORIGIN_ROUTER_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );

    StorageHandle::enter(&mut storage, |s| {
        // Armed but no contributors recorded; the single chain completes the fan-in.
        outbe_intex::api::arm_proceeds(&s, WorldwideDay::new(7), &[10], DEADLINE_FUTURE).unwrap();
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(100u64))
            .unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            7.into(),
            10,
            U256::from(100u64),
        )
        .unwrap();

        // No distribution opened; the ownerless proceeds were destroyed, not vaulted.
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);
        assert_eq!(s.balance(VAULT_ROUTER_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
    });

    let sig = IIntexFactory::ProceedsBurned::SIGNATURE_HASH;
    let found = storage.get_events(INTEX_FACTORY_ADDRESS).iter().any(|log| {
        log.topics().first() == Some(&sig)
            && IIntexFactory::ProceedsBurned::decode_log_data(log)
                .map(|ev| ev.worldwideDay == 7 && ev.amount == U256::from(100u64))
                .unwrap_or(false)
    });
    assert!(found, "expected ProceedsBurned event");
}

#[test]
fn begin_block_drain_completes_active_distributions() {
    with_factory(|s| {
        // Two series, each left partially distributed (1 of 3 contributors paid).
        for (sid, owners) in [
            (7u32, [contrib(1), contrib(2), contrib(3)]),
            (9u32, [contrib(4), contrib(5), contrib(6)]),
        ] {
            outbe_intex::api::record_contributors(
                &s,
                WorldwideDay::new(sid),
                &[
                    (owners[0], U256::from(100u64)),
                    (owners[1], U256::from(200u64)),
                    (owners[2], U256::from(300u64)),
                ],
            )
            .unwrap();
            s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(600u64))
                .unwrap();
            outbe_intex::api::start_distribution(
                &s,
                WorldwideDay::new(sid),
                U256::from(600u64),
                U256::from(600u64),
            )
            .unwrap();
            // Pay only the first contributor, leaving the series active.
            runtime::pay_chunk(&s, WorldwideDay::new(sid), 1).unwrap();
        }
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 2);

        // One begin-block drain finishes both (3 <= DIST_CHUNK_LIMIT).
        runtime::drain_distributions(&s).unwrap();

        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 0);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        // series 7 fully paid
        assert_eq!(s.balance(contrib(1)).unwrap(), U256::from(100u64));
        assert_eq!(s.balance(contrib(2)).unwrap(), U256::from(200u64));
        assert_eq!(s.balance(contrib(3)).unwrap(), U256::from(300u64));
        // series 9 fully paid
        assert_eq!(s.balance(contrib(4)).unwrap(), U256::from(100u64));
        assert_eq!(s.balance(contrib(5)).unwrap(), U256::from(200u64));
        assert_eq!(s.balance(contrib(6)).unwrap(), U256::from(300u64));
    });
}

#[test]
fn begin_block_drain_isolates_failing_series() {
    with_factory(|s| {
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(7),
            &[(contrib(1), U256::from(100u64))],
        )
        .unwrap();
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(100u64))
            .unwrap();
        outbe_intex::api::start_distribution(
            &s,
            WorldwideDay::new(7),
            U256::from(100u64),
            U256::from(100u64),
        )
        .unwrap();

        // Series 9 is unfunded: its first transfer fails mid-chunk.
        outbe_intex::api::record_contributors(
            &s,
            WorldwideDay::new(9),
            &[
                (contrib(2), U256::from(100u64)),
                (contrib(3), U256::from(500u64)),
            ],
        )
        .unwrap();
        outbe_intex::api::start_distribution(
            &s,
            WorldwideDay::new(9),
            U256::from(600u64),
            U256::from(600u64),
        )
        .unwrap();

        // The drain must not error: the failing series is skipped and rolled back.
        runtime::drain_distributions(&s).unwrap();

        assert_eq!(s.balance(contrib(1)).unwrap(), U256::from(100u64));
        assert_eq!(outbe_intex::api::active_dist_count(&s).unwrap(), 1);
        let p = outbe_intex::api::get_progress(&s, WorldwideDay::new(9))
            .unwrap()
            .unwrap();
        assert_eq!(p.cursor, 0);
        assert_eq!(p.paid_so_far, U256::ZERO);
        assert_eq!(s.balance(contrib(2)).unwrap(), U256::ZERO);
    });
}

/// IntexFactory is a payable route, so the boundary credits value to its
/// address. Its dispatch must refuse value for every selector outside
/// `PAYABLE_SELECTORS`, or a funded call to a non-payable selector would strand
/// native value at an address with no accounting entry for it.
///
/// Characterization: the per-arm checks this replaced already covered these two
/// selectors with the same message, so the test pins current behavior rather
/// than proving a fix. Its value is catching a future removal of the guard.
#[test]
fn unpublished_selectors_refuse_native_value() {
    use crate::precompile::{dispatch, IIntexFactory};

    let calls = [
        IIntexFactory::settleCall {
            seriesId: Default::default(),
            intexHolder: Address::ZERO,
            amount: U256::ZERO,
            paymentToken: Address::ZERO,
        }
        .abi_encode(),
        IIntexFactory::setAuthorizedSettlerCall {
            seriesId: Default::default(),
            settler: Address::ZERO,
        }
        .abi_encode(),
    ];

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        for data in &calls {
            let funded = dispatch(storage.clone(), data, Address::ZERO, U256::from(1u64));
            assert!(
                matches!(
                    funded,
                    Err(outbe_primitives::error::PrecompileError::Revert(ref message))
                        if message == "non-payable function called with value"
                ),
                "unpublished selector must refuse value, got {funded:?}"
            );
        }
    });
}

/// `distribute` is the one published payable selector here, and it credits
/// auction proceeds straight from `msg.value`. The route table only decides that
/// this *address* may be credited; nothing in it proves the dispatch actually
/// hands the value to the handler. This does: the handler rejects a zero amount,
/// so the two outcomes separate exactly on whether the value arrived.
#[test]
fn distribute_receives_the_call_value() {
    use alloy_sol_types::SolCall;

    use crate::constants::ORIGIN_ROUTER_ADDRESS;
    use crate::precompile::{dispatch, IIntexFactory};

    let data = IIntexFactory::distributeCall {
        worldwideDay: 7u32,
        srcChainId: 10u32,
    }
    .abi_encode();

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let unfunded = dispatch(storage.clone(), &data, ORIGIN_ROUTER_ADDRESS, U256::ZERO);
        assert!(
            matches!(
                unfunded,
                Err(outbe_primitives::error::PrecompileError::Revert(ref message))
                    if message == "amount must be positive"
            ),
            "a zero-value distribute must reach the handler with nothing, got {unfunded:?}"
        );

        let funded = dispatch(
            storage.clone(),
            &data,
            ORIGIN_ROUTER_ADDRESS,
            U256::from(1_000u64),
        );
        assert!(
            !matches!(
                funded,
                Err(outbe_primitives::error::PrecompileError::Revert(ref message))
                    if message == "amount must be positive"
            ),
            "a funded distribute must reach the handler with the value, got {funded:?}"
        );
    });
}
