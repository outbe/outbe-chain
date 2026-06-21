use std::{
    collections::btree_map::Entry,
    collections::{BTreeMap, VecDeque},
    num::NonZeroU32,
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy_primitives::{keccak256, Address, Bytes, B256};
use commonware_codec::Read as _;
use commonware_consensus::types::Epoch;
use commonware_cryptography::bls12381::{
    self,
    dkg::feldman_desmedt::{observe, DealerLog, Info, Logs, Output, SignedDealerLog},
    primitives::{
        sharing::{Mode, Sharing},
        variant::MinSig,
    },
};
use commonware_parallel::Sequential;
use commonware_utils::{
    ordered::{Quorum, Set},
    N3f1,
};
use eyre::{ensure, Result, WrapErr};
use outbe_primitives::{
    consensus::{DkgBoundaryArtifact, ReshareResult, TeeReshareRegistration},
    reshare_artifact::{encode_boundary_artifact, ConsensusHeaderArtifact},
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::{config, util::rate_limit::LogRateLimiter, validators::ValidatorSet};

#[derive(Clone, Debug)]
pub struct Mailbox {
    inner: Arc<Mutex<State>>,
    duplicate_dealer_log_limiter: Arc<LogRateLimiter>,
}

pub const BOUNDARY_STATUS_CACHE_SIZE: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommittedDkgBoundary {
    pub artifact: DkgBoundaryArtifact,
    pub artifact_hash: B256,
    pub block_number: u64,
    pub block_hash: B256,
}

// `BoundaryCommitted` carries the full committed boundary; the other variants are
// unit. Boxing it would ripple through every match/construct site for a status
// enum that is held briefly per epoch — not worth it for the stack-size delta.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BoundaryStatus {
    NoBoundarySeen,
    BoundaryCommitted(CommittedDkgBoundary),
    Conflict,
}

#[derive(Clone, Debug)]
struct BoundaryStatusCacheEntry {
    query_artifact_hash: B256,
    status: BoundaryStatus,
}

#[derive(Clone, Debug)]
struct CeremonyState {
    epoch: Epoch,
    info: Info<MinSig, bls12381::PublicKey>,
    max_players: NonZeroU32,
    dealers: Set<bls12381::PublicKey>,
    local_dealer_log: Option<Bytes>,
    pending_dealer_logs: BTreeMap<bls12381::PublicKey, Bytes>,
    finalized_dealer_log_tx: Option<mpsc::UnboundedSender<Bytes>>,
    finalized_dealer_logs: BTreeMap<bls12381::PublicKey, DealerLog<MinSig, bls12381::PublicKey>>,
    canonical_output: Option<Output<MinSig, bls12381::PublicKey>>,
}

#[derive(Clone, Debug, Default)]
struct State {
    pending_boundary: Option<DkgBoundaryArtifact>,
    committed_boundary: Option<CommittedDkgBoundary>,
    boundary_status_cache: BTreeMap<B256, BoundaryStatusCacheEntry>,
    boundary_status_lru: VecDeque<B256>,
    ceremony: Option<CeremonyState>,
}

#[derive(Debug)]
struct VerifiedDealerLog {
    dealer: bls12381::PublicKey,
    log: DealerLog<MinSig, bls12381::PublicKey>,
}

enum PendingDealerLogOutcome {
    Stored,
    DuplicateSame,
    DuplicateDifferent { dealer: bls12381::PublicKey },
    IgnoredCanonical,
}

impl Mailbox {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn boundary_artifact_hash(artifact: &DkgBoundaryArtifact) -> Result<B256> {
        let bytes = encode_boundary_artifact(artifact)
            .map_err(|error| eyre::eyre!("failed to encode DKG boundary artifact: {error}"))?;
        Ok(keccak256(bytes.as_ref()))
    }

    fn cache_boundary_status(
        state: &mut State,
        parent_hash: B256,
        query_artifact_hash: B256,
        status: BoundaryStatus,
    ) {
        state
            .boundary_status_lru
            .retain(|hash| *hash != parent_hash);
        state.boundary_status_lru.push_back(parent_hash);
        state.boundary_status_cache.insert(
            parent_hash,
            BoundaryStatusCacheEntry {
                query_artifact_hash,
                status,
            },
        );

        while state.boundary_status_cache.len() > BOUNDARY_STATUS_CACHE_SIZE {
            let Some(victim) = state.boundary_status_lru.pop_front() else {
                break;
            };
            state.boundary_status_cache.remove(&victim);
        }
    }

    fn clear_boundary_status_cache_inner(state: &mut State) {
        state.boundary_status_cache.clear();
        state.boundary_status_lru.clear();
    }

    pub fn cached_boundary_status(
        &self,
        parent_hash: B256,
        query_artifact_hash: B256,
    ) -> Option<BoundaryStatus> {
        self.with_state(|state| {
            let status = state
                .boundary_status_cache
                .get(&parent_hash)
                .filter(|entry| entry.query_artifact_hash == query_artifact_hash)
                .map(|entry| entry.status.clone());
            if status.is_some() {
                state
                    .boundary_status_lru
                    .retain(|hash| *hash != parent_hash);
                state.boundary_status_lru.push_back(parent_hash);
            }
            status
        })
    }

    pub fn record_boundary_status(
        &self,
        parent_hash: B256,
        query_artifact_hash: B256,
        status: BoundaryStatus,
    ) {
        self.with_state(|state| {
            Self::cache_boundary_status(state, parent_hash, query_artifact_hash, status);
        });
    }

    pub fn evict_boundary_status(&self, parent_hash: B256) -> bool {
        self.with_state(|state| {
            state
                .boundary_status_lru
                .retain(|hash| *hash != parent_hash);
            state.boundary_status_cache.remove(&parent_hash).is_some()
        })
    }

    pub fn clear_boundary_status_cache(&self) {
        self.with_state(Self::clear_boundary_status_cache_inner);
    }

    pub fn note_bootstrap_outcome(&self, artifact: DkgBoundaryArtifact) {
        self.with_state(|state| {
            state.pending_boundary = Some(artifact);
            state.committed_boundary = None;
            Self::clear_boundary_status_cache_inner(state);
        });
    }

    pub fn note_ceremony_started(
        &self,
        epoch: Epoch,
        round: u64,
        previous_output: Option<Output<MinSig, bls12381::PublicKey>>,
        participants: Set<bls12381::PublicKey>,
    ) -> Result<()> {
        self.note_ceremony_started_with_finalized_log_tx(
            epoch,
            round,
            previous_output,
            participants,
            None,
        )
    }

    pub fn note_ceremony_started_with_finalized_log_tx(
        &self,
        epoch: Epoch,
        round: u64,
        previous_output: Option<Output<MinSig, bls12381::PublicKey>>,
        participants: Set<bls12381::PublicKey>,
        finalized_dealer_log_tx: Option<mpsc::UnboundedSender<Bytes>>,
    ) -> Result<()> {
        let max_players = NonZeroU32::new(participants.len() as u32)
            .ok_or_else(|| eyre::eyre!("DKG ceremony requires at least one participant"))?;
        let dealers = previous_output
            .as_ref()
            .map(|output| output.players().clone())
            .unwrap_or_else(|| participants.clone());
        let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
            &config::outbe_app_namespace(),
            round,
            previous_output,
            Mode::NonZeroCounter,
            dealers.clone(),
            participants.clone(),
        )
        .wrap_err("failed to build DKG ceremony info")?;

        self.with_state(|state| {
            state.ceremony = Some(CeremonyState {
                epoch,
                info,
                max_players,
                dealers,
                local_dealer_log: None,
                pending_dealer_logs: BTreeMap::new(),
                finalized_dealer_log_tx,
                finalized_dealer_logs: BTreeMap::new(),
                canonical_output: None,
            });
        });
        Ok(())
    }

    pub fn note_local_dealer_log(&self, epoch: Epoch, bytes: Bytes) -> Result<()> {
        let dealer = self.verify_dealer_log_sync(epoch, bytes.as_ref())?;
        self.with_state(|state| {
            if let Some(ceremony) = state.ceremony.as_mut() {
                if ceremony.epoch == epoch {
                    ceremony.pending_dealer_logs.remove(&dealer);
                    ceremony.local_dealer_log = Some(bytes);
                }
            }
        });
        Ok(())
    }

    pub fn note_ceremony_completed(&self, artifact: DkgBoundaryArtifact) {
        self.with_state(|state| {
            state.pending_boundary = Some(artifact);
            state.committed_boundary = None;
            Self::clear_boundary_status_cache_inner(state);
            if let Some(ceremony) = state.ceremony.as_mut() {
                ceremony.local_dealer_log = None;
                ceremony.pending_dealer_logs.clear();
            }
        });
    }

    pub fn note_recovered_pending_boundary(&self, artifact: DkgBoundaryArtifact) {
        self.with_state(|state| {
            state.pending_boundary = Some(artifact);
            state.committed_boundary = None;
            Self::clear_boundary_status_cache_inner(state);
        });
    }

    /// Return the pending boundary for `epoch`.
    ///
    /// Proposer/verifier validity uses this together with parent-chain
    /// evidence. The pending artifact alone must not decide whether a block is
    /// valid; the caller must derive emission/duplication from the parent
    /// ancestry snapshot.
    pub async fn pending_boundary_artifact(&self, epoch: Epoch) -> Option<DkgBoundaryArtifact> {
        self.with_state(|state| {
            state
                .pending_boundary
                .clone()
                .filter(|artifact| artifact.epoch == epoch.get())
        })
    }

    pub async fn take_committed_boundary_artifact(&self) -> Option<DkgBoundaryArtifact> {
        self.with_state(|state| {
            let pending = state.pending_boundary.as_ref()?;
            let committed = state.committed_boundary.as_ref()?;
            if &committed.artifact != pending {
                return None;
            }
            let committed = state.committed_boundary.take()?;
            state.pending_boundary = None;
            Some(committed.artifact)
        })
    }

    pub async fn get_dealer_log(&self, epoch: Epoch) -> Option<Bytes> {
        self.with_state(|state| {
            let ceremony = state
                .ceremony
                .as_ref()
                .filter(|ceremony| ceremony.epoch == epoch)?;
            ceremony.local_dealer_log.clone().or_else(|| {
                ceremony
                    .pending_dealer_logs
                    .iter()
                    .next()
                    .map(|(_, bytes)| bytes.clone())
            })
        })
    }

    pub fn canonical_output(&self, epoch: Epoch) -> Option<Output<MinSig, bls12381::PublicKey>> {
        self.with_state(|state| {
            state
                .ceremony
                .as_ref()
                .filter(|ceremony| ceremony.epoch == epoch)
                .and_then(|ceremony| ceremony.canonical_output.clone())
        })
    }

    /// Verify a carried boundary against the pending boundary only. This does
    /// not decide whether the boundary is required; callers must separately
    /// derive that from the parent chain snapshot.
    pub async fn verify_pending_boundary_artifact(
        &self,
        epoch: Epoch,
        artifact: &DkgBoundaryArtifact,
    ) -> Result<()> {
        let expected = self
            .with_state(|state| state.pending_boundary.clone())
            .ok_or_else(|| eyre::eyre!("no pending DKG boundary artifact"))?;
        ensure!(
            epoch.get() == artifact.epoch,
            "boundary artifact epoch {} does not match verify epoch {}",
            artifact.epoch,
            epoch.get()
        );
        ensure!(
            expected == *artifact,
            "carried DKG boundary artifact does not match local expectation"
        );
        Ok(())
    }

    pub async fn verify_dealer_log(
        &self,
        epoch: Epoch,
        bytes: Vec<u8>,
    ) -> Result<bls12381::PublicKey> {
        self.verify_dealer_log_sync(epoch, &bytes)
    }

    pub fn note_pending_dealer_log(&self, epoch: Epoch, bytes: Bytes) -> Result<()> {
        let dealer = self.verify_dealer_log_sync(epoch, bytes.as_ref())?;
        let outcome = self.with_state(|state| {
            let Some(ceremony) = state
                .ceremony
                .as_mut()
                .filter(|ceremony| ceremony.epoch == epoch)
            else {
                return PendingDealerLogOutcome::IgnoredCanonical;
            };

            if ceremony.finalized_dealer_logs.contains_key(&dealer) {
                return PendingDealerLogOutcome::IgnoredCanonical;
            }
            if ceremony.local_dealer_log.as_ref() == Some(&bytes) {
                return PendingDealerLogOutcome::DuplicateSame;
            }

            match ceremony.pending_dealer_logs.entry(dealer.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(bytes);
                    PendingDealerLogOutcome::Stored
                }
                Entry::Occupied(entry) if entry.get() == &bytes => {
                    PendingDealerLogOutcome::DuplicateSame
                }
                Entry::Occupied(_) => PendingDealerLogOutcome::DuplicateDifferent {
                    dealer: dealer.clone(),
                },
            }
        });

        match outcome {
            PendingDealerLogOutcome::Stored => {
                debug!(?dealer, "recorded pending P2P DKG dealer log candidate");
            }
            PendingDealerLogOutcome::DuplicateSame => {
                debug!(?dealer, "ignoring duplicate pending P2P DKG dealer log");
            }
            PendingDealerLogOutcome::DuplicateDifferent { dealer } => {
                if let Some(suppressed_since_last) = self.duplicate_dealer_log_limiter.check() {
                    warn!(
                        ?dealer,
                        suppressed_since_last, "rejecting conflicting pending P2P DKG dealer log"
                    );
                }
            }
            PendingDealerLogOutcome::IgnoredCanonical => {
                debug!(
                    ?dealer,
                    "ignoring pending P2P DKG dealer log for already-finalized dealer"
                );
            }
        }
        Ok(())
    }

    pub fn note_finalized_header_artifact(&self, artifact: Option<&ConsensusHeaderArtifact>) {
        self.note_finalized_header_artifact_at(0, B256::ZERO, artifact);
    }

    pub fn note_finalized_header_artifact_at(
        &self,
        block_number: u64,
        block_hash: B256,
        artifact: Option<&ConsensusHeaderArtifact>,
    ) {
        self.with_state(|state| match artifact {
            Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) => {
                match Self::boundary_artifact_hash(boundary) {
                    Ok(artifact_hash) => {
                        let committed = CommittedDkgBoundary {
                            artifact: boundary.clone(),
                            artifact_hash,
                            block_number,
                            block_hash,
                        };
                        if state.pending_boundary.as_ref() == Some(boundary) {
                            state.committed_boundary = Some(committed.clone());
                        }
                        if block_hash != B256::ZERO {
                            Self::cache_boundary_status(
                                state,
                                block_hash,
                                artifact_hash,
                                BoundaryStatus::BoundaryCommitted(committed),
                            );
                        }
                    }
                    Err(error) => {
                        warn!(%error, "failed to record finalized DKG boundary status");
                    }
                }
                if let Some(ceremony) = state.ceremony.as_mut() {
                    ceremony.local_dealer_log = None;
                    ceremony.pending_dealer_logs.clear();
                }
            }
            Some(ConsensusHeaderArtifact::DealerLog(bytes)) => {
                if let Some(ceremony) = state.ceremony.as_mut() {
                    let verified = match verify_dealer_log_for_ceremony(ceremony, bytes.as_ref()) {
                        Ok(verified) => verified,
                        Err(error) => {
                            warn!(%error, "ignoring finalized DKG dealer log");
                            return;
                        }
                    };
                    if ceremony.local_dealer_log.as_ref() == Some(bytes) {
                        ceremony.local_dealer_log = None;
                    }
                    ceremony.pending_dealer_logs.remove(&verified.dealer);
                    if ceremony
                        .finalized_dealer_logs
                        .contains_key(&verified.dealer)
                    {
                        debug!(
                            dealer = ?verified.dealer,
                            "ignoring duplicate chain-finalized DKG dealer log"
                        );
                        return;
                    }
                    ceremony
                        .finalized_dealer_logs
                        .insert(verified.dealer.clone(), verified.log);
                    debug!(
                        dealer = ?verified.dealer,
                        logs = ceremony.finalized_dealer_logs.len(),
                        "recorded finalized DKG dealer log"
                    );
                    if let Some(tx) = &ceremony.finalized_dealer_log_tx {
                        if tx.send(bytes.clone()).is_err() {
                            debug!("active DKG actor is no longer accepting finalized dealer logs");
                        }
                    }
                    if ceremony.canonical_output.is_none() {
                        let mut logs =
                            Logs::<MinSig, bls12381::PublicKey, N3f1>::new(ceremony.info.clone());
                        for (dealer, log) in ceremony.finalized_dealer_logs.clone() {
                            logs.record(dealer, log);
                        }
                        match observe::<
                            MinSig,
                            bls12381::PublicKey,
                            N3f1,
                            commonware_cryptography::bls12381::Batch,
                        >(&mut rand_core::OsRng, logs, &Sequential)
                        {
                            Ok(output) => {
                                let output_hash = dkg_output_hash(&output);
                                let polynomial_hash = public_polynomial_hash(output.public());
                                info!(
                                    %output_hash,
                                    %polynomial_hash,
                                    logs = ceremony.finalized_dealer_logs.len(),
                                    dealers = output.dealers().len(),
                                    players = output.players().len(),
                                    "canonical DKG output reconstructed from finalized dealer logs"
                                );
                                ceremony.canonical_output = Some(output);
                            }
                            Err(error) => {
                                debug!(
                                    %error,
                                    logs = ceremony.finalized_dealer_logs.len(),
                                    "finalized DKG dealer logs do not yet produce an output"
                                );
                            }
                        }
                    }
                }
            }
            None => {}
        });
    }

    fn verify_dealer_log_sync(&self, epoch: Epoch, bytes: &[u8]) -> Result<bls12381::PublicKey> {
        self.with_state(|state| {
            let ceremony = state
                .ceremony
                .as_ref()
                .filter(|ceremony| ceremony.epoch == epoch)
                .ok_or_else(|| eyre::eyre!("no active DKG ceremony for epoch {}", epoch.get()))?;
            verify_dealer_log_for_ceremony(ceremony, bytes).map(|verified| verified.dealer)
        })
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        let mut state = match self.inner.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        f(&mut state)
    }
}

impl Default for Mailbox {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(State::default())),
            duplicate_dealer_log_limiter: Arc::new(LogRateLimiter::new(Duration::from_secs(5))),
        }
    }
}

fn verify_dealer_log_for_ceremony(
    ceremony: &CeremonyState,
    bytes: &[u8],
) -> Result<VerifiedDealerLog> {
    let mut reader = bytes;
    let signed_log = SignedDealerLog::<MinSig, bls12381::PrivateKey>::read_cfg(
        &mut reader,
        &ceremony.max_players,
    )
    .wrap_err("failed decoding signed dealer log")?;
    ensure!(reader.is_empty(), "trailing bytes after signed dealer log");

    let (dealer, log) = signed_log
        .check(&ceremony.info)
        .ok_or_else(|| eyre::eyre!("signed dealer log failed cryptographic verification"))?;
    ensure!(
        ceremony.dealers.index(&dealer).is_some(),
        "signed dealer log dealer is not in ceremony committee"
    );
    Ok(VerifiedDealerLog { dealer, log })
}

pub struct BoundaryArtifactInput<'a> {
    pub epoch: Epoch,
    pub validator_set: &'a ValidatorSet,
    pub output: &'a Output<MinSig, bls12381::PublicKey>,
    pub is_full_dkg: bool,
    pub dkg_cycle: u64,
    pub freeze_height: u64,
    pub planned_activation_height: u64,
    pub vrf_material_version: u64,
    pub is_validator_set_change: bool,
    /// New-committee per-validator TEE key re-registrations to carry in this
    /// boundary outcome after a tribute-offer reshare (R5). Empty for a
    /// non-reshare boundary; populated by the epoch loop from the TEE reshare
    /// result. The offer key is preserved across the reshare.
    pub tee_reshare_registrations: Vec<TeeReshareRegistration>,
}

pub fn build_boundary_artifact(input: BoundaryArtifactInput<'_>) -> Result<DkgBoundaryArtifact> {
    let BoundaryArtifactInput {
        epoch,
        validator_set,
        output,
        is_full_dkg,
        dkg_cycle,
        freeze_height,
        planned_activation_height,
        vrf_material_version,
        is_validator_set_change,
        tee_reshare_registrations,
    } = input;

    let participants = output.players();
    let polynomial = output.public();
    let mut new_active_set = Vec::with_capacity(participants.len());
    for bls_pk in participants.iter() {
        let Some(idx) = validator_set.public_keys.iter().position(|pk| pk == bls_pk) else {
            return Err(eyre::eyre!(
                "DKG output contains a player absent from the validator set"
            ));
        };
        new_active_set.push(validator_set.addresses[idx]);
    }

    let group_pk_bytes_vec = commonware_codec::Encode::encode(polynomial.public()).to_vec();
    let vrf_group_public_key = alloy_primitives::keccak256(&group_pk_bytes_vec);
    let active_set_hash = hash_active_set(&new_active_set);
    let target_set_hash =
        hash_target_set(&new_active_set, freeze_height, planned_activation_height);
    let outcome = encode_outcome(epoch, output, is_full_dkg);

    // V2 canonical committee snapshot identit.
    //
    // Per-entry MinPk pubkeys are encoded from the DKG `participants` list, in
    // the same Commonware participant-index order as `new_active_set` (we built
    // `new_active_set` by iterating `participants` above). Length must be 48
    // bytes — `bls12381::PublicKey` is MinPk-compressed.
    let encoded_pubkeys: Vec<Vec<u8>> = participants
        .iter()
        .map(|bls_pk| commonware_codec::Encode::encode(bls_pk).to_vec())
        .collect();
    // Single canonical builder (shared with the finalization actor/resolver and
    // the reporter). `new_active_set[i]` was built by iterating `participants`
    // above, so it is in the same order as `encoded_pubkeys[i]`. The proposer
    // carries the full polynomial commitment hash; it is not folded into
    // committee_set_hash_v2 (the executor re-derives it from the boundary
    // `outcome`).
    let committee_snapshot = crate::proof::build_committee_snapshot(
        &new_active_set,
        &encoded_pubkeys,
        vrf_material_version,
        group_pk_bytes_vec.clone(),
        public_polynomial_hash(output.public()),
    )
    .map_err(|e| eyre::eyre!("DKG boundary committee snapshot build failed: {e}"))?;
    let committee_set_hash = crate::proof::committee_set_hash_v2(epoch.get(), &committee_snapshot);

    Ok(DkgBoundaryArtifact {
        epoch: epoch.get(),
        dkg_cycle,
        freeze_height,
        planned_activation_height,
        target_set_hash,
        vrf_material_version,
        vrf_group_public_key,
        vrf_group_public_key_bytes: Bytes::from(group_pk_bytes_vec),
        committee_set_hash,
        is_validator_set_change,
        outcome,
        is_full_dkg,
        tee_recipient_pubkeys: Vec::new(),
        tee_reshare_registrations,
        reshare: ReshareResult {
            new_active_set,
            active_set_hash,
        },
    })
}

fn hash_active_set(addresses: &[Address]) -> B256 {
    let mut bytes = Vec::with_capacity(8 + addresses.len() * 20);
    bytes.extend_from_slice(&(addresses.len() as u64).to_be_bytes());
    for address in addresses {
        bytes.extend_from_slice(address.as_slice());
    }
    alloy_primitives::keccak256(bytes)
}

fn hash_target_set(
    addresses: &[Address],
    freeze_height: u64,
    planned_activation_height: u64,
) -> B256 {
    let mut bytes = Vec::with_capacity(24 + addresses.len() * 20);
    bytes.extend_from_slice(&freeze_height.to_be_bytes());
    bytes.extend_from_slice(&planned_activation_height.to_be_bytes());
    bytes.extend_from_slice(&(addresses.len() as u64).to_be_bytes());
    for address in addresses {
        bytes.extend_from_slice(address.as_slice());
    }
    alloy_primitives::keccak256(bytes)
}

fn encode_outcome(
    epoch: Epoch,
    output: &Output<MinSig, bls12381::PublicKey>,
    is_full_dkg: bool,
) -> Bytes {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"ODKO");
    buf.push(0x02);
    buf.extend_from_slice(&epoch.get().to_be_bytes());
    buf.push(u8::from(is_full_dkg));

    let output_bytes = commonware_codec::Encode::encode(output);
    buf.extend_from_slice(&(output_bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(output_bytes.as_ref());

    Bytes::from(buf)
}

pub fn dkg_output_hash(output: &Output<MinSig, bls12381::PublicKey>) -> B256 {
    alloy_primitives::keccak256(commonware_codec::Encode::encode(output))
}

/// Enforce the consensus-critical invariant that a locally-computed DKG `local`
/// output matches the chain-`canonical` output (the one the chain reconstructs
/// from finalized dealer logs).
///
/// The DKG actor's output is advisory; the authority is the canonical output.
/// Activating a VRF key from a local output that disagrees with canonical would
/// diverge this node's randomness from the network, so a mismatch is fatal. This
/// is the single definition of the check shared by every activation and recovery
/// path: callers resolve `canonical` from their own authoritative source (the
/// live manager via [`Mailbox::canonical_output`] vs a decoded boundary
/// artifact) and pass both outputs in; `context` names the call site for the
/// error.
pub fn assert_canonical_output(
    local: &Output<MinSig, bls12381::PublicKey>,
    canonical: &Output<MinSig, bls12381::PublicKey>,
    context: &str,
) -> eyre::Result<()> {
    if local != canonical {
        return Err(eyre::eyre!(
            "local DKG output does not match canonical finalized-log output ({context}): \
             local {}, canonical {}",
            dkg_output_hash(local),
            dkg_output_hash(canonical),
        ));
    }
    Ok(())
}

pub fn public_polynomial_hash(polynomial: &Sharing<MinSig>) -> B256 {
    alloy_primitives::keccak256(commonware_codec::Encode::encode(polynomial))
}

/// Decode the ODKO-wrapped boundary `outcome` and return
/// `keccak256(Encode(full public polynomial))` of the carried DKG output.
///
/// Returns `B256::ZERO` when the outcome is not a decodable full-output ODKO
/// record (e.g. a group-key-only bootstrap outcome) — in that case the
/// committee's "invalid seed partial" slash offense is simply unavailable for
/// that epoch. Deterministic and panic-free; safe to call in the executor over
/// the already-consensus-validated boundary `outcome`.
pub fn boundary_outcome_polynomial_hash(outcome: &[u8]) -> B256 {
    use commonware_cryptography::bls12381::primitives::sharing::ModeVersion;
    // ODKO || version(1) || epoch(8) || is_full_dkg(1) || len(4 BE) || Output
    const HEADER_LEN: usize = 4 + 1 + 8 + 1 + 4;
    if outcome.len() < HEADER_LEN || &outcome[0..4] != b"ODKO" {
        return B256::ZERO;
    }
    let Ok(len_bytes) = <[u8; 4]>::try_from(&outcome[14..18]) else {
        return B256::ZERO;
    };
    let len = u32::from_be_bytes(len_bytes) as usize;
    let Some(body) = outcome.get(HEADER_LEN..HEADER_LEN + len) else {
        return B256::ZERO;
    };
    let Some(max) = NonZeroU32::new(crate::bls::MAX_VALIDATORS) else {
        return B256::ZERO;
    };
    let cfg = (max, ModeVersion::v0());
    match Output::<MinSig, bls12381::PublicKey>::read_cfg(&mut &body[..], &cfg) {
        Ok(output) => public_polynomial_hash(output.public()),
        Err(_) => B256::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, B256, U256};
    use commonware_codec::Encode as _;
    use commonware_cryptography::{
        bls12381::{
            dkg::feldman_desmedt::{
                observe, Dealer, DealerLog, Info, Logs, Player, SignedDealerLog,
            },
            primitives::{group::Share, sharing::Mode},
        },
        Signer as _,
    };
    use commonware_math::algebra::Random;
    use commonware_parallel::Sequential;
    use commonware_utils::TryCollect as _;

    use super::*;

    #[allow(clippy::type_complexity)]
    fn run_test_dkg_complete() -> (
        Vec<bls12381::PrivateKey>,
        Set<bls12381::PublicKey>,
        Output<MinSig, bls12381::PublicKey>,
        Sharing<MinSig>,
        Bytes,
    ) {
        let mut keys: Vec<bls12381::PrivateKey> = (0..3)
            .map(|_| bls12381::PrivateKey::random(rand_core::OsRng))
            .collect();
        keys.sort_by_key(|a| a.public_key().encode());

        let participants: Set<bls12381::PublicKey> =
            keys.iter().map(|k| k.public_key()).try_collect().unwrap();

        let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
            &config::outbe_app_namespace(),
            7,
            None,
            Mode::NonZeroCounter,
            participants.clone(),
            participants.clone(),
        )
        .unwrap();

        let mut dealers = Vec::new();
        let mut pub_msgs = Vec::new();
        let mut all_priv_msgs = Vec::new();

        for key in &keys {
            let (dealer, pub_msg, priv_msgs) =
                Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
                    rand_core::OsRng,
                    info.clone(),
                    key.clone(),
                    None,
                )
                .unwrap();
            dealers.push(dealer);
            pub_msgs.push(pub_msg);
            all_priv_msgs.push(priv_msgs);
        }

        let mut players: Vec<Player<MinSig, bls12381::PrivateKey>> = keys
            .iter()
            .map(|k| Player::new(info.clone(), k.clone()).unwrap())
            .collect();

        for (dealer_idx, (pub_msg, priv_msgs)) in
            pub_msgs.iter().zip(all_priv_msgs.iter()).enumerate()
        {
            let dealer_pk = keys[dealer_idx].public_key();
            for (player_pk, priv_msg) in priv_msgs {
                let player_idx = keys
                    .iter()
                    .position(|k| &k.public_key() == player_pk)
                    .unwrap();
                if let Some(ack) = players[player_idx].dealer_message::<N3f1>(
                    dealer_pk.clone(),
                    pub_msg.clone(),
                    priv_msg.clone(),
                ) {
                    dealers[dealer_idx]
                        .receive_player_ack(player_pk.clone(), ack)
                        .unwrap();
                }
            }
        }

        let mut logs = std::collections::BTreeMap::new();
        let mut first_log = None;
        for dealer in dealers {
            let signed_log = dealer.finalize::<N3f1>();
            if first_log.is_none() {
                first_log = Some(Bytes::from(signed_log.encode()));
            }
            if let Some((pk, log)) = signed_log.check(&info) {
                logs.insert(pk, log);
            }
        }

        let mut dkg_logs = Logs::<MinSig, bls12381::PublicKey, N3f1>::new(info.clone());
        for (dealer, log) in logs {
            dkg_logs.record(dealer, log);
        }
        let (output, _share) = players
            .remove(0)
            .finalize::<N3f1, commonware_cryptography::bls12381::Batch>(
                &mut rand_core::OsRng,
                dkg_logs,
                &Sequential,
            )
            .unwrap();
        let polynomial = output.public().clone();

        (keys, participants, output, polynomial, first_log.unwrap())
    }

    #[allow(clippy::type_complexity)]
    fn run_round(
        keys: &[bls12381::PrivateKey],
        participants: Set<bls12381::PublicKey>,
        previous_output: Option<Output<MinSig, bls12381::PublicKey>>,
        previous_shares: Option<&[Share]>,
        round: u64,
    ) -> (
        Info<MinSig, bls12381::PublicKey>,
        Output<MinSig, bls12381::PublicKey>,
        Vec<Share>,
        BTreeMap<bls12381::PublicKey, DealerLog<MinSig, bls12381::PublicKey>>,
        BTreeMap<bls12381::PublicKey, Bytes>,
    ) {
        let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
            &config::outbe_app_namespace(),
            round,
            previous_output,
            Mode::NonZeroCounter,
            participants.clone(),
            participants,
        )
        .unwrap();

        let mut dealers = Vec::new();
        let mut pub_msgs = Vec::new();
        let mut all_priv_msgs = Vec::new();

        for (idx, key) in keys.iter().enumerate() {
            let previous_share = previous_shares.map(|shares| shares[idx].clone());
            let (dealer, pub_msg, priv_msgs) =
                Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
                    rand_core::OsRng,
                    info.clone(),
                    key.clone(),
                    previous_share,
                )
                .unwrap();
            dealers.push(dealer);
            pub_msgs.push(pub_msg);
            all_priv_msgs.push(priv_msgs);
        }

        let mut players: Vec<Player<MinSig, bls12381::PrivateKey>> = keys
            .iter()
            .map(|k| Player::new(info.clone(), k.clone()).unwrap())
            .collect();

        for (dealer_idx, (pub_msg, priv_msgs)) in
            pub_msgs.iter().zip(all_priv_msgs.iter()).enumerate()
        {
            let dealer_pk = keys[dealer_idx].public_key();
            for (player_pk, priv_msg) in priv_msgs {
                let player_idx = keys
                    .iter()
                    .position(|k| &k.public_key() == player_pk)
                    .unwrap();
                if let Some(ack) = players[player_idx].dealer_message::<N3f1>(
                    dealer_pk.clone(),
                    pub_msg.clone(),
                    priv_msg.clone(),
                ) {
                    dealers[dealer_idx]
                        .receive_player_ack(player_pk.clone(), ack)
                        .unwrap();
                }
            }
        }

        let mut logs = BTreeMap::new();
        let mut signed_logs = BTreeMap::new();
        for dealer in dealers {
            let signed_log: SignedDealerLog<MinSig, bls12381::PrivateKey> =
                dealer.finalize::<N3f1>();
            let encoded = Bytes::from(signed_log.encode());
            if let Some((pk, log)) = signed_log.check(&info) {
                signed_logs.insert(pk.clone(), encoded);
                logs.insert(pk, log);
            }
        }

        let mut shares = Vec::new();
        let mut output = None;
        for player in players {
            let mut dkg_logs = Logs::<MinSig, bls12381::PublicKey, N3f1>::new(info.clone());
            for (dealer, log) in logs.clone() {
                dkg_logs.record(dealer, log);
            }
            let (player_output, share) = player
                .finalize::<N3f1, commonware_cryptography::bls12381::Batch>(
                    &mut rand_core::OsRng,
                    dkg_logs,
                    &Sequential,
                )
                .unwrap();
            output = Some(player_output);
            shares.push(share);
        }

        (info, output.unwrap(), shares, logs, signed_logs)
    }

    fn legacy_group_key_only_outcome(
        epoch: Epoch,
        output: &Output<MinSig, bls12381::PublicKey>,
        is_full_dkg: bool,
    ) -> Bytes {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"ODKO");
        buf.push(0x01);
        buf.extend_from_slice(&epoch.get().to_be_bytes());
        buf.push(u8::from(is_full_dkg));
        let group_bytes = output.public().public().encode();
        buf.extend_from_slice(&(group_bytes.len() as u32).to_be_bytes());
        buf.extend_from_slice(group_bytes.as_ref());
        Bytes::from(buf)
    }

    #[test]
    fn boundary_artifact_is_deterministic() {
        let (keys, _participants, output, _polynomial, _log) = run_test_dkg_complete();
        let validator_set = ValidatorSet {
            public_keys: keys.iter().map(|k| k.public_key()).collect(),
            addresses: vec![
                address!("0x1111111111111111111111111111111111111111"),
                address!("0x2222222222222222222222222222222222222222"),
                address!("0x3333333333333333333333333333333333333333"),
            ],
            p2p_addresses: vec![crate::validators::ValidatorP2pAddress::Missing; 3],
        };

        let a = build_boundary_artifact(BoundaryArtifactInput {
            epoch: Epoch::new(1),
            validator_set: &validator_set,
            output: &output,
            is_full_dkg: true,
            dkg_cycle: 1,
            freeze_height: 10,
            planned_activation_height: 20,
            vrf_material_version: 1,
            is_validator_set_change: true,
            tee_reshare_registrations: Vec::new(),
        })
        .unwrap();
        let b = build_boundary_artifact(BoundaryArtifactInput {
            epoch: Epoch::new(1),
            validator_set: &validator_set,
            output: &output,
            is_full_dkg: true,
            dkg_cycle: 1,
            freeze_height: 10,
            planned_activation_height: 20,
            vrf_material_version: 1,
            is_validator_set_change: true,
            tee_reshare_registrations: Vec::new(),
        })
        .unwrap();
        assert_eq!(a, b);
        assert_ne!(a.vrf_group_public_key, B256::ZERO);
        assert_ne!(a.reshare.active_set_hash, B256::ZERO);
    }

    #[test]
    fn assert_canonical_output_accepts_equal_and_rejects_divergent() {
        let (_keys, _participants, output, _polynomial, _log) = run_test_dkg_complete();

        // Equal local/canonical outputs pass — the activation/recovery happy path.
        assert_canonical_output(&output, &output, "equal").expect("equal outputs must match");

        // A genuinely different output is rejected, and the error carries both
        // output hashes plus the call-site context so a divergence is diagnosable.
        let (_k2, _p2, other, _poly2, _log2) = run_test_dkg_complete();
        assert_ne!(output, other, "two independent DKG runs differ");
        let err = assert_canonical_output(&output, &other, "divergent-site")
            .expect_err("divergent outputs must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("divergent-site"),
            "error names the context: {msg}"
        );
        assert!(
            msg.contains(&dkg_output_hash(&output).to_string()),
            "error carries the local output hash: {msg}"
        );
    }

    /// R5.3: the producer threads `tee_reshare_registrations` from the input into
    /// the boundary artifact (and re-encodes them deterministically), so a reshare
    /// boundary carries the new committee's per-validator TEE keys.
    #[test]
    fn boundary_artifact_carries_tee_reshare_registrations() {
        let (keys, _participants, output, _polynomial, _log) = run_test_dkg_complete();
        let validator_set = ValidatorSet {
            public_keys: keys.iter().map(|k| k.public_key()).collect(),
            addresses: vec![
                address!("0x1111111111111111111111111111111111111111"),
                address!("0x2222222222222222222222222222222222222222"),
                address!("0x3333333333333333333333333333333333333333"),
            ],
            p2p_addresses: vec![crate::validators::ValidatorP2pAddress::Missing; 3],
        };
        let regs = vec![TeeReshareRegistration {
            validator: address!("0x1111111111111111111111111111111111111111"),
            recipient_x25519: B256::repeat_byte(0xB1),
            attestation_pub: B256::repeat_byte(0xB2),
            noise_static_pub: B256::repeat_byte(0xB3),
        }];
        let artifact = build_boundary_artifact(BoundaryArtifactInput {
            epoch: Epoch::new(1),
            validator_set: &validator_set,
            output: &output,
            is_full_dkg: false,
            dkg_cycle: 1,
            freeze_height: 10,
            planned_activation_height: 20,
            vrf_material_version: 1,
            is_validator_set_change: true,
            tee_reshare_registrations: regs.clone(),
        })
        .unwrap();
        assert_eq!(artifact.tee_reshare_registrations, regs);
        // The artifact (and thus the hash-committed block bytes) round-trips with
        // the registrations intact.
        let encoded = encode_boundary_artifact(&artifact).unwrap();
        let decoded = outbe_primitives::reshare_artifact::decode_boundary_artifact(&encoded)
            .unwrap()
            .unwrap();
        assert_eq!(decoded.tee_reshare_registrations, regs);
    }

    #[tokio::test]
    async fn dealer_log_roundtrips_through_manager() {
        let (_keys, participants, _output, _polynomial, local_log) = run_test_dkg_complete();
        let manager = Mailbox::new();
        manager
            .note_ceremony_started(Epoch::new(0), 7, None, participants)
            .unwrap();
        manager
            .note_local_dealer_log(Epoch::new(0), local_log.clone())
            .unwrap();

        let served = manager.get_dealer_log(Epoch::new(0)).await.unwrap();
        assert_eq!(served, local_log);
        let _dealer = manager
            .verify_dealer_log(Epoch::new(0), served.to_vec())
            .await
            .unwrap();

        manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(served)));
        assert!(manager.get_dealer_log(Epoch::new(0)).await.is_none());
    }

    #[tokio::test]
    async fn pending_p2p_dealer_log_can_be_served_and_drained() {
        let keys: Vec<bls12381::PrivateKey> = (0..4)
            .map(|_| bls12381::PrivateKey::random(rand_core::OsRng))
            .collect();
        let participants: Set<bls12381::PublicKey> =
            keys.iter().map(|k| k.public_key()).try_collect().unwrap();
        let (_info, _output, _shares, _logs, signed_logs) =
            run_round(&keys, participants.clone(), None, None, 7);
        let first = signed_logs.values().next().unwrap().clone();

        let manager = Mailbox::new();
        manager
            .note_ceremony_started(Epoch::new(0), 7, None, participants)
            .unwrap();
        manager
            .note_pending_dealer_log(Epoch::new(0), first.clone())
            .unwrap();

        assert_eq!(
            manager.get_dealer_log(Epoch::new(0)).await,
            Some(first.clone())
        );
        manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(first)));
        assert!(manager.get_dealer_log(Epoch::new(0)).await.is_none());
    }

    #[tokio::test]
    async fn reshare_ceremony_uses_previous_players_as_dealers() {
        let old_keys: Vec<bls12381::PrivateKey> =
            (1..=4).map(bls12381::PrivateKey::from_seed).collect();
        let old_participants: Set<bls12381::PublicKey> = old_keys
            .iter()
            .map(|key| key.public_key())
            .try_collect()
            .unwrap();
        let (_info, previous_output, _shares, _logs, _signed_logs) =
            run_round(&old_keys, old_participants.clone(), None, None, 0);

        let new_key = bls12381::PrivateKey::from_seed(100);
        let new_pk = new_key.public_key();
        let mut target_keys = old_keys.clone();
        target_keys.push(new_key);
        let target_participants: Set<bls12381::PublicKey> = target_keys
            .iter()
            .map(|key| key.public_key())
            .try_collect()
            .unwrap();

        let manager = Mailbox::new();
        manager
            .note_ceremony_started(Epoch::new(0), 1, Some(previous_output), target_participants)
            .unwrap();

        let dealers = manager.with_state(|state| {
            state
                .ceremony
                .as_ref()
                .expect("ceremony initialized")
                .dealers
                .clone()
        });
        assert_eq!(dealers, old_participants);
        assert!(dealers.position(&new_pk).is_none());
    }

    #[tokio::test]
    async fn reshare_ceremony_keeps_removed_old_player_as_dealer() {
        let old_keys: Vec<bls12381::PrivateKey> =
            (1..=4).map(bls12381::PrivateKey::from_seed).collect();
        let old_participants: Set<bls12381::PublicKey> = old_keys
            .iter()
            .map(|key| key.public_key())
            .try_collect()
            .unwrap();
        let (_info, previous_output, _shares, _logs, _signed_logs) =
            run_round(&old_keys, old_participants.clone(), None, None, 0);

        let removed_pk = old_keys[0].public_key();
        let target_participants: Set<bls12381::PublicKey> = old_keys
            .iter()
            .filter(|key| key.public_key() != removed_pk)
            .map(|key| key.public_key())
            .try_collect()
            .unwrap();

        let manager = Mailbox::new();
        manager
            .note_ceremony_started(Epoch::new(0), 1, Some(previous_output), target_participants)
            .unwrap();

        let dealers = manager.with_state(|state| {
            state
                .ceremony
                .as_ref()
                .expect("ceremony initialized")
                .dealers
                .clone()
        });
        assert_eq!(dealers, old_participants);
        assert!(dealers.position(&removed_pk).is_some());
    }

    #[tokio::test]
    async fn pending_p2p_dealer_log_rejects_wrong_ceremony() {
        let keys: Vec<bls12381::PrivateKey> = (0..4)
            .map(|_| bls12381::PrivateKey::random(rand_core::OsRng))
            .collect();
        let participants: Set<bls12381::PublicKey> =
            keys.iter().map(|k| k.public_key()).try_collect().unwrap();
        let (_info, _output, _shares, _logs, signed_logs) =
            run_round(&keys, participants.clone(), None, None, 7);
        let first = signed_logs.values().next().unwrap().clone();

        let manager = Mailbox::new();
        manager
            .note_ceremony_started(Epoch::new(0), 8, None, participants)
            .unwrap();

        assert!(manager
            .note_pending_dealer_log(Epoch::new(0), first)
            .is_err());
        assert!(manager.get_dealer_log(Epoch::new(0)).await.is_none());
    }

    #[tokio::test]
    async fn pending_p2p_dealer_log_rejects_non_committee_dealer() {
        let keys: Vec<bls12381::PrivateKey> = (0..4)
            .map(|_| bls12381::PrivateKey::random(rand_core::OsRng))
            .collect();
        let participants: Set<bls12381::PublicKey> =
            keys.iter().map(|k| k.public_key()).try_collect().unwrap();
        let (_info, _output, _shares, _logs, signed_logs) =
            run_round(&keys, participants.clone(), None, None, 7);
        let (dealer, bytes) = signed_logs.iter().next().unwrap();

        let manager = Mailbox::new();
        manager
            .note_ceremony_started(Epoch::new(0), 7, None, participants)
            .unwrap();
        manager.with_state(|state| {
            let ceremony = state.ceremony.as_mut().unwrap();
            ceremony.dealers = ceremony
                .dealers
                .iter()
                .filter(|candidate| *candidate != dealer)
                .cloned()
                .try_collect()
                .unwrap();
        });

        assert!(manager
            .note_pending_dealer_log(Epoch::new(0), bytes.clone())
            .is_err());
        assert!(manager.get_dealer_log(Epoch::new(0)).await.is_none());
    }

    #[tokio::test]
    async fn pending_p2p_dealer_log_rejects_conflicting_duplicate() {
        let keys: Vec<bls12381::PrivateKey> = (0..4)
            .map(|_| bls12381::PrivateKey::random(rand_core::OsRng))
            .collect();
        let participants: Set<bls12381::PublicKey> =
            keys.iter().map(|k| k.public_key()).try_collect().unwrap();
        let (_info, _output, _shares, _logs, signed_logs_a) =
            run_round(&keys, participants.clone(), None, None, 7);
        let (_info, _output, _shares, _logs, signed_logs_b) =
            run_round(&keys, participants.clone(), None, None, 7);
        let dealer = signed_logs_a.keys().next().unwrap();
        let first = signed_logs_a.get(dealer).unwrap().clone();
        let conflicting = signed_logs_b.get(dealer).unwrap().clone();
        assert_ne!(first, conflicting);

        let manager = Mailbox::new();
        manager
            .note_ceremony_started(Epoch::new(0), 7, None, participants)
            .unwrap();
        manager
            .note_pending_dealer_log(Epoch::new(0), first.clone())
            .unwrap();
        manager
            .note_pending_dealer_log(Epoch::new(0), conflicting)
            .unwrap();

        assert_eq!(manager.get_dealer_log(Epoch::new(0)).await, Some(first));
    }

    #[test]
    fn chain_finalized_replay_rejects_non_committee_dealer() {
        let keys: Vec<bls12381::PrivateKey> = (0..4)
            .map(|_| bls12381::PrivateKey::random(rand_core::OsRng))
            .collect();
        let participants: Set<bls12381::PublicKey> =
            keys.iter().map(|k| k.public_key()).try_collect().unwrap();
        let (_info, _output, _shares, _logs, signed_logs) =
            run_round(&keys, participants.clone(), None, None, 7);
        let (dealer, bytes) = signed_logs.iter().next().unwrap();

        let manager = Mailbox::new();
        manager
            .note_ceremony_started(Epoch::new(0), 7, None, participants)
            .unwrap();
        manager.with_state(|state| {
            let ceremony = state.ceremony.as_mut().unwrap();
            ceremony.dealers = ceremony
                .dealers
                .iter()
                .filter(|candidate| *candidate != dealer)
                .cloned()
                .try_collect()
                .unwrap();
        });

        manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(
            bytes.clone(),
        )));
        let recorded = manager
            .with_state(|state| state.ceremony.as_ref().unwrap().finalized_dealer_logs.len());
        assert_eq!(recorded, 0);
    }

    #[test]
    fn dealer_log_size_within_extra_data_for_n128() {
        let mut keys: Vec<bls12381::PrivateKey> = (0..128)
            .map(|i| bls12381::PrivateKey::from_seed(i + 1))
            .collect();
        keys.sort_by_key(|key| key.public_key().encode());
        let participants: Set<bls12381::PublicKey> = keys
            .iter()
            .map(|key| key.public_key())
            .try_collect()
            .unwrap();
        let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
            &config::outbe_app_namespace(),
            7,
            None,
            Mode::NonZeroCounter,
            participants.clone(),
            participants,
        )
        .unwrap();

        let dealer_key = keys[0].clone();
        let dealer_pk = dealer_key.public_key();
        let (mut dealer, pub_msg, priv_msgs) =
            Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
                rand_core::OsRng,
                info.clone(),
                dealer_key,
                None,
            )
            .unwrap();
        for (player_pk, priv_msg) in priv_msgs {
            let mut player = Player::new(
                info.clone(),
                keys.iter()
                    .find(|key| key.public_key() == player_pk)
                    .unwrap()
                    .clone(),
            )
            .unwrap();
            let ack = player
                .dealer_message::<N3f1>(dealer_pk.clone(), pub_msg.clone(), priv_msg)
                .unwrap();
            dealer.receive_player_ack(player_pk, ack).unwrap();
        }
        let dealer_log = Bytes::from(dealer.finalize::<N3f1>().encode());

        let encoded = outbe_primitives::reshare_artifact::encode_outbe_block_artifacts(
            &outbe_primitives::reshare_artifact::OutbeBlockArtifacts {
                execution_summary: Some(
                    outbe_primitives::reshare_artifact::ExecutionSummaryArtifact {
                        validator_fee_sum: U256::MAX,
                    },
                ),
                consensus_header_artifact: Some(ConsensusHeaderArtifact::DealerLog(dealer_log)),
                timestamp_millis_part: 0,
                late_finalize_credits: None,
            },
        )
        .unwrap();

        assert!(
            encoded.len() <= outbe_primitives::consensus::OUTBE_MAX_EXTRA_DATA_SIZE,
            "encoded artifact size {} must fit OUTBE_MAX_EXTRA_DATA_SIZE {}",
            encoded.len(),
            outbe_primitives::consensus::OUTBE_MAX_EXTRA_DATA_SIZE
        );
    }

    #[tokio::test]
    async fn verify_boundary_succeeds_after_finalize() {
        let (keys, _participants, output, _polynomial, _local_log) = run_test_dkg_complete();
        let validator_set = ValidatorSet {
            public_keys: keys.iter().map(|k| k.public_key()).collect(),
            addresses: vec![
                address!("0x1111111111111111111111111111111111111111"),
                address!("0x2222222222222222222222222222222222222222"),
                address!("0x3333333333333333333333333333333333333333"),
            ],
            p2p_addresses: vec![crate::validators::ValidatorP2pAddress::Missing; 3],
        };
        let artifact = build_boundary_artifact(BoundaryArtifactInput {
            epoch: Epoch::new(0),
            validator_set: &validator_set,
            output: &output,
            is_full_dkg: false,
            dkg_cycle: 0,
            freeze_height: 0,
            planned_activation_height: 0,
            vrf_material_version: 0,
            is_validator_set_change: true,
            tee_reshare_registrations: Vec::new(),
        })
        .unwrap();

        let manager = Mailbox::new();
        manager.note_bootstrap_outcome(artifact.clone());
        let parent_hash = B256::repeat_byte(0x42);
        let artifact_hash = Mailbox::boundary_artifact_hash(&artifact).unwrap();
        manager.record_boundary_status(parent_hash, artifact_hash, BoundaryStatus::NoBoundarySeen);
        assert!(manager
            .cached_boundary_status(parent_hash, artifact_hash)
            .is_some());
        manager.note_recovered_pending_boundary(artifact.clone());
        assert!(
            manager
                .cached_boundary_status(parent_hash, artifact_hash)
                .is_none(),
            "new pending DKG boundary must clear prior boundary-status cache"
        );

        // The pending artifact is available before finalize.
        assert!(manager
            .pending_boundary_artifact(Epoch::new(0))
            .await
            .is_some());

        // Pending-artifact verification works before finalize.
        manager
            .verify_pending_boundary_artifact(Epoch::new(0), &artifact)
            .await
            .unwrap();

        // Simulate finalize of a block carrying the same artifact.
        manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::BoundaryOutcome(
            artifact.clone(),
        )));

        // Finalize records a committed marker but does not itself decide
        // proposer/verifier validity. The application derives that from parent
        // ancestry and then the scheduler drains this marker.
        assert!(manager
            .pending_boundary_artifact(Epoch::new(0))
            .await
            .is_some());

        // Scheduler activation is driven by the chain-committed marker, not by
        // process-local served state. Draining the committed marker clears the
        // pending boundary after activation.
        assert_eq!(
            manager.take_committed_boundary_artifact().await,
            Some(artifact)
        );
        assert!(manager
            .pending_boundary_artifact(Epoch::new(0))
            .await
            .is_none());
    }

    #[test]
    fn full_output_outcome_detects_reshare_log_subset_divergence() {
        let mut keys: Vec<bls12381::PrivateKey> = (0..4)
            .map(|_| bls12381::PrivateKey::random(rand_core::OsRng))
            .collect();
        keys.sort_by_key(|a| a.public_key().encode());
        let participants: Set<bls12381::PublicKey> =
            keys.iter().map(|k| k.public_key()).try_collect().unwrap();

        let (_initial_info, initial_output, initial_shares, _initial_logs, _initial_signed) =
            run_round(&keys, participants.clone(), None, None, 0);
        let (reshare_info, _reshare_output, _reshare_shares, reshare_logs, _reshare_signed) =
            run_round(
                &keys,
                participants.clone(),
                Some(initial_output),
                Some(&initial_shares),
                1,
            );

        let mut all_logs = Logs::<MinSig, bls12381::PublicKey, N3f1>::new(reshare_info.clone());
        for (dealer, log) in reshare_logs.clone() {
            all_logs.record(dealer, log);
        }
        let all_output = observe::<
            MinSig,
            bls12381::PublicKey,
            N3f1,
            commonware_cryptography::bls12381::Batch,
        >(&mut rand_core::OsRng, all_logs, &Sequential)
        .unwrap();
        let mut subset_logs = reshare_logs.clone();
        let removed = subset_logs.keys().next().cloned().unwrap();
        subset_logs.remove(&removed);
        let mut subset_dkg_logs = Logs::<MinSig, bls12381::PublicKey, N3f1>::new(reshare_info);
        for (dealer, log) in subset_logs {
            subset_dkg_logs.record(dealer, log);
        }
        let subset_output = observe::<
            MinSig,
            bls12381::PublicKey,
            N3f1,
            commonware_cryptography::bls12381::Batch,
        >(&mut rand_core::OsRng, subset_dkg_logs, &Sequential)
        .unwrap();

        assert_eq!(
            all_output.public().public(),
            subset_output.public().public(),
            "reshare preserves the threshold group key even when full output diverges"
        );
        assert_ne!(all_output, subset_output);
        assert_eq!(
            legacy_group_key_only_outcome(Epoch::new(1), &all_output, false),
            legacy_group_key_only_outcome(Epoch::new(1), &subset_output, false),
            "old boundary outcome could not detect the divergence"
        );

        let validator_set = ValidatorSet {
            public_keys: keys.iter().map(|k| k.public_key()).collect(),
            addresses: vec![
                address!("0x1111111111111111111111111111111111111111"),
                address!("0x2222222222222222222222222222222222222222"),
                address!("0x3333333333333333333333333333333333333333"),
                address!("0x4444444444444444444444444444444444444444"),
            ],
            p2p_addresses: vec![crate::validators::ValidatorP2pAddress::Missing; 4],
        };
        let all_artifact = build_boundary_artifact(BoundaryArtifactInput {
            epoch: Epoch::new(1),
            validator_set: &validator_set,
            output: &all_output,
            is_full_dkg: false,
            dkg_cycle: 1,
            freeze_height: 90,
            planned_activation_height: 120,
            vrf_material_version: 1,
            is_validator_set_change: true,
            tee_reshare_registrations: Vec::new(),
        })
        .unwrap();
        let subset_artifact = build_boundary_artifact(BoundaryArtifactInput {
            epoch: Epoch::new(1),
            validator_set: &validator_set,
            output: &subset_output,
            is_full_dkg: false,
            dkg_cycle: 1,
            freeze_height: 90,
            planned_activation_height: 120,
            vrf_material_version: 1,
            is_validator_set_change: true,
            tee_reshare_registrations: Vec::new(),
        })
        .unwrap();
        assert_eq!(
            all_artifact.vrf_group_public_key,
            subset_artifact.vrf_group_public_key
        );
        assert_ne!(all_artifact.outcome, subset_artifact.outcome);
        assert_ne!(all_artifact, subset_artifact);

        // Offense-A parity: the executor derives the committee's polynomial hash
        // from the boundary `outcome` and must get exactly the value the
        // proposer committed (`public_polynomial_hash(output.public())`).
        // Otherwise the snapshot the executor writes would diverge and
        // invalid-seed-partial evidence could never match.
        assert_eq!(
            boundary_outcome_polynomial_hash(all_artifact.outcome.as_ref()),
            public_polynomial_hash(all_output.public()),
            "executor outcome-derived poly hash must equal the proposer's"
        );
        // Distinct polynomials → distinct hashes (no collision).
        assert_ne!(
            boundary_outcome_polynomial_hash(all_artifact.outcome.as_ref()),
            boundary_outcome_polynomial_hash(subset_artifact.outcome.as_ref()),
        );
    }

    #[test]
    fn finalized_dealer_logs_reconstruct_canonical_output() {
        let mut keys: Vec<bls12381::PrivateKey> = (0..4)
            .map(|_| bls12381::PrivateKey::random(rand_core::OsRng))
            .collect();
        keys.sort_by_key(|a| a.public_key().encode());
        let participants: Set<bls12381::PublicKey> =
            keys.iter().map(|k| k.public_key()).try_collect().unwrap();
        let (info, expected_output, _shares, logs, signed_logs) =
            run_round(&keys, participants.clone(), None, None, 11);
        let mut observed_logs = Logs::<MinSig, bls12381::PublicKey, N3f1>::new(info.clone());
        for (dealer, log) in logs {
            observed_logs.record(dealer, log);
        }
        let observed = observe::<
            MinSig,
            bls12381::PublicKey,
            N3f1,
            commonware_cryptography::bls12381::Batch,
        >(&mut rand_core::OsRng, observed_logs, &Sequential)
        .unwrap();
        assert_eq!(expected_output, observed);

        let finalized_order: Vec<Bytes> = signed_logs.values().rev().cloned().collect();
        let mut canonical_logs = BTreeMap::new();
        for bytes in finalized_order.iter().take(3) {
            let mut reader = bytes.as_ref();
            let signed_log = SignedDealerLog::<MinSig, bls12381::PrivateKey>::read_cfg(
                &mut reader,
                &NonZeroU32::new(keys.len() as u32).unwrap(),
            )
            .unwrap();
            let (dealer, log) = signed_log.check(&info).unwrap();
            canonical_logs.insert(dealer, log);
        }
        let mut canonical_dkg_logs = Logs::<MinSig, bls12381::PublicKey, N3f1>::new(info);
        for (dealer, log) in canonical_logs {
            canonical_dkg_logs.record(dealer, log);
        }
        let expected_canonical = observe::<
            MinSig,
            bls12381::PublicKey,
            N3f1,
            commonware_cryptography::bls12381::Batch,
        >(&mut rand_core::OsRng, canonical_dkg_logs, &Sequential)
        .unwrap();

        let manager = Mailbox::new();
        manager
            .note_ceremony_started(Epoch::new(3), 11, None, participants)
            .unwrap();
        for bytes in &finalized_order {
            manager.note_finalized_header_artifact(Some(&ConsensusHeaderArtifact::DealerLog(
                bytes.clone(),
            )));
        }

        assert_eq!(
            manager.canonical_output(Epoch::new(3)),
            Some(expected_canonical)
        );
    }
}
