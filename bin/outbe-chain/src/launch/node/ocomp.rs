use super::*;

pub(super) struct OcompBootstrap {
    pub config: ocomp_exex::OcompExExConfigV1,
    pub retention_selector: Arc<SharedOcompRetentionSelector>,
    pub readiness_publisher: outbe_offchain_data::ProjectionReadinessPublisher,
    pub readiness: ProjectionReadinessHandle,
}
pub(super) fn prepare(
    config: &LaunchConfig,
    args: &ConsensusArgs,
    node_data_dir: &Path,
    ocomp_fork_install: &Arc<outbe_metadosis::config::OcompForkInstallV1>,
    ocomp_install_hash: alloy_primitives::B256,
) -> eyre::Result<OcompBootstrap> {
    let ocomp_limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    let ocomp_domain_root = node_data_dir
        .parent()
        .ok_or_else(|| eyre::eyre!("node data directory has no OCOMP domain parent"))?
        .join("ocomp")
        .join("domain-v1");
    let ocomp_bundle_bytes = ocomp_fork_install
        .protocol_bundle
        .encode_canonical(&ocomp_limits)?;
    let ocomp_bundle = outbe_ocomp::bundle::PinnedProtocolBundle::decode(
        &ocomp_bundle_bytes,
        ocomp_fork_install.request_profile.protocol_bundle_hash,
        &ocomp_limits,
    )?;
    let configured_ocomp_bundle_hashes = std::env::var("OCOMP_PROTOCOL_BUNDLE_HASHES").ok();
    let ocomp_bundles = load_installed_ocomp_bundles(
        &ocomp_domain_root,
        ocomp_bundle,
        configured_ocomp_bundle_hashes.as_deref(),
        &ocomp_limits,
    )?;
    let ocomp_worker_base_port = args
        .listen_address
        .port()
        .checked_add(1)
        .ok_or_else(|| eyre::eyre!("consensus port leaves no OCOMP Worker endpoint port"))?;
    let mut ocomp_runtime_bundles = Vec::with_capacity(ocomp_bundles.len());
    let ocomp_lane_port_stride =
        u16::try_from(outbe_ocomp::worker_transport::MAX_REGISTERED_WORKERS)
            .map_err(|_| eyre::eyre!("OCOMP worker limit exceeds u16"))?
            .checked_add(2)
            .ok_or_else(|| eyre::eyre!("OCOMP bundle lane port stride overflow"))?;
    for (index, bundle) in ocomp_bundles.into_iter().enumerate() {
        let lane =
            u16::try_from(index).map_err(|_| eyre::eyre!("OCOMP bundle lane count exceeds u16"))?;
        let port_offset = lane
            .checked_mul(ocomp_lane_port_stride)
            .ok_or_else(|| eyre::eyre!("OCOMP bundle lane port offset overflow"))?;
        let worker_port = ocomp_worker_base_port
            .checked_add(port_offset)
            .ok_or_else(|| eyre::eyre!("OCOMP bundle lane leaves no Worker endpoint port"))?;
        let worker_address = std::net::SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            worker_port,
        );
        info!(
            bundle_hash = %bundle.hash(),
            lane = index,
            %worker_address,
            "loaded pinned OCOMP runtime bundle lane"
        );
        ocomp_runtime_bundles.push(ocomp_exex::OcompExExBundleConfigV1 {
            worker_address,
            identity: outbe_ocomp_protocol::local_control::EndpointIdentity {
                chain_id: config.chain.chain().id(),
                genesis_hash: config.chain.genesis_hash(),
                boot_nonce: ocomp_install_hash,
                protocol_bundle_hash: bundle.hash(),
            },
            protocol_bundle: bundle,
        });
    }
    let ocomp_policy = if args.is_validator {
        outbe_ocomp::embedded_runtime::EmbeddedNodePolicyV1::Validator
    } else {
        outbe_ocomp::embedded_runtime::EmbeddedNodePolicyV1::FullNode
    };
    let ocomp_validator_rpc_url = if args.is_validator {
        if !config.rpc.http {
            eyre::bail!("validator OCOMP requires the local HTTP RPC server");
        }
        Some(format!("http://127.0.0.1:{}", config.rpc.http_port))
    } else {
        None
    };
    let retention_selector = Arc::new(SharedOcompRetentionSelector::new());
    let discovery_spool_root = ocomp_domain_root.join("exporter-v1/discovery");
    let ocomp_exex_config = ocomp_exex::OcompExExConfigV1 {
        domain_root: ocomp_domain_root,
        discovery_spool_root,
        bundles: ocomp_runtime_bundles,
        policy: ocomp_policy,
        validator_rpc_url: ocomp_validator_rpc_url,
        chain_id: config.chain.chain().id(),
        genesis_hash: config.chain.genesis_hash(),
        retention_selector: Arc::clone(&retention_selector),
        retention_required: args.is_validator || args.upstream.is_some(),
    };
    let ocomp_baseline = ProjectionCheckpoint {
        block_number: 0,
        block_hash: config.chain.genesis_hash(),
    };
    let (ocomp_readiness_publisher, ocomp_readiness) = projection_readiness(
        ocomp_baseline,
        ProjectionStatus::Ready {
            checkpoint: ocomp_baseline,
        },
    );
    Ok(OcompBootstrap {
        config: ocomp_exex_config,
        retention_selector,
        readiness_publisher: ocomp_readiness_publisher,
        readiness: ocomp_readiness,
    })
}

pub(super) fn lysis_openings_runtime<P>(provider: Arc<P>) -> outbe_rpc::OcompLysisOpeningsRuntimeV1
where
    P: HeaderProvider + BlockIdReader + StateProviderFactory + Send + Sync + 'static,
{
    outbe_rpc::OcompLysisOpeningsRuntimeV1::new(move |intent_id, canonical_request| {
        let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
        let request = outbe_ocomp_protocol::control::BuildLysisOpeningsV1::decode_body(
            canonical_request.as_ref(),
            &limits,
        )
        .map_err(|error| format!("decode OCOMP openings request: {error}"))?;
        let finalized_head = provider
            .finalized_block_num_hash()
            .map_err(|error| format!("read finalized head: {error}"))?
            .ok_or_else(|| "finalized head is unavailable".to_owned())?;
        let record = outbe_node::ocomp::retention::read_ocomp_job_record_at(
            provider.as_ref(),
            finalized_head.hash,
            intent_id,
            &limits,
        )
        .map_err(|error| format!("read finalized OCOMP job: {error}"))?;
        let finalized = record
            .finalized
            .as_ref()
            .ok_or_else(|| "OCOMP job is not finalized".to_owned())?;
        if !ocomp_job_available_for_calculation(record.status) {
            return Err("OCOMP job is not available for calculation or replay".to_owned());
        }
        if request.job_id != finalized.job_id {
            return Err("OCOMP openings request JobId mismatch".to_owned());
        }
        let candidate = outbe_node::ocomp::retention::CandidatePinV1 {
            block_number: record.intent_height,
            block_hash: finalized.finalized_request_block_hash,
            state_root: finalized.finalized_request_state_root,
            intent_id,
            wwd: record.intent.wwd,
            ce_sealed_root: record.intent.ce_sealed_root,
            protocol_bundle_hash: record.intent.protocol_bundle_hash,
            input_lease_id: record
                .intent
                .input_lease_id()
                .map_err(|error| format!("derive input lease: {error}"))?,
        };
        let openings = outbe_node::ocomp::build_lysis_openings(
            provider.as_ref(),
            &limits,
            candidate,
            request.subjects,
        )
        .map_err(|error| format!("build exact OCOMP openings: {error}"))?;
        if openings.job_id != finalized.job_id {
            return Err("OCOMP openings JobId mismatch".to_owned());
        }
        openings
            .encode_body(&limits)
            .map(alloy_primitives::Bytes::from)
            .map_err(|error| format!("encode OCOMP openings: {error}"))
    })
}

pub(super) struct StartupChainAuthority {
    pub initial_tee_policy: outbe_primitives::tee_attestation_v1::TeePolicyV1,
    pub dkg_prepare_window_blocks: u64,
    pub minimum_block_time_millis: u64,
    pub ocomp_fork_install: Arc<outbe_metadosis::config::OcompForkInstallV1>,
    pub ocomp_install_hash: alloy_primitives::B256,
}

pub(super) fn validate_startup_authority_and_pruning(
    config: &LaunchConfig,
) -> eyre::Result<StartupChainAuthority> {
    let tee_attestation_v1 =
        outbe_evm::tee_attestation_activation::TeeAttestationChainSpecStateV1::from_chain_spec(
            config.chain.as_ref(),
        );
    let tee_activation = tee_attestation_v1
        .activation()
        .map_err(|error| eyre::eyre!("invalid mandatory teeAttestationV1 ChainSpec: {error}"))?;
    let initial_tee_policy = tee_activation
        .policy_at(outbe_evm::tee_attestation_activation::TEE_ATTESTATION_V1_ACTIVATION_HEIGHT)
        .map_err(eyre::Report::msg)?;
    let extra = &config.chain.genesis.config.extra_fields;
    let dkg_prepare_window_blocks = extra
        .get_deserialized::<u64>("dkgPrepareWindowBlocks")
        .transpose()
        .map_err(|error| eyre::eyre!("invalid dkgPrepareWindowBlocks: {error}"))?
        .unwrap_or(outbe_consensus::config::DEFAULT_DKG_PREPARE_WINDOW_BLOCKS);
    let minimum_block_time_millis = extra
        .get_deserialized::<u64>("minBlockTimeMs")
        .transpose()
        .map_err(|error| eyre::eyre!("invalid minBlockTimeMs: {error}"))?
        .unwrap_or(outbe_consensus::timing::DEFAULT_MIN_BLOCK_TIME_MS);
    info!(
        attestation_mode = ?initial_tee_policy.attestation_mode,
        activation_height = tee_activation.manifest.activation_height,
        policy_schedule_hash = %tee_activation.manifest.policy_schedule_hash,
        "validated mandatory TEE attestation ChainSpec authority"
    );
    let ocomp_fork_install =
        outbe_node::ocomp::fork::require_startup_ocomp_fork_install(config.chain.as_ref())?;
    let ocomp_limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    let ocomp_install_hash = ocomp_fork_install.install_hash(&ocomp_limits)?;
    info!(
        activation_height = ocomp_fork_install.activation_height,
        classification = ?ocomp_fork_install.classification,
        install_hash = %ocomp_install_hash,
        "validated genesis-active immutable OCOMP chain-manifest install"
    );
    let prune_config = config.pruning.prune_config(config.chain.as_ref());
    validate_compressed_storage_runtime_config(CompressedStorageRuntimeConfig {
        persistence_threshold: config.engine.persistence_threshold,
        memory_block_buffer_target: config.engine.memory_block_buffer_target(),
        max_pending_acks: outbe_consensus::config::MAX_PENDING_ACKS,
        receipts_pruning_enabled: prune_config
            .as_ref()
            .is_some_and(|cfg| cfg.has_receipts_pruning()),
        account_history_pruning_enabled: prune_config
            .as_ref()
            .is_some_and(|cfg| cfg.segments.account_history.is_some()),
        storage_history_pruning_enabled: prune_config
            .as_ref()
            .is_some_and(|cfg| cfg.segments.storage_history.is_some()),
    })?;
    Ok(StartupChainAuthority {
        initial_tee_policy: initial_tee_policy.clone(),
        dkg_prepare_window_blocks,
        minimum_block_time_millis,
        ocomp_fork_install,
        ocomp_install_hash,
    })
}
