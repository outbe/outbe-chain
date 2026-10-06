//! Authenticated transport admission and pre-registered startup channels.
use super::super::*;
use outbe_radicle::integration::{EndpointSigningIdentity, EndpointTransport};

pub(super) type Channel<E> = (
    lookup::Sender<bls12381::PublicKey, E>,
    lookup::Receiver<bls12381::PublicKey>,
);
pub(super) type ChannelMux<E> = commonware_p2p::utils::mux::MuxHandle<
    lookup::Sender<bls12381::PublicKey, E>,
    lookup::Receiver<bls12381::PublicKey>,
>;
pub(super) type Subchannel<E> = (
    commonware_p2p::utils::mux::SubSender<lookup::Sender<bls12381::PublicKey, E>>,
    commonware_p2p::utils::mux::SubReceiver<lookup::Receiver<bls12381::PublicKey>>,
);
pub(super) type EpochSubchannels<E> = outbe_consensus::epoch_subchannels::EpochSubchannels<
    lookup::Sender<bls12381::PublicKey, E>,
    lookup::Receiver<bls12381::PublicKey>,
>;
pub(in crate::stack) fn radicle_channel_config() -> (u64, u32) {
    (
        config::RADICLE_ENDPOINT_CHANNEL,
        config::RADICLE_ENDPOINT_CHANNEL_QUOTA,
    )
}

/// Muxer mailbox size for sub-channel buffering.
const MUXER_MAILBOX: usize = 1024;

pub(super) struct StartupTransport<E: Clock> {
    pub(super) network_handle: commonware_runtime::Handle<()>,
    pub(super) oracle: lookup::Oracle<bls12381::PublicKey>,
    pub(super) bootnode_map: BTreeMap<Vec<u8>, SocketAddr>,
    pub(super) initial_peers: commonware_p2p::AddressableTrackedPeers<bls12381::PublicKey>,
    pub(super) broadcast_channel: Channel<E>,
    pub(super) marshal_channel: Channel<E>,
    pub(super) vote_mux: ChannelMux<E>,
    pub(super) cert_mux: ChannelMux<E>,
    pub(super) res_mux: ChannelMux<E>,
    pub(super) dkg_mux: ChannelMux<E>,
    pub(super) tee_dkg_round0: Option<Subchannel<E>>,
    pub(super) tee_bootstrap_round0: Option<Subchannel<E>>,
    pub(super) tee_dkg_mux: Option<ChannelMux<E>>,
    pub(super) tee_bootstrap_mux: Option<ChannelMux<E>>,
}
/// Bind P2P admission to the same canonical state used to load membership.
pub(super) struct TransportAdmission<'a> {
    pub(super) signing_key: &'a bls12381::PrivateKey,
    pub(super) validator_set: &'a validators::ValidatorSet,
    pub(super) initial_peer_hash: B256,
    pub(super) ocomp_install_hash: Option<B256>,
}

pub(super) async fn start_transport<E>(
    ctx: &E,
    args: &ConsensusArgs,
    node: &OutbeFullNode,
    admission: TransportAdmission<'_>,
    radicle_endpoint: Option<(
        outbe_radicle::integration::EndpointNetworkService,
        outbe_radicle::integration::LocalEndpointIdentityHandle,
        outbe_radicle::integration::EndpointTaskOwner,
    )>,
) -> Result<Option<StartupTransport<E>>>
where
    E: BufferPooler
        + Clock
        + CryptoRng
        + Network
        + Resolver
        + Spawner
        + Storage
        + Metrics
        + Send
        + Sync
        + 'static,
{
    let TransportAdmission {
        signing_key,
        validator_set,
        initial_peer_hash,
        ocomp_install_hash,
    } = admission;
    // -- 3. Configure P2P network ---------------------------------------
    let p2p_namespace = ocomp_p2p_namespace(ocomp_install_hash);
    // Cover the full registered validator set plus a local non-validator identity.
    let max_peers_per_set =
        NonZeroUsize::new(outbe_consensus::bls::MAX_VALIDATORS as usize + 1).unwrap();
    let network_cfg = if args.use_local_defaults {
        lookup::Config::local(
            signing_key.clone(),
            &p2p_namespace,
            args.listen_address,
            max_peers_per_set,
            config::MAX_P2P_MESSAGE_SIZE,
        )
    } else {
        lookup::Config::recommended(
            signing_key.clone(),
            &p2p_namespace,
            args.listen_address,
            max_peers_per_set,
            config::MAX_P2P_MESSAGE_SIZE,
        )
    };

    let (mut network, mut oracle) = lookup::Network::new(ctx.child("network"), network_cfg);

    // Register Simplex consensus channels. Muxers wrap them later.
    let votes = network.register(config::VOTES_CHANNEL, Quota::per_second(NZU32!(128)));
    let certificates =
        network.register(config::CERTIFICATES_CHANNEL, Quota::per_second(NZU32!(128)));
    let resolver = network.register(config::RESOLVER_CHANNEL, Quota::per_second(NZU32!(64)));

    // Register broadcast channel for block dissemination (buffered engine).
    let broadcast_channel =
        network.register(config::BROADCAST_CHANNEL, Quota::per_second(NZU32!(32)));

    // Register marshal resolver channel for on-demand block backfill.
    let marshal_channel = network.register(config::MARSHAL_CHANNEL, Quota::per_second(NZU32!(64)));

    // Register DKG ceremony channel (muxed by reshare round).
    let dkg_channel = network.register(config::DKG_CHANNEL, Quota::per_second(NZU32!(128)));

    // Register the one-time TEE bootstrap channel (only when a TEE enclave
    // sidecar is configured). The node uses it once at startup, like the DKG.
    // It coordinates the committee's enclave registrations + EVM signatures into
    // the block-1 `TeeBootstrap` payload. Register it before `network.start()`.
    let mut tee_bootstrap_channel = args
        .tee_enclave_socket
        .as_ref()
        .map(|_| network.register(config::TEE_BOOTSTRAP_CHANNEL, Quota::per_second(NZU32!(64))));

    // Register the one-time TEE DKG channel (only when a TEE enclave sidecar is
    // configured). It carries the enclave identity exchange, the dealer/player
    // gossip, and the offer-key partial-signature round. That round derives the
    // shared tribute offer key at startup. Register it before `network.start()`.
    let mut tee_dkg_channel = args
        .tee_enclave_socket
        .as_ref()
        .map(|_| network.register(config::TEE_DKG_CHANNEL, Quota::per_second(NZU32!(128))));

    let radicle_channel = radicle_endpoint.as_ref().map(|_| {
        let (channel, quota) = radicle_channel_config();
        network.register(
            channel,
            Quota::per_second(NonZeroU32::new(quota).expect("Radicle quota is non-zero")),
        )
    });

    // Parse consensus peers: `<hex_pubkey>@<host:port>` -> (PublicKey, SocketAddr).
    let bootnode_map = parse_consensus_peers(&args.consensus_peers)?;

    if !bootnode_map.is_empty() {
        info!(count = bootnode_map.len(), "parsed bootnode entries");
    }

    // Build peer set from validator config + bootnodes.
    let peer_map = build_peer_map(validator_set, &bootnode_map);
    let admitted_set =
        validators::read_admitted_non_consensus_at_block(&node.provider, initial_peer_hash)
            .wrap_err("failed to read startup non-consensus P2P admission")?;
    let initial_peers = commonware_p2p::AddressableTrackedPeers::new(
        peer_map,
        build_peer_map(&admitted_set, &bootnode_map),
    );
    let resolved_count = initial_peers.primary.len();
    eyre::ensure!(
        oracle.track(0, initial_peers.clone()) == commonware_actor::Feedback::Ok,
        "P2P oracle closed during startup admission"
    );
    info!(
        total = validator_set.public_keys.len(),
        resolved = resolved_count,
        bootnodes = bootnode_map.len(),
        "P2P peer set registered with oracle"
    );

    // -- 4. Start P2P network (needed before DKG can run) ---------------
    let network_handle = network.start();
    info!("P2P network started");

    if let (Some((endpoint, local, owner)), Some((sender, receiver))) =
        (radicle_endpoint, radicle_channel)
    {
        let signer = signing_key.clone();
        if !owner.start(async move {
            let result = endpoint
                .run(
                    EndpointTransport { sender, receiver },
                    EndpointSigningIdentity { signer, local },
                )
                .await;
            if let Err(error) = &result {
                tracing::warn!(%error, "Radicle endpoint actor stopped");
            }
            result
        })? {
            return Ok(None);
        }
    }

    // -- 5. Create Muxers from physical channels ------------------------
    // The Muxers split consensus channels by epoch. Each engine restart
    // gets fresh sub-channels, which prevents message interference.
    let (vote_muxer, vote_mux) = Muxer::new(ctx.child("vote_mux"), votes.0, votes.1, MUXER_MAILBOX);
    vote_muxer.start();

    let (cert_muxer, cert_mux) = Muxer::new(
        ctx.child("cert_mux"),
        certificates.0,
        certificates.1,
        MUXER_MAILBOX,
    );
    cert_muxer.start();

    let (res_muxer, res_mux) =
        Muxer::new(ctx.child("res_mux"), resolver.0, resolver.1, MUXER_MAILBOX);
    res_muxer.start();

    // DKG channel muxed by reshare round.
    let (dkg_muxer, dkg_mux) = Muxer::new(
        ctx.child("dkg_mux"),
        dkg_channel.0,
        dkg_channel.1,
        MUXER_MAILBOX,
    );
    dkg_muxer.start();

    // R5.4: mux the TEE DKG + TEE bootstrap channels by round, as `dkg_mux` does.
    // The startup ceremony (round 0) and a later epoch-boundary reshare (round N)
    // then each get isolated sub-channels. The value is `None` when no TEE enclave
    // sidecar is set.
    let mut tee_dkg_mux = tee_dkg_channel.take().map(|ch| {
        let (muxer, handle) = Muxer::new(ctx.child("tee_dkg_mux"), ch.0, ch.1, MUXER_MAILBOX);
        muxer.start();
        handle
    });
    let mut tee_bootstrap_mux = tee_bootstrap_channel.take().map(|ch| {
        let (muxer, handle) = Muxer::new(ctx.child("tee_boot_mux"), ch.0, ch.1, MUXER_MAILBOX);
        muxer.start();
        handle
    });

    // R5.4: pre-register the round-0 TEE sub-channels EARLY, as the consensus
    // `dkg_mux.register(0)` does at startup. Then every node routes round 0 well
    // before the startup TEE DKG begins. Lazy registration inside the startup block
    // races: a node can broadcast its identity before a peer registers round 0.
    // The mux then drops the unrouted message -> the identity exchange hangs.
    // Reshare rounds (N>0) still register on demand at the boundary.
    let tee_dkg_round0 = match tee_dkg_mux.as_mut() {
        Some(m) => Some(
            m.register(0)
                .await
                .map_err(|e| eyre::eyre!("failed to pre-register TEE DKG round 0: {e}"))?,
        ),
        None => None,
    };
    let tee_bootstrap_round0 = match tee_bootstrap_mux.as_mut() {
        Some(m) => Some(
            m.register(0)
                .await
                .map_err(|e| eyre::eyre!("failed to pre-register TEE bootstrap round 0: {e}"))?,
        ),
        None => None,
    };
    info!("channel muxers started");

    Ok(Some(StartupTransport {
        network_handle,
        oracle,
        bootnode_map,
        initial_peers,
        broadcast_channel,
        marshal_channel,
        vote_mux,
        cert_mux,
        res_mux,
        dkg_mux,
        tee_dkg_round0,
        tee_bootstrap_round0,
        tee_dkg_mux,
        tee_bootstrap_mux,
    }))
}
