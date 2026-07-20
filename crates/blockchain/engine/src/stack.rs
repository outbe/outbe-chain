//! Consensus stack — wires P2P, Simplex engine, application handler, and executor.
//!
//! This is the entry point for the consensus layer, called from the consensus
//! runtime thread in `main.rs`.
//!
//! Startup flow:
//! 1. Load signing key and validator set
//! 2. Set up P2P network (register ALL channels including DKG)
//! 3. Start P2P network and register peers
//! 4. Obtain threshold material:
//!    a. From saved DKG state on disk (restart precedence)
//!    b. From CLI args (`--consensus.signing-share` + `--consensus.public-polynomial`)
//!    c. Via interactive DKG ceremony during fresh genesis formation
//!    d. Via testnet force-DKG disaster recovery when all current validators
//!    intentionally replace lost DKG material on an existing chain
//!    e. Via startup live-join reshare when the chain already has DKG state
//! 5. Create Muxers for epoch-scoped consensus channels
//! 6. Enter epoch loop:
//!    a. Register epoch sub-channels, build HybridScheme + Reporter
//!    b. Start Simplex engine
//!    c. Monitor for reshare triggers (pending_set_change in EVM state)
//!    d. On reshare: run DKG in parallel, then abort engine + restart at new epoch

use alloy_primitives::{Address as EthAddress, Bytes, B256};
use commonware_codec::Read as _;
use commonware_consensus::{
    simplex,
    types::{Epoch, Height, Round, ViewDelta},
    Reporters,
};
use commonware_cryptography::{
    bls12381::{
        self,
        dkg::feldman_desmedt::Output,
        primitives::{
            group::Share,
            sharing::{ModeVersion, Sharing},
            variant::MinSig,
        },
    },
    Signer as _,
};
use commonware_p2p::{
    authenticated::lookup,
    utils::mux::{MuxHandle, Muxer},
    Address, AddressableManager, Receiver as P2pReceiver, Sender as P2pSender,
};
use commonware_runtime::{
    buffer::paged::CacheRef, BufferPooler, Clock, Metrics, Network, Quota, Resolver, Spawner,
    Storage,
};
use commonware_utils::{ordered::Map, TryCollect as _, NZU32};
use eyre::{ensure, Result, WrapErr};
use rand_core::CryptoRngCore;
use reth_ethereum::chainspec::EthChainSpec as _;
use reth_ethereum::network::api::{NetworkInfo, Peers, PeersInfo};
use reth_ethereum::provider::{BlockHashReader, StateProviderFactory};
use reth_node_builder::ConsensusEngineHandle;
use reth_provider::HeaderProvider;
use std::collections::BTreeMap;
use std::future::Future;
use std::net::SocketAddr;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64, NonZeroUsize};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tracing::{debug, info, warn};

use crate::args::ConsensusArgs;
use crate::ce_recovery::CeStartupRecovery;
use crate::validators;
use outbe_consensus::{
    ancestry_readiness::AncestryReadiness,
    application::{
        actor::OutbeApplication,
        handler::{ApplicationDeps, ApplicationHandler},
        ApplicationEpochFence,
    },
    bls,
    committee_provider::CommitteeProvider,
    config,
    digest::Digest,
    dkg_actor,
    dkg_manager::{self, Mailbox as DkgManagerMailbox},
    executor::actor::{ExecutorActor, FinalizedCeCommitter},
    finalization::{
        actor::{FinalizationActor, FinalizationActorDeps},
        block_cache::BlockCache,
        state::new_finalization_view,
    },
    hybrid::{
        election::{HybridElectorConfigProvider, HybridRandom},
        HybridScheme, HybridSchemeProvider, VrfMaterialProvider,
    },
    reporter::{OutbeReporter, ReporterContinuity},
    vrf_safety::VrfSafetyGate,
};

use outbe_node::OutbeFullNode;
use outbe_primitives::{
    consensus::{ConsensusExecutionBridge, DkgBoundaryArtifact},
    projection::ProjectionReadinessHandle,
    reshare_artifact::{
        decode_boundary_artifact, decode_outbe_block_artifacts, encode_boundary_artifact,
        ConsensusHeaderArtifact,
    },
    OutbeHeader, OutbePayloadTypes,
};
use reth_ethereum::storage::{BlockNumReader, BlockReader, TransactionVariant};

/// Type alias for the engine handle.
type EngineHandle = ConsensusEngineHandle<OutbePayloadTypes>;

/// Muxer mailbox size for sub-channel buffering.
const MUXER_MAILBOX: usize = 1024;
const FINALIZED_ROUND_RECOVERY_ATTEMPTS: usize = 5;
const FINALIZED_ROUND_RECOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
const FINALIZED_ROUND_RECOVERY_RETRY_DELAY: std::time::Duration =
    std::time::Duration::from_millis(100);
/// reth's canonical head can lead consensus finalization by the in-flight block
/// (steady state: `head_height = finalized_height + 1`; a few during a
/// finalization hiccup). On a plain restart in that window the head has no
/// finalization record yet — a normal unfinalized head, not archive corruption.
/// A head leading the marshal finalized tip by at most this many blocks is
/// treated as that benign case; a larger lead is suspicious and stays fatal.
const MAX_UNFINALIZED_HEAD_LEAD: u64 = 16;

/// Whether an execution head that leads the marshal's durable finalized tip is
/// the benign "unfinalized in-flight head" case rather than archive corruption:
/// a real finalized tip (`> 0`) and a positive, bounded lead. The caller still
/// confirms the marshal actually holds the finalized tip's finalization record
/// before treating the restart as recoverable.
fn unfinalized_head_lead_is_recoverable(last_execution_height: u64, finalized_tip: u64) -> bool {
    let head_lead = last_execution_height.saturating_sub(finalized_tip);
    finalized_tip > 0 && head_lead > 0 && head_lead <= MAX_UNFINALIZED_HEAD_LEAD
}

/// Highest height that both the execution store and durable consensus finality
/// can authorize at startup. An execution-only head is speculative and must not
/// seed finalized forkchoice state; a consensus-only suffix is backfilled later.
fn durable_recovery_anchor_height(last_execution_height: u64, finalized_tip: u64) -> u64 {
    last_execution_height.min(finalized_tip)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RecoveredApplicationFinalization {
    round: Round,
    digest: Digest,
}

/// Promote the conservative cross-store anchor only when marshal proves the
/// exact canonical execution-head digest. Height agreement alone is not
/// sufficient: it could conceal a same-height fork after a partial restore.
fn reconcile_recovered_execution_head(
    last_execution_height: u64,
    last_execution_hash: B256,
    recovered: Option<RecoveredApplicationFinalization>,
) -> Result<(u64, B256, Option<Round>)> {
    if last_execution_height == 0 {
        ensure!(
            recovered.is_none(),
            "marshal returned a finalization record for genesis execution height"
        );
        return Ok((0, last_execution_hash, None));
    }

    let recovered = recovered.ok_or_else(|| {
        eyre::eyre!(
            "marshal returned no finalization for non-genesis execution height {last_execution_height}"
        )
    })?;
    ensure!(
        recovered.digest.0 == last_execution_hash,
        "marshal finalization digest mismatch at execution height {last_execution_height}: \
         execution={last_execution_hash}, marshal={}",
        recovered.digest.0
    );

    Ok((
        last_execution_height,
        last_execution_hash,
        Some(recovered.round),
    ))
}
/// epoch restart precondition: bounded wait for the finalization
/// view to expose the continuity anchor before launching the new-epoch
/// Simplex engine. Without this, `Automaton::genesis(epoch > 0)` could be
/// queried before the FinalizationActor publishes the boundary block's
/// anchor into `FinalizationView`, and Simplex would lock its `parent_view = 0`
/// to `B256::ZERO` permanently.
const EPOCH_RESTART_ANCHOR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const EPOCH_RESTART_ANCHOR_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(100);

/// A verifier-follower's deferred adoption of a followed DKG rotation's
/// boundary material. The activated cycle's `BoundaryOutcome` rides the FIRST
/// new-epoch block (strictly above the activation height), so it can only be
/// observed after the restarted engine executes past the activation height —
/// the height-event path scans `[next_scan_height, current_height]` forward
/// on each finalized height until the artifact appears, then adopts it.
#[derive(Clone, Copy, Debug)]
struct PendingVerifierBoundaryAdoption {
    /// Activation height of the followed rotation; the adopted boundary must
    /// be committed strictly above it and planned at or above it.
    activation_height: u64,
    /// Next unscanned height — each finalized header is examined once.
    next_scan_height: u64,
}

/// Read `epochLengthBlocks` from genesis.json `config` section.
/// Falls back to [`config::DEFAULT_EPOCH_LENGTH_BLOCKS`] if absent.
fn epoch_length_blocks_from_genesis(node: &OutbeFullNode) -> Result<u32> {
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

/// JSON shape of `genesis.config.teePolicy`, seeded by
/// `scripts/seed_genesis.py`. `B256` fields deserialize from `0x…` hex.
#[derive(serde::Deserialize)]
struct GenesisTeePolicyJson {
    #[serde(default)]
    allowed_mrsigner: Vec<B256>,
    #[serde(default)]
    allowed_mrenclave: Vec<B256>,
    #[serde(default)]
    min_isv_svn: u16,
}

/// Read the genesis TEE attestation policy from `config.teePolicy`. Returns an
/// empty (unconfigured) policy when absent — the producer then emits a payload
/// whose `policy_hash` the Phase 3b handler does not bind (genesis slot 2 stays
/// ZERO). When present it MUST match what `seed_genesis.py` hashed into
/// `TeeRegistry.policy_hash`, so the producer's `payload.policy_hash` equals the
/// genesis-seeded value and the bootstrap is accepted. Used to build the
/// host-side strict connect policy for the consensus DKG/bootstrap sites
/// and the offer-decrypt host connect (`init_enclave_client`).
///
/// `pub` + chain-spec-based so the node binary can build the same policy at
/// startup, where only the chain spec (not a full `OutbeFullNode`) is in scope.
pub fn tee_policy_from_chain_spec(
    spec: &reth_ethereum::chainspec::ChainSpec<OutbeHeader>,
) -> Result<outbe_primitives::tee_bootstrap::TeePolicy> {
    let extra = &spec.genesis.config.extra_fields;
    match extra.get_deserialized::<GenesisTeePolicyJson>("teePolicy") {
        Some(Ok(policy)) => Ok(outbe_primitives::tee_bootstrap::TeePolicy {
            allowed_mrsigner: policy.allowed_mrsigner,
            allowed_mrenclave: policy.allowed_mrenclave,
            min_isv_svn: policy.min_isv_svn,
        }),
        Some(Err(error)) => Err(eyre::eyre!("invalid genesis config teePolicy: {error}")),
        None => Ok(outbe_primitives::tee_bootstrap::TeePolicy::default()),
    }
}

/// Genesis TEE policy for the consensus path, from the live node's chain spec.
fn tee_policy_from_genesis(
    node: &OutbeFullNode,
) -> Result<outbe_primitives::tee_bootstrap::TeePolicy> {
    tee_policy_from_chain_spec(node.chain_spec().as_ref())
}

/// Read a `u64` millisecond timing value from genesis `config`, falling back to
/// `default` when the key is absent. Generic over the deserialize error so the
/// engine crate needs no direct `serde_json` dependency and the helper stays
/// unit-testable with a plain string error.
fn read_ms<E: std::fmt::Display>(
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
/// panic): `0 < min < leader <= cert`. A `minBlockTimeMs` of `0` is rejected —
/// the proposer floor cannot be disabled.
fn validate_timing(min_ms: u64, leader_ms: u64, cert_ms: u64) -> Result<()> {
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
struct BlockTiming {
    min_block_time: std::time::Duration,
    leader_timeout: std::time::Duration,
    certification_timeout: std::time::Duration,
}

/// Read the timing trio from genesis `config` (`minBlockTimeMs` /
/// `leaderTimeoutMs` / `certificationTimeoutMs`), each falling back to its
/// `timing.rs` default, then validate the startup invariants.
fn block_timing_from_genesis(node: &OutbeFullNode) -> Result<BlockTiming> {
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

#[derive(Clone, Copy, Debug)]
struct DkgRotationParams {
    epoch_length_blocks: u64,
    prepare_window_blocks: u64,
    activation_grace_blocks: u64,
}

impl DkgRotationParams {
    fn from_genesis(node: &OutbeFullNode, epoch_length_blocks: u32) -> Self {
        let extra = &node.chain_spec().genesis.config.extra_fields;
        let prepare_window_blocks = extra
            .get_deserialized::<u64>("dkgPrepareWindowBlocks")
            .and_then(|r| r.ok())
            .unwrap_or(config::DEFAULT_DKG_PREPARE_WINDOW_BLOCKS);
        let activation_grace_blocks = extra
            .get_deserialized::<u64>("dkgActivationGraceBlocks")
            .and_then(|r| r.ok())
            .unwrap_or(config::DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS);

        let epoch_length_blocks = u64::from(epoch_length_blocks);
        Self {
            epoch_length_blocks,
            prepare_window_blocks: prepare_window_blocks.min(epoch_length_blocks),
            activation_grace_blocks,
        }
    }

    fn planned_activation_height(self, last_activation_height: u64) -> u64 {
        last_activation_height.saturating_add(self.epoch_length_blocks)
    }

    fn freeze_height(self, last_activation_height: u64) -> u64 {
        self.planned_activation_height(last_activation_height)
            .saturating_sub(self.prepare_window_blocks)
    }
}

fn validate_recovered_vrf_material(
    polynomial: &Sharing<MinSig>,
    boundary: Option<&DkgBoundaryArtifact>,
) -> Result<()> {
    let Some(boundary) = boundary else {
        return Ok(());
    };
    let group_pk_bytes = commonware_codec::Encode::encode(polynomial.public());
    let local_vrf_group_public_key = alloy_primitives::keccak256(&group_pk_bytes);
    ensure!(
        local_vrf_group_public_key == boundary.vrf_group_public_key,
        "saved DKG material does not match finalized VRF group public key"
    );
    Ok(())
}

fn vrf_group_public_key_hash(polynomial: &Sharing<MinSig>) -> B256 {
    let group_pk_bytes = commonware_codec::Encode::encode(polynomial.public());
    alloy_primitives::keccak256(&group_pk_bytes)
}

/// Resolve the consensus participant set for restart/live-join recovery.
///
/// When the node recovers a finalized DKG boundary, the threshold material it
/// restores (`signing_share`, `polynomial`, `last_dkg_output`) belongs to the
/// committee the recovered ceremony ran for — recorded as the DKG output's
/// `players()`. The latest on-chain consensus set may have DRIFTED from that
/// committee (a join/exit/jail/slash after the recovered boundary activated but
/// before the next reshare), so the scheme must NOT be reconstructed against the
/// latest set: committee-dependent data (votes, VRF threshold partials) must be
/// decoded against the committee it was encoded for.
///
/// The recovered output's `players()` is already a sorted, deduplicated
/// `commonware_utils::ordered::Set`, so participant indices derive from it
/// canonically regardless of how the set was assembled — only the *membership*
/// matters, and the members ARE the share holders. In the common no-churn restart
/// this set is identical to the latest committed set, so recovery is unchanged;
/// it diverges only across a churn window, which is exactly the bug this closes.
///
/// **Drift guard.** The recovered boundary records the committee the ceremony ran
/// for in `reshare.new_active_set` (built 1:1 from the same `players()` list at
/// proposal time). A size mismatch between the recovered output's players and
/// that record means the restored consensus material does not correspond to the
/// recovered chain boundary (e.g. a stale or partial consensus-archive restore),
/// so recovery fails fast with an explicit drift error rather than reconstruct
/// the scheme against the wrong committee.
fn select_recovery_participants(
    recovered_output_players: &commonware_utils::ordered::Set<bls12381::PublicKey>,
    boundary: &DkgBoundaryArtifact,
) -> Result<commonware_utils::ordered::Set<bls12381::PublicKey>> {
    let recorded = boundary.reshare.new_active_set.len();
    let recovered = recovered_output_players.len();
    ensure!(
        recovered > 0 && recovered == recorded,
        "validator set has drifted from saved DKG: recovered DKG output has {recovered} \
         player(s) but the recovered boundary (epoch {}, activation height {}) recorded an \
         active set of {recorded} validator(s) — the restored consensus material does not \
         match the chain's recovered DKG boundary",
        boundary.epoch,
        boundary.planned_activation_height,
    );
    Ok(recovered_output_players.clone())
}

fn recover_latest_boundary_artifact(
    provider: &(impl HeaderProvider<Header = OutbeHeader> + BlockHashReader),
    last_execution_height: u64,
    dkg_rotation_params: DkgRotationParams,
) -> Result<Option<(u64, DkgBoundaryArtifact)>> {
    let max_scan = dkg_rotation_params
        .epoch_length_blocks
        .saturating_add(dkg_rotation_params.prepare_window_blocks)
        .saturating_add(dkg_rotation_params.activation_grace_blocks)
        .saturating_mul(2)
        .max(10_000);
    let min_height = last_execution_height.saturating_sub(max_scan);
    let mut height = last_execution_height;
    while height > min_height {
        let Some(header) = provider
            .sealed_header(height)
            .map_err(|error| eyre::eyre!("failed to read header {height}: {error}"))?
        else {
            height = height.saturating_sub(1);
            continue;
        };
        let artifacts = decode_outbe_block_artifacts(header.header().inner.extra_data.as_ref())
            .map_err(|error| {
                eyre::eyre!("failed to decode header artifacts at {height}: {error}")
            })?;
        if let Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) =
            artifacts.consensus_header_artifact
        {
            return Ok(Some((height, boundary)));
        }
        height = height.saturating_sub(1);
    }
    Ok(None)
}

/// Whether a scanned boundary artifact is the one a verifier-follower must
/// adopt after following a DKG rotation that activated at `activation_height`.
///
/// The activated cycle's `BoundaryOutcome` is committed by the FIRST new-epoch
/// block, i.e. strictly ABOVE the activation height (the old epoch is fenced at
/// the activation boundary). A boundary found at or below the activation height
/// is therefore the PREVIOUS cycle's artifact and must not be adopted — doing
/// so would silently keep the follower one rotation stale (its reshare
/// prev_output and round would diverge from the committee's, so a later stake
/// would end ACTIVE-but-voteless). `planned >= activation` additionally rejects
/// stale-cycle artifacts and accepts newer ones when the follower lags multiple
/// rotations: adoption is monotone, newest wins.
const fn verifier_should_adopt_followed_boundary(
    commit_height: u64,
    boundary_planned_activation_height: u64,
    activation_height: u64,
) -> bool {
    commit_height > activation_height && boundary_planned_activation_height >= activation_height
}

/// Forward-scan `[from, to]` (inclusive) for the first header carrying a
/// `BoundaryOutcome` artifact. Used by the verifier-follower's deferred
/// boundary adoption, whose search window opens just above the followed
/// rotation's activation height — the first boundary above it is the activated
/// cycle's. Headers missing from the provider end the scan (they are not yet
/// executed locally; the caller resumes from that height on the next finalized
/// height event), so each header is examined at most once across retries.
fn scan_first_boundary_after(
    provider: &(impl HeaderProvider<Header = OutbeHeader> + BlockHashReader),
    from: u64,
    to: u64,
) -> Result<ForwardBoundaryScan> {
    let mut height = from;
    while height <= to {
        let Some(header) = provider
            .sealed_header(height)
            .map_err(|error| eyre::eyre!("failed to read header {height}: {error}"))?
        else {
            return Ok(ForwardBoundaryScan::NotFound {
                resume_from: height,
            });
        };
        let artifacts = decode_outbe_block_artifacts(header.header().inner.extra_data.as_ref())
            .map_err(|error| {
                eyre::eyre!("failed to decode header artifacts at {height}: {error}")
            })?;
        if let Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) =
            artifacts.consensus_header_artifact
        {
            return Ok(ForwardBoundaryScan::Found {
                commit_height: height,
                boundary: Box::new(boundary),
            });
        }
        height = height.saturating_add(1);
    }
    Ok(ForwardBoundaryScan::NotFound {
        resume_from: height,
    })
}

#[derive(Clone, Debug)]
enum ForwardBoundaryScan {
    Found {
        commit_height: u64,
        boundary: Box<DkgBoundaryArtifact>,
    },
    NotFound {
        /// First height not yet examined (missing from the provider or above
        /// the scan ceiling).
        resume_from: u64,
    },
}

#[derive(Clone, Debug)]
struct StartupDkgSnapshot {
    last_execution_height: u64,
    last_execution_hash: B256,
    recovered_boundary: Option<(u64, DkgBoundaryArtifact)>,
    context: StartupDkgContext,
}

fn startup_threshold_material_candidate_available(args: &ConsensusArgs) -> bool {
    if args.signing_share.is_some() && args.public_polynomial.is_some() {
        return true;
    }

    let Some(keys_dir) = &args.keys_dir else {
        return false;
    };
    keys_dir.join(DKG_SHARE_FILE).exists()
        && keys_dir.join(DKG_POLYNOMIAL_FILE).exists()
        && keys_dir.join(DKG_OUTPUT_FILE).exists()
}

fn read_startup_dkg_snapshot(
    node: &OutbeFullNode,
    args: &ConsensusArgs,
    key_backend: &bls::KeyBackend,
    genesis_hash: B256,
    dkg_rotation_params: DkgRotationParams,
    last_consensus_finalized_height: u64,
) -> Result<StartupDkgSnapshot> {
    let last_execution_height = node
        .provider
        .last_block_number()
        .map_err(|e| eyre::eyre!("failed to get last block number: {e}"))?;
    let last_execution_hash = if last_execution_height > 0 {
        node.provider
            .block_hash(last_execution_height)
            .map_err(|e| {
                eyre::eyre!("failed to get block hash for height {last_execution_height}: {e}")
            })?
            .ok_or_else(|| {
                eyre::eyre!(
                    "missing block hash for execution height {last_execution_height}; refusing genesis fallback"
                )
            })?
    } else {
        genesis_hash
    };
    let boundary_recovery_height = startup_live_join_scan_height(
        last_execution_height,
        last_consensus_finalized_height,
        args.trust_el_head,
    )?;
    // NORMALIZE the tuple height to the ACTIVATION ANCHOR. The header scan
    // returns the height of the block CARRYING the boundary artifact, but that
    // artifact always rides the FIRST block of the new epoch — one block ABOVE
    // the activation height the committee anchored its rotation schedule on
    // (genesis: activation 0, committed in block 1; a reshare activated at H is
    // committed in block H+1). A restarted node that anchors on the commit
    // height runs its whole freeze/activation schedule one block LATE: it waits
    // for activation H+1 while the live committee restarts its engine at H. With
    // one such node the committee still has quorum and the laggard self-heals
    // one block later; with two of five (e.g. a restarted validator plus a
    // freshly promoted one) the new epoch is 3-of-5 < quorum and the chain
    // deadlocks at the boundary. Anchor = commit_height - 1, uniformly.
    let mut recovered_boundary = recover_latest_boundary_artifact(
        &node.provider,
        boundary_recovery_height,
        dkg_rotation_params,
    )
    .wrap_err("failed to recover latest DKG boundary artifact")?
    .map(|(commit_height, artifact)| (commit_height.saturating_sub(1), artifact));
    let mut recovered_boundary_finalized = recovered_boundary.is_some();

    if let Some(keys_dir) = args.keys_dir.as_ref().filter(|_| !args.force_dkg) {
        if let Some(snapshot) = recover_pending_dkg_boundary_snapshot(
            keys_dir,
            key_backend,
            node,
            recovered_boundary.as_ref(),
        )
        .wrap_err("failed to recover pending DKG boundary snapshot")?
        {
            info!(
                keys_dir = %keys_dir.display(),
                activated_at_height = snapshot.activated_at_height,
                dkg_cycle = snapshot.artifact.dkg_cycle,
                epoch = snapshot.artifact.epoch,
                "recovered durable pending DKG boundary snapshot"
            );
            recovered_boundary = Some((snapshot.activated_at_height, snapshot.artifact));
            recovered_boundary_finalized = false;
        }
    }

    let recovered_dkg_output_hash = recovered_boundary
        .as_ref()
        .map(|(_, artifact)| {
            decode_boundary_output(artifact).map(|output| dkg_manager::dkg_output_hash(&output))
        })
        .transpose()?;
    let context = StartupDkgContext {
        last_execution_height,
        last_consensus_finalized_height,
        recovered_boundary_finalized,
        recovered_vrf_group_public_key: recovered_boundary
            .as_ref()
            .map(|(_, artifact)| artifact.vrf_group_public_key),
        recovered_dkg_output_hash,
        genesis_formation_proven: false,
    };
    Ok(StartupDkgSnapshot {
        last_execution_height,
        last_execution_hash,
        recovered_boundary,
        context,
    })
}

async fn collect_reth_genesis_peer_evidence(node: &OutbeFullNode) -> RethGenesisPeerEvidence {
    let peers_result = node.network.get_all_peers().await;
    let (peer_query_failed, peers) = match peers_result {
        Ok(peers) => {
            let statuses = peers
                .into_iter()
                .map(|peer| RethGenesisPeerStatus {
                    genesis: peer.status.genesis,
                    blockhash: peer.status.blockhash,
                    latest_block: peer.status.latest_block,
                })
                .collect();
            (false, statuses)
        }
        Err(error) => {
            warn!(
                ?error,
                "failed to query Reth peers during genesis formation gate"
            );
            (true, Vec::new())
        }
    };

    RethGenesisPeerEvidence {
        connected_peers: node.network.num_connected_peers(),
        is_syncing: node.network.is_syncing(),
        is_initially_syncing: node.network.is_initially_syncing(),
        peer_query_failed,
        peers,
    }
}

#[allow(clippy::too_many_arguments)]
async fn resolve_startup_dkg_snapshot<E>(
    ctx: &E,
    node: &OutbeFullNode,
    args: &ConsensusArgs,
    key_backend: &bls::KeyBackend,
    signing_key: &bls12381::PrivateKey,
    validator_set: &validators::ValidatorSet,
    genesis_hash: B256,
    dkg_rotation_params: DkgRotationParams,
    last_consensus_finalized_height: u64,
) -> Result<StartupDkgSnapshot>
where
    E: Clock,
{
    let local_pk = commonware_cryptography::Signer::public_key(signing_key);
    let startup_participants: commonware_utils::ordered::Set<bls12381::PublicKey> = validator_set
        .public_keys
        .clone()
        .into_iter()
        .try_collect()
        .map_err(|e| eyre::eyre!("invalid participant set: {e}"))?;
    let local_key_in_current_consensus_set = startup_participants.position(&local_pk).is_some();
    let expected_remote_peers = validator_set.public_keys.len().saturating_sub(1);
    let required_remote_peers =
        genesis_formation_required_remote_peers(validator_set.public_keys.len());
    let gate_required = !startup_threshold_material_candidate_available(args);
    let started_at = ctx.current();

    loop {
        let mut snapshot = read_startup_dkg_snapshot(
            node,
            args,
            key_backend,
            genesis_hash,
            dkg_rotation_params,
            last_consensus_finalized_height,
        )?;
        let evidence = collect_reth_genesis_peer_evidence(node).await;
        let gate = genesis_formation_gate_decision(
            snapshot.context,
            genesis_hash,
            required_remote_peers,
            &evidence,
        );

        snapshot.context.genesis_formation_proven = gate == GenesisFormationGate::Proven;

        if !gate_required
            || startup_dkg_mode(
                snapshot.context,
                local_key_in_current_consensus_set,
                args.force_dkg,
            ) == StartupDkgMode::InitialGenesisDkg
            || gate == GenesisFormationGate::ExistingChainJoin
            || !local_key_in_current_consensus_set
        {
            info!(
                last_execution_height = snapshot.last_execution_height,
                %snapshot.last_execution_hash,
                last_consensus_finalized_height,
                recovered_dkg_boundary = snapshot.context.has_chain_finalized_dkg_boundary(),
                genesis_formation_gate = ?gate,
                local_key_in_current_consensus_set,
                gate_required,
                "resolved startup DKG state"
            );
            return Ok(snapshot);
        }

        let elapsed = elapsed_since(ctx.current(), started_at);
        if elapsed >= config::STARTUP_GENESIS_FORMATION_PROBE_TIMEOUT {
            return Err(eyre::eyre!(
                "could not prove genesis formation before DKG round 0: connected_reth_peers={} required_remote_peers={} configured_remote_peers={} reth_syncing={} reth_initial_syncing={} peer_query_failed={}",
                evidence.connected_peers,
                required_remote_peers,
                expected_remote_peers,
                evidence.is_syncing,
                evidence.is_initially_syncing,
                evidence.peer_query_failed,
            ));
        }

        info!(
            connected_reth_peers = evidence.connected_peers,
            required_remote_peers,
            expected_remote_peers,
            reth_syncing = evidence.is_syncing,
            reth_initial_syncing = evidence.is_initially_syncing,
            peer_query_failed = evidence.peer_query_failed,
            "waiting for Reth peer/sync evidence before allowing DKG round 0"
        );
        ctx.sleep(config::STARTUP_GENESIS_FORMATION_PROBE_INTERVAL)
            .await;
    }
}

fn publish_randomness_status(bridge: &ConsensusExecutionBridge, vrf_safety: &VrfSafetyGate) {
    let snapshot = vrf_safety.snapshot();
    let mut status = bridge.consensus_status();
    status.randomness_status = snapshot.randomness_status;
    status.vrf_material_version = snapshot.vrf_material_version;
    status.last_dkg_activation_height = snapshot.last_dkg_activation_height;
    status.next_planned_activation_height = snapshot.next_planned_activation_height;
    status.vrf_expiry_height = snapshot.vrf_expiry_height;
    info!(
        randomness_status = ?snapshot.randomness_status,
        vrf_material_version = snapshot.vrf_material_version,
        last_dkg_activation_height = snapshot.last_dkg_activation_height,
        next_planned_activation_height = snapshot.next_planned_activation_height,
        vrf_expiry_height = snapshot.vrf_expiry_height,
        "VRF/DKG safety status updated"
    );
    bridge.set_consensus_status(status);
}

#[derive(Clone, Debug)]
struct FrozenDkgTarget {
    dkg_cycle: u64,
    freeze_height: u64,
    planned_activation_height: u64,
    validator_set: validators::ValidatorSet,
    participants: commonware_utils::ordered::Set<bls12381::PublicKey>,
    is_validator_set_change: bool,
}

#[derive(Clone, Debug)]
struct PendingDkgActivation {
    target: FrozenDkgTarget,
    complete: dkg_actor::DkgComplete,
    completed_at_height: u64,
}

#[derive(Clone, Debug)]
struct DealerOnlyDkgActivation {
    target: FrozenDkgTarget,
    completed_at_height: u64,
}

enum ThresholdMaterial {
    Ready {
        signing_share: Share,
        polynomial: Sharing<MinSig>,
        last_dkg_output: Option<Output<MinSig, bls12381::PublicKey>>,
        bootstrap_from_live_dkg: bool,
    },
    StartupLiveJoinRequired,
    /// Verifier-join: the node has the public group polynomial + DKG output but NO
    /// threshold share. It runs the consensus engine as a VERIFIER — it follows and
    /// verifies finalized blocks (driving its execution layer to sync) but cannot
    /// propose/sign — and acquires a share at the next DKG reshare, after which the
    /// epoch loop rebuilds its scheme as a signer (Stage 4).
    VerifierOnly {
        polynomial: Sharing<MinSig>,
        last_dkg_output: Option<Output<MinSig, bls12381::PublicKey>>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupDkgMode {
    InitialGenesisDkg,
    LiveJoinRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GenesisFormationGate {
    Proven,
    WaitForExecutionSync,
    ExistingChainJoin,
}

#[derive(Clone, Copy, Debug)]
struct StartupDkgContext {
    last_execution_height: u64,
    last_consensus_finalized_height: u64,
    recovered_boundary_finalized: bool,
    recovered_vrf_group_public_key: Option<B256>,
    recovered_dkg_output_hash: Option<B256>,
    genesis_formation_proven: bool,
}

impl StartupDkgContext {
    fn has_chain_finalized_dkg_boundary(self) -> bool {
        self.recovered_vrf_group_public_key.is_some()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RethGenesisPeerStatus {
    genesis: B256,
    blockhash: B256,
    latest_block: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RethGenesisPeerEvidence {
    connected_peers: usize,
    is_syncing: bool,
    is_initially_syncing: bool,
    peer_query_failed: bool,
    peers: Vec<RethGenesisPeerStatus>,
}

fn startup_dkg_mode(
    context: StartupDkgContext,
    local_key_in_current_consensus_set: bool,
    force_dkg: bool,
) -> StartupDkgMode {
    if !local_key_in_current_consensus_set {
        return StartupDkgMode::LiveJoinRequired;
    }

    if force_dkg {
        warn!("--testnet.force-dkg: forcing fresh DKG ceremony (disaster recovery)");
        return StartupDkgMode::InitialGenesisDkg;
    }

    if context.last_execution_height == 0
        && context.last_consensus_finalized_height == 0
        && !context.has_chain_finalized_dkg_boundary()
        && context.genesis_formation_proven
    {
        StartupDkgMode::InitialGenesisDkg
    } else {
        StartupDkgMode::LiveJoinRequired
    }
}

fn genesis_formation_gate_decision(
    context: StartupDkgContext,
    genesis_hash: B256,
    required_remote_peers: usize,
    evidence: &RethGenesisPeerEvidence,
) -> GenesisFormationGate {
    if context.last_execution_height > 0
        || context.last_consensus_finalized_height > 0
        || context.has_chain_finalized_dkg_boundary()
    {
        return GenesisFormationGate::ExistingChainJoin;
    }

    if evidence.peer_query_failed {
        return GenesisFormationGate::WaitForExecutionSync;
    }

    if evidence.connected_peers < required_remote_peers {
        return GenesisFormationGate::WaitForExecutionSync;
    }

    if evidence.peers.len() < required_remote_peers {
        return GenesisFormationGate::WaitForExecutionSync;
    }

    for peer in &evidence.peers {
        if peer.genesis != genesis_hash {
            return GenesisFormationGate::ExistingChainJoin;
        }
        if peer.blockhash != genesis_hash || peer.latest_block.unwrap_or(0) > 0 {
            return GenesisFormationGate::ExistingChainJoin;
        }
    }

    GenesisFormationGate::Proven
}

/// Direct Reth connections needed to prove a fresh genesis formation before
/// entering the all-member DKG. The execution P2P graph need not be a complete
/// mesh: one local validator plus a `N-f` BFT quorum of matching genesis peers
/// is sufficient evidence. DKG itself still requires every configured genesis
/// dealer log, so lowering this transport gate cannot let a partial committee
/// complete network formation.
fn genesis_formation_required_remote_peers(validator_count: usize) -> usize {
    let max_byzantine = validator_count.saturating_sub(1) / 3;
    validator_count
        .saturating_sub(max_byzantine)
        .saturating_sub(1)
}

fn vrf_material_matches_recovered_boundary(
    polynomial: &Sharing<MinSig>,
    context: StartupDkgContext,
) -> bool {
    let local = vrf_group_public_key_hash(polynomial);
    match context.recovered_vrf_group_public_key {
        Some(expected) => local == expected,
        None => true,
    }
}

fn dkg_output_matches_recovered_boundary(
    output: &Output<MinSig, bls12381::PublicKey>,
    context: StartupDkgContext,
) -> bool {
    match context.recovered_dkg_output_hash {
        Some(expected) => dkg_manager::dkg_output_hash(output) == expected,
        None => true,
    }
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
enum DkgTaskOutcome {
    Complete(dkg_actor::DkgComplete),
    DealerOnly(dkg_actor::DkgDealerOnlyComplete),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalDkgRole {
    DealerAndPlayer,
    DealerOnly,
    PlayerOnly,
    NotParticipant,
}

fn classify_local_reshare_role(
    local: &bls12381::PublicKey,
    previous_output: Option<&Output<MinSig, bls12381::PublicKey>>,
    target_participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
) -> LocalDkgRole {
    let is_player = target_participants.position(local).is_some();
    let is_dealer = previous_output
        .map(|output| output.players().position(local).is_some())
        .unwrap_or(is_player);

    match (is_dealer, is_player) {
        (true, true) => LocalDkgRole::DealerAndPlayer,
        (true, false) => LocalDkgRole::DealerOnly,
        (false, true) => LocalDkgRole::PlayerOnly,
        (false, false) => LocalDkgRole::NotParticipant,
    }
}

enum FrozenValidatorSetRefresh {
    Ready {
        validator_set: validators::ValidatorSet,
        participants: commonware_utils::ordered::Set<bls12381::PublicKey>,
    },
    PendingBlockHash,
}

fn should_start_dkg_rotation(
    has_frozen_target: bool,
    has_pending_activation: bool,
    current_height: u64,
    freeze_height: u64,
) -> bool {
    !has_frozen_target && !has_pending_activation && current_height >= freeze_height
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingDkgActivationDecision {
    Wait,
    Activate,
    Expired { deadline: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingFreezeBlockHashDecision {
    Retry,
    Expired,
}

fn pending_freeze_block_hash_decision(
    current_height: u64,
    planned_activation_height: u64,
) -> PendingFreezeBlockHashDecision {
    if current_height >= planned_activation_height {
        PendingFreezeBlockHashDecision::Expired
    } else {
        PendingFreezeBlockHashDecision::Retry
    }
}

fn pending_dkg_activation_decision(
    current_height: u64,
    planned_activation_height: u64,
    activation_grace_blocks: u64,
) -> PendingDkgActivationDecision {
    let deadline = planned_activation_height.saturating_add(activation_grace_blocks);
    if current_height > deadline {
        PendingDkgActivationDecision::Expired { deadline }
    } else if current_height >= planned_activation_height {
        PendingDkgActivationDecision::Activate
    } else {
        PendingDkgActivationDecision::Wait
    }
}

fn provider_matches_consensus_tip(
    provider: &impl BlockHashReader,
    tip: crate::marshal_update_reporter::ConsensusTip,
    required_height: u64,
) -> Result<bool> {
    if tip.height.get() < required_height {
        return Ok(false);
    }
    let Some(provider_hash) = provider.block_hash(tip.height.get()).map_err(|error| {
        eyre::eyre!(
            "failed to read provider block hash at consensus tip height {}: {error}",
            tip.height.get()
        )
    })?
    else {
        return Ok(false);
    };
    Ok(provider_hash == tip.digest.0)
}

fn elapsed_since(now: SystemTime, since: SystemTime) -> Duration {
    match now.duration_since(since) {
        Ok(elapsed) => elapsed,
        Err(_) => Duration::ZERO,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExecutionWatchdogObservation {
    ProviderReadError,
    ProviderState {
        consensus_tip_height: u64,
        reth_head_height: u64,
        hash_match: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExecutionWatchdogDecision {
    Healthy,
    StartupGrace,
    Unhealthy { unhealthy_for: Duration },
    Fatal { unhealthy_for: Duration },
}

fn execution_watchdog_decision(
    observation: ExecutionWatchdogObservation,
    now: SystemTime,
    startup_started_at: SystemTime,
    unhealthy_since: Option<SystemTime>,
) -> (ExecutionWatchdogDecision, Option<SystemTime>) {
    let unhealthy = match observation {
        ExecutionWatchdogObservation::ProviderReadError => true,
        ExecutionWatchdogObservation::ProviderState {
            consensus_tip_height,
            reth_head_height,
            hash_match,
        } => {
            if hash_match {
                false
            } else if reth_head_height >= consensus_tip_height {
                true
            } else {
                consensus_tip_height.saturating_sub(reth_head_height)
                    > config::EXECUTION_WATCHDOG_LAG_BLOCKS
            }
        }
    };

    if !unhealthy {
        return (ExecutionWatchdogDecision::Healthy, None);
    }

    let startup_grace = Duration::from_secs(config::EXECUTION_WATCHDOG_STARTUP_GRACE_SEC);
    if elapsed_since(now, startup_started_at) < startup_grace {
        return (ExecutionWatchdogDecision::StartupGrace, None);
    }

    let since = unhealthy_since.unwrap_or(now);
    let unhealthy_for = elapsed_since(now, since);
    if unhealthy_for >= config::EXECUTION_WATCHDOG_GRACE {
        (
            ExecutionWatchdogDecision::Fatal { unhealthy_for },
            Some(since),
        )
    } else {
        (
            ExecutionWatchdogDecision::Unhealthy { unhealthy_for },
            Some(since),
        )
    }
}

fn next_consensus_epoch_after_dkg_activation(current_epoch: Epoch) -> Epoch {
    Epoch::new(current_epoch.get().saturating_add(1))
}

/// A verifier may discover a rotation only after catching up to its activation
/// block. In that case there will be no old-epoch finalized-height event after
/// `current_height` to drive the deferred epoch transition, so the current
/// height must be replayed through the local scheduler.
const fn verifier_activation_needs_immediate_replay(
    current_height: u64,
    activation_height: u64,
) -> bool {
    current_height >= activation_height
}

fn build_force_dkg_recovery_boundary(
    validator_set: &validators::ValidatorSet,
    output: &Output<MinSig, bls12381::PublicKey>,
    previous_boundary: &DkgBoundaryArtifact,
    current_head: u64,
) -> Result<(u64, DkgBoundaryArtifact)> {
    ensure!(
        current_head > 0,
        "force-DKG recovery boundary is only valid for an existing chain"
    );
    validate_dkg_output_players_exact(output, validator_set)
        .wrap_err("force-DKG output does not cover the current active validator set")?;

    let dkg_cycle = previous_boundary
        .dkg_cycle
        .checked_add(1)
        .ok_or_else(|| eyre::eyre!("force-DKG dkg_cycle overflow"))?;
    let planned_activation_height = current_head
        .checked_add(1)
        .ok_or_else(|| eyre::eyre!("force-DKG planned activation height overflow"))?;
    let vrf_material_version =
        outbe_validatorset::next_vrf_material_version(previous_boundary.vrf_material_version)?;
    let recovery_epoch =
        next_consensus_epoch_after_dkg_activation(Epoch::new(previous_boundary.epoch));

    let activated_validator_set = validator_set_for_dkg_output_players(output, validator_set)?;
    let current_participants = participants_from_validator_set(validator_set)?;
    let activated_participants = participants_from_validator_set(&activated_validator_set)?;
    let is_validator_set_change = current_participants != activated_participants;

    let artifact = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: recovery_epoch,
        validator_set: &activated_validator_set,
        output,
        is_full_dkg: true,
        dkg_cycle,
        freeze_height: current_head,
        planned_activation_height,
        vrf_material_version,
        is_validator_set_change,
        tee_reshare_registrations: Vec::new(),
    })?;

    Ok((planned_activation_height, artifact))
}

fn genesis_hash(node: &OutbeFullNode) -> Result<B256> {
    let hash = node
        .provider
        .block_hash(0)
        .map_err(|e| eyre::eyre!("failed to get genesis hash: {e}"))?;
    require_genesis_hash(hash)
}

fn require_genesis_hash(hash: Option<B256>) -> Result<B256> {
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
fn genesis_consensus_block(node: &OutbeFullNode) -> Result<outbe_consensus::block::ConsensusBlock> {
    let recovered = node
        .provider
        .recovered_block(0u64.into(), TransactionVariant::NoHash)
        .map_err(|e| eyre::eyre!("failed to read genesis block from provider: {e}"))?
        .ok_or_else(|| eyre::eyre!("missing genesis block (height 0) from provider"))?;
    Ok(outbe_consensus::block::ConsensusBlock::from_sealed(
        recovered.into_sealed_block(),
    ))
}

/// Spawn the drainer that answers `outbe_getFinalization` RPC requests from the
/// marshal. The `outbe-rpc` handler cannot see the marshal or `ConsensusBlock`,
/// so it requests bytes through [`ConsensusExecutionBridge::request_finalization`];
/// this task is the consensus-side responder. Wired on BOTH the validator path
/// (`run_consensus_stack`) and the certified-follower path (a follower can serve
/// upstream too), right after `marshal_mailbox` exists.
///
/// For each `(height, reply)` it reads the finalization certificate and the
/// finalized block from the marshal, encodes both with `commonware_codec`, and
/// answers `Some` only when both are present locally (otherwise `None`, which
/// the RPC maps to a "not available" error).
fn spawn_finalization_drainer<E>(
    ctx: &E,
    marshal_mailbox: outbe_consensus::marshal_types::MarshalMailbox,
    bridge: ConsensusExecutionBridge,
) where
    E: Spawner + Metrics,
{
    let rx = bridge.set_finalization_fetcher();
    ctx.child("finalization_drainer")
        .spawn(move |_| async move {
            let mut rx = rx;
            while let Some((height, reply)) = rx.recv().await {
                let answer =
                    finalization_bytes_for_height(&marshal_mailbox, Height::new(height)).await;
                // The receiver may have gone away (RPC client disconnected); ignore.
                let _ = reply.send(answer);
            }
        });
}

/// Read `(finalization, block)` for `height` from the marshal and encode them
/// for transport. `None` if either is missing locally.
async fn finalization_bytes_for_height(
    marshal_mailbox: &outbe_consensus::marshal_types::MarshalMailbox,
    height: Height,
) -> Option<outbe_primitives::consensus::FinalizedBlockBytes> {
    use commonware_codec::Encode as _;

    let finalization = marshal_mailbox.get_finalization(height).await?;
    // The block is keyed by the finalization's payload digest.
    let block = marshal_mailbox
        .get_block(&finalization.proposal.payload)
        .await?;
    Some(outbe_primitives::consensus::FinalizedBlockBytes {
        finalization: alloy_primitives::Bytes::from(finalization.encode().to_vec()),
        block: alloy_primitives::Bytes::from(block.encode().to_vec()),
    })
}

fn nonzero_u16(value: u16, name: &str) -> Result<NonZeroU16> {
    NonZeroU16::new(value).ok_or_else(|| eyre::eyre!("{name} must be > 0"))
}

fn nonzero_usize(value: usize, name: &str) -> Result<NonZeroUsize> {
    NonZeroUsize::new(value).ok_or_else(|| eyre::eyre!("{name} must be > 0"))
}

fn nonzero_u64(value: u64, name: &str) -> Result<NonZeroU64> {
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

fn parse_consensus_peers(entries: &[String]) -> Result<BTreeMap<Vec<u8>, SocketAddr>> {
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

fn ordered_validator_addresses(
    participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
    validator_set: &validators::ValidatorSet,
) -> Result<Vec<alloy_primitives::Address>> {
    ensure!(
        validator_set.public_keys.len() == validator_set.addresses.len(),
        "validator set has mismatched public key/address lengths: {} public keys, {} addresses",
        validator_set.public_keys.len(),
        validator_set.addresses.len(),
    );

    let mut ordered = Vec::with_capacity(participants.len());
    for pk in participants.iter() {
        let Some(idx) = validator_set.public_keys.iter().position(|p| p == pk) else {
            return Err(eyre::eyre!(
                "participant public key is missing from validator set"
            ));
        };
        ordered.push(validator_set.addresses[idx]);
    }
    Ok(ordered)
}

fn active_set_hash_from_addresses(addresses: &[EthAddress]) -> B256 {
    let mut bytes = Vec::with_capacity(8 + addresses.len() * 20);
    bytes.extend_from_slice(&(addresses.len() as u64).to_be_bytes());
    for address in addresses {
        bytes.extend_from_slice(address.as_slice());
    }
    alloy_primitives::keccak256(bytes)
}

/// Recover the participant-index-aligned EVM address vector from a finalized DKG
/// boundary instead of provider-latest validator state.
///
/// This is the recovery/live-join counterpart of [`ordered_validator_addresses`].
/// `build_boundary_artifact` constructs `reshare.new_active_set` by iterating
/// `output.players()` in Commonware participant order, so the boundary itself is
/// the canonical source of the old epoch's address mapping. That matters on
/// restart when Reth head may have executed an unfinalized membership-changing
/// `BoundaryOutcome` while marshal-finalized consensus still needs the old
/// committee.
fn ordered_addresses_from_recovered_boundary(
    participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
    boundary: &DkgBoundaryArtifact,
) -> Result<Vec<EthAddress>> {
    let boundary_output = decode_boundary_output(boundary)
        .wrap_err("failed to decode recovered DKG boundary output for address mapping")?;
    ensure!(
        boundary_output.players() == participants,
        "recovered DKG boundary output players do not match active participant set"
    );

    let ordered_addresses = boundary.reshare.new_active_set.clone();
    ensure!(
        ordered_addresses.len() == participants.len(),
        "recovered DKG boundary active-set length {} does not match participant count {}",
        ordered_addresses.len(),
        participants.len(),
    );
    ensure!(
        active_set_hash_from_addresses(&ordered_addresses) == boundary.reshare.active_set_hash,
        "recovered DKG boundary active-set hash does not match active-set addresses"
    );
    ensure!(
        alloy_primitives::keccak256(boundary.vrf_group_public_key_bytes.as_ref())
            == boundary.vrf_group_public_key,
        "recovered DKG boundary VRF group public key bytes do not match hash"
    );

    let mut committee = Vec::with_capacity(participants.len());
    for (address, bls_pk) in ordered_addresses.iter().zip(participants.iter()) {
        let encoded = commonware_codec::Encode::encode(bls_pk).to_vec();
        let consensus_pubkey: [u8; 48] = encoded.as_slice().try_into().map_err(|_| {
            eyre::eyre!(
                "encoded MinPk consensus pubkey has unexpected length: expected 48, got {}",
                encoded.len()
            )
        })?;
        committee.push(outbe_consensus::proof::CommitteeEntry {
            address: *address,
            consensus_pubkey,
        });
    }
    let snapshot = outbe_consensus::proof::CommitteeSnapshot {
        committee,
        vrf_material_version: boundary.vrf_material_version,
        vrf_group_public_key_bytes: boundary.vrf_group_public_key_bytes.to_vec(),
        vrf_public_polynomial_hash: dkg_manager::public_polynomial_hash(boundary_output.public()),
    };
    ensure!(
        outbe_consensus::proof::committee_set_hash_v2(boundary.epoch, &snapshot)
            == boundary.committee_set_hash,
        "recovered DKG boundary committee_set_hash does not match boundary committee/address mapping"
    );

    Ok(ordered_addresses)
}

/// Load the validator EVM signer and the committee address set for the one-time
/// TEE bootstrap coordination.
fn tee_bootstrap_setup(
    args: &ConsensusArgs,
    participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
    validator_set: &validators::ValidatorSet,
) -> Result<(
    outbe_primitives::signer::OutbeEvmSigner,
    std::collections::BTreeSet<alloy_primitives::Address>,
)> {
    let evm_key_path = args
        .effective_validator_evm_key()?
        .ok_or_else(|| eyre::eyre!("TEE bootstrap requires a validator EVM key"))?;
    let evm_signer = outbe_primitives::signer::OutbeEvmSigner::from_file(&evm_key_path)
        .map_err(|e| eyre::eyre!("failed to load validator EVM signer for TEE bootstrap: {e}"))?;
    let committee: std::collections::BTreeSet<alloy_primitives::Address> =
        ordered_validator_addresses(participants, validator_set)?
            .into_iter()
            .collect();
    Ok((evm_signer, committee))
}

fn epoch_validation_inputs(
    epoch: Epoch,
    participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
    validator_set: &validators::ValidatorSet,
    recovered_boundary: Option<&DkgBoundaryArtifact>,
    vrf_materials: &VrfMaterialProvider<MinSig>,
) -> Result<(HybridScheme<MinSig>, Vec<alloy_primitives::Address>)> {
    let verifier_scheme = HybridScheme::<MinSig>::verifier_with_vrf_provider(
        &config::outbe_app_namespace(),
        participants.clone(),
        vrf_materials.clone(),
    )
    .ok_or_else(|| {
        eyre::eyre!("failed to build verifier scheme for validator set (epoch {epoch})")
    })?;
    // Simplex participant indices follow ordered::Set pubkey ordering, not the
    // original validator_set order; certificate signer bitmaps use this order.
    // On restart/live-join with a recovered DKG boundary, provider-latest state
    // may include an unfinalized membership-changing head. Use the boundary's
    // own participant-index-aligned address vector for that recovered epoch.
    let ordered_addresses = match recovered_boundary {
        Some(boundary) => ordered_addresses_from_recovered_boundary(participants, boundary)?,
        None => ordered_validator_addresses(participants, validator_set)?,
    };
    Ok((verifier_scheme, ordered_addresses))
}

fn register_epoch_validation_providers(
    epoch: Epoch,
    participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
    validator_set: &validators::ValidatorSet,
    recovered_boundary: Option<&DkgBoundaryArtifact>,
    vrf_materials: &VrfMaterialProvider<MinSig>,
    certificate_scheme_provider: &HybridSchemeProvider<MinSig>,
    committee_provider: &CommitteeProvider,
) -> Result<()> {
    let (verifier_scheme, ordered_addresses) = epoch_validation_inputs(
        epoch,
        participants,
        validator_set,
        recovered_boundary,
        vrf_materials,
    )?;
    let _ = certificate_scheme_provider.register(epoch, verifier_scheme);
    let _ = committee_provider.register(epoch, ordered_addresses);
    Ok(())
}

fn validate_validator_evm_signer(
    args: &ConsensusArgs,
    signing_key: &bls12381::PrivateKey,
    consensus_validator_set: &validators::ValidatorSet,
    active_validator_set: &validators::ValidatorSet,
    recovered_committee: Option<(
        &commonware_utils::ordered::Set<bls12381::PublicKey>,
        &DkgBoundaryArtifact,
    )>,
    verifier_join: bool,
) -> Result<Option<EthAddress>> {
    let Some(evm_key_path) = args.effective_validator_evm_key()? else {
        return Ok(None);
    };
    let signer =
        outbe_primitives::signer::OutbeEvmSigner::from_file(&evm_key_path).wrap_err_with(|| {
            format!(
                "failed to load validator EVM key from {}",
                evm_key_path.display()
            )
        })?;
    let signer_address = signer.address();

    if let Some((participants, boundary)) = recovered_committee {
        let ordered_addresses =
            ordered_addresses_from_recovered_boundary(participants, boundary)
                .wrap_err("failed to validate recovered DKG boundary committee for EVM signer")?;
        let local_public_key = signing_key.public_key();
        let Some(participant_index) = participants.position(&local_public_key) else {
            if verifier_join {
                info!(
                    %signer_address,
                    epoch = boundary.epoch,
                    "verifier-join: local BLS key is not in the recovered DKG boundary committee; \
                     the node syncs as a verifier"
                );
                return Ok(None);
            }
            eyre::bail!(
                "local BLS key is not in recovered DKG boundary committee for epoch {}; \
                 refusing latest-state EVM signer authorization",
                boundary.epoch
            );
        };
        let expected_address = ordered_addresses.get(participant_index).ok_or_else(|| {
            eyre::eyre!(
                "recovered DKG boundary address mapping missing participant index {}",
                participant_index
            )
        })?;
        ensure!(
            *expected_address == signer_address,
            "validator EVM key address {} does not match recovered DKG boundary address {} \
             for local BLS consensus key",
            signer_address,
            expected_address
        );
        info!(
            address = %signer_address,
            epoch = boundary.epoch,
            "validated validator EVM signer against recovered DKG boundary"
        );
        return Ok(Some(signer_address));
    }

    let authorized = consensus_validator_set
        .addresses
        .iter()
        .position(|address| *address == signer_address)
        .map(|index| {
            (
                &consensus_validator_set.public_keys,
                index,
                "active consensus participant set",
            )
        })
        .or_else(|| {
            active_validator_set
                .addresses
                .iter()
                .position(|address| *address == signer_address)
                .map(|index| {
                    (
                        &active_validator_set.public_keys,
                        index,
                        "active validator set",
                    )
                })
        });
    let Some((public_keys, index, source_set)) = authorized else {
        if verifier_join {
            info!(
                %signer_address,
                "verifier-join: EVM signer is not yet in the on-chain validator set; the node \
                 syncs as a verifier and resolves its proposer address once a reshare grants \
                 it a share"
            );
            return Ok(None);
        }
        eyre::bail!(
            "validator EVM key address {} is neither in the active consensus participant set nor the active validator set",
            signer_address
        );
    };

    let local_public_key = signing_key.public_key();
    let Some(registered_public_key) = public_keys.get(index) else {
        eyre::bail!(
            "validator set missing BLS public key for EVM address {}",
            signer_address
        );
    };
    ensure!(
        registered_public_key == &local_public_key,
        "validator EVM key address {} belongs to a different BLS consensus key",
        signer_address
    );
    let address = signer_address;
    info!(
        address = %address,
        source_set,
        "validated validator EVM signer"
    );
    Ok(Some(address))
}

/// Run the consensus stack.
///
/// Wires together:
/// 1. Validator configuration (static JSON or dynamic from EVM state)
/// 2. HybridScheme signing (BLS individual + BLS12-381 threshold VRF)
/// 3. P2P network channels (lookup::Network) with Muxers for epoch-scoped sub-channels
/// 4. Application handler (propose/verify via beacon engine)
/// 5. Executor actor (FCU updates, finalization)
/// 6. Simplex consensus engine (restarted on reshare)
/// 7. Block propagation — proposer broadcasts full blocks via P2P channel
/// 8. Automatic reshare detection and DKG execution
///
/// Follower stack: cold-sync finalized blocks from an upstream node, verify them
/// against the trusted network identity (committee-chaining — see the `follow`
/// module), and drive the EL via the existing executor, WITHOUT running the
/// consensus engine. Selected by `--upstream`.
#[allow(clippy::too_many_arguments)]
async fn run_follow_stack<E>(
    ctx: &E,
    args: ConsensusArgs,
    node: OutbeFullNode,
    bridge: ConsensusExecutionBridge,
    upstream: String,
    projection_readiness: ProjectionReadinessHandle,
    finalized_ce_committer: Arc<dyn FinalizedCeCommitter>,
    ce_startup_recovery: Arc<dyn CeStartupRecovery>,
) -> Result<()>
where
    E: BufferPooler
        + Clock
        + CryptoRngCore
        + Network
        + Resolver
        + Spawner
        + Storage
        + Metrics
        + Send
        + Sync
        + 'static,
{
    let epoch_length = epoch_length_blocks_from_genesis(&node)?;

    if args.upstream_nocertify {
        return Err(eyre::eyre!(
            "--upstream.nocertify (uncertified dev sync) is not yet implemented"
        ));
    }

    // Trust anchor: the genesis validator committee (the MinPk consensus key
    // set), read from the follower's OWN genesis state. Consensus finality is a
    // multisig over these keys, so this set — not the VRF group key — is the
    // trust root, and it is already in genesis (the operator provides nothing).
    let genesis_validators = validators::read_consensus_validators_at_latest(&node.provider)
        .wrap_err("failed to read genesis validator set for the follower trust anchor")?;
    let anchor_participants: commonware_utils::ordered::Set<bls12381::PublicKey> =
        genesis_validators
            .public_keys
            .iter()
            .cloned()
            .try_collect()
            .map_err(|e| {
                eyre::eyre!("genesis validator set is not a valid participant set: {e:?}")
            })?;

    // TEE-chain guardrail. Full execution re-runs offer txs (decrypt in the
    // enclave) and enclave-registration txs (`registerEnclave` seals the
    // resident offer key to the joiner and emits `OfferKeySealed`) — BOTH route
    // through the local enclave, and BOTH land in the block's receipts root. A
    // follower without an enclave silently omits the seal / cannot decrypt, so
    // its receipts root diverges and reth rejects the block (a cryptic
    // "receipt root mismatch" at the first such height). Fail fast here with an
    // actionable message instead. The offer key is a lifetime constant, so it is
    // installed ONCE, before sync, via `outbe-cli tee join`. The TEE-chain probe
    // reads the UPSTREAM's state, not the follower's: the follower is at genesis,
    // where the bootstrap tx that sets the offer key has not run yet.
    let tee_probe = crate::follow_transport::UpstreamRpcClient::new(&upstream)?;
    let tee_offer_public = tee_probe
        .tribute_offer_public_key()
        .await
        .wrap_err("failed to probe the upstream for TEE-chain status (follower prerequisites)")?;
    if !tee_offer_public.is_zero() && args.tee_enclave_socket.is_none() {
        return Err(eyre::eyre!(
            "this is a TEE chain (a tribute offer key is set on-chain) but the follower was \
             started WITHOUT --tee-enclave-socket. A full-execution follower re-runs offer and \
             enclave-registration transactions through the enclave, so it needs an attested \
             enclave that holds the (lifetime-constant) offer key. Install the key once, before \
             syncing:\n  1. start the enclave sidecar,\n  2. `outbe-cli tee join --enclave-socket \
             <socket> --rpc-url {upstream}` (registers + waits for the sealed offer key),\n  3. \
             start the node with `--tee-enclave-socket <socket> --upstream {upstream}`."
        ));
    }

    info!(
        %upstream,
        anchor_validators = anchor_participants.len(),
        epoch_length,
        "follower mode (--upstream) selected; anchored on the genesis validator set"
    );

    run_certified_follow_stack(
        ctx,
        anchor_participants,
        node,
        bridge,
        upstream,
        epoch_length,
        projection_readiness,
        finalized_ce_committer,
        ce_startup_recovery,
    )
    .await
}

/// The committee-chaining follower engine (transport A — upstream RPC, no
/// consensus P2P). Builds the same marshal + executor as the validator path,
/// feeds the marshal finalized blocks fetched from the upstream, and verifies
/// each against the per-epoch committee derived from the trusted anchor.
#[allow(clippy::too_many_arguments)]
async fn run_certified_follow_stack<E>(
    ctx: &E,
    anchor_participants: commonware_utils::ordered::Set<bls12381::PublicKey>,
    node: OutbeFullNode,
    bridge: ConsensusExecutionBridge,
    upstream: String,
    epoch_length_blocks: u32,
    projection_readiness: ProjectionReadinessHandle,
    finalized_ce_committer: Arc<dyn FinalizedCeCommitter>,
    ce_startup_recovery: Arc<dyn CeStartupRecovery>,
) -> Result<()>
where
    E: BufferPooler
        + Clock
        + CryptoRngCore
        + Network
        + Resolver
        + Spawner
        + Storage
        + Metrics
        + Send
        + Sync
        + 'static,
{
    use commonware_consensus::marshal;
    use commonware_cryptography::certificate::Scheme as _;
    use commonware_storage::archive::immutable;
    use outbe_consensus::follow::{run_follow_engine, CommitteeChain, FollowEngineConfig};
    use outbe_consensus::hybrid::{HybridScheme, HybridSchemeProvider};
    use std::sync::{Arc, Mutex};

    // ── 0. Startup chain-state sources ───────────────────────────────────
    let genesis_hash = genesis_hash(&node)?;
    let last_execution_height = node
        .provider
        .last_block_number()
        .map_err(|e| eyre::eyre!("failed to get last block number: {e}"))?;
    let last_execution_hash = if last_execution_height > 0 {
        node.provider
            .block_hash(last_execution_height)
            .map_err(|e| {
                eyre::eyre!("failed to get block hash for height {last_execution_height}: {e}")
            })?
            .ok_or_else(|| {
                eyre::eyre!("missing block hash for execution height {last_execution_height}")
            })?
    } else {
        genesis_hash
    };

    // ── 1. Committee chain anchored on the trusted identity ──────────────
    // The marshal verifies finalization certs against THIS chain's per-epoch
    // verifier provider, so the provider clone we hand the marshal must share
    // state with the chain (HybridSchemeProvider is Arc-backed; `register`
    // through a clone is visible everywhere).
    // Genesis anchor: epoch 0, the genesis validator committee.
    let chain = CommitteeChain::new(Epoch::new(0), anchor_participants);
    let certificate_scheme_provider: HybridSchemeProvider<MinSig> = chain.scheme_provider().clone();
    let anchor_epoch = Epoch::new(chain.anchor_epoch());
    let chain = Arc::new(Mutex::new(chain));

    // ── 2. Page cache + marshal archives (mirrors run_consensus_stack) ───
    let page_cache = CacheRef::from_pooler(
        ctx,
        nonzero_u16(4096, "page cache page size")?,
        nonzero_usize(config::PAGE_CACHE_SIZE / 4096, "PAGE_CACHE_SIZE / 4096")?,
    );

    let partition_prefix = "outbe-marshal".to_string();

    let finalizations_archive = immutable::Archive::init(
        ctx.child("marshal_finalizations"),
        immutable::Config {
            metadata_partition: format!("{partition_prefix}-finalizations-metadata"),
            freezer_table_partition: format!("{partition_prefix}-finalizations-freezer-table"),
            freezer_table_initial_size: config::FREEZER_TABLE_INITIAL_SIZE,
            freezer_table_resize_frequency: config::FREEZER_TABLE_RESIZE_FREQUENCY,
            freezer_table_resize_chunk_size: config::FREEZER_TABLE_RESIZE_CHUNK_SIZE,
            freezer_key_partition: format!("{partition_prefix}-finalizations-freezer-key"),
            freezer_key_page_cache: page_cache.clone(),
            freezer_value_partition: format!("{partition_prefix}-finalizations-freezer-value"),
            freezer_value_target_size: config::FREEZER_VALUE_TARGET_SIZE,
            freezer_value_compression: config::FREEZER_VALUE_COMPRESSION,
            ordinal_partition: format!("{partition_prefix}-finalizations-ordinal"),
            items_per_section: nonzero_u64(
                config::IMMUTABLE_ITEMS_PER_SECTION,
                "IMMUTABLE_ITEMS_PER_SECTION",
            )?,
            codec_config: HybridScheme::<MinSig>::certificate_codec_config_unbounded(),
            replay_buffer: nonzero_usize(config::MARSHAL_REPLAY_BUFFER, "MARSHAL_REPLAY_BUFFER")?,
            freezer_key_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
            freezer_value_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
            ordinal_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
        },
    )
    .await
    .wrap_err("failed to initialize finalizations archive")?;

    let blocks_archive = immutable::Archive::init(
        ctx.child("marshal_blocks"),
        immutable::Config {
            metadata_partition: format!("{partition_prefix}-blocks-metadata"),
            freezer_table_partition: format!("{partition_prefix}-blocks-freezer-table"),
            freezer_table_initial_size: config::FREEZER_TABLE_INITIAL_SIZE,
            freezer_table_resize_frequency: config::FREEZER_TABLE_RESIZE_FREQUENCY,
            freezer_table_resize_chunk_size: config::FREEZER_TABLE_RESIZE_CHUNK_SIZE,
            freezer_key_partition: format!("{partition_prefix}-blocks-freezer-key"),
            freezer_key_page_cache: page_cache.clone(),
            freezer_value_partition: format!("{partition_prefix}-blocks-freezer-value"),
            freezer_value_target_size: config::FREEZER_VALUE_TARGET_SIZE,
            freezer_value_compression: config::FREEZER_VALUE_COMPRESSION,
            ordinal_partition: format!("{partition_prefix}-blocks-ordinal"),
            items_per_section: nonzero_u64(
                config::IMMUTABLE_ITEMS_PER_SECTION,
                "IMMUTABLE_ITEMS_PER_SECTION",
            )?,
            codec_config: (),
            replay_buffer: nonzero_usize(config::MARSHAL_REPLAY_BUFFER, "MARSHAL_REPLAY_BUFFER")?,
            freezer_key_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
            freezer_value_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
            ordinal_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
        },
    )
    .await
    .wrap_err("failed to initialize blocks archive")?;

    // The follower marshal uses the boundary-aligned `FollowerEpocher`, whose
    // epoch boundaries match outbe's on-chain committee epochs (`[E·L+1,
    // (E+1)·L]`). The validator's `FixedEpocher` disagrees by one block at every
    // multiple of L, which would stall a resolver-only follower at boundary
    // blocks (see outbe_consensus::follow::epocher).
    let epocher = outbe_consensus::follow::FollowerEpocher::new(u64::from(epoch_length_blocks));
    let view_retention_timeout = u64::from(config::ACTIVITY_TIMEOUT)
        .checked_mul(config::VIEW_RETENTION_MULTIPLIER)
        .ok_or_else(|| eyre::eyre!("view retention timeout overflow"))?;

    let marshal_genesis_anchor = genesis_consensus_block(&node)?;
    let (marshal_actor, marshal_mailbox, last_consensus_finalized_opt) =
        marshal::core::Actor::init(
            ctx.child("marshal"),
            finalizations_archive,
            blocks_archive,
            marshal::Config {
                provider: certificate_scheme_provider,
                epocher: epocher.clone(),
                start: marshal::Start::Genesis(marshal_genesis_anchor),
                partition_prefix: partition_prefix.clone(),
                mailbox_size: nonzero_usize(config::ENGINE_MAILBOX_SIZE, "ENGINE_MAILBOX_SIZE")?,
                view_retention_timeout: ViewDelta::new(view_retention_timeout),
                prunable_items_per_section: nonzero_u64(
                    config::PRUNABLE_ITEMS_PER_SECTION,
                    "PRUNABLE_ITEMS_PER_SECTION",
                )?,
                page_cache: page_cache.clone(),
                replay_buffer: nonzero_usize(
                    config::MARSHAL_REPLAY_BUFFER,
                    "MARSHAL_REPLAY_BUFFER",
                )?,
                key_write_buffer: nonzero_usize(
                    config::MARSHAL_WRITE_BUFFER,
                    "MARSHAL_WRITE_BUFFER",
                )?,
                value_write_buffer: nonzero_usize(
                    config::MARSHAL_WRITE_BUFFER,
                    "MARSHAL_WRITE_BUFFER",
                )?,
                block_codec_config: (),
                max_repair: nonzero_usize(config::MAX_REPAIR, "MAX_REPAIR")?,
                max_pending_acks: nonzero_usize(config::MAX_PENDING_ACKS, "MAX_PENDING_ACKS")?,
                strategy: commonware_parallel::Sequential,
            },
        )
        .await;
    let last_consensus_finalized = map_marshal_init_height(last_consensus_finalized_opt);
    let recovered_ce_marker = ce_startup_recovery
        .recover_before_participation(last_consensus_finalized.get())
        .wrap_err("compressed-tree startup recovery failed before follower participation")?;
    info!(
        last_consensus_finalized = last_consensus_finalized.get(),
        ce_marker_height = recovered_ce_marker.height,
        last_execution_height,
        "follower marshal initialized"
    );

    // ── 3. Transports ────────────────────────────────────────────────────
    let local = crate::follow_transport::RethLocalBlockSource::new(node.clone());
    let upstream_client = crate::follow_transport::UpstreamRpcClient::new(&upstream)?;
    // Separate cheap client handle for tip discovery (engine takes the
    // `FinalizedSource` and `TipSource` as distinct values).
    let tip_client = crate::follow_transport::UpstreamRpcClient::new(&upstream)?;

    // ── 4. Executor (REUSED verbatim) — drives the EL via FCU+newPayload ──
    let engine_handle: EngineHandle = node.add_ons_handle.beacon_engine_handle.clone();
    let (execution_finalized_height_tx, mut execution_finalized_height_rx) =
        tokio::sync::mpsc::unbounded_channel::<u64>();
    let (executor_actor, executor_mailbox) = ExecutorActor::new(
        ctx.child("executor"),
        engine_handle,
        genesis_hash,
        last_execution_height,
        last_execution_hash,
        projection_readiness,
        Some(execution_finalized_height_tx),
    );
    let _executor_handle = executor_actor
        .with_finalized_ce_committer(finalized_ce_committer)
        .start(marshal_mailbox.clone(), last_consensus_finalized);

    // ── 4b. Serve `outbe_getFinalization` + publish this follower's finalized
    //        height into the bridge (`outbe_consensusStatus.lastFinalizedBlock`),
    //        so this follower can itself be an UPSTREAM for other followers
    //        (their tip discovery polls that status field). ────────────────
    spawn_finalization_drainer(ctx, marshal_mailbox.clone(), bridge.clone());
    ctx.child("follower_tip_publisher")
        .spawn(move |_| async move {
            while let Some(height) = execution_finalized_height_rx.recv().await {
                bridge.set_last_finalized_block_number(height);
            }
        });

    // ── 5. Assemble + run the follower engine ────────────────────────────
    run_follow_engine(
        ctx.child("follow_engine"),
        FollowEngineConfig {
            marshal_actor,
            marshal_mailbox,
            executor_reporter: crate::marshal_update_reporter::MarshalUpdateReporter::new(
                executor_mailbox,
            ),
            upstream: upstream_client,
            local,
            tip: tip_client,
            epocher,
            chain,
            anchor_epoch,
            mailbox_size: nonzero_usize(config::ENGINE_MAILBOX_SIZE, "ENGINE_MAILBOX_SIZE")?,
        },
    )
    .await
}

pub async fn run_consensus_stack<E>(
    ctx: &E,
    args: ConsensusArgs,
    node: OutbeFullNode,
    bridge: ConsensusExecutionBridge,
    projection_readiness: ProjectionReadinessHandle,
    finalized_ce_committer: Arc<dyn FinalizedCeCommitter>,
    ce_startup_recovery: Arc<dyn CeStartupRecovery>,
) -> Result<()>
where
    E: BufferPooler
        + Clock
        + CryptoRngCore
        + Network
        + Resolver
        + Spawner
        + Storage
        + Metrics
        + Send
        + Sync
        + 'static,
{
    // Follower mode: cold-sync finalized blocks from an upstream node and verify
    // them against the trusted network identity, WITHOUT running the consensus
    // engine. Short-circuits before any validator material is loaded.
    if let Some(upstream) = args.upstream.clone() {
        return run_follow_stack(
            ctx,
            args,
            node,
            bridge,
            upstream,
            projection_readiness,
            finalized_ce_committer,
            ce_startup_recovery,
        )
        .await;
    }

    // ── 0. Validate testnet-only disaster-recovery flags ─────────────────
    let chain_id = node.chain_spec().chain().id();
    if (args.trust_el_head || args.force_dkg) && outbe_primitives::chain::is_mainnet(chain_id) {
        return Err(eyre::eyre!(
            "--testnet.trust-el-head and --testnet.force-dkg are not allowed on mainnet (chain_id {chain_id})"
        ));
    }
    if args.force_dkg && !args.trust_el_head {
        return Err(eyre::eyre!(
            "--testnet.force-dkg requires --testnet.trust-el-head"
        ));
    }

    // ── 1. Load signing key ─────────────────────────────────────────────
    let signing_key_path = args
        .signing_key
        .as_ref()
        .ok_or_else(|| eyre::eyre!("--consensus.signing-key is required"))?;
    let key_backend = args.key_backend().wrap_err("invalid BLS key backend")?;
    let signing_key = validators::load_signing_key(signing_key_path, &key_backend)
        .wrap_err("failed to load signing key")?;

    // ── 2. Load validator set ───────────────────────────────────────────
    // Chain state is the only runtime source of validator membership. For a
    // fresh network this is the genesis ValidatorSet storage; for restart/join
    // this is the synced canonical state.
    let mut validator_set = validators::read_consensus_validators_at_latest(&node.provider)
        .wrap_err("failed to load consensus validator set at startup")?;

    info!(
        count = validator_set.public_keys.len(),
        "loaded validator set"
    );

    // ── 3. Set up P2P network ───────────────────────────────────────────
    let p2p_namespace = commonware_utils::union_unique(&config::outbe_app_namespace(), b"_P2P");
    let network_cfg = if args.use_local_defaults {
        lookup::Config::local(
            signing_key.clone(),
            &p2p_namespace,
            args.listen_address,
            config::MAX_P2P_MESSAGE_SIZE,
        )
    } else {
        lookup::Config::recommended(
            signing_key.clone(),
            &p2p_namespace,
            args.listen_address,
            config::MAX_P2P_MESSAGE_SIZE,
        )
    };

    let (mut network, mut oracle) = lookup::Network::new(ctx.child("network"), network_cfg);

    // Register Simplex consensus channels (will be wrapped in Muxers).
    let votes = network.register(
        config::VOTES_CHANNEL,
        Quota::per_second(NZU32!(128)),
        config::CHANNEL_BACKLOG,
    );
    let certificates = network.register(
        config::CERTIFICATES_CHANNEL,
        Quota::per_second(NZU32!(128)),
        config::CHANNEL_BACKLOG,
    );
    let resolver = network.register(
        config::RESOLVER_CHANNEL,
        Quota::per_second(NZU32!(64)),
        config::CHANNEL_BACKLOG,
    );

    // Register broadcast channel for block dissemination (buffered engine).
    let broadcast_channel = network.register(
        config::BROADCAST_CHANNEL,
        Quota::per_second(NZU32!(32)),
        config::CHANNEL_BACKLOG,
    );

    // Register marshal resolver channel for on-demand block backfill.
    let marshal_channel = network.register(
        config::MARSHAL_CHANNEL,
        Quota::per_second(NZU32!(64)),
        config::CHANNEL_BACKLOG,
    );

    // Register DKG ceremony channel (muxed by reshare round).
    let dkg_channel = network.register(
        config::DKG_CHANNEL,
        Quota::per_second(NZU32!(128)),
        config::CHANNEL_BACKLOG,
    );

    // Register the one-time TEE bootstrap channel (only when a TEE enclave
    // sidecar is configured). Used once at startup, like the DKG, to coordinate
    // the committee's enclave registrations + EVM signatures into the block-1
    // `TeeBootstrap` payload. Registered before `network.start()`.
    let mut tee_bootstrap_channel = args.tee_enclave_socket.as_ref().map(|_| {
        network.register(
            config::TEE_BOOTSTRAP_CHANNEL,
            Quota::per_second(NZU32!(64)),
            config::CHANNEL_BACKLOG,
        )
    });

    // Register the one-time TEE DKG channel (only when a TEE enclave sidecar is
    // configured). Carries the enclave identity exchange + dealer/player gossip +
    // offer-key partial-signature round that derives the shared tribute offer key
    // at startup. Registered before `network.start()`.
    let mut tee_dkg_channel = args.tee_enclave_socket.as_ref().map(|_| {
        network.register(
            config::TEE_DKG_CHANNEL,
            Quota::per_second(NZU32!(128)),
            config::CHANNEL_BACKLOG,
        )
    });

    // Register the key-handoff channel (only with a TEE enclave sidecar). A
    // joining/keyless committee member requests the resident offer key here; a
    // current node seals it to the newcomer's attested X25519 key. Long-lived (a
    // node serves handoff requests for as long as it runs), unlike the one-time
    // DKG/bootstrap channels. Registered before `network.start()`.
    let mut tee_handoff_channel = args.tee_enclave_socket.as_ref().map(|_| {
        network.register(
            config::TEE_HANDOFF_CHANNEL,
            Quota::per_second(NZU32!(64)),
            config::CHANNEL_BACKLOG,
        )
    });

    // Parse consensus peers: `<hex_pubkey>@<host:port>` → (PublicKey, SocketAddr).
    let bootnode_map = parse_consensus_peers(&args.consensus_peers)?;

    if !bootnode_map.is_empty() {
        info!(count = bootnode_map.len(), "parsed bootnode entries");
    }

    // Build peer set from validator config + bootnodes.
    let peer_map = build_peer_map(&validator_set, &bootnode_map);
    let mut initial_peer_map = peer_map.clone();
    let resolved_count = peer_map.len();
    let _ = oracle.track(p2p_oracle_chain_peer_set_id(0), peer_map);
    info!(
        total = validator_set.public_keys.len(),
        resolved = resolved_count,
        bootnodes = bootnode_map.len(),
        "P2P peer set registered with oracle"
    );

    // ── 4. Start P2P network (needed before DKG can run) ───────────────
    let mut network_handle = network.start();
    info!("P2P network started");

    // ── 5. Create Muxers from physical channels ────────────────────────
    // Consensus channels are muxed by epoch — each engine restart
    // gets fresh sub-channels, preventing message interference.
    let (vote_muxer, mut vote_mux) =
        Muxer::new(ctx.child("vote_mux"), votes.0, votes.1, MUXER_MAILBOX);
    vote_muxer.start();

    let (cert_muxer, mut cert_mux) = Muxer::new(
        ctx.child("cert_mux"),
        certificates.0,
        certificates.1,
        MUXER_MAILBOX,
    );
    cert_muxer.start();

    let (res_muxer, mut res_mux) =
        Muxer::new(ctx.child("res_mux"), resolver.0, resolver.1, MUXER_MAILBOX);
    res_muxer.start();

    // Stash for sub-channels pre-registered at DKG completion.
    // The activation handler pre-registers vote/cert/res sub-channels for
    // the upcoming epoch as soon as DKG completes — well before the
    // boundary's planned activation height. By the time any peer fires
    // its activation handler, every honest node already has routes for
    // the new epoch's sub-channels and cannot drop early proposals/votes
    // because of an unregistered sub-channel (Mode-B race). The top of
    // the next `'epoch_loop` iteration consumes this stash via
    // `take_or_register_current`.
    let mut next_epoch_subchannels: Option<
        outbe_consensus::epoch_subchannels::EpochSubchannels<_, _>,
    > = None;

    // DKG channel muxed by reshare round.
    let (dkg_muxer, mut dkg_mux) = Muxer::new(
        ctx.child("dkg_mux"),
        dkg_channel.0,
        dkg_channel.1,
        MUXER_MAILBOX,
    );
    dkg_muxer.start();

    // R5.4: mux the TEE DKG + TEE bootstrap channels by round, mirroring `dkg_mux`,
    // so the startup ceremony (round 0) and a later epoch-boundary reshare (round N)
    // each get isolated sub-channels. `None` when no TEE enclave sidecar is set.
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
    let mut tee_handoff_mux = tee_handoff_channel.take().map(|ch| {
        let (muxer, handle) = Muxer::new(ctx.child("tee_handoff_mux"), ch.0, ch.1, MUXER_MAILBOX);
        muxer.start();
        handle
    });

    // R5.4: pre-register the round-0 TEE sub-channels EARLY (mirroring the
    // consensus `dkg_mux.register(0)` at startup) so every node has round 0 routed
    // well before the startup TEE DKG begins. Registering it lazily inside the
    // startup block races: a node can broadcast its identity before a peer has
    // registered round 0, and the mux drops the unrouted message → the identity
    // exchange hangs. Reshare rounds (N>0) still register on demand at the boundary.
    let mut tee_dkg_round0 = match tee_dkg_mux.as_mut() {
        Some(m) => Some(
            m.register(0)
                .await
                .map_err(|e| eyre::eyre!("failed to pre-register TEE DKG round 0: {e}"))?,
        ),
        None => None,
    };
    let mut tee_bootstrap_round0 = match tee_bootstrap_mux.as_mut() {
        Some(m) => Some(
            m.register(0)
                .await
                .map_err(|e| eyre::eyre!("failed to pre-register TEE bootstrap round 0: {e}"))?,
        ),
        None => None,
    };
    // Pre-register the handoff sub-channel (round 0) so a newcomer's request
    // routes to the responders without a registration race.
    let mut tee_handoff_round0 = match tee_handoff_mux.as_mut() {
        Some(m) => Some(
            m.register(0)
                .await
                .map_err(|e| eyre::eyre!("failed to pre-register TEE handoff round 0: {e}"))?,
        ),
        None => None,
    };

    info!("channel muxers started");

    // Startup chain-state sources must exist before threshold material selection:
    // DKG round 0 is allowed only when both execution and marshal prove genesis
    // formation. Local execution height 0 alone is not sufficient for a fresh
    // datadir joining an already-running network.
    let genesis_hash = genesis_hash(&node)?;
    let epoch_length_blocks = epoch_length_blocks_from_genesis(&node)?;
    let dkg_rotation_params = DkgRotationParams::from_genesis(&node, epoch_length_blocks);

    // ── 5b. Pre-compute page cache (shared across marshal + epochs) ─────
    let page_cache = CacheRef::from_pooler(
        ctx,
        nonzero_u16(4096, "page cache page size")?,
        nonzero_usize(config::PAGE_CACHE_SIZE / 4096, "PAGE_CACHE_SIZE / 4096")?,
    );

    // ── 5c. Initialize marshal actor before threshold material selection ─
    //
    // Marshal init exposes persisted consensus finalized height. That height is
    // part of the genesis-formation proof; without it a crash-restart with
    // execution height 0 could incorrectly start DKG round 0.
    use commonware_consensus::marshal;
    use commonware_cryptography::{certificate::Scheme as CertScheme, Signer as _};
    use commonware_storage::archive::immutable;

    let certificate_scheme_provider = HybridSchemeProvider::<MinSig>::new();
    let elector_config_provider = HybridElectorConfigProvider::<MinSig>::new();
    let committee_provider = CommitteeProvider::new();

    let partition_prefix = "outbe-marshal".to_string();

    let finalizations_archive = immutable::Archive::init(
        ctx.child("marshal_finalizations"),
        immutable::Config {
            metadata_partition: format!("{partition_prefix}-finalizations-metadata"),
            freezer_table_partition: format!("{partition_prefix}-finalizations-freezer-table"),
            freezer_table_initial_size: config::FREEZER_TABLE_INITIAL_SIZE,
            freezer_table_resize_frequency: config::FREEZER_TABLE_RESIZE_FREQUENCY,
            freezer_table_resize_chunk_size: config::FREEZER_TABLE_RESIZE_CHUNK_SIZE,
            freezer_key_partition: format!("{partition_prefix}-finalizations-freezer-key"),
            freezer_key_page_cache: page_cache.clone(),
            freezer_value_partition: format!("{partition_prefix}-finalizations-freezer-value"),
            freezer_value_target_size: config::FREEZER_VALUE_TARGET_SIZE,
            freezer_value_compression: config::FREEZER_VALUE_COMPRESSION,
            ordinal_partition: format!("{partition_prefix}-finalizations-ordinal"),
            items_per_section: nonzero_u64(
                config::IMMUTABLE_ITEMS_PER_SECTION,
                "IMMUTABLE_ITEMS_PER_SECTION",
            )?,
            codec_config: HybridScheme::<MinSig>::certificate_codec_config_unbounded(),
            replay_buffer: nonzero_usize(config::MARSHAL_REPLAY_BUFFER, "MARSHAL_REPLAY_BUFFER")?,
            freezer_key_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
            freezer_value_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
            ordinal_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
        },
    )
    .await
    .wrap_err("failed to initialize finalizations archive")?;

    let blocks_archive = immutable::Archive::init(
        ctx.child("marshal_blocks"),
        immutable::Config {
            metadata_partition: format!("{partition_prefix}-blocks-metadata"),
            freezer_table_partition: format!("{partition_prefix}-blocks-freezer-table"),
            freezer_table_initial_size: config::FREEZER_TABLE_INITIAL_SIZE,
            freezer_table_resize_frequency: config::FREEZER_TABLE_RESIZE_FREQUENCY,
            freezer_table_resize_chunk_size: config::FREEZER_TABLE_RESIZE_CHUNK_SIZE,
            freezer_key_partition: format!("{partition_prefix}-blocks-freezer-key"),
            freezer_key_page_cache: page_cache.clone(),
            freezer_value_partition: format!("{partition_prefix}-blocks-freezer-value"),
            freezer_value_target_size: config::FREEZER_VALUE_TARGET_SIZE,
            freezer_value_compression: config::FREEZER_VALUE_COMPRESSION,
            ordinal_partition: format!("{partition_prefix}-blocks-ordinal"),
            items_per_section: nonzero_u64(
                config::IMMUTABLE_ITEMS_PER_SECTION,
                "IMMUTABLE_ITEMS_PER_SECTION",
            )?,
            codec_config: (),
            replay_buffer: nonzero_usize(config::MARSHAL_REPLAY_BUFFER, "MARSHAL_REPLAY_BUFFER")?,
            freezer_key_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
            freezer_value_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
            ordinal_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
        },
    )
    .await
    .wrap_err("failed to initialize blocks archive")?;

    let epocher = commonware_consensus::types::FixedEpocher::new(nonzero_u64(
        u64::from(epoch_length_blocks),
        "epochLengthBlocks",
    )?);
    let view_retention_timeout = u64::from(config::ACTIVITY_TIMEOUT)
        .checked_mul(config::VIEW_RETENTION_MULTIPLIER)
        .ok_or_else(|| eyre::eyre!("view retention timeout overflow"))?;

    let marshal_genesis_anchor = genesis_consensus_block(&node)?;
    let (marshal_actor, marshal_mailbox, last_consensus_finalized_opt) =
        marshal::core::Actor::init(
            ctx.child("marshal"),
            finalizations_archive,
            blocks_archive,
            marshal::Config {
                provider: certificate_scheme_provider.clone(),
                epocher,
                start: marshal::Start::Genesis(marshal_genesis_anchor),
                partition_prefix: partition_prefix.clone(),
                mailbox_size: nonzero_usize(config::ENGINE_MAILBOX_SIZE, "ENGINE_MAILBOX_SIZE")?,
                view_retention_timeout: ViewDelta::new(view_retention_timeout),
                prunable_items_per_section: nonzero_u64(
                    config::PRUNABLE_ITEMS_PER_SECTION,
                    "PRUNABLE_ITEMS_PER_SECTION",
                )?,
                page_cache: page_cache.clone(),
                replay_buffer: nonzero_usize(
                    config::MARSHAL_REPLAY_BUFFER,
                    "MARSHAL_REPLAY_BUFFER",
                )?,
                key_write_buffer: nonzero_usize(
                    config::MARSHAL_WRITE_BUFFER,
                    "MARSHAL_WRITE_BUFFER",
                )?,
                value_write_buffer: nonzero_usize(
                    config::MARSHAL_WRITE_BUFFER,
                    "MARSHAL_WRITE_BUFFER",
                )?,
                block_codec_config: (),
                max_repair: nonzero_usize(config::MAX_REPAIR, "MAX_REPAIR")?,
                max_pending_acks: nonzero_usize(config::MAX_PENDING_ACKS, "MAX_PENDING_ACKS")?,
                strategy: commonware_parallel::Sequential,
            },
        )
        .await;

    // commonware 2026.5.0: `Actor::init` now returns `Option<Height>` — `None`
    // means no durable consensus finalization yet (fresh genesis). Map that to
    // height 0, preserving the prior non-optional `Height` semantics used by the
    // genesis-formation proof, crash-recovery detection, and executor start.
    let last_consensus_finalized = map_marshal_init_height(last_consensus_finalized_opt);

    let recovered_ce_marker = ce_startup_recovery
        .recover_before_participation(last_consensus_finalized.get())
        .wrap_err("compressed-tree startup recovery failed before validator participation")?;

    info!(
        last_consensus_finalized = last_consensus_finalized.get(),
        ce_marker_height = recovered_ce_marker.height,
        "marshal actor initialized"
    );

    let startup_snapshot = resolve_startup_dkg_snapshot(
        ctx,
        &node,
        &args,
        &key_backend,
        &signing_key,
        &validator_set,
        genesis_hash,
        dkg_rotation_params,
        last_consensus_finalized.get(),
    )
    .await?;
    let mut last_execution_height = startup_snapshot.last_execution_height;
    let mut last_execution_hash = startup_snapshot.last_execution_hash;
    let mut recovered_boundary = startup_snapshot.recovered_boundary;
    let startup_dkg_context = startup_snapshot.context;

    if args.force_dkg
        && startup_dkg_context.last_execution_height > 0
        && !startup_dkg_context.has_chain_finalized_dkg_boundary()
    {
        return Err(eyre::eyre!(
            "--testnet.force-dkg existing-chain recovery requires a recovered \
             chain DKG boundary before running a fresh ceremony. Restore \
             validator-N/data/consensus/outbe-* and finalized_parent_certs; \
             do not delete consensus archives when preserving the chain."
        ));
    }

    // ── 6. Obtain threshold material ────────────────────────────────────
    let mut startup_live_join_completed = false;
    // For initial DKG, use subchannel 0 of the DKG mux.
    let (dkg_init_tx, dkg_init_rx) = dkg_mux
        .register(0)
        .await
        .map_err(|e| eyre::eyre!("failed to register initial DKG subchannel: {e}"))?;

    let threshold_material = obtain_threshold_material(
        ctx,
        &args,
        &key_backend,
        &signing_key,
        &validator_set,
        startup_dkg_context,
        dkg_init_tx,
        dkg_init_rx,
    )
    .await?;
    let (mut signing_share, mut polynomial, mut last_dkg_output, bootstrap_from_live_dkg) =
        match threshold_material {
            ThresholdMaterial::Ready {
                signing_share,
                polynomial,
                last_dkg_output,
                bootstrap_from_live_dkg,
            } => (
                Some(signing_share),
                polynomial,
                last_dkg_output,
                bootstrap_from_live_dkg,
            ),
            ThresholdMaterial::VerifierOnly {
                polynomial,
                last_dkg_output,
            } => (None, polynomial, last_dkg_output, false),
            ThresholdMaterial::StartupLiveJoinRequired => {
                startup_live_join_completed = true;
                let joined = run_startup_live_join_reshare(
                    ctx,
                    &args,
                    &key_backend,
                    &signing_key,
                    &node,
                    &mut dkg_mux,
                    &mut oracle,
                    &bootnode_map,
                    last_consensus_finalized.get(),
                )
                .await?;
                recovered_boundary = Some((
                    joined.activated_at_height,
                    joined.activated_boundary.clone(),
                ));
                last_execution_height = node.provider.last_block_number().map_err(|error| {
                    eyre::eyre!(
                        "failed to refresh execution height after startup live-join: {error}"
                    )
                })?;
                last_execution_hash = if last_execution_height > 0 {
                    node.provider
                        .block_hash(last_execution_height)
                        .map_err(|error| {
                            eyre::eyre!(
                                "failed to refresh execution hash at height {last_execution_height} after startup live-join: {error}"
                            )
                        })?
                        .ok_or_else(|| {
                            eyre::eyre!(
                                "missing execution hash at refreshed startup live-join height {last_execution_height}"
                            )
                        })?
                } else {
                    genesis_hash
                };
                validator_set = validators::read_consensus_validators_at_latest(&node.provider)
                    .wrap_err("failed to read consensus validators after startup live-join")?;
                initial_peer_map = build_peer_map(&validator_set, &bootnode_map);
                let _ = oracle.overwrite(initial_peer_map.clone());
                info!(
                    validators = validator_set.public_keys.len(),
                    peers = initial_peer_map.len(),
                    "startup live-join refreshed validator and peer set after activation"
                );
                (
                    Some(joined.signing_share),
                    joined.polynomial,
                    Some(joined.output),
                    false,
                )
            }
        };

    // ── 7. Build participant set (updated after each DKG reshare) ───────
    // when recovering a finalized DKG boundary, reconstruct the scheme
    // against the committee the recovered threshold material belongs to (the DKG
    // output's players), NOT the latest on-chain set, which may have drifted
    // across a churn window. `select_recovery_participants` also fails fast if the
    // restored material does not match the recovered boundary. On a fresh chain or
    // when no boundary/output is recovered, fall back to the latest committed set
    // (the genesis committee on first start).
    let mut participants: commonware_utils::ordered::Set<bls12381::PublicKey> =
        match (recovered_boundary.as_ref(), last_dkg_output.as_ref()) {
            (Some((_, boundary)), Some(output)) => {
                select_recovery_participants(output.players(), boundary)?
            }
            _ => validator_set
                .public_keys
                .clone()
                .into_iter()
                .try_collect()
                .map_err(|e| eyre::eyre!("invalid participant set: {e}"))?,
        };

    let active_validator_set = validators::read_validators_at_latest(&node.provider)
        .wrap_err("failed to load active validator set for EVM signer validation")?;
    // Verifier-join: --consensus.public-polynomial + --consensus.dkg-output without a
    // --consensus.signing-share. The node may not yet be in the on-chain validator set
    // (it is syncing from genesis), so membership is not fatal — it runs as a verifier.
    let verifier_join = args.signing_share.is_none()
        && args.public_polynomial.is_some()
        && args.dkg_output.is_some();
    let recovered_committee_for_signer = recovered_boundary
        .as_ref()
        .map(|(_, boundary)| (&participants, boundary));
    let proposer_evm_address = validate_validator_evm_signer(
        &args,
        &signing_key,
        &validator_set,
        &active_validator_set,
        recovered_committee_for_signer,
        verifier_join,
    )?;

    // ── 7b. One-time TEE DKG + bootstrap coordination (startup, like the DKG) ──
    // On a fresh chain (no executed blocks yet), if this validator runs a TEE
    // enclave sidecar:
    //   1. run the TEE DKG ceremony so the committee's enclaves collaboratively
    //      derive the shared tribute offer key (Seam F: a group threshold
    //      signature over a fixed message → HKDF → X25519; byte-identical on every
    //      honest node, secret resident in each enclave); then
    //   2. coordinate the committee's enclave registrations + EVM signatures into
    //      the block-1 `TeeBootstrap` payload — registering the DKG-derived offer
    //      key — and stash it in the bridge for the proposer to inject (slice 5.1).
    // `committee_snapshot_block` is the fixed block 1. The whole ceremony MUST
    // complete before block 1: it is wrapped in `--tee-bootstrap-timeout-secs` and
    // FAILS FAST (node halts via startup error) on timeout or error, rather than
    // proceeding into a permanently un-bootstrapped chain (no offer key on-chain =>
    // offers impossible). Local liveness only — not a consensus rule on imported
    // blocks. Skipped entirely when no `--tee-enclave-socket` is configured.
    if let (Some(socket), Some(my_validator)) =
        (args.tee_enclave_socket.clone(), proposer_evm_address)
    {
        // The startup DKG closure (last_execution_height == 0) moves `socket`; clone
        // it up front for the handoff responder spawned after the if/else.
        let socket_for_responder = socket.clone();
        if last_execution_height == 0 {
            let dkg_chain_id =
                B256::left_padding_from(&node.chain_spec().chain().id().to_be_bytes());
            let n = participants.len();
            let deadline = std::time::Duration::from_secs(args.tee_bootstrap_timeout_secs);

            let (dkg_sender, dkg_receiver) = tee_dkg_round0
                .take()
                .ok_or_else(|| eyre::eyre!("TEE DKG P2P channel not registered"))?;
            let (tee_sender, tee_receiver) = tee_bootstrap_round0
                .take()
                .ok_or_else(|| eyre::eyre!("TEE bootstrap P2P channel not registered"))?;
            let (evm_signer, committee) =
                tee_bootstrap_setup(&args, &participants, &validator_set)?;
            let tee_policy = tee_policy_from_genesis(&node)?;

            // Step 1 (TEE DKG → shared offer key) + Step 2 (bootstrap coordination →
            // block-1 payload), under one deadline. Any error or timeout halts.
            // The deadline is measured on the consensus runtime `Clock` (the same
            // time source the deterministic test runtime can mock and advance), not
            // wall-clock — keeping startup-timeout behavior reproducible and free of a
            // direct async-runtime timer dependency in the consensus stack.
            // `Clock::timeout` requires a `Send + 'static` future; the `async move`
            // owns every capture, so the bound holds.
            // Owned `Clock` clone moved into the `'static` startup future so the TEE
            // DKG identity-exchange cadence runs on the consensus runtime clock, not
            // tokio's wall-clock (mockable under the deterministic test runtime).
            let dkg_clock = ctx.child("tee_dkg_clock");
            let bootstrap_clock = ctx.child("tee_bootstrap_clock");
            let payload = ctx
                .timeout(deadline, async move {
                    // Host connect policy from the genesis teePolicy: strict
                    // under gramine-sgx, unattested-fallback on the dev box.
                    let connect_policy =
                        crate::tee_bootstrap::quote_policy_from_tee_policy(&tee_policy);
                    let (tribute_offer_public, tribute_offer_group_public_key) =
                        crate::tee_bootstrap::run_tee_dkg_at_startup(
                            &socket,
                            dkg_clock,
                            n,
                            dkg_chain_id,
                            0,
                            &connect_policy,
                            dkg_sender,
                            dkg_receiver,
                        )
                        .await
                        .map_err(|e| eyre::eyre!("TEE DKG ceremony failed: {e}"))?;
                    info!(
                        tribute_offer_public = %B256::from(tribute_offer_public),
                        "TEE DKG complete — shared tribute offer key derived"
                    );
                    let payload = crate::tee_bootstrap::run_tee_bootstrap_at_startup(
                        &socket,
                        bootstrap_clock,
                        my_validator,
                        committee,
                        B256::from(tribute_offer_public),
                        tribute_offer_group_public_key,
                        tee_policy,
                        &evm_signer,
                        tee_sender,
                        tee_receiver,
                    )
                    .await
                    .map_err(|e| eyre::eyre!("TEE bootstrap coordination failed: {e}"))?;
                    Ok::<_, eyre::Report>(payload)
                })
                .await
                .map_err(|_| {
                    eyre::eyre!(
                        "TEE DKG + bootstrap did not complete within {}s \
                     (--tee-bootstrap-timeout-secs); halting before block 1",
                        args.tee_bootstrap_timeout_secs
                    )
                })??;

            info!(
                validators = payload.registrations.len(),
                "TEE bootstrap coordinated — payload ready for block 1"
            );
            bridge.set_pending_tee_bootstrap(payload);
        } else {
            // Existing-chain join: if the enclave lacks the offer key — its
            // advertised offer public differs from the on-chain registered key —
            // obtain it via a key-handoff before block production (the node cannot
            // decrypt offers without it). Fail-fast on timeout, like the startup DKG.
            let on_chain_offer =
                validators::read_tee_offer_public_at_latest(&node.provider).unwrap_or(B256::ZERO);
            if on_chain_offer != B256::ZERO {
                let connect_policy = crate::tee_bootstrap::quote_policy_from_tee_policy(
                    &tee_policy_from_genesis(&node)?,
                );
                let enclave_offer =
                    crate::tee_bootstrap::query_enclave_offer_public(&socket, &connect_policy)
                        .unwrap_or(B256::ZERO);
                if enclave_offer != on_chain_offer {
                    let (htx, hrx) = tee_handoff_round0
                        .take()
                        .ok_or_else(|| eyre::eyre!("TEE handoff P2P channel not registered"))?;
                    let chain_id =
                        B256::left_padding_from(&node.chain_spec().chain().id().to_be_bytes());
                    let deadline = std::time::Duration::from_secs(args.tee_bootstrap_timeout_secs);
                    let socket_join = socket.clone();
                    // Handoff quorum (fault tolerance): require confirmations from
                    // `f + 1` DISTINCT responders, where `f = ⌊(n-1)/3⌋` is the BFT
                    // fault bound — so at least one confirmer is honest even if up to
                    // `f` are Byzantine, while TOLERATING up to `f` unavailable nodes.
                    // (Correctness is already guaranteed by the newcomer's on-chain key
                    // check, so this quorum is anti-equivocation/availability defense in
                    // depth; ⌊2n/3⌋+1 required ALL honest nodes at n=4 and stalled if
                    // any one was down.)
                    let committee_n = validator_set.public_keys.len();
                    let f = committee_n.saturating_sub(1) / 3;
                    let min_confirmations = f + 1;
                    // Derive the offer key for the chain's current epoch, not a
                    // hardcoded 0 (future-proofs the handoff for offer-key rotation).
                    let tribute_offer_epoch =
                        validators::read_tee_offer_epoch_at_latest(&node.provider).unwrap_or(0);
                    let handoff_clock = ctx.child("tee_handoff_clock");
                    ctx.timeout(deadline, async move {
                        crate::tee_bootstrap::run_tee_handoff_join(
                            &socket_join,
                            handoff_clock,
                            on_chain_offer,
                            chain_id,
                            tribute_offer_epoch,
                            min_confirmations,
                            &connect_policy,
                            htx,
                            hrx,
                        )
                        .await
                    })
                    .await
                    .map_err(|_| {
                        eyre::eyre!(
                            "TEE key-handoff did not complete within {}s \
                             (--tee-bootstrap-timeout-secs); halting before block production",
                            args.tee_bootstrap_timeout_secs
                        )
                    })??;
                    info!(
                        %on_chain_offer,
                        "TEE offer key obtained via key-handoff (existing-chain join)"
                    );
                }
            }

            // Record THIS validator's current enclave keys on-chain via a
            // normal EOA tx (a node-submitted on-chain enclave-registration tx).
            // The genesis bootstrap registered the committee at block 1; on an
            // existing-chain restart the enclave's ephemeral attestation/noise keys are
            // freshly generated, so re-submit to keep the on-chain registry accurate.
            // Best-effort: a submission failure logs a warning but does NOT halt block
            // production — the offer key (the consensus-critical material) is already
            // present, and the registry's attestation/noise slots are an audit snapshot
            // not read on the runtime path.
            if let Some(evm_key_path) = args.effective_validator_evm_key()? {
                let connect_policy = crate::tee_bootstrap::quote_policy_from_tee_policy(
                    &tee_policy_from_genesis(&node)?,
                );
                let evm_signer = outbe_primitives::signer::OutbeEvmSigner::from_file(&evm_key_path)
                    .map_err(|e| {
                        eyre::eyre!("failed to load validator EVM signer for registration: {e}")
                    })?;
                let chain_id = node.chain_spec().chain().id();
                // Latest block base fee, so the registration tx prices above the pool
                // floor + current base fee and actually lands (0 ⇒ silently parked).
                // Default 0 ⇒ the gas-price helper falls back to the pool floor.
                let base_fee = node
                    .provider
                    .last_block_number()
                    .ok()
                    .and_then(|n| node.provider.sealed_header(n).ok().flatten())
                    .and_then(|h| h.header().inner.base_fee_per_gas)
                    .unwrap_or(0);
                match crate::tee_bootstrap::submit_enclave_registration(
                    &socket,
                    &connect_policy,
                    &evm_signer,
                    &node.pool,
                    &node.provider,
                    chain_id,
                    base_fee,
                )
                .await
                {
                    Ok(tx_hash) => info!(
                        %tx_hash,
                        validator = %evm_signer.address(),
                        "submitted on-chain enclave registration (existing-chain join)"
                    ),
                    Err(e) => warn!(
                        %e,
                        "on-chain enclave registration submission failed (non-fatal)"
                    ),
                }
            }
        }

        // Handoff responder: when this node has (or just obtained) the offer key, serve
        // handoff requests for other joiners for as long as it runs. Skipped on a
        // node that consumed the handoff channel as a newcomer (v1 — it does not also
        // respond). Authorization uses the startup active set (set-change deferred).
        if let Some((htx, hrx)) = tee_handoff_round0.take() {
            let connect_policy = crate::tee_bootstrap::quote_policy_from_tee_policy(
                &tee_policy_from_genesis(&node)?,
            );
            let verify_policy = connect_policy.clone();
            let authorized = validator_set.public_keys.clone();
            // WS-M2 A.3 (M7): the responder binds the seal target to the requester's
            // on-chain registered `recipient_x25519`, read at REQUEST time from the
            // latest state — NOT a genesis snapshot (which is empty before the block-1
            // `TeeBootstrap` writes the registry, and would never see a post-genesis
            // `registerEnclave`). Map each committee consensus key → EVM address; the
            // closure resolves that address's registered recipient per request.
            let addr_by_key: std::collections::BTreeMap<Vec<u8>, alloy_primitives::Address> =
                validator_set
                    .public_keys
                    .iter()
                    .zip(validator_set.addresses.iter())
                    .map(|(pk, addr)| (commonware_codec::Encode::encode(pk).to_vec(), *addr))
                    .collect();
            let recipient_provider = node.provider.clone();
            let registered_recipient = move |from_key: &[u8]| -> Option<B256> {
                let addr = addr_by_key.get(from_key)?;
                validators::read_tee_recipient_x25519_at_latest(&recipient_provider, *addr)
                    .ok()
                    .filter(|recipient| *recipient != B256::ZERO)
            };
            let socket_serve = socket_for_responder;
            ctx.child("tee_handoff_responder")
                .spawn(move |_| async move {
                    if let Err(e) = crate::tee_bootstrap::serve_tee_handoff(
                        &socket_serve,
                        &connect_policy,
                        verify_policy,
                        authorized,
                        registered_recipient,
                        htx,
                        hrx,
                    )
                    .await
                    {
                        warn!(%e, "TEE handoff responder exited");
                    }
                });
        }
    }

    // ── 8. Recover execution finalized state ──────────────────────────────
    let force_dkg_existing_chain = args.force_dkg && last_execution_height > 0;
    let mut active_boundary = recovered_boundary.clone();
    let mut active_boundary_finalized = startup_dkg_context.recovered_boundary_finalized;

    if force_dkg_existing_chain {
        let previous_boundary = recovered_boundary
            .as_ref()
            .map(|(_, artifact)| artifact.clone())
            .ok_or_else(|| {
                eyre::eyre!(
                    "--testnet.force-dkg existing-chain recovery requires a recovered \
                     chain DKG boundary. Restore validator-N/data/consensus/outbe-* \
                     and finalized_parent_certs; do not delete consensus archives when \
                     preserving the chain."
                )
            })?;
        ensure!(
            startup_dkg_context.recovered_boundary_finalized,
            "--testnet.force-dkg cannot replace a still-pending DKG boundary; restart \
             without force-dkg or restore finalized consensus state first"
        );
        let recovery_output = last_dkg_output.as_ref().ok_or_else(|| {
            eyre::eyre!("--testnet.force-dkg completed without a DKG output artifact")
        })?;
        let (recovery_activation_height, recovery_boundary) = build_force_dkg_recovery_boundary(
            &validator_set,
            recovery_output,
            &previous_boundary,
            last_execution_height,
        )?;
        if let Some(ref keys_dir) = args.keys_dir {
            save_pending_dkg_state(
                keys_dir,
                signing_share
                    .as_ref()
                    .ok_or_else(|| eyre::eyre!("--testnet.force-dkg requires a threshold share"))?,
                &polynomial,
                recovery_output,
                &key_backend,
            )
            .wrap_err("failed to durably save force-DKG pending state")?;
            save_pending_dkg_boundary(
                keys_dir,
                &PendingDkgBoundarySnapshot {
                    artifact: recovery_boundary.clone(),
                    activated_at_height: recovery_activation_height,
                },
            )
            .wrap_err("failed to durably save force-DKG pending boundary")?;
        }
        info!(
            previous_epoch = previous_boundary.epoch,
            recovery_epoch = recovery_boundary.epoch,
            dkg_cycle = recovery_boundary.dkg_cycle,
            freeze_height = recovery_boundary.freeze_height,
            planned_activation_height = recovery_boundary.planned_activation_height,
            vrf_material_version = recovery_boundary.vrf_material_version,
            vrf_group_public_key = %recovery_boundary.vrf_group_public_key,
            "--testnet.force-dkg queued disaster-recovery DKG boundary"
        );
        active_boundary = Some((recovery_activation_height, recovery_boundary));
        active_boundary_finalized = false;
    }

    let recovered_boundary_artifact = active_boundary.as_ref().map(|(_, artifact)| artifact);
    let mut vrf_material_version = recovered_boundary_artifact
        .map(|artifact| artifact.vrf_material_version)
        .unwrap_or(0);
    // These strict checks (saved polynomial / DKG output must equal the recovered
    // finalized boundary) only matter for a SIGNER, which signs with its polynomial +
    // share. A share-less VERIFIER follows finality via the certificate's PARTICIPANT
    // set (not its polynomial), so its CLI `--public-polynomial`/`--dkg-output` may be
    // off (e.g. a TEE chain's runtime-derived genesis consensus polynomial differs
    // from the bootstrap file, or the chain has rotated past it) without affecting
    // sync — only its local VRF/leader view is degraded (process-local, non-fatal,
    // same as the post-rotation verifier-follower case). Enforcing these on a restarted
    // verifier would fatally crash an otherwise-healthy follower, so gate them to
    // signers; the verifier syncs and the running epoch loop advances it.
    if signing_share.is_some() {
        validate_recovered_vrf_material(&polynomial, recovered_boundary_artifact)?;
        if let (Some(output), Some(boundary)) = (&last_dkg_output, recovered_boundary_artifact) {
            let canonical_output = decode_boundary_output(boundary)
                .wrap_err("failed to decode recovered DKG boundary output")?;
            dkg_manager::assert_canonical_output(output, &canonical_output, "restart recovery")?;
        }
    } else if let Some(boundary) = recovered_boundary_artifact {
        // Verifier-follower with a recovered on-chain DKG boundary (e.g. a restart, or
        // a TEE chain whose runtime genesis consensus output differs from the bootstrap
        // CLI files): adopt the chain's CURRENT canonical DKG output as both the
        // polynomial and the reshare prev_output. The DKG reshare ceremony binds the
        // FULL previous output into its `info_hash` (not just the group key), so if the
        // verifier later becomes a frozen-target player it MUST present the committee's
        // current output as prev_output — its stale `--consensus.dkg-output` would yield
        // a divergent `info_hash`, the dealers' bundles get dropped, and the ceremony
        // times out (the node never gets a share). The genesis/boundary artifact carries
        // the full `Output`, so `decode_boundary_output` recovers exactly what the
        // committee holds. Finality still verifies via the participant set regardless.
        let canonical_output = decode_boundary_output(boundary)
            .wrap_err("failed to decode recovered DKG boundary output for verifier")?;
        polynomial = canonical_output.public().clone();
        last_dkg_output = Some(canonical_output);
    }
    let vrf_materials = VrfMaterialProvider::new(
        vrf_material_version,
        polynomial.clone(),
        signing_share.clone(),
    );
    // The boundary tuple height is the ACTIVATION ANCHOR (the height the live
    // committee anchored its rotation schedule on), NOT the commit height of the
    // artifact-carrying block: every producer normalizes commit → anchor
    // (commit - 1, since the artifact rides the FIRST new-epoch block), and the
    // pending-snapshot restore stores the anchor directly. Anchoring on the
    // commit height instead shifts a restarted node's freeze/activation schedule
    // one block late vs the committee — fatal when several committee members
    // restart within one epoch (the new epoch loses quorum waiting for an
    // activation height that can never be produced).
    let mut last_dkg_activation_height = active_boundary
        .as_ref()
        .map(|(height, _)| *height)
        .unwrap_or(last_execution_height);
    let mut dkg_cycle = recovered_boundary_artifact
        .map(|artifact| artifact.dkg_cycle.saturating_add(1))
        .unwrap_or(1);
    let recovered_epoch = recovered_boundary_artifact
        .map(|artifact| artifact.epoch)
        .unwrap_or(0);
    let vrf_safety = VrfSafetyGate::new(
        vrf_material_version,
        last_dkg_activation_height,
        dkg_rotation_params.planned_activation_height(last_dkg_activation_height),
        dkg_rotation_params.activation_grace_blocks,
    );
    info!(
        vrf_material_version,
        vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
        last_dkg_activation_height,
        next_planned_activation_height = dkg_rotation_params
            .planned_activation_height(last_dkg_activation_height),
        vrf_expiry_height = dkg_rotation_params
            .planned_activation_height(last_dkg_activation_height)
            .saturating_add(dkg_rotation_params.activation_grace_blocks),
        "VRF material active"
    );
    publish_randomness_status(&bridge, &vrf_safety);

    // NOTE: is_fresh_bootstrap is determined AFTER marshal init (below),
    // using both execution height and consensus processed height.
    // This prevents false fresh-bootstrap on crash restart (SIGKILL/OOM)
    // where Reth lost in-memory state but consensus is durable.
    let dkg_manager = DkgManagerMailbox::new();
    let ancestry_readiness_target = last_consensus_finalized.get();
    let ancestry_readiness =
        AncestryReadiness::new(last_execution_height, ancestry_readiness_target);
    if !ancestry_readiness.is_ready() {
        info!(
            last_execution_height,
            last_consensus_finalized = ancestry_readiness_target,
            "marshal ancestry gate closed until executor backfills durable consensus blocks"
        );
    }
    if !active_boundary_finalized {
        if let Some(boundary) = recovered_boundary_artifact.cloned() {
            dkg_manager.note_recovered_pending_boundary(boundary);
            info!(
                epoch = recovered_epoch,
                last_dkg_activation_height,
                "restored pending DKG boundary into DKG manager before starting Simplex"
            );
        }
    }

    // ── 9. Get beacon engine handle and payload builder handle ──────────
    let engine_handle: EngineHandle = node.add_ons_handle.beacon_engine_handle.clone();
    let payload_builder = node.payload_builder_handle.clone();

    // sus-5: the executor publishes execution-finalized heights here; the
    // supervisor consumes them to drive height-based DKG/VRF rotation. The
    // consumer arm is gated off while a reshare is in progress
    // (`if !reshare_in_progress`), so heights accumulate during a reshare. The
    // backlog is BOUNDED by the reshare duration (one height per finalized block
    // for the length of a reshare) and is drained in order afterwards. We keep an
    // ordered (unbounded) mpsc rather than a latest-only `watch` deliberately: the
    // drain feeds per-height rotation-threshold logic (freeze/activation heights),
    // so heights are processed in sequence rather than coalesced to the latest.
    let (execution_finalized_height_tx, mut execution_finalized_height_rx) =
        tokio::sync::mpsc::unbounded_channel::<u64>();
    let (consensus_tip_tx, mut consensus_tip_rx) =
        tokio::sync::watch::channel::<Option<crate::marshal_update_reporter::ConsensusTip>>(None);

    // Reth may have one or more speculative canonical blocks above marshal's
    // durable certified tip when the process stops. Those blocks remain useful
    // as local payload data, but they are not a finalization authority. Seed the
    // executor and FinalizationView at the highest height confirmed by both
    // stores so a different block winning at the first unfinalized height can
    // be imported and selected by forkchoice after restart.
    let recovery_anchor_height =
        durable_recovery_anchor_height(last_execution_height, last_consensus_finalized.get());
    let recovery_anchor_hash = if recovery_anchor_height == 0 {
        genesis_hash
    } else if recovery_anchor_height == last_execution_height {
        last_execution_hash
    } else {
        node.provider
            .block_hash(recovery_anchor_height)
            .map_err(|error| {
                eyre::eyre!(
                    "failed to read recovery-anchor block hash at height \
                     {recovery_anchor_height}: {error}"
                )
            })?
            .ok_or_else(|| {
                eyre::eyre!(
                    "missing canonical block hash for recovery anchor at height \
                     {recovery_anchor_height}"
                )
            })?
    };

    // ── 10. Create executor actor (state-aware init) ────────────────────
    let (mut executor_actor, executor_mailbox) = ExecutorActor::new(
        ctx.child("executor"),
        engine_handle.clone(),
        genesis_hash,
        recovery_anchor_height,
        recovery_anchor_hash,
        projection_readiness.clone(),
        Some(execution_finalized_height_tx.clone()),
    );

    // ── 12. Create application actor and handler ────────────────────────
    let (application, application_rx) =
        OutbeApplication::new(config::ENGINE_MAILBOX_SIZE, marshal_mailbox.clone());

    // ── 12d. Conditional bootstrap validation data ─────────────────────
    // Determined AFTER marshal init so we can use both execution height
    // and consensus processed height. Prevents false fresh-bootstrap on
    // crash restart where Reth lost in-memory state (SIGKILL/OOM) but
    // consensus layer persisted progress durably.
    let is_fresh_bootstrap = last_execution_height == 0 && last_consensus_finalized.get() == 0;

    if last_execution_height == 0 && last_consensus_finalized.get() > 0 {
        info!(
            consensus_height = last_consensus_finalized.get(),
            "crash recovery detected — execution lost but consensus durable, will backfill"
        );
    }

    if is_fresh_bootstrap {
        use outbe_primitives::consensus::{GenesisValidator, GenesisValidators};

        let genesis_vals: Vec<GenesisValidator> = validator_set
            .addresses
            .iter()
            .zip(validator_set.public_keys.iter())
            .map(|(addr, pk)| {
                let pk_bytes = commonware_codec::Encode::encode(pk);
                let mut pubkey = [0u8; 48];
                let len = pk_bytes.len().min(48);
                pubkey[..len].copy_from_slice(&pk_bytes[..len]);
                GenesisValidator {
                    address: *addr,
                    consensus_pubkey: pubkey,
                }
            })
            .collect();

        bridge.set_genesis_validators(GenesisValidators {
            validators: genesis_vals,
            epoch_length_blocks,
        });

        let bootstrap_output = last_dkg_output.as_ref().ok_or_else(|| {
            eyre::eyre!(
                "fresh bootstrap requires full DKG output; public polynomial alone cannot build canonical boundary"
            )
        })?;
        validate_dkg_output_players_exact(bootstrap_output, &validator_set)
            .wrap_err("fresh bootstrap DKG output does not cover the genesis validator set")?;
        let bootstrap_artifact =
            dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
                epoch: Epoch::new(0),
                validator_set: &validator_set,
                output: bootstrap_output,
                is_full_dkg: bootstrap_from_live_dkg,
                dkg_cycle: 0,
                freeze_height: 0,
                planned_activation_height: 0,
                vrf_material_version,
                is_validator_set_change: true,
                tee_reshare_registrations: Vec::new(),
            })?;
        info!(
            vrf_material_version,
            vrf_group_public_key = %bootstrap_artifact.vrf_group_public_key,
            target_set_hash = %bootstrap_artifact.target_set_hash,
            active_set_hash = %bootstrap_artifact.reshare.active_set_hash,
            "genesis DKG boundary artifact queued; VRF active from view 2"
        );
        dkg_manager.note_bootstrap_outcome(bootstrap_artifact);
        info!("fresh bootstrap — genesis validators validation data queued");
    } else {
        info!(
            last_execution_height,
            last_consensus_finalized = last_consensus_finalized.get(),
            "ordinary restart — skipping genesis seeding"
        );
    }

    // Create broadcast engine for block dissemination.
    let (broadcast_engine, broadcast_mailbox) = commonware_broadcast::buffered::Engine::new(
        ctx.child("broadcast"),
        commonware_broadcast::buffered::Config {
            public_key: signing_key.public_key(),
            mailbox_size: nonzero_usize(config::ENGINE_MAILBOX_SIZE, "ENGINE_MAILBOX_SIZE")?,
            deque_size: config::BROADCAST_DEQUE_SIZE,
            peer_provider: oracle.clone(),
            priority: true,
            codec_config: (),
        },
    );

    // Initialize resolver for marshal.
    let resolver = marshal::resolver::p2p::init(
        ctx.child("marshal_resolver"),
        marshal::resolver::p2p::Config {
            public_key: signing_key.public_key(),
            peer_provider: oracle.clone(),
            blocker: oracle.clone(),
            mailbox_size: nonzero_usize(config::ENGINE_MAILBOX_SIZE, "ENGINE_MAILBOX_SIZE")?,
            initial: std::time::Duration::from_secs(1),
            timeout: std::time::Duration::from_secs(2),
            fetch_retry_timeout: std::time::Duration::from_millis(100),
            priority_requests: false,
            priority_responses: false,
        },
        marshal_channel,
    );

    // Start marshal actor with a composite reporter (tempo-style).
    // Marshal delivers finalized blocks to executor via Reporter trait and
    // publishes finalized tips to provider-readiness/watchdog consumers.
    // Executor acknowledges after successful EL processing, which gates
    // marshal's processed height — the recovery truth on restart.
    let (peer_manager_actor, peer_manager_mailbox) = crate::peer_manager::Actor::new(
        ctx.child("peer_manager"),
        crate::peer_manager::Config {
            oracle: oracle.clone(),
            node: node.clone(),
            executor: executor_mailbox.clone(),
            bootnode_map: bootnode_map.clone(),
            initial_peers: initial_peer_map,
        },
    );
    let mut peer_manager_handle_task = peer_manager_actor.start();

    let marshal_reporter =
        crate::marshal_update_reporter::MarshalUpdateReporter::new(executor_mailbox.clone())
            .add_tip_consumer(consensus_tip_tx.clone())
            .add_block_consumer(peer_manager_mailbox.clone());
    let mut marshal_handle =
        marshal_actor.start(marshal_reporter, broadcast_mailbox.clone(), resolver);

    // Serve `outbe_getFinalization` from the marshal so `--upstream` followers
    // can backfill + verify finalized blocks from this validator.
    spawn_finalization_drainer(ctx, marshal_mailbox.clone(), bridge.clone());

    let (recovery_anchor_height, recovery_anchor_hash, recovered_finalized_round) =
        match recover_application_finalized_round(ctx, &marshal_mailbox, last_execution_height)
            .await
        {
            Ok(recovered) => reconcile_recovered_execution_head(
                last_execution_height,
                last_execution_hash,
                recovered,
            )?,
            Err(error) if args.force_dkg && last_execution_height > 0 => {
                return Err(error).wrap_err(
                    "--testnet.force-dkg existing-chain recovery requires durable marshal \
                 finalization history; restore validator-N/data/consensus/outbe-* \
                 instead of deleting consensus archives",
                );
            }
            Err(error) if startup_live_join_completed || args.trust_el_head => {
                warn!(
                    %error,
                    last_execution_height,
                    "marshal archive lacks finalized-round history after startup live-join or force-dkg; continuing from synced execution boundary"
                );
                (recovery_anchor_height, recovery_anchor_hash, None)
            }
            Err(head_error) => {
                // reth's canonical head can lead consensus finalization by the
                // in-flight block: one this node proposed and applied as its head
                // but had not finalized when it stopped (steady state:
                // head_height = finalized_height + 1). On a plain restart in that
                // window the head's finalization legitimately does not exist yet —
                // a normal unfinalized head, NOT archive corruption. Confirm the
                // marshal still holds its own finalized tip's record (a gap *there*
                // is genuine corruption) and that the head leads by a bounded
                // amount, then continue from marshal's durable finalized boundary.
                // The speculative Reth head remains available locally, but neither
                // ExecutorActor nor FinalizationView may call it finalized. The
                // network re-finalizes forward and Reth reorgs via forkchoice if a
                // different block wins the first unfinalized height.
                let finalized_tip = last_consensus_finalized.get();
                if !unfinalized_head_lead_is_recoverable(last_execution_height, finalized_tip) {
                    return Err(head_error);
                }
                let Ok(recovered_finalization) =
                    recover_application_finalized_round(ctx, &marshal_mailbox, finalized_tip).await
                else {
                    return Err(head_error);
                };
                let (certified_height, certified_hash, recovered_round) =
                    reconcile_recovered_execution_head(
                        finalized_tip,
                        recovery_anchor_hash,
                        recovered_finalization,
                    )
                    .wrap_err(
                        "marshal finalized-tip record disagrees with canonical execution history",
                    )?;
                warn!(
                    last_execution_height,
                    finalized_tip,
                    head_lead = last_execution_height.saturating_sub(finalized_tip),
                    recovery_anchor_hash = %recovery_anchor_hash,
                    "execution head leads the marshal finalized tip on restart; anchoring \
                     recovery at certified finality (unfinalized head re-finalized forward)"
                );
                (certified_height, certified_hash, recovered_round)
            }
        };

    executor_actor = executor_actor.with_recovered_finalized_state(
        genesis_hash,
        recovery_anchor_height,
        recovery_anchor_hash,
    );

    // Start broadcast engine with P2P channel.
    let _broadcast_handle = broadcast_engine.start(broadcast_channel);

    let application_epoch_fence = ApplicationEpochFence::new(Epoch::new(recovered_epoch));

    // ── Half B step 21: build the shared finalization view + block
    // cache BEFORE constructing the application handler. Both the
    // application handler (`build_block` reads `prev_randao` /
    // `last_timestamp_millis`; proposer inserts into `block_cache`) and
    // the FinalizationActor (sole writer for the view; evicts entries
    // below the new finalized height from `block_cache`) hold the same
    // `Arc`s. Recovery state is seeded into the view here.
    let finalization_view = new_finalization_view(
        recovery_anchor_hash,
        recovery_anchor_height,
        recovered_finalized_round,
    );
    let finalization_block_cache = BlockCache::new();

    // Construct the consensus-owned exact-parent certificate handoff store
    // before either the application handler (consumer-side waiter) or the
    // FinalizationActor (single writer) so both can clone from the same durable
    // backing.
    let parent_cert_dir = args
        .storage_dir
        .as_ref()
        .ok_or_else(|| eyre::eyre!("consensus storage_dir must be set before stack startup"))?
        .join("finalized_parent_certs");
    let finalized_parent_cert_store =
        outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore::open(
            &parent_cert_dir,
        )
        .wrap_err_with(|| {
            format!(
                "failed to open finalized parent certificate store at {}",
                parent_cert_dir.display()
            )
        })?;

    // Defensive startup hygiene: a crash between persisting a finalization parent
    // record and advancing the finalization view can leave an ahead-of-view
    // record on disk. Drop any finalization record above the recovered finalized
    // height so the store never retains a height the view has not reached.
    let pruned_ahead = finalized_parent_cert_store
        .prune_above_height(recovery_anchor_height)
        .wrap_err("failed to prune ahead-of-view finalized parent certificate records")?;
    if pruned_ahead > 0 {
        tracing::info!(
            pruned_ahead,
            recovered_finalized_height = recovery_anchor_height,
            "dropped ahead-of-recovered-view finalization parent records at startup"
        );
    }

    // Resolve consensus-sync block timings from genesis (timing.rs fallbacks,
    // no CLI override) once, before the handler ctor and the epoch loop.
    let bt = block_timing_from_genesis(&node)?;

    // one process-local late-finalize signature store shared by the
    // application handler (packs the proposer artifact), the FinalizationActor
    // (resolves views → block numbers), and every per-epoch OutbeReporter
    // (records observed individual finalize votes). Best-effort, never consensus
    // state — the resulting artifact is re-verified pre-exec on every node.
    let late_sig_store = outbe_consensus::finalization::late_sig_store::shared(
        outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K,
    );

    // Create application handler with marshal mailbox and shared finalization state.
    let application_handler = ApplicationHandler::new(ApplicationDeps {
        rx: application_rx,
        engine: engine_handle,
        payload_builder,
        executor_mailbox,
        genesis_hash,
        validators: validator_set.clone(),
        chain_id: node.chain_spec().chain().id(),
        marshal_mailbox: marshal_mailbox.clone(),
        certificate_scheme_provider: certificate_scheme_provider.clone(),
        elector_config_provider: elector_config_provider.clone(),
        committee_provider: committee_provider.clone(),
        dkg_manager: dkg_manager.clone(),
        vrf_safety: vrf_safety.clone(),
        epoch_fence: application_epoch_fence.clone(),
        ancestry_readiness: ancestry_readiness.clone(),
        projection_readiness,
        finalization_view: finalization_view.clone(),
        block_cache: finalization_block_cache.clone(),
        finalization_selector: outbe_consensus::finalization::selection::ParentProofSelector::new(
            finalized_parent_cert_store.clone(),
        ),
        payload_resolve_time: std::time::Duration::from_millis(args.payload_resolve_time_ms),
        min_block_time: bt.min_block_time,
        proposer_evm_address,
        trust_el_head: args.trust_el_head,
        late_sig_store: late_sig_store.clone(),
    });

    info!(
        last_execution_height,
        last_consensus_finalized = last_consensus_finalized.get(),
        "starting executor with recovery state"
    );

    // ── 13. Spawn persistent actors (survive engine restarts) ───────────

    // Half B step 21: construct FinalizationActor + matching mailbox.
    // After step 21 the actor IS the production finalization path: the
    // OutbeReporter (constructed below per-epoch) sends finalizations
    // through `finalization_mailbox.notify_finalized` and the actor
    // owns all bridge / DKG / view-update side effects.
    //
    // Hand a clone of the exact-parent certificate store to the actor. The actor
    // is the only writer; the application handler reads hash-exact records via
    // the `ParentProofSelector` constructed above.
    let (finalization_actor, finalization_mailbox) =
        FinalizationActor::new(FinalizationActorDeps {
            view: finalization_view.clone(),
            block_cache: finalization_block_cache.clone(),
            marshal_mailbox: Some(marshal_mailbox.clone()),
            bridge: Some(bridge.clone()),
            dkg_manager: dkg_manager.clone(),
            vrf_safety: vrf_safety.clone(),
            parent_cert_store: finalized_parent_cert_store.clone(),
            certificate_scheme_provider: certificate_scheme_provider.clone(),
            late_sig_store: late_sig_store.clone(),
        });
    let mut finalization_handle = ctx
        .child("finalization")
        .spawn(move |ctx| finalization_actor.run(ctx));

    // persistent off-thread finalize-vote verifier. Each per-epoch
    // OutbeReporter enqueues raw finalize votes here instead of verifying
    // O(committee) BLS pairings inline on the Simplex voter task; the actor
    // resolves each vote's committee scheme by epoch through the shared
    // `certificate_scheme_provider` and admits only verified votes to
    // `late_sig_store`.
    let (finalize_verify_actor, finalize_verify_mailbox) =
        outbe_consensus::finalization::finalize_verify::FinalizeVerifyActor::new(
            certificate_scheme_provider.clone(),
            late_sig_store.clone(),
        );
    // Best-effort actor: its exit is non-fatal (consensus continues; only late
    // credits stop), so it is held for the engine's lifetime but not polled in
    // the fatal-exit select below. The named `_`-binding keeps the task alive
    // (a bare `_` would drop and abort it immediately).
    let _finalize_verify_handle = ctx
        .child("finalize_verify")
        .spawn(move |_ctx| finalize_verify_actor.run());

    let mut executor_handle_task = executor_actor
        .with_ancestry_readiness(ancestry_readiness.clone())
        .with_finalized_ce_committer(finalized_ce_committer)
        .start(marshal_mailbox.clone(), last_consensus_finalized);
    let mut handler_handle = ctx
        .child("application")
        .spawn(move |ctx| application_handler.run(ctx));

    info!("consensus actors and marshal block availability started");

    // ═══════════════════════════════════════════════════════════════════
    // EPOCH LOOP — manages engine lifecycle and reshare triggering.
    // Each iteration creates a new Simplex engine with epoch-scoped
    // sub-channels. The engine is aborted when a reshare completes,
    // and a new engine starts at the next epoch.
    // ═══════════════════════════════════════════════════════════════════
    let mut current_epoch = Epoch::new(recovered_epoch);
    let reporter_continuity = ReporterContinuity::default();

    // Channel for receiving DKG reshare results from background tasks.
    let (dkg_result_tx, mut dkg_result_rx) =
        tokio::sync::mpsc::unbounded_channel::<Result<DkgTaskOutcome>>();
    let (dkg_progress_tx, mut dkg_progress_rx) =
        tokio::sync::mpsc::unbounded_channel::<dkg_actor::DkgProgress>();
    let mut reshare_in_progress = false;
    // Track the next oracle round (0 was used for the initial startup set).
    let mut frozen_dkg_target: Option<FrozenDkgTarget> = None;
    let mut pending_dkg_activation: Option<PendingDkgActivation> = None;
    let mut dealer_only_dkg_activation: Option<DealerOnlyDkgActivation> = None;
    // Finalized-follower: a share-less verifier (TEE full-node) never runs a DKG
    // ceremony, so it never sets `pending_dkg_activation`. Instead it notes the
    // upcoming activation height here (set at the freeze when it classifies as
    // NotParticipant) and, when it syncs to that height, advances its epoch +
    // restarts the engine — reusing its polynomial (the group key is preserved
    // across reshares) so its per-epoch reporter keeps verifying finality.
    let mut pending_verifier_activation: Option<u64> = None;
    // Deferred follow-up to a verifier activation: the activated cycle's
    // `BoundaryOutcome` rides the first new-epoch block, which this follower
    // can only execute AFTER its engine restarts, so the activation block
    // queues the adoption here and the height-event path completes it.
    let mut pending_verifier_boundary_adoption: Option<PendingVerifierBoundaryAdoption> = None;
    let mut latest_consensus_tip = *consensus_tip_rx.borrow();
    let mut pending_provider_ready_height: Option<u64> = None;
    let mut watchdog_unhealthy_since: Option<SystemTime> = None;
    let watchdog_started_at = ctx.current();
    let mut provider_ready_retry_timer: Pin<Box<dyn Future<Output = ()> + Send>> =
        Box::pin(std::future::pending());
    let mut execution_watchdog_timer: Pin<Box<dyn Future<Output = ()> + Send>> =
        Box::pin(ctx.sleep(config::EXECUTION_WATCHDOG_INTERVAL));
    let mut retry_frozen_dkg = false;
    info!(
        epoch_length_blocks = dkg_rotation_params.epoch_length_blocks,
        prepare_window_blocks = dkg_rotation_params.prepare_window_blocks,
        activation_grace_blocks = dkg_rotation_params.activation_grace_blocks,
        "configured block-based DKG/VRF rotation"
    );
    info!(
        min_block_time = ?bt.min_block_time,
        leader_timeout = ?bt.leader_timeout,
        certification_timeout = ?bt.certification_timeout,
        "consensus timeouts (genesis-sourced, no CLI override)"
    );
    'epoch_loop: loop {
        // ── a. Register or take pre-registered epoch sub-channels ───────
        // Activation pre-registers `next_epoch_subchannels` at DKG
        // completion (see DKG completion handler below); the top of the
        // next iteration consumes it. The fallback path covers the
        // genesis-bootstrap iteration where no prior DKG completion has
        // run.
        let outbe_consensus::epoch_subchannels::EpochSubchannels {
            vote, cert, res, ..
        } = outbe_consensus::epoch_subchannels::take_or_register_current(
            current_epoch,
            &mut next_epoch_subchannels,
            &mut vote_mux,
            &mut cert_mux,
            &mut res_mux,
        )
        .await?;

        // ── b. Build HybridScheme for this epoch ────────────────────────
        use commonware_consensus::simplex::elector::Config as ElectorConfig;
        let scheme = if signing_share.is_some() {
            HybridScheme::<MinSig>::signer_with_vrf_provider(
                &config::outbe_app_namespace(),
                participants.clone(),
                signing_key.clone(),
                vrf_materials.clone(),
            )
            .ok_or_else(|| {
                eyre::eyre!(
                    "signing key or BLS share invalid for validator set (epoch {current_epoch})"
                )
            })?
        } else {
            // Verifier mode (no threshold share this epoch): the engine follows and
            // verifies finalized blocks — driving its execution layer to sync — but
            // cannot propose or sign. `me()` is None, so the simplex engine never
            // invokes signing. The node acquires a share at the next reshare, after
            // which the next epoch iteration rebuilds this scheme as a signer (Stage 4).
            info!(
                epoch = %current_epoch,
                "no threshold share for this epoch — running consensus engine in VERIFIER mode"
            );
            HybridScheme::<MinSig>::verifier_with_vrf_provider(
                &config::outbe_app_namespace(),
                participants.clone(),
                vrf_materials.clone(),
            )
            .ok_or_else(|| {
                eyre::eyre!(
                    "verifier scheme invalid for validator set (epoch {current_epoch}): \
                     polynomial total ({}) must equal participant count ({})",
                    vrf_materials.active_polynomial_total().unwrap_or(0),
                    participants.len(),
                )
            })?
        };

        // ── c. Create reporter for this epoch ───────────────────────────
        let recovered_boundary_for_epoch =
            recovered_boundary_artifact.filter(|artifact| artifact.epoch == current_epoch.get());
        let (verifier_scheme, ordered_addresses) = epoch_validation_inputs(
            current_epoch,
            &participants,
            &validator_set,
            recovered_boundary_for_epoch,
            &vrf_materials,
        )?;

        let elector_config =
            epoch_elector_config(current_epoch, &reporter_continuity, vrf_materials.clone())?;
        let reporter_elector = elector_config.clone().build(&participants);

        let _ = certificate_scheme_provider.register(current_epoch, verifier_scheme.clone());
        let _ = elector_config_provider.register(current_epoch, elector_config.clone());
        let _ = committee_provider.register(current_epoch, ordered_addresses.clone());

        let outbe_reporter = OutbeReporter::new(
            reporter_continuity.clone(),
            ordered_addresses,
            finalization_mailbox.clone(),
            Some(bridge.clone()),
            verifier_scheme,
            reporter_elector,
            current_epoch,
            std::sync::Arc::new(finalized_parent_cert_store.clone()),
            finalize_verify_mailbox.clone(),
        );

        // Combine OutbeReporter + marshal mailbox as a joint Simplex reporter
        // (tempo-style). Both receive Activity events including Finalization:
        // - OutbeReporter: bridge/VRF/missed-proposer processing AND
        // `Activity::Certification` → CertifiedParentProofStore.
        // - Marshal: finalized block delivery → executor → ack → recovery truth.
        //   Marshal's mailbox drops Certification via its `_ => return;` arm
        //   (monorepo `consensus/src/marshal/core/mailbox.rs:396-410`), so
        //   ordering between Outbe and marshal does not need to be sequential;
        //   `Reporters::from((outbe, marshal))` runs both via `futures::join!`
        //   and Outbe is the sole persistent consumer of Certification.
        let combined_reporter = Reporters::from((outbe_reporter, marshal_mailbox.clone()));

        // ── d. Resolve the Simplex genesis floor ─────────────────────────
        // commonware 2026.5.0 removed `Automaton::genesis(epoch)`; the
        // genesis anchor is now an explicit `simplex::Config.floor`. We must
        // feed the byte-identical value the old `handle_genesis(epoch)`
        // returned:
        //   * epoch 0  → the chain genesis block hash (`Digest(genesis_hash)`),
        //     the parent of `view = 1` for the bootstrap engine.
        //   * epoch > 0 → the canonical last-finalized block's hash (the
        //     continuity anchor read from `FinalizationView`), the parent of
        //     `view = 1` for the restarted engine.
        // We use `Floor::Genesis(digest)` in both cases (never
        // `Floor::Finalized`) so behaviour matches the prior synthetic
        // `parent_view = 0` resolution path.
        //
        // The bounded-wait guard below preserves the prior epoch-restart
        // invariant: for `epoch > 0` we must not start the engine until the
        // FinalizationActor has published the boundary block's anchor, or the
        // floor (and Phase 1 finalized-round proof key) would be missing. The
        // 5s deadline accommodates transient races between the
        // FinalizationActor and the DKG-manager-driven epoch advance.
        let floor_digest = if current_epoch.get() == 0 {
            Digest(genesis_hash)
        } else {
            let deadline = ctx.current() + EPOCH_RESTART_ANCHOR_TIMEOUT;
            loop {
                let (height, hash, round_ready) = {
                    let view = finalization_view.read();
                    (
                        view.last_finalized_number,
                        view.forkchoice.finalized_block_hash,
                        view.last_finalized_round.is_some(),
                    )
                };
                if height > 0 && hash != alloy_primitives::B256::ZERO && round_ready {
                    break Digest(hash);
                }
                if ctx.current() >= deadline {
                    return Err(eyre::eyre!(
                        "epoch={} restart without finalized anchor after {:?}; \
                         handle_genesis would return ZERO, or Phase 1 would lack \
                         the finalized-round proof key for parent_view=0",
                        current_epoch.get(),
                        EPOCH_RESTART_ANCHOR_TIMEOUT,
                    ));
                }
                ctx.sleep(EPOCH_RESTART_ANCHOR_POLL_INTERVAL).await;
            }
        };

        // ── e. Build engine config ──────────────────────────────────────
        let simplex_cfg = simplex::Config {
            scheme,
            elector: elector_config,
            blocker: oracle.clone(),
            automaton: application.clone(),
            relay: application.clone(),
            forwarding: simplex::ForwardingPolicy::Disabled,
            reporter: combined_reporter,
            strategy: commonware_parallel::Sequential,
            partition: format!("outbe-simplex-{}", current_epoch),
            mailbox_size: nonzero_usize(config::ENGINE_MAILBOX_SIZE, "ENGINE_MAILBOX_SIZE")?,
            epoch: current_epoch,
            floor: simplex::Floor::Genesis(floor_digest),
            replay_buffer: nonzero_usize(config::REPLAY_BUFFER, "REPLAY_BUFFER")?,
            write_buffer: nonzero_usize(config::WRITE_BUFFER, "WRITE_BUFFER")?,
            page_cache: page_cache.clone(),
            leader_timeout: bt.leader_timeout,
            certification_timeout: bt.certification_timeout,
            timeout_retry: config::DEFAULT_NULLIFY_REBROADCAST,
            activity_timeout: ViewDelta::new(u64::from(config::ACTIVITY_TIMEOUT)),
            skip_timeout: ViewDelta::new(u64::from(config::SKIP_TIMEOUT)),
            fetch_timeout: config::DEFAULT_PEER_RESPONSE_TIMEOUT,
            fetch_concurrent: nonzero_usize(config::FETCH_CONCURRENT, "FETCH_CONCURRENT")?,
        };

        // ── f. Start engine ─────────────────────────────────────────────
        let engine = simplex::Engine::new(
            ctx.child("engine").with_attribute("epoch", current_epoch),
            simplex_cfg,
        );
        let mut engine_handle_task = engine.start(vote, cert, res);

        info!(epoch = %current_epoch, "simplex engine started — blocks can now be produced");

        // ── g. Engine event loop ────────────────────────────────────────
        // Monitors engine, component exits, and block-height-driven reshare triggers.

        loop {
            tokio::select! {
                biased;

                // Network exit → fatal.
                _ = &mut network_handle => {
                    info!("P2P network exited");
                    break 'epoch_loop;
                }

                // Engine exit → clean shutdown.
                _ = &mut engine_handle_task => {
                    info!(epoch = %current_epoch, "simplex engine exited");
                    break 'epoch_loop;
                }

                // DKG reshare completed (from background task).
                Some(dkg_result) = dkg_result_rx.recv() => {
                    reshare_in_progress = false;
                    match dkg_result {
                        Ok(DkgTaskOutcome::Complete(dkg_complete)) => {
                            let Some(target) = frozen_dkg_target.take() else {
                                warn!("DKG completed without a frozen target; ignoring stale outcome");
                                outbe_consensus::metrics::record_dkg_status(0);
                                continue;
                            };

                            let current_height =
                                node.provider.last_block_number().map_err(|error| {
                                    eyre::eyre!(
                                        "failed to read latest block height after DKG completion: {error}"
                                    )
                                })?;
                            match pending_dkg_activation_decision(
                                current_height,
                                target.planned_activation_height,
                                dkg_rotation_params.activation_grace_blocks,
                            ) {
                                PendingDkgActivationDecision::Expired { deadline } => {
                                    vrf_safety.mark_expired(current_height);
                                    publish_randomness_status(&bridge, &vrf_safety);
                                    return Err(eyre::eyre!(
                                        "DKG completed after VRF expiry: cycle {}, height {}, deadline {}",
                                        target.dkg_cycle,
                                        current_height,
                                        deadline
                                    ));
                                }
                                PendingDkgActivationDecision::Wait
                                | PendingDkgActivationDecision::Activate => {}
                            }

                            if let Some(ref keys_dir) = args.keys_dir {
                                persist_completed_dkg_before_activation(
                                    keys_dir,
                                    &key_backend,
                                    current_epoch,
                                    vrf_material_version,
                                    &participants,
                                    &target,
                                    &dkg_complete,
                                    current_height,
                                )?;
                            }

                            info!(
                                epoch = %current_epoch,
                                dkg_cycle = target.dkg_cycle,
                                is_validator_set_change = target.is_validator_set_change,
                                planned_activation_height = target.planned_activation_height,
                                current_height,
                                "DKG completed; waiting for activation height"
                            );
                            outbe_consensus::metrics::record_dkg_status(2); // completed
                            outbe_consensus::metrics::record_reshare_completed();
                            if current_height > target.planned_activation_height {
                                vrf_safety.note_grace(
                                    target.planned_activation_height,
                                    dkg_rotation_params.activation_grace_blocks,
                                );
                            } else {
                                vrf_safety.note_pending_activation(
                                    target.planned_activation_height,
                                    dkg_rotation_params.activation_grace_blocks,
                                );
                            }
                            publish_randomness_status(&bridge, &vrf_safety);

                            // Pre-register vote/cert/res sub-channels for
                            // the upcoming epoch BEFORE stashing the pending
                            // activation and BEFORE the
                            // `execution_finalized_height_tx.send(...)`
                            // call that may immediately wake the activation
                            // branch (when `should_activate_now` is true).
                            // This closes the cross-node race where a
                            // faster peer can begin broadcasting epoch-N+1
                            // traffic before this node has registered the
                            // matching sub-channel on its Mux. See
                            // `epoch_subchannels::register_epoch_subchannels`.
                            //
                            // Fail-fast: a Mux-level error
                            // (AlreadyRegistered, closed Mux) on a
                            // consensus-critical channel is a hard fault.
                            // No silent fallback to lazy registration.
                            let next_epoch =
                                next_consensus_epoch_after_dkg_activation(current_epoch);
                            if next_epoch_subchannels.is_some() {
                                warn!(
                                    epoch = %next_epoch,
                                    "stale DKG completion arrived after next-epoch subchannels were already pre-registered; ignoring"
                                );
                                continue;
                            }
                            next_epoch_subchannels = Some(
                                outbe_consensus::epoch_subchannels::register_epoch_subchannels(
                                    next_epoch,
                                    &mut vote_mux,
                                    &mut cert_mux,
                                    &mut res_mux,
                                )
                                .await
                                .wrap_err_with(|| {
                                    format!(
                                        "pre-register next-epoch subchannels at DKG \
                                         completion for epoch {next_epoch}"
                                    )
                                })?,
                            );

                            let should_activate_now = current_height >= target.planned_activation_height;
                            pending_dkg_activation = Some(PendingDkgActivation {
                                target,
                                complete: dkg_complete,
                                completed_at_height: current_height,
                            });
                            if should_activate_now {
                                let _ = execution_finalized_height_tx.send(current_height);
                            }
                        }
                        Ok(DkgTaskOutcome::DealerOnly(dealer_only_complete)) => {
                            let Some(target) = frozen_dkg_target.as_ref().cloned() else {
                                warn!("dealer-only DKG completed without a frozen target; ignoring stale outcome");
                                outbe_consensus::metrics::record_dkg_status(0);
                                continue;
                            };
                            if dealer_only_complete.participants != target.participants {
                                return Err(eyre::eyre!(
                                    "dealer-only DKG participant set does not match frozen target"
                                ));
                            }

                            let current_height =
                                node.provider.last_block_number().map_err(|error| {
                                    eyre::eyre!(
                                        "failed to read latest block height after dealer-only DKG completion: {error}"
                                    )
                                })?;
                            info!(
                                epoch = %current_epoch,
                                dkg_cycle = target.dkg_cycle,
                                planned_activation_height = target.planned_activation_height,
                                current_height,
                                "dealer-only DKG completed; remaining in old validator set until activation"
                            );
                            dealer_only_dkg_activation = Some(DealerOnlyDkgActivation {
                                target,
                                completed_at_height: current_height,
                            });
                            outbe_consensus::metrics::record_dkg_status(2);
                            outbe_consensus::metrics::record_reshare_completed();
                            let _ = execution_finalized_height_tx.send(current_height);
                        }
                        Err(e) => {
                            warn!(?e, "DKG reshare failed, retrying frozen target on next check");
                            retry_frozen_dkg = true;
                        }
                    }
                }

                Some(progress) = dkg_progress_rx.recv() => {
                    match progress {
                        dkg_actor::DkgProgress::LocalDealerLog(bytes) => {
                            if let Err(error) = dkg_manager.note_local_dealer_log(current_epoch, bytes) {
                                warn!(%error, epoch = %current_epoch, "failed recording local dealer log");
                            }
                        }
                        dkg_actor::DkgProgress::P2pDealerLog(bytes) => {
                            if let Err(error) = dkg_manager.note_pending_dealer_log(current_epoch, bytes) {
                                warn!(%error, epoch = %current_epoch, "failed recording P2P dealer log candidate");
                            }
                        }
                    }
                }

                _ = &mut execution_watchdog_timer => {
                    execution_watchdog_timer =
                        Box::pin(ctx.sleep(config::EXECUTION_WATCHDOG_INTERVAL));
                    let Some(tip) = latest_consensus_tip else {
                        debug!("execution watchdog waiting for first consensus tip");
                        continue;
                    };

                    let consensus_tip_height = tip.height.get();
                    let mut reth_head_height = match node.provider.last_block_number() {
                        Ok(height) => height,
                        Err(error) => {
                            let now = ctx.current();
                            let (decision, next_unhealthy_since) = execution_watchdog_decision(
                                ExecutionWatchdogObservation::ProviderReadError,
                                now,
                                watchdog_started_at,
                                watchdog_unhealthy_since,
                            );
                            watchdog_unhealthy_since = next_unhealthy_since;
                            match decision {
                                ExecutionWatchdogDecision::StartupGrace => {
                                    let startup_elapsed = elapsed_since(now, watchdog_started_at);
                                    warn!(
                                        %error,
                                        startup_elapsed_ms = startup_elapsed.as_millis(),
                                        startup_grace_sec = config::EXECUTION_WATCHDOG_STARTUP_GRACE_SEC,
                                        "execution watchdog failed to read Reth provider head during startup/backfill grace"
                                    );
                                }
                                ExecutionWatchdogDecision::Unhealthy { unhealthy_for } => {
                                    warn!(
                                        %error,
                                        unhealthy_for_ms = unhealthy_for.as_millis(),
                                        "execution watchdog failed to read Reth provider head"
                                    );
                                }
                                ExecutionWatchdogDecision::Fatal { unhealthy_for } => {
                                    return Err(eyre::eyre!(
                                        "execution watchdog provider read failed for {:?}: {error}",
                                        unhealthy_for
                                    ));
                                }
                                ExecutionWatchdogDecision::Healthy => {}
                            }
                            continue;
                        }
                    };
                    let provider_tip_hash = match node.provider.block_hash(consensus_tip_height) {
                        Ok(hash) => hash,
                        Err(error) => {
                            let now = ctx.current();
                            let (decision, next_unhealthy_since) = execution_watchdog_decision(
                                ExecutionWatchdogObservation::ProviderReadError,
                                now,
                                watchdog_started_at,
                                watchdog_unhealthy_since,
                            );
                            watchdog_unhealthy_since = next_unhealthy_since;
                            match decision {
                                ExecutionWatchdogDecision::StartupGrace => {
                                    let startup_elapsed = elapsed_since(now, watchdog_started_at);
                                    warn!(
                                        %error,
                                        consensus_tip_height,
                                        consensus_tip_digest = %tip.digest,
                                        reth_head_height,
                                        startup_elapsed_ms = startup_elapsed.as_millis(),
                                        startup_grace_sec = config::EXECUTION_WATCHDOG_STARTUP_GRACE_SEC,
                                        "execution watchdog failed to read Reth provider hash at consensus tip during startup/backfill grace"
                                    );
                                }
                                ExecutionWatchdogDecision::Unhealthy { unhealthy_for } => {
                                    warn!(
                                        %error,
                                        consensus_tip_height,
                                        consensus_tip_digest = %tip.digest,
                                        reth_head_height,
                                        unhealthy_for_ms = unhealthy_for.as_millis(),
                                        "execution watchdog failed to read Reth provider hash at consensus tip"
                                    );
                                }
                                ExecutionWatchdogDecision::Fatal { unhealthy_for } => {
                                    return Err(eyre::eyre!(
                                        "execution watchdog provider hash read failed at consensus tip height {} for {:?}: {error}",
                                        consensus_tip_height,
                                        unhealthy_for
                                    ));
                                }
                                ExecutionWatchdogDecision::Healthy => {}
                            }
                            continue;
                        }
                    };
                    let hash_match = provider_tip_hash == Some(tip.digest.0);
                    match node.provider.last_block_number() {
                        Ok(height) => {
                            reth_head_height = height;
                        }
                        Err(error) => {
                            warn!(
                                %error,
                                consensus_tip_height,
                                consensus_tip_digest = %tip.digest,
                                previous_reth_head_height = reth_head_height,
                                "execution watchdog failed to refresh Reth provider head after consensus tip hash probe; using previous height sample"
                            );
                        }
                    }
                    outbe_consensus::metrics::record_consensus_reth_state(
                        consensus_tip_height,
                        reth_head_height,
                        hash_match,
                    );

                    let consensus_ahead = consensus_tip_height.saturating_sub(reth_head_height);
                    let now = ctx.current();
                    let (decision, next_unhealthy_since) = execution_watchdog_decision(
                        ExecutionWatchdogObservation::ProviderState {
                            consensus_tip_height,
                            reth_head_height,
                            hash_match,
                        },
                        now,
                        watchdog_started_at,
                        watchdog_unhealthy_since,
                    );
                    watchdog_unhealthy_since = next_unhealthy_since;
                    match decision {
                        ExecutionWatchdogDecision::Healthy => {}
                        ExecutionWatchdogDecision::StartupGrace => {
                            let startup_elapsed = elapsed_since(now, watchdog_started_at);
                            warn!(
                                consensus_tip_height,
                                consensus_tip_digest = %tip.digest,
                                reth_head_height,
                                ?provider_tip_hash,
                                consensus_ahead,
                                hash_match,
                                startup_elapsed_ms = startup_elapsed.as_millis(),
                                startup_grace_sec = config::EXECUTION_WATCHDOG_STARTUP_GRACE_SEC,
                                "execution watchdog detected Reth provider behind consensus tip during startup/backfill grace"
                            );
                        }
                        ExecutionWatchdogDecision::Unhealthy { unhealthy_for } => {
                            warn!(
                                consensus_tip_height,
                                consensus_tip_digest = %tip.digest,
                                reth_head_height,
                                ?provider_tip_hash,
                                consensus_ahead,
                                hash_match,
                                unhealthy_for_ms = unhealthy_for.as_millis(),
                                "execution watchdog detected Reth provider behind consensus tip"
                            );
                        }
                        ExecutionWatchdogDecision::Fatal { unhealthy_for } => {
                            return Err(eyre::eyre!(
                                "execution watchdog fatal: Reth provider head/hash not ready for consensus tip height {} digest {} (reth_head={}, provider_tip_hash={:?}, unhealthy_for={:?})",
                                consensus_tip_height,
                                tip.digest,
                                reth_head_height,
                                provider_tip_hash,
                                unhealthy_for,
                            ));
                        }
                    }
                }

                consensus_tip_changed = consensus_tip_rx.changed() => {
                    match consensus_tip_changed {
                        Ok(()) => {
                            latest_consensus_tip = *consensus_tip_rx.borrow_and_update();
                            if let Some(current_height) = pending_provider_ready_height {
                                let _ = execution_finalized_height_tx.send(current_height);
                            }
                        }
                        Err(error) => {
                            warn!(%error, "consensus tip watch channel closed");
                        }
                    }
                }

                _ = &mut provider_ready_retry_timer, if pending_provider_ready_height.is_some() => {
                    provider_ready_retry_timer = Box::pin(std::future::pending());
                    if let Some(current_height) = pending_provider_ready_height {
                        let _ = execution_finalized_height_tx.send(current_height);
                    }
                }

                // Block-height based DKG/VRF rotation. This is driven by execution-finalized
                // height notifications after successful new_payload + FCU, not wall-clock
                // polling or raw consensus finalization.
                Some(current_height) = execution_finalized_height_rx.recv(), if !reshare_in_progress => {
                    match latest_consensus_tip {
                        Some(tip) => {
                            if !provider_matches_consensus_tip(&node.provider, tip, current_height)? {
                                pending_provider_ready_height = Some(current_height);
                                provider_ready_retry_timer =
                                    Box::pin(ctx.sleep(config::DEFAULT_PEER_RESPONSE_TIMEOUT));
                                debug!(
                                    current_height,
                                    consensus_tip_height = tip.height.get(),
                                    consensus_tip_digest = %tip.digest,
                                    "provider not ready for DKG/VRF scheduling; retrying"
                                );
                                continue;
                            }
                            pending_provider_ready_height = None;
                            provider_ready_retry_timer = Box::pin(std::future::pending());
                            if let Some(boundary) =
                                dkg_manager.take_committed_boundary_artifact().await
                            {
                                let boundary_output = decode_boundary_output(&boundary)
                                    .wrap_err("failed to decode finalized DKG boundary output")?;
                                if last_dkg_output.as_ref() != Some(&boundary_output) {
                                    let local_pk = signing_key.public_key();
                                    if boundary_output.players().position(&local_pk).is_none() {
                                        info!(
                                            dkg_output_hash = %dkg_manager::dkg_output_hash(&boundary_output),
                                            "finalized DKG boundary excludes local validator; exiting validator mode"
                                        );
                                        return Ok(());
                                    }
                                    return Err(eyre::eyre!(
                                        "finalized DKG boundary output does not match active local DKG output"
                                    ));
                                }
                                if let Some(ref keys_dir) = args.keys_dir {
                                    if let Some(share) = signing_share.as_ref() {
                                        save_dkg_state(
                                            keys_dir,
                                            share,
                                            &polynomial,
                                            &boundary_output,
                                            &key_backend,
                                        )
                                        .wrap_err(
                                            "failed to promote finalized DKG state to disk",
                                        )?;
                                        info!(
                                            keys_dir = %keys_dir.display(),
                                            dkg_output_hash = %dkg_manager::dkg_output_hash(&boundary_output),
                                            "promoted finalized DKG state to durable storage"
                                        );
                                    } else {
                                        info!(
                                            keys_dir = %keys_dir.display(),
                                            dkg_output_hash = %dkg_manager::dkg_output_hash(&boundary_output),
                                            "finalized DKG boundary adopted in verifier mode; no private share to promote"
                                        );
                                    }
                                    remove_pending_dkg_state(keys_dir);
                                    clear_pending_dkg_boundary(keys_dir);
                                }
                            }
                        }
                        None => {
                            pending_provider_ready_height = Some(current_height);
                            debug!(
                                current_height,
                                "no consensus tip available for DKG/VRF scheduling; retrying"
                            );
                            continue;
                        }
                    }

                    // Deferred verifier-follower boundary adoption: refresh this
                    // follower's DKG material from the boundary of the rotation it just
                    // followed, so its NEXT reshare presents what the committee holds.
                    // The ceremony id binds round + FULL previous output + participants,
                    // so a long-lived full-node that follows >=1 reshare then stakes
                    // must track BOTH the chain's current output (stale prev_output ->
                    // info_hash mismatch -> dealer bundles ignored) AND the chain's
                    // dkg_cycle (the follower skips the freeze-path `dkg_cycle`
                    // increment at its `continue`, so its round goes stale -> wrong mux
                    // sub-channel + dealer logs fail verification) -> ceremony timeout
                    // -> ACTIVE-but-voteless. Mirrors startup recovery (`dkg_cycle =
                    // artifact.dkg_cycle + 1`), which is why the restart path never had
                    // this gap. Group key is preserved across reshares, so finality
                    // (verified via the participant set) is unaffected while the
                    // adoption is pending.
                    if let Some(adoption) = pending_verifier_boundary_adoption {
                        if current_height >= adoption.next_scan_height {
                            match scan_first_boundary_after(
                                &node.provider,
                                adoption.next_scan_height,
                                current_height,
                            ) {
                                Ok(ForwardBoundaryScan::Found {
                                    commit_height,
                                    boundary: activated_boundary,
                                }) => {
                                    pending_verifier_boundary_adoption = None;
                                    if verifier_should_adopt_followed_boundary(
                                        commit_height,
                                        activated_boundary.planned_activation_height,
                                        adoption.activation_height,
                                    ) {
                                        match decode_boundary_output(&activated_boundary) {
                                            Ok(activated_output) => {
                                                vrf_material_version =
                                                    activated_boundary.vrf_material_version;
                                                dkg_cycle = activated_boundary
                                                    .dkg_cycle
                                                    .saturating_add(1);
                                                polynomial = activated_output.public().clone();
                                                vrf_materials.activate(
                                                    vrf_material_version,
                                                    polynomial.clone(),
                                                    None,
                                                );
                                                last_dkg_output = Some(activated_output);
                                                vrf_safety.note_activated(
                                                    vrf_material_version,
                                                    last_dkg_activation_height,
                                                    dkg_rotation_params
                                                        .planned_activation_height(
                                                            last_dkg_activation_height,
                                                        ),
                                                    dkg_rotation_params.activation_grace_blocks,
                                                );
                                                publish_randomness_status(&bridge, &vrf_safety);
                                                info!(
                                                    commit_height,
                                                    activation_height =
                                                        adoption.activation_height,
                                                    dkg_cycle,
                                                    vrf_material_version,
                                                    "verifier-follower: adopted activated DKG boundary output"
                                                );
                                            }
                                            Err(error) => warn!(
                                                %error,
                                                "verifier-follower: failed to decode activated boundary output; keeping prior DKG material"
                                            ),
                                        }
                                    } else {
                                        warn!(
                                            commit_height,
                                            boundary_planned_activation_height =
                                                activated_boundary.planned_activation_height,
                                            activation_height = adoption.activation_height,
                                            "verifier-follower: boundary found above the followed activation belongs to a stale cycle; keeping prior DKG material"
                                        );
                                    }
                                }
                                Ok(ForwardBoundaryScan::NotFound { resume_from }) => {
                                    // The first new-epoch block is not executed locally
                                    // yet — resume from the first unexamined height on
                                    // the next finalized height event.
                                    pending_verifier_boundary_adoption =
                                        Some(PendingVerifierBoundaryAdoption {
                                            activation_height: adoption.activation_height,
                                            next_scan_height: resume_from,
                                        });
                                }
                                Err(error) => warn!(
                                    %error,
                                    next_scan_height = adoption.next_scan_height,
                                    "verifier-follower: failed to scan for activated boundary; retrying on next finalized height"
                                ),
                            }
                        }
                    }

                    // Verifier-follower DKG activation: a share-less verifier that noted an
                    // upcoming rotation (above) advances its epoch once it has synced to the
                    // activation height. It does NOT run a ceremony and reuses its polynomial
                    // (the reshare preserves the group public key, so the per-epoch reporter
                    // built at the top of `'epoch_loop` keeps verifying the new epoch's
                    // finality); only `current_epoch` advances and the Simplex engine restarts.
                    if let Some(activation_height) = pending_verifier_activation {
                        if current_height >= activation_height {
                            let next_epoch =
                                next_consensus_epoch_after_dkg_activation(current_epoch);
                            info!(
                                epoch = %current_epoch,
                                next_epoch = %next_epoch,
                                activation_height,
                                "verifier-follower: DKG rotation activated — advancing epoch (reusing preserved group key) and restarting the Simplex engine"
                            );
                            pending_verifier_activation = None;
                            // Anchor on the OBSERVED height (mirror the participant path's
                            // `current_height.max(activation_height)`), not the planned height —
                            // `last_dkg_activation_height` is a local accumulator that drives the
                            // next freeze/activation schedule, so storing the bare planned value
                            // would let schedule skew accumulate vs the committee across cycles.
                            last_dkg_activation_height = current_height.max(activation_height);
                            application_epoch_fence.advance_epoch(next_epoch);
                            engine_handle_task.abort();
                            current_epoch = next_epoch;
                            // Same FinalizationActor anchor wait as the participant activation
                            // path: the restarted engine's floor needs the activation block
                            // finalized before `'epoch_loop` rebuilds the scheme.
                            let deadline =
                                ctx.current() + EPOCH_RESTART_ANCHOR_TIMEOUT;
                            loop {
                                let (finalized, finalized_hash, round_ready) = {
                                    let view = finalization_view.read();
                                    (
                                        view.last_finalized_number,
                                        view.forkchoice.finalized_block_hash,
                                        view.last_finalized_round.is_some(),
                                    )
                                };
                                if finalized >= activation_height
                                    && finalized_hash != alloy_primitives::B256::ZERO
                                    && round_ready
                                {
                                    break;
                                }
                                if ctx.current() >= deadline {
                                    return Err(eyre::eyre!(
                                        "verifier DKG activation race after {:?}: \
                                         finalized=(height={}, hash={}, round_ready={}) \
                                         activation_height={}",
                                        EPOCH_RESTART_ANCHOR_TIMEOUT,
                                        finalized,
                                        finalized_hash,
                                        round_ready,
                                        activation_height
                                    ));
                                }
                                ctx.sleep(EPOCH_RESTART_ANCHOR_POLL_INTERVAL).await;
                            }
                            // Queue adoption of the just-activated boundary's DKG
                            // material. It CANNOT be read here: the activated cycle's
                            // `BoundaryOutcome` rides the FIRST new-epoch block —
                            // strictly ABOVE the activation height — and this follower
                            // executes finalized blocks only while its (just aborted)
                            // engine runs, so waiting for the artifact in this block
                            // would deadlock against our own engine restart. The
                            // height-event path performs the scan + adoption once the
                            // restarted engine executes past the activation height.
                            pending_verifier_boundary_adoption =
                                Some(PendingVerifierBoundaryAdoption {
                                    activation_height,
                                    next_scan_height: activation_height.saturating_add(1),
                                });
                            continue 'epoch_loop;
                        }
                    }

                    if let Some(pending) = pending_dkg_activation.as_ref() {
                        let activation_height = current_height.max(pending.completed_at_height);
                        match pending_dkg_activation_decision(
                            activation_height,
                            pending.target.planned_activation_height,
                            dkg_rotation_params.activation_grace_blocks,
                        ) {
                            PendingDkgActivationDecision::Expired { deadline } => {
                                vrf_safety.mark_expired(activation_height);
                                publish_randomness_status(&bridge, &vrf_safety);
                                return Err(eyre::eyre!(
                                    "pending DKG activation missed VRF expiry: cycle {}, height {}, deadline {}",
                                    pending.target.dkg_cycle,
                                    activation_height,
                                    deadline
                                ));
                            }
                            PendingDkgActivationDecision::Wait => {}
                            PendingDkgActivationDecision::Activate => {
                                let Some(canonical_output) =
                                    dkg_manager.canonical_output(current_epoch)
                                else {
                                    warn!(
                                        epoch = %current_epoch,
                                        activation_height,
                                        "DKG activation height reached but canonical finalized-log output is not ready"
                                    );
                                    continue;
                                };
                                let Some(pending) = pending_dkg_activation.take() else {
                                    return Err(eyre::eyre!(
                                        "pending DKG activation missing after precheck at height {activation_height}"
                                    ));
                                };
                            let target = pending.target;
                            let dkg_complete = pending.complete;
                            if let Err(error) = dkg_manager::assert_canonical_output(
                                &dkg_complete.output,
                                &canonical_output,
                                &format!("cycle {}", target.dkg_cycle),
                            ) {
                                vrf_safety.mark_expired(activation_height);
                                publish_randomness_status(&bridge, &vrf_safety);
                                return Err(error);
                            }

                            let activated_validator_set = validator_set_for_dkg_output_players(
                                &canonical_output,
                                &target.validator_set,
                            )?;
                            let activated_participants =
                                participants_from_validator_set(&activated_validator_set)?;
                            let activated_is_validator_set_change = activated_participants != participants;
                            // invariant:
                            // `vrf_material_version` increments by exactly 1 per
                            // successful reshare activation. Overflow is a
                            // deterministic activation error, not saturation.
                            // The single source of truth lives in the
                            // `outbe-validatorset` crate so proposer and
                            // validator paths cannot diverge.
                            let activated_vrf_material_version =
                                outbe_validatorset::next_vrf_material_version(
                                    vrf_material_version,
                                )?;
                            let activated_polynomial = canonical_output.public().clone();
                            let activated_signing_share = dkg_complete.share;
                            let next_epoch = next_consensus_epoch_after_dkg_activation(current_epoch);
                            let boundary_artifact = dkg_manager::build_boundary_artifact(
                                dkg_manager::BoundaryArtifactInput {
                                    epoch: next_epoch,
                                    validator_set: &activated_validator_set,
                                    output: &canonical_output,
                                    is_full_dkg: false,
                                    dkg_cycle: target.dkg_cycle,
                                    freeze_height: target.freeze_height,
                                    planned_activation_height: target.planned_activation_height,
                                    vrf_material_version: activated_vrf_material_version,
                                    is_validator_set_change: activated_is_validator_set_change,
                                    tee_reshare_registrations: Vec::new(),
                                },
                            )?;
                            let epoch_boundary_height =
                                activation_height.max(target.planned_activation_height);
                            if let Some(ref keys_dir) = args.keys_dir {
                                // A shareless VERIFIER activation (C1) has no private
                                // share to persist; only the boundary snapshot is saved.
                                if let Some(share) = activated_signing_share.as_ref() {
                                    save_pending_dkg_state(
                                        keys_dir,
                                        share,
                                        &activated_polynomial,
                                        &canonical_output,
                                        &key_backend,
                                    )
                                    .wrap_err(
                                        "failed to durably save pending DKG state before activation",
                                    )?;
                                }
                                save_pending_dkg_boundary(
                                    keys_dir,
                                    &PendingDkgBoundarySnapshot {
                                        artifact: boundary_artifact.clone(),
                                        activated_at_height: epoch_boundary_height,
                                    },
                                )
                                .wrap_err("failed to durably save pending DKG boundary before activation")?;
                            }

                            vrf_material_version = activated_vrf_material_version;
                            polynomial = activated_polynomial;
                            last_dkg_output = Some(canonical_output.clone());
                            signing_share = activated_signing_share;
                            vrf_materials.activate(
                                vrf_material_version,
                                polynomial.clone(),
                                signing_share.clone(),
                            );

                            dkg_manager.note_ceremony_completed(boundary_artifact);
                            application_epoch_fence.arm_activation_boundary(
                                current_epoch,
                                epoch_boundary_height,
                            );
                            debug!(
                                epoch = %current_epoch,
                                dkg_cycle = target.dkg_cycle,
                                max_block_height = epoch_boundary_height,
                                "armed application epoch fence for DKG activation"
                            );

                            last_dkg_activation_height =
                                activation_height.max(target.planned_activation_height);
                            vrf_safety.note_activated(
                                vrf_material_version,
                                last_dkg_activation_height,
                                dkg_rotation_params
                                    .planned_activation_height(last_dkg_activation_height),
                                dkg_rotation_params.activation_grace_blocks,
                            );
                            info!(
                                dkg_cycle = target.dkg_cycle,
                                activation_height = last_dkg_activation_height,
                                planned_activation_height = target.planned_activation_height,
                                vrf_material_version,
                                vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                                dkg_output_hash = %dkg_manager::dkg_output_hash(&canonical_output),
                                dkg_public_polynomial_hash = %dkg_manager::public_polynomial_hash(&polynomial),
                                is_validator_set_change = activated_is_validator_set_change,
                                "VRF/DKG material activated"
                            );
                            publish_randomness_status(&bridge, &vrf_safety);
                            register_epoch_validation_providers(
                                next_epoch,
                                &activated_participants,
                                &activated_validator_set,
                                None,
                                &vrf_materials,
                                &certificate_scheme_provider,
                                &committee_provider,
                            )?;
                            validator_set = activated_validator_set;
                            frozen_dkg_target = None;
                            outbe_consensus::metrics::record_dkg_status(0);

                            participants = activated_participants;
                            application_epoch_fence.advance_epoch(next_epoch);
                            engine_handle_task.abort();
                            current_epoch = next_epoch;
                            info!(
                                epoch = %current_epoch,
                                vrf_material_version,
                                is_validator_set_change = activated_is_validator_set_change,
                                "DKG activation advanced consensus epoch; restarting Simplex engine"
                            );
                            // DKG activation race: before bouncing back
                            // into the epoch loop (which will call `engine.start`
                            // for the new epoch), wait for the FinalizationActor
                            // to publish the activation block as finalized. The
                            // generic `current_epoch > 0` guard at the top of
                            // the loop checks only that *some* finalized anchor
                            // exists, which is a weaker condition than
                            // `last_finalized_number >= activation_height` — a
                            // stale anchor would still satisfy the generic
                            // guard while pointing Simplex at the wrong parent.
                            let activation_height = last_dkg_activation_height;
                            let deadline =
                                ctx.current() + EPOCH_RESTART_ANCHOR_TIMEOUT;
                            loop {
                                let (finalized, finalized_hash, round_ready) = {
                                    let view = finalization_view.read();
                                    (
                                        view.last_finalized_number,
                                        view.forkchoice.finalized_block_hash,
                                        view.last_finalized_round.is_some(),
                                    )
                                };
                                if finalized >= activation_height
                                    && finalized_hash != alloy_primitives::B256::ZERO
                                    && round_ready
                                {
                                    break;
                                }
                                if ctx.current() >= deadline {
                                    return Err(eyre::eyre!(
                                        "DKG activation race after {:?}: \
                                         finalized_anchor=(height={}, hash={}, round_ready={}) \
                                         activation_height={}; \
                                         FinalizationActor is lagging the DKG manager",
                                        EPOCH_RESTART_ANCHOR_TIMEOUT,
                                        finalized,
                                        finalized_hash,
                                        round_ready,
                                        activation_height
                                    ));
                                }
                                ctx.sleep(EPOCH_RESTART_ANCHOR_POLL_INTERVAL).await;
                            }
                            continue 'epoch_loop;
                            }
                        }
                    }

                    // Read the dealer-only decision WITHOUT holding a borrow on
                    // `dealer_only_dkg_activation` so the Activate arm can `.take()` it.
                    let dealer_only_decision = dealer_only_dkg_activation.as_ref().map(|d| {
                        (
                            current_height.max(d.completed_at_height),
                            d.target.planned_activation_height,
                            d.target.dkg_cycle,
                        )
                    });
                    if let Some((activation_height, planned_activation_height, target_dkg_cycle)) =
                        dealer_only_decision
                    {
                        match pending_dkg_activation_decision(
                            activation_height,
                            planned_activation_height,
                            dkg_rotation_params.activation_grace_blocks,
                        ) {
                            PendingDkgActivationDecision::Expired { deadline } => {
                                vrf_safety.mark_expired(activation_height);
                                publish_randomness_status(&bridge, &vrf_safety);
                                return Err(eyre::eyre!(
                                    "dealer-only DKG activation missed VRF expiry: cycle {}, height {}, deadline {}",
                                    target_dkg_cycle,
                                    activation_height,
                                    deadline
                                ));
                            }
                            PendingDkgActivationDecision::Wait => {}
                            PendingDkgActivationDecision::Activate => {
                                // S3 demotion: an exited validator (deactivated/unstaked) is a
                                // previous-output dealer but not a frozen-target player, so it
                                // finishes its dealer duties for the resharded committee and then,
                                // instead of looping until VRF expiry kills the process, DEMOTES to
                                // a share-less verifier-follower of the smaller (N-1) committee. It
                                // adopts the new group polynomial reconstructed from the finalized
                                // dealer logs it just helped produce (`canonical_output` for the
                                // ceremony epoch), drops its share, advances its epoch, and restarts
                                // the Simplex engine in verifier mode — the same finalized-follower
                                // path a non-staked TEE full-node uses. The reshared output is a
                                // membership change, so unlike the same-membership verifier-follow
                                // the node MUST take the new polynomial + participant set here (it
                                // has them from the ceremony) rather than reusing the old ones.
                                let Some(canonical_output) =
                                    dkg_manager.canonical_output(current_epoch)
                                else {
                                    warn!(
                                        epoch = %current_epoch,
                                        activation_height,
                                        "exited validator: dealer-only activation reached but resharded canonical output not yet reconstructed; waiting"
                                    );
                                    continue;
                                };
                                let Some(dealer_only) = dealer_only_dkg_activation.take() else {
                                    return Err(eyre::eyre!(
                                        "dealer-only activation missing after decision at height {activation_height}"
                                    ));
                                };
                                let target = dealer_only.target;
                                let next_epoch =
                                    next_consensus_epoch_after_dkg_activation(current_epoch);
                                info!(
                                    epoch = %current_epoch,
                                    next_epoch = %next_epoch,
                                    activation_height,
                                    old = participants.len(),
                                    new = target.participants.len(),
                                    "exited validator: dealer-only DKG duties complete — demoting to verifier-follower of the resharded committee"
                                );
                                let new_vrf_material_version =
                                    match outbe_validatorset::next_vrf_material_version(
                                        vrf_material_version,
                                    ) {
                                        Ok(version) => version,
                                        Err(error) => {
                                            warn!(%error, "exited validator: vrf material version overflow during demotion; reusing current");
                                            vrf_material_version
                                        }
                                    };
                                signing_share = None;
                                polynomial = canonical_output.public().clone();
                                last_dkg_output = Some(canonical_output);
                                validator_set = target.validator_set.clone();
                                participants = target.participants.clone();
                                vrf_material_version = new_vrf_material_version;
                                dkg_cycle = target.dkg_cycle.saturating_add(1);
                                vrf_materials.activate(
                                    vrf_material_version,
                                    polynomial.clone(),
                                    None,
                                );
                                let anchored_height = current_height.max(activation_height);
                                last_dkg_activation_height = anchored_height;
                                vrf_safety.note_activated(
                                    vrf_material_version,
                                    anchored_height,
                                    dkg_rotation_params.planned_activation_height(anchored_height),
                                    dkg_rotation_params.activation_grace_blocks,
                                );
                                publish_randomness_status(&bridge, &vrf_safety);
                                application_epoch_fence.advance_epoch(next_epoch);
                                engine_handle_task.abort();
                                current_epoch = next_epoch;
                                // Anchor wait (mirror the verifier activation): the restarted
                                // verifier engine's floor needs the activation block finalized
                                // before `'epoch_loop` rebuilds the verifier scheme.
                                let deadline =
                                    ctx.current() + EPOCH_RESTART_ANCHOR_TIMEOUT;
                                loop {
                                    let (finalized, finalized_hash, round_ready) = {
                                        let view = finalization_view.read();
                                        (
                                            view.last_finalized_number,
                                            view.forkchoice.finalized_block_hash,
                                            view.last_finalized_round.is_some(),
                                        )
                                    };
                                    if finalized >= activation_height
                                        && finalized_hash != alloy_primitives::B256::ZERO
                                        && round_ready
                                    {
                                        break;
                                    }
                                    if ctx.current() >= deadline {
                                        return Err(eyre::eyre!(
                                            "exited-validator demotion activation race after {:?}: \
                                             finalized=(height={}, hash={}, round_ready={}) activation_height={}",
                                            EPOCH_RESTART_ANCHOR_TIMEOUT,
                                            finalized,
                                            finalized_hash,
                                            round_ready,
                                            activation_height
                                        ));
                                    }
                                    ctx.sleep(EPOCH_RESTART_ANCHOR_POLL_INTERVAL).await;
                                }
                                continue 'epoch_loop;
                            }
                        }
                    }

                    if let Some(target) = frozen_dkg_target.as_ref() {
                        let activation_deadline = target
                            .planned_activation_height
                            .saturating_add(dkg_rotation_params.activation_grace_blocks);
                        if current_height > activation_deadline {
                            vrf_safety.mark_expired(current_height);
                            publish_randomness_status(&bridge, &vrf_safety);
                            return Err(eyre::eyre!(
                                "frozen DKG target missed VRF expiry: cycle {}, height {}, deadline {}",
                                target.dkg_cycle,
                                current_height,
                                activation_deadline
                            ));
                        }
                    }

                    if retry_frozen_dkg {
                        retry_frozen_dkg = false;
                        if let Some(target) = frozen_dkg_target.as_ref().cloned() {
                            info!(
                                dkg_cycle = target.dkg_cycle,
                                planned_activation_height = target.planned_activation_height,
                                "retrying DKG for frozen target"
                            );
                            reshare_in_progress = true;
                            outbe_consensus::metrics::record_dkg_status(1);

                            match dkg_mux.register(target.dkg_cycle).await {
                                Ok((dkg_tx, dkg_rx)) => {
                                    let round = target.dkg_cycle;
                                    let tx = dkg_result_tx.clone();
                                    let progress_tx = dkg_progress_tx.clone();
                                    let key = signing_key.clone();
                                    let parts = target.participants.clone();
                                    // Share-less joiner: refresh prev_output from the chain so
                                    // the ceremony info_hash matches the committee's (see
                                    // refresh_verifier_join_prev_output).
                                    if signing_share.is_none() {
                                        refresh_verifier_join_prev_output(
                                            &node.provider,
                                            target.freeze_height,
                                            dkg_rotation_params,
                                            &mut last_dkg_output,
                                        );
                                    }
                                    let prev_output = last_dkg_output.clone();
                                    let prev_share = signing_share.clone();
                                    let role = classify_local_reshare_role(
                                        &key.public_key(),
                                        prev_output.as_ref(),
                                        &parts,
                                    );
                                    let (finalized_log_tx, finalized_log_rx) =
                                        tokio::sync::mpsc::unbounded_channel();
                                    if let Err(error) = dkg_manager.note_ceremony_started_with_finalized_log_tx(
                                        current_epoch,
                                        round,
                                        prev_output.clone(),
                                        target.participants.clone(),
                                        Some(finalized_log_tx.clone()),
                                    ) {
                                        warn!(%error, epoch = %current_epoch, round, "failed to initialize DKG manager state for frozen-target retry");
                                        reshare_in_progress = false;
                                        outbe_consensus::metrics::record_dkg_status(0);
                                        continue;
                                    }
                                    let mut replay_height = target.freeze_height;
                                    if let Err(error) = replay_finalized_dealer_logs_into_manager(
                                        &node.provider,
                                        &mut replay_height,
                                        current_height,
                                        &dkg_manager,
                                    ) {
                                        warn!(
                                            %error,
                                            epoch = %current_epoch,
                                            round,
                                            from_height = target.freeze_height,
                                            to_height = current_height,
                                            "failed to replay finalized dealer logs for frozen-target retry"
                                        );
                                        reshare_in_progress = false;
                                        retry_frozen_dkg = true;
                                        outbe_consensus::metrics::record_dkg_status(0);
                                        continue;
                                    }
                                    ctx.child("dkg_retry").spawn(move |dkg_ctx| async move {
                                        let result = match role {
                                            LocalDkgRole::DealerAndPlayer => {
                                                dkg_actor::run_initial_dkg(
                                                    &dkg_ctx,
                                                    key,
                                                    parts,
                                                    prev_output,
                                                    prev_share,
                                                    round,
                                                    Some(progress_tx),
                                                    Some(finalized_log_rx),
                                                    dkg_tx,
                                                    dkg_rx,
                                                )
                                                .await
                                                .map(DkgTaskOutcome::Complete)
                                            }
                                            LocalDkgRole::PlayerOnly => {
                                                dkg_actor::run_initial_dkg(
                                                    &dkg_ctx,
                                                    key,
                                                    parts,
                                                    prev_output,
                                                    None,
                                                    round,
                                                    Some(progress_tx),
                                                    Some(finalized_log_rx),
                                                    dkg_tx,
                                                    dkg_rx,
                                                )
                                                .await
                                                .map(DkgTaskOutcome::Complete)
                                            }
                                            LocalDkgRole::DealerOnly => match (prev_output, prev_share) {
                                                (Some(output), Some(share)) => dkg_actor::run_reshare_dealer_only(
                                                    &dkg_ctx,
                                                    key,
                                                    parts,
                                                    output,
                                                    share,
                                                    round,
                                                    progress_tx,
                                                    dkg_tx,
                                                    dkg_rx,
                                                )
                                                .await
                                                .map(DkgTaskOutcome::DealerOnly),
                                                (None, _) => Err(eyre::eyre!(
                                                    "dealer-only DKG retry requires previous output"
                                                )),
                                                (Some(_), None) => Err(eyre::eyre!(
                                                    "dealer-only DKG requires a previous share"
                                                )),
                                            },
                                            LocalDkgRole::NotParticipant => Err(eyre::eyre!(
                                                "local key is neither previous dealer nor target player for DKG retry"
                                            )),
                                        };
                                        let _ = tx.send(result);
                                    });
                                }
                                Err(e) => {
                                    warn!(?e, "failed to register DKG subchannel for retry");
                                    reshare_in_progress = false;
                                    retry_frozen_dkg = true;
                                }
                            }
                            continue;
                        }
                    }

                    let freeze_height = dkg_rotation_params.freeze_height(last_dkg_activation_height);
                    if pending_verifier_activation.is_none()
                        && should_start_dkg_rotation(
                            frozen_dkg_target.is_some(),
                            pending_dkg_activation.is_some(),
                            current_height,
                            freeze_height,
                        )
                    {
                        let planned_activation_height =
                            dkg_rotation_params.planned_activation_height(last_dkg_activation_height);
                        info!(
                            dkg_cycle,
                            current_height,
                            freeze_height,
                            planned_activation_height,
                            "freezing validator set and starting DKG rotation"
                        );

                        // Freeze the target set from the EVM state at freeze_height.
                        // This keeps DKG membership deterministic across validators.
                        let (target_validator_set, target_participants) = match refresh_validator_set_at_height(&node.provider, freeze_height) {
                            Ok(FrozenValidatorSetRefresh::Ready {
                                validator_set: new_set,
                                participants: new_participants,
                            }) => {
                                let old_count = participants.len();
                                let local_role = classify_local_reshare_role(
                                    &signing_key.public_key(),
                                    last_dkg_output.as_ref(),
                                    &new_participants,
                                );
                                if local_role == LocalDkgRole::NotParticipant {
                                    // A share-less verifier-follower (TEE full-node) is not a
                                    // committee member: neither a previous dealer nor a frozen
                                    // target player. It must NOT crash or run a ceremony — it
                                    // notes the activation height and keeps following; when it
                                    // syncs to that height it advances its epoch (reusing its
                                    // polynomial, since the group key is preserved across
                                    // reshares) so its per-epoch reporter verifies the new epoch's
                                    // finality. A node that HELD a share but is now NotParticipant
                                    // is a real error and still fails.
                                    if signing_share.is_none() {
                                        // Finalized-follow is only valid for a SAME-MEMBERSHIP
                                        // reshare: certificate verification binds the ordered
                                        // participant set (not just the preserved group key), and
                                        // the rebuilt verifier scheme requires polynomial-total ==
                                        // participant-count. On a membership change the verifier
                                        // would need the NEW group polynomial (only the group key,
                                        // not the full polynomial, is on-chain), so fail loudly +
                                        // resync rather than silently reuse a stale committee.
                                        if new_participants != participants {
                                            return Err(eyre::eyre!(
                                                "verifier-follower cannot follow a membership-changing DKG reshare at height {freeze_height} \
                                                 (committee {} → {}); resync required",
                                                participants.len(),
                                                new_participants.len()
                                            ));
                                        }
                                        info!(
                                            freeze_height,
                                            planned_activation_height,
                                            "verifier-follower: DKG rotation noted; will advance epoch at the activation height"
                                        );
                                        pending_verifier_activation = Some(planned_activation_height);
                                        if verifier_activation_needs_immediate_replay(
                                            current_height,
                                            planned_activation_height,
                                        ) {
                                            // Re-enter this height-event path immediately. The
                                            // pending-activation branch above will advance the
                                            // epoch before the verifier waits for a new-epoch
                                            // block that its old verifier cannot accept.
                                            let _ = execution_finalized_height_tx
                                                .send(current_height);
                                        }
                                        continue;
                                    }
                                    return Err(eyre::eyre!(
                                        "local validator is neither previous DKG dealer nor frozen target player at height {freeze_height}"
                                    ));
                                }

                                // Update P2P oracle so new validators can participate in DKG.
                                let peer_map = build_peer_map(&new_set, &bootnode_map);
                                let chain_peer_set_id = p2p_oracle_chain_peer_set_id(freeze_height);
                                let dkg_peer_set_id = p2p_oracle_dkg_peer_set_id(freeze_height);
                                let _ = oracle.track(chain_peer_set_id, peer_map.clone());
                                let _ = oracle.track(dkg_peer_set_id, peer_map);

                                info!(
                                    old = old_count,
                                    new = new_participants.len(),
                                    ?local_role,
                                    chain_peer_set_id,
                                    dkg_peer_set_id,
                                    "refreshed validator set from EVM state for reshare"
                                );
                                (new_set, new_participants)
                            }
                            Ok(FrozenValidatorSetRefresh::PendingBlockHash) => {
                                match pending_freeze_block_hash_decision(
                                    current_height,
                                    planned_activation_height,
                                ) {
                                    PendingFreezeBlockHashDecision::Retry => {}
                                    PendingFreezeBlockHashDecision::Expired => {
                                        vrf_safety.mark_expired(current_height);
                                        publish_randomness_status(&bridge, &vrf_safety);
                                        return Err(eyre::eyre!(
                                            "frozen validator set block hash unavailable by planned activation: freeze height {freeze_height}, current height {current_height}, planned activation {planned_activation_height}"
                                        ));
                                    }
                                }
                                warn!(
                                    current_height,
                                    freeze_height,
                                    planned_activation_height,
                                    "frozen validator set block hash is not available yet; retrying on next finalized height"
                                );
                                continue;
                            }
                            Err(e) => {
                                vrf_safety.mark_expired(current_height);
                                publish_randomness_status(&bridge, &vrf_safety);
                                return Err(eyre::eyre!(
                                    "failed to refresh frozen validator set at height {freeze_height}: {e}"
                                ));
                            }
                        };

                        let is_validator_set_change = target_participants != participants;
                        let target_dkg_cycle = dkg_cycle;
                        frozen_dkg_target = Some(FrozenDkgTarget {
                            dkg_cycle: target_dkg_cycle,
                            freeze_height,
                            planned_activation_height,
                            validator_set: target_validator_set.clone(),
                            participants: target_participants.clone(),
                            is_validator_set_change,
                        });
                        vrf_safety.note_preparing(
                            target_dkg_cycle,
                            freeze_height,
                            planned_activation_height,
                            dkg_rotation_params.activation_grace_blocks,
                        );
                        publish_randomness_status(&bridge, &vrf_safety);
                        dkg_cycle = target_dkg_cycle.saturating_add(1);

                        reshare_in_progress = true;
                        outbe_consensus::metrics::record_dkg_status(1); // in progress

                        // Register DKG sub-channel for this reshare round.
                        match dkg_mux.register(target_dkg_cycle).await {
                            Ok((dkg_tx, dkg_rx)) => {
                                let round = target_dkg_cycle;
                                let tx = dkg_result_tx.clone();
                                let progress_tx = dkg_progress_tx.clone();
                                let key = signing_key.clone();
                                let parts = target_participants.clone();
                                // Share-less joiner (verifier-join becoming a player): refresh
                                // prev_output from the chain's canonical output so the ceremony
                                // info_hash matches the committee's. Without this a long-lived
                                // TEE full-node joining at its FIRST reshare presents its stale
                                // CLI `--consensus.dkg-output` -> info_hash mismatch -> dealer
                                // bundles dropped -> timeout -> ACTIVE-but-voteless.
                                if signing_share.is_none() {
                                    refresh_verifier_join_prev_output(
                                        &node.provider,
                                        freeze_height,
                                        dkg_rotation_params,
                                        &mut last_dkg_output,
                                    );
                                }
                                // Capture previous DKG state for reshare.
                                let prev_output = last_dkg_output.clone();
                                let prev_share = signing_share.clone();
                                let role = classify_local_reshare_role(
                                    &key.public_key(),
                                    prev_output.as_ref(),
                                    &parts,
                                );
                                let (finalized_log_tx, finalized_log_rx) =
                                    tokio::sync::mpsc::unbounded_channel();
                                if let Err(error) = dkg_manager.note_ceremony_started_with_finalized_log_tx(
                                    current_epoch,
                                    round,
                                    prev_output.clone(),
                                    target_participants.clone(),
                                    Some(finalized_log_tx.clone()),
                                ) {
                                    warn!(%error, epoch = %current_epoch, round, "failed to initialize DKG manager state for live reshare");
                                    reshare_in_progress = false;
                                    frozen_dkg_target = None;
                                    outbe_consensus::metrics::record_dkg_status(0);
                                    continue;
                                }
                                let mut replay_height = freeze_height;
                                if let Err(error) = replay_finalized_dealer_logs_into_manager(
                                    &node.provider,
                                    &mut replay_height,
                                    current_height,
                                    &dkg_manager,
                                ) {
                                    warn!(
                                        %error,
                                        epoch = %current_epoch,
                                        round,
                                        from_height = freeze_height,
                                        to_height = current_height,
                                        "failed to replay finalized dealer logs for live reshare"
                                    );
                                    reshare_in_progress = false;
                                    retry_frozen_dkg = true;
                                    outbe_consensus::metrics::record_dkg_status(0);
                                    continue;
                                }
                                ctx.child("dkg_live").spawn(move |dkg_ctx| async move {
                                    let result = match role {
                                        LocalDkgRole::DealerAndPlayer => {
                                            dkg_actor::run_initial_dkg(
                                                &dkg_ctx,
                                                key,
                                                parts,
                                                prev_output,
                                                prev_share,
                                                round,
                                                Some(progress_tx),
                                                Some(finalized_log_rx),
                                                dkg_tx,
                                                dkg_rx,
                                            )
                                            .await
                                            .map(DkgTaskOutcome::Complete)
                                        }
                                        LocalDkgRole::PlayerOnly => {
                                            dkg_actor::run_initial_dkg(
                                                &dkg_ctx,
                                                key,
                                                parts,
                                                prev_output,
                                                None,
                                                round,
                                                Some(progress_tx),
                                                Some(finalized_log_rx),
                                                dkg_tx,
                                                dkg_rx,
                                            )
                                            .await
                                            .map(DkgTaskOutcome::Complete)
                                        }
                                        LocalDkgRole::DealerOnly => match (prev_output, prev_share) {
                                            (Some(output), Some(share)) => dkg_actor::run_reshare_dealer_only(
                                                &dkg_ctx,
                                                key,
                                                parts,
                                                output,
                                                share,
                                                round,
                                                progress_tx,
                                                dkg_tx,
                                                dkg_rx,
                                            )
                                            .await
                                            .map(DkgTaskOutcome::DealerOnly),
                                            (None, _) => Err(eyre::eyre!(
                                                "dealer-only live DKG requires previous output"
                                            )),
                                            (Some(_), None) => Err(eyre::eyre!(
                                                "dealer-only live DKG requires a previous share"
                                            )),
                                        },
                                        LocalDkgRole::NotParticipant => Err(eyre::eyre!(
                                            "local key is neither previous dealer nor target player for live DKG"
                                        )),
                                    };
                                    let _ = tx.send(result);
                                });
                            }
                            Err(e) => {
                                warn!(?e, "failed to register DKG subchannel");
                                reshare_in_progress = false;
                                frozen_dkg_target = None;
                            }
                        }
                    } else {
                        debug!(
                            current_height,
                            freeze_height,
                            "DKG rotation freeze height not reached"
                        );
                    }
                }

                // Component exits → fatal.
                result = &mut executor_handle_task => {
                    info!("executor actor exited");
                    let executor_result = result
                        .map_err(|e| eyre::eyre!("executor actor task failed: {e:?}"))?;
                    executor_result.wrap_err("executor actor returned fatal error")?;
                    break 'epoch_loop;
                }
                result = &mut handler_handle => {
                    info!("application handler exited");
                    let application_result = result
                        .map_err(|e| eyre::eyre!("application handler task failed: {e:?}"))?;
                    application_result.wrap_err("application handler returned fatal error")?;
                    break 'epoch_loop;
                }
                result = &mut finalization_handle => {
                    info!("finalization actor exited");
                    let finalization_result = result
                        .map_err(|e| eyre::eyre!("finalization actor task failed: {e:?}"))?;
                    finalization_result.wrap_err("finalization actor returned fatal error")?;
                    break 'epoch_loop;
                }
                result = &mut peer_manager_handle_task => {
                    info!("peer manager actor exited");
                    result.map_err(|e| eyre::eyre!("peer manager actor exited: {e:?}"))?;
                    break 'epoch_loop;
                }
                // SSA-8: the marshal actor is consensus-liveness-critical (block
                // availability, finalized-block delivery to the executor). With
                // `catch_panics`, a marshal panic (e.g. an unacknowledged Exact,
                // or a future telemetry-label assert) resolves its handle instead
                // of aborting the process — so an UNmonitored handle would leave
                // the node silently stalled (no blocks delivered, consensus
                // wedged). Monitor it like the other components: a marshal exit
                // is fatal and shuts the node down with the cause.
                result = &mut marshal_handle => {
                    info!("marshal actor exited");
                    result.map_err(|e| eyre::eyre!("marshal actor exited: {e:?}"))?;
                    break 'epoch_loop;
                }
                // The broadcast (buffered dissemination) handle remains managed by
                // the Commonware runtime; its failure degrades to the marshal
                // pull/serve path rather than a consensus stall.
            }
        }
    }

    // Bridge is kept alive for the duration of consensus — executor reads from it.
    drop(bridge);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Helper functions
// ═══════════════════════════════════════════════════════════════════════════

async fn recover_application_finalized_round(
    clock: &impl Clock,
    marshal_mailbox: &outbe_consensus::marshal_types::MarshalMailbox,
    last_execution_height: u64,
) -> Result<Option<RecoveredApplicationFinalization>> {
    if last_execution_height == 0 {
        return Ok(None);
    }

    let height = Height::new(last_execution_height);
    for attempt in 1..=FINALIZED_ROUND_RECOVERY_ATTEMPTS {
        // Measure the per-attempt timeout on the consensus runtime `Clock`, not
        // tokio's wall-clock, so recovery is reproducible under the deterministic
        // test runtime. `Clock::timeout` requires a `Send + 'static` future, so the
        // mailbox is cloned (a cheap sender clone) and moved into the request.
        let mailbox = marshal_mailbox.clone();
        match clock
            .timeout(FINALIZED_ROUND_RECOVERY_TIMEOUT, async move {
                mailbox.get_finalization(height).await
            })
            .await
        {
            Ok(Some(finalization)) => {
                let round = finalization.proposal.round;
                let digest = finalization.proposal.payload;
                info!(
                    last_execution_height,
                    ?round,
                    %digest,
                    "recovered application finalized round from marshal archive"
                );
                return Ok(Some(RecoveredApplicationFinalization { round, digest }));
            }
            Ok(None) if attempt < FINALIZED_ROUND_RECOVERY_ATTEMPTS => {
                clock.sleep(FINALIZED_ROUND_RECOVERY_RETRY_DELAY).await;
            }
            Ok(None) => {
                return Err(eyre::eyre!(
                    "marshal finalization missing for finalized execution height {last_execution_height}; \
                     likely partial restore/archive corruption; resync/rebuild consensus storage"
                ));
            }
            Err(_) if attempt < FINALIZED_ROUND_RECOVERY_ATTEMPTS => {
                clock.sleep(FINALIZED_ROUND_RECOVERY_RETRY_DELAY).await;
            }
            Err(_) => {
                return Err(eyre::eyre!(
                    "timed out recovering marshal finalization for finalized execution height {last_execution_height}; \
                     likely partial restore/archive corruption; resync/rebuild consensus storage"
                ));
            }
        }
    }

    Err(eyre::eyre!(
        "marshal finalization missing for finalized execution height {last_execution_height}; \
         likely partial restore/archive corruption; resync/rebuild consensus storage"
    ))
}

/// Build the leader-elector config for an epoch start.
///
/// Epoch 0 has no previous finalized certificate, so view 1 uses the one-time
/// genesis round-robin exception. Every later epoch must start from the last
/// finalized certificate of the previous epoch so that view 1 continues to use
/// VRF-derived leader selection rather than silently falling back to round-robin.
fn epoch_elector_config(
    epoch: Epoch,
    continuity: &ReporterContinuity,
    vrf_materials: VrfMaterialProvider<MinSig>,
) -> Result<HybridRandom<MinSig>> {
    if epoch.get() == 0 {
        return Ok(HybridRandom::with_vrf_materials(vrf_materials));
    }

    let snapshot = continuity.snapshot();
    if snapshot.last_finalized_view == 0 {
        warn!(
            epoch = epoch.get(),
            "starting epoch without reporter continuity; leader election will use active VRF material until a certificate is finalized"
        );
        return Ok(HybridRandom::with_vrf_materials(vrf_materials));
    }
    let seed = snapshot.last_vrf_seed.unwrap_or_default();
    if seed.is_empty() {
        Ok(HybridRandom::with_vrf_materials(vrf_materials))
    } else {
        Ok(HybridRandom::with_bootstrap_seed_and_vrf_materials(
            seed,
            vrf_materials,
        ))
    }
}

/// DKG state file names within keys_dir.
const DKG_SHARE_FILE: &str = "dkg_share.hex";
const DKG_POLYNOMIAL_FILE: &str = "dkg_polynomial.hex";
const DKG_OUTPUT_FILE: &str = "dkg_output.hex";
const DKG_PENDING_SHARE_FILE: &str = "dkg_pending_share.hex";
const DKG_PENDING_POLYNOMIAL_FILE: &str = "dkg_pending_polynomial.hex";
const DKG_PENDING_OUTPUT_FILE: &str = "dkg_pending_output.hex";
const DKG_PENDING_BOUNDARY_FILE: &str = "dkg_pending_boundary.bin";
const DKG_PENDING_BOUNDARY_TMP_FILE: &str = "dkg_pending_boundary.bin.tmp";

const DKG_ALL_FILES: &[&str] = &[
    DKG_SHARE_FILE,
    DKG_POLYNOMIAL_FILE,
    DKG_OUTPUT_FILE,
    DKG_PENDING_SHARE_FILE,
    DKG_PENDING_POLYNOMIAL_FILE,
    DKG_PENDING_OUTPUT_FILE,
    DKG_PENDING_BOUNDARY_FILE,
];

/// Move DKG key files from legacy location (`consensus/`) to dedicated `keys/` dir.
pub fn migrate_dkg_keys_if_needed(
    consensus_dir: &std::path::Path,
    keys_dir: &std::path::Path,
) -> eyre::Result<()> {
    let old_share = consensus_dir.join(DKG_SHARE_FILE);
    if !old_share.exists() || keys_dir.join(DKG_SHARE_FILE).exists() {
        return Ok(());
    }
    std::fs::create_dir_all(keys_dir)
        .wrap_err_with(|| format!("failed to create keys dir: {}", keys_dir.display()))?;
    for file in DKG_ALL_FILES {
        let src = consensus_dir.join(file);
        if src.exists() {
            let dst = keys_dir.join(file);
            std::fs::rename(&src, &dst).wrap_err_with(|| {
                format!("failed to migrate {} → {}", src.display(), dst.display())
            })?;
            info!(from = %src.display(), to = %dst.display(), "migrated DKG key file");
        }
    }
    Ok(())
}
const DKG_PENDING_BOUNDARY_MAGIC: &[u8; 8] = b"ODKGPB01";
const STARTUP_JOIN_SCAN_INTERVAL: Duration = Duration::from_millis(250);
const DKG_OUTPUT_MAX_PARTICIPANTS: u32 = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingDkgBoundarySnapshot {
    artifact: DkgBoundaryArtifact,
    activated_at_height: u64,
}

struct StartupLiveJoinResult {
    signing_share: Share,
    polynomial: Sharing<MinSig>,
    output: Output<MinSig, bls12381::PublicKey>,
    activated_at_height: u64,
    activated_boundary: DkgBoundaryArtifact,
}

fn next_live_reshare_round(previous_boundary: &DkgBoundaryArtifact) -> u64 {
    previous_boundary.dkg_cycle.saturating_add(1)
}

fn decode_boundary_output(
    artifact: &DkgBoundaryArtifact,
) -> Result<Output<MinSig, bls12381::PublicKey>> {
    let bytes = artifact.outcome.as_ref();
    let header_len = 4 + 1 + std::mem::size_of::<u64>() + 1 + std::mem::size_of::<u32>();
    ensure!(
        bytes.len() >= header_len,
        "DKG boundary outcome too short: {} < {header_len}",
        bytes.len()
    );
    ensure!(
        &bytes[..4] == b"ODKO",
        "DKG boundary outcome has invalid magic"
    );
    ensure!(
        bytes[4] == 0x02,
        "DKG boundary outcome version {} is unsupported",
        bytes[4]
    );
    let epoch = u64::from_be_bytes(bytes[5..13].try_into()?);
    ensure!(
        epoch == artifact.epoch,
        "DKG boundary outcome epoch {epoch} does not match artifact epoch {}",
        artifact.epoch
    );
    let output_len_offset = 14;
    let output_len = u32::from_be_bytes(
        bytes[output_len_offset..output_len_offset + 4]
            .try_into()
            .map_err(|_| eyre::eyre!("invalid DKG output length bytes"))?,
    ) as usize;
    let output_start = output_len_offset + 4;
    let output_end = output_start.saturating_add(output_len);
    ensure!(
        output_end == bytes.len(),
        "DKG boundary outcome length mismatch: output_end={output_end}, total={}",
        bytes.len()
    );

    let mut output_bytes = &bytes[output_start..output_end];
    let cfg = (
        NonZeroU32::new(DKG_OUTPUT_MAX_PARTICIPANTS)
            .ok_or_else(|| eyre::eyre!("DKG output max participants must be non-zero"))?,
        ModeVersion::v0(),
    );
    let output = Output::<MinSig, bls12381::PublicKey>::read_cfg(&mut output_bytes, &cfg)
        .map_err(|error| eyre::eyre!("invalid DKG output in boundary artifact: {error}"))?;
    ensure!(
        output_bytes.is_empty(),
        "trailing bytes after DKG output in boundary artifact"
    );
    Ok(output)
}

/// A share-less node (verifier-join TEE full-node) that is about to participate in
/// a DKG reshare as a player must present the COMMITTEE's current output as the
/// ceremony `prev_output` — the DKG ceremony id binds the full previous output, so
/// a divergent prev_output yields a divergent `info_hash`, every dealer bundle is
/// dropped ("received DKG message for a different ceremony"), the ceremony times
/// out, and the joiner goes ACTIVE-but-voteless. Its in-memory `last_dkg_output`
/// may be the stale CLI `--consensus.dkg-output` bootstrap value (on a TEE chain
/// the runtime-derived genesis consensus output differs from the bootstrap file)
/// when it joins WITHOUT first following a reshare or restarting. Refresh it from
/// the chain's latest finalized DKG boundary (scanning back from `scan_height`)
/// before the ceremony. Signers already hold the correct output from their prior
/// ceremony, so this only runs for the share-less case. Best-effort: on any
/// recovery/decode failure it keeps the local value and warns.
fn refresh_verifier_join_prev_output(
    provider: &(impl HeaderProvider<Header = OutbeHeader> + BlockHashReader),
    scan_height: u64,
    dkg_rotation_params: DkgRotationParams,
    last_dkg_output: &mut Option<Output<MinSig, bls12381::PublicKey>>,
) {
    match recover_latest_boundary_artifact(provider, scan_height, dkg_rotation_params) {
        Ok(Some((commit_height, boundary))) => match decode_boundary_output(&boundary) {
            Ok(output) => {
                if last_dkg_output.as_ref() != Some(&output) {
                    info!(
                        commit_height,
                        "verifier-join: adopted chain canonical DKG output as reshare prev_output"
                    );
                }
                *last_dkg_output = Some(output);
            }
            Err(error) => warn!(
                %error,
                "verifier-join: failed to decode boundary output for reshare prev_output; using local"
            ),
        },
        Ok(None) => warn!(
            scan_height,
            "verifier-join: no boundary artifact for reshare prev_output; using local"
        ),
        Err(error) => warn!(
            %error,
            "verifier-join: failed to recover boundary for reshare prev_output; using local"
        ),
    }
}

fn encode_pending_dkg_boundary_snapshot(snapshot: &PendingDkgBoundarySnapshot) -> Result<Vec<u8>> {
    let boundary = encode_boundary_artifact(&snapshot.artifact)
        .map_err(|error| eyre::eyre!("failed to encode pending DKG boundary artifact: {error}"))?;
    let len: u32 = boundary.len().try_into().map_err(|_| {
        eyre::eyre!(
            "pending DKG boundary artifact too large: {} bytes",
            boundary.len()
        )
    })?;
    let mut out = Vec::with_capacity(DKG_PENDING_BOUNDARY_MAGIC.len() + 8 + 4 + boundary.len());
    out.extend_from_slice(DKG_PENDING_BOUNDARY_MAGIC);
    out.extend_from_slice(&snapshot.activated_at_height.to_be_bytes());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(boundary.as_ref());
    Ok(out)
}

fn decode_pending_dkg_boundary_snapshot(bytes: &[u8]) -> Result<PendingDkgBoundarySnapshot> {
    let header_len = DKG_PENDING_BOUNDARY_MAGIC.len() + 8 + 4;
    ensure!(
        bytes.len() >= header_len,
        "pending DKG boundary snapshot too short: {} < {header_len}",
        bytes.len()
    );
    ensure!(
        &bytes[..DKG_PENDING_BOUNDARY_MAGIC.len()] == DKG_PENDING_BOUNDARY_MAGIC,
        "invalid pending DKG boundary snapshot magic"
    );
    let height_offset = DKG_PENDING_BOUNDARY_MAGIC.len();
    let activated_at_height = u64::from_be_bytes(
        bytes[height_offset..height_offset + 8]
            .try_into()
            .map_err(|_| eyre::eyre!("invalid pending DKG boundary height field"))?,
    );
    let len_offset = height_offset + 8;
    let artifact_len = u32::from_be_bytes(
        bytes[len_offset..len_offset + 4]
            .try_into()
            .map_err(|_| eyre::eyre!("invalid pending DKG boundary length field"))?,
    ) as usize;
    let artifact_start = len_offset + 4;
    let artifact_end = artifact_start
        .checked_add(artifact_len)
        .ok_or_else(|| eyre::eyre!("pending DKG boundary length overflow"))?;
    ensure!(
        bytes.len() == artifact_end,
        "pending DKG boundary snapshot length mismatch: expected {artifact_end}, got {}",
        bytes.len()
    );
    let artifact = decode_boundary_artifact(&bytes[artifact_start..artifact_end])
        .map_err(|error| eyre::eyre!("failed to decode pending DKG boundary artifact: {error}"))?
        .ok_or_else(|| {
            eyre::eyre!("pending DKG boundary snapshot does not contain a BoundaryOutcome")
        })?;
    Ok(PendingDkgBoundarySnapshot {
        artifact,
        activated_at_height,
    })
}

fn pending_dkg_boundary_path(storage_dir: &std::path::Path) -> std::path::PathBuf {
    storage_dir.join(DKG_PENDING_BOUNDARY_FILE)
}

#[allow(clippy::too_many_arguments)]
fn persist_completed_dkg_before_activation(
    keys_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
    current_epoch: Epoch,
    vrf_material_version: u64,
    current_participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
    target: &FrozenDkgTarget,
    complete: &dkg_actor::DkgComplete,
    completed_at_height: u64,
) -> Result<()> {
    let activated_validator_set =
        validator_set_for_dkg_output_players(&complete.output, &target.validator_set)?;
    let activated_participants = participants_from_validator_set(&activated_validator_set)?;
    ensure!(
        complete.participants == activated_participants,
        "completed DKG participant set does not match reconstructed output players"
    );
    let next_epoch = next_consensus_epoch_after_dkg_activation(current_epoch);
    let next_vrf_material_version =
        outbe_validatorset::next_vrf_material_version(vrf_material_version)?;
    let boundary_artifact =
        dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
            epoch: next_epoch,
            validator_set: &activated_validator_set,
            output: &complete.output,
            is_full_dkg: false,
            dkg_cycle: target.dkg_cycle,
            freeze_height: target.freeze_height,
            planned_activation_height: target.planned_activation_height,
            vrf_material_version: next_vrf_material_version,
            is_validator_set_change: activated_participants != *current_participants,
            tee_reshare_registrations: Vec::new(),
        })?;

    if let Some(share) = complete.share.as_ref() {
        save_pending_dkg_state(
            keys_dir,
            share,
            complete.output.public(),
            &complete.output,
            key_backend,
        )
        .wrap_err("failed to durably save completed DKG state before activation")?;
    }
    let activated_at_height = completed_at_height.max(target.planned_activation_height);
    save_pending_dkg_boundary(
        keys_dir,
        &PendingDkgBoundarySnapshot {
            artifact: boundary_artifact,
            activated_at_height,
        },
    )
    .wrap_err("failed to durably save completed DKG boundary before activation")?;
    info!(
        keys_dir = %keys_dir.display(),
        dkg_cycle = target.dkg_cycle,
        epoch = %next_epoch,
        activated_at_height,
        dkg_output_hash = %dkg_manager::dkg_output_hash(&complete.output),
        "persisted completed DKG state before activation"
    );
    Ok(())
}

fn save_pending_dkg_boundary(
    storage_dir: &std::path::Path,
    snapshot: &PendingDkgBoundarySnapshot,
) -> Result<()> {
    std::fs::create_dir_all(storage_dir)
        .wrap_err_with(|| format!("failed to create storage dir: {}", storage_dir.display()))?;
    let bytes = encode_pending_dkg_boundary_snapshot(snapshot)?;
    let tmp_path = storage_dir.join(DKG_PENDING_BOUNDARY_TMP_FILE);
    let final_path = pending_dkg_boundary_path(storage_dir);
    std::fs::write(&tmp_path, bytes)
        .wrap_err_with(|| format!("failed to write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &final_path).wrap_err_with(|| {
        format!(
            "failed to atomically install pending DKG boundary snapshot {}",
            final_path.display()
        )
    })?;
    Ok(())
}

fn load_pending_dkg_boundary(
    storage_dir: &std::path::Path,
) -> Result<Option<PendingDkgBoundarySnapshot>> {
    let path = pending_dkg_boundary_path(storage_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes =
        std::fs::read(&path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
    decode_pending_dkg_boundary_snapshot(&bytes).map(Some)
}

fn clear_pending_dkg_boundary(storage_dir: &std::path::Path) {
    let _ = std::fs::remove_file(pending_dkg_boundary_path(storage_dir));
    let _ = std::fs::remove_file(storage_dir.join(DKG_PENDING_BOUNDARY_TMP_FILE));
}

fn pending_boundary_is_finalized(
    pending: &PendingDkgBoundarySnapshot,
    recovered_boundary: Option<&(u64, DkgBoundaryArtifact)>,
) -> bool {
    recovered_boundary.is_some_and(|(height, artifact)| {
        artifact == &pending.artifact
            || (*height >= pending.activated_at_height
                && artifact.dkg_cycle >= pending.artifact.dkg_cycle)
    })
}

fn validate_pending_boundary_snapshot(
    snapshot: &PendingDkgBoundarySnapshot,
    local_output: &Output<MinSig, bls12381::PublicKey>,
    provider: &(impl StateProviderFactory + BlockHashReader),
) -> Result<()> {
    let boundary_output = decode_boundary_output(&snapshot.artifact)
        .wrap_err("failed to decode pending DKG boundary output")?;
    dkg_manager::assert_canonical_output(
        local_output,
        &boundary_output,
        "pending boundary snapshot",
    )?;
    let frozen = match refresh_validator_set_at_height(provider, snapshot.artifact.freeze_height)? {
        FrozenValidatorSetRefresh::Ready { validator_set, .. } => validator_set,
        FrozenValidatorSetRefresh::PendingBlockHash => {
            return Err(eyre::eyre!(
                "pending DKG boundary freeze-height state unavailable at height {}; refusing unsafe recovery",
                snapshot.artifact.freeze_height
            ));
        }
    };
    let activated_validator_set = validator_set_for_dkg_output_players(local_output, &frozen)?;
    let rebuilt = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(snapshot.artifact.epoch),
        validator_set: &activated_validator_set,
        output: local_output,
        is_full_dkg: false,
        dkg_cycle: snapshot.artifact.dkg_cycle,
        freeze_height: snapshot.artifact.freeze_height,
        planned_activation_height: snapshot.artifact.planned_activation_height,
        vrf_material_version: snapshot.artifact.vrf_material_version,
        is_validator_set_change: snapshot.artifact.is_validator_set_change,
        tee_reshare_registrations: Vec::new(),
    })?;
    ensure!(
        rebuilt == snapshot.artifact,
        "pending DKG boundary snapshot does not match freeze-height validator set and DKG output"
    );
    Ok(())
}

fn recover_pending_dkg_boundary_snapshot(
    storage_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
    node: &OutbeFullNode,
    recovered_boundary: Option<&(u64, DkgBoundaryArtifact)>,
) -> Result<Option<PendingDkgBoundarySnapshot>> {
    let Some(snapshot) = load_pending_dkg_boundary(storage_dir)? else {
        return Ok(None);
    };
    if pending_boundary_is_finalized(&snapshot, recovered_boundary) {
        clear_pending_dkg_boundary(storage_dir);
        remove_pending_dkg_state(storage_dir);
        info!(
            storage_dir = %storage_dir.display(),
            activated_at_height = snapshot.activated_at_height,
            dkg_cycle = snapshot.artifact.dkg_cycle,
            "cleared stale pending DKG boundary snapshot after finalized boundary recovery"
        );
        return Ok(None);
    }

    let local_output = match load_pending_dkg_state(storage_dir, key_backend)?.or_else(|| {
        load_saved_dkg_state(storage_dir, key_backend)
            .ok()
            .flatten()
    }) {
        Some((_, _, output)) => output,
        None => {
            return Err(eyre::eyre!(
                "pending DKG boundary snapshot exists but no matching pending/saved DKG material is available"
            ));
        }
    };
    validate_pending_boundary_snapshot(&snapshot, &local_output, &node.provider)?;
    Ok(Some(snapshot))
}

fn startup_live_join_scan_height(
    execution_height: u64,
    consensus_finalized_height: u64,
    trust_el_head: bool,
) -> Result<u64> {
    if consensus_finalized_height == 0 {
        if !trust_el_head {
            ensure!(
                execution_height == 0,
                "startup live join found execution history at height {execution_height} but no durable consensus-finalized height; refusing to recover DKG artifacts from unfinalized execution head. Wait for consensus finalization evidence or use --testnet.trust-el-head for disaster recovery."
            );
        } else if execution_height > 0 {
            warn!(
                execution_height,
                "trusting EL head with no consensus-finalized height (--testnet.trust-el-head)"
            );
        }
        return Ok(0);
    }
    Ok(execution_height.min(consensus_finalized_height))
}

fn feed_finalized_dealer_logs_from_headers(
    provider: &impl HeaderProvider<Header = OutbeHeader>,
    next_scan_height: &mut u64,
    latest_height: u64,
    finalized_log_tx: &tokio::sync::mpsc::UnboundedSender<Bytes>,
) -> Result<()> {
    while *next_scan_height <= latest_height {
        if let Some(header) = provider
            .sealed_header(*next_scan_height)
            .map_err(|error| eyre::eyre!("failed to read header {}: {error}", *next_scan_height))?
        {
            let artifacts = decode_outbe_block_artifacts(header.header().inner.extra_data.as_ref())
                .map_err(|error| {
                    eyre::eyre!(
                        "failed to decode header artifacts at {}: {error}",
                        *next_scan_height
                    )
                })?;
            if let Some(ConsensusHeaderArtifact::DealerLog(bytes)) =
                artifacts.consensus_header_artifact
            {
                let _ = finalized_log_tx.send(bytes);
            }
        }
        *next_scan_height = next_scan_height.saturating_add(1);
    }
    Ok(())
}

fn replay_finalized_dealer_logs_into_manager(
    provider: &impl HeaderProvider<Header = OutbeHeader>,
    next_scan_height: &mut u64,
    latest_height: u64,
    dkg_manager: &DkgManagerMailbox,
) -> Result<()> {
    while *next_scan_height <= latest_height {
        if let Some(header) = provider
            .sealed_header(*next_scan_height)
            .map_err(|error| eyre::eyre!("failed to read header {}: {error}", *next_scan_height))?
        {
            let artifacts = decode_outbe_block_artifacts(header.header().inner.extra_data.as_ref())
                .map_err(|error| {
                    eyre::eyre!(
                        "failed to decode header artifacts at {}: {error}",
                        *next_scan_height
                    )
                })?;
            if matches!(
                artifacts.consensus_header_artifact.as_ref(),
                Some(ConsensusHeaderArtifact::DealerLog(_))
            ) {
                dkg_manager.note_finalized_header_artifact_at(
                    *next_scan_height,
                    header.hash(),
                    artifacts.consensus_header_artifact.as_ref(),
                );
            }
        }
        *next_scan_height = next_scan_height.saturating_add(1);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_startup_live_join_reshare<E, S, R, O>(
    ctx: &E,
    args: &ConsensusArgs,
    key_backend: &bls::KeyBackend,
    signing_key: &bls12381::PrivateKey,
    node: &OutbeFullNode,
    dkg_mux: &mut MuxHandle<S, R>,
    oracle: &mut O,
    bootnode_map: &BTreeMap<Vec<u8>, SocketAddr>,
    last_consensus_finalized_height: u64,
) -> Result<StartupLiveJoinResult>
where
    E: Clock,
    S: P2pSender<PublicKey = bls12381::PublicKey>,
    R: P2pReceiver<PublicKey = bls12381::PublicKey>,
    O: AddressableManager<PublicKey = bls12381::PublicKey>,
{
    let local_pk = commonware_cryptography::Signer::public_key(signing_key);
    let epoch_length_blocks = epoch_length_blocks_from_genesis(node)?;
    let dkg_rotation_params = DkgRotationParams::from_genesis(node, epoch_length_blocks);
    // Single-pass by design: startup live-join recovers from the immutable
    // finalized-height snapshot once and refuses to wait/retry on an unfinalized
    // head (hence no loop — the previous `loop` always broke or returned on its
    // first iteration).
    let (last_activation_height, previous_boundary) = {
        let last_execution_height = node.provider.last_block_number().map_err(|e| {
            eyre::eyre!("failed to get last block number for startup live join: {e}")
        })?;
        let recovery_height = startup_live_join_scan_height(
            last_execution_height,
            last_consensus_finalized_height,
            args.trust_el_head,
        )?;
        match recover_latest_boundary_artifact(&node.provider, recovery_height, dkg_rotation_params)
            .wrap_err("failed to recover latest DKG boundary for startup live join")?
        {
            Some(boundary) => boundary,
            None => {
                return Err(eyre::eyre!(
                    "startup live join could not recover a prior finalized DKG boundary at consensus-finalized height {}; refusing to wait on an immutable startup height snapshot or scan unfinalized execution head {}. Wait for durable finalized evidence or use a documented trusted-join flow.",
                    last_consensus_finalized_height,
                    last_execution_height
                ));
            }
        }
    };
    let previous_output = decode_boundary_output(&previous_boundary)?;
    let dkg_round = next_live_reshare_round(&previous_boundary);
    let freeze_height = dkg_rotation_params.freeze_height(last_activation_height);
    let planned_activation_height =
        dkg_rotation_params.planned_activation_height(last_activation_height);
    let activation_deadline =
        planned_activation_height.saturating_add(dkg_rotation_params.activation_grace_blocks);

    let (target_height, target_validator_set, target_participants) = loop {
        let current_height = node.provider.last_block_number().map_err(|error| {
            eyre::eyre!("failed to read latest block height for startup live join: {error}")
        })?;
        if current_height > activation_deadline {
            return Err(eyre::eyre!(
                "startup live join missed DKG activation window: current height {}, deadline {}",
                current_height,
                activation_deadline
            ));
        }

        if current_height < freeze_height {
            info!(
                current_height,
                freeze_height, "startup live join waiting for DKG freeze height"
            );
            ctx.sleep(Duration::from_secs(1)).await;
            continue;
        }

        let (target_validator_set, target_participants) =
            match refresh_validator_set_at_height(&node.provider, freeze_height) {
                Ok(FrozenValidatorSetRefresh::Ready {
                    validator_set,
                    participants,
                }) => (validator_set, participants),
                Ok(FrozenValidatorSetRefresh::PendingBlockHash) => {
                    info!(
                        current_height,
                        freeze_height, "startup live join waiting for freeze block hash"
                    );
                    ctx.sleep(Duration::from_secs(1)).await;
                    continue;
                }
                Err(error) => {
                    return Err(error)
                        .wrap_err("failed to read frozen validator set for startup live join");
                }
            };

        if target_participants.position(&local_pk).is_none() {
            info!(
                current_height,
                "startup live join waiting for local key to appear in active target set"
            );
            ctx.sleep(Duration::from_secs(1)).await;
            continue;
        }
        break (current_height, target_validator_set, target_participants);
    };

    let target_peer_map = build_peer_map(&target_validator_set, bootnode_map);
    let target_peer_count = target_peer_map.len();
    let chain_peer_set_id = p2p_oracle_chain_peer_set_id(freeze_height);
    let dkg_peer_set_id = p2p_oracle_dkg_peer_set_id(freeze_height);
    let _ = oracle.track(chain_peer_set_id, target_peer_map.clone());
    let _ = oracle.track(dkg_peer_set_id, target_peer_map);

    info!(
        target_validators = target_validator_set.public_keys.len(),
        target_peers = target_peer_count,
        chain_peer_set_id,
        dkg_peer_set_id,
        target_height,
        dkg_round,
        freeze_height,
        planned_activation_height,
        "startup live join entering player-only DKG reshare"
    );
    let (dkg_tx, dkg_rx) = dkg_mux
        .register(dkg_round)
        .await
        .map_err(|e| eyre::eyre!("failed to register startup live-join DKG subchannel: {e}"))?;
    let (finalized_log_tx, finalized_log_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut next_scan_height = freeze_height;
    let dkg_future = dkg_actor::run_initial_dkg(
        ctx,
        signing_key.clone(),
        target_participants,
        Some(previous_output),
        None,
        dkg_round,
        None,
        Some(finalized_log_rx),
        dkg_tx,
        dkg_rx,
    );
    tokio::pin!(dkg_future);
    let complete = loop {
        let mut scan_timer = Box::pin(ctx.sleep(STARTUP_JOIN_SCAN_INTERVAL));
        tokio::select! {
            result = &mut dkg_future => {
                break result.wrap_err("startup live-join DKG failed")?;
            }
            _ = &mut scan_timer => {
                let latest_execution_height = node.provider.last_block_number().map_err(|error| {
                    eyre::eyre!("failed to read latest block height while scanning startup live-join logs: {error}")
                })?;
                let latest_height = startup_live_join_scan_height(
                    latest_execution_height,
                    last_consensus_finalized_height,
                    args.trust_el_head,
                )?;
                feed_finalized_dealer_logs_from_headers(
                    &node.provider,
                    &mut next_scan_height,
                    latest_height,
                    &finalized_log_tx,
                )?;
            }
        }
    };

    let polynomial = complete.output.public().clone();
    let expected_vrf_group_public_key = vrf_group_public_key_hash(&polynomial);
    let (activated_at_height, activated_boundary) = loop {
        let current_height = node.provider.last_block_number().map_err(|error| {
            eyre::eyre!(
                "failed to read latest block height while waiting for startup live-join activation: {error}"
            )
        })?;
        let recovery_height = startup_live_join_scan_height(
            current_height,
            last_consensus_finalized_height,
            args.trust_el_head,
        )?;
        if let Some((height, boundary)) =
            recover_latest_boundary_artifact(&node.provider, recovery_height, dkg_rotation_params)
                .wrap_err("failed to recover startup live-join activation boundary")?
        {
            if boundary.dkg_cycle == dkg_round
                && boundary.vrf_group_public_key == expected_vrf_group_public_key
            {
                info!(
                    height,
                    dkg_round,
                    vrf_group_public_key = %expected_vrf_group_public_key,
                    "startup live-join boundary finalized; threshold material is current"
                );
                // Same activation-anchor normalization as `read_startup_dkg_snapshot`:
                // the artifact rides the FIRST new-epoch block (activation + 1), so
                // anchoring on the commit height would shift this node's whole
                // rotation schedule one block late vs the live committee.
                break (height.saturating_sub(1), boundary);
            }
        }

        if current_height > activation_deadline {
            return Err(eyre::eyre!(
                "startup live join DKG completed but matching BoundaryOutcome was not finalized before deadline: current height {}, deadline {}, dkg_round {}, vrf_group_public_key {}",
                current_height,
                activation_deadline,
                dkg_round,
                expected_vrf_group_public_key
            ));
        }

        info!(
            current_height,
            activation_deadline,
            dkg_round,
            vrf_group_public_key = %expected_vrf_group_public_key,
            "startup live join waiting for matching chain-finalized DKG boundary"
        );
        ctx.sleep(STARTUP_JOIN_SCAN_INTERVAL).await;
    };

    let canonical_output = decode_boundary_output(&activated_boundary)
        .wrap_err("failed to decode startup live-join activation boundary output")?;
    dkg_manager::assert_canonical_output(
        &complete.output,
        &canonical_output,
        &format!("startup live-join cycle {dkg_round}"),
    )?;
    let canonical_polynomial = canonical_output.public().clone();

    // Startup live-join runs `run_initial_dkg` in chain-finalized mode, so a finalize
    // failure can return a shareless (verifier) result. A startup joiner that can only
    // verify should instead use the `ThresholdMaterial::VerifierOnly` startup path
    // (provisioned public polynomial), so here we require a share. (Follow-up: make
    // `StartupLiveJoinResult.signing_share` optional → VerifierOnly for symmetric
    // startup resilience.)
    let signing_share = complete.share.ok_or_else(|| {
        eyre::eyre!(
            "startup live-join DKG completed without a signing share; join as a verifier via a \
             provisioned public polynomial instead"
        )
    })?;
    if let Some(ref keys_dir) = args.keys_dir {
        save_dkg_state(
            keys_dir,
            &signing_share,
            &canonical_polynomial,
            &canonical_output,
            key_backend,
        )?;
    }

    Ok(StartupLiveJoinResult {
        signing_share,
        polynomial: canonical_polynomial,
        output: canonical_output,
        activated_at_height,
        activated_boundary,
    })
}

/// Obtain threshold material (signing share + public polynomial).
///
/// Three paths (tried in order):
/// 1. **Saved DKG state** in `keys_dir` — restart precedence, wins over CLI
/// 2. **CLI args provided** — fallback for fresh bootstrap / manual provisioning
/// 3. **No material and no chain DKG history** — run the one-time interactive
///    genesis DKG ceremony over P2P (BLOCKING, no blocks)
/// 4. **No material or stale material on an existing chain** — wait for live
///    reshare instead of starting a new genesis DKG
///
/// Returns `(share, polynomial, previous_output, bootstrap_from_live_dkg)`.
///
/// `previous_output` is restored from persisted state when available so the
/// next live reshare can continue from the correct prior DKG output.
/// `bootstrap_from_live_dkg` is `true` only when this startup actually ran the
/// interactive initial DKG ceremony (path 3).
#[allow(clippy::too_many_arguments)]
async fn obtain_threshold_material(
    clock: &impl Clock,
    args: &ConsensusArgs,
    key_backend: &bls::KeyBackend,
    signing_key: &bls12381::PrivateKey,
    validator_set: &validators::ValidatorSet,
    startup_dkg_context: StartupDkgContext,
    dkg_sender: impl P2pSender<PublicKey = bls12381::PublicKey>,
    dkg_receiver: impl P2pReceiver<PublicKey = bls12381::PublicKey>,
) -> Result<ThresholdMaterial> {
    if !args.force_dkg {
        // Path 1: Try loading saved DKG state from keys_dir (restart precedence).
        // On ordinary restart, saved local DKG state wins over CLI bootstrap material.
        if let Some(ref keys_dir) = args.keys_dir {
            let mut saved_state_error: Option<eyre::Report> = None;
            let saved_state = match load_saved_dkg_state(keys_dir, key_backend) {
                Ok(state) => state,
                Err(error) => {
                    warn!(
                        %error,
                        keys_dir = %keys_dir.display(),
                        "saved DKG state is incomplete or corrupt; checking pending DKG state before failing"
                    );
                    saved_state_error = Some(error);
                    None
                }
            };
            if let Some((signing_share, polynomial, output)) = saved_state {
                if vrf_material_matches_recovered_boundary(&polynomial, startup_dkg_context)
                    && dkg_output_matches_recovered_boundary(&output, startup_dkg_context)
                {
                    info!(
                        keys_dir = %keys_dir.display(),
                        vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                        "threshold material ready from saved DKG state"
                    );
                    return Ok(ThresholdMaterial::Ready {
                        signing_share,
                        polynomial,
                        last_dkg_output: Some(output),
                        bootstrap_from_live_dkg: false,
                    });
                }
                warn!(
                    keys_dir = %keys_dir.display(),
                    local_vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                    local_dkg_output_hash = %dkg_manager::dkg_output_hash(&output),
                    recovered_vrf_group_public_key = ?startup_dkg_context.recovered_vrf_group_public_key,
                    recovered_dkg_output_hash = ?startup_dkg_context.recovered_dkg_output_hash,
                    "saved DKG material is stale for the latest finalized boundary; checking pending DKG state"
                );
            }

            let pending_state = match load_pending_dkg_state(keys_dir, key_backend) {
                Ok(state) => state,
                Err(error) => {
                    warn!(
                        %error,
                        keys_dir = %keys_dir.display(),
                        "pending DKG state is incomplete or corrupt; ignoring pending material"
                    );
                    None
                }
            };
            if let Some((signing_share, polynomial, output)) = pending_state {
                if startup_dkg_context.recovered_dkg_output_hash.is_some()
                    && vrf_material_matches_recovered_boundary(&polynomial, startup_dkg_context)
                    && dkg_output_matches_recovered_boundary(&output, startup_dkg_context)
                {
                    if startup_dkg_context.recovered_boundary_finalized {
                        save_dkg_state(keys_dir, &signing_share, &polynomial, &output, key_backend)
                            .wrap_err(
                                "failed to promote pending DKG state after boundary finalization",
                            )?;
                        remove_pending_dkg_state(keys_dir);
                        clear_pending_dkg_boundary(keys_dir);
                        info!(
                            keys_dir = %keys_dir.display(),
                            vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                            dkg_output_hash = %dkg_manager::dkg_output_hash(&output),
                            "threshold material ready from promoted pending DKG state"
                        );
                    } else {
                        info!(
                            keys_dir = %keys_dir.display(),
                            vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                            dkg_output_hash = %dkg_manager::dkg_output_hash(&output),
                            "threshold material ready from durable pending DKG state and boundary snapshot"
                        );
                    }
                    return Ok(ThresholdMaterial::Ready {
                        signing_share,
                        polynomial,
                        last_dkg_output: Some(output),
                        bootstrap_from_live_dkg: false,
                    });
                }
                warn!(
                    keys_dir = %keys_dir.display(),
                    local_vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                    local_dkg_output_hash = %dkg_manager::dkg_output_hash(&output),
                    recovered_vrf_group_public_key = ?startup_dkg_context.recovered_vrf_group_public_key,
                    recovered_dkg_output_hash = ?startup_dkg_context.recovered_dkg_output_hash,
                    "pending DKG material is not finalized for the latest boundary"
                );
            }

            if let Some(error) = saved_state_error {
                return Err(error).wrap_err(
                    "saved DKG state failed to load and pending state could not be promoted",
                );
            }

            if startup_dkg_context.has_chain_finalized_dkg_boundary()
                && !startup_dkg_context.recovered_boundary_finalized
            {
                return Err(eyre::eyre!(
                    "pending DKG boundary snapshot was recovered but matching DKG material is unavailable"
                ));
            }

            if startup_dkg_context.has_chain_finalized_dkg_boundary() {
                // A verifier-join (no signing share, has --public-polynomial) must NOT
                // enter the startup live-join reshare path on RESTART: that path waits
                // for the freeze height WITHOUT driving sync, so a restarted-but-behind
                // verifier deadlocks (current_height frozen below freeze_height forever —
                // it can never reach the freeze without the engine running). Instead it
                // falls through to the VerifierOnly path below, loads its polynomial,
                // and syncs as a verifier; the running epoch loop then handles any
                // reshare once it is synced + in the frozen target (the finalized-follower
                // path for a non-staked full-node, the participant path once it is a
                // PENDING frozen-target player). Only a share-holding node (a
                // reconnecting signer) needs the immediate startup live-join reshare.
                if !(args.signing_share.is_none() && args.public_polynomial.is_some()) {
                    return Ok(ThresholdMaterial::StartupLiveJoinRequired);
                }
            }
        }

        // Path 2: Load from CLI args (fresh bootstrap / manual provisioning).
        if let (Some(share_path), Some(poly_path)) = (&args.signing_share, &args.public_polynomial)
        {
            let signing_share = bls::load_signing_share(share_path, key_backend)
                .wrap_err("failed to load BLS signing share")?;
            let polynomial = bls::load_public_polynomial(poly_path, key_backend)
                .wrap_err("failed to load BLS public polynomial")?;
            let cli_dkg_output = if let Some(output_path) = &args.dkg_output {
                let output = bls::load_dkg_output(output_path, key_backend)
                    .wrap_err("failed to load BLS DKG output")?;
                bls::validate_dkg_triplet(&signing_share, &polynomial, &output)
                    .wrap_err("CLI DKG material triplet is inconsistent")?;
                Some(output)
            } else {
                None
            };

            if startup_dkg_context.recovered_dkg_output_hash.is_some() && cli_dkg_output.is_none() {
                warn!(
                    share_path = %share_path.display(),
                    poly_path = %poly_path.display(),
                    recovered_dkg_output_hash = ?startup_dkg_context.recovered_dkg_output_hash,
                    "CLI DKG material lacks required output for recovered chain boundary; waiting for live reshare"
                );
                return Ok(ThresholdMaterial::StartupLiveJoinRequired);
            }

            if !vrf_material_matches_recovered_boundary(&polynomial, startup_dkg_context)
                || cli_dkg_output.as_ref().is_some_and(|output| {
                    !dkg_output_matches_recovered_boundary(output, startup_dkg_context)
                })
            {
                warn!(
                    share_path = %share_path.display(),
                    poly_path = %poly_path.display(),
                    local_vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                    local_dkg_output_hash = ?cli_dkg_output.as_ref().map(dkg_manager::dkg_output_hash),
                    recovered_vrf_group_public_key = ?startup_dkg_context.recovered_vrf_group_public_key,
                    recovered_dkg_output_hash = ?startup_dkg_context.recovered_dkg_output_hash,
                    "CLI DKG material is stale for the latest finalized boundary; waiting for live reshare"
                );
                return Ok(ThresholdMaterial::StartupLiveJoinRequired);
            }

            info!(
                vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                "threshold material ready from CLI args"
            );
            return Ok(ThresholdMaterial::Ready {
                signing_share,
                polynomial,
                last_dkg_output: cli_dkg_output,
                bootstrap_from_live_dkg: false,
            });
        }

        // Path 2b: Verifier-join — public group material (--consensus.public-polynomial
        // + --consensus.dkg-output) WITHOUT a signing share. The node runs the consensus
        // engine in verifier mode (follow/verify finalized blocks → sync its execution
        // layer) and acquires a share at the next reshare. See ThresholdMaterial::VerifierOnly.
        if args.signing_share.is_none() {
            if let (Some(poly_path), Some(output_path)) =
                (&args.public_polynomial, &args.dkg_output)
            {
                let polynomial = bls::load_public_polynomial(poly_path, key_backend)
                    .wrap_err("failed to load BLS public polynomial for verifier-join")?;
                let output = bls::load_dkg_output(output_path, key_backend)
                    .wrap_err("failed to load BLS DKG output for verifier-join")?;
                info!(
                    vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                    "verifier-join: no threshold share; running consensus in VERIFIER mode \
                     (follow + verify) until the next reshare grants a share"
                );
                return Ok(ThresholdMaterial::VerifierOnly {
                    polynomial,
                    last_dkg_output: Some(output),
                });
            }
        }
    } else {
        info!("--testnet.force-dkg: ignoring saved and CLI DKG material");
    }

    // Path 3: Run interactive DKG ceremony.
    let local_pk = commonware_cryptography::Signer::public_key(signing_key);
    let startup_participants: commonware_utils::ordered::Set<bls12381::PublicKey> = validator_set
        .public_keys
        .clone()
        .into_iter()
        .try_collect()
        .map_err(|e| eyre::eyre!("invalid participant set: {e}"))?;
    let local_key_in_current_consensus_set = startup_participants.position(&local_pk).is_some();
    if args.force_dkg && !local_key_in_current_consensus_set {
        return Err(eyre::eyre!(
            "--testnet.force-dkg requires the local BLS key to be in the current active consensus set"
        ));
    }
    match startup_dkg_mode(
        startup_dkg_context,
        local_key_in_current_consensus_set,
        args.force_dkg,
    ) {
        StartupDkgMode::LiveJoinRequired => {
            warn!(
                last_execution_height = startup_dkg_context.last_execution_height,
                has_finalized_dkg_boundary = startup_dkg_context.has_chain_finalized_dkg_boundary(),
                local_key_in_current_consensus_set,
                "no current threshold material; startup live-join reshare is required"
            );
            return Ok(ThresholdMaterial::StartupLiveJoinRequired);
        }
        StartupDkgMode::InitialGenesisDkg => {}
    }

    info!("no threshold material available — running DKG ceremony (NO BLOCKS until complete)");

    let dkg_result = dkg_actor::run_initial_dkg(
        clock,
        signing_key.clone(),
        startup_participants,
        None, // initial: no previous output
        None, // initial: no previous share
        0,    // initial: round 0
        None,
        None,
        dkg_sender,
        dkg_receiver,
    )
    .await
    .wrap_err("DKG ceremony failed")?;

    let polynomial = dkg_result.output.public().clone();
    // Genesis bootstrap has NO chain carrier to authenticate a shareless verifier
    // polynomial, so completing without a share is fatal (a broken founding committee
    // must not be silently accepted). Only the chain-finalized reshare path may return
    // `None` → verifier; genesis passes no finalized-log receiver, so A5 keeps finalize
    // fatal and this is always `Some`.
    let signing_share = dkg_result.share.ok_or_else(|| {
        eyre::eyre!("initial genesis DKG completed without a signing share; cannot bootstrap")
    })?;
    info!(
        vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
        "initial DKG ceremony completed; threshold material ready"
    );

    // Save DKG state to keys_dir for future restarts.
    if let Some(ref keys_dir) = args.keys_dir {
        let save_result = if args.force_dkg && startup_dkg_context.last_execution_height > 0 {
            save_pending_dkg_state(
                keys_dir,
                &signing_share,
                &polynomial,
                &dkg_result.output,
                key_backend,
            )
        } else {
            save_dkg_state(
                keys_dir,
                &signing_share,
                &polynomial,
                &dkg_result.output,
                key_backend,
            )
        };
        if let Err(e) = save_result {
            warn!(
                ?e,
                "failed to save DKG state to disk (node will need to re-run DKG on restart)"
            );
        } else {
            info!(
                keys_dir = %keys_dir.display(),
                pending = args.force_dkg && startup_dkg_context.last_execution_height > 0,
                "saved DKG state to disk"
            );
        }
    } else {
        warn!("no --consensus.keys-dir set, DKG state will not be persisted");
    }

    info!("DKG ceremony complete — threshold material obtained via P2P");

    Ok(ThresholdMaterial::Ready {
        signing_share,
        polynomial,
        last_dkg_output: Some(dkg_result.output),
        bootstrap_from_live_dkg: true,
    })
}

fn validator_set_for_dkg_output_players(
    output: &Output<MinSig, bls12381::PublicKey>,
    source: &validators::ValidatorSet,
) -> Result<validators::ValidatorSet> {
    let players = output.players();
    let mut public_keys = Vec::with_capacity(players.len());
    let mut addresses = Vec::with_capacity(players.len());
    let mut p2p_addresses = Vec::with_capacity(players.len());
    for player in players.iter() {
        let Some(idx) = source.public_keys.iter().position(|pk| pk == player) else {
            return Err(eyre::eyre!(
                "DKG output contains a player absent from the frozen validator set"
            ));
        };
        public_keys.push(source.public_keys[idx].clone());
        addresses.push(source.addresses[idx]);
        p2p_addresses.push(source.p2p_addresses[idx].clone());
    }
    Ok(validators::ValidatorSet {
        public_keys,
        addresses,
        p2p_addresses,
    })
}

fn participants_from_validator_set(
    validator_set: &validators::ValidatorSet,
) -> Result<commonware_utils::ordered::Set<bls12381::PublicKey>> {
    validator_set
        .public_keys
        .clone()
        .into_iter()
        .try_collect()
        .map_err(|e| eyre::eyre!("invalid DKG output participant set: {e}"))
}

fn validate_dkg_output_players_exact(
    output: &Output<MinSig, bls12381::PublicKey>,
    validator_set: &validators::ValidatorSet,
) -> Result<()> {
    let players = output.players();
    ensure!(
        players.len() == validator_set.public_keys.len(),
        "DKG output player count {} does not match validator set size {}",
        players.len(),
        validator_set.public_keys.len()
    );
    for public_key in &validator_set.public_keys {
        ensure!(
            players.position(public_key).is_some(),
            "validator set public key is missing from DKG output players"
        );
    }
    Ok(())
}

pub(crate) const fn p2p_oracle_chain_peer_set_id(height: u64) -> u64 {
    height.saturating_mul(2)
}

pub(crate) const fn p2p_oracle_dkg_peer_set_id(height: u64) -> u64 {
    p2p_oracle_chain_peer_set_id(height).saturating_add(1)
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

/// Refresh the target validator set from frozen EVM state.
///
/// Called at freeze_height so dynamically added/removed validators are applied
/// from the same historical state on every node.
fn refresh_validator_set_at_height(
    provider: &(impl StateProviderFactory + BlockHashReader),
    freeze_height: u64,
) -> Result<FrozenValidatorSetRefresh> {
    let Some(block_hash) = provider.block_hash(freeze_height).map_err(|e| {
        eyre::eyre!("failed to get block hash at freeze height {freeze_height}: {e}")
    })?
    else {
        return Ok(FrozenValidatorSetRefresh::PendingBlockHash);
    };
    let state = provider
        .state_by_block_hash(block_hash)
        .map_err(|e| eyre::eyre!("failed to get state at freeze height {freeze_height}: {e}"))?;
    // The reshare TARGET (next_players) is ACTIVE∪PENDING: ACTIVE members stay and
    // PENDING joiners are activated by this ceremony. EXITING validators are excluded
    // (the reshare removes them). Using the reshare-target reader — not the ACTIVE-only
    // voting reader — is what lets a staked PENDING joiner receive a share.
    let new_set = validators::read_reshare_target_from_state(&state)
        .wrap_err("failed to read reshare target set from frozen EVM state")?;

    let participants: commonware_utils::ordered::Set<bls12381::PublicKey> = new_set
        .public_keys
        .clone()
        .into_iter()
        .try_collect()
        .map_err(|e| eyre::eyre!("invalid participant set after refresh: {e}"))?;

    Ok(FrozenValidatorSetRefresh::Ready {
        validator_set: new_set,
        participants,
    })
}

type PersistedDkgState = (Share, Sharing<MinSig>, Output<MinSig, bls12381::PublicKey>);

#[allow(clippy::too_many_arguments)]
fn load_dkg_state_files(
    storage_dir: &std::path::Path,
    share_file: &str,
    polynomial_file: &str,
    output_file: &str,
    key_backend: &bls::KeyBackend,
    label: &str,
) -> Result<Option<PersistedDkgState>> {
    let share_path = storage_dir.join(share_file);
    let poly_path = storage_dir.join(polynomial_file);
    let output_path = storage_dir.join(output_file);

    let has_share = share_path.exists();
    let has_poly = poly_path.exists();
    let has_output = output_path.exists();

    if !has_share && !has_poly && !has_output {
        return Ok(None);
    }

    ensure!(
        has_share && has_poly && has_output,
        "{label} DKG state is incomplete in {}: expected {}, {}, and {}",
        storage_dir.display(),
        share_file,
        polynomial_file,
        output_file,
    );

    let signing_share = bls::load_signing_share(&share_path, key_backend)
        .wrap_err_with(|| format!("failed to load BLS signing share from {label} DKG state"))?;
    let polynomial = bls::load_public_polynomial(&poly_path, key_backend)
        .wrap_err_with(|| format!("failed to load BLS public polynomial from {label} DKG state"))?;
    let output = bls::load_dkg_output(&output_path, key_backend)
        .wrap_err_with(|| format!("failed to load BLS DKG output from {label} DKG state"))?;
    bls::validate_dkg_triplet(&signing_share, &polynomial, &output)
        .wrap_err_with(|| format!("{label} DKG state triplet is inconsistent"))?;

    Ok(Some((signing_share, polynomial, output)))
}

/// Load finalized DKG results from disk for crash recovery.
#[allow(clippy::type_complexity)]
fn load_saved_dkg_state(
    storage_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
) -> Result<Option<PersistedDkgState>> {
    load_dkg_state_files(
        storage_dir,
        DKG_SHARE_FILE,
        DKG_POLYNOMIAL_FILE,
        DKG_OUTPUT_FILE,
        key_backend,
        "saved",
    )
}

fn load_pending_dkg_state(
    storage_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
) -> Result<Option<PersistedDkgState>> {
    load_dkg_state_files(
        storage_dir,
        DKG_PENDING_SHARE_FILE,
        DKG_PENDING_POLYNOMIAL_FILE,
        DKG_PENDING_OUTPUT_FILE,
        key_backend,
        "pending",
    )
}

#[allow(clippy::too_many_arguments)]
fn save_dkg_state_files(
    storage_dir: &std::path::Path,
    share_file: &str,
    polynomial_file: &str,
    output_file: &str,
    share: &Share,
    polynomial: &Sharing<MinSig>,
    output: &Output<MinSig, bls12381::PublicKey>,
    key_backend: &bls::KeyBackend,
) -> Result<()> {
    std::fs::create_dir_all(storage_dir)
        .wrap_err_with(|| format!("failed to create storage dir: {}", storage_dir.display()))?;

    bls::save_signing_share(&storage_dir.join(share_file), share, key_backend)
        .wrap_err("failed to save DKG signing share")?;

    bls::save_public_polynomial(&storage_dir.join(polynomial_file), polynomial, key_backend)
        .wrap_err("failed to save DKG public polynomial")?;

    bls::save_dkg_output(&storage_dir.join(output_file), output, key_backend)
        .wrap_err("failed to save DKG output artifact")?;

    Ok(())
}

/// Save finalized DKG results to disk for crash recovery.
fn save_dkg_state(
    storage_dir: &std::path::Path,
    share: &Share,
    polynomial: &Sharing<MinSig>,
    output: &Output<MinSig, bls12381::PublicKey>,
    key_backend: &bls::KeyBackend,
) -> Result<()> {
    save_dkg_state_files(
        storage_dir,
        DKG_SHARE_FILE,
        DKG_POLYNOMIAL_FILE,
        DKG_OUTPUT_FILE,
        share,
        polynomial,
        output,
        key_backend,
    )
}

fn save_pending_dkg_state(
    storage_dir: &std::path::Path,
    share: &Share,
    polynomial: &Sharing<MinSig>,
    output: &Output<MinSig, bls12381::PublicKey>,
    key_backend: &bls::KeyBackend,
) -> Result<()> {
    save_dkg_state_files(
        storage_dir,
        DKG_PENDING_SHARE_FILE,
        DKG_PENDING_POLYNOMIAL_FILE,
        DKG_PENDING_OUTPUT_FILE,
        share,
        polynomial,
        output,
        key_backend,
    )
}

fn remove_pending_dkg_state(storage_dir: &std::path::Path) {
    for file in [
        DKG_PENDING_SHARE_FILE,
        DKG_PENDING_POLYNOMIAL_FILE,
        DKG_PENDING_OUTPUT_FILE,
    ] {
        let _ = std::fs::remove_file(storage_dir.join(file));
    }
}

#[cfg(test)]
#[path = "stack_tests.rs"]
mod tests;
