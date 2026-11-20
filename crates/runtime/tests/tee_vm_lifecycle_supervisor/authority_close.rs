//! Door-side refresh reads the next canonical block from the same provider used by both RPCs.
use crate::{
    chain_storage,
    execution_committee::Committee,
    proof,
    rejoin_execution::{import_register, Ledger},
    Harness, Signer, TEST_CHAIN_ID,
};
use alloy_consensus::TxEip1559;
use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
use alloy_primitives::{Address, Bytes, TxKind, B256, U256};
use alloy_sol_types::SolCall as _;
use commonware_runtime::Runner as _;
use jsonrpsee::{server::MethodsError, Methods, RpcModule};
use outbe_core_common::confidential_tee::{
    derive_account_keys_message, eip191_hash, Ledger as Domain,
};
use outbe_execution::tee_remote_session::RegistryChainIdentity;
use outbe_primitives::{
    tee_attestation_v1::{AttestationOperationV1, TrustedNetworkDescriptorV1},
    OutbeBlock,
};
use outbe_rpc::OutbeApiServer as _;
use outbe_tee::{
    authority::{
        install_tee_authority_watch, note_finalized_authority_revision, tee_authority_test_guard,
    },
    dcap_protocol::{DcapOnboardingArtifactV1, DcapOnboardingContextV1},
    finalized_admission::upgrade_registry_slots_v1,
    in_process::InProcessTeeContext,
    upgrade_transfer::UpgradeKeyProofV1,
    TeeAuthorityChannel, TeeAuthorityState,
};
use reth_ethereum::TransactionSigned;
use reth_primitives_traits::{SealedHeader, SignedTransaction as _};
use reth_provider::ReceiptProvider;
use std::sync::Arc;

#[path = "authority_follower.rs"]
mod authority_follower;
#[path = "../support/follower_import.rs"]
mod follower_import;
use authority_follower::{
    build_successor, chain_spec, recovered_headers, seed_execution_committee, Successor,
};

struct PrivateRequests {
    upgrade: (Bytes, UpgradeKeyProofV1, bool),
    derive: (Domain, Address, B256, Bytes),
}

impl PrivateRequests {
    fn new(context: &DcapOnboardingContextV1, proof: UpgradeKeyProofV1) -> Self {
        let signer = crate::test_signers().0;
        let account = signer.address();
        let ephemeral = B256::from(
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([0x53; 32])).to_bytes(),
        );
        let signature = signer
            .sign_hash(&eip191_hash(&derive_account_keys_message(
                Domain::Gratis,
                account,
                ephemeral,
            )))
            .unwrap();
        Self {
            upgrade: (Bytes::from(context.encode_canonical()), proof, false),
            derive: (
                Domain::Gratis,
                account,
                ephemeral,
                Bytes::copy_from_slice(&signature),
            ),
        }
    }

    async fn assert_open(&self, rpc: &Methods, context: DcapOnboardingContextV1) {
        let artifact: Bytes = rpc
            .call("outbe_upgradeKeyV1", self.upgrade.clone())
            .await
            .unwrap();
        assert_eq!(
            DcapOnboardingArtifactV1::decode_canonical(&artifact)
                .unwrap()
                .context,
            context
        );
        let keys: outbe_runtime::domain_rpc::GratisKeysSealed = rpc
            .call("outbe_deriveKeys", self.derive.clone())
            .await
            .unwrap();
        assert!(!keys.sealed.is_empty());
    }

    async fn assert_closed(&self, rpc: &Methods) {
        for error in [
            rpc.call::<_, Bytes>("outbe_upgradeKeyV1", self.upgrade.clone())
                .await
                .unwrap_err(),
            rpc.call::<_, outbe_runtime::domain_rpc::GratisKeysSealed>(
                "outbe_deriveKeys",
                self.derive.clone(),
            )
            .await
            .unwrap_err(),
        ] {
            let MethodsError::JsonRpc(error) = error else {
                panic!("{error:?}");
            };
            assert_eq!(error.message(), "PrivateExecutionUnavailable");
        }
    }
}

pub(super) async fn authority_closes_private_rpc(expected: TeeAuthorityState) {
    tokio::task::spawn_blocking(move || {
        let _watch_guard = tee_authority_test_guard();
        let h = Harness::new();
        let datadir = tempfile::tempdir().unwrap();
        let config = commonware_runtime::tokio::Config::new()
            .with_storage_directory(datadir.path().join("marshal"));
        commonware_runtime::tokio::Runner::new(config)
            .start(|ctx| run_authority_case(ctx, h, expected, datadir.path()));
    })
    .await
    .unwrap();
}

async fn run_authority_case(
    ctx: commonware_runtime::tokio::Context,
    mut h: Harness,
    expected: TeeAuthorityState,
    datadir: &std::path::Path,
) {
    outbe_consensus::config::init_consensus_chain_id(TEST_CHAIN_ID).unwrap();
    let mut spec = chain_spec();
    let genesis = spec.sealed_genesis_header();
    let mut descriptor = proof::descriptor(&crate::finality::FinalityCommitteeFixture::new(10));
    descriptor.network_binding.genesis_hash = genesis.hash();
    h.live = Arc::new(proof::context(11, &descriptor));
    h.chain.at.genesis_hash = genesis.hash();
    h.chain.policy.genesis_hash = genesis.hash();
    spec.genesis.config.extra_fields.insert(
        "teeAttestationV1".into(),
        outbe_primitives::tee_test_utils::tee_attestation_v1_extra_field(&h.chain.policy).unwrap(),
    );
    let spec = Arc::new(spec);
    h.chain.at.height = 0;
    h.chain.at.hash = genesis.hash();
    let donor = install_donor(&mut h, &descriptor);
    let producer_tee = outbe_tee::in_process_tee_context().unwrap();
    let committee = Committee::new(10);
    let mut ledger = Ledger::new(&h, h.chain.public);
    h.chain.at = ledger.tip;
    h.chain.balance = ledger.nonce_and_balance().1;
    assert!(matches!(
        h.step().await,
        crate::engine::Step::Enrollment(
            outbe_tee::lifecycle::enrollment::EnrollmentOutcome::Submitted { .. }
        )
    ));
    import_register(&mut h, &mut ledger, &committee).await;
    drop(donor);
    install_resident(&h.live);
    let proposer = Arc::new(outbe_evm::OutbeEvmSigner::from_secret_bytes([8; 32]).unwrap());
    let snapshot = seed_execution_committee(
        &mut ledger,
        &committee,
        (proposer.address(), producer_tee.attestation_public()),
        expected == TeeAuthorityState::SuspendedJailed,
    );
    let binding = ledger.binding();
    let context = stage_candidate(&mut ledger, &h, &binding, &descriptor);
    authority_follower::seed_recovered_state(&mut ledger);
    ledger.advance(binding.valid_until - 1, None);
    let blocks = recovered_headers(&mut ledger, genesis.hash());
    let first = ledger.tip;
    let anchor = SealedHeader::seal_slow(ledger.block.header.clone());
    let (_, state) = ledger.snapshot();
    let node = follower_import::launch_with_seed(
        Arc::clone(&spec),
        &datadir.join("follower"),
        Some(crate::test_signers().0),
        move |provider| {
            chain_storage::seed_recovered_anchor(provider, blocks, state)?;
            Ok(())
        },
    )
    .await
    .unwrap();
    let provider = Arc::new(node.provider.clone());
    let mut follower = follower_import::FollowerImport::start(
        ctx,
        node,
        OutbeBlock {
            header: genesis.header().clone(),
            body: outbe_primitives::OutbeBlockBody {
                withdrawals: Some(Default::default()),
                ..Default::default()
            },
        },
        &anchor,
    )
    .await;
    let channel = TeeAuthorityChannel::default();
    let watch = channel.subscribe();
    install_tee_authority_watch(watch.clone()).unwrap();
    let scheduler = outbe_runtime::LifecycleAuthorityProbe::new(
        Arc::clone(&h.live),
        outbe_consensus::validators::LocalTeeRuntimeIdentityV1 {
            reth_p2p_public: h.live.manifest().node_id.reth_p2p_public,
            expected_enclave_id: Some(binding.enclave_id),
            validator: Some(Signer::address()),
        },
        (first.height, first.hash),
        channel,
    )
    .unwrap();
    let protocol = Arc::new(outbe_runtime::OutbeRuntime::new());
    let chain = RegistryChainIdentity {
        chain_id: first.chain_id,
        genesis_hash: first.genesis_hash,
    };
    scheduler.bind_sync_refresh(provider.as_ref().clone(), Arc::clone(&protocol), chain);
    note_finalized_authority_revision(first.height);
    assert_eq!(
        watch.ensure_current(),
        TeeAuthorityState::Armed {
            enclave_id: binding.enclave_id,
            valid_until: binding.valid_until,
        }
    );
    let mut rpc = RpcModule::new(());
    rpc.merge(
        outbe_rpc::OutbeApiHandler::new(Arc::clone(&provider), protocol.clone())
            .with_chain_identity(chain.chain_id, chain.genesis_hash)
            .into_rpc(),
    )
    .unwrap();
    rpc.merge(scheduler.domain_rpc(Arc::clone(&provider), (chain.chain_id, chain.genesis_hash)))
        .unwrap();
    let requests = PrivateRequests::new(
        &context,
        ledger.opening(&committee, upgrade_registry_slots_v1(&context)),
    );
    requests.assert_open(&rpc, context).await;

    let config = outbe_evm::OutbeEvmConfig::new_with_provider_only(
        spec,
        Arc::new(outbe_evm::RethAccountedParentArtifactProvider::new(
            provider.as_ref().clone(),
            None,
        )),
        protocol,
    )
    .with_evm_signer(Arc::clone(&proposer));
    let timestamp = binding.valid_until + 1;
    let block = build_successor(
        &config,
        provider.as_ref(),
        (&committee, &snapshot),
        proposer.address(),
        Successor {
            parent: &anchor,
            timestamp,
            user: None,
        },
    );
    let closed = SealedHeader::seal_slow(block.header.clone());
    follower.finalize(block).await;
    follower_import::assert_finalized(
        provider.as_ref(),
        provider.as_ref(),
        first.height + 1,
        closed.hash(),
    );
    let observed_at_close = watch.observed_finalized_height();
    requests.assert_closed(&rpc).await;
    assert_eq!(watch.snapshot(), expected);
    assert_eq!(
        h.live.network_encryption_public().unwrap(),
        context.network_encryption_public
    );

    let transaction = private_tx(ledger.nonce_and_balance().0, timestamp);
    let tx_hash = *transaction.tx_hash();
    let block = {
        let _producer =
            outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(producer_tee));
        build_successor(
            &config,
            provider.as_ref(),
            (&committee, &snapshot),
            proposer.address(),
            Successor {
                parent: &closed,
                timestamp: timestamp + 1,
                user: Some(transaction.try_into_recovered().unwrap()),
            },
        )
    };
    let hash = block.header.inner.hash_slow();
    assert!(provider.receipt_by_hash(tx_hash).unwrap().is_none());
    follower.finalize(block).await;
    let height = first.height + 2;
    follower_import::assert_finalized(provider.as_ref(), provider.as_ref(), height, hash);
    assert!(
        provider
            .receipt_by_hash(tx_hash)
            .unwrap()
            .expect("finalized confidential receipt")
            .success
    );
    requests.assert_closed(&rpc).await;
    assert_eq!(watch.snapshot(), expected);
    assert_eq!(watch.applied_finalized_height(), height);
    assert_eq!(
        h.live.network_encryption_public().unwrap(),
        context.network_encryption_public
    );
    assert_eq!(
        observed_at_close,
        first.height + 1,
        "H finalization notification"
    );
    assert_eq!(
        watch.observed_finalized_height(),
        height,
        "H+1 finalization notification"
    );
}

fn install_resident(context: &Arc<InProcessTeeContext>) {
    if let Some(previous) = outbe_tee::in_process_tee_context() {
        outbe_tee::replace_in_process_tee_context::<outbe_tee::InProcessDispatchError>(
            &previous,
            Arc::clone(context),
            || Ok(()),
        )
        .unwrap();
    } else {
        outbe_tee::install_in_process_tee_context(Arc::clone(context)).unwrap();
    }
}

fn install_donor(
    h: &mut Harness,
    descriptor: &TrustedNetworkDescriptorV1,
) -> outbe_tee::in_process_dispatch::InProcessTestGuard {
    let donor = proof::context(1, descriptor);
    donor
        .activate_for_test(
            outbe_tee::resident::ResidentOfferMaterial::for_test(
                vec![5; 96],
                B256::from(donor.manifest().chain_id),
                0,
                0,
            )
            .unwrap(),
        )
        .unwrap();
    h.chain.public = donor.network_encryption_public().unwrap().into();
    outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(Arc::new(donor)))
}

fn stage_candidate(
    ledger: &mut Ledger,
    h: &Harness,
    binding: &outbe_tee::registry_view::RenewalBindingV1,
    descriptor: &TrustedNetworkDescriptorV1,
) -> DcapOnboardingContextV1 {
    let candidate = proof::context(12, descriptor);
    let mut intent =
        crate::shared::intent(&candidate, AttestationOperationV1::PrepareEnclaveUpgrade);
    intent.policy_hash = binding.policy_hash;
    let context = proof::onboarding_context(&intent, h.chain.public.0);
    ledger.storage(|storage| {
        let registry = outbe_teeregistry::TeeRegistry::new(storage);
        registry
            .strict_upgrade_successor
            .write(context.policy_hash)
            .unwrap();
        registry
            .upgrade_candidate_context
            .write(&context.node_id_hash, context.context_hash())
            .unwrap();
        registry
            .upgrade_candidate_expiry
            .write(&context.node_id_hash, binding.valid_until)
            .unwrap();
        registry
            .upgrade_candidate_source
            .write(&context.node_id_hash, binding.binding_id)
            .unwrap();
        registry
            .upgrade_candidate_nonce
            .write(&context.node_id_hash, 1)
            .unwrap();
    });
    context
}

fn private_tx(nonce: u64, timestamp: u64) -> TransactionSigned {
    let signer = crate::test_signers().0;
    let account = signer.address();
    let expiry = timestamp + 60;
    let message = outbe_fidelity::fidelity_query_auth_message(
        B256::from(U256::from(TEST_CHAIN_ID)),
        account,
        expiry,
    );
    let signature = signer.sign_hash(&eip191_hash(&message)).unwrap();
    let input = outbe_fidelity::precompile::IFidelity::getFidelityIndexCall {
        account,
        expiry,
        signature: Bytes::copy_from_slice(&signature),
    }
    .abi_encode()
    .into();
    signer
        .sign_eip1559(TxEip1559 {
            chain_id: TEST_CHAIN_ID,
            nonce,
            gas_limit: 800_000,
            max_fee_per_gas: u128::from(MIN_PROTOCOL_BASE_FEE),
            to: TxKind::Call(outbe_system_common::addresses::FIDELITY_ADDRESS),
            input,
            ..Default::default()
        })
        .unwrap()
}
