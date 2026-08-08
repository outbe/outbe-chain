//! Node-owned assembly of the existing OCOMP Supervisor compute path.
//!
//! Canonical-chain observation remains with `outbe-chain`. This module owns
//! only local capabilities: the Worker endpoint, export adoption, deterministic
//! runner, immutable local result store and (Validator-only) vote submission.

use std::{
    fs::DirBuilder,
    os::unix::fs::DirBuilderExt as _,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, MutexGuard,
    },
    thread,
    time::Duration,
};

use alloy_primitives::B256;
use outbe_node::ocomp::local_result::{
    CommittedLocalLysisResultV1, LoadedLocalLysisResultV1, LocalLysisResultStore,
};
use outbe_ocomp_protocol::{
    capacity::OCOMP_POC_CAS_QUOTA_BYTES, local_control::EndpointIdentity, result::LysisResultV1,
    SchemaLimits,
};
use outbe_primitives::signer::OutbeEvmSigner;
use thiserror::Error;

use crate::{
    bundle::PinnedProtocolBundle,
    cas::CasLimits,
    control::effective_uid,
    inbox::WorkerInboxLimits,
    result_attestation::LocalResultVoteAttesterV1,
    result_signer::OcompSigner,
    sign_once::SignOnceStore,
    supervisor::DiscoveryRecord,
    supervisor_export::{
        SupervisorExportAdoption, SupervisorExportAdoptionConfig, SupervisorExportAdoptionOutcome,
    },
    supervisor_job::{
        CompletedSupervisorJobV1, SupervisorJobFailureClassV1, SupervisorJobRunnerConfigV1,
        SupervisorJobRunnerV1,
    },
    vote_submitter::{
        LocalVoteTransactionPreparerV1, PublicVoteRpcClientV1, SupervisorVoteSubmitterV1,
        VoteSubmissionConfigV1, VoteSubmissionOutcomeV1,
    },
    worker_transport::SupervisorWorkerServerV1,
};

const CAS_MAX_OBJECT_BYTES: u64 = 1_048_576;
const WORKER_INBOX_MAX_ARTIFACT_BYTES: u64 = 1_048_576;
const WORKER_INBOX_MAX_TOTAL_BYTES: u64 = 67_108_864;
const RPC_MAX_RESPONSE_BYTES: usize = 33_554_432;
const RETRY_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbeddedNodePolicyV1 {
    Validator,
    FullNode,
}

#[derive(Clone, Debug)]
pub struct EmbeddedOcompDomainConfigV1 {
    pub domain_root: PathBuf,
    pub worker_address: std::net::SocketAddr,
    pub identity: EndpointIdentity,
    pub registry_generation: u64,
    pub protocol_bundle: PinnedProtocolBundle,
    pub policy: EmbeddedNodePolicyV1,
    /// Required only for Validator policy. FullNode never opens this URL for
    /// vote submission and never loads either signing key.
    pub validator_rpc_url: Option<String>,
    pub limits: SchemaLimits,
}

#[derive(Clone, Debug)]
struct EmbeddedOcompLayoutV1 {
    supervisor_root: PathBuf,
    exporter_root: PathBuf,
    node_root: PathBuf,
    cas_root: PathBuf,
    worker_inbox_root: PathBuf,
    input_ref_root: PathBuf,
    receipt_root: PathBuf,
    binding_root: PathBuf,
    job_root: PathBuf,
    local_result_root: PathBuf,
    vote_submission_root: PathBuf,
    sign_once_root: PathBuf,
    evm_key_path: PathBuf,
    result_key_path: PathBuf,
    checkpoint_root: PathBuf,
    fatal_evidence_root: PathBuf,
}

impl EmbeddedOcompLayoutV1 {
    fn from_root(root: &Path) -> Result<Self, EmbeddedOcompRuntimeErrorV1> {
        if !root.is_absolute() || root.parent().is_none() || root == Path::new("/") {
            return Err(EmbeddedOcompRuntimeErrorV1::InvalidDomainRoot);
        }
        let supervisor = root.join("supervisor-v1");
        let exporter = root.join("exporter-v1");
        let node = root.join("node-v1");
        Ok(Self {
            supervisor_root: supervisor.clone(),
            exporter_root: exporter.clone(),
            node_root: node.clone(),
            cas_root: root.join("cas-v1"),
            worker_inbox_root: root.join("worker-inbox-v1"),
            input_ref_root: exporter.join("input-refs"),
            receipt_root: exporter.join("receipts"),
            binding_root: supervisor.join("export-bindings"),
            job_root: supervisor.join("jobs"),
            local_result_root: node.join("local-results"),
            vote_submission_root: supervisor.join("vote-submissions"),
            sign_once_root: supervisor.join("sign-once"),
            evm_key_path: root.join("ocomp-evm-key.hex"),
            result_key_path: root.join("ocomp-key-v1.hex"),
            checkpoint_root: node.join("exex-checkpoint"),
            fatal_evidence_root: node.join("fatal-evidence"),
        })
    }
}

struct ValidatorVotePolicyV1 {
    preparer: Arc<LocalVoteTransactionPreparerV1>,
    submission_gate: Arc<ValidatorVoteSubmissionGateV1>,
    ocomp_key_hash: B256,
    rpc_url: String,
    journal_root: PathBuf,
    chain_id: u64,
    sender_address: alloy_primitives::Address,
    limits: SchemaLimits,
}

#[derive(Default)]
struct ValidatorVoteSubmissionGateV1(Mutex<()>);

impl ValidatorVoteSubmissionGateV1 {
    fn acquire(&self) -> Result<MutexGuard<'_, ()>, EmbeddedOcompRuntimeErrorV1> {
        self.0
            .lock()
            .map_err(|_| EmbeddedOcompRuntimeErrorV1::Stage {
                name: "lock Validator OCOMP vote submission",
                detail: "vote submission gate is poisoned".to_owned(),
            })
    }
}

pub struct EmbeddedOcompDomainV1 {
    _worker_server: SupervisorWorkerServerV1,
    runner: Arc<SupervisorJobRunnerV1>,
    adoption: SupervisorExportAdoptionConfig,
    local_results: Arc<LocalLysisResultStore>,
    validator_vote: Option<ValidatorVotePolicyV1>,
    checkpoint_root: PathBuf,
    fatal_evidence_root: PathBuf,
}

impl EmbeddedOcompDomainV1 {
    pub fn open(config: EmbeddedOcompDomainConfigV1) -> Result<Self, EmbeddedOcompRuntimeErrorV1> {
        if config.registry_generation == 0
            || !config.worker_address.ip().is_loopback()
            || config.worker_address.port() == 0
        {
            return Err(EmbeddedOcompRuntimeErrorV1::InvalidConfig);
        }
        let layout = EmbeddedOcompLayoutV1::from_root(&config.domain_root)?;
        for private_root in [
            &layout.node_root,
            &layout.supervisor_root,
            &layout.exporter_root,
        ] {
            let mut root_builder = DirBuilder::new();
            root_builder
                .recursive(true)
                .mode(0o700)
                .create(private_root)
                .map_err(|error| stage("create embedded OCOMP private root", error))?;
        }
        let cas_limits = CasLimits {
            max_object_bytes: CAS_MAX_OBJECT_BYTES,
            max_total_bytes: OCOMP_POC_CAS_QUOTA_BYTES,
        };
        let worker_server = SupervisorWorkerServerV1::start(
            config.worker_address,
            config.identity,
            config.registry_generation,
            config.limits,
        )
        .map_err(|error| stage("start Node-owned OCOMP Worker endpoint", error))?;
        let runner = Arc::new(
            SupervisorJobRunnerV1::open(
                SupervisorJobRunnerConfigV1 {
                    cas_root: layout.cas_root.clone(),
                    cas_limits,
                    input_ref_root: layout.input_ref_root.clone(),
                    job_root: layout.job_root.clone(),
                    worker_inbox_root: layout.worker_inbox_root,
                    worker_inbox_limits: WorkerInboxLimits {
                        max_artifact_bytes: WORKER_INBOX_MAX_ARTIFACT_BYTES,
                        max_total_bytes: WORKER_INBOX_MAX_TOTAL_BYTES,
                    },
                    protocol_bundle: config.protocol_bundle.clone(),
                    limits: config.limits,
                },
                worker_server.dispatcher(),
            )
            .map_err(|error| stage("open embedded OCOMP runner", error))?,
        );
        let adoption = SupervisorExportAdoptionConfig {
            cas_root: layout.cas_root,
            cas_limits,
            input_ref_root: layout.input_ref_root,
            receipt_root: layout.receipt_root,
            binding_root: layout.binding_root,
            protocol_bundle: config.protocol_bundle.clone(),
            limits: config.limits,
        };
        let local_results = Arc::new(
            LocalLysisResultStore::open(&layout.local_result_root, config.limits)
                .map_err(|error| stage("open embedded OCOMP local result store", error))?,
        );
        let validator_vote = match config.policy {
            EmbeddedNodePolicyV1::FullNode => {
                if config.validator_rpc_url.is_some() {
                    return Err(EmbeddedOcompRuntimeErrorV1::FullNodeVoteAuthority);
                }
                None
            }
            EmbeddedNodePolicyV1::Validator => {
                let rpc_url = config
                    .validator_rpc_url
                    .ok_or(EmbeddedOcompRuntimeErrorV1::MissingValidatorRpc)?;
                let owner_uid =
                    effective_uid().map_err(|error| stage("resolve Node effective uid", error))?;
                let evm_signer = OutbeEvmSigner::from_strict_file(&layout.evm_key_path, owner_uid)
                    .map_err(|error| stage("open Validator OCOMP EVM key", error))?;
                let result_signer = OcompSigner::from_file(&layout.result_key_path, owner_uid)
                    .map_err(|error| stage("open Validator OCOMP result key", error))?;
                let sign_once =
                    SignOnceStore::open(layout.sign_once_root, owner_uid, config.limits)
                        .map_err(|error| stage("open Validator OCOMP sign-once store", error))?;
                let attester = LocalResultVoteAttesterV1::new(
                    config.identity,
                    config.protocol_bundle.bundle().fork_id,
                    result_signer,
                    sign_once,
                    config.limits,
                )
                .map_err(|error| stage("open Validator OCOMP result attester", error))?;
                let ocomp_key_hash = attester.ocomp_key_hash();
                let preparer = Arc::new(
                    LocalVoteTransactionPreparerV1::new(
                        evm_signer,
                        attester,
                        config.identity.chain_id,
                        config.limits,
                    )
                    .map_err(|error| stage("open Validator OCOMP vote preparer", error))?,
                );
                Some(ValidatorVotePolicyV1 {
                    sender_address: preparer.sender_address(),
                    preparer,
                    submission_gate: Arc::new(ValidatorVoteSubmissionGateV1::default()),
                    ocomp_key_hash,
                    rpc_url,
                    journal_root: layout.vote_submission_root,
                    chain_id: config.identity.chain_id,
                    limits: config.limits,
                })
            }
        };
        Ok(Self {
            _worker_server: worker_server,
            runner,
            adoption,
            local_results,
            validator_vote,
            checkpoint_root: layout.checkpoint_root,
            fatal_evidence_root: layout.fatal_evidence_root,
        })
    }

    #[must_use]
    pub fn checkpoint_root(&self) -> &Path {
        &self.checkpoint_root
    }

    #[must_use]
    pub fn fatal_evidence_root(&self) -> &Path {
        &self.fatal_evidence_root
    }

    #[must_use]
    pub fn validator_ocomp_key_hash(&self) -> Option<B256> {
        self.validator_vote
            .as_ref()
            .map(|policy| policy.ocomp_key_hash)
    }

    pub fn commit_local_result(
        &self,
        completed: &CompletedSupervisorJobV1,
    ) -> Result<CommittedLocalLysisResultV1, EmbeddedOcompRuntimeErrorV1> {
        self.local_results
            .commit(completed.job_id, &completed.canonical_result)
            .map_err(|error| stage("commit embedded OCOMP local result", error))
    }

    pub fn load_local_result(
        &self,
        job_id: B256,
    ) -> Result<Option<LoadedLocalLysisResultV1>, EmbeddedOcompRuntimeErrorV1> {
        self.local_results
            .load(job_id)
            .map_err(|error| stage("load embedded OCOMP local result", error))
    }

    pub fn verify_exact_canonical_result(
        &self,
        job_id: B256,
        canonical: &LysisResultV1,
    ) -> Result<CommittedLocalLysisResultV1, EmbeddedOcompRuntimeErrorV1> {
        self.local_results
            .verify_exact(job_id, canonical)
            .map_err(|error| stage("verify exact FullNode OCOMP result", error))
    }

    pub fn spawn_compute(
        &self,
        record: DiscoveryRecord,
        cancelled: Arc<AtomicBool>,
        sender: mpsc::Sender<EmbeddedComputeOutcomeV1>,
    ) -> Result<(), EmbeddedOcompRuntimeErrorV1> {
        let runner = Arc::clone(&self.runner);
        let adoption_config = self.adoption.clone();
        let job_id = record.spec.summary.job_id;
        thread::Builder::new()
            .name(format!("ocomp-job-{}", short_job(job_id)))
            .spawn(move || loop {
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                let adoption = match SupervisorExportAdoption::open(adoption_config.clone()) {
                    Ok(adoption) => adoption,
                    Err(error) => {
                        let _ = sender.send(EmbeddedComputeOutcomeV1::Unrecoverable {
                            job_id,
                            detail: error.to_string(),
                        });
                        return;
                    }
                };
                match adoption.try_adopt(&record) {
                    Ok(SupervisorExportAdoptionOutcome::Pending) => {
                        thread::sleep(RETRY_INTERVAL);
                    }
                    Ok(SupervisorExportAdoptionOutcome::Adopted(binding)) => {
                        match runner.run_to_result(&record, &binding, &cancelled) {
                            Ok(completed) => {
                                let _ = sender.send(EmbeddedComputeOutcomeV1::Completed(completed));
                                return;
                            }
                            Err(error)
                                if error.class() == SupervisorJobFailureClassV1::Retryable =>
                            {
                                thread::sleep(RETRY_INTERVAL);
                            }
                            Err(error) => {
                                let _ = sender.send(EmbeddedComputeOutcomeV1::Unrecoverable {
                                    job_id,
                                    detail: error.to_string(),
                                });
                                return;
                            }
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(EmbeddedComputeOutcomeV1::Unrecoverable {
                            job_id,
                            detail: error.to_string(),
                        });
                        return;
                    }
                }
            })
            .map_err(|error| stage("spawn embedded OCOMP compute thread", error))?;
        Ok(())
    }

    pub fn spawn_validator_vote(
        &self,
        record: DiscoveryRecord,
        job_id: B256,
        result_digest: B256,
        canonical_result: Vec<u8>,
        cancelled: Arc<AtomicBool>,
        sender: mpsc::Sender<EmbeddedVoteOutcomeV1>,
    ) -> Result<(), EmbeddedOcompRuntimeErrorV1> {
        let policy = self
            .validator_vote
            .as_ref()
            .ok_or(EmbeddedOcompRuntimeErrorV1::FullNodeVoteAuthority)?;
        let preparer = Arc::clone(&policy.preparer);
        let submission_gate = Arc::clone(&policy.submission_gate);
        let rpc_url = policy.rpc_url.clone();
        let config = VoteSubmissionConfigV1 {
            journal_root: policy.journal_root.join(hex::encode(job_id.as_slice())),
            expected_chain_id: policy.chain_id,
            sender_address: policy.sender_address,
            limits: policy.limits,
        };
        thread::Builder::new()
            .name(format!("ocomp-vote-{}", short_job(job_id)))
            .spawn(move || {
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                let _submission_permit = match submission_gate.acquire() {
                    Ok(permit) => permit,
                    Err(error) => {
                        let _ = sender.send(EmbeddedVoteOutcomeV1::Unrecoverable {
                            job_id,
                            detail: error.to_string(),
                        });
                        return;
                    }
                };
                if cancelled.load(Ordering::Acquire) {
                    return;
                }
                let rpc = match PublicVoteRpcClientV1::new(rpc_url, RPC_MAX_RESPONSE_BYTES) {
                    Ok(rpc) => rpc,
                    Err(error) => {
                        let _ = sender.send(EmbeddedVoteOutcomeV1::Unrecoverable {
                            job_id,
                            detail: error.to_string(),
                        });
                        return;
                    }
                };
                let mut submitter = match SupervisorVoteSubmitterV1::open(config, rpc) {
                    Ok(submitter) => submitter,
                    Err(error) => {
                        let _ = sender.send(EmbeddedVoteOutcomeV1::Unrecoverable {
                            job_id,
                            detail: error.to_string(),
                        });
                        return;
                    }
                };
                while !cancelled.load(Ordering::Acquire) {
                    match submitter.reconcile(
                        preparer.as_ref(),
                        job_id,
                        result_digest,
                        &canonical_result,
                        &record.spec,
                    ) {
                        Ok(VoteSubmissionOutcomeV1::Finalized(inclusion)) => {
                            let _ = sender.send(EmbeddedVoteOutcomeV1::Finalized {
                                job_id,
                                success: inclusion.success,
                            });
                            return;
                        }
                        Ok(_) | Err(_) => thread::sleep(RETRY_INTERVAL),
                    }
                }
            })
            .map_err(|error| stage("spawn embedded OCOMP vote thread", error))?;
        Ok(())
    }
}

pub enum EmbeddedComputeOutcomeV1 {
    Completed(CompletedSupervisorJobV1),
    Unrecoverable { job_id: B256, detail: String },
}

pub enum EmbeddedVoteOutcomeV1 {
    Finalized { job_id: B256, success: bool },
    Unrecoverable { job_id: B256, detail: String },
}

fn short_job(job_id: B256) -> String {
    hex::encode(&job_id.as_slice()[..4])
}

fn stage(name: &'static str, error: impl std::fmt::Display) -> EmbeddedOcompRuntimeErrorV1 {
    EmbeddedOcompRuntimeErrorV1::Stage {
        name,
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{mpsc, Arc},
        thread,
        time::Duration,
    };

    use super::ValidatorVoteSubmissionGateV1;

    #[test]
    fn one_validator_signer_serializes_two_live_job_votes() {
        let gate = Arc::new(ValidatorVoteSubmissionGateV1::default());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let first_gate = Arc::clone(&gate);
        let first_entered = entered_tx.clone();
        let first = thread::spawn(move || {
            let _permit = first_gate.acquire().expect("first signer permit");
            first_entered.send(1).expect("first entered signal");
            release_rx.recv().expect("release first vote");
        });
        assert_eq!(entered_rx.recv().expect("first vote entered"), 1);

        let second_gate = Arc::clone(&gate);
        let second = thread::spawn(move || {
            let _permit = second_gate.acquire().expect("second signer permit");
            entered_tx.send(2).expect("second entered signal");
        });
        assert!(entered_rx.recv_timeout(Duration::from_millis(50)).is_err());

        release_tx.send(()).expect("release first vote");
        assert_eq!(
            entered_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("second vote entered after first finalized"),
            2
        );
        first.join().expect("first vote thread");
        second.join().expect("second vote thread");
    }
}

#[derive(Debug, Error)]
pub enum EmbeddedOcompRuntimeErrorV1 {
    #[error("embedded OCOMP domain root is invalid")]
    InvalidDomainRoot,
    #[error("embedded OCOMP runtime configuration is invalid")]
    InvalidConfig,
    #[error("Validator embedded OCOMP policy requires local RPC")]
    MissingValidatorRpc,
    #[error("FullNode embedded OCOMP policy cannot load or use vote authority")]
    FullNodeVoteAuthority,
    #[error("embedded OCOMP stage `{name}` failed: {detail}")]
    Stage { name: &'static str, detail: String },
}
