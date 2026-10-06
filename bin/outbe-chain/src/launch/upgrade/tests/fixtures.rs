use super::*;
use outbe_node::tee_remote_session::{
    LocalFinalizedSuccessorStatusV1, LocalRegistryAdmissionError,
};
use outbe_primitives::tee_attestation_v1::{
    AttestationMode, EnclaveInitializationManifestV1, NodeIdV1,
};
use outbe_tee::FinalizedRegistryViewV1;
use std::cell::RefCell;
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Inspect,
    Manifest,
    Status,
    Authorize,
    Finalize(u64, B256),
    Promote,
    Promoted,
    Missed(u64, u64),
}

pub(super) struct RecordingIo {
    pub(super) journal:
        RefCell<VecDeque<eyre::Result<Option<outbe_operator::tee::UpgradeJournalSnapshotV1>>>>,
    pub(super) manifest: EnclaveInitializationManifestV1,
    pub(super) status:
        RefCell<VecDeque<Result<LocalFinalizedSuccessorStatusV1, LocalRegistryAdmissionError>>>,
    pub(super) authorization:
        RefCell<Option<Result<FinalizedRegistryViewV1, LocalRegistryAdmissionError>>>,
    pub(super) fail: Option<Action>,
    pub(super) actions: RefCell<Vec<Action>>,
}

impl RecordingIo {
    pub(super) fn new() -> eyre::Result<Self> {
        let key = k256::ecdsa::SigningKey::from_bytes((&[0x41; 32]).into())?;
        Ok(Self {
            journal: RefCell::new(VecDeque::new()),
            manifest: EnclaveInitializationManifestV1 {
                chain_id: U256::from(676).to_be_bytes(),
                genesis_hash: B256::repeat_byte(1),
                attestation_mode: AttestationMode::DcapRequired,
                node_id: NodeIdV1 {
                    reth_p2p_public: key
                        .verifying_key()
                        .to_encoded_point(true)
                        .as_bytes()
                        .try_into()?,
                },
                initialization_challenge: [2; 32],
                node_host_noise_x25519: [3; 32],
                recipient_x25519: [4; 32],
                attestation_ed25519: [5; 32],
                noise_responder_x25519: [6; 32],
            },
            status: RefCell::new(VecDeque::new()),
            authorization: RefCell::new(None),
            fail: None,
            actions: RefCell::new(Vec::new()),
        })
    }

    pub(super) fn checkpoint(&self, state: UpgradeJournalStateV1) {
        self.journal.borrow_mut().push_back(Ok(Some(
            outbe_operator::tee::UpgradeJournalSnapshotV1::new(state),
        )));
    }

    fn perform(&self, action: Action) -> eyre::Result<()> {
        self.actions.borrow_mut().push(action);
        eyre::ensure!(self.fail != Some(action), "injected host operation failure");
        Ok(())
    }
}

impl UpgradePromotionIo for RecordingIo {
    type Authorization = FinalizedRegistryViewV1;

    fn inspect_journal(
        &self,
    ) -> eyre::Result<Option<outbe_operator::tee::UpgradeJournalSnapshotV1>> {
        self.perform(Action::Inspect)?;
        self.journal
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Err(eyre::eyre!("end of recorded cycles")))
    }

    fn committed_manifest(&self) -> eyre::Result<EnclaveInitializationManifestV1> {
        self.perform(Action::Manifest)?;
        Ok(self.manifest.clone())
    }

    fn successor_status(
        &self,
    ) -> Result<LocalFinalizedSuccessorStatusV1, LocalRegistryAdmissionError> {
        self.perform(Action::Status)
            .map_err(|e| LocalRegistryAdmissionError::Provider(e.to_string()))?;
        self.status.borrow_mut().pop_front().ok_or_else(|| {
            LocalRegistryAdmissionError::Provider("unexpected finalized status read".into())
        })?
    }

    fn replacement_authorization(
        &self,
        node_id: &NodeIdV1,
    ) -> Result<ports::PromotionAuthorization<Self::Authorization>, LocalRegistryAdmissionError>
    {
        self.perform(Action::Authorize)
            .map_err(|e| LocalRegistryAdmissionError::Provider(e.to_string()))?;
        assert_eq!(node_id, &self.manifest.node_id);
        let view = self.authorization.borrow_mut().take().ok_or_else(|| {
            LocalRegistryAdmissionError::Provider(
                "unexpected replacement authorization read".into(),
            )
        })??;
        Ok(ports::PromotionAuthorization {
            authorization: view,
            view,
        })
    }

    fn promote_candidate(&self, authorization: &Self::Authorization) -> eyre::Result<()> {
        assert_eq!(*authorization, finalized_view(105));
        self.perform(Action::Promote)
    }

    fn record_finalized(&self, height: u64, hash: B256) -> eyre::Result<()> {
        self.perform(Action::Finalize(height, hash))
    }
    fn record_promoted(&self) -> eyre::Result<()> {
        self.perform(Action::Promoted)
    }
    fn record_missed_cutoff(&self, height: u64, activation_height: u64) -> eyre::Result<()> {
        self.perform(Action::Missed(height, activation_height))
    }
}

pub(super) fn config() -> UpgradePromotionWorkerConfigV1 {
    UpgradePromotionWorkerConfigV1 {
        chain_id: 676,
        genesis_hash: B256::repeat_byte(1),
        node_data_dir: PathBuf::from("node"),
        poll_secs: 0,
        warning_blocks: 10,
        critical_blocks: 5,
        promoted: Arc::new(tokio::sync::Notify::new()),
    }
}

pub(super) fn context() -> outbe_operator::tee::UpgradeContextV1 {
    outbe_operator::tee::UpgradeContextV1 {
        predecessor_manifest_hash: B256::repeat_byte(10),
        candidate_manifest_hash: B256::repeat_byte(11),
        successor_policy_hash: B256::repeat_byte(12),
        activation_height: 100,
        active_tee_dir: PathBuf::from("active"),
        candidate_tee_dir: PathBuf::from("candidate"),
    }
}

pub(super) fn finalized_view(height: u64) -> FinalizedRegistryViewV1 {
    FinalizedRegistryViewV1 {
        chain_id: U256::from(676).to_be_bytes(),
        genesis_hash: B256::repeat_byte(1),
        block_number: height,
        block_hash: B256::repeat_byte(13),
        state_root: B256::repeat_byte(14),
        consensus_timestamp: 200,
    }
}

pub(super) fn status(height: u64) -> LocalFinalizedSuccessorStatusV1 {
    LocalFinalizedSuccessorStatusV1 {
        view: finalized_view(height),
        staged_proposal_id: None,
        staged_policy: None,
        strict_upgrade: Default::default(),
        retirement_height: 0,
    }
}

pub(super) fn submission() -> outbe_operator::tee::PreparedUpgradeSubmissionV1 {
    outbe_operator::tee::PreparedUpgradeSubmissionV1 {
        intent_hash: B256::repeat_byte(20),
        evidence_hash: B256::repeat_byte(21),
        calldata_hash: B256::repeat_byte(22),
        relay: alloy_primitives::Address::repeat_byte(23),
        relay_variants: Vec::new(),
    }
}

pub(super) fn security() -> outbe_operator::tee::UpgradeSecurityMaterialV1 {
    outbe_operator::tee::UpgradeSecurityMaterialV1 {
        sealed_root_hash: B256::repeat_byte(24),
        resident_offer_public: B256::repeat_byte(25),
        proof_hash: B256::repeat_byte(26),
    }
}

pub(super) fn finalized(context: outbe_operator::tee::UpgradeContextV1) -> UpgradeJournalStateV1 {
    UpgradeJournalStateV1::Finalized {
        context,
        security: security(),
        submission: submission(),
        finalized_height: 95,
        finalized_hash: B256::repeat_byte(27),
    }
}

pub(super) fn submitted() -> eyre::Result<RecordingIo> {
    let io = RecordingIo::new()?;
    io.checkpoint(UpgradeJournalStateV1::Submitted {
        context: context(),
        security: security(),
        submission: submission(),
        submitted_at_finalized_height: 90,
        transaction_hashes: vec![B256::repeat_byte(28)],
    });
    io.status.borrow_mut().push_back(Ok(status(105)));
    *io.authorization.borrow_mut() = Some(Ok(finalized_view(105)));
    Ok(io)
}
