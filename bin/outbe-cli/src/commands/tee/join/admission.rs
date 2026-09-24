use super::super::admission_history;
use super::super::json_b256_field;
use super::super::json_hex_array;
use super::super::json_hex_u256_field;
use super::super::json_hex_u64_field;
use super::super::sign_node_hash;
use super::classify_join_offer_key_state_transport;

use super::JoinEnclave;
use super::JoinOfferKeyState;

use crate::rpc::Rpc;
use alloy_consensus::BlockHeader as _;

use alloy_primitives::B256;

use eyre::Result;
use eyre::WrapErr;

use outbe_operator::tee::FinalizedRegistryChainViewV1;

use outbe_operator::tee::RenewalBindingV1;

use outbe_primitives::reshare_artifact::decode_outbe_block_artifacts;
use outbe_primitives::reshare_artifact::ConsensusHeaderArtifact;

use outbe_primitives::tee_attestation_v1::TeePolicyScheduleV1;

use outbe_tee::connect_committed_node_host_enclave;

use outbe_tee::finalized_admission::onboarding_registry_slots_v1;
use outbe_tee::finalized_admission::FinalizedAdmissionRecordKindV1;
use outbe_tee::finalized_admission::FinalizedAdmissionWitnessV1;
use outbe_tee::finalized_admission::MptAccountProofV1;
use outbe_tee::finalized_admission::MptStorageProofV1;

use outbe_tee::persist_finalized_join_admission_anchor;

use outbe_tee::protocol::EnclaveRequest;

use outbe_tee::FinalizedJoinAdmissionAnchorV1;
use outbe_tee::FinalizedRegistryViewV1;

use outbe_tee::NodeHostIdentityV1;

use outbe_tee::TransportError;
use std::fs;
use std::path::Path;

pub(in super::super) enum FinalizedAdmissionAttemptErrorV1 {
    Transport(TransportError),
    Local(eyre::Report),
}

impl From<TransportError> for FinalizedAdmissionAttemptErrorV1 {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

impl From<eyre::Report> for FinalizedAdmissionAttemptErrorV1 {
    fn from(error: eyre::Report) -> Self {
        Self::Local(error)
    }
}

pub(in super::super) trait FinalizedAdmissionRecoveryIoV1 {
    fn upload_once(
        &mut self,
    ) -> impl std::future::Future<Output = std::result::Result<[u8; 32], FinalizedAdmissionAttemptErrorV1>>;

    fn reconnect_exact(&mut self) -> std::result::Result<(), TransportError>;

    fn probe_offer_key(&mut self) -> std::result::Result<JoinOfferKeyState, TransportError>;

    fn expected_offer_key(&self) -> [u8; 32];
}

pub(in super::super) async fn run_finalized_admission_recovery_v1(
    io: &mut impl FinalizedAdmissionRecoveryIoV1,
) -> Result<[u8; 32]> {
    let mut uploads = 0_u8;
    loop {
        uploads += 1;
        match io.upload_once().await {
            Ok(offer_public) => return Ok(offer_public),
            Err(FinalizedAdmissionAttemptErrorV1::Local(error)) => return Err(error),
            Err(FinalizedAdmissionAttemptErrorV1::Transport(error))
                if !error.is_connection_fault() =>
            {
                return Err(eyre::eyre!(error));
            }
            Err(FinalizedAdmissionAttemptErrorV1::Transport(error)) => {
                io.reconnect_exact().map_err(|reconnect| {
                    eyre::eyre!(
                        "finalized-admission connection fault ({error}); exact reconnect failed: {reconnect}"
                    )
                })?;
                match io.probe_offer_key().map_err(|probe| {
                    eyre::eyre!(
                        "finalized-admission connection fault ({error}); authenticated recovery probe failed: {probe}"
                    )
                })? {
                    JoinOfferKeyState::ReadyExact => return Ok(io.expected_offer_key()),
                    JoinOfferKeyState::ReadyMismatch => {
                        eyre::bail!(
                            "resident permanent offer key does not match finalized TeeRegistry after reconnect"
                        );
                    }
                    JoinOfferKeyState::Keyless if uploads == 1 => continue,
                    JoinOfferKeyState::Keyless => {
                        eyre::bail!(
                            "finalized-admission replay exhausted after a second connection fault; authenticated enclave remains keyless"
                        );
                    }
                }
            }
        }
    }
}

pub(in super::super) struct CliFinalizedAdmissionIoV1<'a, R> {
    pub(in super::super) rpc: &'a R,
    pub(in super::super) finalized_height: u64,
    pub(in super::super) context: &'a outbe_tee::dcap_protocol::DcapOnboardingContextV1,
    pub(in super::super) artifact: &'a [u8],
    pub(in super::super) anchor_outcome: &'a [u8],
    pub(in super::super) expected_intent_hash: B256,
    pub(in super::super) expected_offer_public: [u8; 32],
    pub(in super::super) expected_key_epoch: u64,
    pub(in super::super) expected_offer_epoch: u64,
    pub(in super::super) enclave: &'a mut JoinEnclave,
    pub(in super::super) enclave_socket: &'a str,
    pub(in super::super) node_data_dir: &'a Path,
    pub(in super::super) node_host_identity: NodeHostIdentityV1,
    pub(in super::super) node_signing_key: &'a k256::ecdsa::SigningKey,
}

impl<R: Rpc + Sync> FinalizedAdmissionRecoveryIoV1 for CliFinalizedAdmissionIoV1<'_, R> {
    fn upload_once(
        &mut self,
    ) -> impl std::future::Future<Output = std::result::Result<[u8; 32], FinalizedAdmissionAttemptErrorV1>>
    {
        stream_finalized_admission_attempt_v1(
            self.rpc,
            self.finalized_height,
            self.context,
            self.artifact,
            self.anchor_outcome,
            self.expected_intent_hash,
            self.expected_offer_public,
            self.expected_key_epoch,
            self.expected_offer_epoch,
            self.enclave,
        )
    }

    fn reconnect_exact(&mut self) -> std::result::Result<(), TransportError> {
        let reconnected = match self.enclave {
            JoinEnclave::Committed(_) => JoinEnclave::Committed(
                connect_committed_node_host_enclave(self.enclave_socket, self.node_data_dir)?,
            ),
            JoinEnclave::Candidate(_) => JoinEnclave::Candidate(Box::new(
                outbe_tee::prepare_node_host_enclave_replacement_candidate(
                    self.enclave_socket,
                    self.node_data_dir,
                    self.node_host_identity,
                    |hash| sign_node_hash(self.node_signing_key, hash),
                )?,
            )),
            JoinEnclave::Development(_) => {
                return Err(TransportError::DcapVerification(
                    "finalized admission cannot reconnect a development enclave".into(),
                ));
            }
        };
        *self.enclave = reconnected;
        Ok(())
    }

    fn probe_offer_key(&mut self) -> std::result::Result<JoinOfferKeyState, TransportError> {
        let response = self
            .enclave
            .request_transport(&EnclaveRequest::GetPublicKeys)?;
        classify_join_offer_key_state_transport(response, self.expected_offer_public)
    }

    fn expected_offer_key(&self) -> [u8; 32] {
        self.expected_offer_public
    }
}

pub(in super::super) fn finalized_join_admission_anchor_v1(
    view: &FinalizedRegistryViewV1,
    binding: &RenewalBindingV1,
) -> FinalizedJoinAdmissionAnchorV1 {
    FinalizedJoinAdmissionAnchorV1 {
        chain_id: view.chain_id,
        genesis_hash: view.genesis_hash,
        node_id_hash: binding.node_id_hash,
        enclave_id: binding.enclave_id,
        intent_hash: binding.intent_hash,
        finalized_height: view.block_number,
        finalized_hash: view.block_hash,
        finalized_state_root: view.state_root,
        finalized_consensus_timestamp: view.consensus_timestamp,
    }
}

pub(in super::super) fn persist_authorized_join_admission_anchor_v1(
    node_data_dir: &Path,
    finalized: &FinalizedRegistryChainViewV1,
    node_id_hash: B256,
    enclave_id: B256,
    intent_hash: B256,
) -> Result<FinalizedJoinAdmissionAnchorV1> {
    let binding = finalized
        .binding
        .as_ref()
        .ok_or_else(|| eyre::eyre!("finalized Registry lost the completed join binding"))?;
    if binding.node_id_hash != node_id_hash
        || binding.enclave_id != enclave_id
        || binding.intent_hash != intent_hash
    {
        eyre::bail!("finalized join admission anchor differs from the completed join binding");
    }
    let anchor = finalized_join_admission_anchor_v1(&finalized.view, binding);
    persist_finalized_join_admission_anchor(node_data_dir, anchor)
        .map_err(|error| eyre::eyre!("persist finalized join admission anchor: {error}"))
}

pub(in super::super) async fn load_finalized_admission_anchor_v1(
    rpc: &(impl Rpc + Sync),
    genesis: &Path,
    finalized_height: u64,
    context: &outbe_tee::dcap_protocol::DcapOnboardingContextV1,
) -> Result<Vec<u8>> {
    if finalized_height == 0 {
        eyre::bail!("onboarding admission cannot be proved at genesis");
    }
    let genesis_json: serde_json::Value = serde_json::from_slice(
        &fs::read(genesis)
            .wrap_err_with(|| format!("read exact final genesis: {}", genesis.display()))?,
    )
    .wrap_err("parse exact final genesis")?;
    let policy_schedule = genesis_json
        .pointer("/config/teeAttestationV1/policySchedule")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("final genesis has no TEE policy schedule"))?;
    let policy_schedule = hex::decode(policy_schedule.trim_start_matches("0x"))
        .wrap_err("decode final genesis TEE policy schedule")?;
    let schedule = TeePolicyScheduleV1::decode_canonical(&policy_schedule)
        .map_err(|error| eyre::eyre!("decode exact final genesis TEE policy schedule: {error}"))?;
    if schedule.chain_id != context.chain_id || schedule.genesis_hash != context.genesis_hash {
        eyre::bail!("exact final genesis does not match the onboarding network identity");
    }

    for height in 1..=finalized_height {
        if !matches!(
            admission_history::history_artifact(rpc, height).await?,
            Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) if boundary.epoch == 0
        ) {
            continue;
        }
        let public = rpc
            .outbe_get_finality_proof(height)
            .await
            .wrap_err_with(|| format!("read finalization at height {height}"))?;
        let (block, _) = admission_history::compact_header(&public, height)?;
        let artifacts = decode_outbe_block_artifacts(block.header().extra_data().as_ref())
            .map_err(|error| eyre::eyre!("decode block {height} artifacts: {error:?}"))?;
        if let Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) =
            artifacts.consensus_header_artifact
        {
            if boundary.epoch == 0 {
                return Ok(boundary.outcome.to_vec());
            }
        }
    }

    eyre::bail!("finalized history has no epoch-0 BoundaryOutcome");
}

#[allow(clippy::too_many_arguments)]
async fn stream_finalized_admission_attempt_v1(
    rpc: &(impl Rpc + Sync),
    finalized_height: u64,
    context: &outbe_tee::dcap_protocol::DcapOnboardingContextV1,
    artifact: &[u8],
    anchor_outcome: &[u8],
    expected_intent_hash: B256,
    expected_offer_public: [u8; 32],
    expected_key_epoch: u64,
    expected_offer_epoch: u64,
    enclave: &mut JoinEnclave,
) -> std::result::Result<[u8; 32], FinalizedAdmissionAttemptErrorV1> {
    let request_hash = enclave.begin_finalized_admission_v1(
        artifact,
        anchor_outcome,
        expected_intent_hash,
        expected_offer_public,
        expected_key_epoch,
        expected_offer_epoch,
    )?;

    // Capture the exact state proof before scanning history while the head advances.
    let slots = onboarding_registry_slots_v1(context);
    let slot_params = slots
        .iter()
        .map(|slot| format!("{slot:#x}"))
        .collect::<Vec<_>>();
    let (finalized_height, opening) =
        admission_history::registry_opening(rpc, finalized_height, &slot_params).await?;

    let mut next_transition_epoch = 1_u64;
    let mut admission = None;
    for height in 1..=finalized_height {
        let Some(public) = admission_history::admission_public(
            rpc,
            height,
            finalized_height,
            next_transition_epoch,
        )
        .await?
        else {
            continue;
        };
        let (block, compact) = admission_history::compact_header(&public, height)?;
        let artifacts = decode_outbe_block_artifacts(block.header().extra_data().as_ref())
            .map_err(|error| eyre::eyre!("decode block {height} artifacts: {error:?}"))?;
        if let Some(ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, .. }) =
            artifacts.consensus_header_artifact
        {
            if height < finalized_height && epoch == next_transition_epoch {
                let transition = compact
                    .clone()
                    .encode_canonical()
                    .map_err(|error| eyre::eyre!("encode committee transition: {error}"))?;
                enclave.upload_finalized_admission_record_v1(
                    request_hash,
                    FinalizedAdmissionRecordKindV1::CommitteeTransition,
                    &transition,
                )?;
                next_transition_epoch = next_transition_epoch
                    .checked_add(1)
                    .ok_or_else(|| eyre::eyre!("committee transition epoch overflow"))?;
            }
        }
        if height == finalized_height {
            admission = Some(compact);
        }
    }

    let registry_account = MptAccountProofV1 {
        nonce: json_hex_u64_field(&opening, "nonce")?,
        balance: json_hex_u256_field(&opening, "balance")?,
        code_hash: json_b256_field(&opening, "codeHash")?,
        storage_root: json_b256_field(&opening, "storageHash")?,
        nodes: json_hex_array(&opening, "accountProof")?,
    };
    let storage = opening
        .get("storageProof")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| eyre::eyre!("eth_getProof has no storageProof array"))?;
    let mut registry_storage = Vec::with_capacity(storage.len());
    for item in storage {
        registry_storage.push(MptStorageProofV1 {
            key: json_b256_field(item, "key")?,
            value: json_hex_u256_field(item, "value")?,
            nodes: json_hex_array(item, "proof")?,
        });
    }
    if registry_storage.len() != slots.len() {
        return Err(eyre::eyre!("eth_getProof omitted a required TeeRegistry slot").into());
    }
    let admission_witness = FinalizedAdmissionWitnessV1 {
        admission: admission.expect("positive finalized height sets admission proof"),
        registry_account,
        registry_storage,
    }
    .encode_canonical()
    .map_err(|error| eyre::eyre!("encode finalized onboarding admission witness: {error}"))?;
    enclave.upload_finalized_admission_record_v1(
        request_hash,
        FinalizedAdmissionRecordKindV1::Admission,
        &admission_witness,
    )?;
    enclave
        .finish_finalized_admission_v1(request_hash, expected_offer_public)
        .map_err(Into::into)
}
