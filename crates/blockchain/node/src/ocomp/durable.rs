//! Durable OCOMP frame watermark, active-job registry, and immutable capsules.
//!
//! A frame is acknowledged only after every capsule referenced by that frame is
//! fsynced and the small registry/checkpoint snapshot is atomically published.
//! Capsule files are immutable and content-addressed by observation identity;
//! an orphan produced by a crash before state publication is harmless.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _},
    path::{Path, PathBuf},
};

use alloy_primitives::{keccak256, B256};
use outbe_primitives::projection::ProjectionCheckpoint;
use thiserror::Error;

const STATE_MAGIC: [u8; 8] = *b"OCOMPDS1";
const CAPSULE_MAGIC: [u8; 8] = *b"OCOMPCP1";
const STORE_VERSION: u16 = 1;
const STATE_FILE: &str = "state-v1.bin";
const STATE_PENDING_FILE: &str = "state-v1.pending";
const CAPSULE_DIRECTORY: &str = "capsules-v1";
const CAPSULE_SUFFIX: &str = ".capsule";
const MAX_STATE_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CAPSULE_FILE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_DURABLE_JOBS: usize = u16::MAX as usize;
const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;

/// One immutable, already-verified input payload sufficient for chain-blind
/// snapshot materialization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PruningSafeMaterializationCapsuleV1 {
    observation_id: B256,
    payload_digest: B256,
    canonical_bytes: Vec<u8>,
}

impl PruningSafeMaterializationCapsuleV1 {
    /// Wraps canonical bytes after the caller has verified all typed request,
    /// finality, Tribute, opening, and protocol-bundle bindings.
    pub fn from_verified_canonical_bytes(
        observation_id: B256,
        canonical_bytes: Vec<u8>,
    ) -> Result<Self, DurableOcompStoreErrorV1> {
        if observation_id.is_zero() || canonical_bytes.is_empty() {
            return Err(DurableOcompStoreErrorV1::InvalidCapsule);
        }
        if u64::try_from(canonical_bytes.len())
            .map_or(true, |len| len > MAX_CAPSULE_FILE_BYTES.saturating_sub(82))
        {
            return Err(DurableOcompStoreErrorV1::Capacity);
        }
        Ok(Self {
            observation_id,
            payload_digest: keccak256(&canonical_bytes),
            canonical_bytes,
        })
    }

    #[must_use]
    pub const fn observation_id(&self) -> B256 {
        self.observation_id
    }

    #[must_use]
    pub const fn payload_digest(&self) -> B256 {
        self.payload_digest
    }

    #[must_use]
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum DurableOcompJobLifecycleV1 {
    Observed = 1,
    Finalized = 2,
    VotingOpen = 3,
    Completed = 4,
    Terminal = 5,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportReceiptBindingV1 {
    pub transport_digest: B256,
    pub encoded_bytes: u64,
    pub expected_ocb1_kind: Option<u16>,
    pub receipt_digest: B256,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DurableOutboxStateV1 {
    Pending {
        delivery_attempts: u32,
    },
    Leased {
        delivery_attempts: u32,
        lease_owner: B256,
        delivery_sequence: u64,
        lease_deadline_millis: u64,
    },
    Acknowledged {
        delivery_attempts: u32,
        acknowledged_at_millis: u64,
        receipt: ExportReceiptBindingV1,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableOcompJobV1 {
    observation_id: B256,
    payload_digest: B256,
    generation: u64,
    lifecycle: DurableOcompJobLifecycleV1,
    canonical_finalized_job_spec: Option<Vec<u8>>,
    open_height: Option<u64>,
    deadline_height: Option<u64>,
    terminal_height: Option<u64>,
    canonical_result_vote: Option<Vec<u8>>,
    result_digest: Option<B256>,
    quorum_block: Option<ProjectionCheckpoint>,
    outbox: DurableOutboxStateV1,
}

impl DurableOcompJobV1 {
    #[must_use]
    pub const fn observation_id(&self) -> B256 {
        self.observation_id
    }

    #[must_use]
    pub const fn payload_digest(&self) -> B256 {
        self.payload_digest
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub const fn lifecycle(&self) -> DurableOcompJobLifecycleV1 {
        self.lifecycle
    }

    #[must_use]
    pub const fn outbox(&self) -> &DurableOutboxStateV1 {
        &self.outbox
    }

    #[must_use]
    pub fn canonical_finalized_job_spec(&self) -> Option<&[u8]> {
        self.canonical_finalized_job_spec.as_deref()
    }

    #[must_use]
    pub fn canonical_result_vote(&self) -> Option<&[u8]> {
        self.canonical_result_vote.as_deref()
    }

    #[must_use]
    pub const fn result_digest(&self) -> Option<B256> {
        self.result_digest
    }

    #[must_use]
    pub const fn quorum_block(&self) -> Option<ProjectionCheckpoint> {
        self.quorum_block
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DurableOcompJobTransitionV1 {
    Finalized {
        observation_id: B256,
        canonical_finalized_job_spec: Vec<u8>,
        open_height: u64,
        deadline_height: u64,
    },
    VotingOpen {
        observation_id: B256,
    },
    Completed {
        observation_id: B256,
        canonical_result_vote: Vec<u8>,
        result_digest: B256,
        quorum_block: ProjectionCheckpoint,
    },
    Terminal {
        observation_id: B256,
        terminal_height: u64,
    },
}

impl DurableOcompJobTransitionV1 {
    const fn observation_id(&self) -> B256 {
        match self {
            Self::Finalized { observation_id, .. }
            | Self::VotingOpen { observation_id }
            | Self::Completed { observation_id, .. }
            | Self::Terminal { observation_id, .. } => *observation_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OcompFrameCommitV1 {
    expected_previous: Option<ProjectionCheckpoint>,
    next: ProjectionCheckpoint,
    capsules: Vec<PruningSafeMaterializationCapsuleV1>,
    transitions: Vec<DurableOcompJobTransitionV1>,
}

impl OcompFrameCommitV1 {
    #[must_use]
    pub fn new(
        expected_previous: Option<ProjectionCheckpoint>,
        next: ProjectionCheckpoint,
        capsules: Vec<PruningSafeMaterializationCapsuleV1>,
        transitions: Vec<DurableOcompJobTransitionV1>,
    ) -> Self {
        Self {
            expected_previous,
            next,
            capsules,
            transitions,
        }
    }

    #[must_use]
    pub fn empty(
        expected_previous: Option<ProjectionCheckpoint>,
        next: ProjectionCheckpoint,
    ) -> Self {
        Self::new(expected_previous, next, Vec::new(), Vec::new())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitOutcomeV1 {
    Committed,
    AlreadyCommitted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DurableStateV1 {
    generation: u64,
    checkpoint: Option<ProjectionCheckpoint>,
    last_commit_digest: B256,
    jobs: BTreeMap<B256, DurableOcompJobV1>,
}

impl Default for DurableStateV1 {
    fn default() -> Self {
        Self {
            generation: 0,
            checkpoint: None,
            last_commit_digest: B256::ZERO,
            jobs: BTreeMap::new(),
        }
    }
}

pub struct DurableOcompStoreV1 {
    root: PathBuf,
    capsules: PathBuf,
    state: DurableStateV1,
    quarantined: bool,
}

impl DurableOcompStoreV1 {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, DurableOcompStoreErrorV1> {
        let root = root.as_ref().to_path_buf();
        ensure_private_directory(&root)?;
        let capsules = root.join(CAPSULE_DIRECTORY);
        ensure_private_directory(&capsules)?;
        recover_unpublished_state(&root)?;
        let state_path = root.join(STATE_FILE);
        let state = if state_path
            .try_exists()
            .map_err(|source| io_error("inspect state", state_path.clone(), source))?
        {
            decode_state(&read_bounded_file(&state_path, MAX_STATE_FILE_BYTES)?)?
        } else {
            DurableStateV1::default()
        };
        let store = Self {
            root,
            capsules,
            state,
            quarantined: false,
        };
        store.verify_referenced_capsules()?;
        Ok(store)
    }

    #[must_use]
    pub const fn checkpoint(&self) -> Option<ProjectionCheckpoint> {
        self.state.checkpoint
    }

    pub fn jobs(&self) -> impl ExactSizeIterator<Item = &DurableOcompJobV1> {
        self.state.jobs.values()
    }

    #[must_use]
    pub fn job(&self, observation_id: B256) -> Option<&DurableOcompJobV1> {
        self.state.jobs.get(&observation_id)
    }

    pub fn load_capsule(
        &self,
        observation_id: B256,
    ) -> Result<PruningSafeMaterializationCapsuleV1, DurableOcompStoreErrorV1> {
        let job = self
            .state
            .jobs
            .get(&observation_id)
            .ok_or(DurableOcompStoreErrorV1::UnknownObservation(observation_id))?;
        let capsule = read_capsule(&capsule_path(&self.capsules, observation_id))?;
        if capsule.observation_id != observation_id || capsule.payload_digest != job.payload_digest
        {
            return Err(DurableOcompStoreErrorV1::CapsuleConflict(observation_id));
        }
        Ok(capsule)
    }

    pub fn commit_frame(
        &mut self,
        commit: OcompFrameCommitV1,
    ) -> Result<CommitOutcomeV1, DurableOcompStoreErrorV1> {
        self.require_healthy()?;
        let commit_digest = frame_commit_digest(&commit)?;
        if let Some(current) = self.state.checkpoint {
            if commit.next.block_number == current.block_number {
                if commit.next == current && commit_digest == self.state.last_commit_digest {
                    return Ok(CommitOutcomeV1::AlreadyCommitted);
                }
                return Err(DurableOcompStoreErrorV1::ConflictingReplay {
                    block_number: commit.next.block_number,
                });
            }
        }
        if commit.expected_previous != self.state.checkpoint {
            return Err(DurableOcompStoreErrorV1::UnexpectedPreviousCheckpoint {
                expected: self.state.checkpoint,
                actual: commit.expected_previous,
            });
        }
        if commit.next.block_hash.is_zero() {
            return Err(DurableOcompStoreErrorV1::InvalidCheckpoint);
        }
        if let Some(previous) = self.state.checkpoint {
            let expected_number = previous
                .block_number
                .checked_add(1)
                .ok_or(DurableOcompStoreErrorV1::Capacity)?;
            if commit.next.block_number != expected_number {
                return Err(DurableOcompStoreErrorV1::NonSequentialCheckpoint {
                    previous: previous.block_number,
                    next: commit.next.block_number,
                });
            }
        }
        validate_commit_uniqueness(&commit)?;

        for capsule in &commit.capsules {
            self.persist_capsule(capsule)?;
        }
        let mut next_state = self.state.clone();
        for capsule in &commit.capsules {
            match next_state.jobs.get(&capsule.observation_id) {
                Some(existing) if existing.payload_digest == capsule.payload_digest => {}
                Some(_) => {
                    return Err(DurableOcompStoreErrorV1::CapsuleConflict(
                        capsule.observation_id,
                    ));
                }
                None => {
                    if next_state.jobs.len() >= MAX_DURABLE_JOBS {
                        return Err(DurableOcompStoreErrorV1::Capacity);
                    }
                    next_state.jobs.insert(
                        capsule.observation_id,
                        DurableOcompJobV1 {
                            observation_id: capsule.observation_id,
                            payload_digest: capsule.payload_digest,
                            generation: 1,
                            lifecycle: DurableOcompJobLifecycleV1::Observed,
                            canonical_finalized_job_spec: None,
                            open_height: None,
                            deadline_height: None,
                            terminal_height: None,
                            canonical_result_vote: None,
                            result_digest: None,
                            quorum_block: None,
                            outbox: DurableOutboxStateV1::Pending {
                                delivery_attempts: 0,
                            },
                        },
                    );
                }
            }
        }
        for transition in &commit.transitions {
            apply_transition(&mut next_state, transition)?;
        }
        next_state.generation = next_state
            .generation
            .checked_add(1)
            .ok_or(DurableOcompStoreErrorV1::Capacity)?;
        next_state.checkpoint = Some(commit.next);
        next_state.last_commit_digest = commit_digest;
        if let Err(error) = persist_state(&self.root, &next_state) {
            self.quarantined = true;
            return Err(error);
        }
        self.state = next_state;
        Ok(CommitOutcomeV1::Committed)
    }

    fn require_healthy(&self) -> Result<(), DurableOcompStoreErrorV1> {
        if self.quarantined {
            Err(DurableOcompStoreErrorV1::Quarantined)
        } else {
            Ok(())
        }
    }

    fn persist_capsule(
        &self,
        capsule: &PruningSafeMaterializationCapsuleV1,
    ) -> Result<(), DurableOcompStoreErrorV1> {
        let path = capsule_path(&self.capsules, capsule.observation_id);
        if path
            .try_exists()
            .map_err(|source| io_error("inspect capsule", path.clone(), source))?
        {
            if read_capsule(&path)? == *capsule {
                return Ok(());
            }
            return Err(DurableOcompStoreErrorV1::CapsuleConflict(
                capsule.observation_id,
            ));
        }
        let temporary = self.capsules.join(format!(
            "{}.pending",
            observation_component(capsule.observation_id)
        ));
        if temporary
            .try_exists()
            .map_err(|source| io_error("inspect pending capsule", temporary.clone(), source))?
        {
            fs::remove_file(&temporary).map_err(|source| {
                io_error("remove unpublished capsule", temporary.clone(), source)
            })?;
        }
        let encoded = encode_capsule(capsule)?;
        write_new_synced(&temporary, &encoded, "write capsule")?;
        fs::rename(&temporary, &path)
            .map_err(|source| io_error("publish capsule", path.clone(), source))?;
        sync_directory(&self.capsules, "sync capsule directory")
    }

    fn verify_referenced_capsules(&self) -> Result<(), DurableOcompStoreErrorV1> {
        for (observation_id, job) in &self.state.jobs {
            let capsule = read_capsule(&capsule_path(&self.capsules, *observation_id))?;
            if capsule.observation_id != *observation_id
                || capsule.payload_digest != job.payload_digest
            {
                return Err(DurableOcompStoreErrorV1::CapsuleConflict(*observation_id));
            }
        }
        Ok(())
    }
}

fn apply_transition(
    state: &mut DurableStateV1,
    transition: &DurableOcompJobTransitionV1,
) -> Result<(), DurableOcompStoreErrorV1> {
    let observation_id = transition.observation_id();
    let job = state
        .jobs
        .get_mut(&observation_id)
        .ok_or(DurableOcompStoreErrorV1::UnknownObservation(observation_id))?;
    match transition {
        DurableOcompJobTransitionV1::Finalized {
            canonical_finalized_job_spec,
            open_height,
            deadline_height,
            ..
        } => {
            if canonical_finalized_job_spec.is_empty()
                || canonical_finalized_job_spec.len() > MAX_RECORD_BYTES
                || open_height >= deadline_height
            {
                return Err(DurableOcompStoreErrorV1::InvalidTransition);
            }
            if job.lifecycle == DurableOcompJobLifecycleV1::Finalized
                && job.canonical_finalized_job_spec.as_deref()
                    == Some(canonical_finalized_job_spec.as_slice())
                && job.open_height == Some(*open_height)
                && job.deadline_height == Some(*deadline_height)
            {
                return Ok(());
            }
            if job.lifecycle != DurableOcompJobLifecycleV1::Observed {
                return Err(DurableOcompStoreErrorV1::InvalidTransition);
            }
            job.lifecycle = DurableOcompJobLifecycleV1::Finalized;
            job.canonical_finalized_job_spec = Some(canonical_finalized_job_spec.clone());
            job.open_height = Some(*open_height);
            job.deadline_height = Some(*deadline_height);
        }
        DurableOcompJobTransitionV1::VotingOpen { .. } => {
            if job.lifecycle == DurableOcompJobLifecycleV1::VotingOpen {
                return Ok(());
            }
            if job.lifecycle != DurableOcompJobLifecycleV1::Finalized {
                return Err(DurableOcompStoreErrorV1::InvalidTransition);
            }
            job.lifecycle = DurableOcompJobLifecycleV1::VotingOpen;
        }
        DurableOcompJobTransitionV1::Completed {
            canonical_result_vote,
            result_digest,
            quorum_block,
            ..
        } => {
            if canonical_result_vote.is_empty()
                || canonical_result_vote.len() > MAX_RECORD_BYTES
                || result_digest.is_zero()
                || quorum_block.block_hash.is_zero()
            {
                return Err(DurableOcompStoreErrorV1::InvalidTransition);
            }
            if job.lifecycle == DurableOcompJobLifecycleV1::Completed
                && job.canonical_result_vote.as_deref() == Some(canonical_result_vote.as_slice())
                && job.result_digest == Some(*result_digest)
                && job.quorum_block == Some(*quorum_block)
            {
                return Ok(());
            }
            if !matches!(
                job.lifecycle,
                DurableOcompJobLifecycleV1::Finalized | DurableOcompJobLifecycleV1::VotingOpen
            ) {
                return Err(DurableOcompStoreErrorV1::InvalidTransition);
            }
            job.lifecycle = DurableOcompJobLifecycleV1::Completed;
            job.canonical_result_vote = Some(canonical_result_vote.clone());
            job.result_digest = Some(*result_digest);
            job.quorum_block = Some(*quorum_block);
        }
        DurableOcompJobTransitionV1::Terminal {
            terminal_height, ..
        } => {
            if job.lifecycle == DurableOcompJobLifecycleV1::Terminal
                && job.terminal_height == Some(*terminal_height)
            {
                return Ok(());
            }
            if job.lifecycle == DurableOcompJobLifecycleV1::Observed || *terminal_height == 0 {
                return Err(DurableOcompStoreErrorV1::InvalidTransition);
            }
            job.lifecycle = DurableOcompJobLifecycleV1::Terminal;
            job.terminal_height = Some(*terminal_height);
        }
    }
    job.generation = job
        .generation
        .checked_add(1)
        .ok_or(DurableOcompStoreErrorV1::Capacity)?;
    Ok(())
}

fn validate_commit_uniqueness(commit: &OcompFrameCommitV1) -> Result<(), DurableOcompStoreErrorV1> {
    let mut capsules = BTreeSet::new();
    for capsule in &commit.capsules {
        if !capsules.insert(capsule.observation_id) {
            return Err(DurableOcompStoreErrorV1::DuplicateObservation(
                capsule.observation_id,
            ));
        }
    }
    let mut transitions = BTreeSet::new();
    for transition in &commit.transitions {
        if !transitions.insert(transition.observation_id()) {
            return Err(DurableOcompStoreErrorV1::DuplicateTransition(
                transition.observation_id(),
            ));
        }
    }
    Ok(())
}

fn frame_commit_digest(commit: &OcompFrameCommitV1) -> Result<B256, DurableOcompStoreErrorV1> {
    let mut encoded = Vec::new();
    encoded.extend_from_slice(b"OUTBE_OCOMP_FRAME_COMMIT_V1");
    encode_optional_checkpoint(&mut encoded, commit.expected_previous);
    encode_checkpoint(&mut encoded, commit.next);
    put_u32(&mut encoded, commit.capsules.len())?;
    for capsule in &commit.capsules {
        encoded.extend_from_slice(capsule.observation_id.as_slice());
        encoded.extend_from_slice(capsule.payload_digest.as_slice());
    }
    put_u32(&mut encoded, commit.transitions.len())?;
    for transition in &commit.transitions {
        encode_transition(&mut encoded, transition)?;
    }
    Ok(keccak256(encoded))
}

fn encode_transition(
    output: &mut Vec<u8>,
    transition: &DurableOcompJobTransitionV1,
) -> Result<(), DurableOcompStoreErrorV1> {
    match transition {
        DurableOcompJobTransitionV1::Finalized {
            observation_id,
            canonical_finalized_job_spec,
            open_height,
            deadline_height,
        } => {
            output.push(1);
            output.extend_from_slice(observation_id.as_slice());
            put_bytes(output, canonical_finalized_job_spec)?;
            output.extend_from_slice(&open_height.to_be_bytes());
            output.extend_from_slice(&deadline_height.to_be_bytes());
        }
        DurableOcompJobTransitionV1::VotingOpen { observation_id } => {
            output.push(2);
            output.extend_from_slice(observation_id.as_slice());
        }
        DurableOcompJobTransitionV1::Completed {
            observation_id,
            canonical_result_vote,
            result_digest,
            quorum_block,
        } => {
            output.push(3);
            output.extend_from_slice(observation_id.as_slice());
            put_bytes(output, canonical_result_vote)?;
            output.extend_from_slice(result_digest.as_slice());
            encode_checkpoint(output, *quorum_block);
        }
        DurableOcompJobTransitionV1::Terminal {
            observation_id,
            terminal_height,
        } => {
            output.push(4);
            output.extend_from_slice(observation_id.as_slice());
            output.extend_from_slice(&terminal_height.to_be_bytes());
        }
    }
    Ok(())
}

fn encode_capsule(
    capsule: &PruningSafeMaterializationCapsuleV1,
) -> Result<Vec<u8>, DurableOcompStoreErrorV1> {
    let payload_len = u64::try_from(capsule.canonical_bytes.len())
        .map_err(|_| DurableOcompStoreErrorV1::Capacity)?;
    let mut encoded = Vec::with_capacity(capsule.canonical_bytes.len().saturating_add(114));
    encoded.extend_from_slice(&CAPSULE_MAGIC);
    encoded.extend_from_slice(&STORE_VERSION.to_be_bytes());
    encoded.extend_from_slice(capsule.observation_id.as_slice());
    encoded.extend_from_slice(&payload_len.to_be_bytes());
    encoded.extend_from_slice(&capsule.canonical_bytes);
    encoded.extend_from_slice(capsule.payload_digest.as_slice());
    let checksum = keccak256(&encoded);
    encoded.extend_from_slice(checksum.as_slice());
    Ok(encoded)
}

fn decode_capsule(
    encoded: &[u8],
) -> Result<PruningSafeMaterializationCapsuleV1, DurableOcompStoreErrorV1> {
    if encoded.len() < 114 {
        return Err(DurableOcompStoreErrorV1::MalformedCapsule);
    }
    let checksum_at = encoded.len() - 32;
    if keccak256(&encoded[..checksum_at]) != B256::from_slice(&encoded[checksum_at..]) {
        return Err(DurableOcompStoreErrorV1::MalformedCapsule);
    }
    let mut input = Decoder::new(&encoded[..checksum_at]);
    if input.fixed::<8>()? != CAPSULE_MAGIC || input.u16()? != STORE_VERSION {
        return Err(DurableOcompStoreErrorV1::MalformedCapsule);
    }
    let observation_id = input.b256()?;
    let length = usize::try_from(input.u64()?).map_err(|_| DurableOcompStoreErrorV1::Capacity)?;
    let canonical_bytes = input.bytes_exact(length)?.to_vec();
    let payload_digest = input.b256()?;
    input.finish()?;
    let capsule = PruningSafeMaterializationCapsuleV1::from_verified_canonical_bytes(
        observation_id,
        canonical_bytes,
    )?;
    if capsule.payload_digest != payload_digest {
        return Err(DurableOcompStoreErrorV1::MalformedCapsule);
    }
    Ok(capsule)
}

fn encode_state(state: &DurableStateV1) -> Result<Vec<u8>, DurableOcompStoreErrorV1> {
    let mut encoded = Vec::new();
    encoded.extend_from_slice(&STATE_MAGIC);
    encoded.extend_from_slice(&STORE_VERSION.to_be_bytes());
    encoded.extend_from_slice(&state.generation.to_be_bytes());
    encode_optional_checkpoint(&mut encoded, state.checkpoint);
    encoded.extend_from_slice(state.last_commit_digest.as_slice());
    put_u32(&mut encoded, state.jobs.len())?;
    for job in state.jobs.values() {
        encode_job(&mut encoded, job)?;
    }
    let checksum = keccak256(&encoded);
    encoded.extend_from_slice(checksum.as_slice());
    if u64::try_from(encoded.len()).map_or(true, |len| len > MAX_STATE_FILE_BYTES) {
        return Err(DurableOcompStoreErrorV1::Capacity);
    }
    Ok(encoded)
}

fn decode_state(encoded: &[u8]) -> Result<DurableStateV1, DurableOcompStoreErrorV1> {
    if encoded.len() < 8 + 2 + 8 + 1 + 32 + 4 + 32 {
        return Err(DurableOcompStoreErrorV1::MalformedState);
    }
    let checksum_at = encoded.len() - 32;
    if keccak256(&encoded[..checksum_at]) != B256::from_slice(&encoded[checksum_at..]) {
        return Err(DurableOcompStoreErrorV1::MalformedState);
    }
    let mut input = Decoder::new(&encoded[..checksum_at]);
    if input.fixed::<8>()? != STATE_MAGIC || input.u16()? != STORE_VERSION {
        return Err(DurableOcompStoreErrorV1::MalformedState);
    }
    let generation = input.u64()?;
    let checkpoint = decode_optional_checkpoint(&mut input)?;
    let last_commit_digest = input.b256()?;
    let count = usize::try_from(input.u32()?).map_err(|_| DurableOcompStoreErrorV1::Capacity)?;
    if count > MAX_DURABLE_JOBS {
        return Err(DurableOcompStoreErrorV1::Capacity);
    }
    let mut jobs = BTreeMap::new();
    for _ in 0..count {
        let job = decode_job(&mut input)?;
        if jobs.insert(job.observation_id, job).is_some() {
            return Err(DurableOcompStoreErrorV1::MalformedState);
        }
    }
    input.finish()?;
    if checkpoint.is_none() != (generation == 0)
        || checkpoint.is_none() != last_commit_digest.is_zero()
    {
        return Err(DurableOcompStoreErrorV1::MalformedState);
    }
    Ok(DurableStateV1 {
        generation,
        checkpoint,
        last_commit_digest,
        jobs,
    })
}

fn encode_job(
    output: &mut Vec<u8>,
    job: &DurableOcompJobV1,
) -> Result<(), DurableOcompStoreErrorV1> {
    output.extend_from_slice(job.observation_id.as_slice());
    output.extend_from_slice(job.payload_digest.as_slice());
    output.extend_from_slice(&job.generation.to_be_bytes());
    output.push(job.lifecycle as u8);
    put_optional_bytes(output, job.canonical_finalized_job_spec.as_deref())?;
    put_optional_u64(output, job.open_height);
    put_optional_u64(output, job.deadline_height);
    put_optional_u64(output, job.terminal_height);
    put_optional_bytes(output, job.canonical_result_vote.as_deref())?;
    put_optional_b256(output, job.result_digest);
    encode_optional_checkpoint(output, job.quorum_block);
    encode_outbox(output, &job.outbox);
    Ok(())
}

fn decode_job(input: &mut Decoder<'_>) -> Result<DurableOcompJobV1, DurableOcompStoreErrorV1> {
    let observation_id = input.b256()?;
    let payload_digest = input.b256()?;
    let generation = input.u64()?;
    let lifecycle = match input.u8()? {
        1 => DurableOcompJobLifecycleV1::Observed,
        2 => DurableOcompJobLifecycleV1::Finalized,
        3 => DurableOcompJobLifecycleV1::VotingOpen,
        4 => DurableOcompJobLifecycleV1::Completed,
        5 => DurableOcompJobLifecycleV1::Terminal,
        _ => return Err(DurableOcompStoreErrorV1::MalformedState),
    };
    let canonical_finalized_job_spec = input.optional_bytes(MAX_RECORD_BYTES)?;
    let open_height = input.optional_u64()?;
    let deadline_height = input.optional_u64()?;
    let terminal_height = input.optional_u64()?;
    let canonical_result_vote = input.optional_bytes(MAX_RECORD_BYTES)?;
    let result_digest = input.optional_b256()?;
    let quorum_block = decode_optional_checkpoint(input)?;
    let outbox = decode_outbox(input)?;
    if observation_id.is_zero()
        || payload_digest.is_zero()
        || generation == 0
        || matches!(lifecycle, DurableOcompJobLifecycleV1::Observed)
            && (canonical_finalized_job_spec.is_some()
                || canonical_result_vote.is_some()
                || terminal_height.is_some())
    {
        return Err(DurableOcompStoreErrorV1::MalformedState);
    }
    Ok(DurableOcompJobV1 {
        observation_id,
        payload_digest,
        generation,
        lifecycle,
        canonical_finalized_job_spec,
        open_height,
        deadline_height,
        terminal_height,
        canonical_result_vote,
        result_digest,
        quorum_block,
        outbox,
    })
}

fn encode_outbox(output: &mut Vec<u8>, state: &DurableOutboxStateV1) {
    match state {
        DurableOutboxStateV1::Pending { delivery_attempts } => {
            output.push(1);
            output.extend_from_slice(&delivery_attempts.to_be_bytes());
        }
        DurableOutboxStateV1::Leased {
            delivery_attempts,
            lease_owner,
            delivery_sequence,
            lease_deadline_millis,
        } => {
            output.push(2);
            output.extend_from_slice(&delivery_attempts.to_be_bytes());
            output.extend_from_slice(lease_owner.as_slice());
            output.extend_from_slice(&delivery_sequence.to_be_bytes());
            output.extend_from_slice(&lease_deadline_millis.to_be_bytes());
        }
        DurableOutboxStateV1::Acknowledged {
            delivery_attempts,
            acknowledged_at_millis,
            receipt,
        } => {
            output.push(3);
            output.extend_from_slice(&delivery_attempts.to_be_bytes());
            output.extend_from_slice(&acknowledged_at_millis.to_be_bytes());
            output.extend_from_slice(receipt.transport_digest.as_slice());
            output.extend_from_slice(&receipt.encoded_bytes.to_be_bytes());
            match receipt.expected_ocb1_kind {
                Some(kind) => {
                    output.push(1);
                    output.extend_from_slice(&kind.to_be_bytes());
                }
                None => output.push(0),
            }
            output.extend_from_slice(receipt.receipt_digest.as_slice());
        }
    }
}

fn decode_outbox(
    input: &mut Decoder<'_>,
) -> Result<DurableOutboxStateV1, DurableOcompStoreErrorV1> {
    let tag = input.u8()?;
    let delivery_attempts = input.u32()?;
    match tag {
        1 => Ok(DurableOutboxStateV1::Pending { delivery_attempts }),
        2 => {
            let lease_owner = input.b256()?;
            let delivery_sequence = input.u64()?;
            let lease_deadline_millis = input.u64()?;
            if lease_owner.is_zero() || delivery_sequence == 0 || lease_deadline_millis == 0 {
                return Err(DurableOcompStoreErrorV1::MalformedState);
            }
            Ok(DurableOutboxStateV1::Leased {
                delivery_attempts,
                lease_owner,
                delivery_sequence,
                lease_deadline_millis,
            })
        }
        3 => {
            let acknowledged_at_millis = input.u64()?;
            let transport_digest = input.b256()?;
            let encoded_bytes = input.u64()?;
            let expected_ocb1_kind = match input.u8()? {
                0 => None,
                1 => Some(input.u16()?),
                _ => return Err(DurableOcompStoreErrorV1::MalformedState),
            };
            let receipt_digest = input.b256()?;
            if acknowledged_at_millis == 0
                || transport_digest.is_zero()
                || encoded_bytes == 0
                || receipt_digest.is_zero()
            {
                return Err(DurableOcompStoreErrorV1::MalformedState);
            }
            Ok(DurableOutboxStateV1::Acknowledged {
                delivery_attempts,
                acknowledged_at_millis,
                receipt: ExportReceiptBindingV1 {
                    transport_digest,
                    encoded_bytes,
                    expected_ocb1_kind,
                    receipt_digest,
                },
            })
        }
        _ => Err(DurableOcompStoreErrorV1::MalformedState),
    }
}

fn persist_state(root: &Path, state: &DurableStateV1) -> Result<(), DurableOcompStoreErrorV1> {
    let pending = root.join(STATE_PENDING_FILE);
    let target = root.join(STATE_FILE);
    if pending
        .try_exists()
        .map_err(|source| io_error("inspect pending state", pending.clone(), source))?
    {
        return Err(DurableOcompStoreErrorV1::AmbiguousPendingState);
    }
    write_new_synced(&pending, &encode_state(state)?, "write state")?;
    fs::rename(&pending, &target)
        .map_err(|source| io_error("publish state", target.clone(), source))?;
    sync_directory(root, "sync state directory")
}

fn recover_unpublished_state(root: &Path) -> Result<(), DurableOcompStoreErrorV1> {
    let pending = root.join(STATE_PENDING_FILE);
    if pending
        .try_exists()
        .map_err(|source| io_error("inspect pending state", pending.clone(), source))?
    {
        fs::remove_file(&pending)
            .map_err(|source| io_error("discard unpublished state", pending, source))?;
        sync_directory(root, "sync recovered state directory")?;
    }
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<(), DurableOcompStoreErrorV1> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            Ok(())
        }
        Ok(_) => Err(DurableOcompStoreErrorV1::UnsafePath(path.to_path_buf())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = DirBuilder::new();
            builder.mode(0o700).recursive(true);
            builder
                .create(path)
                .map_err(|source| io_error("create directory", path.to_path_buf(), source))?;
            sync_directory(path, "sync new directory")
        }
        Err(source) => Err(io_error("stat directory", path.to_path_buf(), source)),
    }
}

fn write_new_synced(
    path: &Path,
    encoded: &[u8],
    operation: &'static str,
) -> Result<(), DurableOcompStoreErrorV1> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true).mode(0o600);
    let mut file = options
        .open(path)
        .map_err(|source| io_error(operation, path.to_path_buf(), source))?;
    file.write_all(encoded)
        .and_then(|()| file.sync_all())
        .map_err(|source| io_error(operation, path.to_path_buf(), source))
}

fn sync_directory(path: &Path, operation: &'static str) -> Result<(), DurableOcompStoreErrorV1> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(operation, path.to_path_buf(), source))
}

fn read_bounded_file(path: &Path, maximum: u64) -> Result<Vec<u8>, DurableOcompStoreErrorV1> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("stat durable file", path.to_path_buf(), source))?;
    if !metadata.file_type().is_file() || metadata.len() > maximum {
        return Err(DurableOcompStoreErrorV1::UnsafePath(path.to_path_buf()));
    }
    let capacity =
        usize::try_from(metadata.len()).map_err(|_| DurableOcompStoreErrorV1::Capacity)?;
    let mut encoded = Vec::with_capacity(capacity);
    File::open(path)
        .and_then(|mut file| file.read_to_end(&mut encoded))
        .map_err(|source| io_error("read durable file", path.to_path_buf(), source))?;
    if encoded.len() != capacity {
        return Err(DurableOcompStoreErrorV1::MalformedState);
    }
    Ok(encoded)
}

fn read_capsule(
    path: &Path,
) -> Result<PruningSafeMaterializationCapsuleV1, DurableOcompStoreErrorV1> {
    decode_capsule(&read_bounded_file(path, MAX_CAPSULE_FILE_BYTES)?)
}

fn capsule_path(root: &Path, observation_id: B256) -> PathBuf {
    root.join(format!(
        "{}{}",
        observation_component(observation_id),
        CAPSULE_SUFFIX
    ))
}

fn observation_component(observation_id: B256) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut component = String::with_capacity(64);
    for byte in observation_id.as_slice() {
        component.push(char::from(HEX[usize::from(byte >> 4)]));
        component.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    component
}

fn encode_checkpoint(output: &mut Vec<u8>, checkpoint: ProjectionCheckpoint) {
    output.extend_from_slice(&checkpoint.block_number.to_be_bytes());
    output.extend_from_slice(checkpoint.block_hash.as_slice());
}

fn encode_optional_checkpoint(output: &mut Vec<u8>, checkpoint: Option<ProjectionCheckpoint>) {
    match checkpoint {
        Some(checkpoint) => {
            output.push(1);
            encode_checkpoint(output, checkpoint);
        }
        None => output.push(0),
    }
}

fn decode_optional_checkpoint(
    input: &mut Decoder<'_>,
) -> Result<Option<ProjectionCheckpoint>, DurableOcompStoreErrorV1> {
    match input.u8()? {
        0 => Ok(None),
        1 => {
            let checkpoint = ProjectionCheckpoint {
                block_number: input.u64()?,
                block_hash: input.b256()?,
            };
            if checkpoint.block_hash.is_zero() {
                return Err(DurableOcompStoreErrorV1::MalformedState);
            }
            Ok(Some(checkpoint))
        }
        _ => Err(DurableOcompStoreErrorV1::MalformedState),
    }
}

fn put_u32(output: &mut Vec<u8>, value: usize) -> Result<(), DurableOcompStoreErrorV1> {
    output.extend_from_slice(
        &u32::try_from(value)
            .map_err(|_| DurableOcompStoreErrorV1::Capacity)?
            .to_be_bytes(),
    );
    Ok(())
}

fn put_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), DurableOcompStoreErrorV1> {
    put_u32(output, bytes.len())?;
    output.extend_from_slice(bytes);
    Ok(())
}

fn put_optional_bytes(
    output: &mut Vec<u8>,
    bytes: Option<&[u8]>,
) -> Result<(), DurableOcompStoreErrorV1> {
    match bytes {
        Some(bytes) => {
            output.push(1);
            put_bytes(output, bytes)?;
        }
        None => output.push(0),
    }
    Ok(())
}

fn put_optional_u64(output: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            output.push(1);
            output.extend_from_slice(&value.to_be_bytes());
        }
        None => output.push(0),
    }
}

fn put_optional_b256(output: &mut Vec<u8>, value: Option<B256>) {
    match value {
        Some(value) => {
            output.push(1);
            output.extend_from_slice(value.as_slice());
        }
        None => output.push(0),
    }
}

struct Decoder<'a> {
    encoded: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    const fn new(encoded: &'a [u8]) -> Self {
        Self { encoded, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], DurableOcompStoreErrorV1> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(DurableOcompStoreErrorV1::Capacity)?;
        let bytes = self
            .encoded
            .get(self.offset..end)
            .ok_or(DurableOcompStoreErrorV1::MalformedState)?;
        self.offset = end;
        Ok(bytes)
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], DurableOcompStoreErrorV1> {
        self.take(N)?
            .try_into()
            .map_err(|_| DurableOcompStoreErrorV1::MalformedState)
    }

    fn bytes_exact(&mut self, length: usize) -> Result<&'a [u8], DurableOcompStoreErrorV1> {
        self.take(length)
    }

    fn u8(&mut self) -> Result<u8, DurableOcompStoreErrorV1> {
        Ok(self.fixed::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, DurableOcompStoreErrorV1> {
        Ok(u16::from_be_bytes(self.fixed()?))
    }

    fn u32(&mut self) -> Result<u32, DurableOcompStoreErrorV1> {
        Ok(u32::from_be_bytes(self.fixed()?))
    }

    fn u64(&mut self) -> Result<u64, DurableOcompStoreErrorV1> {
        Ok(u64::from_be_bytes(self.fixed()?))
    }

    fn b256(&mut self) -> Result<B256, DurableOcompStoreErrorV1> {
        Ok(B256::from(self.fixed::<32>()?))
    }

    fn optional_u64(&mut self) -> Result<Option<u64>, DurableOcompStoreErrorV1> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.u64()?)),
            _ => Err(DurableOcompStoreErrorV1::MalformedState),
        }
    }

    fn optional_b256(&mut self) -> Result<Option<B256>, DurableOcompStoreErrorV1> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.b256()?)),
            _ => Err(DurableOcompStoreErrorV1::MalformedState),
        }
    }

    fn optional_bytes(
        &mut self,
        maximum: usize,
    ) -> Result<Option<Vec<u8>>, DurableOcompStoreErrorV1> {
        match self.u8()? {
            0 => Ok(None),
            1 => {
                let length =
                    usize::try_from(self.u32()?).map_err(|_| DurableOcompStoreErrorV1::Capacity)?;
                if length == 0 || length > maximum {
                    return Err(DurableOcompStoreErrorV1::MalformedState);
                }
                Ok(Some(self.take(length)?.to_vec()))
            }
            _ => Err(DurableOcompStoreErrorV1::MalformedState),
        }
    }

    fn finish(self) -> Result<(), DurableOcompStoreErrorV1> {
        if self.offset == self.encoded.len() {
            Ok(())
        } else {
            Err(DurableOcompStoreErrorV1::MalformedState)
        }
    }
}

fn io_error(
    operation: &'static str,
    path: PathBuf,
    source: std::io::Error,
) -> DurableOcompStoreErrorV1 {
    DurableOcompStoreErrorV1::Io {
        operation,
        path,
        source,
    }
}

#[derive(Debug, Error)]
pub enum DurableOcompStoreErrorV1 {
    #[error("durable OCOMP store is quarantined after an indeterminate write")]
    Quarantined,
    #[error("durable OCOMP store path is unsafe: {0}")]
    UnsafePath(PathBuf),
    #[error("durable OCOMP state has an ambiguous pending write")]
    AmbiguousPendingState,
    #[error("durable OCOMP state is malformed")]
    MalformedState,
    #[error("durable OCOMP capsule is malformed")]
    MalformedCapsule,
    #[error("durable OCOMP capsule is invalid")]
    InvalidCapsule,
    #[error("durable OCOMP checkpoint is invalid")]
    InvalidCheckpoint,
    #[error("durable OCOMP store capacity exceeded")]
    Capacity,
    #[error("durable OCOMP frame {block_number} conflicts with its committed replay")]
    ConflictingReplay { block_number: u64 },
    #[error("durable OCOMP frame expected previous checkpoint {expected:?}, received {actual:?}")]
    UnexpectedPreviousCheckpoint {
        expected: Option<ProjectionCheckpoint>,
        actual: Option<ProjectionCheckpoint>,
    },
    #[error("durable OCOMP checkpoint skipped from {previous} to {next}")]
    NonSequentialCheckpoint { previous: u64, next: u64 },
    #[error("duplicate OCOMP observation in one frame: {0}")]
    DuplicateObservation(B256),
    #[error("duplicate OCOMP transition in one frame: {0}")]
    DuplicateTransition(B256),
    #[error("conflicting immutable OCOMP capsule: {0}")]
    CapsuleConflict(B256),
    #[error("unknown durable OCOMP observation: {0}")]
    UnknownObservation(B256),
    #[error("invalid durable OCOMP job transition")]
    InvalidTransition,
    #[error("durable OCOMP I/O failed during {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write as _};

    use alloy_primitives::{keccak256, B256};
    use outbe_primitives::projection::ProjectionCheckpoint;

    use super::{
        capsule_path, CommitOutcomeV1, DurableOcompJobLifecycleV1, DurableOcompJobTransitionV1,
        DurableOcompStoreErrorV1, DurableOcompStoreV1, OcompFrameCommitV1,
        PruningSafeMaterializationCapsuleV1, STATE_FILE, STATE_PENDING_FILE,
    };

    fn checkpoint(block_number: u64, marker: u8) -> ProjectionCheckpoint {
        ProjectionCheckpoint {
            block_number,
            block_hash: B256::repeat_byte(marker),
        }
    }

    fn capsule(marker: u8) -> PruningSafeMaterializationCapsuleV1 {
        let payload = vec![marker; usize::from(marker) + 3];
        PruningSafeMaterializationCapsuleV1::from_verified_canonical_bytes(
            keccak256([b"observation".as_slice(), &[marker]].concat()),
            payload,
        )
        .expect("valid capsule")
    }

    #[test]
    fn empty_frames_advance_the_durable_ocomp_checkpoint_across_restart() {
        let root = tempfile::tempdir().unwrap();
        let mut store = DurableOcompStoreV1::open(root.path()).unwrap();

        assert_eq!(store.checkpoint(), None);
        assert_eq!(
            store
                .commit_frame(OcompFrameCommitV1::empty(None, checkpoint(1, 1)))
                .unwrap(),
            CommitOutcomeV1::Committed
        );
        assert_eq!(
            store
                .commit_frame(OcompFrameCommitV1::empty(
                    Some(checkpoint(1, 1)),
                    checkpoint(2, 2),
                ))
                .unwrap(),
            CommitOutcomeV1::Committed
        );
        drop(store);

        let reopened = DurableOcompStoreV1::open(root.path()).unwrap();
        assert_eq!(reopened.checkpoint(), Some(checkpoint(2, 2)));
        assert_eq!(reopened.jobs().len(), 0);
    }

    #[test]
    fn capsule_and_checkpoint_are_published_as_one_logical_commit() {
        let root = tempfile::tempdir().unwrap();
        let mut store = DurableOcompStoreV1::open(root.path()).unwrap();
        let capsule = capsule(7);
        let observation_id = capsule.observation_id();
        let payload_digest = capsule.payload_digest();
        let payload = capsule.canonical_bytes().to_vec();

        store
            .commit_frame(OcompFrameCommitV1::new(
                None,
                checkpoint(1, 1),
                vec![capsule],
                Vec::new(),
            ))
            .unwrap();
        drop(store);

        let reopened = DurableOcompStoreV1::open(root.path()).unwrap();
        let job = reopened.job(observation_id).expect("durable job");
        assert_eq!(job.payload_digest(), payload_digest);
        assert_eq!(
            reopened
                .load_capsule(observation_id)
                .unwrap()
                .canonical_bytes(),
            payload
        );
        assert_eq!(reopened.checkpoint(), Some(checkpoint(1, 1)));
    }

    #[test]
    fn exact_frame_replay_is_idempotent_but_conflicting_replay_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let mut store = DurableOcompStoreV1::open(root.path()).unwrap();
        let first = capsule(3);
        let exact = first.clone();

        store
            .commit_frame(OcompFrameCommitV1::new(
                None,
                checkpoint(1, 1),
                vec![first],
                Vec::new(),
            ))
            .unwrap();
        assert_eq!(
            store
                .commit_frame(OcompFrameCommitV1::new(
                    None,
                    checkpoint(1, 1),
                    vec![exact],
                    Vec::new(),
                ))
                .unwrap(),
            CommitOutcomeV1::AlreadyCommitted
        );

        let error = store
            .commit_frame(OcompFrameCommitV1::new(
                None,
                checkpoint(1, 9),
                Vec::new(),
                Vec::new(),
            ))
            .unwrap_err();
        assert!(matches!(
            error,
            DurableOcompStoreErrorV1::ConflictingReplay { block_number: 1 }
        ));

        let error = store
            .commit_frame(OcompFrameCommitV1::empty(
                Some(checkpoint(1, 1)),
                checkpoint(3, 3),
            ))
            .unwrap_err();
        assert!(matches!(
            error,
            DurableOcompStoreErrorV1::NonSequentialCheckpoint { .. }
        ));
    }

    #[test]
    fn an_active_job_does_not_hold_back_later_empty_frames() {
        let root = tempfile::tempdir().unwrap();
        let mut store = DurableOcompStoreV1::open(root.path()).unwrap();
        let capsule = capsule(5);
        let observation_id = capsule.observation_id();
        store
            .commit_frame(OcompFrameCommitV1::new(
                None,
                checkpoint(1, 1),
                vec![capsule],
                Vec::new(),
            ))
            .unwrap();

        for height in 2..=128 {
            store
                .commit_frame(OcompFrameCommitV1::empty(
                    Some(checkpoint(height - 1, (height - 1) as u8)),
                    checkpoint(height, height as u8),
                ))
                .unwrap();
        }

        assert!(store.job(observation_id).is_some());
        assert_eq!(store.checkpoint(), Some(checkpoint(128, 128)));
    }

    #[test]
    fn crash_before_state_publish_leaves_only_an_ignored_orphan_capsule() {
        let root = tempfile::tempdir().unwrap();
        let store = DurableOcompStoreV1::open(root.path()).unwrap();
        let capsule = capsule(11);
        let observation_id = capsule.observation_id();
        store.persist_capsule(&capsule).unwrap();
        assert!(capsule_path(&store.capsules, observation_id).is_file());
        drop(store);

        let mut reopened = DurableOcompStoreV1::open(root.path()).unwrap();
        assert_eq!(reopened.checkpoint(), None);
        assert!(reopened.job(observation_id).is_none());
        reopened
            .commit_frame(OcompFrameCommitV1::new(
                None,
                checkpoint(1, 1),
                vec![capsule],
                Vec::new(),
            ))
            .unwrap();
        assert!(reopened.job(observation_id).is_some());
    }

    #[test]
    fn crash_before_state_rename_discards_pending_and_keeps_last_published_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let mut store = DurableOcompStoreV1::open(root.path()).unwrap();
        store
            .commit_frame(OcompFrameCommitV1::empty(None, checkpoint(1, 1)))
            .unwrap();
        drop(store);

        let pending = root.path().join(STATE_PENDING_FILE);
        let mut file = fs::File::create(&pending).unwrap();
        file.write_all(b"interrupted state write").unwrap();
        file.sync_all().unwrap();

        let reopened = DurableOcompStoreV1::open(root.path()).unwrap();
        assert_eq!(reopened.checkpoint(), Some(checkpoint(1, 1)));
        assert!(!pending.exists());
    }

    #[test]
    fn finalized_and_quorum_evidence_survive_restart_without_a_provider() {
        let root = tempfile::tempdir().unwrap();
        let mut store = DurableOcompStoreV1::open(root.path()).unwrap();
        let capsule = capsule(13);
        let observation_id = capsule.observation_id();
        store
            .commit_frame(OcompFrameCommitV1::new(
                None,
                checkpoint(1, 1),
                vec![capsule],
                Vec::new(),
            ))
            .unwrap();
        store
            .commit_frame(OcompFrameCommitV1::new(
                Some(checkpoint(1, 1)),
                checkpoint(2, 2),
                Vec::new(),
                vec![DurableOcompJobTransitionV1::Finalized {
                    observation_id,
                    canonical_finalized_job_spec: b"canonical-job-spec".to_vec(),
                    open_height: 3,
                    deadline_height: 1_803,
                }],
            ))
            .unwrap();
        store
            .commit_frame(OcompFrameCommitV1::new(
                Some(checkpoint(2, 2)),
                checkpoint(3, 3),
                Vec::new(),
                vec![DurableOcompJobTransitionV1::VotingOpen { observation_id }],
            ))
            .unwrap();
        let result_digest = B256::repeat_byte(0x51);
        store
            .commit_frame(OcompFrameCommitV1::new(
                Some(checkpoint(3, 3)),
                checkpoint(4, 4),
                Vec::new(),
                vec![DurableOcompJobTransitionV1::Completed {
                    observation_id,
                    canonical_result_vote: b"canonical-result-vote".to_vec(),
                    result_digest,
                    quorum_block: checkpoint(4, 4),
                }],
            ))
            .unwrap();
        drop(store);

        let reopened = DurableOcompStoreV1::open(root.path()).unwrap();
        let job = reopened.job(observation_id).unwrap();
        assert_eq!(job.lifecycle(), DurableOcompJobLifecycleV1::Completed);
        assert_eq!(
            job.canonical_finalized_job_spec(),
            Some(b"canonical-job-spec".as_slice())
        );
        assert_eq!(
            job.canonical_result_vote(),
            Some(b"canonical-result-vote".as_slice())
        );
        assert_eq!(job.result_digest(), Some(result_digest));
        assert_eq!(job.quorum_block(), Some(checkpoint(4, 4)));
    }

    #[test]
    fn tampered_published_state_fails_closed_on_restart() {
        let root = tempfile::tempdir().unwrap();
        let mut store = DurableOcompStoreV1::open(root.path()).unwrap();
        store
            .commit_frame(OcompFrameCommitV1::empty(None, checkpoint(1, 1)))
            .unwrap();
        drop(store);

        let path = root.path().join(STATE_FILE);
        let mut encoded = fs::read(&path).unwrap();
        encoded[10] ^= 1;
        fs::write(path, encoded).unwrap();

        let error = match DurableOcompStoreV1::open(root.path()) {
            Ok(_) => panic!("tampered durable state must fail closed"),
            Err(error) => error,
        };
        assert!(matches!(error, DurableOcompStoreErrorV1::MalformedState));
    }
}
