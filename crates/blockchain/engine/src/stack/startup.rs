use super::*;

/// Read `epochLengthBlocks` from genesis.json `config` section.
/// Falls back to [`config::DEFAULT_EPOCH_LENGTH_BLOCKS`] if absent.
pub(in crate::stack) fn epoch_length_blocks_from_genesis(node: &OutbeFullNode) -> Result<u32> {
    let extra = &node.chain_spec().genesis.config.extra_fields;
    if extra.get("epochDuration").is_some() {
        return Err(eyre::eyre!(
            "genesis config uses deprecated epochDuration; use epochLengthBlocks"
        ));
    }
    if extra.get("dkgRotationIntervalBlocks").is_some() {
        return Err(eyre::eyre!(
            "genesis config uses deprecated dkgRotationIntervalBlocks; use epochLengthBlocks"
        ));
    }

    match extra.get_deserialized::<u32>("epochLengthBlocks") {
        Some(Ok(0)) => Err(eyre::eyre!("genesis config epochLengthBlocks must be > 0")),
        Some(Ok(value)) => Ok(value),
        Some(Err(error)) => Err(eyre::eyre!(
            "invalid genesis config epochLengthBlocks: {error}"
        )),
        None => Ok(config::DEFAULT_EPOCH_LENGTH_BLOCKS),
    }
}

/// Read a `u64` millisecond timing value from genesis `config`, falling back to
/// `default` when the key is absent. Generic over the deserialize error so the
/// engine crate needs no direct `serde_json` dependency and the helper stays
/// unit-testable with a plain string error.
pub(in crate::stack) fn read_ms<E: std::fmt::Display>(
    parsed: Option<Result<u64, E>>,
    key: &str,
    default: u64,
) -> Result<u64> {
    match parsed {
        Some(Ok(value)) => Ok(value),
        Some(Err(error)) => Err(eyre::eyre!("invalid genesis config {key}: {error}")),
        None => Ok(default),
    }
}

/// Startup invariants for the consensus-sync timing trio (structured error, no
/// panic): `0 < min < leader <= cert`. A `minBlockTimeMs` of `0` is rejected -
/// the proposer floor cannot be disabled.
pub(in crate::stack) fn validate_timing(min_ms: u64, leader_ms: u64, cert_ms: u64) -> Result<()> {
    if min_ms == 0 {
        return Err(eyre::eyre!(
            "genesis config minBlockTimeMs must be > 0 (the floor cannot be disabled)"
        ));
    }
    if leader_ms == 0 {
        return Err(eyre::eyre!("genesis config leaderTimeoutMs must be > 0"));
    }
    if cert_ms == 0 {
        return Err(eyre::eyre!(
            "genesis config certificationTimeoutMs must be > 0"
        ));
    }
    if min_ms >= leader_ms {
        return Err(eyre::eyre!(
            "genesis config minBlockTimeMs ({min_ms}) must be < leaderTimeoutMs ({leader_ms})"
        ));
    }
    if leader_ms > cert_ms {
        return Err(eyre::eyre!(
            "genesis config leaderTimeoutMs ({leader_ms}) must be <= certificationTimeoutMs ({cert_ms})"
        ));
    }
    Ok(())
}

/// Consensus-sync block-timing knobs, resolved from genesis with `timing.rs`
/// fallbacks. There is no CLI override for any of these (see
/// `outbe_consensus::timing`). In-memory only; never written to EVM storage.
#[derive(Clone, Copy, Debug)]
pub(in crate::stack) struct BlockTiming {
    pub(in crate::stack) min_block_time: std::time::Duration,
    pub(in crate::stack) leader_timeout: std::time::Duration,
    pub(in crate::stack) certification_timeout: std::time::Duration,
}

/// Read the timing trio from genesis `config` (`minBlockTimeMs` /
/// `leaderTimeoutMs` / `certificationTimeoutMs`), each falling back to its
/// `timing.rs` default, then validate the startup invariants.
pub(in crate::stack) fn block_timing_from_genesis(node: &OutbeFullNode) -> Result<BlockTiming> {
    let extra = &node.chain_spec().genesis.config.extra_fields;
    let min_ms = read_ms(
        extra.get_deserialized::<u64>("minBlockTimeMs"),
        "minBlockTimeMs",
        outbe_consensus::timing::DEFAULT_MIN_BLOCK_TIME_MS,
    )?;
    let leader_ms = read_ms(
        extra.get_deserialized::<u64>("leaderTimeoutMs"),
        "leaderTimeoutMs",
        outbe_consensus::timing::DEFAULT_LEADER_TIMEOUT_MS,
    )?;
    let cert_ms = read_ms(
        extra.get_deserialized::<u64>("certificationTimeoutMs"),
        "certificationTimeoutMs",
        outbe_consensus::timing::DEFAULT_CERTIFICATION_TIMEOUT_MS,
    )?;
    validate_timing(min_ms, leader_ms, cert_ms)?;
    Ok(BlockTiming {
        min_block_time: std::time::Duration::from_millis(min_ms),
        leader_timeout: std::time::Duration::from_millis(leader_ms),
        certification_timeout: std::time::Duration::from_millis(cert_ms),
    })
}

pub(in crate::stack) fn genesis_hash(node: &OutbeFullNode) -> Result<B256> {
    let hash = node
        .provider
        .block_hash(0)
        .map_err(|e| eyre::eyre!("failed to get genesis hash: {e}"))?;
    require_genesis_hash(hash)
}

pub(in crate::stack) fn require_genesis_hash(hash: Option<B256>) -> Result<B256> {
    hash.ok_or_else(|| eyre::eyre!("missing genesis block hash from provider"))
}

/// Read the canonical height-0 genesis block from the provider and wrap it as a
/// [`ConsensusBlock`](outbe_consensus::block::ConsensusBlock).
///
/// commonware 2026.5.0 replaced the removed `Automaton::genesis` call with an
/// explicit `marshal::Config.start`. Marshal's `Start::Genesis` anchor must be
/// the real height-0 block (the actor asserts `anchor.height() == 0`), so we
/// read the canonical genesis block straight from the execution DB rather than
/// synthesizing one. Sealing comes from the provider's stored block, so the
/// anchor's `block_hash()` is byte-identical to the chain `genesis_hash`.
pub(in crate::stack) fn genesis_consensus_block(
    node: &OutbeFullNode,
) -> Result<outbe_consensus::block::ConsensusBlock> {
    let recovered = node
        .provider
        .recovered_block(0u64.into(), TransactionVariant::NoHash)
        .map_err(|e| eyre::eyre!("failed to read genesis block from provider: {e}"))?
        .ok_or_else(|| eyre::eyre!("missing genesis block (height 0) from provider"))?;
    Ok(outbe_consensus::block::ConsensusBlock::from_sealed(
        recovered.into_sealed_block(),
    ))
}

pub(in crate::stack) fn nonzero_u16(value: u16, name: &str) -> Result<NonZeroU16> {
    NonZeroU16::new(value).ok_or_else(|| eyre::eyre!("{name} must be > 0"))
}

pub(in crate::stack) fn nonzero_usize(value: usize, name: &str) -> Result<NonZeroUsize> {
    NonZeroUsize::new(value).ok_or_else(|| eyre::eyre!("{name} must be > 0"))
}

pub(in crate::stack) fn nonzero_u64(value: u64, name: &str) -> Result<NonZeroU64> {
    NonZeroU64::new(value).ok_or_else(|| eyre::eyre!("{name} must be > 0"))
}

/// Map `marshal::core::Actor::init`'s `Option<Height>` to the executor/startup
/// finalized height: `None` (no durable consensus finalization yet) means a
/// fresh genesis node, mapped to height 0; `Some(n)` resumes from the durable
/// finalized height `n` (finalization is monotonic). A restarted node that
/// already finalized must NOT be reset toward genesis. Extracted so the
/// regression test exercises this exact mapping rather than stdlib `unwrap_or`.
pub(crate) fn map_marshal_init_height(opt: Option<Height>) -> Height {
    opt.unwrap_or(Height::zero())
}

pub(in crate::stack) fn parse_consensus_peers(
    entries: &[String],
) -> Result<BTreeMap<Vec<u8>, SocketAddr>> {
    let mut peers = BTreeMap::new();
    for entry in entries {
        let (pk_hex, addr_str) = entry.split_once('@').ok_or_else(|| {
            eyre::eyre!("invalid consensus peer {entry:?}: expected <hex_bls_pubkey>@<host:port>")
        })?;

        ensure!(
            !pk_hex.is_empty(),
            "invalid consensus peer {entry:?}: public key is empty"
        );

        let pk_bytes = hex::decode(pk_hex).map_err(|e| {
            eyre::eyre!("invalid consensus peer {entry:?}: public key is not hex: {e}")
        })?;
        ensure!(
            !pk_bytes.is_empty(),
            "invalid consensus peer {entry:?}: decoded public key is empty"
        );

        let addr = addr_str.parse::<SocketAddr>().map_err(|e| {
            eyre::eyre!("invalid consensus peer {entry:?}: invalid socket address: {e}")
        })?;
        peers.insert(pk_bytes, addr);
    }
    Ok(peers)
}

pub(in crate::stack) fn validate_testnet_only_flags(
    trust_el_head: bool,
    unix_time_offset_secs: Option<i64>,
    chain_id: u64,
) -> Result<()> {
    let is_explicit_test_network = outbe_primitives::chain::is_devnet(chain_id)
        || outbe_primitives::chain::is_testnet(chain_id);
    if trust_el_head && !is_explicit_test_network {
        return Err(eyre::eyre!(
            "--testnet.trust-el-head is not allowed on non-test networks (chain_id {chain_id})"
        ));
    }
    if unix_time_offset_secs.is_some() && !is_explicit_test_network {
        return Err(eyre::eyre!(
            "--testnet.unix-time-offset-secs is not allowed on non-test networks (chain_id {chain_id})"
        ));
    }
    Ok(())
}

pub(in crate::stack) fn ocomp_p2p_namespace(install_hash: Option<B256>) -> Vec<u8> {
    let base = commonware_utils::union_unique(&config::outbe_app_namespace(), b"_P2P");
    let Some(install_hash) = install_hash else {
        return base;
    };
    let mut preimage = Vec::with_capacity(b"OUTBE_OCOMP_P2P_NAMESPACE_V1".len() + base.len() + 32);
    preimage.extend_from_slice(b"OUTBE_OCOMP_P2P_NAMESPACE_V1");
    preimage.extend_from_slice(&base);
    preimage.extend_from_slice(install_hash.as_slice());
    alloy_primitives::keccak256(preimage).to_vec()
}

/// Build a P2P peer map from a validator set and bootnode entries.
///
/// Registry/config P2P address takes priority; bootnodes fill missing gaps.
/// Invalid registry entries are excluded and never replaced with static
/// bootstrap addresses.
pub(crate) fn build_peer_map(
    validator_set: &validators::ValidatorSet,
    bootnode_map: &BTreeMap<Vec<u8>, SocketAddr>,
) -> Map<bls12381::PublicKey, Address> {
    let peer_entries: Vec<(bls12381::PublicKey, Address)> = validator_set
        .public_keys
        .iter()
        .zip(validator_set.p2p_addresses.iter())
        .filter_map(|(pk, p2p_addr)| {
            match p2p_addr {
                validators::ValidatorP2pAddress::Known(addr) => {
                    return Some((pk.clone(), addr.clone()));
                }
                validators::ValidatorP2pAddress::Invalid => return None,
                validators::ValidatorP2pAddress::Missing => {}
            }
            let pk_bytes = commonware_codec::Encode::encode(pk);
            if let Some(addr) = bootnode_map.get(pk_bytes.as_ref()) {
                return Some((pk.clone(), Address::Symmetric(*addr)));
            }
            None
        })
        .collect();

    Map::from_iter_dedup(peer_entries)
}
