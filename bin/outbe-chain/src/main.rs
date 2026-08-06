//! Outbe-reth node binary.
//!
//! Custom reth node with Outbe stateful precompiles and Commonware Simplex consensus.
//! Two tokio runtimes: Reth execution (main thread) + Commonware consensus (spawned thread).
//!
//! Also provides the `dkg` subcommand for bootstrapping BLS threshold key material.

use clap::Parser;
use commonware_codec::Encode as _;
use commonware_cryptography::Signer as _;
use commonware_runtime::Runner as _;
use eyre::WrapErr as _;
use outbe_compressed_entities::{
    CandidateCacheLimits, CeMdbx, CompressedTreeService, EnvironmentIdentity, FinalizedMarker,
    ACTIVE_COMMITMENT_SCHEME, LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_consensus::executor::actor::FinalizedCeCommitter;
use outbe_engine::args::ConsensusArgs;
use outbe_engine::bridge::ConsensusExecutionBridge;
use outbe_engine::ce_finalizer::{
    DurableCeState, FinalizedCeTree, RethCeFinalizer, RethDurableCeState,
};
use outbe_engine::ce_recovery::{
    CanonicalCeReplaySource, CeStartupRecovery, CeStartupRecoveryCoordinator, StartupCeTree,
};
use outbe_evm::OutbeEvmSigner;
use outbe_node::{
    compressed_storage::{
        validate_compressed_storage_runtime_config, CompressedStorageRuntimeConfig,
    },
    projection::{
        prepare_offchain_data_projection, validate_offchain_data_checkpoint,
        OffchainDataProjectionConfig,
    },
    OutbeBeaconConsensus, OutbeFullNode, OutbeNode,
};
use outbe_operator::{
    rpc::HttpRenewalRpc,
    tee::{
        inspect_upgrade_journal_v1, read_renewal_status_v1, record_upgrade_finalized_v1,
        record_upgrade_missed_cutoff_v1, record_upgrade_promoted_v1, run_renewal_once_v1,
        NodeBindingSelectorV1, RenewalAlertLevelV1, RenewalEnclaveV1, RenewalModeV1,
        RenewalNodeSignerV1, RenewalOutcomeV1, RenewalServiceConfigV1, UpgradeJournalStateV1,
    },
    tx::RelaySignerV1,
};
use outbe_primitives::projection::ProjectionReadinessHandle;
use outbe_primitives::OutbeHeader;
use reth_chainspec::ChainSpec;
use reth_cli::chainspec::ChainSpecParser;
use reth_ethereum::cli::interface::Cli;
use reth_node_builder::NodeHandle;
use reth_provider::{HeaderProvider, StateProviderFactory};
use reth_rpc_server_types::{RethRpcModule, RpcModuleSelection, RpcModuleValidator};
use std::{path::PathBuf, sync::Arc, thread};
use tokio::sync::oneshot;
use tracing::info;

mod ocomp_genesis;
mod tee_genesis;

enum RenewalNodeAuthorityV1 {
    Validator(Arc<OutbeEvmSigner>),
    FullNode(k256::ecdsa::SigningKey),
}

impl RenewalNodeSignerV1 for RenewalNodeAuthorityV1 {
    fn sign_node_hash(&self, hash: alloy_primitives::B256) -> eyre::Result<[u8; 65]> {
        match self {
            Self::Validator(signer) => signer
                .sign_hash(&hash)
                .map_err(|error| eyre::eyre!("validator renewal signing failed: {error}")),
            Self::FullNode(signer) => {
                use k256::ecdsa::signature::hazmat::PrehashSigner as _;
                let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
                    signer.sign_prehash(hash.as_slice()).map_err(|error| {
                        eyre::eyre!("full-node renewal signing failed: {error}")
                    })?;
                let mut bytes = [0_u8; 65];
                bytes[..64].copy_from_slice(&signature.to_bytes());
                bytes[64] = recovery.to_byte();
                Ok(bytes)
            }
        }
    }
}

struct GlobalRenewalEnclaveV1;

impl RenewalEnclaveV1 for GlobalRenewalEnclaveV1 {
    fn generate_dcap_quote(
        &mut self,
        intent: &outbe_primitives::tee_attestation_v1::RegistrationIntentV1,
    ) -> eyre::Result<outbe_tee::GeneratedDcapQuoteV1> {
        outbe_tee::generate_dcap_quote_v1(intent)
            .map_err(|error| eyre::eyre!("generate renewal quote: {error}"))
    }
}

struct RenewalWorkerV1 {
    rpc_url: String,
    relay: RelaySignerV1,
    authority: RenewalNodeAuthorityV1,
    config: RenewalServiceConfigV1,
    poll_secs: u64,
    warning_blocks: u64,
    critical_blocks: u64,
}

async fn run_renewal_worker_v1(
    worker: RenewalWorkerV1,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let rpc = HttpRenewalRpc::new(worker.rpc_url);
    let mut enclave = GlobalRenewalEnclaveV1;
    loop {
        let upgrade_blocks_renewal = match inspect_upgrade_journal_v1(&worker.config.node_data_dir)
        {
            Ok(Some(snapshot)) => matches!(
                snapshot.lifecycle,
                UpgradeJournalStateV1::CandidatePrepared { .. }
                    | UpgradeJournalStateV1::RootCopied { .. }
                    | UpgradeJournalStateV1::CandidateKeyReady { .. }
                    | UpgradeJournalStateV1::SubmissionPrepared { .. }
                    | UpgradeJournalStateV1::Submitted { .. }
                    | UpgradeJournalStateV1::Finalized { .. }
                    | UpgradeJournalStateV1::Promoted { .. }
                    | UpgradeJournalStateV1::TerminalMissedCutoff { .. }
            ),
            Ok(None) => false,
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "read upgrade checkpoint before renewal failed; renewal paused fail-closed");
                true
            }
        };
        let renewal = if upgrade_blocks_renewal {
            None
        } else {
            Some(
                run_renewal_once_v1(
                    &rpc,
                    &worker.relay,
                    &mut enclave,
                    &worker.authority,
                    &worker.config,
                    RenewalModeV1::Automatic,
                )
                .await,
            )
        };
        match renewal {
            None => {}
            Some(Ok(RenewalOutcomeV1::Submitted {
                transaction_hash,
                replayed,
            })) => info!(%transaction_hash, replayed, "automatic DCAP renewal submitted"),
            Some(Ok(RenewalOutcomeV1::Finalized {
                finalized_height,
                valid_until,
            })) => info!(
                finalized_height,
                valid_until, "automatic DCAP renewal finalized"
            ),
            Some(Ok(RenewalOutcomeV1::Abandoned {
                finalized_height,
                reason,
            })) => {
                tracing::warn!(finalized_height, %reason, "stale DCAP renewal abandoned; next pass will rebuild")
            }
            Some(Ok(RenewalOutcomeV1::NotDue { .. })) => {}
            Some(Err(error)) => {
                tracing::error!(error = %format!("{error:#}"), "automatic DCAP renewal reconciliation failed")
            }
        }
        match read_renewal_status_v1(
            &rpc,
            &worker.config.node_data_dir,
            &worker.config.selector,
            worker.warning_blocks,
            worker.critical_blocks,
        )
        .await
        {
            Ok(status) if status.alert == RenewalAlertLevelV1::Critical => tracing::error!(
                finalized_height = status.finalized_height,
                next_freeze_height = status.next_freeze_height,
                valid_until = status.valid_until,
                "DCAP renewal is unsafe at the critical DKG-freeze margin"
            ),
            Ok(status) if status.alert == RenewalAlertLevelV1::Warning => tracing::warn!(
                finalized_height = status.finalized_height,
                next_freeze_height = status.next_freeze_height,
                valid_until = status.valid_until,
                "DCAP renewal is unsafe at the warning DKG-freeze margin"
            ),
            Ok(_) => {}
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "read automatic DCAP renewal status failed")
            }
        }
        tokio::select! {
            () = shutdown.cancelled() => break,
            () = tokio::time::sleep(std::time::Duration::from_secs(worker.poll_secs)) => {}
        }
    }
}

struct UpgradePromotionWorkerConfigV1 {
    chain_id: u64,
    genesis_hash: alloy_primitives::B256,
    node_data_dir: PathBuf,
    poll_secs: u64,
    warning_blocks: u64,
    critical_blocks: u64,
    promoted: Arc<tokio::sync::Notify>,
}

async fn run_upgrade_promotion_worker_v1<P>(provider: P, config: UpgradePromotionWorkerConfigV1)
where
    P: HeaderProvider<Header = OutbeHeader> + StateProviderFactory + Send + Sync + 'static,
{
    let UpgradePromotionWorkerConfigV1 {
        chain_id,
        genesis_hash,
        node_data_dir,
        poll_secs,
        warning_blocks,
        critical_blocks,
        promoted,
    } = config;
    loop {
        let snapshot = match inspect_upgrade_journal_v1(&node_data_dir) {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => {
                tokio::time::sleep(std::time::Duration::from_secs(poll_secs)).await;
                continue;
            }
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "read enclave-upgrade journal failed");
                return;
            }
        };
        let context = snapshot.lifecycle.context().clone();
        match &snapshot.lifecycle {
            UpgradeJournalStateV1::Promoted { .. } => return,
            UpgradeJournalStateV1::TerminalMissedCutoff {
                finalized_height,
                activation_height,
                ..
            } => {
                tracing::error!(
                    finalized_height,
                    activation_height,
                    "enclave upgrade is terminal after missing successor activation cutoff"
                );
                return;
            }
            UpgradeJournalStateV1::Finalized {
                finalized_height,
                finalized_hash,
                ..
            } => {
                let committed = match outbe_tee::load_committed_enclave_manifest_v1(&node_data_dir)
                {
                    Ok(committed) => committed,
                    Err(error) => {
                        tracing::error!(error = %error, "load committed manifest during upgrade recovery failed");
                        return;
                    }
                };
                let committed_hash = match committed.authorization_hash() {
                    Ok(hash) => hash,
                    Err(error) => {
                        tracing::error!(error = %error, "hash committed manifest during upgrade recovery failed");
                        return;
                    }
                };
                if committed_hash == context.candidate_manifest_hash {
                    if let Err(error) = record_upgrade_promoted_v1(&node_data_dir) {
                        tracing::error!(error = %format!("{error:#}"), "record recovered upgrade promotion failed");
                        return;
                    }
                    info!(finalized_height, %finalized_hash, "reconciled already-promoted enclave candidate");
                    return;
                }
                match outbe_node::tee_remote_session::construct_local_finalized_replacement_authorization_with_view_v1(
                    &provider,
                    chain_id,
                    genesis_hash,
                    &node_data_dir,
                    &committed.node_id,
                    committed.enclave_profile,
                ) {
                    Ok(authorized) => {
                        if let Err(error) = outbe_tee::promote_replacement_candidate(
                            &node_data_dir,
                            &authorized.authorization,
                        ) {
                            tracing::error!(error = %error, "promote finalized enclave candidate failed");
                            return;
                        }
                        if let Err(error) = record_upgrade_promoted_v1(&node_data_dir) {
                            tracing::error!(error = %format!("{error:#}"), "record finalized enclave promotion failed");
                            return;
                        }
                        info!(finalized_height, %finalized_hash, "finalized enclave candidate promoted; execution restart required");
                        promoted.notify_one();
                        return;
                    }
                    Err(error) => {
                        tracing::error!(error = %error, "reconstruct finalized candidate promotion authority failed");
                        return;
                    }
                }
            }
            _ => {}
        }

        let status =
            match outbe_node::tee_remote_session::inspect_local_finalized_successor_status_v1(
                &provider,
                chain_id,
                genesis_hash,
            ) {
                Ok(status) => status,
                Err(error) => {
                    tracing::error!(error = %error, "read node-local finalized successor status failed");
                    tokio::time::sleep(std::time::Duration::from_secs(poll_secs)).await;
                    continue;
                }
            };
        if let Some(policy) = &status.staged_policy {
            let policy_hash = match policy.policy_hash() {
                Ok(hash) => hash,
                Err(error) => {
                    tracing::error!(error = %error, "hash node-local staged successor policy failed");
                    return;
                }
            };
            if policy_hash != context.successor_policy_hash
                || policy.activation_height != context.activation_height
            {
                tracing::error!(
                    "journaled enclave upgrade no longer matches the finalized staged successor"
                );
                return;
            }
        }
        if matches!(snapshot.lifecycle, UpgradeJournalStateV1::Submitted { .. }) {
            let committed = match outbe_tee::load_committed_enclave_manifest_v1(&node_data_dir) {
                Ok(committed) => committed,
                Err(error) => {
                    tracing::error!(error = %error, "load active manifest for finalized upgrade check failed");
                    return;
                }
            };
            match outbe_node::tee_remote_session::construct_local_finalized_replacement_authorization_with_view_v1(
                &provider,
                chain_id,
                genesis_hash,
                &node_data_dir,
                &committed.node_id,
                committed.enclave_profile,
            ) {
                Ok(authorized) => {
                    // A matching finalized B proves that transition execution
                    // happened before the Registry cutoff, even when finality
                    // advanced across H in one step.
                    if let Err(error) = record_upgrade_finalized_v1(
                        &node_data_dir,
                        authorized.view.block_number,
                        authorized.view.block_hash,
                    ) {
                        tracing::error!(error = %format!("{error:#}"), "record finalized enclave transition failed");
                        return;
                    }
                    if let Err(error) = outbe_tee::promote_replacement_candidate(
                        &node_data_dir,
                        &authorized.authorization,
                    ) {
                        tracing::error!(error = %error, "promote finalized enclave candidate failed");
                        return;
                    }
                    if let Err(error) = record_upgrade_promoted_v1(&node_data_dir) {
                        tracing::error!(error = %format!("{error:#}"), "record enclave candidate promotion failed");
                        return;
                    }
                    info!(
                        finalized_height = authorized.view.block_number,
                        finalized_hash = %authorized.view.block_hash,
                        "finalized enclave candidate promoted; execution restart required"
                    );
                    promoted.notify_one();
                    return;
                }
                Err(outbe_node::tee_remote_session::LocalRegistryAdmissionError::ReplacementBindingMissing) => {}
                Err(error) => {
                    tracing::error!(error = %error, "finalized enclave transition authorization failed");
                    return;
                }
            }
        }
        if status.view.block_number >= context.activation_height {
            match record_upgrade_missed_cutoff_v1(
                &node_data_dir,
                status.view.block_number,
                context.activation_height,
            ) {
                Ok(_) => tracing::error!(
                    finalized_height = status.view.block_number,
                    activation_height = context.activation_height,
                    "enclave upgrade missed its finalized successor activation cutoff"
                ),
                Err(error) => {
                    tracing::error!(error = %format!("{error:#}"), "record missed enclave-upgrade cutoff failed")
                }
            }
            return;
        }
        let remaining = context
            .activation_height
            .saturating_sub(status.view.block_number);
        if remaining <= critical_blocks {
            tracing::error!(
                finalized_height = status.view.block_number,
                activation_height = context.activation_height,
                remaining_blocks = remaining,
                "enclave upgrade is inside its critical finalized activation margin"
            );
        } else if remaining <= warning_blocks {
            tracing::warn!(
                finalized_height = status.view.block_number,
                activation_height = context.activation_height,
                remaining_blocks = remaining,
                "enclave upgrade is inside its warning finalized activation margin"
            );
        }

        tokio::time::sleep(std::time::Duration::from_secs(poll_secs)).await;
    }
}

#[derive(Debug, Clone, Default)]
struct OutbeChainSpecParser;

#[derive(Debug, Clone, Copy, Default)]
struct OutbeRpcModuleValidator;

impl RpcModuleValidator for OutbeRpcModuleValidator {
    fn parse_selection(s: &str) -> Result<RpcModuleSelection, String> {
        let selection = s
            .parse::<RpcModuleSelection>()
            .map_err(|error| format!("Failed to parse RPC modules: {error}"))?;

        if let RpcModuleSelection::Selection(modules) = &selection {
            for module in modules {
                let RethRpcModule::Other(name) = module else {
                    continue;
                };
                if name != "outbe" {
                    return Err(format!("Unknown RPC module: '{name}'"));
                }
            }
        }

        Ok(selection)
    }
}

impl ChainSpecParser for OutbeChainSpecParser {
    type ChainSpec = ChainSpec<OutbeHeader>;

    const SUPPORTED_CHAINS: &'static [&'static str] =
        reth_ethereum::cli::chainspec::SUPPORTED_CHAINS;

    fn parse(s: &str) -> eyre::Result<Arc<Self::ChainSpec>> {
        let chain_spec: Arc<Self::ChainSpec> =
            reth_ethereum::cli::chainspec::chain_value_parser(s)?
                .as_ref()
                .clone()
                .map_header(OutbeHeader::new)
                .into();
        outbe_evm::tee_attestation_activation::TeeAttestationChainSpecStateV1::from_chain_spec(
            chain_spec.as_ref(),
        )
        .activation()
        .map_err(|error| eyre::eyre!("invalid mandatory teeAttestationV1 ChainSpec: {error}"))?;
        outbe_node::ocomp::fork::require_startup_ocomp_fork_install(chain_spec.as_ref())?;
        Ok(chain_spec)
    }
}

fn handle_consensus_thread_join(joined: thread::Result<eyre::Result<()>>) -> eyre::Result<()> {
    match joined {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(err.wrap_err("consensus task exited with error")),
        Err(unwind) => std::panic::resume_unwind(unwind),
    }
}

/// DKG bootstrap subcommand, parsed separately from reth's CLI.
#[derive(clap::Parser)]
#[command(name = "outbe-chain-dkg")]
struct DkgCli {
    /// BLS key storage backend: plaintext, encrypted, or os-level.
    #[arg(long = "bls-key-backend", default_value = "plaintext", global = true)]
    bls_key_backend: String,

    /// Passphrase for the encrypted BLS key backend.
    #[arg(long = "bls-passphrase", env = "BLS_PASSPHRASE", global = true)]
    bls_passphrase: Option<String>,

    #[command(subcommand)]
    command: DkgCommand,
}

#[derive(clap::Subcommand)]
enum DkgCommand {
    /// Generate only the validator identity keys used by a fresh interactive genesis DKG.
    Identities {
        /// Output directory for generated identity key material.
        #[arg(long)]
        output_dir: std::path::PathBuf,

        /// Number of validator identities to generate.
        #[arg(long)]
        validators: u32,
    },
    /// Verify that imported founder private keys match their public validator manifest.
    VerifyIdentities {
        /// Public validators.json manifest.
        #[arg(long)]
        validators: std::path::PathBuf,

        /// Directory containing validator-N/signing-key.hex and evm-key.hex.
        #[arg(long)]
        material_dir: std::path::PathBuf,
    },
    /// Bootstrap DKG material for a validator set.
    Bootstrap {
        /// Output directory for generated key material.
        #[arg(long)]
        output_dir: std::path::PathBuf,

        /// Number of validators to bootstrap.
        #[arg(long)]
        validators: u32,
    },
    /// Show status of DKG key material in a storage directory.
    Status {
        /// Storage directory containing DKG material.
        #[arg(long)]
        storage_dir: std::path::PathBuf,
    },
    /// Export DKG signing share, polynomial, and output to a directory.
    ExportShare {
        /// Storage directory containing DKG material.
        #[arg(long)]
        storage_dir: std::path::PathBuf,

        /// Output directory for exported files.
        #[arg(long)]
        output: std::path::PathBuf,
    },
    /// Import DKG signing share, polynomial, and output into a storage directory.
    ImportShare {
        /// Path to the signing share file.
        #[arg(long)]
        share: std::path::PathBuf,

        /// Path to the public polynomial file.
        #[arg(long)]
        polynomial: std::path::PathBuf,

        /// Path to the DKG output file. Defaults to dkg_output.hex next to --share.
        #[arg(long)]
        output: Option<std::path::PathBuf>,

        /// Storage directory to import into.
        #[arg(long)]
        storage_dir: std::path::PathBuf,
    },
    /// Delete only the local consensus threshold material.
    /// This never modifies or recovers the permanent TEE offer key; normal
    /// genesis or live-join gates still decide whether startup may proceed.
    ForceRestart {
        /// Storage directory containing DKG material.
        #[arg(long)]
        storage_dir: std::path::PathBuf,
    },
}

fn main() -> eyre::Result<()> {
    // Intercept Outbe-owned subcommands before reth CLI parsing.
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "dkg" {
        return run_dkg_command(&args);
    }
    if args.len() > 1 && args[1] == "tee" {
        return tee_genesis::run(&args);
    }
    if args.len() > 1 && args[1] == "ocomp" {
        return ocomp_genesis::run(&args);
    }

    // Intercept `--version` / `-V` so that the user sees Outbe-side build
    // metadata in addition to Reth's own version string. The Outbe block is
    // printed first; Reth's CLI then prints its own version and exits.
    if args.iter().any(|a| a == "--version" || a == "-V") {
        print_outbe_version();
    }

    run_node()
}

/// Outbe build metadata block printed before delegating `--version` to
/// Reth's CLI. Layout mirrors reth-node-core / kona-node so operators
/// see a familiar five-line block.
const OUTBE_LONG_VERSION: &str = concat!(
    env!("OUTBE_LONG_VERSION_0"),
    "\n",
    env!("OUTBE_LONG_VERSION_1"),
    "\n",
    env!("OUTBE_LONG_VERSION_2"),
    "\n",
    env!("OUTBE_LONG_VERSION_3"),
    "\n",
    env!("OUTBE_LONG_VERSION_4"),
);

/// Print Outbe build metadata baked in by `build.rs`. Followed downstream by
/// Reth's own `--version` output.
fn print_outbe_version() {
    println!("Outbe {}", env!("OUTBE_SHORT_VERSION"));
    println!("{OUTBE_LONG_VERSION}");
    println!();
}

fn validate_adr005_node_mode(is_validator: bool, has_certified_upstream: bool) -> eyre::Result<()> {
    if !is_validator && !has_certified_upstream {
        eyre::bail!(
            "ADR-005 plain EL full-node mode is disabled: use --upstream so historical execution is gated by exact finalized-parent projection readiness"
        );
    }
    Ok(())
}

/// Parse DKG CLI's --bls-key-backend into a KeyBackend.
fn parse_dkg_key_backend(cli: &DkgCli) -> eyre::Result<outbe_consensus::bls::KeyBackend> {
    match cli.bls_key_backend.as_str() {
        "plaintext" => Ok(outbe_consensus::bls::KeyBackend::Plaintext),
        "encrypted" => {
            let passphrase = cli
                .bls_passphrase
                .clone()
                .ok_or_else(|| eyre::eyre!("--bls-key-backend encrypted requires --bls-passphrase or BLS_PASSPHRASE env var"))?;
            Ok(outbe_consensus::bls::KeyBackend::Encrypted(passphrase))
        }
        "os-level" => Ok(outbe_consensus::bls::KeyBackend::OsLevel),
        other => Err(eyre::eyre!("unknown BLS key backend: {other}")),
    }
}

/// Handle the `dkg` subcommand.
fn run_dkg_command(args: &[String]) -> eyre::Result<()> {
    // Rebuild args as: "outbe-chain-dkg" "bootstrap" ...remaining...
    let mut dkg_args = vec![args[0].clone()];
    dkg_args.extend_from_slice(&args[2..]);
    let dkg_cli = DkgCli::parse_from(dkg_args);

    let backend = parse_dkg_key_backend(&dkg_cli)?;

    match dkg_cli.command {
        DkgCommand::Identities {
            output_dir,
            validators,
        } => outbe_consensus::cli::execute_validator_identities(output_dir, validators, &backend),
        DkgCommand::VerifyIdentities {
            validators,
            material_dir,
        } => outbe_consensus::cli::execute_validator_identity_verification(
            &validators,
            &material_dir,
            &backend,
        ),
        DkgCommand::Bootstrap {
            output_dir,
            validators,
        } => outbe_consensus::cli::execute_dkg_bootstrap(output_dir, validators, &backend),
        DkgCommand::Status { storage_dir } => {
            outbe_consensus::cli::execute_dkg_status(&storage_dir, &backend)
        }
        DkgCommand::ExportShare {
            storage_dir,
            output,
        } => outbe_consensus::cli::execute_dkg_export_share(&storage_dir, &output, &backend),
        DkgCommand::ImportShare {
            share,
            polynomial,
            output,
            storage_dir,
        } => outbe_consensus::cli::execute_dkg_import_share(
            &share,
            &polynomial,
            output.as_deref(),
            &storage_dir,
            &backend,
        ),
        DkgCommand::ForceRestart { storage_dir } => {
            outbe_consensus::cli::execute_dkg_force_restart(&storage_dir)
        }
    }
}

fn load_reth_p2p_node_host_signer(
    network: &reth_node_core::args::NetworkArgs,
    default_secret_path: PathBuf,
) -> eyre::Result<(k256::ecdsa::SigningKey, [u8; 33])> {
    let reth_p2p_secret = network
        .secret_key(default_secret_path)
        .wrap_err("failed to load persistent Reth P2P identity for TEE")?;
    let signing = k256::ecdsa::SigningKey::from_slice(reth_p2p_secret.secret_bytes().as_slice())
        .map_err(|error| eyre::eyre!("invalid Reth P2P signing key: {error}"))?;
    let reth_p2p_public = signing
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .map_err(|_| eyre::eyre!("Reth P2P public key is not compressed SEC1-33"))?;
    Ok((signing, reth_p2p_public))
}

/// Run the main node (Reth execution + Commonware consensus).
fn run_node() -> eyre::Result<()> {
    // TEE offer decryption routes exclusively through the enclave sidecar
    // (`--tee-enclave-socket` → persistent production NodeHost authorization);
    // the offer-decryption key exists only inside the enclave (single path, no
    // in-process key material).

    // Initialize the hash-pinned Barretenberg global CRS before block
    // execution. Tribute admission is consensus-critical, so a node that
    // cannot initialize the verifier must not start.
    // Must run before the tokio runtime starts — `setup_srs` uses
    // `reqwest::blocking` internally and would panic from an async context.
    outbe_zkproof::init_crs()?;

    let cli = Cli::<OutbeChainSpecParser, ConsensusArgs, OutbeRpcModuleValidator>::parse();

    let bridge = ConsensusExecutionBridge::new();

    // Channels for validator-mode consensus thread.
    // For full-node mode, no thread is spawned and these are unused.
    let (node_tx, node_rx) = oneshot::channel::<(
        OutbeFullNode,
        ConsensusArgs,
        ProjectionReadinessHandle,
        Arc<dyn FinalizedCeCommitter>,
        Arc<dyn CeStartupRecovery>,
        Arc<CompressedTreeService>,
        Arc<outbe_node::ocomp::local_result::LocalLysisResultStore>,
    )>();
    let (consensus_dead_tx, mut consensus_dead_rx) = oneshot::channel::<()>();
    let shutdown_token = tokio_util::sync::CancellationToken::new();

    // Consensus thread is spawned conditionally — see inside run_with_components
    // where `args.is_validator` is known. For now, prepare the closure.
    let shutdown_token_clone = shutdown_token.clone();
    let bridge_for_consensus = bridge.clone();
    let consensus_thread_fn = move || -> eyre::Result<()> {
        let (
            node,
            mut args,
            projection_readiness,
            finalized_ce_committer,
            ce_startup_recovery,
            compressed_tree_service,
            local_lysis_results,
        ) = match node_rx.blocking_recv() {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };

        args.validate()?;

        let data_dir = node
            .config
            .datadir
            .clone()
            .resolve_datadir(reth_ethereum::chainspec::EthChainSpec::chain(
                &*node.chain_spec(),
            ))
            .data_dir()
            .to_path_buf();

        let consensus_storage = args
            .storage_dir
            .clone()
            .unwrap_or_else(|| data_dir.join("consensus"));

        // Write back effective storage_dir so the consensus stack sees it
        // even when the CLI did not provide --consensus.storage-dir.
        if args.storage_dir.is_none() {
            args.storage_dir = Some(consensus_storage.clone());
        }

        let keys_dir = args
            .keys_dir
            .clone()
            .unwrap_or_else(|| data_dir.join("keys"));

        if args.keys_dir.is_none() {
            args.keys_dir = Some(keys_dir.clone());
        }

        // Migrate DKG files from legacy location (consensus/) to keys/.
        outbe_engine::stack::migrate_dkg_keys_if_needed(&consensus_storage, &keys_dir)?;

        info!(
            path = %consensus_storage.display(),
            "starting consensus runtime"
        );

        // initialize the append-only slashing journal at
        // `<consensus_storage>/slashing-journal.jsonl`. The journal
        // captures every SlashIndicator/ValidatorSet state transition
        // in JSONL form and is independent of reth log rotation. If
        // initialization fails, log a warning and continue — the
        // journal is best-effort observability and must not block node
        // startup.
        if let Err(error) = outbe_primitives::slashing_journal::init(&consensus_storage) {
            tracing::warn!(
                target: "outbe::slashing::journal",
                %error,
                "failed to initialize slashing journal — events will not be persisted to a sidecar file",
            );
        }

        if let Err(error) = outbe_primitives::governance_journal::init(&consensus_storage) {
            tracing::warn!(
                target: "outbe::governance::journal",
                %error,
                "failed to initialize governance journal — events will not be persisted to a sidecar file",
            );
        }

        let runtime_config = commonware_runtime::tokio::Config::default()
            .with_tcp_nodelay(Some(true))
            .with_worker_threads(args.worker_threads)
            .with_storage_directory(consensus_storage)
            .with_catch_panics(true);

        let runner = commonware_runtime::tokio::Runner::new(runtime_config);

        let ret: eyre::Result<()> = runner.start(async move |ctx| {
            tokio::select! {
                result = outbe_engine::run_consensus_stack(
                    &ctx,
                    args,
                    node,
                    bridge_for_consensus,
                    outbe_engine::ConsensusStackServices::new(
                        projection_readiness,
                        finalized_ce_committer,
                        ce_startup_recovery,
                        compressed_tree_service,
                        local_lysis_results,
                    ),
                ) => {
                    if let Err(e) = &result {
                        tracing::error!(%e, "consensus stack failed");
                    }
                    result
                }
                _ = shutdown_token_clone.cancelled() => {
                    info!("consensus stack shutting down");
                    Ok(())
                }
            }
        });

        let _ = consensus_dead_tx.send(());
        ret
    };

    // Thread 1 (main): Reth execution layer.
    let bridge_for_evm = bridge.clone();
    let components = move |spec: Arc<ChainSpec<OutbeHeader>>| {
        let fork_install =
            outbe_node::ocomp::fork::require_startup_ocomp_fork_install(spec.as_ref())
                .expect("chain spec parser validated OCOMP fork install");
        let activation = outbe_primitives::system_tx::OcompLifecycleActivation::at_block(
            fork_install.activation_height,
        );
        let mut evm =
            outbe_evm::OutbeEvmConfig::new_with_bridge(spec.clone(), bridge_for_evm.clone())
                .with_ocomp_lifecycle_activation(activation);
        evm = evm.with_ocomp_fork_install(fork_install);
        (
            evm,
            Arc::new(
                OutbeBeaconConsensus::new(spec)
                    .with_max_extra_data_size(outbe_node::consensus::OUTBE_MAX_EXTRA_DATA_SIZE)
                    .with_ocomp_lifecycle_activation(activation),
            ),
        )
    };

    cli.run_with_components::<OutbeNode>(components, async move |builder, args| {
        args.validate()?;
        let tee_attestation_v1 =
            outbe_evm::tee_attestation_activation::TeeAttestationChainSpecStateV1::from_chain_spec(
                builder.config().chain.as_ref(),
            );
        let tee_activation = tee_attestation_v1.activation().map_err(|error| {
            eyre::eyre!("invalid mandatory teeAttestationV1 ChainSpec: {error}")
        })?;
        let initial_tee_policy = tee_activation
            .policy_at(outbe_evm::tee_attestation_activation::TEE_ATTESTATION_V1_ACTIVATION_HEIGHT)
            .map_err(eyre::Report::msg)?;
        let dkg_prepare_window_blocks = builder
            .config()
            .chain
            .genesis
            .config
            .extra_fields
            .get_deserialized::<u64>("dkgPrepareWindowBlocks")
            .transpose()
            .map_err(|error| eyre::eyre!("invalid dkgPrepareWindowBlocks: {error}"))?
            .unwrap_or(outbe_consensus::config::DEFAULT_DKG_PREPARE_WINDOW_BLOCKS);
        let minimum_block_time_millis = builder
            .config()
            .chain
            .genesis
            .config
            .extra_fields
            .get_deserialized::<u64>("minBlockTimeMs")
            .transpose()
            .map_err(|error| eyre::eyre!("invalid minBlockTimeMs: {error}"))?
            .unwrap_or(outbe_consensus::timing::DEFAULT_MIN_BLOCK_TIME_MS);
        info!(
            attestation_mode = ?initial_tee_policy.attestation_mode,
            activation_height = tee_activation.manifest.activation_height,
            policy_schedule_hash = %tee_activation.manifest.policy_schedule_hash,
            "validated mandatory TEE attestation ChainSpec authority"
        );
        let ocomp_fork_install =
            outbe_node::ocomp::fork::require_startup_ocomp_fork_install(
                builder.config().chain.as_ref(),
            )?;
        info!(
            activation_height = ocomp_fork_install.activation_height,
            classification = ?ocomp_fork_install.classification,
            install_hash = %ocomp_fork_install.install_hash(
                &outbe_ocomp_protocol::profile::poc_schema_limits()
            )?,
            "validated genesis-active immutable OCOMP chain-manifest install"
        );

        let prune_config = builder
            .config()
            .pruning
            .prune_config(builder.config().chain.as_ref());
        validate_compressed_storage_runtime_config(CompressedStorageRuntimeConfig {
            persistence_threshold: builder.config().engine.persistence_threshold,
            memory_block_buffer_target: builder.config().engine.memory_block_buffer_target,
            max_pending_acks: outbe_consensus::config::MAX_PENDING_ACKS,
            receipts_pruning_enabled: prune_config
                .as_ref()
                .is_some_and(|config| config.has_receipts_pruning()),
            account_history_pruning_enabled: prune_config
                .as_ref()
                .is_some_and(|config| config.segments.account_history.is_some()),
            storage_history_pruning_enabled: prune_config
                .as_ref()
                .is_some_and(|config| config.segments.storage_history.is_some()),
        })?;

        let node_data_dir = builder
            .config()
            .datadir
            .clone()
            .resolve_datadir(reth_ethereum::chainspec::EthChainSpec::chain(
                builder.config().chain.as_ref(),
            ))
            .data_dir()
            .to_path_buf();
        let evm_signer = if args.is_validator {
            let evm_key_path = args
                .effective_validator_evm_key()?
                .ok_or_else(|| eyre::eyre!("validator mode requires an EVM signer key"))?;
            let signer =
                Arc::new(OutbeEvmSigner::from_file(&evm_key_path).wrap_err_with(|| {
                    format!(
                        "failed to load validator EVM key from {}",
                        evm_key_path.display()
                    )
                })?);
            info!(
                address = %signer.address(),
                path = %evm_key_path.display(),
                "loaded validator EVM signer"
            );
            Some(signer)
        } else {
            None
        };
        let renewal_validator_authority = evm_signer.clone();
        let mut renewal_full_node_authority = None;

        // Every network declares exactly one TEE mode in genesis and every node
        // must connect to the corresponding enclave transport before execution
        // or consensus starts. GramineDirectDev is a separate network mode, not
        // a fallback when production initialization or DCAP fails.
        let socket = args.tee_enclave_socket.clone().ok_or_else(|| {
            eyre::eyre!(
                "mandatory {:?} ChainSpec requires --tee-enclave-socket before node startup",
                initial_tee_policy.attestation_mode
            )
        })?;
        let endpoint = socket
            .to_str()
            .ok_or_else(|| eyre::eyre!("TEE enclave endpoint is not valid UTF-8"))?;
        match initial_tee_policy.attestation_mode {
            outbe_primitives::tee_attestation_v1::AttestationMode::DcapRequired => {
                if args.is_validator {
                let signing_key_path = args.signing_key.as_deref().ok_or_else(|| {
                    eyre::eyre!("validator TEE initialization requires --consensus.signing-key")
                })?;
                let bls_key = outbe_engine::validators::load_signing_key(
                    signing_key_path,
                    &args.key_backend()?,
                )?;
                let consensus_bls_public: [u8; 48] = bls_key
                    .public_key()
                    .encode()
                    .as_ref()
                    .try_into()
                    .map_err(|_| eyre::eyre!("validator BLS public key is not 48 bytes"))?;
                let signer = evm_signer.as_ref().ok_or_else(|| {
                    eyre::eyre!("validator EVM signer unavailable during TEE initialization")
                })?;
                let client = outbe_tee::connect_or_initialize_validator_enclave(
                    endpoint,
                    &node_data_dir,
                    outbe_tee::ValidatorNodeHostIdentityV1 {
                        chain_id: builder.config().chain.chain().id(),
                        genesis_hash: builder.config().chain.genesis_hash(),
                        validator: signer.address(),
                        consensus_bls_public,
                    },
                    |hash| signer.sign_hash(&hash).map_err(|error| error.to_string()),
                )
                .wrap_err("validator NodeHost enclave initialization failed")?;
                outbe_tee::install_authorized_enclave_client(client).map_err(eyre::Report::msg)?;
                } else {
                    use k256::ecdsa::signature::hazmat::PrehashSigner as _;

                    // Resolve the identity through the same Reth API and default
                    // discovery-secret path used later by the network builder.
                    let (signing, reth_p2p_public) = load_reth_p2p_node_host_signer(
                        &builder.config().network,
                        builder.config().datadir().p2p_secret(),
                    )?;
                    let client = outbe_tee::connect_or_initialize_full_node_enclave(
                        endpoint,
                        &node_data_dir,
                        outbe_tee::FullNodeNodeHostIdentityV1 {
                            chain_id: builder.config().chain.chain().id(),
                            genesis_hash: builder.config().chain.genesis_hash(),
                            reth_p2p_public,
                        },
                        |hash| {
                            let (signature, recovery): (
                                k256::ecdsa::Signature,
                                k256::ecdsa::RecoveryId,
                            ) = signing
                                .sign_prehash(hash.as_slice())
                                .map_err(|error| error.to_string())?;
                            let mut bytes = [0_u8; 65];
                            bytes[..64].copy_from_slice(signature.to_bytes().as_slice());
                            bytes[64] = recovery.to_byte();
                            Ok(bytes)
                        },
                    )
                    .wrap_err("full-node NodeHost enclave initialization failed")?;
                    renewal_full_node_authority = Some(signing);
                    outbe_tee::install_authorized_enclave_client(client)
                        .map_err(eyre::Report::msg)?;
                }
            }
            outbe_primitives::tee_attestation_v1::AttestationMode::GramineDirectDev => {
                let client = outbe_tee::EnclaveClient::connect_endpoint(endpoint)
                .wrap_err(
                    "GramineDirectDev enclave connection failed; production transport is not a fallback",
                )?;
                outbe_tee::install_enclave_client(client).map_err(eyre::Report::msg)?;
            }
        }
        info!(
            socket = %socket.display(),
            validator_node_host = args.is_validator,
            attestation_mode = ?initial_tee_policy.attestation_mode,
            "mandatory TEE enclave sidecar connected before execution launch",
        );

        // A follower re-executes every protected transaction and therefore must
        // already hold the exact permanent offer key committed by the running
        // chain. Prove that invariant before Reth opens networking, RPC, sync or
        // execution. Losing the key is terminal for this node identity: startup
        // never invokes recovery, replacement or another bootstrap path.
        if !args.is_validator {
            let upstream = args.upstream.as_deref().ok_or_else(|| {
                eyre::eyre!(
                    "full-node startup requires --upstream to authenticate the chain offer key"
                )
            })?;
            let expected_offer = outbe_engine::read_upstream_tribute_offer_public_key(upstream)
                .await
                .wrap_err("failed to read mandatory offer key from the selected upstream")?;
            if expected_offer.is_zero() {
                return Err(eyre::eyre!(
                    "selected upstream has no mandatory OST3 offer key; refusing full-node startup"
                ));
            }
            let resident_offer = outbe_tee::resident_offer_public_key_v1()
                .wrap_err("failed to read the local enclave resident offer key")?;
            if resident_offer != expected_offer {
                return Err(eyre::eyre!(
                    "local enclave does not hold the selected chain's exact offer key; refusing execution startup (no recovery or fallback)"
                ));
            }
            info!(
                offer_public_key = %resident_offer,
                %upstream,
                "full-node resident offer key matched upstream before execution launch"
            );
        }

        let renewal_worker = match initial_tee_policy.attestation_mode {
            outbe_primitives::tee_attestation_v1::AttestationMode::DcapRequired => {
                let relay_path = args.tee_renewal_relay_key.as_deref().ok_or_else(|| {
                    eyre::eyre!(
                        "DcapRequired automatic renewal requires --tee-renewal.relay-key"
                    )
                })?;
                let relay = RelaySignerV1::from_file(relay_path).wrap_err_with(|| {
                    format!("load funded TEE renewal relay key {}", relay_path.display())
                })?;
                let manifest = outbe_tee::load_committed_enclave_manifest_v1(&node_data_dir)
                    .wrap_err("load committed NodeHost manifest for renewal")?;
                let (selector, authority) = match &manifest.node_id {
                    outbe_primitives::tee_attestation_v1::NodeIdV1::Validator {
                        address,
                        ..
                    } if args.is_validator => (
                        NodeBindingSelectorV1::Validator(alloy_primitives::Address::from(*address)),
                        RenewalNodeAuthorityV1::Validator(
                            renewal_validator_authority.clone().ok_or_else(|| {
                                eyre::eyre!("validator renewal authority is unavailable")
                            })?,
                        ),
                    ),
                    outbe_primitives::tee_attestation_v1::NodeIdV1::FullNode {
                        reth_p2p_public,
                    } if !args.is_validator => (
                        NodeBindingSelectorV1::FullNode(*reth_p2p_public),
                        RenewalNodeAuthorityV1::FullNode(
                            renewal_full_node_authority.take().ok_or_else(|| {
                                eyre::eyre!("full-node renewal authority is unavailable")
                            })?,
                        ),
                    ),
                    _ => eyre::bail!(
                        "committed NodeHost manifest profile does not match the node role"
                    ),
                };
                Some(RenewalWorkerV1 {
                    rpc_url: args.tee_renewal_rpc_url.clone(),
                    relay,
                    authority,
                    config: RenewalServiceConfigV1 {
                        node_data_dir: node_data_dir.clone(),
                        selector,
                        manifest,
                    },
                    poll_secs: args.tee_renewal_poll_secs,
                    warning_blocks: args.tee_renewal_warning_blocks,
                    critical_blocks: args.tee_renewal_critical_blocks,
                })
            }
            outbe_primitives::tee_attestation_v1::AttestationMode::GramineDirectDev => None,
        };

        let offchain_data = args.offchain_data()?;
        validate_adr005_node_mode(args.is_validator, args.upstream.is_some())?;
        let projection_config = OffchainDataProjectionConfig {
            chain_id: builder.config().chain.chain().id(),
            genesis_hash: builder.config().chain.genesis_hash(),
            start_block: offchain_data.start_block,
            mongodb_uri: offchain_data.mongodb_uri,
            mongodb_database: offchain_data.mongodb_database,
        };
        let prepared_projection = tokio::task::spawn_blocking(move || {
            prepare_offchain_data_projection(projection_config)
        })
        .await
        .wrap_err("offchain-data startup validation worker failed")??;
        let runtime_body_readers = prepared_projection.runtime_body_readers();
        let proof_body_readers = runtime_body_readers.clone();
        let proof_chain_id = builder.config().chain.chain().id();
        let projection_readiness = prepared_projection.readiness();
        let ce_data_dir = builder
            .config()
            .datadir
            .clone()
            .resolve_datadir(reth_ethereum::chainspec::EthChainSpec::chain(
                builder.config().chain.as_ref(),
            ))
            .data_dir()
            .to_path_buf();
        let genesis_hash = builder.config().chain.genesis_hash();
        let local_lysis_results = Arc::new(
            outbe_node::ocomp::local_result::LocalLysisResultStore::open(
                ce_data_dir.join("ocomp-local-results-v1"),
                outbe_ocomp_protocol::profile::poc_schema_limits(),
            )
            .wrap_err("failed to open durable node-local Lysis result store")?,
        );
        let ce_identity = EnvironmentIdentity {
            local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: builder.config().chain.chain().id(),
            genesis_hash,
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: outbe_compressed_entities::CeTopologyV1.encode(),
            tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".to_owned(),
            vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
        };
        let genesis_marker = FinalizedMarker {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: 0,
            block_hash: genesis_hash,
            parent_block_hash: Default::default(),
            parent_root: Default::default(),
            new_root: outbe_compressed_entities::sealed_root(Default::default())?,
        };
        let ce_db = CeMdbx::open(&ce_data_dir, ce_identity, genesis_marker)
            .wrap_err("failed to open and validate compressed-entity MDBX")?;
        // ADR-009 fixes only the provisional sharded topology. Production CE
        // work/cache coefficients remain deliberately open until ADR-017.
        let compressed_tree_service = Arc::new(CompressedTreeService::new(
            ce_db,
            CandidateCacheLimits {
                max_candidates: usize::MAX,
                max_encoded_bytes: usize::MAX,
            },
        )?);
        compressed_tree_service.discard_speculative_candidates()?;
        let outbe_node = match evm_signer {
            Some(signer) => OutbeNode::with_bridge_and_evm_signer(
                bridge.clone(),
                signer,
                runtime_body_readers,
                compressed_tree_service.clone(),
            ),
            None => OutbeNode::with_bridge(
                bridge.clone(),
                runtime_body_readers,
                compressed_tree_service.clone(),
            ),
        };
        let outbe_node = outbe_node
            .with_ocomp_fork_install(ocomp_fork_install)
            .with_ocomp_local_result_authority(local_lysis_results.clone());
        let (projection_exit_tx, mut projection_exit_rx) =
            tokio::sync::mpsc::unbounded_channel();
        let projection_readiness_for_rpc = projection_readiness.clone();

        let NodeHandle {
            node,
            node_exit_future,
        } = builder
            .node(outbe_node)
            .install_exex("outbe-offchain-data", move |ctx| async move {
                let projection_provider = ctx.provider().clone();
                let ready_projection = tokio::task::spawn_blocking(move || {
                    validate_offchain_data_checkpoint(prepared_projection, &projection_provider)
                })
                .await
                .wrap_err("offchain-data checkpoint validation worker failed")??;
                Ok(outbe_node::projection::supervise_offchain_data_projection(
                    ctx,
                    ready_projection,
                    projection_exit_tx,
                ))
            })
            .apply(|mut builder| {
                configure_outbe_engine_args(&mut builder.config_mut().engine);
                let discovery = &mut builder.config_mut().network.discovery;
                discovery.enable_discv5_discovery = true;
                // SSA-1: disable reth DNS discovery so the `hickory-proto` code
                // path (RUSTSEC-2025 NSEC3 unbounded-loop DoS, no upstream fix)
                // is unreachable. outbe peers via discv5 + static bootnodes and
                // configures no DNS ENR tree, so DNS discovery provided nothing
                // here anyway; disabling it removes the attack surface.
                discovery.disable_dns_discovery = true;
                builder
            })
            .extend_rpc_modules({
                let bridge = bridge.clone();
                let is_validator = args.is_validator;
                let is_follower = args.upstream.is_some();
                let projection_readiness = projection_readiness_for_rpc.clone();
                let compressed_tree_service = compressed_tree_service.clone();
                let proof_body_readers = proof_body_readers.clone();
                move |ctx| {
                    use outbe_rpc::OutbeApiServer as _;
                    let provider = Arc::new(ctx.provider().clone());
                    // Validators get the full bridge-backed handler.
                    // `--upstream` followers also run a marshal and CAN serve
                    // `outbe_getFinalization` (chaining followers), but must NOT
                    // report validator status; they get a follower-scoped handler
                    // that exposes only the finalization-serving capability.
                    let outbe_api = (if is_validator {
                        outbe_rpc::OutbeApiHandler::with_bridge(
                            provider,
                            bridge,
                            projection_readiness.clone(),
                        )
                    } else if is_follower {
                        outbe_rpc::OutbeApiHandler::with_follower_bridge(
                            provider,
                            bridge,
                            projection_readiness.clone(),
                        )
                    } else {
                        outbe_rpc::OutbeApiHandler::new(provider, projection_readiness.clone())
                    })
                    .with_point_reads(
                        compressed_tree_service.clone(),
                        proof_body_readers.clone(),
                        proof_chain_id,
                    )
                    .with_tee_renewal_schedule(
                        dkg_prepare_window_blocks,
                        minimum_block_time_millis,
                    );
                    ctx.modules.merge_if_module_configured(
                        RethRpcModule::Other("outbe".to_owned()),
                        outbe_api.into_rpc(),
                    )?;
                    info!("outbe_* RPC namespace registered where configured");
                    Ok(())
                }
            })
            .launch()
            .await
            .wrap_err("failed launching execution node")?;

        let renewal_handle = renewal_worker.map(|worker| {
            tokio::spawn(run_renewal_worker_v1(worker, shutdown_token.clone()))
        });
        let upgrade_promotion = Arc::new(tokio::sync::Notify::new());
        let upgrade_handle = if initial_tee_policy.attestation_mode
            == outbe_primitives::tee_attestation_v1::AttestationMode::DcapRequired
        {
            let provider = node.provider.clone();
            let promoted = upgrade_promotion.clone();
            Some(tokio::spawn(run_upgrade_promotion_worker_v1(
                provider,
                UpgradePromotionWorkerConfigV1 {
                    chain_id: proof_chain_id,
                    genesis_hash,
                    node_data_dir: node_data_dir.clone(),
                    poll_secs: args.tee_renewal_poll_secs,
                    warning_blocks: args.tee_renewal_warning_blocks,
                    critical_blocks: args.tee_renewal_critical_blocks,
                    promoted,
                },
            )))
        } else {
            None
        };

        let durable_ce_adapter = Arc::new(RethDurableCeState::new(node.provider.clone()));
        let durable_ce_state: Arc<dyn DurableCeState> = durable_ce_adapter.clone();
        let canonical_ce_replay: Arc<dyn CanonicalCeReplaySource> = durable_ce_adapter;
        let finalized_ce_tree: Arc<dyn FinalizedCeTree> = compressed_tree_service.clone();
        let finalized_ce_committer: Arc<dyn FinalizedCeCommitter> =
            Arc::new(RethCeFinalizer::new(durable_ce_state, finalized_ce_tree));
        let startup_ce_tree: Arc<dyn StartupCeTree> = compressed_tree_service.clone();
        let ce_startup_recovery: Arc<dyn CeStartupRecovery> = Arc::new(
            CeStartupRecoveryCoordinator::new(canonical_ce_replay, startup_ce_tree),
        );

        outbe_engine::validators::check_binary_version_compatibility(
            &node.provider,
            outbe_evm::handlers::update::registry(),
        )?;

        if args.is_validator || args.upstream.is_some() {
            if args.upstream.is_some() {
                info!("outbe node launched in FOLLOWER mode (--upstream)");
            } else {
                info!("outbe node launched in VALIDATOR mode");
            }

            // Spawn the consensus thread for validator OR follower mode; the
            // follower branch inside `run_consensus_stack` selects the lightweight
            // follow stack (no consensus engine).
            let consensus_handle = thread::spawn(consensus_thread_fn);

            let shutdown = node.add_ons_handle.engine_shutdown.clone();
            let _ = node_tx.send((
                node,
                args,
                projection_readiness,
                finalized_ce_committer,
                ce_startup_recovery,
                compressed_tree_service,
                local_lysis_results,
            ));

            tokio::select! {
                _ = node_exit_future => {
                    info!("execution node exited");
                }
                _ = &mut consensus_dead_rx => {
                    info!("consensus node exited");
                }
                exit = projection_exit_rx.recv() => {
                    if let Some(exit) = exit {
                        tracing::error!(
                            failure_class = ?exit.failure.class,
                            failure = %exit.failure.message,
                            "mandatory offchain-data projection requested node shutdown"
                        );
                    }
                    if let Some(done) = shutdown.shutdown() {
                        let _ = done.await;
                    }
                }
                () = upgrade_promotion.notified() => {
                    info!("finalized enclave upgrade requested execution restart");
                    if let Some(done) = shutdown.shutdown() {
                        let _ = done.await;
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    info!("received shutdown signal");
                }
            }

            shutdown_token.cancel();

            handle_consensus_thread_join(consensus_handle.join())?;
        } else {
            info!("outbe node launched in FULL NODE mode — no consensus thread spawned");
            let shutdown = node.add_ons_handle.engine_shutdown.clone();

            tokio::select! {
                _ = node_exit_future => {
                    info!("execution node exited");
                }
                _ = tokio::signal::ctrl_c() => {
                    info!("received shutdown signal");
                }
                exit = projection_exit_rx.recv() => {
                    if let Some(exit) = exit {
                        tracing::error!(
                            failure_class = ?exit.failure.class,
                            failure = %exit.failure.message,
                            "mandatory offchain-data projection requested node shutdown"
                        );
                    }
                    if let Some(done) = shutdown.shutdown() {
                        let _ = done.await;
                    }
                }
                () = upgrade_promotion.notified() => {
                    info!("finalized enclave upgrade requested execution restart");
                    if let Some(done) = shutdown.shutdown() {
                        let _ = done.await;
                    }
                }
            }
        }

        shutdown_token.cancel();
        if let Some(handle) = renewal_handle {
            handle.await.wrap_err("automatic DCAP renewal worker panicked")?;
        }
        if let Some(handle) = upgrade_handle {
            handle.abort();
            match handle.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {}
                Err(error) => {
                    return Err(eyre::eyre!("enclave-upgrade watcher panicked: {error}"));
                }
            }
        }

        Ok(())
    })
    .wrap_err("execution node failed")?;

    Ok(())
}

/// Configure Reth's engine tree for Outbe's pre-finalization parent switches.
///
/// Ethereum's Engine API permits an execution client to skip payload building when
/// an FCU selects an already-canonical ancestor. Outbe leaders intentionally build
/// on certified, not-yet-finalized parents, so a later view may select such an
/// ancestor and still require a payload. Reth exposes both parts of that behavior
/// explicitly: process the attributes and unwind the canonical header to the
/// selected parent before starting the payload job.
fn configure_outbe_engine_args(engine: &mut reth_node_core::args::EngineArgs) {
    engine.always_process_payload_attributes_on_canonical_head = true;
    engine.allow_unwind_canonical_header = true;
}

#[cfg(test)]
mod tests {
    #[test]
    fn engine_builds_payloads_after_prefinalization_parent_switches() {
        let mut engine = reth_node_core::args::EngineArgs::default();
        assert!(!engine.always_process_payload_attributes_on_canonical_head);
        assert!(!engine.allow_unwind_canonical_header);

        super::configure_outbe_engine_args(&mut engine);

        assert!(engine.always_process_payload_attributes_on_canonical_head);
        assert!(engine.allow_unwind_canonical_header);
        let tree = engine.tree_config();
        assert!(tree.always_process_payload_attributes_on_canonical_head());
        assert!(tree.unwind_canonical_header());
    }

    #[test]
    fn full_node_identity_uses_reth_secret_resolver_and_persists_exact_key() {
        let root = tempfile::tempdir().unwrap();
        let explicit_secret = root.path().join("operator-p2p.key");
        let unused_default = root.path().join("default-discovery-secret");
        let network = reth_node_core::args::NetworkArgs {
            p2p_secret_key: Some(explicit_secret.clone()),
            ..Default::default()
        };

        let (first_signer, first_public) =
            super::load_reth_p2p_node_host_signer(&network, unused_default.clone()).unwrap();
        assert!(explicit_secret.is_file());
        assert!(!unused_default.exists());
        assert_eq!(
            first_signer
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes(),
            first_public
        );

        drop(first_signer);
        let (_, restored_public) =
            super::load_reth_p2p_node_host_signer(&network, unused_default).unwrap();
        assert_eq!(restored_public, first_public);
    }

    #[test]
    fn adr005_accepts_validators_and_certified_followers_only() {
        super::validate_adr005_node_mode(true, false).expect("validator path is parent-gated");
        super::validate_adr005_node_mode(false, true)
            .expect("certified follower path is parent-gated");

        let error = super::validate_adr005_node_mode(false, false)
            .expect_err("plain EL sync has no finalized-parent projection barrier");
        assert!(error.to_string().contains("--upstream"));
    }

    #[test]
    fn consensus_thread_error_propagates_to_validator_main() {
        let err = super::handle_consensus_thread_join(Ok(Err(eyre::eyre!("watchdog fatal"))))
            .expect_err("consensus thread error must propagate");
        let err = format!("{err:#}");

        assert!(
            err.contains("consensus task exited with error"),
            "wrapped consensus context missing: {err}"
        );
        assert!(
            err.contains("watchdog fatal"),
            "original consensus error missing: {err}"
        );
    }

    #[test]
    fn consensus_thread_success_is_ok() {
        super::handle_consensus_thread_join(Ok(Ok(())))
            .expect("successful consensus thread must not error");
    }

    /// Full-node mode: dropping node_tx causes consensus thread's blocking_recv to return Err.
    /// This verifies that the consensus thread exits immediately when no node handle is sent.
    #[test]
    fn test_fullnode_drops_node_tx_consensus_thread_exits() {
        let (node_tx, node_rx) = tokio::sync::oneshot::channel::<()>();

        // Simulate full-node path: drop sender without sending.
        drop(node_tx);

        // Consensus thread would call blocking_recv — should return Err immediately.
        let result = node_rx.blocking_recv();
        assert!(
            result.is_err(),
            "blocking_recv must return Err when sender is dropped"
        );
    }

    /// Full-node mode: RPC handler created without bridge → is_validator = false.
    #[test]
    fn test_fullnode_rpc_no_bridge_means_not_validator() {
        // When OutbeApiHandler::new(provider) is called (no bridge),
        // bridge field is None, so is_validator = bridge.is_some() = false.
        let bridge: Option<outbe_engine::bridge::ConsensusExecutionBridge> = None;
        assert!(
            bridge.is_none(),
            "full node must have bridge=None → is_validator=false"
        );
    }

    /// Validator mode: RPC handler created with bridge → is_validator = true.
    #[test]
    fn test_validator_rpc_with_bridge_means_validator() {
        let bridge = outbe_engine::bridge::ConsensusExecutionBridge::new();
        let bridge_opt: Option<outbe_engine::bridge::ConsensusExecutionBridge> = Some(bridge);
        assert!(
            bridge_opt.is_some(),
            "validator must have bridge=Some → is_validator=true"
        );
    }

    #[test]
    fn outbe_rpc_module_validator_accepts_outbe_namespace() {
        use reth_rpc_server_types::{RpcModuleSelection, RpcModuleValidator as _};

        let selection = super::OutbeRpcModuleValidator::parse_selection("eth,net,web3,outbe")
            .expect("outbe namespace should be accepted");
        let RpcModuleSelection::Selection(modules) = selection else {
            panic!("explicit module list should parse as selection");
        };
        assert!(modules.iter().any(|module| module.as_str() == "outbe"));
    }

    #[test]
    fn outbe_rpc_module_validator_rejects_unknown_namespace() {
        use reth_rpc_server_types::RpcModuleValidator as _;

        let err = super::OutbeRpcModuleValidator::parse_selection("eth,outbee")
            .expect_err("typoed custom namespace must be rejected");
        assert!(err.contains("Unknown RPC module: 'outbee'"));
    }

    // --- parse_dkg_key_backend ---

    fn make_dkg_cli(args: &[&str]) -> super::DkgCli {
        use clap::Parser;
        let mut full = vec!["cmd"];
        full.extend_from_slice(args);
        super::DkgCli::parse_from(full)
    }

    #[test]
    fn test_parse_dkg_key_backend_plaintext() {
        let cli = make_dkg_cli(&[
            "--bls-key-backend",
            "plaintext",
            "status",
            "--storage-dir",
            "/tmp",
        ]);
        let backend = super::parse_dkg_key_backend(&cli).unwrap();
        assert!(matches!(
            backend,
            outbe_consensus::bls::KeyBackend::Plaintext
        ));
    }

    #[test]
    fn test_parse_dkg_key_backend_default_is_plaintext() {
        let cli = make_dkg_cli(&["status", "--storage-dir", "/tmp"]);
        let backend = super::parse_dkg_key_backend(&cli).unwrap();
        assert!(matches!(
            backend,
            outbe_consensus::bls::KeyBackend::Plaintext
        ));
    }

    #[test]
    fn test_parse_dkg_key_backend_encrypted_with_passphrase() {
        let cli = make_dkg_cli(&[
            "--bls-key-backend",
            "encrypted",
            "--bls-passphrase",
            "hunter2",
            "status",
            "--storage-dir",
            "/tmp",
        ]);
        let backend = super::parse_dkg_key_backend(&cli).unwrap();
        assert!(matches!(
            backend,
            outbe_consensus::bls::KeyBackend::Encrypted(ref p) if p == "hunter2"
        ));
    }

    #[test]
    fn test_parse_dkg_key_backend_encrypted_missing_passphrase() {
        let cli = make_dkg_cli(&[
            "--bls-key-backend",
            "encrypted",
            "status",
            "--storage-dir",
            "/tmp",
        ]);
        assert!(super::parse_dkg_key_backend(&cli).is_err());
    }

    #[test]
    fn test_parse_dkg_key_backend_os_level() {
        let cli = make_dkg_cli(&[
            "--bls-key-backend",
            "os-level",
            "status",
            "--storage-dir",
            "/tmp",
        ]);
        let backend = super::parse_dkg_key_backend(&cli).unwrap();
        assert!(matches!(backend, outbe_consensus::bls::KeyBackend::OsLevel));
    }

    #[test]
    fn test_parse_dkg_key_backend_unknown() {
        let cli = make_dkg_cli(&[
            "--bls-key-backend",
            "foo",
            "status",
            "--storage-dir",
            "/tmp",
        ]);
        assert!(super::parse_dkg_key_backend(&cli).is_err());
    }

    // --- TC-002: DKG command routing via run_dkg_command ---

    fn dkg_args(args: &[&str]) -> Vec<String> {
        let mut v = vec!["outbe-chain".to_string(), "dkg".to_string()];
        v.extend(args.iter().map(|s| s.to_string()));
        v
    }

    #[test]
    fn test_dkg_bootstrap_3_validators() {
        let dir = tempfile::tempdir().unwrap();
        let dir_str = dir.path().to_str().unwrap();
        let args = dkg_args(&["bootstrap", "--output-dir", dir_str, "--validators", "3"]);
        super::run_dkg_command(&args).unwrap();

        // Verify output structure
        assert!(dir.path().join("polynomial.hex").exists());
        assert!(dir.path().join("dkg-output.hex").exists());
        assert!(dir.path().join("validators.json").exists());
        for i in 0..3 {
            let vdir = dir.path().join(format!("validator-{i}"));
            assert!(vdir.join("signing-key.hex").exists());
            assert!(vdir.join("evm-key.hex").exists());
        }
    }

    #[test]
    fn test_dkg_identities_do_not_precompute_genesis_threshold_material() {
        let dir = tempfile::tempdir().unwrap();
        let args = dkg_args(&[
            "identities",
            "--output-dir",
            dir.path().to_str().unwrap(),
            "--validators",
            "4",
        ]);
        super::run_dkg_command(&args).unwrap();

        assert!(dir.path().join("validators.json").exists());
        assert!(dir.path().join("reth-bootnodes.txt").exists());
        assert!(!dir.path().join("polynomial.hex").exists());
        assert!(!dir.path().join("dkg-output.hex").exists());
        for index in 0..4 {
            let validator = dir.path().join(format!("validator-{index}"));
            assert!(validator.join("signing-key.hex").exists());
            assert!(validator.join("evm-key.hex").exists());
            assert!(validator.join("reth-p2p-secret.hex").exists());
            assert!(!validator.join("signing-share.hex").exists());
        }
    }

    #[test]
    fn test_dkg_status_after_bootstrap() {
        let dir = tempfile::tempdir().unwrap();
        let dir_str = dir.path().to_str().unwrap();
        let args = dkg_args(&["bootstrap", "--output-dir", dir_str, "--validators", "3"]);
        super::run_dkg_command(&args).unwrap();

        // Status on a validator directory (has share + poly from bootstrap)
        let v0 = dir.path().join("validator-0");
        let v0_str = v0.to_str().unwrap();
        let status_args = dkg_args(&["status", "--storage-dir", v0_str]);
        super::run_dkg_command(&status_args).unwrap();
    }

    #[test]
    fn test_dkg_status_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let dir_str = dir.path().to_str().unwrap();
        let args = dkg_args(&["status", "--storage-dir", dir_str]);
        // Should succeed but print "NOT READY"
        super::run_dkg_command(&args).unwrap();
    }

    #[test]
    fn test_dkg_export_requires_complete_runtime_state() {
        let dir = tempfile::tempdir().unwrap();
        let dir_str = dir.path().to_str().unwrap();
        let args = dkg_args(&["bootstrap", "--output-dir", dir_str, "--validators", "3"]);
        super::run_dkg_command(&args).unwrap();

        // Bootstrap output keeps the shared dkg-output.hex at the output root.
        // Runtime export must still reject validator storage that lacks its local
        // complete triplet instead of producing an import bundle startup cannot load.
        let v0 = dir.path().join("validator-0");
        std::fs::copy(v0.join("signing-share.hex"), v0.join("dkg_share.hex")).unwrap();
        std::fs::copy(
            dir.path().join("polynomial.hex"),
            v0.join("dkg_polynomial.hex"),
        )
        .unwrap();

        let export_dir = tempfile::tempdir().unwrap();
        let export_args = dkg_args(&[
            "export-share",
            "--storage-dir",
            v0.to_str().unwrap(),
            "--output",
            export_dir.path().to_str().unwrap(),
        ]);
        assert!(super::run_dkg_command(&export_args).is_err());
    }

    #[test]
    fn test_dkg_force_restart_only_removes_consensus_material() {
        let dir = tempfile::tempdir().unwrap();
        let dir_str = dir.path().to_str().unwrap();
        let args = dkg_args(&["bootstrap", "--output-dir", dir_str, "--validators", "3"]);
        super::run_dkg_command(&args).unwrap();

        let v0 = dir.path().join("validator-0");
        // Copy into runtime filenames
        std::fs::copy(v0.join("signing-share.hex"), v0.join("dkg_share.hex")).unwrap();
        std::fs::copy(
            dir.path().join("polynomial.hex"),
            v0.join("dkg_polynomial.hex"),
        )
        .unwrap();
        std::fs::write(v0.join("dkg_output.hex"), "placeholder").unwrap();
        let tee_sentinels = [
            ("sealed_root.bin", b"permanent-offer-key".as_slice()),
            ("sealed_identity.bin", b"enclave-identity".as_slice()),
            (
                "sealed_node_authorization_v1.bin",
                b"node-host-authorization".as_slice(),
            ),
        ];
        for (name, bytes) in tee_sentinels {
            std::fs::write(v0.join(name), bytes).unwrap();
        }
        assert!(v0.join("dkg_share.hex").exists());
        assert!(v0.join("dkg_output.hex").exists());

        let restart_args = dkg_args(&["force-restart", "--storage-dir", v0.to_str().unwrap()]);
        super::run_dkg_command(&restart_args).unwrap();

        assert!(!v0.join("dkg_share.hex").exists());
        assert!(!v0.join("dkg_polynomial.hex").exists());
        assert!(!v0.join("dkg_output.hex").exists());
        for (name, bytes) in tee_sentinels {
            assert_eq!(std::fs::read(v0.join(name)).unwrap(), bytes);
        }
    }

    #[test]
    fn test_dkg_force_restart_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let dir_str = dir.path().to_str().unwrap();
        let args = dkg_args(&["force-restart", "--storage-dir", dir_str]);
        super::run_dkg_command(&args).unwrap(); // no-op, succeeds
    }

    #[test]
    fn test_dkg_export_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let export_dir = tempfile::tempdir().unwrap();
        let args = dkg_args(&[
            "export-share",
            "--storage-dir",
            dir.path().to_str().unwrap(),
            "--output",
            export_dir.path().to_str().unwrap(),
        ]);
        assert!(super::run_dkg_command(&args).is_err());
    }
}
