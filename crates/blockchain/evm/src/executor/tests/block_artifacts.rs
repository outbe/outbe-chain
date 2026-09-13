use super::*;

alloy_sol_types::sol! {
    event DepositEvent(
        bytes pubkey,
        bytes withdrawal_credentials,
        bytes amount,
        bytes signature,
        bytes index
    );
}

#[test]
fn compressed_entities_header_semantics_reject_missing_wrong_scheme_and_wrong_root() {
    let root = B256::repeat_byte(0xA1);
    assert!(validate_compressed_entities_root_scheme(None)
        .unwrap_err()
        .to_string()
        .contains("missing compressed-entities root artifact"));
    assert!(
        validate_compressed_entities_root_scheme(Some(CompressedEntitiesRootArtifact {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME + 1,
            r_sealed: root,
        }))
        .unwrap_err()
        .to_string()
        .contains("scheme mismatch")
    );
    assert!(validate_compressed_entities_root_after_seal(
        Some(CompressedEntitiesRootArtifact {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            r_sealed: B256::ZERO,
        }),
        root,
    )
    .unwrap_err()
    .to_string()
    .contains("header/SealOutput root mismatch"));
    assert_eq!(
        validate_compressed_entities_root_after_seal(
            Some(CompressedEntitiesRootArtifact {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                r_sealed: root,
            }),
            root,
        )
        .unwrap()
        .r_sealed,
        root
    );
}

fn state_with_active_proposer_and_funded_account_without_ocomp(
    proposer: Address,
    funded: Address,
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    state_with_active_proposer_and_funded_account_fixture(proposer, funded, false)
}

#[test]
fn pending_rpc_context_opens_ce_scope_but_skips_consensus_hooks() {
    let user_tx = test_regular_tx()
        .try_into_recovered()
        .expect("regular tx signer should recover");
    let mut state =
        state_with_active_proposer_and_funded_account(REWARDS_ADDRESS, user_tx.signer());
    let evm_env = test_evm_env(2, REWARDS_ADDRESS);
    let config = OutbeEvmConfig::new(test_chain_spec());
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut ctx = execution_ctx(None, Bytes::new());
    ctx.execute_outbe_block_hooks = false;
    let mut executor = config.create_executor(evm, ctx);

    executor
        .apply_pre_execution_changes()
        .expect("pending RPC env should skip consensus-only Outbe hooks");
    assert!(executor.receipts().is_empty());
    drop(
        executor
            .compressed_entities_scope
            .begin_explicit_gas_window(0)
            .expect("pending RPC env must open the CE lifecycle"),
    );
    executor
        .execute_transaction(user_tx)
        .expect("pending RPC env must execute txpool transactions inside a CE scope");
    assert_eq!(executor.receipts().len(), 1);
}

#[test]
fn outbe_post_execution_preserves_behavior_for_absent_or_empty_withdrawals() {
    use alloy_eips::eip6110::{DEPOSIT_REQUEST_TYPE, MAINNET_DEPOSIT_CONTRACT_ADDRESS};
    use reth_trie::{test_utils::state_root_prehashed, HashedPostState, KeccakKeyHasher};

    const DAO_BALANCE: u128 = 37;
    const CUMULATIVE_TX_GAS: u64 = 11;
    const REGULAR_GAS: u64 = 17;
    const STATE_GAS: u64 = 23;

    struct Case {
        name: &'static str,
        chain_spec: Arc<ChainSpec<OutbeHeader>>,
        spec_id: SpecId,
        include_deposit: bool,
        expected_gas_used: u64,
    }

    fn fixture_receipt(include_deposit: bool) -> Receipt {
        let logs = if include_deposit {
            let event = DepositEvent {
                pubkey: Bytes::from(vec![0x11; 48]),
                withdrawal_credentials: Bytes::from(vec![0x22; 32]),
                amount: Bytes::from(vec![0x33; 8]),
                signature: Bytes::from(vec![0x44; 96]),
                index: Bytes::from(vec![0x55; 8]),
            };
            vec![Log {
                address: MAINNET_DEPOSIT_CONTRACT_ADDRESS,
                data: event.encode_log_data(),
            }]
        } else {
            Vec::new()
        };
        Receipt {
            tx_type: reth_ethereum::TxType::Legacy,
            success: true,
            cumulative_gas_used: CUMULATIVE_TX_GAS,
            logs,
        }
    }

    fn fixture_state() -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
        let mut database = CacheDB::<EmptyDBTyped<ProviderError>>::default();
        database.insert_account_info(
            alloy_evm::eth::dao_fork::DAO_HARDFORK_ACCOUNTS[0],
            AccountInfo {
                balance: U256::from(DAO_BALANCE),
                ..Default::default()
            },
        );
        State::builder()
            .with_database(database)
            .with_bundle_update()
            .build()
    }

    fn post_state_root(state: &revm::database::BundleState) -> B256 {
        let sorted =
            HashedPostState::from_bundle_state::<KeccakKeyHasher>(state.state()).into_sorted();
        let storages = sorted.storages;
        let accounts = sorted
            .accounts
            .into_iter()
            .filter_map(|(address, account)| {
                account.map(|account| {
                    let storage = storages
                        .get(&address)
                        .map(|storage| storage.storage_slots.clone())
                        .unwrap_or_default();
                    (address, (account, storage))
                })
            });
        state_root_prehashed(accounts)
    }

    fn balance(state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>, address: Address) -> U256 {
        state
            .basic(address)
            .expect("post-execution balance is readable")
            .map_or(U256::ZERO, |account| account.balance)
    }

    let chain_spec = |activate: fn(ChainSpecBuilder) -> ChainSpecBuilder| {
        let mut spec = activate(ChainSpecBuilder::from(&*MAINNET)).build();
        spec.chain = CHAIN_ID.into();
        spec.genesis.config.chain_id = CHAIN_ID;
        Arc::new(spec.map_header(OutbeHeader::new))
    };
    let cases = [
        Case {
            name: "shanghai-withdrawals-and-dao",
            chain_spec: chain_spec(ChainSpecBuilder::shanghai_activated),
            spec_id: SpecId::SHANGHAI,
            include_deposit: false,
            expected_gas_used: CUMULATIVE_TX_GAS,
        },
        Case {
            name: "prague-deposit-and-system-requests",
            chain_spec: chain_spec(ChainSpecBuilder::prague_activated),
            spec_id: SpecId::PRAGUE,
            include_deposit: true,
            expected_gas_used: CUMULATIVE_TX_GAS,
        },
        Case {
            name: "amsterdam-state-gas",
            chain_spec: chain_spec(ChainSpecBuilder::amsterdam_activated),
            spec_id: SpecId::AMSTERDAM,
            include_deposit: false,
            expected_gas_used: STATE_GAS,
        },
    ];

    let withdrawal_cases = [("none", None), ("empty", Some(Vec::new()))];

    for case in cases {
        for (withdrawal_name, withdrawals) in withdrawal_cases.clone() {
            let run = |ocomp: bool| {
                let mut state = fixture_state();
                let config = if ocomp {
                    OutbeEvmConfig::new(case.chain_spec.clone())
                        .with_ocomp_lifecycle_activation(OcompLifecycleActivation::at_block(0))
                } else {
                    OutbeEvmConfig::new(case.chain_spec.clone())
                };
                let evm_env = EvmEnv {
                    cfg_env: CfgEnv::new()
                        .with_chain_id(case.chain_spec.chain().id())
                        .with_spec_and_mainnet_gas_params(case.spec_id),
                    block_env: BlockEnv {
                        number: U256::ZERO,
                        gas_limit: 30_000_000,
                        beneficiary: REWARDS_ADDRESS,
                        timestamp: U256::ZERO,
                        ..Default::default()
                    },
                };
                let evm = config.evm_with_env(&mut state, evm_env);
                let mut ctx = execution_ctx(Some(1), Bytes::new());
                ctx.execute_outbe_block_hooks = false;
                ctx.inner.withdrawals = withdrawals.clone().map(std::borrow::Cow::Owned);

                let mut executor = config.create_executor(evm, ctx);
                executor.inner.receipts = vec![fixture_receipt(case.include_deposit)];
                executor.inner.cumulative_tx_gas_used = CUMULATIVE_TX_GAS;
                executor.inner.block_regular_gas_used = REGULAR_GAS;
                executor.inner.block_state_gas_used = STATE_GAS;
                executor.inner.blob_gas_used = 5;
                executor.validate_execution_summary = false;
                if ocomp {
                    executor.ocomp_lifecycle_active = true;
                    executor.ocomp_terminal_request_consumed = true;
                    executor
                        .apply_outbe_ethereum_post_execution()
                        .expect("OCOMP post-execution phase succeeds");
                }
                let (evm, result) = executor.finish().expect("Outbe result assembly succeeds");
                drop(evm);

                let root = post_state_root(&state.bundle_state);
                let dao_source_balance = balance(
                    &mut state,
                    alloy_evm::eth::dao_fork::DAO_HARDFORK_ACCOUNTS[0],
                );
                let dao_beneficiary_balance = balance(
                    &mut state,
                    alloy_evm::eth::dao_fork::DAO_HARDFORK_BENEFICIARY,
                );
                let withdrawal_balance = balance(
                    &mut state,
                    address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"),
                );
                (
                    result,
                    root,
                    dao_source_balance,
                    dao_beneficiary_balance,
                    withdrawal_balance,
                )
            };

            let ocomp = run(true);
            let normal = run(false);
            assert_eq!(
                ocomp, normal,
                "{} / {withdrawal_name}: proposer, validator and OCOMP execution must agree",
                case.name,
            );
            assert_eq!(ocomp.0.gas_used, case.expected_gas_used, "{}", case.name);
            assert_eq!(ocomp.2, U256::ZERO, "{}: DAO source drains", case.name);
            assert_eq!(
                ocomp.3,
                U256::from(DAO_BALANCE),
                "{}: DAO beneficiary receives the drained balance",
                case.name
            );
            assert_eq!(
                ocomp.4,
                U256::ZERO,
                "{} / {withdrawal_name}: absent or empty withdrawals do not credit a balance",
                case.name,
            );
            assert_eq!(
                ocomp
                    .0
                    .requests
                    .iter()
                    .any(|request| request.first() == Some(&DEPOSIT_REQUEST_TYPE)),
                case.include_deposit,
                "{}: Prague deposit request branch is observable",
                case.name
            );
        }
    }
}

#[test]
fn non_empty_withdrawal_rejects_before_any_state_write() {
    use alloy_eips::eip4895::Withdrawal;

    const DAO_BALANCE: u128 = 37;
    const WITHDRAWAL_TARGET: Address = address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");

    fn account_balance(
        state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
        address: Address,
    ) -> U256 {
        state
            .basic(address)
            .expect("post-execution balance is readable")
            .map_or(U256::ZERO, |account| account.balance)
    }

    for ocomp in [false, true] {
        let mut spec = ChainSpecBuilder::from(&*MAINNET)
            .shanghai_activated()
            .build();
        spec.chain = CHAIN_ID.into();
        spec.genesis.config.chain_id = CHAIN_ID;
        let chain_spec = Arc::new(spec.map_header(OutbeHeader::new));
        let mut database = CacheDB::<EmptyDBTyped<ProviderError>>::default();
        database.insert_account_info(
            alloy_evm::eth::dao_fork::DAO_HARDFORK_ACCOUNTS[0],
            AccountInfo {
                balance: U256::from(DAO_BALANCE),
                ..Default::default()
            },
        );
        let mut state = State::builder()
            .with_database(database)
            .with_bundle_update()
            .build();
        let config = if ocomp {
            OutbeEvmConfig::new(chain_spec.clone())
                .with_ocomp_lifecycle_activation(OcompLifecycleActivation::at_block(0))
        } else {
            OutbeEvmConfig::new(chain_spec.clone())
        };
        let evm_env = EvmEnv {
            cfg_env: CfgEnv::new()
                .with_chain_id(chain_spec.chain().id())
                .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
            block_env: BlockEnv {
                number: U256::ZERO,
                gas_limit: 30_000_000,
                beneficiary: REWARDS_ADDRESS,
                timestamp: U256::ZERO,
                ..Default::default()
            },
        };
        let evm = config.evm_with_env(&mut state, evm_env);
        let mut ctx = execution_ctx(Some(0), Bytes::new());
        ctx.execute_outbe_block_hooks = false;
        ctx.inner.withdrawals = Some(std::borrow::Cow::Owned(vec![Withdrawal {
            index: 0,
            validator_index: 0,
            address: WITHDRAWAL_TARGET,
            amount: 1_000,
        }]));
        let mut executor = config.create_executor(evm, ctx);
        executor.validate_execution_summary = false;

        let error = executor
            .apply_pre_execution_changes()
            .expect_err("every non-empty withdrawals list must be rejected pre-state");
        drop(executor);
        assert!(
            error
                .to_string()
                .contains("non-empty EIP-4895 withdrawals are unsupported on Outbe"),
            "{error}"
        );

        assert_eq!(
            account_balance(
                &mut state,
                alloy_evm::eth::dao_fork::DAO_HARDFORK_ACCOUNTS[0]
            ),
            U256::from(DAO_BALANCE),
            "validation must precede the DAO drain"
        );
        assert_eq!(
            account_balance(
                &mut state,
                alloy_evm::eth::dao_fork::DAO_HARDFORK_BENEFICIARY
            ),
            U256::ZERO,
            "validation must precede any beneficiary credit"
        );
        assert_eq!(
            account_balance(&mut state, WITHDRAWAL_TARGET),
            U256::ZERO,
            "unsupported withdrawal must not credit its target"
        );
    }
}

#[test]
fn active_lifecycle_proposer_and_replay_match_receipts_roots_and_header_artifacts() {
    use reth_trie::{test_utils::state_root_prehashed, HashedPostState, KeccakKeyHasher};

    fn post_state_root(state: &revm::database::BundleState) -> B256 {
        let sorted =
            HashedPostState::from_bundle_state::<KeccakKeyHasher>(state.state()).into_sorted();
        let storages = sorted.storages;
        let accounts = sorted
            .accounts
            .into_iter()
            .filter_map(|(address, account)| {
                account.map(|account| {
                    let storage = storages
                        .get(&address)
                        .map(|storage| storage.storage_slots.clone())
                        .unwrap_or_default();
                    (address, (account, storage))
                })
            });
        state_root_prehashed(accounts)
    }

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
        let begin =
            begin_system_txs_for_test(&config, 1, B256::ZERO, &Bytes::new(), None, proposer);
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
        {
            let mut provider = super::DirectStorageProvider::new(
                &mut state,
                BlockContext::empty_for_tests(1, 1, chain_spec.chain().id()),
            );
            let storage = StorageHandle::new(&mut provider);
            assert!(
                outbe_metadosis::api::is_active_ocomp_fork_install(storage, &install)
                    .expect("read persisted block-1 fork installation"),
                "block-1 lifecycle must persist the exact fork installation"
            );
        }

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

/// determinism gate: a block carrying a valid BLS late-finalize
/// credit, executed on the proposer's encoded `extra_data` and on the bytes
/// a validator decodes+re-encodes, reaches **identical** post-state and
/// receipts. Proves the begin-zone late-credit verify+record path is
/// deterministic across proposer and validator (artifact byte-identity is
/// pinned here via the codec round-trip; full proposer/validator lockstep is
/// covered end-to-end by the localnet harness).
///
/// The block executes at `N+K` so the begin-zone `settle_matured` is not a
/// no-op: a pre-seeded matured escrow (block `N`, non-zero fee, one credited
/// voter at `k=1`) is actually **paid** - and the resulting fee-share
/// **balance delta** (voter + drained `REWARDS`) must match byte-for-byte on
/// both the proposer and validator paths. This closes the gap
/// where a zero `validator_fee_sum` made settlement prove nothing.
#[test]
fn proposer_validator_same_state_root() {
    use commonware_codec::Encode as _;
    use commonware_consensus::simplex::types::Proposal;
    use commonware_consensus::types::{Epoch, Round, View};
    use commonware_cryptography::bls12381::{
        self,
        primitives::{ops::aggregate, variant::MinPk},
    };
    use commonware_cryptography::Signer as _;
    use commonware_math::algebra::Random as _;
    use outbe_consensus::digest::Digest as OutbeDigest;
    use outbe_consensus::proof::{
        committee_set_hash_v2, finalize_namespace, CommitteeEntry, CommitteeSnapshot,
    };
    use outbe_primitives::reshare_artifact::{
        decode_outbe_block_artifacts, LateFinalizeCreditsArtifact, PerBlockCredit,
    };

    // Mirror production startup: the consensus chain id is installed into the
    // namespace source of truth BEFORE anything signs or verifies. The
    // executor below constructs `OutbeEvmConfig`, which now installs it for
    // every constructor; install it here too so the finalize
    // aggregate signed below uses the same `finalize_namespace` the verify
    // path reads - otherwise the late-finalize BLS check fails on a namespace
    // mismatch (`b"outbe" || 0` at sign time vs `b"outbe" || CHAIN_ID` at
    // verify time). CHAIN_ID matches the Outbe Devnet identity used by
    // `test_chain_spec()` throughout this shared lib-test process.
    outbe_consensus::proof::init_consensus_chain_id(CHAIN_ID).unwrap();

    let epoch = 0u64;
    // Real BLS committee of 4 (committee addresses are the late-credit voters).
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

    // Execute at block N+K so the begin-zone `settle_matured` is not a no-op.
    let window_k = outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K;
    let settle_block = window_k + 1; // K+1 = 4: first block where N=1 matures.
    let progress_marker = settle_block - 2; // CPA progress gate: last_accounted.

    // Live credit for the finalized parent (fb = settle_block - 1, distance 1),
    // signers 0..2. The credit targets the finalized parent (`parent_hash`) -
    // the very block the block-(N+K) CPA escrows - so its canonical binding
    // (number->{fb_hash, epoch, committee_set_hash}) is written by
    // `on_finalized_metadata` and the credit authenticates against it.
    // The CPA metadata carries no base voters, so only the late credit's
    // signers are recorded. This exercises the *recording* path's parity.
    let (fb_number, view, parent_view) = (settle_block - 1, 9u64, 8u64);
    let parent_hash = B256::with_last_byte(0xAA);
    let fb_hash = parent_hash;

    // Pre-seeded MATURED escrow for block N = settle_block - K with a non-zero
    // fee and one credited voter at k=1. `settle_matured(settle_block, K)`
    // settles this block, so the begin-zone actually PAYS - proving the
    // fee-share balance delta is identical on both paths. A
    // distinct fb_hash and a dedicated voter address keep this concern isolated
    // from the live recording credit above.
    let settle_target = settle_block - window_k; // = 1
    let settle_fb_hash = B256::with_last_byte(0x11);
    let settle_voter = Address::with_last_byte(0x77);
    let settle_committee = 4u64;
    let settle_fee = U256::from(4_000u64);
    // payout_i = fee * w(1) / (committee * w_max) = 4000 * 100 / 400 = 1000.
    let expected_payout = settle_fee * outbe_rewards::constants::decay_weight(1)
        / outbe_rewards::constants::fixed_denominator(settle_committee);
    let proposal = Proposal::new(
        Round::new(Epoch::new(epoch), View::new(view)),
        View::new(parent_view),
        OutbeDigest(fb_hash),
    );
    let msg = proposal.encode().to_vec();
    // finalize votes bind the ordered committee; build the canonical
    // `Set` from the same committee the snapshot/verifier uses.
    let committee_set: commonware_utils::ordered::Set<bls12381::PublicKey> =
        commonware_utils::ordered::Set::from_iter_dedup(keys.iter().map(|k| k.public_key()));
    let sigs: Vec<bls12381::Signature> = [0usize, 1, 2]
        .iter()
        .map(|&i| keys[i].sign(&finalize_namespace(&committee_set), &msg))
        .collect();
    let agg = aggregate::combine_signatures::<MinPk, _>(
        commonware_utils::iter::NonEmpty::try_new(sigs.iter().map(|s| s.as_ref())).unwrap(),
    );
    let mut aggregate_signature = [0u8; 96];
    aggregate_signature.copy_from_slice(&agg.encode());
    let mut signer_bitmap = vec![0u8; 4usize.div_ceil(8)];
    for i in [0usize, 1, 2] {
        signer_bitmap[i / 8] |= 1u8 << (i % 8);
    }
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

    // Proposer encodes; validator decodes the same bytes and re-encodes.
    let extra_proposer = encode_outbe_block_artifacts(&artifact).unwrap();
    let decoded = decode_outbe_block_artifacts(extra_proposer.as_ref()).unwrap();
    let extra_validator = encode_outbe_block_artifacts(&decoded).unwrap();
    assert_eq!(
        extra_proposer, extra_validator,
        "codec round-trip must be byte-identical (proposer encode == validator re-encode)"
    );

    // Execute block N+K with the begin-zone, capturing the recorded
    // late-credit state for the live credit, the settled voter's fee-share
    // balance, the drained REWARDS balance, and receipt shape.
    let run = |extra_data: Bytes| -> (usize, Vec<u64>, u32, Vec<Address>, U256, U256, u64) {
        let signer = test_evm_signer();
        let proposer = signer.address();
        let snapshot = snapshot.clone();
        // Register the committee members so the window-close absentee pass can
        // slash them: all four are absent for the settled block (which credited
        // only `settle_voter`). At a single miss this is counter-only (no felony),
        // adding no balance effect - only the parity-checked miss counters.
        let mut seeded: Vec<(Address, [u8; 48])> = vec![(proposer, dummy_pubkey(0xA2))];
        for member in &snapshot.committee {
            seeded.push((member.address, member.consensus_pubkey));
        }
        let mut state = state_with_active_validators_seeded(&seeded, move |storage| {
            // The live credit's escrow binding is written by the N+K CPA
            // (on_finalized_metadata); the committee snapshot is pre-seeded
            // for the credit's BLS verify.
            outbe_validatorset::write_committee_snapshot(storage.clone(), epoch, &snapshot)
                .expect("seed committee snapshot");

            // Pre-seed the matured escrow (block N), its k=1 voter, fund
            // REWARDS to back the payout + residue burn, and advance the
            // accounting marker so the N+K CPA progress gate passes.
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
            )
            .expect("seed matured escrow");
            seed_ctx
                .storage
                .contract::<outbe_rewards::schema::Rewards>()
                .pending_reward_day
                .write(&settle_fb_hash, 19700101)
                .expect("seed canonical reward day");
            outbe_rewards::late_settlement::record_late_credit(
                &seed_ctx,
                settle_fb_hash,
                settle_voter,
                1,
            )
            .expect("seed k=1 voter");
            seed_ctx
                .storage
                .increase_balance(REWARDS_ADDRESS, settle_fee)
                .expect("fund REWARDS for settle");
            outbe_accounting::record_phase1_progress(&seed_ctx, progress_marker)
                .expect("seed accounting progress");
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
        // Canonical binding the CPA escrows; must match the credit.
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

        // Phase 1 disabled (no CPA cert seeded); late-finalize verify runs on
        // the valid credit + seeded snapshot.
        super::with_phase1_verify_disabled(|| {
            executor
                .apply_pre_execution_changes()
                .expect("pre-exec ok for a valid credit + seeded snapshot");
        });
        let system_txs = begin_system_txs_for_test(
            &config,
            settle_block,
            parent_hash,
            &extra_data,
            Some(metadata),
            proposer,
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
    // Settlement actually PAID: the k=1 voter received its decay-weighted
    // fee-share, and REWARDS was drained of the settled escrow.
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
    // Window-close slash parity: the absent committee member's miss is recorded
    // (slash fired) and is byte-identical on the proposer and validator paths
    // (the tuple equality above already compares it).
    assert_eq!(
        proposer_out.6, 1,
        "absent committee voter is slashed (miss recorded) at window close on both paths"
    );
}

#[test]
fn factory_boundaries_are_byte_equal_across_proposer_and_validator_execution() {
    use std::collections::BTreeMap;

    use reth_primitives_traits::Account as TrieAccount;
    use reth_trie::test_utils::state_root;

    #[derive(Clone, Copy, Debug)]
    enum Boundary {
        Approved,
        Expired,
        Error,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Output {
        state_root: B256,
        receipts_root: B256,
        logs_bloom: alloy_primitives::Bloom,
        receipt_bytes: Vec<Vec<u8>>,
        receipt_success: Vec<bool>,
        cumulative_gas: Vec<u64>,
        created_logs: usize,
        refunded_logs: usize,
        burned_logs: usize,
        status: ProposalStatus,
        settlement: BondSettlement,
        factory_count: U256,
        registered_token_id: Option<B256>,
        token_by_id: Address,
        token_by_ticker: Address,
        token_code_hash: Option<B256>,
        token_total_supply: U256,
        issuer_token_balance: U256,
        issuer_balance: U256,
        vote_balance: U256,
        liabilities: U256,
        reservation_exists: bool,
    }

    fn full_state_root(state: &State<CacheDB<EmptyDBTyped<ProviderError>>>) -> B256 {
        let mut accounts: BTreeMap<Address, (AccountInfo, BTreeMap<U256, U256>)> = state
            .database
            .cache
            .accounts
            .iter()
            .filter_map(|(address, account)| {
                account.info().map(|info| {
                    (
                        *address,
                        (
                            info,
                            account.storage.iter().map(|(k, v)| (*k, *v)).collect(),
                        ),
                    )
                })
            })
            .collect();
        for (address, cached) in &state.cache.accounts {
            match &cached.account {
                Some(current) => {
                    let entry = accounts
                        .entry(*address)
                        .or_insert_with(|| (current.info.clone(), BTreeMap::new()));
                    entry.0 = current.info.clone();
                    entry
                        .1
                        .extend(current.storage.iter().map(|(k, v)| (*k, *v)));
                }
                None => {
                    accounts.remove(address);
                }
            }
        }
        state_root(accounts.into_iter().map(|(address, (info, storage))| {
            let bytecode_hash = (!info.code_hash.is_zero() && info.code_hash != keccak256([]))
                .then_some(info.code_hash);
            let account = TrieAccount {
                nonce: info.nonce,
                balance: info.balance,
                bytecode_hash,
            };
            let storage = storage
                .into_iter()
                .filter(|(_, value)| !value.is_zero())
                .map(|(slot, value)| (B256::from(slot.to_be_bytes::<32>()), value));
            (address, (account, storage))
        }))
    }

    fn run(boundary: Boundary, validator_execution: bool) -> Output {
        const CREATION_BLOCK: u64 = 7;
        let finalization_block = CREATION_BLOCK + VOTING_WINDOW_BLOCKS + 1;
        let signer = test_evm_signer();
        let proposer = signer.address();
        let issuer = Address::repeat_byte(0x31);
        let validators = [
            (proposer, dummy_pubkey(0xc1)),
            (Address::repeat_byte(0xc2), dummy_pubkey(0xc2)),
            (Address::repeat_byte(0xc3), dummy_pubkey(0xc3)),
        ];
        let payload = encode_canonical_stablecoin_create(&StablecoinCreatePayload {
            issuer,
            name: "Parity Dollar".into(),
            ticker: "PARUSD".into(),
            iso4217: 840,
            decimals: 6,
            supply_cap: U256::from(1_000_000u64),
            policy_id: U256::from(1u64),
        })
        .expect("canonical Factory payload");
        let payload = core::str::from_utf8(&payload).expect("canonical payload is UTF-8");

        let mut state =
            state_with_active_validators_seeded_at_block(&validators, CREATION_BLOCK, |_| {});
        let seed_context = BlockContext::new(
            CREATION_BLOCK,
            1_700_000_000,
            CHAIN_ID,
            proposer,
            validators.iter().map(|(address, _)| *address).collect(),
        );
        let (expected_token_id, expected_token) = {
            let mut provider = super::DirectStorageProvider::new(&mut state, seed_context.clone());
            let storage = StorageHandle::new(&mut provider);
            storage
                .set_balance(VOTE_ADDRESS, STABLECOIN_CREATE_BOND)
                .unwrap();
            let predicted = StablecoinFactoryContract::new(storage.clone())
                .predict_token_address(issuer, "PARUSD")
                .unwrap();
            let mut vote = Vote::new(storage.clone());
            let proposal_id = vote
                .create_proposal_with_value(
                    issuer,
                    STABLECOIN_FACTORY_ADDRESS,
                    payload,
                    CREATION_BLOCK,
                    STABLECOIN_CREATE_BOND,
                    crate::handlers::vote::registry(),
                )
                .unwrap();
            match boundary {
                Boundary::Approved | Boundary::Error => {
                    vote.cast_vote_approve(proposal_id, validators[0].0, true, CREATION_BLOCK + 1)
                        .unwrap();
                    vote.cast_vote_approve(proposal_id, validators[1].0, true, CREATION_BLOCK + 1)
                        .unwrap();
                }
                Boundary::Expired => {}
            }
            if matches!(boundary, Boundary::Error) {
                let mut corrupted = vote.proposals.get(proposal_id).unwrap().unwrap();
                corrupted.payload = "{".into();
                vote.proposals.update(&corrupted).unwrap();
            }
            let progress_context = BlockRuntimeContext::new(seed_context, storage.clone());
            outbe_accounting::record_phase1_progress(&progress_context, finalization_block - 2)
                .unwrap();
            provider.flush().expect("seed direct storage");
            predicted
        };

        let parent_hash = B256::repeat_byte(0x71);
        let mut metadata = test_metadata();
        metadata.finalized_block_number = finalization_block - 1;
        metadata.finalized_block_hash = parent_hash;
        metadata.ordered_committee = validators.iter().map(|(address, _)| *address).collect();
        metadata.signer_bitmap = vec![1; validators.len()];

        let bridge = ConsensusExecutionBridge::new();
        bridge.record_execution_summary_with_state_root(
            metadata.finalized_block_number,
            parent_hash,
            ExecutionSummaryArtifact {
                validator_fee_sum: U256::ZERO,
            },
            1_700_000_000,
            B256::repeat_byte(0x91),
        );
        let config =
            OutbeEvmConfig::new_with_bridge(test_chain_spec(), bridge).with_evm_signer(signer);
        let system_txs = begin_system_txs_for_test(
            &config,
            finalization_block,
            parent_hash,
            &Bytes::new(),
            Some(metadata.clone()),
            proposer,
        );
        let evm = config.evm_with_env(
            &mut state,
            test_evm_env(finalization_block, REWARDS_ADDRESS),
        );
        let mut execution = execution_ctx(Some(0), Bytes::new());
        execution.inner.parent_hash = parent_hash;
        execution.parent_consensus_metadata = Some(metadata);
        execution.proposer_evm_address = Some(proposer);
        if validator_execution {
            execution.expected_begin_system_txs = system_txs.clone();
        }
        let mut executor = config.create_executor(evm, execution);
        super::with_phase1_verify_disabled(|| {
            executor
                .apply_pre_execution_changes()
                .expect("stablecoin boundary pre-execution");
        });
        for transaction in system_txs {
            executor
                .execute_transaction(transaction)
                .expect("mandatory begin-zone transaction");
        }

        let receipts = executor.receipts().to_vec();
        let receipt_bytes = receipts
            .iter()
            .map(|receipt| receipt.with_bloom_ref().encoded_2718())
            .collect();
        let receipt_blooms: Vec<_> = receipts
            .iter()
            .map(|receipt| receipt.with_bloom_ref())
            .collect();
        let receipts_root = alloy_consensus::proofs::calculate_receipt_root(&receipt_blooms);
        let block_bloom = logs_bloom(receipts.iter().flat_map(|receipt| receipt.logs.iter()));
        let receipt_success = receipts.iter().map(|receipt| receipt.success).collect();
        let cumulative_gas = receipts
            .iter()
            .map(|receipt| receipt.cumulative_gas_used)
            .collect();
        let created_logs = receipts
            .iter()
            .flat_map(|receipt| &receipt.logs)
            .filter(|log| {
                log.address == STABLECOIN_FACTORY_ADDRESS
                    && log.data.topics().first()
                        == Some(&IStablecoinFactory::StablecoinCreated::SIGNATURE_HASH)
            })
            .count();
        let refunded_logs = receipts
            .iter()
            .flat_map(|receipt| &receipt.logs)
            .filter(|log| {
                log.address == VOTE_ADDRESS
                    && log.data.topics().first()
                        == Some(&IVote::ProposalBondRefunded::SIGNATURE_HASH)
            })
            .count();
        let burned_logs = receipts
            .iter()
            .flat_map(|receipt| &receipt.logs)
            .filter(|log| {
                log.address == VOTE_ADDRESS
                    && log.data.topics().first() == Some(&IVote::ProposalBondBurned::SIGNATURE_HASH)
            })
            .count();
        drop(executor);

        let read_context = BlockContext::new(
            finalization_block,
            1_700_000_000,
            CHAIN_ID,
            proposer,
            validators.iter().map(|(address, _)| *address).collect(),
        );
        let (
            status,
            settlement,
            factory_count,
            registered_token_id,
            token_by_id,
            token_by_ticker,
            token_total_supply,
            issuer_token_balance,
            issuer_balance,
            vote_balance,
            liabilities,
            reservation_exists,
        ) = {
            let mut provider = super::DirectStorageProvider::new(&mut state, read_context);
            let storage = StorageHandle::new(&mut provider);
            let vote = Vote::new(storage.clone());
            let factory = StablecoinFactoryContract::new(storage.clone());
            let factory_count = factory.token_count().unwrap();
            let (token_total_supply, issuer_token_balance) = if factory_count == U256::ONE {
                let token = StablecoinContract::new(storage.clone(), expected_token);
                (
                    token.total_supply().unwrap(),
                    token.balance_of(issuer).unwrap(),
                )
            } else {
                (U256::ZERO, U256::ZERO)
            };
            (
                vote.proposals
                    .get(U256::from(1u64))
                    .unwrap()
                    .unwrap()
                    .proposal_status()
                    .unwrap(),
                vote.proposal_bond(U256::from(1u64)).unwrap().settlement,
                factory_count,
                factory.registered_token_id(expected_token).unwrap(),
                factory.token_by_id(expected_token_id).unwrap(),
                factory.token_by_ticker("PARUSD").unwrap(),
                token_total_supply,
                issuer_token_balance,
                storage.balance(issuer).unwrap(),
                storage.balance(VOTE_ADDRESS).unwrap(),
                vote.bond_liabilities().unwrap(),
                factory.reservations.exists(U256::from(1u64)).unwrap(),
            )
        };
        let token_code_hash = state
            .basic(expected_token)
            .expect("token account read")
            .map(|account| account.code_hash);
        let state_root = full_state_root(&state);

        if matches!(boundary, Boundary::Approved) {
            assert_eq!(registered_token_id, Some(expected_token_id));
        }
        Output {
            state_root,
            receipts_root,
            logs_bloom: block_bloom,
            receipt_bytes,
            receipt_success,
            cumulative_gas,
            created_logs,
            refunded_logs,
            burned_logs,
            status,
            settlement,
            factory_count,
            registered_token_id,
            token_by_id,
            token_by_ticker,
            token_code_hash,
            token_total_supply,
            issuer_token_balance,
            issuer_balance,
            vote_balance,
            liabilities,
            reservation_exists,
        }
    }

    for boundary in [Boundary::Approved, Boundary::Expired, Boundary::Error] {
        let proposer = run(boundary, false);
        let validator = run(boundary, true);
        assert_eq!(
            proposer, validator,
            "{boundary:?} must be byte/state equal across execution roles"
        );
        assert_eq!(proposer.receipt_success, vec![true; 6]);
        assert_eq!(proposer.cumulative_gas.len(), 6);
        match boundary {
            Boundary::Approved => {
                assert_eq!(proposer.created_logs, 1);
                assert_eq!(proposer.refunded_logs, 1);
                assert_eq!(proposer.burned_logs, 0);
                assert_eq!(proposer.status, ProposalStatus::Approved);
                assert_eq!(proposer.settlement, BondSettlement::Refunded);
                assert_eq!(proposer.factory_count, U256::ONE);
                assert_ne!(proposer.token_by_id, Address::ZERO);
                assert_eq!(proposer.token_by_id, proposer.token_by_ticker);
                assert_eq!(
                    proposer.token_code_hash,
                    Some(keccak256(
                        outbe_primitives::addresses::STABLECOIN_MARKER_CODE
                    ))
                );
                assert_eq!(proposer.token_total_supply, U256::ZERO);
                assert_eq!(proposer.issuer_token_balance, U256::ZERO);
                assert_eq!(proposer.issuer_balance, STABLECOIN_CREATE_BOND);
                assert_eq!(proposer.vote_balance, U256::ZERO);
                assert_eq!(proposer.liabilities, U256::ZERO);
                assert!(!proposer.reservation_exists);
            }
            Boundary::Expired => {
                assert_eq!(proposer.created_logs, 0);
                assert_eq!(proposer.refunded_logs, 0);
                assert_eq!(proposer.burned_logs, 1);
                assert_eq!(proposer.status, ProposalStatus::Expired);
                assert_eq!(proposer.settlement, BondSettlement::Burned);
                assert_eq!(proposer.factory_count, U256::ZERO);
                assert_eq!(proposer.issuer_balance, U256::ZERO);
                assert_eq!(proposer.vote_balance, U256::ZERO);
                assert_eq!(proposer.liabilities, U256::ZERO);
                assert!(!proposer.reservation_exists);
                assert!(proposer.registered_token_id.is_none());
                assert_eq!(proposer.token_by_id, Address::ZERO);
                assert_eq!(proposer.token_by_ticker, Address::ZERO);
                assert_eq!(proposer.token_total_supply, U256::ZERO);
            }
            Boundary::Error => {
                assert_eq!(proposer.created_logs, 0);
                assert_eq!(proposer.refunded_logs, 0);
                assert_eq!(proposer.burned_logs, 0);
                assert_eq!(proposer.status, ProposalStatus::Error);
                assert_eq!(proposer.settlement, BondSettlement::Unsettled);
                assert_eq!(proposer.factory_count, U256::ZERO);
                assert_eq!(proposer.issuer_balance, U256::ZERO);
                assert_eq!(proposer.vote_balance, STABLECOIN_CREATE_BOND);
                assert_eq!(proposer.liabilities, STABLECOIN_CREATE_BOND);
                assert!(proposer.reservation_exists);
                assert!(proposer.registered_token_id.is_none());
                assert_eq!(proposer.token_by_id, Address::ZERO);
                assert_eq!(proposer.token_by_ticker, Address::ZERO);
                assert_eq!(proposer.token_total_supply, U256::ZERO);
            }
        }
    }
}

#[test]
fn independent_body_stores_produce_identical_full_block_state_receipts_and_balances() {
    use reth_trie::{test_utils::state_root_prehashed, HashedPostState, KeccakKeyHasher};

    fn post_state_root(state: &revm::database::BundleState) -> B256 {
        let sorted =
            HashedPostState::from_bundle_state::<KeccakKeyHasher>(state.state()).into_sorted();
        let storages = sorted.storages;
        let accounts = sorted
            .accounts
            .into_iter()
            .filter_map(|(address, account)| {
                account.map(|account| {
                    let storage = storages
                        .get(&address)
                        .map(|storage| storage.storage_slots.clone())
                        .unwrap_or_default();
                    (address, (account, storage))
                })
            });
        state_root_prehashed(accounts)
    }

    let proposer = test_evm_signer().address();
    let worldwide_day = WorldwideDay::new(20_241_220);
    let floor_price_minor = U256::from(500_000u64);
    let bucket_key = NodContract::bucket_key(worldwide_day, floor_price_minor, 840);
    let seed_state = || {
        let (directory, tree_service) = persistent_test_tree(B256::ZERO);
        let empty_root = outbe_compressed_entities::sealed_root(B256::ZERO).unwrap();
        let parent_tree = tree_service
            .open_parent(ExactParentIdentity {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                block_number: 0,
                block_hash: B256::ZERO,
                root: empty_root,
            })
            .expect("open exact empty CE parent");
        let scope =
            ExecutionScope::with_parent_tree(parent_tree, CeWorkConfig::new(0, 0, u64::MAX));
        let mut staged = None;
        let state = state_with_active_validators_seeded_at_block(
            &[(proposer, dummy_pubkey(0xA2))],
            1,
            |storage| {
                storage
                    .sstore(
                        outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
                        U256::ZERO,
                        U256::from(2_u64),
                    )
                    .unwrap();
                storage
                    .sstore(
                        outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
                        U256::from(1_u64),
                        U256::from_be_bytes(empty_root.0),
                    )
                    .unwrap();
                outbe_compressed_entities::begin_block(storage.clone(), &scope)
                    .expect("open compressed-entity seed scope");
                let empty_reader = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
                outbe_nod::api::add_nod(
                    &storage,
                    &scope,
                    &empty_reader,
                    &NodItemState {
                        nod_id: NodContract::generate_nod_id(proposer, worldwide_day).unwrap(),
                        owner: proposer,
                        gratis_load_minor: U256::from(1_000_000u64),
                        worldwide_day,
                        league_id: 1,
                        floor_price_minor,
                        bucket_key,
                        issuance_currency: 840,
                        reference_currency: 840,
                        issued_at: 1,
                    },
                    U256::from(450_000_000u64),
                )
                .expect("seed compact Nod scheduling state");
                // Qualification runs on the daily Nod trigger and requires the
                // previous completed UTC day's finalized VWAP. A live quote
                // alone leaves the bucket untouched, so there is no CE cleanup
                // diff for the parallel-root hook to observe.
                let (.., pair_index) =
                    outbe_oracle::api::require_coen_pair(storage.clone(), 840).unwrap();
                let previous_day = outbe_primitives::time::previous_date_key(
                    outbe_primitives::time::timestamp_to_date_key(TEST_BLOCK_TIMESTAMP_BASE + 2),
                );
                let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
                oracle
                    .utc_day_vwap_value
                    .get_nested(&previous_day)
                    .write(&pair_index, U256::from(1_000_000u64))
                    .unwrap();
                oracle
                    .utc_day_vwap_last_finalized
                    .write(previous_day)
                    .unwrap();
                outbe_cycle::schema::Cycle::new(storage.clone())
                    .last_executed_at
                    .write(
                        &outbe_cycle::triggers::TriggerId::NodCallDaily.as_u32(),
                        TEST_BLOCK_TIMESTAMP_BASE - 86_400,
                    )
                    .unwrap();
                staged = Some(
                    outbe_compressed_entities::end_block(storage, &scope)
                        .expect("close compressed-entity seed scope")
                        .staged_tree_batch,
                );
            },
        );
        let staged = staged.expect("seed lifecycle must produce a tree batch");
        let seed_hash = B256::repeat_byte(0x41);
        let seed_root = staged.new_root();
        tree_service
            .publish_candidate(seed_hash, staged)
            .expect("publish seed CE candidate");
        tree_service
            .apply_finalized(1, seed_hash, seed_root)
            .expect("finalize seed CE candidate");
        (state, directory, tree_service, seed_hash)
    };
    let independent_readers = || {
        let adapter = Arc::new(MemoryStorage::new());
        let reader: StorageReaderHandle = adapter.clone();
        let writer: StorageWriterHandle = adapter;
        NodRepositoryWriter::new(reader.clone(), writer)
            .put_bucket(&NodBucketState {
                bucket_key,
                worldwide_day,
                floor_price_minor,
                is_qualified: false,
                entry_price_minor: U256::from(450_000_000u64),
                reference_currency: 840,
            })
            .expect("seed independent off-chain Nod bucket");
        let readers = RuntimeBodyReaders::new(reader);
        assert!(readers
            .nod()
            .get_bucket(outbe_compressed_entities::WwdEntityId::from_day_and_digest(
                worldwide_day,
                bucket_key.0,
            ))
            .expect("independent bucket read")
            .is_some());
        readers
    };

    let run = |expected_validator_body: bool, readers: RuntimeBodyReaders| {
        let signer = test_evm_signer();
        let (mut state, _tree_directory, tree_service, seed_hash) = seed_state();
        let config = OutbeEvmConfig::new_with_runtime_body_readers(test_chain_spec(), readers)
            .with_evm_signer(signer)
            .with_compressed_tree_service(tree_service.clone());
        let mut parent_metadata = metadata_with(vec![proposer], vec![1], Vec::new());
        parent_metadata.finalized_block_number = 1;
        parent_metadata.finalized_block_hash = seed_hash;
        let system_txs = begin_system_txs_for_test(
            &config,
            2,
            seed_hash,
            &Bytes::new(),
            Some(parent_metadata.clone()),
            proposer,
        );
        let visible_envelopes: Vec<u64> = system_txs.iter().map(|tx| tx.tx().gas_limit()).collect();
        let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
        let mut execution = execution_ctx(Some(1), Bytes::new());
        execution.inner.parent_hash = seed_hash;
        execution.parent_consensus_metadata = Some(parent_metadata);
        execution.parent_artifact_hint = Some(AccountedParentArtifact {
            summary: ExecutionSummaryArtifact {
                validator_fee_sum: U256::ZERO,
            },
            timestamp: 0,
            state_root: Some(B256::repeat_byte(0x91)),
        });
        execution.proposer_evm_address = Some(proposer);
        if expected_validator_body {
            execution.expected_begin_system_txs = system_txs.clone();
        }
        let mut executor = config.create_executor(evm, execution);
        super::with_phase1_verify_disabled(|| {
            executor
                .apply_pre_execution_changes()
                .expect("reader-backed pre-execution hook must succeed");
        });
        for tx in system_txs {
            executor
                .execute_transaction(tx)
                .expect("begin-zone transaction must execute");
        }
        let receipts = executor.receipts().to_vec();
        let cleanup_hook_observation = Arc::new(Mutex::new(None));
        let cleanup_hook_capture = cleanup_hook_observation.clone();
        executor.evm_mut().db_mut().set_state_hook(Some(Box::new(
            move |changes: revm::state::EvmState| {
                let Some(compressed_entities) =
                    changes.get(&outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS)
                else {
                    return;
                };
                let cleared_slots = compressed_entities
                    .storage
                    .values()
                    .filter(|slot| {
                        slot.is_changed()
                            && !slot.original_value.is_zero()
                            && slot.present_value.is_zero()
                    })
                    .count();
                if cleared_slots > 0 {
                    *cleanup_hook_capture.lock().unwrap() = Some(cleared_slots);
                }
            },
        )));
        // Match the production payload-builder ordering: finalize CE while
        // the parallel-root hook is attached, prove the zeroing diff was
        // observed, then detach the hook and freeze/finalize the root.
        executor
            .finalize_compressed_entities()
            .expect("pre-root compressed-entity cleanup must succeed");
        executor
            .prepare_final_header_artifacts(0)
            .expect("final extra_data should encode");
        let sealed = executor
            .compressed_entities_seal_output()
            .expect("block cleanup must produce a CE tree batch");
        let block_hash = B256::repeat_byte(0x42);
        let block_root = sealed.new_root;
        tree_service
            .publish_candidate(block_hash, sealed.staged_tree_batch)
            .expect("publish block CE candidate");
        tree_service
            .apply_finalized(2, block_hash, block_root)
            .expect("finalize block CE candidate");
        let cleanup_hook_cleared_slots = cleanup_hook_observation
            .lock()
            .unwrap()
            .expect("parallel-root hook must observe CE cleanup before root detach");
        assert!(
            cleanup_hook_cleared_slots > 0,
            "pre-root hook must expose at least one temporary CE slot changing to zero"
        );
        executor.evm_mut().db_mut().set_state_hook(None);
        let (evm, block_result) = executor.finish().expect("block finish must succeed");
        drop(evm);
        let bundle = state.bundle_state.clone();
        let root = post_state_root(&bundle);
        let proposer_balance = signer_balance(&mut state, proposer);
        let rewards_balance = signer_balance(&mut state, REWARDS_ADDRESS);

        // A new lifecycle can only open when every pending body/index record and
        // touched list from the finished block has been removed. This checks the
        // same committed bundle used for the state root above, not a mock store.
        let clean_parent = tree_service
            .open_parent(ExactParentIdentity {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                block_number: 2,
                block_hash,
                root: block_root,
            })
            .expect("open finalized block CE parent");
        let clean_scope =
            ExecutionScope::with_parent_tree(clean_parent, CeWorkConfig::new(0, 0, u64::MAX));
        let clean_ctx = BlockContext::new(3, 2, CHAIN_ID, proposer, vec![proposer]);
        super::run_atomic_storage_hooks(&mut state, clean_ctx, |hook_ctx| {
            outbe_compressed_entities::begin_block(hook_ctx.storage.clone(), &clean_scope)?;
            outbe_compressed_entities::end_block(hook_ctx.storage.clone(), &clean_scope).map(|_| ())
        })
        .expect("finished block must leave a clean compressed-entity overlay");
        (
            root,
            bundle,
            receipts,
            proposer_balance,
            rewards_balance,
            block_result.gas_used,
            visible_envelopes,
            cleanup_hook_cleared_slots,
        )
    };

    let proposer_result = run(false, independent_readers());
    let validator_result = run(true, independent_readers());
    assert_eq!(proposer_result, validator_result);
    assert!(proposer_result.2.iter().any(|receipt| {
        receipt.logs.iter().any(|log| {
            log.address == NOD_ADDRESS
                && log.data.topics().first() == Some(&INod::NodBucketBodyStored::SIGNATURE_HASH)
        })
    }));
    let body_receipt_index = proposer_result
        .2
        .iter()
        .position(|receipt| {
            receipt.logs.iter().any(|log| {
                log.address == NOD_ADDRESS
                    && log.data.topics().first() == Some(&INod::NodBucketBodyStored::SIGNATURE_HASH)
            })
        })
        .expect("CycleTick body mutation receipt");
    let previous_cumulative = body_receipt_index
        .checked_sub(1)
        .map_or(0, |index| proposer_result.2[index].cumulative_gas_used);
    let body_receipt_gas = proposer_result.2[body_receipt_index]
        .cumulative_gas_used
        .saturating_sub(previous_cumulative);
    let cycle_intrinsic_gas =
        system_tx_intrinsic_gas(SystemTxInputV2::CycleTick.encode().unwrap().as_ref()).unwrap();
    assert!(
        body_receipt_gas > cycle_intrinsic_gas,
        "receipt-visible CycleTick gas must add explicit CE work to intrinsic gas"
    );
    assert!(
        body_receipt_gas <= proposer_result.6[body_receipt_index],
        "receipt-visible CycleTick gas must not exceed its signed gas limit"
    );
    assert_eq!(
        proposer_result.5,
        proposer_result.2.last().unwrap().cumulative_gas_used,
        "header gas_used must equal the final receipt cumulative gas including CE work"
    );
}

#[test]
fn proposer_validator_body_mints_match_for_all_three_commitment_namespaces() {
    use reth_trie::{test_utils::state_root_prehashed, HashedPostState, KeccakKeyHasher};

    fn post_state_root(state: &revm::database::BundleState) -> B256 {
        let sorted =
            HashedPostState::from_bundle_state::<KeccakKeyHasher>(state.state()).into_sorted();
        let storages = sorted.storages;
        let accounts = sorted
            .accounts
            .into_iter()
            .filter_map(|(address, account)| {
                account.map(|account| {
                    let storage = storages
                        .get(&address)
                        .map(|storage| storage.storage_slots.clone())
                        .unwrap_or_default();
                    (address, (account, storage))
                })
            });
        state_root_prehashed(accounts)
    }

    let proposer = test_evm_signer().address();
    let day = WorldwideDay::new(20_260_716);
    let tribute_owner = Address::repeat_byte(0x31);
    let tribute_id =
        outbe_compressed_entities::derive_poseidon_entity_id(tribute_owner, day).unwrap();
    let nod_owner = Address::repeat_byte(0x32);
    let nod_id = outbe_compressed_entities::derive_poseidon_entity_id(nod_owner, day).unwrap();
    let bucket_key = NodContract::bucket_key(day, U256::from(13), 978);
    let ctx = BlockContext::new(1, 1, CHAIN_ID, proposer, vec![proposer]);

    let run = || {
        let bodies = Arc::new(MemoryStorage::new());
        let tribute_reader = TributeRepositoryReader::new(bodies.clone());
        let nod_reader = NodRepositoryReader::new(bodies);
        let scope = ExecutionScope::new();
        let mut state =
            state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |_| {});
        let (changes, events) =
            super::run_atomic_storage_hooks(&mut state, ctx.clone(), |hook_ctx| {
                outbe_compressed_entities::begin_block(hook_ctx.storage.clone(), &scope)?;
                let tribute = TributeData {
                    tribute_id,
                    owner: tribute_owner,
                    worldwide_day: day,
                    issuance_amount_minor: U256::from(10),
                    issuance_currency: 840,
                    nominal_amount_minor: U256::from(11),
                    reference_currency: 978,
                    tribute_price_minor: U256::from(12),
                    exclude_from_intex_issuance: false,
                };
                let mut tribute_contract = TributeContract::new(hook_ctx.storage.clone());
                tribute_contract.unseal_day(day)?;
                tribute_contract.issue(&scope, &tribute_reader, &tribute)?;
                outbe_nod::api::add_nod(
                    &hook_ctx.storage,
                    &scope,
                    &nod_reader,
                    &NodItemState {
                        nod_id,
                        owner: nod_owner,
                        gratis_load_minor: U256::from(1),
                        worldwide_day: day,
                        league_id: 2,
                        floor_price_minor: U256::from(13),
                        bucket_key,
                        issuance_currency: 840,
                        reference_currency: 978,
                        issued_at: 15,
                    },
                    U256::from(16),
                )?;
                outbe_compressed_entities::end_block(hook_ctx.storage.clone(), &scope).map(|_| ())
            })
            .expect("body mint execution must succeed");
        let compressed_root = {
            let mut provider = outbe_primitives::storage::direct::DirectStorageProvider::new(
                &mut state,
                ctx.clone(),
            );
            StorageHandle::enter(&mut provider, |storage| {
                storage
                    .sload(
                        outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
                        U256::from(1),
                    )
                    .map(|root| B256::from(root.to_be_bytes::<32>()))
            })
            .unwrap()
        };
        let root = post_state_root(&state.bundle_state);
        let proposer_balance = signer_balance(&mut state, proposer);
        let rewards_balance = signer_balance(&mut state, REWARDS_ADDRESS);
        (
            changes,
            events,
            compressed_root,
            root,
            state.bundle_state,
            proposer_balance,
            rewards_balance,
        )
    };

    let proposer_result = run();
    let validator_result = run();
    assert_eq!(proposer_result, validator_result);
    assert!(proposer_result.1.iter().any(|event| {
        event.address == outbe_primitives::addresses::TRIBUTE_ADDRESS
            && event.data.topics()[0]
                == outbe_tribute::precompile::ITribute::TributeBodyStored::SIGNATURE_HASH
    }));
    assert!(proposer_result.1.iter().any(|event| {
        event.address == NOD_ADDRESS
            && event.data.topics()[0] == INod::NodBodyStored::SIGNATURE_HASH
    }));
    assert!(proposer_result.1.iter().any(|event| {
        event.address == NOD_ADDRESS
            && event.data.topics()[0] == INod::NodBucketBodyStored::SIGNATURE_HASH
    }));

    let bodies = Arc::new(MemoryStorage::new());
    let tribute_reader = TributeRepositoryReader::new(bodies.clone());
    let nod_reader = NodRepositoryReader::new(bodies);
    let scope = ExecutionScope::new();
    let mut failed_state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |_| {});
    let error = super::run_atomic_storage_hooks(&mut failed_state, ctx.clone(), |hook_ctx| {
        outbe_compressed_entities::begin_block(hook_ctx.storage.clone(), &scope)?;
        let tribute = TributeData {
            tribute_id,
            owner: tribute_owner,
            worldwide_day: day,
            issuance_amount_minor: U256::from(10),
            issuance_currency: 840,
            nominal_amount_minor: U256::from(11),
            reference_currency: 978,
            tribute_price_minor: U256::from(12),
            exclude_from_intex_issuance: false,
        };
        let mut tribute_contract = TributeContract::new(hook_ctx.storage.clone());
        tribute_contract.unseal_day(day)?;
        tribute_contract.issue(&scope, &tribute_reader, &tribute)?;
        outbe_nod::api::add_nod(
            &hook_ctx.storage,
            &scope,
            &nod_reader,
            &NodItemState {
                nod_id,
                owner: nod_owner,
                gratis_load_minor: U256::from(1),
                worldwide_day: day,
                league_id: 2,
                floor_price_minor: U256::from(13),
                bucket_key,
                issuance_currency: 840,
                reference_currency: 978,
                issued_at: 15,
            },
            U256::from(16),
        )?;
        Err(outbe_primitives::error::PrecompileError::Fatal(
            "later transaction stage failed".into(),
        ))
    })
    .expect_err("failed transaction must roll back every body namespace");
    assert!(error.to_string().contains("later transaction stage failed"));
    let mut read_provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut failed_state, ctx);
    StorageHandle::enter(&mut read_provider, |storage| {
        assert_eq!(
            B256::from(
                storage
                    .sload(
                        outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
                        U256::from(1),
                    )?
                    .to_be_bytes::<32>(),
            ),
            outbe_compressed_entities::sealed_root(B256::ZERO).unwrap()
        );
        assert_eq!(TributeContract::new(storage.clone()).total_supply()?, 0);
        assert_eq!(NodContract::new(storage).total_supply()?, 0);
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .unwrap();
}

#[test]
fn finish_uses_final_extra_data_setter_for_summary_validation() {
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone());
    let db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();
    let evm_env = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
        block_env: BlockEnv {
            number: U256::from(1u64),
            gas_limit: 30_000_000,
            basefee: 1_000_000_000,
            beneficiary: OWNER,
            timestamp: U256::from(1u64),
            ..Default::default()
        },
    };
    let evm = config.evm_with_env(&mut state, evm_env);
    let ctx = execution_ctx(Some(0), Bytes::new());
    let mut executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        None,
        Bytes::new(),
        None,
        true,
        None,
        ctx.inner.parent_hash,
        None,
        ctx.expected_begin_system_txs.clone(),
        ctx.expected_end_system_txs.clone(),
        ctx.system_layout_error.clone(),
        ctx.parent_consensus_metadata.clone(),
        ctx.proposer_evm_address,
        ctx.execute_outbe_block_hooks,
        ctx.prebuilt_phase1_tx.clone(),
        ctx.parent_artifact_hint,
    );
    let final_extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        execution_summary: Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        }),
        consensus_header_artifact: None,
        timestamp_millis_part: 0,
        late_finalize_credits: None,
        compressed_entities_root: None,
    })
    .expect("final extra_data must encode");

    executor.set_final_extra_data(final_extra_data);
    let artifacts = outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(
        executor.final_extra_data().as_ref(),
    )
    .unwrap();
    super::validate_execution_summary_artifact(
        true,
        1,
        artifacts.execution_summary,
        executor.current_execution_summary(),
    )
    .expect("final extra_data summary must validate");
}

#[test]
fn finish_without_final_extra_data_setter_rejects_missing_summary() {
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone());
    let db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();
    let evm_env = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
        block_env: BlockEnv {
            number: U256::from(1u64),
            gas_limit: 30_000_000,
            basefee: 1_000_000_000,
            beneficiary: OWNER,
            timestamp: U256::from(1u64),
            ..Default::default()
        },
    };
    let evm = config.evm_with_env(&mut state, evm_env);
    let ctx = execution_ctx(Some(0), Bytes::new());
    let executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        None,
        Bytes::new(),
        None,
        true,
        None,
        ctx.inner.parent_hash,
        None,
        ctx.expected_begin_system_txs.clone(),
        ctx.expected_end_system_txs.clone(),
        ctx.system_layout_error.clone(),
        ctx.parent_consensus_metadata.clone(),
        ctx.proposer_evm_address,
        ctx.execute_outbe_block_hooks,
        ctx.prebuilt_phase1_tx.clone(),
        ctx.parent_artifact_hint,
    );

    let artifacts = outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(
        executor.final_extra_data().as_ref(),
    )
    .unwrap();
    let err = super::validate_execution_summary_artifact(
        true,
        1,
        artifacts.execution_summary,
        executor.current_execution_summary(),
    )
    .expect_err("stale pre-summary extra_data must not validate");

    assert!(err
        .to_string()
        .contains("missing execution summary artifact in block extra_data"));
}

#[test]
fn finish_rejects_non_artifact_header_extra_data() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();
    let evm_env = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
        block_env: BlockEnv {
            number: U256::from(1u64),
            gas_limit: 30_000_000,
            basefee: 1_000_000_000,
            beneficiary: OWNER,
            timestamp: U256::from(1u64),
            ..Default::default()
        },
    };
    let evm = config.evm_with_env(&mut state, evm_env);
    let ctx = execution_ctx(Some(0), Bytes::from_static(b"reth/vtest/macos"));
    let executor = config.create_executor(evm, ctx);

    let err = match executor.finish() {
        Ok(_) => {
            panic!("non-artifact extra_data must currently reproduce the payload-builder failure")
        }
        Err(err) => err,
    };

    assert!(err
        .to_string()
        .contains("unknown non-empty extra_data block artifact"));
}
