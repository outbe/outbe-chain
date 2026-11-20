use super::*;

#[test]
fn multi_league_lysis_and_issue_nod_keep_league_nominals_fractions_and_loads_private() {
    use alloy_sol_types::SolCall;

    let mut provider = HashMapStorageProvider::new(1);
    let base_ts = 1_750_000_000u64;
    provider.set_timestamp(U256::from(base_ts));
    provider.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 3);

    let day: outbe_system_common::time::WorldwideDay = 20241221u32.into();
    let owner1 = address!("0x1111111111111111111111111111111111111111");
    let owner2 = address!("0x2222222222222222222222222222222222222222");
    let owner3 = address!("0x3333333333333333333333333333333333333333");

    let (nod1_id, nod2_id, f1, f2, load1, load2, cost1, cost2) =
        StorageHandle::enter(&mut provider, |storage| {
            let _enclave = crate::enclave_client::test_enclave::scope();
            let mut tc = TributeContract::new(storage.clone());

            let mut t1 = sample_tribute();
            set_owner(&mut t1, owner1);
            set_day(&mut t1, day);
            t1.issuance_amount_minor = U256::from(400_000_000u64);
            t1.nominal_amount_minor = U256::from(200_000_000u64);

            let mut t2 = sample_tribute();
            set_owner(&mut t2, owner2);
            set_day(&mut t2, day);
            t2.issuance_amount_minor = U256::from(800_000_000u64);
            t2.nominal_amount_minor = U256::from(400_000_000u64);

            let mut t3 = sample_tribute();
            set_owner(&mut t3, owner3);
            set_day(&mut t3, day);
            t3.issuance_amount_minor = U256::from(800_000_000u64);
            t3.nominal_amount_minor = U256::from(400_000_000u64);

            let price_day = outbe_nod::openings::entry_price_day(base_ts).unwrap();
            seed_entry_price(&storage, 840, price_day, U256::from(2_000_000u64));
            tc.issue_with_league(&t1, Some(100)).unwrap();
            tc.issue_with_league(&t2, Some(200)).unwrap();
            tc.issue_with_league(&t3, Some(200)).unwrap();
            in_process_ready(&storage, || tc.seal_worldwide_day(day));

            let binding = metadosis_process_ready_binding(1, 0, base_ts);
            storage
                .with_scoped_mutation::<CycleLifecyclePurpose, _>(binding, || {
                    seal_lysis(&mut tc, day, U256::from(500_000_000u64), base_ts)
                })
                .unwrap();

            outbe_emission::api::link_wwd(storage.clone(), price_day, day).unwrap();
            let totals_call = crate::precompile::ITribute::dayTotalsCall {
                emissionDay: price_day,
            }
            .abi_encode();
            let totals_before = crate::precompile::ITribute::dayTotalsCall::abi_decode_returns(
                &crate::precompile::dispatch(storage.clone(), &totals_call, owner1, U256::ZERO)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(totals_before.tributeCount, 3);
            assert!(totals_before.isSealed);

            let series =
                outbe_core_common::series_id::SeriesId::for_pair(price_day, 840, 840).unwrap();
            let group_key = outbe_nod::api::series_group(&storage, series)
                .unwrap()
                .unwrap();
            let nod_contract = outbe_nod::NodContract::new(storage.clone());
            let open_load_ct_before = nod_contract.encrypted_group_open_load(group_key).unwrap();
            assert!(!open_load_ct_before.is_empty());

            assert!(!tc
                .encrypted_day_fractions(EmissionDate::new(price_day))
                .unwrap()
                .is_empty());

            let f1 = test_day_league_fraction(&tc, day, 100);
            let f2 = test_day_league_fraction(&tc, day, 200);
            assert!(f1 > 0 && f2 > 0 && f1 != f2);
            let nod1_id = tc.issue_nod(owner1, t1.tribute_id).unwrap();

            // `group_open_load_ct` and `tributeNominalTotalMinor` must be completely unchanged after `issueNod`.
            let open_load_ct_after = nod_contract.encrypted_group_open_load(group_key).unwrap();
            assert_eq!(open_load_ct_after, open_load_ct_before);
            let totals_after = crate::precompile::ITribute::dayTotalsCall::abi_decode_returns(
                &crate::precompile::dispatch(storage.clone(), &totals_call, owner1, U256::ZERO)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(totals_after.tributeCount, 2);
            assert_eq!(
                totals_after.tributeNominalTotalMinor,
                totals_before.tributeNominalTotalMinor
            );

            let nod2_id = tc.issue_nod(owner2, t2.tribute_id).unwrap();
            let rec1 = outbe_nod::api::get_item(&storage, nod1_id)
                .unwrap()
                .unwrap();
            let rec2 = outbe_nod::api::get_item(&storage, nod2_id)
                .unwrap()
                .unwrap();
            let view_key1 = outbe_nod::nod_encryption::derive_view_key(
                &crate::enclave_client::test_enclave::state_key(),
                owner1,
            )
            .unwrap();
            let view_key2 = outbe_nod::nod_encryption::derive_view_key(
                &crate::enclave_client::test_enclave::state_key(),
                owner2,
            )
            .unwrap();
            let load1 =
                outbe_nod::test_support::decrypt_for_owner(&view_key1, &rec1.encrypted).unwrap();
            let load2 =
                outbe_nod::test_support::decrypt_for_owner(&view_key2, &rec2.encrypted).unwrap();
            assert_eq!(
                load1,
                t1.nominal_amount_minor * U256::from(f1) / U256::from(1_000_000u64)
            );
            assert_eq!(
                load2,
                t2.nominal_amount_minor * U256::from(f2) / U256::from(1_000_000u64)
            );
            let cost1 =
                outbe_nod::api::settlement_cost_minor(U256::from(2_000_000u64), load1).unwrap();
            let cost2 =
                outbe_nod::api::settlement_cost_minor(U256::from(2_000_000u64), load2).unwrap();
            (nod1_id, nod2_id, f1, f2, load1, load2, cost1, cost2)
        });

    let settlement_asset = address!("0x7171717171717171717171717171717171717171");
    let asset_vaults_count_selector: [u8; 4] =
        alloy_primitives::keccak256(b"assetVaultsCount(address)")[..4]
            .try_into()
            .unwrap();
    provider.stub_sub_call_at_selector(
        outbe_system_common::addresses::VAULT_ROUTER_ADDRESS,
        asset_vaults_count_selector,
        alloy_primitives::Bytes::from(U256::ONE.to_be_bytes::<32>()),
    );
    provider.stub_sub_call_at_selector(
        settlement_asset,
        outbe_nodfactory::sol_ext::IReferenceCurrency::isoCodeCall::SELECTOR,
        alloy_primitives::Bytes::from(
            outbe_nodfactory::sol_ext::IReferenceCurrency::isoCodeCall::abi_encode_returns(&840u16),
        ),
    );
    provider.stub_sub_call_at_selector(
        settlement_asset,
        outbe_nodfactory::sol_ext::IERC20::decimalsCall::SELECTOR,
        alloy_primitives::Bytes::from(
            outbe_nodfactory::sol_ext::IERC20::decimalsCall::abi_encode_returns(&6u8),
        ),
    );
    StorageHandle::enter(&mut provider, |storage| {
        let _enclave = crate::enclave_client::test_enclave::scope();
        let quote_call = outbe_nodfactory::precompile::INodFactory::quoteSettlementCall {
            nodId: nod1_id,
            asset: settlement_asset,
        }
        .abi_encode();
        let non_owner_err = outbe_nodfactory::precompile::dispatch(
            storage.clone(),
            &quote_call,
            owner2,
            U256::ZERO,
        )
        .unwrap_err();
        assert_eq!(
            non_owner_err.to_string(),
            PrecompileError::from(outbe_nodfactory::errors::NodFactoryError::NotNodOwner)
                .to_string()
        );
        let quote_raw =
            outbe_nodfactory::precompile::dispatch(storage, &quote_call, owner1, U256::ZERO)
                .unwrap();
        let quote_ret =
            outbe_nodfactory::precompile::INodFactory::quoteSettlementCall::abi_decode_returns(
                &quote_raw,
            )
            .unwrap();
        let view_key1 = outbe_nod::nod_encryption::derive_view_key(
            &crate::enclave_client::test_enclave::state_key(),
            owner1,
        )
        .unwrap();
        let decrypted_quote = outbe_nod::test_support::decrypt_quote(
            &view_key1,
            &outbe_core_common::nod_encryption::NodQuoteContextV1 {
                chain_id: 1,
                nod_id: nod1_id,
                owner: owner1,
                asset: settlement_asset,
                settlement_currency: quote_ret.settlementCurrency,
                snapshot_id: quote_ret.snapshotId,
            },
            &quote_ret.encryptedPaymentMinor,
        )
        .unwrap();
        assert_eq!(decrypted_quote, cost1);
    });

    let forbidden_words = [
        U256::from(200_000_000u64),
        U256::from(400_000_000u64),
        U256::from(800_000_000u64),
        U256::from(f1),
        U256::from(f2),
        load1,
        load2,
        cost1,
        cost2,
    ];
    for ((addr, _slot), value) in &provider.storage {
        if *addr == TRIBUTE_ADDRESS || *addr == outbe_system_common::addresses::NOD_ADDRESS {
            assert!(
                !forbidden_words.contains(value),
                "sensitive value {value} leaked into storage of {addr}"
            );
        }
    }
    StorageHandle::enter(&mut provider, |storage| {
        for nod_id in [nod1_id, nod2_id] {
            let call = outbe_nod::precompile::INod::getNodCall { nodId: nod_id }.abi_encode();
            let raw = outbe_nod::precompile::dispatch(storage.clone(), &call, owner1, U256::ZERO)
                .unwrap();
            for chunk in raw.chunks_exact(32) {
                let word = U256::from_be_slice(chunk);
                assert!(
                    !forbidden_words.contains(&word),
                    "sensitive value {word} leaked in getNod({nod_id})"
                );
            }
        }
    });
}
