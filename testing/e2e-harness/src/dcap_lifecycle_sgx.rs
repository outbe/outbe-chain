//! Focused one-host hardware evidence for the three DCAP lifecycle gaps.
//!
//! This is deliberately not a production network simulator. Four real Gramine
//! SGX validator enclaves establish the permanent offer key. A keyless Validator
//! and FullNode then exercise purpose-bound onboarding and renewal, and one
//! founder exercises the same-MRSIGNER A-to-B root-copy/key-ready boundary.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    net::TcpListener,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use alloy_primitives::{B256, U256};
use alloy_signer::SignerSync as _;
use alloy_signer_local::PrivateKeySigner;
use commonware_codec::Encode as _;
use commonware_cryptography::{bls12381, Signer as _};
use eyre::{bail, ensure, eyre, Result, WrapErr as _};
use k256::ecdsa::{signature::hazmat::PrehashSigner as _, SigningKey};
use outbe_operator::tee::{
    copy_same_platform_sealed_root_and_checkpoint_v1, prepare_upgrade_journal_v1,
    record_candidate_key_ready_v1, UpgradeContextV1, UpgradeJournalStateV1,
};
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, AttestationMode, AttestationOperationV1, DcapCollateralComponentV1,
    DcapCollateralKind, DcapEvidenceV1, EnclaveInitializationManifestV1, EnclaveProfile,
    PlatformTcbStatusSetV1, QvlTcbStatusV1, RegistrationIntentV1, TeeMeasurementRuleV1,
    TeePolicyV1,
};
use outbe_tee::{
    dcap_protocol::DcapVerificationOutcomeV1,
    node_host::{
        connect_or_initialize_full_node_enclave, connect_or_initialize_validator_enclave,
        load_committed_enclave_manifest_v1, prepare_validator_enclave_replacement_candidate,
        FullNodeNodeHostIdentityV1, ValidatorNodeHostIdentityV1,
    },
    protocol::{EnclaveRequest, EnclaveResponse, ParticipantAnnounce},
    tee_dkg::{Ack, DealerBundle, FinalizedLog},
    AuthorizedEnclaveClient, CeremonyCoordinator,
};
use rand_core::{OsRng, RngCore as _};
use serde::Serialize;

const CHAIN_ID: u64 = 54_322_345;
const GENESIS_HASH: B256 = B256::repeat_byte(0x53);
const ACTIVATION_HEIGHT: u64 = 100;
const ADMIT_UNTIL: u64 = 1_000_000;
const VALID_FOR: u64 = 86_400;
const INTEL_ROOT_DER_HASH_HEX: &str =
    "44a0196b2b99f889b8e149e95b807a350e7424964399e885a7cbb8ccfab674d3";
const COMPONENT_FILES: [(DcapCollateralKind, &str); 8] = [
    (
        DcapCollateralKind::PckCertificateChain,
        "pck-certificate-chain.pem0",
    ),
    (DcapCollateralKind::PckCrl, "pck.crl.der"),
    (
        DcapCollateralKind::PckCrlIssuerChain,
        "pck-crl-issuer-chain.pem",
    ),
    (DcapCollateralKind::RootCaCrl, "root-ca.crl.der"),
    (DcapCollateralKind::TcbInfo, "tcb-info.json"),
    (
        DcapCollateralKind::TcbInfoIssuerChain,
        "tcb-info-issuer-chain.pem",
    ),
    (DcapCollateralKind::QeIdentity, "qe-identity.json"),
    (
        DcapCollateralKind::QeIdentityIssuerChain,
        "qe-identity-issuer-chain.pem",
    ),
];

#[derive(clap::Parser, Debug)]
#[command(name = "outbe-dcap-lifecycle-sgx")]
struct Cli {
    /// Current production-shaped enclave binary, built with `native-dcap`.
    #[arg(long, default_value = "target/release/outbe-tee-enclave")]
    enclave_binary: PathBuf,
    /// One explicit SGX signing key shared by A and B.
    #[arg(long)]
    signing_key: Option<PathBuf>,
    /// Keep the private temporary evidence directory after success.
    #[arg(long)]
    keep_work_dir: bool,
}

#[derive(Clone, Debug)]
struct Measurement {
    mrenclave: B256,
    mrsigner: B256,
    isv_prod_id: u16,
    isv_svn: u16,
}

struct SgxSpec {
    name: String,
    endpoint: String,
    tee_dir: PathBuf,
    manifest_base: PathBuf,
    measurement: Measurement,
}

struct RunningSgx {
    child: Child,
    log: PathBuf,
}

impl RunningSgx {
    fn stop(mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill().wrap_err("stop Gramine enclave")?;
        }
        let _ = self.child.wait();
        Ok(())
    }
}

impl Drop for RunningSgx {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EvidenceSummary {
    chain_id: u64,
    backend: &'static str,
    founder_validators: usize,
    validator_onboarding_cases: usize,
    full_nodes: usize,
    validator_onboarding_and_renewal: bool,
    full_node_onboarding_and_renewal: bool,
    same_mrsigner_root_reopen: bool,
    transition_key_ready_proof: bool,
    enclave_qvl_accepted_all_quotes: bool,
    offer_public: String,
    active_mrenclave: String,
    candidate_mrenclave: String,
    mrsigner: String,
}

pub fn main() -> Result<()> {
    use clap::Parser as _;
    run(Cli::parse())
}

fn run(cli: Cli) -> Result<()> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| eyre!("cannot resolve repository root"))?
        .to_path_buf();
    let enclave_binary = fs::canonicalize(repo.join(&cli.enclave_binary))
        .wrap_err("resolve current enclave binary")?;
    ensure!(enclave_binary.is_file(), "enclave binary is not a file");
    let signing_key = cli.signing_key.unwrap_or_else(default_signing_key);
    let signing_key = fs::canonicalize(signing_key).wrap_err("resolve SGX signing key")?;

    let work = tempfile::Builder::new()
        .prefix("outbe-dcap-lifecycle-sgx-")
        .tempdir()
        .wrap_err("create lifecycle SGX work directory")?;
    fs::set_permissions(work.path(), fs::Permissions::from_mode(0o700))?;
    let qvl_dir = prepare_qvl_dir(work.path())?;
    let template = repo.join("bin/outbe-tee-enclave/gramine/outbe-tee-enclave.manifest.template");

    let mut specs = Vec::new();
    for name in [
        "validator-0",
        "validator-1",
        "validator-2",
        "validator-3",
        "validator-joiner",
        "full-node",
        "validator-0-candidate",
    ] {
        specs.push(render_and_sign(
            work.path(),
            &template,
            &enclave_binary,
            &signing_key,
            &qvl_dir,
            name,
        )?);
    }
    let policy = lifecycle_policy(&specs)?;
    let policy_hash = policy
        .policy_hash()
        .map_err(|error| eyre!(error.to_string()))?;
    let policy_bytes = policy
        .encode_canonical()
        .map_err(|error| eyre!(error.to_string()))?;

    let validator_signers = (0..5)
        .map(|_| random_node_signer())
        .collect::<Result<Vec<_>>>()?;
    let validator_bls = (0..5)
        .map(|index| valid_bls_public(100 + index as u64))
        .collect::<Result<Vec<_>>>()?;
    let validator_data = (0..5)
        .map(|index| private_dir(&work.path().join(format!("validator-{index}-data"))))
        .collect::<Result<Vec<_>>>()?;

    let mut founders = Vec::new();
    let mut founder_processes = Vec::new();
    for (index, spec) in specs.iter().take(4).enumerate() {
        let mut process = start_sgx(spec)?;
        wait_for_log(
            &mut process,
            "listening on tcp://",
            Duration::from_secs(180),
        )?;
        let identity = validator_identity(&validator_signers[index], validator_bls[index]);
        let signer = &validator_signers[index];
        let client = connect_or_initialize_validator_enclave(
            &spec.endpoint,
            &validator_data[index],
            identity,
            |hash| node_signature(signer, hash),
        )?;
        founders.push(client);
        founder_processes.push(process);
    }
    let offer_public = establish_offer_key(&mut founders)?;
    for (index, spec) in specs.iter().take(4).enumerate() {
        ensure!(
            spec.tee_dir.join("sealed_root.bin").is_file(),
            "founder {index} did not persist sealed_root.bin"
        );
    }

    // Keep one resident A as the sequential onboarding source and QVL verifier.
    // The other founders have already established their logical DKG state; stop
    // them now so hardware evidence does not manufacture avoidable EPC pressure.
    founders.truncate(1);
    founder_processes.pop().unwrap().stop()?;
    founder_processes.pop().unwrap().stop()?;
    founder_processes.pop().unwrap().stop()?;

    eprintln!("stage: validator onboarding");
    let joiner_index = 4;
    let mut joiner_process = start_sgx(&specs[4])?;
    wait_for_log(
        &mut joiner_process,
        "listening on tcp://",
        Duration::from_secs(180),
    )?;
    let joiner_identity = validator_identity(
        &validator_signers[joiner_index],
        validator_bls[joiner_index],
    );
    let joiner_signer = &validator_signers[joiner_index];
    let mut joiner = connect_or_initialize_validator_enclave(
        &specs[4].endpoint,
        &validator_data[joiner_index],
        joiner_identity,
        |hash| node_signature(joiner_signer, hash),
    )?;
    let joiner_manifest = load_committed_enclave_manifest_v1(&validator_data[joiner_index])?;
    let joiner_binding = random_nonzero_b256()?;
    let now = unix_seconds()?;
    let joiner_register = registration_intent(
        &joiner_manifest,
        AttestationOperationV1::RegisterEnclave,
        policy_hash,
        joiner_binding,
        1,
        1,
        0,
        0,
        now + VALID_FOR,
    )?;
    let joiner_generated = joiner.generate_dcap_quote(&joiner_register)?;

    let collateral_dir = work.path().join("collateral");
    let quote_path = work.path().join("joiner-quote.bin");
    fs::write(&quote_path, &joiner_generated.quote_body)?;
    capture_collateral(&repo, &quote_path, &collateral_dir)?;
    let components = collateral_components(&collateral_dir)?;
    onboard_keyless_enclave(
        &mut founders[0],
        &mut joiner,
        &joiner_register,
        joiner_generated,
        &components,
        &policy_bytes,
        node_signature(joiner_signer, joiner_register.intent_hash().map_err(codec)?)
            .map_err(eyre::Report::msg)?,
        offer_public,
        now,
    )?;
    drop(joiner);
    joiner_process.stop()?;
    let mut joiner_process = start_sgx(&specs[4])?;
    wait_for_log(
        &mut joiner_process,
        "listening on tcp://",
        Duration::from_secs(180),
    )?;
    let mut joiner = connect_or_initialize_validator_enclave(
        &specs[4].endpoint,
        &validator_data[joiner_index],
        joiner_identity,
        |hash| node_signature(joiner_signer, hash),
    )?;
    verify_renewal_quote(
        &mut founders[0],
        &mut joiner,
        &joiner_manifest,
        joiner_signer,
        policy_hash,
        joiner_binding,
        &components,
        &policy_bytes,
        now,
    )?;
    drop(joiner);
    joiner_process.stop()?;

    eprintln!("stage: full-node onboarding");
    let full_node_data = private_dir(&work.path().join("full-node-data"))?;
    let full_node_signer = random_k256_signer()?;
    let full_node_public = compressed_public(&full_node_signer)?;
    let full_node_identity = FullNodeNodeHostIdentityV1 {
        chain_id: CHAIN_ID,
        genesis_hash: GENESIS_HASH,
        reth_p2p_public: full_node_public,
    };
    let mut full_node_process = start_sgx(&specs[5])?;
    wait_for_log(
        &mut full_node_process,
        "listening on tcp://",
        Duration::from_secs(180),
    )?;
    let mut full_node = connect_or_initialize_full_node_enclave(
        &specs[5].endpoint,
        &full_node_data,
        full_node_identity,
        |hash| prehash_signature(&full_node_signer, hash),
    )?;
    let full_node_manifest = load_committed_enclave_manifest_v1(&full_node_data)?;
    let full_node_binding = random_nonzero_b256()?;
    let full_node_register = registration_intent(
        &full_node_manifest,
        AttestationOperationV1::RegisterEnclave,
        policy_hash,
        full_node_binding,
        1,
        1,
        0,
        0,
        now + VALID_FOR,
    )?;
    let full_node_generated = full_node.generate_dcap_quote(&full_node_register)?;
    onboard_keyless_enclave(
        &mut founders[0],
        &mut full_node,
        &full_node_register,
        full_node_generated,
        &components,
        &policy_bytes,
        prehash_signature(
            &full_node_signer,
            full_node_register.intent_hash().map_err(codec)?,
        )
        .map_err(eyre::Report::msg)?,
        offer_public,
        now,
    )?;
    drop(full_node);
    full_node_process.stop()?;
    let mut full_node_process = start_sgx(&specs[5])?;
    wait_for_log(
        &mut full_node_process,
        "listening on tcp://",
        Duration::from_secs(180),
    )?;
    let mut full_node = connect_or_initialize_full_node_enclave(
        &specs[5].endpoint,
        &full_node_data,
        full_node_identity,
        |hash| prehash_signature(&full_node_signer, hash),
    )?;
    verify_renewal_quote_k256(
        &mut founders[0],
        &mut full_node,
        &full_node_manifest,
        &full_node_signer,
        policy_hash,
        full_node_binding,
        &components,
        &policy_bytes,
        now,
    )?;
    drop(full_node);
    full_node_process.stop()?;

    eprintln!("stage: same-platform candidate prepare and root copy");
    let active_manifest = load_committed_enclave_manifest_v1(&validator_data[0])?;
    let mut candidate_process = start_sgx(&specs[6])?;
    wait_for_log(
        &mut candidate_process,
        "listening on tcp://",
        Duration::from_secs(180),
    )?;
    let active_signer = &validator_signers[0];
    let active_identity = validator_identity(active_signer, validator_bls[0]);
    let candidate = prepare_validator_enclave_replacement_candidate(
        &specs[6].endpoint,
        &validator_data[0],
        active_identity,
        |hash| node_signature(active_signer, hash),
    )?;
    let candidate_manifest = candidate.manifest().clone();
    drop(candidate);
    candidate_process.stop()?;
    let context = UpgradeContextV1 {
        predecessor_manifest_hash: active_manifest.authorization_hash().map_err(codec)?,
        candidate_manifest_hash: candidate_manifest.authorization_hash().map_err(codec)?,
        successor_policy_hash: policy_hash,
        activation_height: ACTIVATION_HEIGHT,
        active_tee_dir: specs[0].tee_dir.clone(),
        candidate_tee_dir: specs[6].tee_dir.clone(),
    };
    prepare_upgrade_journal_v1(&validator_data[0], context)?;
    let copied = copy_same_platform_sealed_root_and_checkpoint_v1(&validator_data[0])?;
    ensure!(
        matches!(copied.lifecycle, UpgradeJournalStateV1::RootCopied { .. }),
        "upgrade journal did not reach rootCopied"
    );

    eprintln!("stage: candidate restart, transition quote and key-ready proof");
    let mut candidate_process = start_sgx(&specs[6])?;
    wait_for_log(
        &mut candidate_process,
        "listening on tcp://",
        Duration::from_secs(180),
    )?;
    let mut candidate = prepare_validator_enclave_replacement_candidate(
        &specs[6].endpoint,
        &validator_data[0],
        active_identity,
        |hash| node_signature(active_signer, hash),
    )?;
    let transition_binding = random_nonzero_b256()?;
    let transition = registration_intent(
        &candidate_manifest,
        AttestationOperationV1::TransitionEnclaveMeasurement,
        policy_hash,
        transition_binding,
        2,
        1,
        0,
        1,
        now + VALID_FOR,
    )?;
    let generated = candidate.generate_dcap_quote(&transition)?;
    let proof = generated
        .transition_key_ready_proof
        .ok_or_else(|| eyre!("candidate returned no transition key-ready proof"))?;
    let transition_evidence = AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
        intent: transition.clone(),
        quote: generated.quote_body,
        components: components.clone(),
        transition_key_ready_proof: Some(proof),
    });
    verify_evidence(&mut founders[0], &transition_evidence, &policy_bytes, now)?;
    let ready =
        record_candidate_key_ready_v1(&validator_data[0], &transition, &proof, offer_public)?;
    ensure!(
        matches!(
            ready.lifecycle,
            UpgradeJournalStateV1::CandidateKeyReady { .. }
        ),
        "upgrade journal did not reach candidateKeyReady"
    );
    drop(candidate);
    candidate_process.stop()?;
    drop(founders);
    for process in founder_processes {
        process.stop()?;
    }

    ensure!(
        specs[0].measurement.mrsigner == specs[6].measurement.mrsigner,
        "A and B were not signed by the same MRSIGNER"
    );
    let summary = EvidenceSummary {
        chain_id: CHAIN_ID,
        backend: "gramine-sgx-dcap",
        founder_validators: 4,
        validator_onboarding_cases: 1,
        full_nodes: 1,
        validator_onboarding_and_renewal: true,
        full_node_onboarding_and_renewal: true,
        same_mrsigner_root_reopen: true,
        transition_key_ready_proof: true,
        enclave_qvl_accepted_all_quotes: true,
        offer_public: hex::encode(offer_public),
        active_mrenclave: hex::encode(specs[0].measurement.mrenclave),
        candidate_mrenclave: hex::encode(specs[6].measurement.mrenclave),
        mrsigner: hex::encode(specs[0].measurement.mrsigner),
    };
    println!("{}", serde_json::to_string_pretty(&summary)?);
    if cli.keep_work_dir {
        let kept = work.keep();
        eprintln!("kept lifecycle SGX evidence at {}", kept.display());
    }
    Ok(())
}

fn establish_offer_key(clients: &mut [AuthorizedEnclaveClient]) -> Result<[u8; 32]> {
    let participants = clients
        .iter_mut()
        .map(
            |client| match client.request(&EnclaveRequest::GetPublicKeys)? {
                EnclaveResponse::PublicKeys {
                    tee_bls_pub,
                    dkg_enc_pub,
                    dkg_enc_sig,
                    ..
                } => Ok(ParticipantAnnounce {
                    bls_pub: tee_bls_pub,
                    enc_pub: dkg_enc_pub,
                    enc_sig: dkg_enc_sig,
                }),
                other => Err(eyre!("unexpected GetPublicKeys response: {other:?}")),
            },
        )
        .collect::<Result<Vec<_>>>()?;
    let index_of = participants
        .iter()
        .enumerate()
        .map(|(index, participant)| (participant.bls_pub.clone(), index))
        .collect::<BTreeMap<_, _>>();
    let ceremony_id = random_nonzero_b256()?;
    let coords = participants
        .iter()
        .map(|participant| {
            CeremonyCoordinator::new(
                ceremony_id,
                0,
                participant.bls_pub.clone(),
                participants.clone(),
            )
        })
        .collect::<Vec<_>>();
    for index in 0..clients.len() {
        coords[index].open(&mut clients[index])?;
    }
    let mut bundles = vec![Vec::<DealerBundle>::new(); clients.len()];
    for index in 0..clients.len() {
        for addressed in coords[index].deal(&mut clients[index])? {
            bundles[index_of[&addressed.to]].push(addressed.msg);
        }
    }
    let mut acks = vec![Vec::<Ack>::new(); clients.len()];
    for recipient in 0..clients.len() {
        for bundle in std::mem::take(&mut bundles[recipient]) {
            if let Some(addressed) = coords[recipient].ingest(&mut clients[recipient], &bundle)? {
                acks[index_of[&addressed.to]].push(addressed.msg);
            }
        }
    }
    for dealer in 0..clients.len() {
        for ack in std::mem::take(&mut acks[dealer]) {
            coords[dealer].receive_ack(&mut clients[dealer], &ack)?;
        }
    }
    let logs = (0..clients.len())
        .map(|index| {
            coords[index]
                .finalize_dealer(&mut clients[index])
                .map_err(Into::into)
        })
        .collect::<Result<Vec<FinalizedLog>>>()?;
    let outcomes = (0..clients.len())
        .map(|index| {
            coords[index]
                .finalize_player(&mut clients[index], &logs)
                .map_err(Into::into)
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        outcomes
            .iter()
            .all(|outcome| outcome.group_public == outcomes[0].group_public),
        "founders disagree on DKG group public key"
    );
    let commitments = outcomes
        .iter()
        .map(|outcome| outcome.share_commitment)
        .collect::<BTreeSet<_>>();
    ensure!(
        commitments.len() == clients.len(),
        "founder DKG shares are not distinct"
    );

    let mut sealed = vec![BTreeMap::<Vec<u8>, Vec<u8>>::new(); clients.len()];
    for signer in 0..clients.len() {
        for (recipient, blob) in
            coords[signer].tribute_offer_partials_sealed(&mut clients[signer])?
        {
            sealed[index_of[&recipient]].insert(participants[signer].bls_pub.clone(), blob);
        }
    }
    let chain_word = B256::from(U256::from(CHAIN_ID).to_be_bytes());
    let mut offer_public = None;
    for recipient in 0..clients.len() {
        let partials = sealed[recipient].values().cloned().collect::<Vec<_>>();
        let (public, _) = coords[recipient].finalize_tribute_offer(
            &mut clients[recipient],
            &partials,
            chain_word,
            0,
        )?;
        if let Some(expected) = offer_public {
            ensure!(
                public == expected,
                "founders derived different permanent offer keys"
            );
        } else {
            offer_public = Some(public);
        }
    }
    offer_public.ok_or_else(|| eyre!("founder DKG returned no offer key"))
}

#[allow(clippy::too_many_arguments)]
fn onboard_keyless_enclave(
    source: &mut AuthorizedEnclaveClient,
    target: &mut AuthorizedEnclaveClient,
    intent: &RegistrationIntentV1,
    generated: outbe_tee::GeneratedDcapQuoteV1,
    components: &[DcapCollateralComponentV1],
    policy: &[u8],
    node_signature: [u8; 65],
    offer_public: [u8; 32],
    timestamp: u64,
) -> Result<()> {
    let evidence = AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
        intent: intent.clone(),
        quote: generated.quote_body,
        components: components.to_vec(),
        transition_key_ready_proof: None,
    });
    let evidence_bytes = evidence.encode_canonical().map_err(codec)?;
    let result = source.verify_dcap_registration_and_seal_v1(
        &evidence_bytes,
        policy,
        timestamp,
        &node_signature,
        &generated.enclave_signature,
        offer_public,
        0,
        0,
    )?;
    ensure!(
        matches!(result.outcome, DcapVerificationOutcomeV1::Accepted(_)),
        "onboarding evidence was rejected"
    );
    let artifact = result
        .artifact
        .ok_or_else(|| eyre!("accepted onboarding returned no artifact"))?;
    let artifact = artifact
        .encode_canonical()
        .map_err(|code| eyre!("encode onboarding artifact: {:#06x}", code.code()))?;
    let response = target.request(&EnclaveRequest::IngestDcapOnboardingArtifactV1 {
        artifact,
        expected_intent_hash: intent.intent_hash().map_err(codec)?,
        expected_tribute_offer_public: offer_public,
        expected_key_epoch: 0,
        expected_tribute_offer_epoch: 0,
    })?;
    ensure!(
        matches!(response, EnclaveResponse::OfferKeyForRegistryIngested { tribute_offer_public } if tribute_offer_public == offer_public),
        "target did not install the exact permanent offer key"
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn verify_renewal_quote(
    verifier: &mut AuthorizedEnclaveClient,
    target: &mut AuthorizedEnclaveClient,
    manifest: &EnclaveInitializationManifestV1,
    signer: &PrivateKeySigner,
    policy_hash: B256,
    binding_id: B256,
    components: &[DcapCollateralComponentV1],
    policy: &[u8],
    timestamp: u64,
) -> Result<()> {
    let intent = registration_intent(
        manifest,
        AttestationOperationV1::RenewEnclave,
        policy_hash,
        binding_id,
        1,
        1,
        1,
        0,
        timestamp + VALID_FOR,
    )?;
    let generated = target.generate_dcap_quote(&intent)?;
    ensure!(
        intent.verify_node_signature(
            &node_signature(signer, intent.intent_hash().map_err(codec)?)
                .map_err(eyre::Report::msg)?
        ),
        "validator renewal node signature is invalid"
    );
    verify_generated(verifier, &intent, generated, components, policy, timestamp)
}

#[allow(clippy::too_many_arguments)]
fn verify_renewal_quote_k256(
    verifier: &mut AuthorizedEnclaveClient,
    target: &mut AuthorizedEnclaveClient,
    manifest: &EnclaveInitializationManifestV1,
    signer: &SigningKey,
    policy_hash: B256,
    binding_id: B256,
    components: &[DcapCollateralComponentV1],
    policy: &[u8],
    timestamp: u64,
) -> Result<()> {
    let intent = registration_intent(
        manifest,
        AttestationOperationV1::RenewEnclave,
        policy_hash,
        binding_id,
        1,
        1,
        1,
        0,
        timestamp + VALID_FOR,
    )?;
    ensure!(
        intent.verify_node_signature(
            &prehash_signature(signer, intent.intent_hash().map_err(codec)?)
                .map_err(eyre::Report::msg)?
        ),
        "full-node renewal node signature is invalid"
    );
    let generated = target.generate_dcap_quote(&intent)?;
    verify_generated(verifier, &intent, generated, components, policy, timestamp)
}

fn verify_generated(
    verifier: &mut AuthorizedEnclaveClient,
    intent: &RegistrationIntentV1,
    generated: outbe_tee::GeneratedDcapQuoteV1,
    components: &[DcapCollateralComponentV1],
    policy: &[u8],
    timestamp: u64,
) -> Result<()> {
    let evidence = AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
        intent: intent.clone(),
        quote: generated.quote_body,
        components: components.to_vec(),
        transition_key_ready_proof: generated.transition_key_ready_proof,
    });
    verify_evidence(verifier, &evidence, policy, timestamp)
}

fn verify_evidence(
    verifier: &mut AuthorizedEnclaveClient,
    evidence: &AttestationEvidenceV1,
    policy: &[u8],
    timestamp: u64,
) -> Result<()> {
    let bytes = evidence.encode_canonical().map_err(codec)?;
    ensure!(
        matches!(
            verifier.verify_dcap_evidence_v1(&bytes, policy, timestamp)?,
            DcapVerificationOutcomeV1::Accepted(_)
        ),
        "enclave QVL rejected lifecycle evidence"
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn registration_intent(
    manifest: &EnclaveInitializationManifestV1,
    operation: AttestationOperationV1,
    policy_hash: B256,
    binding_id: B256,
    binding_version: u64,
    registration_version: u64,
    renewal_nonce: u64,
    transition_nonce: u64,
    requested_valid_until: u64,
) -> Result<RegistrationIntentV1> {
    let intent = RegistrationIntentV1 {
        chain_id: manifest.chain_id,
        genesis_hash: manifest.genesis_hash,
        operation,
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash,
        enclave_profile: manifest.enclave_profile,
        node_id: manifest.node_id.clone(),
        enclave_id: manifest.enclave_id().map_err(codec)?,
        binding_id,
        binding_version,
        registration_version,
        renewal_nonce,
        transition_nonce,
        requested_valid_until,
        recipient_x25519: manifest.recipient_x25519,
        attestation_ed25519: manifest.attestation_ed25519,
        noise_responder_x25519: manifest.noise_responder_x25519,
        node_host_authorization_hash: manifest.node_host_authorization_hash().map_err(codec)?,
    };
    intent.encode_canonical().map_err(codec)?;
    Ok(intent)
}

fn lifecycle_policy(specs: &[SgxSpec]) -> Result<TeePolicyV1> {
    let mut rules = specs
        .iter()
        .enumerate()
        .map(|(index, spec)| TeeMeasurementRuleV1 {
            enclave_profile: if index == 5 {
                EnclaveProfile::FullNode
            } else {
                EnclaveProfile::Validator
            },
            mrenclave: spec.measurement.mrenclave,
            mrsigner: spec.measurement.mrsigner,
            isv_prod_id: spec.measurement.isv_prod_id,
            minimum_isv_svn: spec.measurement.isv_svn,
            admit_from_height: 1,
            admit_until_height_exclusive: ADMIT_UNTIL,
        })
        .collect::<Vec<_>>();
    rules.sort_by_key(|rule| {
        let profile = match rule.enclave_profile {
            EnclaveProfile::Validator => 1_u8,
            EnclaveProfile::FullNode => 2_u8,
        };
        (
            profile,
            rule.mrenclave,
            rule.mrsigner,
            rule.isv_prod_id,
            rule.minimum_isv_svn,
            rule.admit_from_height,
            rule.admit_until_height_exclusive,
        )
    });
    rules.dedup();
    let intel_root_der_hash = B256::from_slice(&hex::decode(INTEL_ROOT_DER_HASH_HEX)?);
    let policy = TeePolicyV1 {
        policy_version: 1,
        chain_id: U256::from(CHAIN_ID).to_be_bytes(),
        genesis_hash: GENESIS_HASH,
        activation_height: 1,
        predecessor_policy_hash: B256::ZERO,
        attestation_mode: AttestationMode::DcapRequired,
        intel_root_der_hash,
        quote_version: 3,
        tee_type: 0,
        attestation_key_type: 2,
        qe_vendor_id: [
            0x93, 0x9a, 0x72, 0x33, 0xf7, 0x9c, 0x4c, 0xa9, 0x94, 0x0a, 0x0d, 0xb3, 0x95, 0x7f,
            0x06, 0x07,
        ],
        certification_data_type: 5,
        tcb_info_schema_version: 3,
        qe_identity_schema_version: 2,
        minimum_tcb_evaluation_data_number: 1,
        accepted_platform_tcb_statuses: PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
        accepted_qe_tcb_status: QvlTcbStatusV1::UpToDate,
        minimum_lease: 3_600,
        maximum_lease: 604_800,
        collateral_margin: 3_600,
        resource_schedule_hash: B256::repeat_byte(0x44),
        measurement_rules: rules,
    };
    policy.encode_canonical().map_err(codec)?;
    Ok(policy)
}

fn render_and_sign(
    work: &Path,
    template: &Path,
    binary: &Path,
    signing_key: &Path,
    qvl_dir: &Path,
    name: &str,
) -> Result<SgxSpec> {
    let tee_dir = private_dir(&work.join(format!("{name}-tee")))?;
    let endpoint = allocate_loopback_endpoint()?;
    let manifest_base = work.join(name);
    let manifest = manifest_base.with_extension("manifest");
    command_ok(
        Command::new("gramine-manifest")
            .arg("-Dlog_level=error")
            .arg("-Darch_libdir=/lib/x86_64-linux-gnu")
            .arg(format!("-Dentrypoint={}", binary.display()))
            .arg(format!("-Dtee_dir={}", tee_dir.display()))
            .arg("-Dremote_attestation=dcap")
            .arg(format!("-Dqvl_host_dir={}", qvl_dir.display()))
            .arg(template)
            .arg(&manifest),
        "render lifecycle SGX manifest",
    )?;
    command_ok(
        Command::new("gramine-sgx-sign")
            .args(["--manifest"])
            .arg(&manifest)
            .args(["--output"])
            .arg(manifest_base.with_extension("manifest.sgx"))
            .args(["--key"])
            .arg(signing_key),
        "sign lifecycle SGX manifest",
    )?;
    let output = Command::new("gramine-sgx-sigstruct-view")
        .arg(manifest_base.with_extension("sig"))
        .output()
        .wrap_err("read lifecycle SIGSTRUCT")?;
    ensure!(output.status.success(), "gramine-sgx-sigstruct-view failed");
    let measurement = parse_measurement(&String::from_utf8(output.stdout)?)?;
    Ok(SgxSpec {
        name: name.to_owned(),
        endpoint,
        tee_dir,
        manifest_base,
        measurement,
    })
}

fn start_sgx(spec: &SgxSpec) -> Result<RunningSgx> {
    let log = spec.manifest_base.with_extension("log");
    let stdout = File::create(&log)?;
    let stderr = stdout.try_clone()?;
    let chain_id = format!(
        "0x{}",
        hex::encode(U256::from(CHAIN_ID).to_be_bytes::<32>())
    );
    let child = Command::new("gramine-sgx")
        .arg(&spec.manifest_base)
        .args(["--socket", &spec.endpoint, "--tee-dir"])
        .arg(&spec.tee_dir)
        .args(["--chain-id", &chain_id])
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .wrap_err_with(|| format!("start {}", spec.name))?;
    Ok(RunningSgx { child, log })
}

fn wait_for_log(process: &mut RunningSgx, marker: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let content = fs::read_to_string(&process.log).unwrap_or_default();
        if content.contains(marker) {
            return Ok(());
        }
        if let Some(status) = process.child.try_wait()? {
            bail!("enclave exited before {marker:?} with {status}:\n{content}");
        }
        if Instant::now() >= deadline {
            bail!("enclave did not emit {marker:?} within {timeout:?}:\n{content}");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn capture_collateral(repo: &Path, quote: &Path, output: &Path) -> Result<()> {
    command_ok(
        Command::new("python3")
            .arg(repo.join("scripts/release/capture_dcap_collateral.py"))
            .args(["--quote"])
            .arg(quote)
            .args(["--output-dir"])
            .arg(output)
            .args(["--expected-pck-ca", "processor"]),
        "capture fresh Processor collateral",
    )
}

fn collateral_components(dir: &Path) -> Result<Vec<DcapCollateralComponentV1>> {
    COMPONENT_FILES
        .iter()
        .map(|(kind, name)| {
            Ok(DcapCollateralComponentV1 {
                kind: *kind,
                bytes: fs::read(dir.join(name))?,
            })
        })
        .collect()
}

fn prepare_qvl_dir(work: &Path) -> Result<PathBuf> {
    let dir = private_dir(&work.join("qvl"))?;
    for source in [
        "/lib/x86_64-linux-gnu/libsgx_dcap_quoteverify.so.1",
        "/lib/x86_64-linux-gnu/libstdc++.so.6",
        "/lib/x86_64-linux-gnu/libgcc_s.so.1",
    ] {
        let source = Path::new(source);
        fs::copy(
            source,
            dir.join(
                source
                    .file_name()
                    .ok_or_else(|| eyre!("QVL path has no file name"))?,
            ),
        )?;
    }
    Ok(dir)
}

fn parse_measurement(output: &str) -> Result<Measurement> {
    fn field<'a>(output: &'a str, name: &str) -> Result<&'a str> {
        output
            .lines()
            .find_map(|line| {
                let (actual, value) = line.split_once(':')?;
                actual
                    .trim()
                    .eq_ignore_ascii_case(name)
                    .then(|| value.trim())
            })
            .ok_or_else(|| eyre!("SIGSTRUCT output has no {name}"))
    }
    fn b256(output: &str, name: &str) -> Result<B256> {
        let raw = field(output, name)?
            .strip_prefix("0x")
            .unwrap_or(field(output, name)?);
        let bytes = hex::decode(raw)?;
        ensure!(bytes.len() == 32, "SIGSTRUCT {name} is not 32 bytes");
        Ok(B256::from_slice(&bytes))
    }
    fn number(output: &str, name: &str) -> Result<u16> {
        let value = field(output, name)?;
        if let Some(hex) = value.strip_prefix("0x") {
            Ok(u16::from_str_radix(hex, 16)?)
        } else {
            Ok(value.parse()?)
        }
    }
    Ok(Measurement {
        mrenclave: b256(output, "mr_enclave")?,
        mrsigner: b256(output, "mr_signer")?,
        isv_prod_id: number(output, "isv_prod_id")?,
        isv_svn: number(output, "isv_svn")?,
    })
}

fn validator_identity(
    signer: &PrivateKeySigner,
    consensus_bls_public: [u8; 48],
) -> ValidatorNodeHostIdentityV1 {
    ValidatorNodeHostIdentityV1 {
        chain_id: CHAIN_ID,
        genesis_hash: GENESIS_HASH,
        validator: signer.address(),
        consensus_bls_public,
    }
}

fn random_node_signer() -> Result<PrivateKeySigner> {
    for _ in 0..16 {
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        if let Ok(signer) = PrivateKeySigner::from_bytes(&B256::from(bytes)) {
            return Ok(signer);
        }
    }
    bail!("failed to generate validator signer")
}

fn random_k256_signer() -> Result<SigningKey> {
    for _ in 0..16 {
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        if let Ok(signer) = SigningKey::from_slice(&bytes) {
            return Ok(signer);
        }
    }
    bail!("failed to generate full-node signer")
}

fn node_signature(signer: &PrivateKeySigner, hash: B256) -> Result<[u8; 65], String> {
    let signature = signer
        .sign_hash_sync(&hash)
        .map_err(|error| error.to_string())?;
    let mut bytes = [0_u8; 65];
    bytes[..32].copy_from_slice(&signature.r().to_be_bytes::<32>());
    bytes[32..64].copy_from_slice(&signature.s().to_be_bytes::<32>());
    bytes[64] = u8::from(signature.v());
    Ok(bytes)
}

fn prehash_signature(signer: &SigningKey, hash: B256) -> Result<[u8; 65], String> {
    let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = signer
        .sign_prehash(hash.as_slice())
        .map_err(|error| error.to_string())?;
    let mut bytes = [0_u8; 65];
    bytes[..64].copy_from_slice(signature.to_bytes().as_slice());
    bytes[64] = recovery.to_byte();
    Ok(bytes)
}

fn compressed_public(signer: &SigningKey) -> Result<[u8; 33]> {
    signer
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .map_err(|_| eyre!("full-node public key is not SEC1-33"))
}

fn valid_bls_public(seed: u64) -> Result<[u8; 48]> {
    bls12381::PrivateKey::from_seed(seed)
        .public_key()
        .encode()
        .as_ref()
        .try_into()
        .map_err(|_| eyre!("BLS public key is not 48 bytes"))
}

fn random_nonzero_b256() -> Result<B256> {
    for _ in 0..16 {
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        if bytes != [0; 32] {
            return Ok(B256::from(bytes));
        }
    }
    bail!("OS RNG repeatedly returned zero")
}

fn unix_seconds() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn allocate_loopback_endpoint() -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.to_string())
}

fn private_dir(path: &Path) -> Result<PathBuf> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(path.to_path_buf())
}

fn default_signing_key() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/nonexistent"))
        .join(".config/gramine/enclave-key.pem")
}

fn codec(error: impl std::fmt::Display) -> eyre::Report {
    eyre!(error.to_string())
}

fn command_ok(command: &mut Command, context: &str) -> Result<()> {
    let output = command.output().wrap_err_with(|| context.to_owned())?;
    if !output.status.success() {
        bail!(
            "{context} failed with {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}
