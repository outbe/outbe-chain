//! Owner-only, crash-consistent journal for one DCAP renewal intent.

use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read as _, Write as _},
    os::unix::fs::{
        DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
    },
    path::{Path, PathBuf},
};

use alloy_primitives::{keccak256, Address, B256};
use eyre::{Result, WrapErr as _};
use outbe_primitives::tee_attestation_v1::{AttestationEvidenceV1, RegistrationIntentV1};
use outbe_tee::dcap_protocol::dcap_evidence_hash_v1;
use serde::{Deserialize, Serialize};

use crate::tx::RawRelayTransactionV1;

use super::registry::RenewalBindingV1;

const DIRECTORY: &str = "tee-renewal-v1";
const JOURNAL: &str = "journal.json";
const NEXT: &str = "journal.next";
const LOCK: &str = "state.lock";
const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;
const MAX_RELAY_VARIANTS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedRenewalV1 {
    pub source: RenewalBindingV1,
    pub intent: Vec<u8>,
    pub intent_hash: B256,
    pub evidence: Vec<u8>,
    pub evidence_hash: B256,
    pub node_signature: Vec<u8>,
    pub enclave_signature: Vec<u8>,
    pub calldata: Vec<u8>,
    pub calldata_hash: B256,
    pub requested_valid_until: u64,
    pub collateral_valid_until: u64,
    pub collateral_margin: u64,
    pub relay: Address,
    pub relay_variants: Vec<RawRelayTransactionV1>,
}

impl PreparedRenewalV1 {
    fn validate(&self) -> Result<()> {
        if self.intent.is_empty()
            || self.evidence.is_empty()
            || self.calldata.is_empty()
            || self.node_signature.len() != 65
            || self.enclave_signature.len() != 64
            || self.relay_variants.is_empty()
            || self.relay_variants.len() > MAX_RELAY_VARIANTS
        {
            eyre::bail!("renewal journal contains invalid bounded material");
        }
        let intent = RegistrationIntentV1::decode_canonical(&self.intent)
            .map_err(|error| eyre::eyre!("decode renewal journal intent: {error}"))?;
        let intent_hash = intent
            .intent_hash()
            .map_err(|error| eyre::eyre!("hash renewal journal intent: {error}"))?;
        let evidence = AttestationEvidenceV1::decode_canonical(&self.evidence)
            .map_err(|error| eyre::eyre!("decode renewal journal evidence: {error}"))?;
        let evidence_intent = match &evidence {
            AttestationEvidenceV1::Dcap(value) => &value.intent,
            AttestationEvidenceV1::GramineDirectDev(_) => {
                eyre::bail!("automatic renewal journal contains non-DCAP evidence");
            }
        };
        let evidence_hash = dcap_evidence_hash_v1(&self.evidence)
            .map_err(|code| eyre::eyre!("hash renewal journal DCAP evidence: {code:?}"))?;
        if intent_hash != self.intent_hash
            || evidence_intent != &intent
            || evidence_hash != self.evidence_hash
            || keccak256(&self.calldata) != self.calldata_hash
        {
            eyre::bail!("renewal journal hash commitment mismatch");
        }
        let first = &self.relay_variants[0];
        if first.relay != self.relay || first.calldata_hash != self.calldata_hash {
            eyre::bail!("renewal journal relay binding mismatch");
        }
        for variant in &self.relay_variants {
            if variant.relay != self.relay
                || variant.chain_id != first.chain_id
                || variant.account_nonce != first.account_nonce
                || variant.gas_limit != first.gas_limit
                || variant.calldata_hash != self.calldata_hash
                || keccak256(&variant.raw_transaction) != variant.transaction_hash
            {
                eyre::bail!("renewal journal contains a competing relay variant");
            }
        }
        let ceiling = self
            .collateral_valid_until
            .checked_sub(self.collateral_margin)
            .ok_or_else(|| eyre::eyre!("renewal collateral margin underflow"))?;
        if self.requested_valid_until > ceiling {
            eyre::bail!("renewal journal lease exceeds collateral ceiling");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase", deny_unknown_fields)]
#[allow(clippy::large_enum_variant)]
pub enum RenewalJournalStateV1 {
    Prepared {
        attempt: PreparedRenewalV1,
    },
    Submitted {
        attempt: PreparedRenewalV1,
        submitted_at_finalized_height: u64,
        transaction_hashes: Vec<B256>,
    },
    Finalized {
        attempt: Box<PreparedRenewalV1>,
        finalized_binding: RenewalBindingV1,
        finalized_height: u64,
        finalized_hash: B256,
    },
    Abandoned {
        attempt: PreparedRenewalV1,
        abandoned_at_finalized_height: u64,
        reason: String,
    },
}

impl RenewalJournalStateV1 {
    pub fn attempt(&self) -> &PreparedRenewalV1 {
        match self {
            Self::Prepared { attempt }
            | Self::Submitted { attempt, .. }
            | Self::Abandoned { attempt, .. } => attempt,
            Self::Finalized { attempt, .. } => attempt.as_ref(),
        }
    }

    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Prepared { .. } => "prepared",
            Self::Submitted { .. } => "submitted",
            Self::Finalized { .. } => "finalized",
            Self::Abandoned { .. } => "abandoned",
        }
    }

    fn validate(&self) -> Result<()> {
        self.attempt().validate()?;
        if let Self::Submitted {
            attempt,
            transaction_hashes,
            ..
        } = self
        {
            if transaction_hashes.is_empty()
                || transaction_hashes.len() > attempt.relay_variants.len()
                || transaction_hashes
                    .iter()
                    .enumerate()
                    .any(|(index, hash)| attempt.relay_variants[index].transaction_hash != *hash)
            {
                eyre::bail!("submitted renewal journal transaction list is non-canonical");
            }
        }
        if let Self::Finalized {
            attempt,
            finalized_binding,
            finalized_hash,
            ..
        } = self
        {
            if finalized_hash.is_zero()
                || finalized_binding.intent_hash != attempt.intent_hash
                || finalized_binding.evidence_hash != attempt.evidence_hash
            {
                eyre::bail!("finalized renewal journal binding mismatch");
            }
        }
        if let Self::Abandoned { reason, .. } = self {
            if reason.is_empty() || reason.len() > 512 {
                eyre::bail!("abandoned renewal journal reason is invalid");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenewalJournalSnapshotV1 {
    pub version: u8,
    pub generation: u64,
    pub lifecycle: RenewalJournalStateV1,
}

impl RenewalJournalSnapshotV1 {
    pub fn new(lifecycle: RenewalJournalStateV1) -> Self {
        Self {
            version: 1,
            generation: 1,
            lifecycle,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.version != 1 || self.generation == 0 {
            eyre::bail!("unsupported renewal journal version or generation");
        }
        self.lifecycle.validate()
    }
}

pub(crate) struct RenewalJournalGuard {
    paths: JournalPaths,
    _lock: File,
}

impl RenewalJournalGuard {
    pub(crate) fn acquire(node_data_dir: &Path) -> Result<Self> {
        let paths = JournalPaths::new(node_data_dir);
        create_or_validate_directory(&paths.root)?;
        let lock = open_private_file(&paths.lock, true)?;
        if let Err(error) =
            rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        {
            let error = std::io::Error::from(error);
            if error.kind() == std::io::ErrorKind::WouldBlock {
                eyre::bail!("another renewal worker or CLI process owns the journal lock");
            }
            return Err(error).wrap_err("lock renewal journal");
        }
        reconcile_scratch(&paths)?;
        Ok(Self { paths, _lock: lock })
    }

    pub(crate) fn load(&self) -> Result<Option<RenewalJournalSnapshotV1>> {
        read_snapshot(&self.paths.journal)
    }

    pub(crate) fn store(&self, mut snapshot: RenewalJournalSnapshotV1) -> Result<()> {
        if let Some(current) = self.load()? {
            snapshot.generation = current
                .generation
                .checked_add(1)
                .ok_or_else(|| eyre::eyre!("renewal journal generation exhausted"))?;
        }
        snapshot.validate()?;
        let encoded = serde_json::to_vec(&snapshot).wrap_err("encode renewal journal")?;
        if encoded.len() as u64 > MAX_JOURNAL_BYTES {
            eyre::bail!("renewal journal exceeds its size cap");
        }
        let mut next = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(&self.paths.next)
            .wrap_err("create renewal journal scratch")?;
        next.write_all(&encoded)
            .wrap_err("write renewal journal scratch")?;
        next.sync_all().wrap_err("fsync renewal journal scratch")?;
        fs::rename(&self.paths.next, &self.paths.journal).wrap_err("commit renewal journal")?;
        sync_directory(&self.paths.root)
    }
}

pub(crate) fn inspect_journal(node_data_dir: &Path) -> Result<Option<RenewalJournalSnapshotV1>> {
    let paths = JournalPaths::new(node_data_dir);
    if !paths.root.exists() {
        return Ok(None);
    }
    validate_directory(&paths.root)?;
    read_snapshot(&paths.journal)
}

#[derive(Clone)]
struct JournalPaths {
    root: PathBuf,
    journal: PathBuf,
    next: PathBuf,
    lock: PathBuf,
}

impl JournalPaths {
    fn new(node_data_dir: &Path) -> Self {
        let root = node_data_dir.join(DIRECTORY);
        Self {
            journal: root.join(JOURNAL),
            next: root.join(NEXT),
            lock: root.join(LOCK),
            root,
        }
    }
}

fn create_or_validate_directory(path: &Path) -> Result<()> {
    let mut builder = DirBuilder::new();
    builder.mode(DIRECTORY_MODE);
    match builder.create(path) {
        Ok(()) => sync_directory(
            path.parent()
                .ok_or_else(|| eyre::eyre!("renewal directory has no parent"))?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => validate_directory(path),
        Err(error) => Err(error).wrap_err("create renewal journal directory"),
    }
}

fn validate_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).wrap_err("stat renewal journal directory")?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != DIRECTORY_MODE
    {
        eyre::bail!("renewal journal directory is not owner-only 0700");
    }
    Ok(())
}

fn open_private_file(path: &Path, create: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .mode(FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .wrap_err_with(|| format!("open private renewal file {}", path.display()))?;
    validate_private_file(path)?;
    Ok(file)
}

fn validate_private_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .wrap_err_with(|| format!("stat renewal file {}", path.display()))?;
    if !metadata.file_type().is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != FILE_MODE
    {
        eyre::bail!("renewal journal file is not owner-only 0600");
    }
    if metadata.len() > MAX_JOURNAL_BYTES {
        eyre::bail!("renewal journal file exceeds its size cap");
    }
    Ok(())
}

fn reconcile_scratch(paths: &JournalPaths) -> Result<()> {
    if paths.next.exists() {
        validate_private_file(&paths.next)?;
        fs::remove_file(&paths.next).wrap_err("discard incomplete renewal journal scratch")?;
        sync_directory(&paths.root)?;
    }
    Ok(())
}

fn read_snapshot(path: &Path) -> Result<Option<RenewalJournalSnapshotV1>> {
    if !path.exists() {
        return Ok(None);
    }
    validate_private_file(path)?;
    let mut bytes = Vec::new();
    File::open(path)
        .wrap_err("open renewal journal")?
        .take(MAX_JOURNAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .wrap_err("read renewal journal")?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        eyre::bail!("renewal journal exceeds its size cap");
    }
    let snapshot: RenewalJournalSnapshotV1 =
        serde_json::from_slice(&bytes).wrap_err("decode renewal journal")?;
    snapshot.validate()?;
    Ok(Some(snapshot))
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .wrap_err_with(|| format!("open directory {} for fsync", path.display()))?
        .sync_all()
        .wrap_err_with(|| format!("fsync directory {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use outbe_primitives::tee_attestation_v1::{
        AttestationMode, AttestationOperationV1, DcapCollateralComponentV1, DcapCollateralKind,
        DcapEvidenceV1, NodeIdV1,
    };

    fn binding() -> RenewalBindingV1 {
        RenewalBindingV1 {
            node_id_hash: B256::repeat_byte(1),
            enclave_id: B256::repeat_byte(2),
            binding_id: B256::repeat_byte(3),
            intent_hash: B256::repeat_byte(4),
            evidence_hash: B256::repeat_byte(5),
            policy_hash: B256::repeat_byte(6),
            binding_version: 1,
            registration_version: 0,
            renewal_nonce: 0,
            transition_nonce: 0,
            lease_started_at: 100,
            valid_until: 200,
            collateral_valid_until: 300,
            recipient_x25519: B256::repeat_byte(7),
            attestation_ed25519: B256::repeat_byte(8),
            noise_responder_x25519: B256::repeat_byte(9),
            mrenclave: B256::repeat_byte(10),
            mrsigner: B256::repeat_byte(11),
            isv_prod_id: 1,
            isv_svn: 2,
            platform_tcb_status: 0,
            verdict_hash: B256::repeat_byte(12),
            node_host_authorization_hash: B256::repeat_byte(13),
        }
    }

    fn attempt() -> PreparedRenewalV1 {
        let full_node_public: [u8; 33] = k256::ecdsa::SigningKey::from_bytes((&[1; 32]).into())
            .unwrap()
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap();
        let intent_value = RegistrationIntentV1 {
            chain_id: [1; 32],
            genesis_hash: B256::repeat_byte(2),
            operation: AttestationOperationV1::RenewEnclave,
            attestation_mode: AttestationMode::DcapRequired,
            policy_hash: B256::repeat_byte(6),
            node_id: NodeIdV1 {
                reth_p2p_public: full_node_public,
            },
            enclave_id: B256::repeat_byte(2),
            binding_id: B256::repeat_byte(3),
            binding_version: 1,
            registration_version: 1,
            renewal_nonce: 1,
            transition_nonce: 0,
            requested_valid_until: 250,
            recipient_x25519: [7; 32],
            attestation_ed25519: [8; 32],
            noise_responder_x25519: [9; 32],
            node_host_authorization_hash: B256::repeat_byte(13),
        };
        let intent = intent_value.encode_canonical().unwrap();
        let evidence_value = AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
            intent: intent_value.clone(),
            quote: vec![1],
            components: [
                DcapCollateralKind::PckCertificateChain,
                DcapCollateralKind::PckCrl,
                DcapCollateralKind::PckCrlIssuerChain,
                DcapCollateralKind::RootCaCrl,
                DcapCollateralKind::TcbInfo,
                DcapCollateralKind::TcbInfoIssuerChain,
                DcapCollateralKind::QeIdentity,
                DcapCollateralKind::QeIdentityIssuerChain,
            ]
            .into_iter()
            .map(|kind| DcapCollateralComponentV1 {
                kind,
                bytes: vec![kind as u8],
            })
            .collect(),
            transition_key_ready_proof: None,
        });
        let evidence = evidence_value.encode_canonical().unwrap();
        let evidence_hash = dcap_evidence_hash_v1(&evidence).unwrap();
        let calldata = vec![3, 4];
        let raw_transaction = vec![5, 6];
        PreparedRenewalV1 {
            source: binding(),
            intent_hash: intent_value.intent_hash().unwrap(),
            intent,
            evidence_hash,
            evidence,
            node_signature: vec![9; 65],
            enclave_signature: vec![10; 64],
            calldata_hash: keccak256(&calldata),
            calldata,
            requested_valid_until: 250,
            collateral_valid_until: 300,
            collateral_margin: 50,
            relay: Address::repeat_byte(15),
            relay_variants: vec![RawRelayTransactionV1 {
                relay: Address::repeat_byte(15),
                chain_id: 1,
                account_nonce: 2,
                gas_price: U256::from(3),
                gas_limit: 4,
                calldata_hash: keccak256([3, 4]),
                transaction_hash: keccak256(&raw_transaction),
                raw_transaction,
            }],
        }
    }

    #[test]
    fn owner_only_atomic_round_trip_and_generation() {
        let root = tempfile::tempdir().unwrap();
        let guard = RenewalJournalGuard::acquire(root.path()).unwrap();
        let first =
            RenewalJournalSnapshotV1::new(RenewalJournalStateV1::Prepared { attempt: attempt() });
        guard.store(first).unwrap();
        assert_eq!(guard.load().unwrap().unwrap().generation, 1);
        let second = RenewalJournalSnapshotV1::new(RenewalJournalStateV1::Abandoned {
            attempt: attempt(),
            abandoned_at_finalized_height: 5,
            reason: "stale".to_owned(),
        });
        guard.store(second).unwrap();
        assert_eq!(guard.load().unwrap().unwrap().generation, 2);
        let metadata = fs::metadata(root.path().join(DIRECTORY).join(JOURNAL)).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, FILE_MODE);
    }

    #[test]
    fn duplicate_owner_is_rejected_and_read_only_inspection_does_not_mutate() {
        let root = tempfile::tempdir().unwrap();
        let guard = RenewalJournalGuard::acquire(root.path()).unwrap();
        guard
            .store(RenewalJournalSnapshotV1::new(
                RenewalJournalStateV1::Prepared { attempt: attempt() },
            ))
            .unwrap();
        assert!(RenewalJournalGuard::acquire(root.path()).is_err());
        let inspected = inspect_journal(root.path()).unwrap().unwrap();
        assert_eq!(inspected.lifecycle.label(), "prepared");
    }

    #[test]
    fn torn_scratch_is_discarded_but_corrupt_committed_state_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        {
            let guard = RenewalJournalGuard::acquire(root.path()).unwrap();
            guard
                .store(RenewalJournalSnapshotV1::new(
                    RenewalJournalStateV1::Prepared { attempt: attempt() },
                ))
                .unwrap();
        }
        let directory = root.path().join(DIRECTORY);
        fs::write(directory.join(NEXT), b"partial").unwrap();
        fs::set_permissions(directory.join(NEXT), fs::Permissions::from_mode(FILE_MODE)).unwrap();
        let guard = RenewalJournalGuard::acquire(root.path()).unwrap();
        assert!(!directory.join(NEXT).exists());
        drop(guard);
        fs::write(directory.join(JOURNAL), b"corrupt").unwrap();
        assert!(inspect_journal(root.path()).is_err());
    }
}
