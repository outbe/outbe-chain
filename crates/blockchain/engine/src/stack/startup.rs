use super::*;

pub(in crate::stack) fn repository_publication_enabled_from_genesis(
    node: &OutbeFullNode,
) -> Result<bool> {
    use outbe_primitives::radicle_genesis::{
        GovernmentsDescriptor, GENESIS_GOVERNMENTS_FIELD, GOVERNMENTS_DESCRIPTOR_HASH_SLOT,
    };
    let spec = node.chain_spec();
    let genesis = &spec.genesis;
    let slot = alloy_primitives::B256::from(
        alloy_primitives::U256::from(GOVERNMENTS_DESCRIPTOR_HASH_SLOT).to_be_bytes::<32>(),
    );
    let seeded_hash = genesis
        .alloc
        .get(&outbe_primitives::addresses::CODE_REGISTRY_ADDRESS)
        .and_then(|account| account.storage.as_ref())
        .and_then(|storage| storage.get(&slot))
        .copied()
        .unwrap_or_default();
    Ok(GovernmentsDescriptor::from_genesis_field(
        genesis.config.extra_fields.get(GENESIS_GOVERNMENTS_FIELD),
        spec.chain().id(),
        genesis.timestamp,
        seeded_hash,
    )
    .map_err(|error| {
        crate::errors::StackStartupError::InvalidGovernmentsGenesis(error.to_string())
    })?
    .is_some())
}

/// Read `epochLengthBlocks` from genesis.json `config` section.
/// Falls back to [`config::DEFAULT_EPOCH_LENGTH_BLOCKS`] if absent.
pub(in crate::stack) fn epoch_length_blocks_from_genesis(node: &OutbeFullNode) -> Result<u32> {
    let extra = &node.chain_spec().genesis.config.extra_fields;
    if extra.get("epochDuration").is_some() {
        return Err(crate::errors::StackStartupError::DeprecatedEpochDuration.into());
    }
    if extra.get("dkgRotationIntervalBlocks").is_some() {
        return Err(crate::errors::StackStartupError::DeprecatedDkgRotationIntervalBlocks.into());
    }

    match extra.get_deserialized::<u32>("epochLengthBlocks") {
        Some(Ok(0)) => Err(crate::errors::StackStartupError::EpochLengthBlocksZero.into()),
        Some(Ok(value)) => Ok(value),
        Some(Err(error)) => Err(crate::errors::StackStartupError::InvalidEpochLengthBlocks(
            error.to_string(),
        )
        .into()),
        None => Ok(config::DEFAULT_EPOCH_LENGTH_BLOCKS),
    }
}

/// Read a `u64` millisecond timing value from genesis `config`, falling back to
/// `default` when the key is absent. It is generic over the deserialize error.
/// So the engine crate needs no direct `serde_json` dependency, and the helper
/// stays unit-testable with a plain string error.
pub(in crate::stack) fn read_ms<E: std::fmt::Display>(
    parsed: Option<Result<u64, E>>,
    key: &str,
    default: u64,
) -> Result<u64> {
    match parsed {
        Some(Ok(value)) => Ok(value),
        Some(Err(error)) => Err(
            crate::errors::StackStartupError::InvalidGenesisTimingConfig {
                key: key.to_string(),
                error: error.to_string(),
            }
            .into(),
        ),
        None => Ok(default),
    }
}

/// Startup invariants for the consensus-sync timing trio (structured error, no
/// panic): `0 < min < leader <= cert`. This function rejects a `minBlockTimeMs`
/// of `0`. The proposer floor cannot be disabled.
pub(in crate::stack) fn validate_timing(min_ms: u64, leader_ms: u64, cert_ms: u64) -> Result<()> {
    if min_ms == 0 {
        return Err(crate::errors::StackStartupError::MinBlockTimeMsZero.into());
    }
    if leader_ms == 0 {
        return Err(crate::errors::StackStartupError::LeaderTimeoutMsZero.into());
    }
    if cert_ms == 0 {
        return Err(crate::errors::StackStartupError::CertificationTimeoutMsZero.into());
    }
    if min_ms >= leader_ms {
        return Err(
            crate::errors::StackStartupError::MinBlockTimeExceedsLeaderTimeout {
                min_ms,
                leader_ms,
            }
            .into(),
        );
    }
    if leader_ms > cert_ms {
        return Err(
            crate::errors::StackStartupError::LeaderTimeoutExceedsCertificationTimeout {
                leader_ms,
                cert_ms,
            }
            .into(),
        );
    }
    Ok(())
}

/// Consensus-sync block-timing knobs, resolved from genesis with `timing.rs`
/// fallbacks. There is no CLI override for any of these (see
/// `outbe_consensus::timing`). The values are in-memory only. The node never
/// writes them to EVM storage.
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
        .map_err(|e| crate::errors::StackStartupError::GetGenesisHash(format!("{e}")))?;
    require_genesis_hash(hash)
}

pub(in crate::stack) fn require_genesis_hash(hash: Option<B256>) -> Result<B256> {
    hash.ok_or_else(|| crate::errors::StackStartupError::MissingGenesisBlockHash.into())
}

/// Read the canonical height-0 genesis block from the provider and wrap it as a
/// [`ConsensusBlock`](outbe_consensus::block::ConsensusBlock).
///
/// commonware 2026.5.0 replaced the removed `Automaton::genesis` call with an
/// explicit `marshal::Config.start`. Marshal's `Start::Genesis` anchor must be
/// the real height-0 block (the actor asserts `anchor.height() == 0`). So we
/// read the canonical genesis block straight from the execution DB rather than
/// synthesizing one. Sealing comes from the provider's stored block, so the
/// anchor's `block_hash()` is byte-identical to the chain `genesis_hash`.
pub(in crate::stack) fn genesis_consensus_block(
    node: &OutbeFullNode,
) -> Result<outbe_consensus::block::ConsensusBlock> {
    let recovered = node
        .provider
        .recovered_block(0u64.into(), TransactionVariant::NoHash)
        .map_err(|e| crate::errors::StackStartupError::ReadGenesisBlock(format!("{e}")))?
        .ok_or(crate::errors::StackStartupError::MissingGenesisBlock)?;
    Ok(outbe_consensus::block::ConsensusBlock::from_sealed(
        recovered.into_sealed_block(),
    ))
}

pub(in crate::stack) fn nonzero_u16(value: u16, name: &str) -> Result<NonZeroU16> {
    NonZeroU16::new(value).ok_or_else(|| {
        crate::errors::StackStartupError::ValueMustBePositive(name.to_string()).into()
    })
}

pub(in crate::stack) fn nonzero_usize(value: usize, name: &str) -> Result<NonZeroUsize> {
    NonZeroUsize::new(value).ok_or_else(|| {
        crate::errors::StackStartupError::ValueMustBePositive(name.to_string()).into()
    })
}

pub(in crate::stack) fn nonzero_u64(value: u64, name: &str) -> Result<NonZeroU64> {
    NonZeroU64::new(value).ok_or_else(|| {
        crate::errors::StackStartupError::ValueMustBePositive(name.to_string()).into()
    })
}

/// Map `marshal::core::Actor::init`'s `Option<Height>` to the executor/startup
/// finalized height:
/// - `None` (no durable consensus finalization yet) means a fresh genesis node.
///   It maps to height 0.
/// - `Some(n)` resumes from the durable finalized height `n` (finalization is
///   monotonic).
///
/// A restarted node that already finalized must NOT be reset toward genesis.
/// This mapping is a separate function so that the regression test exercises
/// this exact mapping rather than stdlib `unwrap_or`.
pub(crate) fn map_marshal_init_height(opt: Option<Height>) -> Height {
    opt.unwrap_or(Height::zero())
}

pub(in crate::stack) fn parse_consensus_peers(
    entries: &[String],
) -> Result<BTreeMap<Vec<u8>, SocketAddr>> {
    let mut peers = BTreeMap::new();
    for entry in entries {
        let (pk_hex, addr_str) = entry.split_once('@').ok_or_else(|| {
            crate::errors::StackStartupError::ConsensusPeerMissingAtSign {
                entry: entry.clone(),
            }
        })?;

        if pk_hex.is_empty() {
            return Err(
                crate::errors::StackStartupError::ConsensusPeerEmptyPublicKey {
                    entry: entry.clone(),
                }
                .into(),
            );
        }

        let pk_bytes = hex::decode(pk_hex).map_err(|e| {
            crate::errors::StackStartupError::ConsensusPeerPublicKeyNotHex {
                entry: entry.clone(),
                error: format!("{e}"),
            }
        })?;
        if pk_bytes.is_empty() {
            return Err(
                crate::errors::StackStartupError::ConsensusPeerDecodedPublicKeyEmpty {
                    entry: entry.clone(),
                }
                .into(),
            );
        }

        let addr = addr_str.parse::<SocketAddr>().map_err(|e| {
            crate::errors::StackStartupError::ConsensusPeerInvalidSocketAddr {
                entry: entry.clone(),
                error: format!("{e}"),
            }
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
        return Err(
            crate::errors::StackStartupError::TrustElHeadNotAllowedOnChain(chain_id).into(),
        );
    }
    if unix_time_offset_secs.is_some() && !is_explicit_test_network {
        return Err(
            crate::errors::StackStartupError::UnixTimeOffsetNotAllowedOnChain(chain_id).into(),
        );
    }
    Ok(())
}

/// Build a P2P peer map from a validator set and bootnode entries.
///
/// Registry/config P2P address takes priority. Bootnodes fill missing gaps.
/// This function excludes invalid registry entries and never replaces them with
/// static bootstrap addresses.
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
