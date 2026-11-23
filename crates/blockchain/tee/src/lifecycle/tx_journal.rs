//! One writer, one unresolved signed service transaction; no resident secrets.
use super::rpc_port::{LifecycleCheckpointV1, LifecycleUpgradeCandidateV1};
use crate::{
    journal_storage::{private_file_options, sync_directory, JournalPaths, JournalStorageError},
    registry_view::RenewalBindingV1,
};
use alloy_consensus::{transaction::RlpEcdsaDecodableTx, TxEip1559};
use alloy_primitives::{keccak256, Address, Bytes, B256};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::Read as _,
    os::unix::fs::{
        DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
    },
    path::Path,
};

// Match the existing public host transaction journal storage ceiling.
const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceTxKindV1 {
    Register,
    Renew,
    UpgradePrepare,
    UpgradeTransition,
    UpgradeCancel,
}

/// Exact expected canonical fields, not a claim that the RAM key exists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceBindingTargetV1 {
    pub node_id_hash: B256,
    pub enclave_id: B256,
    pub binding_id: B256,
    pub binding_version: u64,
    pub registration_version: u64,
    pub renewal_nonce: u64,
    pub transition_nonce: u64,
    pub valid_until: u64,
    pub policy_hash: B256,
    pub recipient_x25519: B256,
}
impl ServiceBindingTargetV1 {
    pub(super) fn matches(&self, binding: &RenewalBindingV1) -> bool {
        *self
            == Self {
                node_id_hash: binding.node_id_hash,
                enclave_id: binding.enclave_id,
                binding_id: binding.binding_id,
                binding_version: binding.binding_version,
                registration_version: binding.registration_version,
                renewal_nonce: binding.renewal_nonce,
                transition_nonce: binding.transition_nonce,
                valid_until: binding.valid_until,
                policy_hash: binding.policy_hash,
                recipient_x25519: binding.recipient_x25519,
            }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceTxIntentV1 {
    pub target: ServiceBindingTargetV1,
    /// Required for prepare/cancel; pins the candidate context, source and nonce.
    pub candidate: Option<LifecycleUpgradeCandidateV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceTxStateV1 {
    Pending,
    Completed,
    DiscardedLostIdentity,
    RebuildRequired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceTxRecordV1 {
    pub raw_signed_tx: Bytes,
    pub tx_hash: B256,
    pub sender: Address,
    pub nonce: u64,
    pub kind: ServiceTxKindV1,
    pub public_intent: ServiceTxIntentV1,
    pub finalized_checkpoint: LifecycleCheckpointV1,
    pub state: ServiceTxStateV1,
    pub reconciled_checkpoint: Option<LifecycleCheckpointV1>,
}

/// Typed proof issued only by `reconcile_service_tx_v1` when an unfinalized
/// `Pending` record belongs to a lost process identity and its nonce slot is free.
/// Authorizes cross-kind replacement via [`ServiceTxJournalV1::begin_replacing`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LostIdentityProof {
    tx_hash: B256,
    sender: Address,
    nonce: u64,
    chain_id: u64,
    genesis_hash: B256,
}

impl LostIdentityProof {
    pub(super) const fn new(record: &ServiceTxRecordV1) -> Self {
        Self {
            tx_hash: record.tx_hash,
            sender: record.sender,
            nonce: record.nonce,
            chain_id: record.finalized_checkpoint.chain_id,
            genesis_hash: record.finalized_checkpoint.genesis_hash,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub(super) const fn nonce(&self) -> u64 {
        self.nonce
    }

    #[cfg(test)]
    #[must_use]
    pub(super) const fn tx_hash(&self) -> B256 {
        self.tx_hash
    }

    fn matches_pending(&self, old: &ServiceTxRecordV1) -> bool {
        (
            self.tx_hash,
            self.sender,
            self.nonce,
            self.chain_id,
            self.genesis_hash,
        ) == (
            old.tx_hash,
            old.sender,
            old.nonce,
            old.finalized_checkpoint.chain_id,
            old.finalized_checkpoint.genesis_hash,
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceTxJournalError {
    #[error("service journal I/O failed")]
    Io(#[from] std::io::Error),
    #[error("service journal JSON rejected")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Storage(#[from] JournalStorageError),
    #[error("signed EIP-1559 transaction rejected")]
    TransactionDecode(#[from] alloy_rlp::Error),
    #[error("transaction signature rejected")]
    Signature(#[from] alloy_primitives::SignatureError),
    #[error("service transaction calldata rejected")]
    CallDecode(#[from] alloy_sol_types::Error),
    #[error("service transaction evidence rejected")]
    EvidenceCodec(#[from] outbe_primitives::tee_attestation_v1::CodecError),
    #[error("journal file or directory is not owner-only regular state")]
    UnsafePath,
    #[error("service transaction journal exceeds size cap")]
    Oversized,
    #[error("service transaction metadata disagrees with signed envelope")]
    InvalidTransaction,
    #[error("service intent or checkpoint is incomplete")]
    InvalidIntent,
    #[error("journal has an unresolved transaction")]
    UnresolvedTransaction,
    #[error("journal outcome must name the current pending transaction")]
    TransactionMismatch,
    #[error("durability is uncertain; reopen and reconcile before writing")]
    DurabilityUncertain,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u8,
    record: ServiceTxRecordV1,
}

impl ServiceTxRecordV1 {
    pub fn validate(&self) -> Result<(), ServiceTxJournalError> {
        if self.raw_signed_tx.first() != Some(&2) {
            return Err(ServiceTxJournalError::InvalidTransaction);
        }
        let mut encoded = &self.raw_signed_tx[1..];
        let signed = TxEip1559::rlp_decode_signed(&mut encoded)?;
        let tx = signed.tx();
        let metadata = (tx.nonce, tx.chain_id, signed.recover_signer()?);
        let expected = (self.nonce, self.finalized_checkpoint.chain_id, self.sender);
        let registry =
            alloy_primitives::TxKind::Call(outbe_primitives::addresses::TEE_REGISTRY_ADDRESS);
        if !encoded.is_empty()
            || keccak256(&self.raw_signed_tx) != self.tx_hash
            || metadata != expected
            || tx.to != registry
        {
            return Err(ServiceTxJournalError::InvalidTransaction);
        }
        let target = &self.public_intent.target;
        let candidate_required = matches!(
            self.kind,
            ServiceTxKindV1::UpgradePrepare | ServiceTxKindV1::UpgradeCancel
        );
        let public_fields = [
            target.enclave_id,
            target.node_id_hash,
            target.binding_id,
            target.recipient_x25519,
            target.policy_hash,
            self.finalized_checkpoint.hash,
            self.finalized_checkpoint.genesis_hash,
        ];
        let fields_present = public_fields.iter().all(|value| !value.is_zero());
        let state_consistent =
            (self.state == ServiceTxStateV1::Pending) == self.reconciled_checkpoint.is_none();
        if candidate_required != self.public_intent.candidate.is_some()
            || !fields_present
            || !state_consistent
        {
            return Err(ServiceTxJournalError::InvalidIntent);
        }
        if self.kind == ServiceTxKindV1::Renew {
            self.validate_renew_calldata(&tx.input)?;
        }
        Ok(())
    }

    fn validate_renew_calldata(&self, input: &[u8]) -> Result<(), ServiceTxJournalError> {
        use alloy_primitives::U256;
        use alloy_sol_types::SolCall as _;
        use outbe_primitives::{
            tee_attestation_v1::{AttestationEvidenceV1, AttestationOperationV1},
            tee_registry_abi_v1::ITeeRegistryV1,
        };

        let call = ITeeRegistryV1::renewEnclaveCall::abi_decode(input)?;
        if call.abi_encode() != input {
            return Err(ServiceTxJournalError::InvalidTransaction);
        }
        let evidence = AttestationEvidenceV1::decode_canonical(&call.evidence)?;
        if call.evidence.as_ref() != evidence.encode_canonical()?.as_slice() {
            return Err(ServiceTxJournalError::InvalidTransaction);
        }
        let intent = evidence.intent();
        if intent.operation != AttestationOperationV1::RenewEnclave
            || intent.derived_enclave_id()? != intent.enclave_id
            || U256::from_be_bytes(intent.chain_id)
                != U256::from(self.finalized_checkpoint.chain_id)
            || intent.genesis_hash != self.finalized_checkpoint.genesis_hash
        {
            return Err(ServiceTxJournalError::InvalidIntent);
        }
        let node_signature: [u8; 65] = call
            .nodeSignature
            .as_ref()
            .try_into()
            .map_err(|_| ServiceTxJournalError::InvalidIntent)?;
        let enclave_signature: [u8; 64] = call
            .enclaveSignature
            .as_ref()
            .try_into()
            .map_err(|_| ServiceTxJournalError::InvalidIntent)?;
        if !intent.verify_node_signature(&node_signature)
            || !intent.verify_enclave_signature(&enclave_signature)
        {
            return Err(ServiceTxJournalError::InvalidIntent);
        }
        if let AttestationEvidenceV1::NoAttestationDev(dev) = &evidence {
            if dev.dev_attestation_public != intent.attestation_ed25519
                || dev.dev_signature != enclave_signature
                || !intent.verify_enclave_signature(&dev.dev_signature)
            {
                return Err(ServiceTxJournalError::InvalidIntent);
            }
        }
        if super::renewal::prepare::target_v1(intent)? != self.public_intent.target {
            return Err(ServiceTxJournalError::InvalidIntent);
        }
        Ok(())
    }

    /// Prepare keeps the source binding as its mutation target, but owns a RAM candidate.
    pub(super) fn process_identity(&self) -> Result<(B256, B256), ServiceTxJournalError> {
        if self.kind != ServiceTxKindV1::UpgradePrepare {
            return Ok((
                self.public_intent.target.enclave_id,
                self.public_intent.target.recipient_x25519,
            ));
        }
        use alloy_sol_types::SolCall as _;
        use outbe_primitives::{
            tee_attestation_v1::{AttestationEvidenceV1, AttestationOperationV1},
            tee_registry_abi_v1::ITeeRegistryV1,
        };
        let mut raw = &self.raw_signed_tx[1..];
        let tx = TxEip1559::rlp_decode_signed(&mut raw)?;
        let call = ITeeRegistryV1::prepareEnclaveUpgradeCall::abi_decode(&tx.tx().input)?;
        let evidence = AttestationEvidenceV1::decode_canonical(&call.evidence)?;
        let intent = evidence.intent();
        if intent.operation != AttestationOperationV1::PrepareEnclaveUpgrade {
            return Err(ServiceTxJournalError::InvalidIntent);
        }
        Ok((intent.enclave_id, intent.recipient_x25519.into()))
    }

    fn replaces_lost_identity(&self, old: &Self) -> bool {
        let same_slot = (
            self.sender,
            self.nonce,
            self.finalized_checkpoint.chain_id,
            self.finalized_checkpoint.genesis_hash,
        ) == (
            old.sender,
            old.nonce,
            old.finalized_checkpoint.chain_id,
            old.finalized_checkpoint.genesis_hash,
        );
        let new_identity = match (self.process_identity(), old.process_identity()) {
            (Ok(next), Ok(previous)) => next != previous,
            _ => false,
        };
        same_slot && new_identity && self.tx_hash != old.tx_hash
    }
}

/// Holds the persistent inode lock for the whole load/reconcile/submit sequence.
/// Journal state is loaded on demand, so an uncertain fsync never becomes cached success.
pub struct ServiceTxJournalV1 {
    paths: JournalPaths,
    _lock: File,
    poisoned: bool,
}
impl ServiceTxJournalV1 {
    pub fn open(node_data_dir: &Path) -> Result<Self, ServiceTxJournalError> {
        let paths = JournalPaths::new(node_data_dir, "tee-service-tx-v1");
        match DirBuilder::new().mode(0o700).create(&paths.root) {
            Ok(()) => sync_directory(node_data_dir)?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let m = fs::symlink_metadata(&paths.root)?;
        if !m.is_dir() || !owned_mode(&m, 0o700) {
            return Err(ServiceTxJournalError::UnsafePath);
        }
        let lock = private_file_options(true).open(&paths.lock)?;
        validate_file(&lock)?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(std::io::Error::from)?;
        match fs::symlink_metadata(&paths.next) {
            Ok(m) => {
                if !m.is_file() || !owned_mode(&m, 0o600) {
                    return Err(ServiceTxJournalError::UnsafePath);
                }
                fs::remove_file(&paths.next)?;
                sync_directory(&paths.root)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let journal = Self {
            paths,
            _lock: lock,
            poisoned: false,
        };
        journal.load()?;
        Ok(journal)
    }

    pub fn load(&self) -> Result<Option<ServiceTxRecordV1>, ServiceTxJournalError> {
        let file = match private_file_options(false).open(&self.paths.journal) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        validate_file(&file)?;
        let mut bytes = Vec::new();
        file.take(MAX_JOURNAL_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(ServiceTxJournalError::Oversized);
        }
        let wire: Wire = serde_json::from_slice(&bytes)?;
        if wire.version != 1 {
            return Err(ServiceTxJournalError::InvalidIntent);
        }
        wire.record.validate()?;
        Ok(Some(wire.record))
    }

    /// Persist before sending. Pending replacement requires a new process identity
    /// and signed transaction at the exact same account nonce on the same chain.
    pub fn begin(&mut self, record: ServiceTxRecordV1) -> Result<(), ServiceTxJournalError> {
        if self.poisoned {
            return Err(ServiceTxJournalError::DurabilityUncertain);
        }
        record.validate()?;
        if record.state != ServiceTxStateV1::Pending {
            return Err(ServiceTxJournalError::InvalidIntent);
        }
        if let Some(old) = self.load()? {
            if old == record {
                return Ok(());
            }
            if old.state == ServiceTxStateV1::Pending && !record.replaces_lost_identity(&old) {
                return Err(ServiceTxJournalError::UnresolvedTransaction);
            }
        }
        self.persist(record)
    }

    /// Persist a replacement transaction authorized by a [`LostIdentityProof`] from
    /// `reconcile_service_tx_v1`. Permits cross-kind replacement on the exact same
    /// `(sender, nonce, chain_id, genesis_hash)` slot when the prior `Pending` record
    /// belonged to a lost process identity.
    pub fn begin_replacing(
        &mut self,
        record: ServiceTxRecordV1,
        proof: LostIdentityProof,
    ) -> Result<(), ServiceTxJournalError> {
        if self.poisoned {
            return Err(ServiceTxJournalError::DurabilityUncertain);
        }
        record.validate()?;
        if record.state != ServiceTxStateV1::Pending {
            return Err(ServiceTxJournalError::InvalidIntent);
        }
        if let Some(old) = self.load()? {
            if old == record {
                return Ok(());
            }
            if old.state == ServiceTxStateV1::Pending {
                let same_slot = (
                    record.sender,
                    record.nonce,
                    record.finalized_checkpoint.chain_id,
                    record.finalized_checkpoint.genesis_hash,
                ) == (
                    old.sender,
                    old.nonce,
                    old.finalized_checkpoint.chain_id,
                    old.finalized_checkpoint.genesis_hash,
                );
                if !proof.matches_pending(&old) || !same_slot || record.tx_hash == old.tx_hash {
                    return Err(ServiceTxJournalError::UnresolvedTransaction);
                }
            }
        }
        self.persist(record)
    }

    pub(super) fn finish(
        &mut self,
        tx_hash: B256,
        state: ServiceTxStateV1,
        at: LifecycleCheckpointV1,
    ) -> Result<(), ServiceTxJournalError> {
        let mut record = self
            .load()?
            .ok_or(ServiceTxJournalError::TransactionMismatch)?;
        if record.tx_hash != tx_hash || state == ServiceTxStateV1::Pending {
            return Err(ServiceTxJournalError::TransactionMismatch);
        }
        record.state = state;
        record.reconciled_checkpoint = Some(at);
        self.persist(record)
    }

    fn persist(&mut self, record: ServiceTxRecordV1) -> Result<(), ServiceTxJournalError> {
        if self.poisoned {
            return Err(ServiceTxJournalError::DurabilityUncertain);
        }
        record.validate()?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        self.poisoned = true;
        self.paths.commit("service transaction").commit_json(
            &Wire { version: 1, record },
            &options,
            MAX_JOURNAL_BYTES,
        )?;
        self.poisoned = false;
        Ok(())
    }
}
fn owned_mode(m: &fs::Metadata, mode: u32) -> bool {
    m.uid() == rustix::process::geteuid().as_raw() && m.permissions().mode() & 0o777 == mode
}
fn validate_file(file: &File) -> Result<(), ServiceTxJournalError> {
    let m = file.metadata()?;
    if !m.is_file() || !owned_mode(&m, 0o600) || m.nlink() != 1 {
        return Err(ServiceTxJournalError::UnsafePath);
    }
    if m.len() > MAX_JOURNAL_BYTES {
        return Err(ServiceTxJournalError::Oversized);
    }
    Ok(())
}
