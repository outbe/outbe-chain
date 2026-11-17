//! Fresh RAM identity with the stable, owner-only NodeHost key kept on disk.
use super::*;
use outbe_primitives::tee_attestation_v1::{
    EnclaveInitializationManifestV1, NodeIdV1, TrustedNetworkDescriptorV1,
};
use outbe_tee::{
    in_process::{InProcessTeeContext, ProofSourceV1},
    resident::ResidentTeeKeys,
};

pub(super) fn initialize(
    config: &LaunchConfig,
    node_data_dir: &Path,
    policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
) -> eyre::Result<outbe_consensus::validators::LocalTeeRuntimeIdentityV1> {
    // Unsupported hardware fails before creating any persistent identity.
    let proof = ProofSourceV1::NoHardware;
    proof.ensure_available(policy.attestation_mode)?;
    let binding =
        outbe_tee::tee_attestation_activation::DcapSeededChainSpecBindingV1::from_chain_spec(
            &config.chain,
        )?;
    let (_, reth_p2p_public) =
        load_reth_p2p_node_host_signer(&config.network, config.datadir().p2p_secret())?;
    let node_host = load_node_host_key(node_data_dir)?;
    let keys = ResidentTeeKeys::generate()?;
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: policy.chain_id,
        genesis_hash: policy.genesis_hash,
        attestation_mode: policy.attestation_mode,
        node_id: NodeIdV1 { reth_p2p_public },
        initialization_challenge: alloy_primitives::B256::random().0,
        node_host_noise_x25519: node_host.public(),
        recipient_x25519: keys.tribute_offer_public(),
        attestation_ed25519: keys.attestation_pub(),
        noise_responder_x25519: keys.noise_public(),
    };
    let context = Arc::new(InProcessTeeContext::new(
        keys,
        outbe_tee::NodeHostIdentityV1 {
            network_binding: policy.network_binding(),
            reth_p2p_public,
        },
        manifest,
        TrustedNetworkDescriptorV1 {
            network_binding: policy.network_binding(),
            genesis_consensus_keys: binding.genesis_consensus_keys,
        },
        proof,
    )?);
    let expected_enclave_id = Some(context.manifest().enclave_id()?);
    outbe_tee::install_in_process_tee_context(
        context,
        outbe_enclave::host::domain_in_process::dispatch_payload,
    )?;
    Ok(outbe_consensus::validators::LocalTeeRuntimeIdentityV1 {
        reth_p2p_public,
        expected_enclave_id,
        validator: None,
    })
}

#[derive(Debug, thiserror::Error)]
enum NodeHostStartupError {
    #[error("NodeHost directory must be a non-symlink directory with mode 0700")]
    InvalidDirectory,
}

fn load_node_host_key(node_data_dir: &Path) -> eyre::Result<outbe_tee::NodeHostNoiseKey> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    let directory = node_data_dir.join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1);
    let fresh = match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error.into()),
    };
    let metadata = std::fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o777 != 0o700 {
        return Err(NodeHostStartupError::InvalidDirectory.into());
    }
    let path = directory.join(outbe_tee::node_host::NODE_HOST_NOISE_KEY_V1);
    // A previously initialized directory never regenerates a lost identity.
    Ok(if fresh {
        outbe_tee::NodeHostNoiseKey::create_new(&path)?
    } else {
        outbe_tee::NodeHostNoiseKey::load(&path)?
    })
}

pub(super) fn sidecar_enclave_id(
    quote: &outbe_tee::protocol::EnclaveResponse,
) -> eyre::Result<alloy_primitives::B256> {
    let outbe_tee::protocol::EnclaveResponse::Quote {
        recipient_x25519_pub,
        attestation_pub,
        noise_static_pub,
        ..
    } = quote
    else {
        return Err(
            crate::admission_authority::live::LiveTeeError::LegacyIdentityUnavailable.into(),
        );
    };
    let mut preimage = Vec::from(outbe_primitives::tee_attestation_v1::ENCLAVE_ID_DOMAIN_V1);
    preimage.extend_from_slice(recipient_x25519_pub);
    preimage.extend_from_slice(attestation_pub);
    preimage.extend_from_slice(noise_static_pub);
    Ok(alloy_primitives::keccak256(preimage))
}

pub(super) fn bind_activation_refresh<P>(
    protocol: &Arc<crate::OutbeRuntime>,
    provider: P,
    config: crate::admission::TeeLeaseGuardConfigV1,
    (authority, owner): (
        outbe_tee::TeeAuthorityChannel,
        Option<Arc<crate::admission_authority::live::LiveTeeRuntimeV1>>,
    ),
) -> eyre::Result<()>
where
    P: BlockIdReader
        + HeaderProvider<Header = OutbeHeader>
        + StateProviderFactory
        + Send
        + Sync
        + 'static,
{
    if let Some(owner) = &owner {
        owner.bind_armed(config.armed.clone())?;
    }
    let weak_protocol = Arc::downgrade(protocol);
    let legacy = std::sync::Mutex::new((config.anchors, config.anchors.gate.is_armed()));
    protocol.install_tee_refresh(Arc::new(move || {
        use outbe_consensus::protocol_state::EngineProtocolError;
        let protocol = weak_protocol
            .upgrade()
            .ok_or(EngineProtocolError::NotInstalled)?;
        let next = if let Some(owner) = &owner {
            owner.refresh(&provider, protocol.as_ref(), &config.chain)
        } else {
            let mut state = legacy.lock().map_err(|_| {
                EngineProtocolError::TeeRefresh(
                    crate::admission_authority::live::LiveTeeError::LockPoisoned.into(),
                )
            })?;
            let (anchors, bootstrap) = &mut *state;
            crate::admission_authority::guard::tick(
                (&provider, protocol.as_ref()),
                (&config.chain, config.identity),
                (anchors, bootstrap),
                &authority,
                || outbe_tee::resident_network_encryption_public_key_state_v1().map_err(Into::into),
            )
        }
        .map_err(EngineProtocolError::TeeRefresh)?;
        let open = next.is_authority_open();
        if *config.armed.borrow() != open {
            config.armed.send_replace(open);
        }
        Ok(())
    }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::{TeeLeaseGuardAnchorsV1, TeeLeaseGuardConfigV1, TeeLeaseGuardGateV1};
    use crate::admission_authority::{live::*, test_support as support};
    use outbe_consensus::protocol_state::EngineProtocolState as _;
    use outbe_tee::TeeAuthorityState;

    #[test]
    fn development_quote_identity_matches_the_canonical_registration_identity() {
        use outbe_primitives::tee_attestation_v1::{AttestationOperationV1, RegistrationIntentV1};
        use outbe_tee::protocol::EnclaveResponse;
        let context = support::context(9);
        let manifest = context.manifest();
        let intent = RegistrationIntentV1 {
            chain_id: manifest.chain_id,
            genesis_hash: manifest.genesis_hash,
            operation: AttestationOperationV1::RegisterEnclave,
            attestation_mode: manifest.attestation_mode,
            policy_hash: alloy_primitives::B256::repeat_byte(1),
            node_id: manifest.node_id.clone(),
            enclave_id: manifest.enclave_id().unwrap(),
            binding_id: alloy_primitives::B256::repeat_byte(2),
            binding_version: 1,
            registration_version: 1,
            renewal_nonce: 0,
            transition_nonce: 0,
            requested_valid_until: 1000,
            recipient_x25519: manifest.recipient_x25519,
            attestation_ed25519: manifest.attestation_ed25519,
            noise_responder_x25519: manifest.noise_responder_x25519,
            node_host_authorization_hash: manifest.node_host_authorization_hash().unwrap(),
        };
        let quote = EnclaveResponse::Quote {
            mrenclave: Default::default(),
            mrsigner: Default::default(),
            isv_svn: 0,
            report_data: Default::default(),
            recipient_x25519_pub: intent.recipient_x25519,
            attestation_pub: intent.attestation_ed25519,
            noise_static_pub: intent.noise_responder_x25519,
            quote_body: Vec::new(),
            attestation: "none".to_owned(),
        };
        assert_eq!(
            sidecar_enclave_id(&quote).unwrap(),
            intent.derived_enclave_id().unwrap()
        );
    }

    #[test]
    fn node_host_key_is_stable_and_loss_never_regenerates_identity() {
        let root = tempfile::tempdir().unwrap();
        let first = load_node_host_key(root.path()).unwrap().public();
        assert_eq!(load_node_host_key(root.path()).unwrap().public(), first);
        let path = root
            .path()
            .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
            .join(outbe_tee::node_host::NODE_HOST_NOISE_KEY_V1);
        std::fs::remove_file(&path).unwrap();
        assert!(load_node_host_key(root.path()).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn activation_callback_opens_bootstrap_and_bool_watch_before_any_worker_tick() {
        let _port = outbe_tee::authority::tee_authority_test_guard();
        let original = support::context(9);
        let (provider, chain, identity) =
            support::provider(&original, 0, 1_700_000_000, 1_800_001_000, false);
        let context = Arc::new(
            InProcessTeeContext::new(
                ResidentTeeKeys::from_seed([9; 32]).unwrap(),
                *original.identity(),
                original.manifest().clone(),
                TrustedNetworkDescriptorV1 {
                    network_binding: original.manifest().network_binding(),
                    genesis_consensus_keys: vec![],
                },
                ProofSourceV1::NoHardware,
            )
            .unwrap(),
        );
        provider.inner.accounts.lock().clear();
        let authority = outbe_tee::TeeAuthorityChannel::default();
        outbe_tee::install_tee_authority_watch(authority.subscribe()).unwrap();
        let anchors = TeeLeaseGuardAnchorsV1 {
            gate: TeeLeaseGuardGateV1::Armed,
            durable: None,
        };
        let owner = Arc::new(
            LiveTeeRuntimeV1::new(
                LiveTeeSnapshotV1 {
                    identity,
                    anchors,
                    context: context.clone(),
                },
                authority.clone(),
            )
            .unwrap(),
        );
        let runtime = Arc::new(crate::OutbeRuntime::new());
        let (armed, armed_rx) = tokio::sync::watch::channel(false);
        bind_activation_refresh(
            &runtime,
            provider,
            TeeLeaseGuardConfigV1 {
                chain,
                identity,
                anchors,
                finalization_notify: Arc::new(tokio::sync::Notify::new()),
                armed,
            },
            (authority.clone(), Some(owner)),
        )
        .unwrap();
        runtime.refresh_tee_authority().unwrap();
        assert_eq!(
            authority.snapshot(),
            TeeAuthorityState::UnkeyedAwaitingAdmission
        );
        assert!(!*armed_rx.borrow());
        context
            .activate_for_test(
                outbe_tee::resident::ResidentOfferMaterial::for_test(
                    vec![5; 96],
                    alloy_primitives::B256::from(context.manifest().chain_id),
                    0,
                    0,
                )
                .unwrap(),
            )
            .unwrap();
        runtime.refresh_tee_authority().unwrap();
        assert_eq!(
            authority.snapshot(),
            TeeAuthorityState::ArmedBootstrap {
                enclave_id: context.manifest().enclave_id().unwrap()
            }
        );
        assert!(*armed_rx.borrow());
        assert_eq!(runtime.tee_authority_state(), authority.snapshot());
    }

    #[test]
    fn durable_checkpoint_cannot_arm_a_fresh_process_without_the_resident_offer_key() {
        let old = support::context(9);
        let (provider, chain, identity) =
            support::provider(&old, 7, 1_800_000_000, 1_800_001_000, false);
        let fresh = Arc::new(
            InProcessTeeContext::new(
                ResidentTeeKeys::from_seed([9; 32]).unwrap(),
                *old.identity(),
                old.manifest().clone(),
                TrustedNetworkDescriptorV1 {
                    network_binding: old.manifest().network_binding(),
                    genesis_consensus_keys: vec![],
                },
                ProofSourceV1::NoHardware,
            )
            .unwrap(),
        );
        let finalized = provider.finalized_block_num_hash().unwrap().unwrap();
        let anchor = crate::admission::LocalTeeAdmissionAnchorV1 {
            finalized_height: finalized.number,
            finalized_hash: finalized.hash,
        };
        let owner = LiveTeeRuntimeV1::new(
            LiveTeeSnapshotV1 {
                identity,
                context: fresh.clone(),
                anchors: TeeLeaseGuardAnchorsV1 {
                    gate: TeeLeaseGuardGateV1::Pending(anchor),
                    durable: Some(anchor),
                },
            },
            outbe_tee::TeeAuthorityChannel::default(),
        )
        .unwrap();
        assert_eq!(
            owner
                .refresh(&provider, &crate::OutbeRuntime::new(), &chain)
                .unwrap(),
            TeeAuthorityState::UnkeyedAwaitingAdmission
        );
        assert!(!owner.authority.snapshot().is_authority_open());
        assert!(!fresh.offer_key_ready().unwrap());
    }
}
