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
