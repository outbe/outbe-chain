use super::super::CliFinalityRpc;
use super::{
    finalized_binding_matches_intent, load_finalized_admission_anchor_v1,
    run_finalized_admission_recovery_v1, CliFinalizedAdmissionIoV1, DurableJoinSubmissionV1,
    JoinEnclave, JoinTransport,
};
use crate::rpc::Rpc;
use alloy_primitives::B256;
use eyre::{Result, WrapErr};
use outbe_operator::tee::{
    await_finalized_onboarding_v1, read_finalized_registry_view_v1, ExpectedOnboardingBindingV1,
    NodeBindingSelectorV1,
};
use outbe_primitives::tee_attestation_v1::{AttestationMode, RegistrationIntentV1, TeePolicyV1};
use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_tee::{
    clear_committed_join_checkpoint, connect_committed_node_host_enclave,
    construct_finalized_replacement_authorization_v1, promote_replacement_candidate,
    FinalizedReplacementBindingV1, NodeHostIdentityV1,
};
use std::path::Path;
use std::time::Duration;

pub(super) struct OnboardingIngestContext<'a> {
    pub(super) binding_selector: &'a NodeBindingSelectorV1,
    pub(super) policy: &'a TeePolicyV1,
    pub(super) node_id_hash: B256,
    pub(super) enclave_id: B256,
    pub(super) intent_hash: B256,
    pub(super) recipient_x25519: [u8; 32],
    pub(super) expected_offer_pub: [u8; 32],
    pub(super) key_epoch: u64,
    pub(super) tribute_offer_epoch: u64,
    pub(super) tx_hash: &'a str,
    pub(super) timeout_secs: u64,
    pub(super) genesis: &'a Path,
    pub(super) authorized_node_data_dir: Option<&'a Path>,
    pub(super) enclave_socket: &'a str,
    pub(super) reth_p2p_public: [u8; 33],
    pub(super) node_signing_key: &'a k256::ecdsa::SigningKey,
}

pub(super) async fn ingest_onboarding_offer_key(
    client: &(impl Rpc + Sync),
    enclave: &mut JoinEnclave,
    ctx: OnboardingIngestContext<'_>,
) -> Result<[u8; 32]> {
    let expected = ExpectedOnboardingBindingV1 {
        selector: ctx.binding_selector.clone(),
        chain_id: ctx.policy.chain_id,
        genesis_hash: ctx.policy.genesis_hash,
        node_id_hash: ctx.node_id_hash,
        enclave_id: ctx.enclave_id,
        intent_hash: ctx.intent_hash,
        recipient_x25519: ctx.recipient_x25519,
        tribute_offer_public: ctx.expected_offer_pub,
        key_epoch: ctx.key_epoch,
        tribute_offer_epoch: ctx.tribute_offer_epoch,
    };
    let finalized = await_finalized_onboarding_v1(
        &CliFinalityRpc(client),
        ctx.tx_hash,
        &expected,
        Duration::from_secs(ctx.timeout_secs),
    )
    .await?;
    let artifact = finalized
        .artifact
        .encode_canonical()
        .map_err(|code| eyre::eyre!("encode finalized artifact: {:#06x}", code.code()))?;
    println!(
        "exact onboarding finalized at height {} (artifact {} bytes)",
        finalized.finalized_height,
        artifact.len()
    );
    match ctx.policy.attestation_mode {
        AttestationMode::DcapRequired => {
            let anchor_outcome = load_finalized_admission_anchor_v1(
                client,
                ctx.genesis,
                finalized.finalized_height,
                &finalized.artifact.context,
            )
            .await?;
            let node_data_dir = ctx
                .authorized_node_data_dir
                .expect("DcapRequired join has a durable NodeHost admission anchor");
            let mut recovery = CliFinalizedAdmissionIoV1 {
                rpc: client,
                finalized_height: finalized.finalized_height,
                context: &finalized.artifact.context,
                artifact: &artifact,
                anchor_outcome: &anchor_outcome,
                expected_intent_hash: ctx.intent_hash,
                expected_offer_public: ctx.expected_offer_pub,
                expected_key_epoch: ctx.key_epoch,
                expected_offer_epoch: ctx.tribute_offer_epoch,
                enclave,
                enclave_socket: ctx.enclave_socket,
                node_data_dir,
                node_host_identity: NodeHostIdentityV1 {
                    network_binding: ctx.policy.network_binding(),
                    reth_p2p_public: ctx.reth_p2p_public,
                },
                node_signing_key: ctx.node_signing_key,
            };
            run_finalized_admission_recovery_v1(&mut recovery)
                .await
                .wrap_err("enclave purpose-bound onboarding ingest failed")
        }
        AttestationMode::GramineDirectDev => {
            ingest_gramine_direct_dev_artifact(enclave, artifact, &ctx)
        }
    }
}

fn ingest_gramine_direct_dev_artifact(
    enclave: &mut JoinEnclave,
    artifact: Vec<u8>,
    ctx: &OnboardingIngestContext<'_>,
) -> Result<[u8; 32]> {
    if ctx.authorized_node_data_dir.is_none() {
        eyre::bail!(
            "GramineDirectDev post-bootstrap onboarding requires authenticated NodeHost state"
        );
    }
    match enclave.request(&EnclaveRequest::IngestGramineDirectDevOnboardingArtifactV1 {
        artifact,
        expected_intent_hash: ctx.intent_hash,
        expected_tribute_offer_public: ctx.expected_offer_pub,
        expected_key_epoch: ctx.key_epoch,
        expected_tribute_offer_epoch: ctx.tribute_offer_epoch,
    })? {
        EnclaveResponse::GramineDirectDevOnboardingArtifactIngestedV1 {
            tribute_offer_public,
        } if tribute_offer_public == ctx.expected_offer_pub => Ok(tribute_offer_public),
        other => {
            eyre::bail!("GramineDirectDev onboarding returned an unexpected response: {other:?}");
        }
    }
}

pub(super) struct JoinFinalizationContext<'a> {
    pub(super) join_transport: JoinTransport,
    pub(super) authorized_node_data_dir: Option<&'a Path>,
    pub(super) promote_candidate: bool,
    pub(super) binding_selector: &'a NodeBindingSelectorV1,
    pub(super) intent: &'a RegistrationIntentV1,
    pub(super) intent_hash: B256,
    pub(super) enclave_socket: &'a str,
    pub(super) expected_offer_pub: [u8; 32],
    pub(super) tribute_offer_public: [u8; 32],
    pub(super) durable_submission: Option<&'a DurableJoinSubmissionV1>,
}

pub(super) async fn finalize_and_reopen_join(
    client: &(impl Rpc + Sync),
    enclave: JoinEnclave,
    ctx: JoinFinalizationContext<'_>,
) -> Result<()> {
    if ctx.join_transport == JoinTransport::AuthorizedNodeHost {
        let node_data_dir = ctx
            .authorized_node_data_dir
            .expect("authorized NodeHost data directory was validated before key activation");
        let promotion = if ctx.promote_candidate {
            Some(
                load_candidate_promotion_binding(client, ctx.binding_selector, ctx.intent).await?,
            )
        } else {
            None
        };
        drop(enclave);
        if let Some(finalized_binding) = promotion {
            let authorization = construct_finalized_replacement_authorization_v1(
                node_data_dir,
                &finalized_binding,
            )
            .map_err(|error| eyre::eyre!("authorize finalized candidate promotion: {error}"))?;
            promote_replacement_candidate(node_data_dir, &authorization)
                .map_err(|error| eyre::eyre!("promote finalized rejoin candidate: {error}"))?;
        }
        verify_reopened_committed_enclave(ctx.enclave_socket, node_data_dir, ctx.expected_offer_pub)?;
        if ctx
            .durable_submission
            .is_some_and(DurableJoinSubmissionV1::is_committed)
        {
            clear_committed_join_checkpoint(node_data_dir, ctx.intent_hash).map_err(|error| {
                eyre::eyre!("clear completed committed join checkpoint: {error}")
            })?;
        }
        println!(
            "[OK] offer key durably installed and the authenticated enclave connection reopened (offer_public 0x{}). \
             You can now start outbe-chain node.",
            hex::encode(ctx.tribute_offer_public)
        );
    } else {
        drop(enclave);
        println!(
            "[OK] development offer key installed in enclave (offer_public 0x{}). \
             You can now start the separate GramineDirectDev node.",
            hex::encode(ctx.tribute_offer_public)
        );
    }
    Ok(())
}

async fn load_candidate_promotion_binding(
    client: &(impl Rpc + Sync),
    binding_selector: &NodeBindingSelectorV1,
    intent: &RegistrationIntentV1,
) -> Result<FinalizedReplacementBindingV1> {
    let exact = read_finalized_registry_view_v1(&CliFinalityRpc(client), binding_selector)
        .await
        .wrap_err("read finalized candidate promotion binding")?;
    let binding = exact
        .binding
        .ok_or_else(|| eyre::eyre!("finalized Registry lost the candidate binding"))?;
    if !finalized_binding_matches_intent(&binding, intent)? {
        eyre::bail!("finalized Registry binding differs from the durable candidate intent");
    }
    Ok(FinalizedReplacementBindingV1 {
        view: exact.view,
        node_id_hash: binding.node_id_hash,
        enclave_id: binding.enclave_id,
        binding_id: binding.binding_id,
        intent_hash: binding.intent_hash,
        binding_version: binding.binding_version,
        registration_version: binding.registration_version,
        valid_until: binding.valid_until,
        recipient_x25519: binding.recipient_x25519.into(),
        attestation_ed25519: binding.attestation_ed25519.into(),
        noise_responder_x25519: binding.noise_responder_x25519.into(),
        node_host_authorization_hash: binding.node_host_authorization_hash,
    })
}

fn verify_reopened_committed_enclave(
    enclave_socket: &str,
    node_data_dir: &Path,
    expected_offer_pub: [u8; 32],
) -> Result<()> {
    let mut reopened = connect_committed_node_host_enclave(enclave_socket, node_data_dir)
        .map_err(|error| eyre::eyre!("reopen committed enclave: {error}"))?;
    match reopened.request(&EnclaveRequest::GetPublicKeys)? {
        EnclaveResponse::PublicKeys {
            offer_key_ready: true,
            recipient_x25519_pub,
            ..
        } if recipient_x25519_pub == expected_offer_pub => Ok(()),
        other => Err(eyre::eyre!(
            "durable onboarding reopen did not expose the exact permanent offer key: {other:?}"
        )),
    }
}
