//! `outbe-cli tee` - V1 TEE registration for a joining validator or full node.
//!
//! Pre-start flow: before launching `outbe-chain node` on a
//! TEE-bootstrapped chain, the joiner registers its enclave on-chain
//! (`registerEnclave(bytes,bytes,bytes,bytes,bytes,bytes)`), reads the
//! deterministically sealed offer
//! key from its own transaction log (`OfferKeySealedForRegistryV1`), and installs
//! it in its enclave. Only
//! then can the node execute offer blocks. Mirrors `secretd tx register auth` +
//! `q register seed` + `configure-secret`, run before `secretd start`.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use clap::Subcommand;
use eyre::{Result, WrapErr};
use k256::ecdsa::signature::hazmat::PrehashSigner as _;
use outbe_operator::{
    rpc::{FinalityRpc, RenewalRpc},
    tee::{
        await_finalized_onboarding_v1, copy_same_platform_sealed_root_and_checkpoint_v1,
        inspect_upgrade_journal_v1, prepare_upgrade_journal_v1, read_finalized_registry_view_v1,
        read_finalized_staged_successor_policy_v1, read_renewal_status_v1, run_renewal_once_v1,
        run_upgrade_submission_v1, ExpectedOnboardingBindingV1, FinalizedRegistryChainViewV1,
        NodeBindingSelectorV1, RenewalBindingV1, RenewalOutcomeV1, RenewalServiceConfigV1,
        UpgradeContextV1,
    },
    tx::{buffered_gas_price, RelaySignerV1},
};
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, AttestationMode, AttestationOperationV1, DcapEvidenceV1,
    GramineDirectEvidenceV1, NodeIdV1, RegistrationIntentV1, RegistryMutatorV1, TeePolicyV1,
    TeeRegistryGasScheduleV1, ValidatorNodeBindingV1, ENCLAVE_ID_DOMAIN_V1,
};
use outbe_tee::protocol::{
    EnclaveRequest, EnclaveResponse, MAX_SEALED_OFFER_KEY_FOR_REGISTRY_BYTES,
    MIN_SEALED_OFFER_KEY_FOR_REGISTRY_BYTES,
};
use outbe_tee::{
    acquire_dcap_collateral_v1, clear_committed_join_checkpoint,
    connect_committed_node_host_enclave, construct_finalized_replacement_authorization_v1,
    load_committed_enclave_manifest_v1, load_committed_join_relay, load_committed_join_submission,
    load_replacement_candidate_relay, load_replacement_candidate_submission,
    persist_committed_join_relay, persist_committed_join_submission,
    persist_finalized_join_admission_anchor, persist_replacement_candidate_relay,
    persist_replacement_candidate_submission, promote_replacement_candidate,
    AuthorizedEnclaveClient, CommittedJoinSubmissionV1, EnclaveClient,
    FinalizedJoinAdmissionAnchorV1, FinalizedRegistryViewV1, FinalizedReplacementBindingV1,
    GeneratedDcapQuoteV1, NodeHostIdentityV1, ReplacementCandidateEnclaveV1,
    ReplacementCandidateSubmissionV1,
};
use zeroize::Zeroizing;

use crate::abi::{self, ITeeRegistry};
use crate::rpc::Rpc;

const DEV_NODE_HOST_DOMAIN_V1: &[u8] = b"outbe/tee/dev-node-host/v1";
const MANUAL_RENEWAL_FINALITY_TIMEOUT: Duration = Duration::from_secs(300);
const MANUAL_RENEWAL_RECONCILE_INTERVAL: Duration = Duration::from_secs(2);

struct CliFinalityRpc<'a, R>(&'a R);

impl<R: Rpc + Sync> FinalityRpc for CliFinalityRpc<'_, R> {
    async fn transaction_receipt(
        &self,
        transaction_hash: &str,
    ) -> Result<Option<serde_json::Value>> {
        self.0.eth_get_transaction_receipt(transaction_hash).await
    }

    async fn logs(
        &self,
        address: Address,
        topics: &[Option<String>],
        from_block: &str,
        to_block: &str,
    ) -> Result<Vec<serde_json::Value>> {
        self.0
            .eth_get_logs(address, topics, from_block, to_block)
            .await
    }

    async fn block_by_number(&self, block: u64) -> Result<serde_json::Value> {
        self.0.eth_get_block_by_number(block).await
    }

    async fn finalized_block(&self) -> Result<serde_json::Value> {
        self.0.eth_get_finalized_block().await
    }

    async fn call_at(&self, to: Address, data: &[u8], block_tag: &str) -> Result<Vec<u8>> {
        self.0.eth_call_at(to, data, block_tag).await
    }
}

impl<R: Rpc + Sync> RenewalRpc for CliFinalityRpc<'_, R> {
    async fn chain_id(&self) -> Result<u64> {
        self.0.eth_chain_id().await
    }

    async fn gas_price(&self) -> Result<U256> {
        self.0.eth_gas_price().await
    }

    async fn transaction_count(&self, address: Address) -> Result<u64> {
        self.0.eth_get_transaction_count(address).await
    }

    async fn balance(&self, address: Address) -> Result<U256> {
        self.0.eth_get_balance(address).await
    }

    async fn send_raw_transaction(&self, raw_transaction: &[u8]) -> Result<String> {
        self.0.eth_send_raw_transaction(raw_transaction).await
    }

    async fn tee_renewal_schedule_v1(
        &self,
    ) -> Result<outbe_primitives::tee_operator_v1::TeeRenewalScheduleV1> {
        self.0.outbe_tee_renewal_schedule_v1().await
    }
}

#[derive(Subcommand)]
pub enum TeeCmd {
    /// Register this node's enclave on-chain and install the offer key it is sealed.
    /// Run BEFORE `outbe-chain node` when joining a running TEE-bootstrapped chain.
    Join {
        /// Enclave sidecar endpoint: a UDS path or a `host:port` (Gramine) address.
        #[arg(long)]
        enclave_socket: String,
        /// Resolved chain-specific Reth data directory. Required by
        /// DcapRequired NodeHost initialization.
        #[arg(long)]
        node_data_dir: Option<PathBuf>,
        /// Persistent Reth P2P secp256k1 secret file.
        #[arg(long)]
        reth_p2p_secret_key: Option<PathBuf>,
        /// Fresh one-use nonzero 32-byte binding id. Keep it stable while
        /// tracking one submitted transaction.
        #[arg(long)]
        binding_id: String,
        /// Requested lease deadline as a consensus Unix timestamp.
        #[arg(long)]
        valid_until: u64,
        /// Seconds to wait for the matching on-chain
        /// `OfferKeySealedForRegistryV1` transaction event.
        #[arg(long, default_value_t = 60)]
        timeout_secs: u64,
    },
    /// Generate, durably journal, submit and reconcile one manual renewal.
    Renew {
        /// Production enclave sidecar endpoint.
        #[arg(long)]
        enclave_socket: String,
        /// Resolved chain-specific node data directory containing NodeHost state.
        #[arg(long)]
        node_data_dir: PathBuf,
        /// Persistent Reth P2P secret file.
        #[arg(long)]
        reth_p2p_secret_key: Option<PathBuf>,
    },
    /// Read finalized renewal/freeze facts and the local journal without
    /// creating or changing any lifecycle state.
    Status {
        /// Resolved chain-specific node data directory containing NodeHost state.
        #[arg(long)]
        node_data_dir: PathBuf,
        /// Emit warning once an unsafe lease is this many blocks from freeze.
        #[arg(long, default_value_t = 600)]
        warning_blocks: u64,
        /// Emit critical once an unsafe lease is this many blocks from freeze.
        #[arg(long, default_value_t = 120)]
        critical_blocks: u64,
    },
    /// Stage fresh candidate B and durably bind it to the finalized successor
    /// policy. This command does not copy the offer key or stop/start Gramine.
    UpgradePrepare {
        #[arg(long)]
        candidate_enclave_socket: String,
        #[arg(long)]
        node_data_dir: PathBuf,
        #[arg(long)]
        active_tee_dir: PathBuf,
        #[arg(long)]
        candidate_tee_dir: PathBuf,
        #[arg(long)]
        reth_p2p_secret_key: Option<PathBuf>,
    },
    /// Copy only MRSIGNER-sealed `sealed_root.bin` from A to B and fsync the
    /// checkpoint. Stop B before this command and restart B afterwards.
    UpgradeCopyRoot {
        #[arg(long)]
        node_data_dir: PathBuf,
    },
    /// Reconnect restarted B, prove its resident permanent offer key, durably
    /// prepare exact transition bytes and submit them through the global EVM signer.
    UpgradeSubmit {
        #[arg(long)]
        candidate_enclave_socket: String,
        #[arg(long)]
        node_data_dir: PathBuf,
        #[arg(long)]
        reth_p2p_secret_key: Option<PathBuf>,
        #[arg(long)]
        binding_id: String,
        #[arg(long)]
        valid_until: u64,
    },
    /// Print the durable same-platform upgrade checkpoint without changing it.
    UpgradeStatus {
        #[arg(long)]
        node_data_dir: PathBuf,
    },
    /// Print this enclave's resident tribute-offer public key (the key clients
    /// encrypt offers to once DKG completes) and its DKG identity key. With
    /// `--diff-chain`, also read the on-chain registry `tributeOfferPublicKey()`
    /// and assert it MATCHES the enclave - exits non-zero on a registry-vs-enclave
    /// mismatch, so it can gate scripts.
    Pubkey {
        /// Enclave sidecar endpoint: a UDS path or a `host:port` (Gramine) address.
        #[arg(long)]
        enclave_socket: String,
        /// Also read the on-chain `tributeOfferPublicKey()` (TEE registry slot-1)
        /// and assert it equals the enclave's resident offer key.
        #[arg(long, default_value_t = false)]
        diff_chain: bool,
    },
}

impl TeeCmd {
    pub async fn run(self, client: &(impl Rpc + Sync), private_key: Option<&str>) -> Result<()> {
        match self {
            TeeCmd::Join {
                enclave_socket,
                node_data_dir,
                reth_p2p_secret_key,
                binding_id,
                valid_until,
                timeout_secs,
            } => {
                join(
                    client,
                    TeeJoinArgs {
                        private_key,
                        enclave_socket: &enclave_socket,
                        node_data_dir: node_data_dir.as_deref(),
                        reth_p2p_secret_key: reth_p2p_secret_key.as_deref(),
                        binding_id: &binding_id,
                        valid_until,
                        timeout_secs,
                    },
                )
                .await
            }
            TeeCmd::Renew {
                enclave_socket,
                node_data_dir,
                reth_p2p_secret_key,
            } => {
                renew(
                    client,
                    private_key,
                    &enclave_socket,
                    &node_data_dir,
                    reth_p2p_secret_key.as_deref(),
                )
                .await
            }
            TeeCmd::Status {
                node_data_dir,
                warning_blocks,
                critical_blocks,
            } => renewal_status(client, &node_data_dir, warning_blocks, critical_blocks).await,
            TeeCmd::UpgradePrepare {
                candidate_enclave_socket,
                node_data_dir,
                active_tee_dir,
                candidate_tee_dir,
                reth_p2p_secret_key,
            } => {
                upgrade_prepare(
                    client,
                    &candidate_enclave_socket,
                    &node_data_dir,
                    &active_tee_dir,
                    &candidate_tee_dir,
                    reth_p2p_secret_key.as_deref(),
                )
                .await
            }
            TeeCmd::UpgradeCopyRoot { node_data_dir } => upgrade_copy_root(&node_data_dir),
            TeeCmd::UpgradeSubmit {
                candidate_enclave_socket,
                node_data_dir,
                reth_p2p_secret_key,
                binding_id,
                valid_until,
            } => {
                upgrade_submit(
                    client,
                    private_key,
                    &candidate_enclave_socket,
                    &node_data_dir,
                    reth_p2p_secret_key.as_deref(),
                    &binding_id,
                    valid_until,
                )
                .await
            }
            TeeCmd::UpgradeStatus { node_data_dir } => upgrade_status(&node_data_dir),
            TeeCmd::Pubkey {
                enclave_socket,
                diff_chain,
            } => pubkey(client, &enclave_socket, diff_chain).await,
        }
    }
}

async fn renew(
    client: &(impl Rpc + Sync),
    private_key: Option<&str>,
    enclave_socket: &str,
    node_data_dir: &std::path::Path,
    reth_p2p_secret_key: Option<&std::path::Path>,
) -> Result<()> {
    let private_key = private_key
        .ok_or_else(|| eyre::eyre!("tee renew requires the global --private-key EVM signer"))?;
    let evm_signer = RelaySignerV1::new(private_key)?;
    let manifest = load_committed_enclave_manifest_v1(node_data_dir)
        .map_err(|error| eyre::eyre!("load committed NodeHost manifest: {error}"))?;
    let rpc_chain_id = client.eth_chain_id().await?;
    let path = reth_p2p_secret_key
        .ok_or_else(|| eyre::eyre!("NodeHost renewal requires --reth-p2p-secret-key"))?;
    let node_signing_key = load_secp256k1_key_file(path)?;
    ensure_signer_matches_node_id(&node_signing_key, &manifest.node_id)?;
    let mut enclave = outbe_tee::connect_or_initialize_node_host_enclave(
        enclave_socket,
        node_data_dir,
        NodeHostIdentityV1 {
            chain_id: rpc_chain_id,
            genesis_hash: manifest.genesis_hash,
            reth_p2p_public: manifest.node_id.reth_p2p_public,
        },
        |hash| sign_node_hash(&node_signing_key, hash),
    )
    .map_err(|error| eyre::eyre!("connect NodeHost enclave: {error}"))?;
    let selector = NodeBindingSelectorV1::NodeHost(manifest.node_id.reth_p2p_public);
    let config = RenewalServiceConfigV1 {
        node_data_dir: node_data_dir.to_path_buf(),
        selector,
        manifest,
    };
    let signer = |hash| {
        sign_node_hash(&node_signing_key, hash)
            .map_err(|error| eyre::eyre!("node authority signing failed: {error}"))
    };
    let started = Instant::now();
    loop {
        let outcome = run_renewal_once_v1(
            &CliFinalityRpc(client),
            &evm_signer,
            &mut enclave,
            &signer,
            &config,
        )
        .await?;
        match &outcome {
            RenewalOutcomeV1::Finalized { .. } | RenewalOutcomeV1::NotDue { .. } => {
                println!("{outcome:#?}");
                return Ok(());
            }
            RenewalOutcomeV1::Submitted { .. } | RenewalOutcomeV1::Abandoned { .. } => {
                if started.elapsed() >= MANUAL_RENEWAL_FINALITY_TIMEOUT {
                    return Err(eyre::eyre!(
                        "tee renew timed out waiting for canonical finality; last outcome: {outcome:#?}"
                    ));
                }
                tokio::time::sleep(MANUAL_RENEWAL_RECONCILE_INTERVAL).await;
            }
        }
    }
}

async fn renewal_status(
    client: &(impl Rpc + Sync),
    node_data_dir: &std::path::Path,
    warning_blocks: u64,
    critical_blocks: u64,
) -> Result<()> {
    let manifest = load_committed_enclave_manifest_v1(node_data_dir)
        .map_err(|error| eyre::eyre!("load committed NodeHost manifest: {error}"))?;
    let selector = NodeBindingSelectorV1::NodeHost(manifest.node_id.reth_p2p_public);
    let status = read_renewal_status_v1(
        &CliFinalityRpc(client),
        node_data_dir,
        &selector,
        warning_blocks,
        critical_blocks,
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&status)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn upgrade_prepare(
    client: &(impl Rpc + Sync),
    candidate_enclave_socket: &str,
    node_data_dir: &std::path::Path,
    active_tee_dir: &std::path::Path,
    candidate_tee_dir: &std::path::Path,
    reth_p2p_secret_key: Option<&std::path::Path>,
) -> Result<()> {
    let active = load_committed_enclave_manifest_v1(node_data_dir)
        .map_err(|error| eyre::eyre!("load committed NodeHost manifest: {error}"))?;
    let rpc_chain_id = client.eth_chain_id().await?;
    let (candidate, _, _) = connect_upgrade_candidate_v1(
        candidate_enclave_socket,
        node_data_dir,
        &active,
        rpc_chain_id,
        reth_p2p_secret_key,
    )?;
    let staged = read_finalized_staged_successor_policy_v1(&CliFinalityRpc(client))
        .await?
        .ok_or_else(|| eyre::eyre!("no successor TEE policy is staged at finalized state"))?;
    let successor_policy_hash = staged
        .policy
        .policy_hash()
        .map_err(|error| eyre::eyre!("hash finalized staged successor policy: {error}"))?;
    let predecessor_manifest_hash = active
        .authorization_hash()
        .map_err(|error| eyre::eyre!("hash active NodeHost manifest: {error}"))?;
    let candidate_manifest_hash = candidate
        .manifest()
        .authorization_hash()
        .map_err(|error| eyre::eyre!("hash candidate NodeHost manifest: {error}"))?;
    let snapshot = prepare_upgrade_journal_v1(
        node_data_dir,
        UpgradeContextV1 {
            predecessor_manifest_hash,
            candidate_manifest_hash,
            successor_policy_hash,
            activation_height: staged.policy.activation_height,
            active_tee_dir: active_tee_dir.to_path_buf(),
            candidate_tee_dir: candidate_tee_dir.to_path_buf(),
        },
    )?;
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    println!(
        "next: stop candidate B, run `outbe-cli tee upgrade-copy-root --node-data-dir {}`, then restart B",
        node_data_dir.display()
    );
    Ok(())
}

fn upgrade_copy_root(node_data_dir: &std::path::Path) -> Result<()> {
    let snapshot = copy_same_platform_sealed_root_and_checkpoint_v1(node_data_dir)?;
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    println!("next: restart candidate B, then run `outbe-cli tee upgrade-submit ...`");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn upgrade_submit(
    client: &(impl Rpc + Sync),
    private_key: Option<&str>,
    candidate_enclave_socket: &str,
    node_data_dir: &std::path::Path,
    reth_p2p_secret_key: Option<&std::path::Path>,
    binding_id: &str,
    valid_until: u64,
) -> Result<()> {
    let private_key = private_key.ok_or_else(|| {
        eyre::eyre!("tee upgrade-submit requires the global --private-key EVM signer")
    })?;
    let evm_signer = RelaySignerV1::new(private_key)?;
    let active = load_committed_enclave_manifest_v1(node_data_dir)
        .map_err(|error| eyre::eyre!("load committed NodeHost manifest: {error}"))?;
    let rpc_chain_id = client.eth_chain_id().await?;
    let (mut candidate, node_signing_key, selector) = connect_upgrade_candidate_v1(
        candidate_enclave_socket,
        node_data_dir,
        &active,
        rpc_chain_id,
        reth_p2p_secret_key,
    )?;
    let signer = |hash| {
        sign_node_hash(&node_signing_key, hash)
            .map_err(|error| eyre::eyre!("node authority signing failed: {error}"))
    };
    let outcome = run_upgrade_submission_v1(
        &CliFinalityRpc(client),
        &evm_signer,
        &mut candidate,
        &signer,
        node_data_dir,
        &selector,
        parse_nonzero_b256(binding_id, "--binding-id")?,
        valid_until,
    )
    .await?;
    println!("{outcome:#?}");
    println!(
        "the running node will promote B only after the exact transition binding is locally finalized"
    );
    Ok(())
}

fn upgrade_status(node_data_dir: &std::path::Path) -> Result<()> {
    match inspect_upgrade_journal_v1(node_data_dir)? {
        Some(snapshot) => println!("{}", serde_json::to_string_pretty(&snapshot)?),
        None => println!("no same-platform enclave upgrade is journaled"),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn connect_upgrade_candidate_v1(
    candidate_enclave_socket: &str,
    node_data_dir: &std::path::Path,
    active: &outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1,
    rpc_chain_id: u64,
    reth_p2p_secret_key: Option<&std::path::Path>,
) -> Result<(
    ReplacementCandidateEnclaveV1,
    k256::ecdsa::SigningKey,
    NodeBindingSelectorV1,
)> {
    if active.chain_id != U256::from(rpc_chain_id).to_be_bytes() {
        eyre::bail!("committed NodeHost manifest chain id does not match eth_chainId");
    }
    let path = reth_p2p_secret_key
        .ok_or_else(|| eyre::eyre!("NodeHost upgrade requires --reth-p2p-secret-key"))?;
    let signing = load_secp256k1_key_file(path)?;
    ensure_signer_matches_node_id(&signing, &active.node_id)?;
    let candidate = outbe_tee::prepare_node_host_enclave_replacement_candidate(
        candidate_enclave_socket,
        node_data_dir,
        NodeHostIdentityV1 {
            chain_id: rpc_chain_id,
            genesis_hash: active.genesis_hash,
            reth_p2p_public: active.node_id.reth_p2p_public,
        },
        |hash| sign_node_hash(&signing, hash),
    )
    .map_err(|error| eyre::eyre!("prepare NodeHost candidate B: {error}"))?;
    Ok((
        candidate,
        signing,
        NodeBindingSelectorV1::NodeHost(active.node_id.reth_p2p_public),
    ))
}

/// Query the enclave's offer recipient + identity public keys and (optionally)
/// diff the permanent offer key against the on-chain registry. Before readiness
/// the recipient is onboarding-only and cannot be compared with chain state.
async fn pubkey(client: &(impl Rpc + Sync), enclave_socket: &str, diff_chain: bool) -> Result<()> {
    let mut enclave = EnclaveClient::connect_endpoint(enclave_socket)
        .map_err(|e| eyre::eyre!("connect enclave at {enclave_socket}: {e}"))?;
    let label = enclave.attestation_label().to_string();
    let (mrenclave, mrsigner, isv_svn) = enclave.measurements();
    let hardware_quote_type_reported = enclave.is_hardware_attested();
    let (offer_key_ready, offer_pub, tee_bls_pub) =
        match enclave.request(&EnclaveRequest::GetPublicKeys) {
            Ok(EnclaveResponse::PublicKeys {
                offer_key_ready,
                recipient_x25519_pub,
                tee_bls_pub,
                ..
            }) => (offer_key_ready, recipient_x25519_pub, tee_bls_pub),
            Ok(other) => return Err(eyre::eyre!("expected enclave PublicKeys, got {other:?}")),
            Err(e) => return Err(eyre::eyre!("enclave GetPublicKeys failed: {e}")),
        };
    println!(
        "enclave offer pubkey (recipient_x25519): 0x{}",
        hex::encode(offer_pub)
    );
    println!(
        "enclave tee_bls_pub (DKG identity):      0x{}",
        hex::encode(&tee_bls_pub)
    );
    println!("permanent offer key ready:                 {offer_key_ready}");
    println!("attestation:                             {label}");
    println!("hardware quote type reported:           {hardware_quote_type_reported}");
    println!(
        "mrenclave:                               0x{}",
        hex::encode(mrenclave)
    );
    println!(
        "mrsigner:                                0x{}",
        hex::encode(mrsigner)
    );
    println!("isv_svn:                                 {isv_svn}");
    if !diff_chain {
        return Ok(());
    }
    if !offer_key_ready {
        return Err(eyre::eyre!(
            "enclave permanent offer key is not ready; onboarding recipient cannot be compared with chain state"
        ));
    }

    let onchain = call_u256(
        client,
        ITeeRegistry::tributeOfferPublicKeyCall {}.abi_encode(),
    )
    .await?;
    if onchain.is_zero() {
        return Err(eyre::eyre!(
            "on-chain tributeOfferPublicKey == 0 - chain is not TEE-bootstrapped yet, \
             nothing to diff against"
        ));
    }
    let onchain_bytes: [u8; 32] = onchain.to_be_bytes();
    println!(
        "on-chain tributeOfferPublicKey (slot-1): 0x{}",
        hex::encode(onchain_bytes)
    );
    if onchain_bytes == offer_pub {
        println!("[OK] MATCH - enclave resident offer key == on-chain registry");
        Ok(())
    } else {
        Err(eyre::eyre!(
            "[FAIL] MISMATCH - enclave offer key 0x{} != on-chain 0x{}",
            hex::encode(offer_pub),
            hex::encode(onchain_bytes)
        ))
    }
}

struct TeeJoinArgs<'a> {
    private_key: Option<&'a str>,
    enclave_socket: &'a str,
    node_data_dir: Option<&'a std::path::Path>,
    reth_p2p_secret_key: Option<&'a std::path::Path>,
    binding_id: &'a str,
    valid_until: u64,
    timeout_secs: u64,
}

fn authorize_validator_node_binding(
    chain_id: [u8; 32],
    genesis_hash: B256,
    node_id_hash: B256,
    evm_signer: &crate::tx::TxSigner,
) -> Result<(ValidatorNodeBindingV1, B256, [u8; 65])> {
    let binding = ValidatorNodeBindingV1 {
        chain_id,
        genesis_hash,
        validator: evm_signer.address().into_array(),
        node_id_hash,
    };
    let binding_hash = binding
        .binding_hash()
        .map_err(|error| eyre::eyre!("hash address-to-NodeHost binding: {error}"))?;
    let signature = sign_node_hash(evm_signer.key(), binding_hash)
        .map_err(|error| eyre::eyre!("sign address-to-NodeHost binding: {error}"))?;
    Ok((binding, binding_hash, signature))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JoinTransport {
    AuthorizedNodeHost,
    Development,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JoinOfferKeyState {
    Keyless,
    ReadyExact,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct JoinCompletionPlan {
    ingest_offer_key: bool,
    promote_candidate: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MissingCommittedRelayPlan {
    ConstructAndPersist,
    CleanupReadyExact,
}

struct ExactJoinRelayV1 {
    transaction_hash: B256,
    raw_transaction: Vec<u8>,
}

enum DurableJoinSubmissionV1 {
    Candidate(ReplacementCandidateSubmissionV1),
    Committed(CommittedJoinSubmissionV1),
}

impl DurableJoinSubmissionV1 {
    fn evidence(&self) -> &[u8] {
        match self {
            Self::Candidate(value) => value.evidence(),
            Self::Committed(value) => value.evidence(),
        }
    }

    const fn node_signature(&self) -> &[u8; 65] {
        match self {
            Self::Candidate(value) => value.node_signature(),
            Self::Committed(value) => value.node_signature(),
        }
    }

    const fn enclave_signature(&self) -> &[u8; 64] {
        match self {
            Self::Candidate(value) => value.enclave_signature(),
            Self::Committed(value) => value.enclave_signature(),
        }
    }

    const fn is_candidate(&self) -> bool {
        matches!(self, Self::Candidate(_))
    }

    const fn is_committed(&self) -> bool {
        matches!(self, Self::Committed(_))
    }

    const fn registration_caller(&self) -> Option<Address> {
        match self {
            Self::Candidate(_) => None,
            Self::Committed(value) => Some(value.registration_caller()),
        }
    }
}

fn ensure_durable_join_registration_caller(
    durable_caller: Option<Address>,
    current_caller: Address,
) -> Result<()> {
    if durable_caller.is_some_and(|caller| caller != current_caller) {
        eyre::bail!("durable tee join was created with a different global --private-key");
    }
    Ok(())
}

fn plan_missing_committed_relay(
    resumes_finalized_target: bool,
    offer_key_state: JoinOfferKeyState,
) -> Result<MissingCommittedRelayPlan> {
    match (resumes_finalized_target, offer_key_state) {
        (false, _) => Ok(MissingCommittedRelayPlan::ConstructAndPersist),
        (true, JoinOfferKeyState::ReadyExact) => Ok(MissingCommittedRelayPlan::CleanupReadyExact),
        (true, JoinOfferKeyState::Keyless) => eyre::bail!(
            "finalized committed binding has no durable pre-relay transaction checkpoint"
        ),
    }
}

async fn relay_exact_join_transaction(
    client: &(impl Rpc + Sync),
    relay: &ExactJoinRelayV1,
    resumes_finalized_target: bool,
) -> Result<String> {
    if keccak256(&relay.raw_transaction) != relay.transaction_hash {
        eyre::bail!("durable join relay transaction hash mismatch")
    }
    let encoded_hash = format!("0x{}", hex::encode(relay.transaction_hash));
    if resumes_finalized_target {
        return Ok(encoded_hash);
    }
    let returned = match client
        .eth_send_raw_transaction(&relay.raw_transaction)
        .await
    {
        Ok(returned) => returned,
        Err(error) => {
            let message = format!("{error:#}").to_ascii_lowercase();
            if message.contains("already known") || message.contains("known transaction") {
                return Ok(encoded_hash);
            }
            if message.contains("nonce too low") {
                let exact_receipt_exists = client
                    .eth_get_transaction_receipt(&encoded_hash)
                    .await?
                    .and_then(|receipt| {
                        receipt
                            .get("transactionHash")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .is_some_and(|hash| hash.eq_ignore_ascii_case(&encoded_hash));
                if exact_receipt_exists {
                    return Ok(encoded_hash);
                }
            }
            return Err(error).wrap_err("relay exact durable registerEnclave transaction");
        }
    };
    let returned_hash = returned
        .parse::<B256>()
        .wrap_err("parse durable registerEnclave transaction hash")?;
    if returned_hash != relay.transaction_hash {
        eyre::bail!("RPC returned a different durable join transaction hash");
    }
    Ok(encoded_hash)
}

fn plan_join_completion(
    offer_key_state: JoinOfferKeyState,
    is_candidate: bool,
) -> JoinCompletionPlan {
    JoinCompletionPlan {
        ingest_offer_key: offer_key_state == JoinOfferKeyState::Keyless,
        promote_candidate: is_candidate,
    }
}

fn classify_join_offer_key_state(
    response: EnclaveResponse,
    expected_offer_pub: [u8; 32],
) -> Result<JoinOfferKeyState> {
    match response {
        EnclaveResponse::PublicKeys {
            offer_key_ready: false,
            ..
        } => Ok(JoinOfferKeyState::Keyless),
        EnclaveResponse::PublicKeys {
            offer_key_ready: true,
            recipient_x25519_pub,
            ..
        } if recipient_x25519_pub == expected_offer_pub => Ok(JoinOfferKeyState::ReadyExact),
        EnclaveResponse::PublicKeys {
            offer_key_ready: true,
            ..
        } => eyre::bail!("resident permanent offer key does not match finalized TeeRegistry"),
        other => eyre::bail!("expected enclave PublicKeys, got {other:?}"),
    }
}

enum JoinEnclave {
    Committed(AuthorizedEnclaveClient),
    Candidate(Box<ReplacementCandidateEnclaveV1>),
    Development(Box<EnclaveClient>),
}

impl JoinEnclave {
    fn is_candidate(&self) -> bool {
        matches!(self, Self::Candidate(_))
    }

    fn generate_dcap_quote(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<GeneratedDcapQuoteV1> {
        match self {
            Self::Committed(client) => client.generate_dcap_quote(intent),
            Self::Candidate(client) => client.generate_dcap_quote(intent),
            Self::Development(_) => unreachable!("DCAP join cannot use development transport"),
        }
        .map_err(|error| eyre::eyre!("generate intent-bound DCAP quote: {error}"))
    }

    fn sign_registration_intent_dev_v1(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<[u8; 64]> {
        match self {
            Self::Committed(client) => client.sign_registration_intent_dev_v1(intent),
            Self::Candidate(client) => client.sign_registration_intent_dev_v1(intent),
            Self::Development(client) => client.sign_registration_intent_dev_v1(intent),
        }
        .map_err(|error| eyre::eyre!("sign development V1 intent: {error}"))
    }

    fn request(&mut self, request: &EnclaveRequest) -> Result<EnclaveResponse> {
        match self {
            Self::Committed(client) => client.request(request),
            Self::Candidate(client) => client.request(request),
            Self::Development(client) => client.request(request),
        }
        .map_err(|error| eyre::eyre!(error))
    }
}

fn select_join_transport(
    attestation_mode: AttestationMode,
    has_node_data_dir: bool,
) -> Result<JoinTransport> {
    match (attestation_mode, has_node_data_dir) {
        (AttestationMode::DcapRequired, true) | (AttestationMode::GramineDirectDev, true) => {
            Ok(JoinTransport::AuthorizedNodeHost)
        }
        (AttestationMode::DcapRequired, false) => Err(eyre::eyre!(
            "DcapRequired tee join requires --node-data-dir"
        )),
        (AttestationMode::GramineDirectDev, false) => Ok(JoinTransport::Development),
    }
}

fn ensure_joinable_binding(
    binding: Option<&outbe_operator::tee::RenewalBindingV1>,
    finalized_timestamp: u64,
) -> Result<()> {
    if binding.is_some_and(|binding| finalized_timestamp < binding.valid_until) {
        eyre::bail!("finalized enclave lease is live; use `outbe-cli tee renew`");
    }
    Ok(())
}

fn registration_counters(
    binding: Option<&outbe_operator::tee::RenewalBindingV1>,
) -> Result<(u64, u64, u64, u64)> {
    let Some(binding) = binding else {
        return Ok((1, 0, 0, 0));
    };
    Ok((
        binding
            .binding_version
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("binding version exhausted"))?,
        binding
            .registration_version
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("registration version exhausted"))?,
        binding.renewal_nonce,
        binding.transition_nonce,
    ))
}

fn finalized_binding_matches_intent(
    binding: &RenewalBindingV1,
    intent: &RegistrationIntentV1,
) -> Result<bool> {
    Ok(binding.node_id_hash
        == intent
            .node_id
            .node_id_hash()
            .map_err(|error| eyre::eyre!("hash registration node identity: {error}"))?
        && binding.enclave_id == intent.enclave_id
        && binding.binding_id == intent.binding_id
        && binding.intent_hash
            == intent
                .intent_hash()
                .map_err(|error| eyre::eyre!("hash registration intent: {error}"))?
        && binding.binding_version == intent.binding_version
        && binding.registration_version == intent.registration_version
        && binding.renewal_nonce == intent.renewal_nonce
        && binding.transition_nonce == intent.transition_nonce
        && binding.valid_until == intent.requested_valid_until
        && binding.recipient_x25519 == B256::from(intent.recipient_x25519)
        && binding.attestation_ed25519 == B256::from(intent.attestation_ed25519)
        && binding.noise_responder_x25519 == B256::from(intent.noise_responder_x25519)
        && binding.node_host_authorization_hash == intent.node_host_authorization_hash)
}

fn committed_manifest_matches_binding(
    manifest: &outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1,
    binding: &RenewalBindingV1,
) -> Result<bool> {
    Ok(manifest
        .node_id
        .node_id_hash()
        .map_err(|error| eyre::eyre!("hash committed node identity: {error}"))?
        == binding.node_id_hash
        && manifest
            .enclave_id()
            .map_err(|error| eyre::eyre!("derive committed enclave identity: {error}"))?
            == binding.enclave_id
        && B256::from(manifest.recipient_x25519) == binding.recipient_x25519
        && B256::from(manifest.attestation_ed25519) == binding.attestation_ed25519
        && B256::from(manifest.noise_responder_x25519) == binding.noise_responder_x25519
        && manifest
            .node_host_authorization_hash()
            .map_err(|error| eyre::eyre!("derive committed NodeHost authorization: {error}"))?
            == binding.node_host_authorization_hash)
}

fn finalized_join_admission_anchor_v1(
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

fn persist_authorized_join_admission_anchor_v1(
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

async fn join(client: &(impl Rpc + Sync), args: TeeJoinArgs<'_>) -> Result<()> {
    let TeeJoinArgs {
        private_key,
        enclave_socket,
        node_data_dir,
        reth_p2p_secret_key,
        binding_id,
        valid_until,
        timeout_secs,
    } = args;
    let private_key_hex = private_key
        .ok_or_else(|| eyre::eyre!("tee join requires the global --private-key EVM signer"))?;
    let evm_signer = super::require_signer(Some(private_key_hex))?;
    let rpc_chain_id = client.eth_chain_id().await?;
    let binding_id = parse_nonzero_b256(binding_id, "--binding-id")?;
    let path = reth_p2p_secret_key
        .ok_or_else(|| eyre::eyre!("tee join requires --reth-p2p-secret-key"))?;
    let node_signing_key = load_secp256k1_key_file(path)?;
    let reth_p2p_public = compressed_public_key(&node_signing_key)?;
    let node_id = NodeIdV1 { reth_p2p_public };
    let node_id_hash = node_id
        .node_id_hash()
        .map_err(|error| eyre::eyre!("hash V1 node identity: {error}"))?;
    let binding_selector = NodeBindingSelectorV1::NodeHost(reth_p2p_public);
    let finalized = read_finalized_registry_view_v1(&CliFinalityRpc(client), &binding_selector)
        .await
        .wrap_err("read exact finalized TeeRegistry join state")?;
    let policy = finalized.policy.clone();
    if policy.chain_id != U256::from(rpc_chain_id).to_be_bytes() {
        return Err(eyre::eyre!(
            "finalized V1 policy chain id does not match eth_chainId"
        ));
    }
    if finalized
        .binding
        .as_ref()
        .is_some_and(|binding| binding.node_id_hash != node_id_hash)
    {
        eyre::bail!("finalized Registry selector returned another node identity");
    }
    if let Some(node_binding) = finalized.binding.as_ref() {
        let signer_view = read_finalized_registry_view_v1(
            &CliFinalityRpc(client),
            &NodeBindingSelectorV1::Validator(evm_signer.address()),
        )
        .await
        .wrap_err("authenticate global EVM signer against finalized TeeRegistry association")?;
        if signer_view.binding.as_ref() != Some(node_binding) {
            eyre::bail!("global --private-key does not own this finalized NodeHost association");
        }
    }
    let mut durable_submission = if let Some(node_data_dir) = node_data_dir {
        let manifest_path = node_data_dir
            .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
            .join(outbe_tee::node_host::NODE_HOST_MANIFEST_V1);
        if manifest_path.exists() {
            let candidate = load_replacement_candidate_submission(node_data_dir)
                .map_err(|error| eyre::eyre!("inspect candidate join checkpoint: {error}"))?;
            let committed = load_committed_join_submission(node_data_dir)
                .map_err(|error| eyre::eyre!("inspect committed join checkpoint: {error}"))?;
            match (candidate, committed) {
                (Some(_), Some(_)) => {
                    eyre::bail!("candidate and committed tee join checkpoints coexist")
                }
                (Some(value), None) => Some(DurableJoinSubmissionV1::Candidate(value)),
                (None, Some(value)) => Some(DurableJoinSubmissionV1::Committed(value)),
                (None, None) => None,
            }
        } else {
            None
        }
    } else {
        None
    };
    ensure_durable_join_registration_caller(
        durable_submission
            .as_ref()
            .and_then(DurableJoinSubmissionV1::registration_caller),
        evm_signer.address(),
    )?;
    let durable_resume_intent = durable_submission
        .as_ref()
        .map(|submission| {
            let evidence = AttestationEvidenceV1::decode_canonical(submission.evidence())
                .map_err(|error| eyre::eyre!("decode durable join checkpoint: {error}"))?;
            Ok::<_, eyre::Report>(match evidence {
                AttestationEvidenceV1::Dcap(value) => value.intent,
                AttestationEvidenceV1::GramineDirectDev(value) => value.intent,
            })
        })
        .transpose()?;
    let resumes_finalized_target = match (&finalized.binding, &durable_resume_intent) {
        (Some(binding), Some(intent)) => finalized_binding_matches_intent(binding, intent)?,
        _ => false,
    };
    if !resumes_finalized_target {
        if let Some(binding) = finalized
            .binding
            .as_ref()
            .filter(|binding| finalized.schedule.finalized_timestamp < binding.valid_until)
        {
            let exact_completed_replay = if binding.binding_id == binding_id
                && binding.valid_until == valid_until
            {
                if let Some(node_data_dir) = node_data_dir {
                    let manifest = load_committed_enclave_manifest_v1(node_data_dir)
                        .map_err(|error| eyre::eyre!("load committed replay manifest: {error}"))?;
                    committed_manifest_matches_binding(&manifest, binding)?
                } else {
                    false
                }
            } else {
                false
            };
            if exact_completed_replay {
                let mut committed = connect_committed_node_host_enclave(
                    enclave_socket,
                    node_data_dir.expect("completed replay requires NodeHost state"),
                )
                .map_err(|error| eyre::eyre!("reopen completed tee join: {error}"))?;
                match committed.request(&EnclaveRequest::GetPublicKeys)? {
                    EnclaveResponse::PublicKeys {
                        offer_key_ready: true,
                        recipient_x25519_pub,
                        ..
                    } if recipient_x25519_pub
                        == <B256 as Into<[u8; 32]>>::into(finalized.tribute_offer_public) =>
                    {
                        persist_authorized_join_admission_anchor_v1(
                            node_data_dir.expect("completed replay requires NodeHost state"),
                            &finalized,
                            binding.node_id_hash,
                            binding.enclave_id,
                            binding.intent_hash,
                        )?;
                        println!("[ok] exact tee join is already finalized and locally committed");
                        return Ok(());
                    }
                    other => eyre::bail!(
                        "finalized tee join is not durably ready in the committed enclave: {other:?}"
                    ),
                }
            }
            ensure_joinable_binding(Some(binding), finalized.schedule.finalized_timestamp)?;
        }
    }
    if !resumes_finalized_target {
        let lease = valid_until
            .checked_sub(finalized.schedule.finalized_timestamp)
            .ok_or_else(|| eyre::eyre!("--valid-until is not after finalized consensus time"))?;
        if lease < policy.minimum_lease || lease > policy.maximum_lease {
            return Err(eyre::eyre!(
                "--valid-until lease {lease}s is outside finalized policy range {}..={}s",
                policy.minimum_lease,
                policy.maximum_lease
            ));
        }
    }
    let (validator_binding, validator_binding_hash, validator_signature) =
        authorize_validator_node_binding(
            policy.chain_id,
            policy.genesis_hash,
            node_id_hash,
            &evm_signer,
        )?;
    let node_binding_signature = sign_node_hash(&node_signing_key, validator_binding_hash)
        .map_err(|error| eyre::eyre!("sign NodeHost side of address binding: {error}"))?;
    let validator_binding = validator_binding
        .encode_canonical()
        .map_err(|error| eyre::eyre!("encode address-to-NodeHost binding: {error}"))?;
    // Read the permanent chain key before generating a fresh quote. Its exact
    // value is later authenticated again inside the recipient enclave.
    let expected_offer_pub: [u8; 32] = finalized.tribute_offer_public.into();
    let key_epoch = call_u256(client, ITeeRegistry::keyEpochCall {}.abi_encode())
        .await?
        .to::<u64>();
    let tribute_offer_epoch =
        call_u256(client, ITeeRegistry::tributeOfferEpochCall {}.abi_encode())
            .await?
            .to::<u64>();
    let policy_hash = policy
        .policy_hash()
        .map_err(|error| eyre::eyre!("active V1 policy is invalid: {error}"))?;
    if !resumes_finalized_target {
        ensure_joinable_binding(
            finalized.binding.as_ref(),
            finalized.schedule.finalized_timestamp,
        )?;
    }

    let join_transport = select_join_transport(policy.attestation_mode, node_data_dir.is_some())?;
    let (
        mut enclave,
        recipient_x25519,
        attestation_ed25519,
        noise_responder_x25519,
        enclave_id,
        node_host_authorization_hash,
    ) = match join_transport {
        JoinTransport::AuthorizedNodeHost => {
            let node_data_dir = node_data_dir.expect("transport selection requires node data dir");
            fs::create_dir_all(node_data_dir).wrap_err_with(|| {
                format!("create NodeHost data directory {}", node_data_dir.display())
            })?;
            let manifest_path = node_data_dir
                .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
                .join(outbe_tee::node_host::NODE_HOST_MANIFEST_V1);
            let (client, manifest) = if manifest_path.exists() {
                let committed = load_committed_enclave_manifest_v1(node_data_dir)
                    .map_err(|error| eyre::eyre!("load committed NodeHost manifest: {error}"))?;
                match connect_committed_node_host_enclave(enclave_socket, node_data_dir) {
                    Ok(client) => (JoinEnclave::Committed(client), committed),
                    Err(committed_error) if finalized.binding.is_some() => {
                        let candidate = outbe_tee::prepare_node_host_enclave_replacement_candidate(
                            enclave_socket,
                            node_data_dir,
                            NodeHostIdentityV1 {
                                chain_id: rpc_chain_id,
                                genesis_hash: policy.genesis_hash,
                                reth_p2p_public,
                            },
                            |hash| sign_node_hash(&node_signing_key, hash),
                        )
                        .map_err(|candidate_error| {
                            eyre::eyre!(
                                "endpoint matches neither the committed enclave ({committed_error}) nor a resumable fresh candidate ({candidate_error})"
                            )
                        })?;
                        let manifest = candidate.manifest().clone();
                        (JoinEnclave::Candidate(Box::new(candidate)), manifest)
                    }
                    Err(error) => {
                        return Err(eyre::eyre!(
                            "reconnect unregistered committed NodeHost enclave: {error}"
                        ));
                    }
                }
            } else {
                let client = outbe_tee::connect_or_initialize_node_host_enclave(
                    enclave_socket,
                    node_data_dir,
                    NodeHostIdentityV1 {
                        chain_id: rpc_chain_id,
                        genesis_hash: policy.genesis_hash,
                        reth_p2p_public,
                    },
                    |hash| sign_node_hash(&node_signing_key, hash),
                )
                .map_err(|error| {
                    eyre::eyre!("production NodeHost initialization failed: {error}")
                })?;
                let manifest = load_committed_enclave_manifest_v1(node_data_dir)
                    .map_err(|error| eyre::eyre!("load committed NodeHost manifest: {error}"))?;
                (JoinEnclave::Committed(client), manifest)
            };
            if manifest.chain_id != policy.chain_id
                || manifest.genesis_hash != policy.genesis_hash
                || manifest.node_id != node_id
            {
                return Err(eyre::eyre!(
                    "committed NodeHost manifest does not match the active chain and node identity"
                ));
            }
            let enclave_id = manifest
                .enclave_id()
                .map_err(|error| eyre::eyre!("invalid committed enclave identity: {error}"))?;
            let authorization = manifest.node_host_authorization_hash().map_err(|error| {
                eyre::eyre!("invalid committed NodeHost authorization: {error}")
            })?;
            (
                client,
                manifest.recipient_x25519,
                manifest.attestation_ed25519,
                manifest.noise_responder_x25519,
                enclave_id,
                authorization,
            )
        }
        JoinTransport::Development => {
            let development = EnclaveClient::connect_endpoint(enclave_socket).map_err(|error| {
                eyre::eyre!("connect development enclave at {enclave_socket}: {error}")
            })?;
            if development.is_hardware_attested() {
                return Err(eyre::eyre!(
                    "GramineDirectDev policy requires the separate non-SGX development transport"
                ));
            }
            let (recipient, attestation, noise) = match development.quote() {
                EnclaveResponse::Quote {
                    recipient_x25519_pub,
                    attestation_pub,
                    noise_static_pub,
                    ..
                } => (*recipient_x25519_pub, *attestation_pub, *noise_static_pub),
                other => return Err(eyre::eyre!("expected development Quote, got {other:?}")),
            };
            let (enclave_id, authorization) =
                development_identity_v1(&policy, &node_id, recipient, attestation, noise)?;
            (
                JoinEnclave::Development(Box::new(development)),
                recipient,
                attestation,
                noise,
                enclave_id,
                authorization,
            )
        }
    };

    // A permanent offer key is write-once enclave state. Classify it before
    // producing or relaying a fresh registration so a mismatched resident key
    // cannot mutate Registry and a matching same-enclave rejoin never repeats
    // the onboarding ingest.
    let offer_key_state = classify_join_offer_key_state(
        enclave.request(&EnclaveRequest::GetPublicKeys)?,
        expected_offer_pub,
    )?;
    let completion_plan = plan_join_completion(offer_key_state, enclave.is_candidate());
    if durable_submission
        .as_ref()
        .is_some_and(|submission| submission.is_candidate() != enclave.is_candidate())
    {
        eyre::bail!("durable tee join checkpoint targets another enclave lifecycle");
    }

    let source_binding = if resumes_finalized_target {
        None
    } else {
        finalized.binding.as_ref()
    };
    let (binding_version, registration_version, renewal_nonce, transition_nonce) =
        registration_counters(source_binding)?;

    let fresh_intent = RegistrationIntentV1 {
        chain_id: policy.chain_id,
        genesis_hash: policy.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: policy.attestation_mode,
        policy_hash,
        node_id,
        enclave_id,
        binding_id,
        binding_version,
        registration_version,
        renewal_nonce,
        transition_nonce,
        requested_valid_until: valid_until,
        recipient_x25519,
        attestation_ed25519,
        noise_responder_x25519,
        node_host_authorization_hash,
    };
    let intent = durable_resume_intent.unwrap_or(fresh_intent);
    if intent.binding_id != binding_id
        || intent.requested_valid_until != valid_until
        || intent.enclave_id != enclave_id
        || intent.recipient_x25519 != recipient_x25519
        || intent.attestation_ed25519 != attestation_ed25519
        || intent.noise_responder_x25519 != noise_responder_x25519
        || intent.node_host_authorization_hash != node_host_authorization_hash
    {
        eyre::bail!("requested tee join does not match its durable checkpoint");
    }
    let intent_hash = intent
        .intent_hash()
        .map_err(|error| eyre::eyre!("invalid V1 registration intent: {error}"))?;
    let fresh_node_signature = sign_node_hash(&node_signing_key, intent_hash)
        .map_err(|error| eyre::eyre!("sign V1 node intent: {error}"))?;
    let (evidence_value, node_signature, enclave_signature) = if let Some(durable) =
        durable_submission.as_ref()
    {
        let evidence = AttestationEvidenceV1::decode_canonical(durable.evidence())
            .map_err(|error| eyre::eyre!("decode durable join evidence: {error}"))?;
        let durable_intent = match &evidence {
            AttestationEvidenceV1::Dcap(value) => &value.intent,
            AttestationEvidenceV1::GramineDirectDev(value) => &value.intent,
        };
        if durable_intent != &intent || durable.node_signature() != &fresh_node_signature {
            eyre::bail!("requested tee join conflicts with the durable registration");
        }
        (
            evidence,
            *durable.node_signature(),
            *durable.enclave_signature(),
        )
    } else {
        let (evidence, signature) = match policy.attestation_mode {
            AttestationMode::DcapRequired => {
                let generated = enclave.generate_dcap_quote(&intent)?;
                let components = acquire_dcap_collateral_v1(&generated.quote_body)
                    .map_err(|error| eyre::eyre!("acquire canonical DCAP collateral: {error}"))?;
                let signature = generated.enclave_signature;
                (
                    AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
                        intent: intent.clone(),
                        quote: generated.quote_body,
                        components,
                        transition_key_ready_proof: generated.transition_key_ready_proof,
                    }),
                    signature,
                )
            }
            AttestationMode::GramineDirectDev => {
                let signature = enclave.sign_registration_intent_dev_v1(&intent)?;
                (
                    AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
                        intent: intent.clone(),
                        dev_attestation_public: attestation_ed25519,
                        dev_signature: signature,
                    }),
                    signature,
                )
            }
        };
        if enclave.is_candidate() {
            let persisted = persist_replacement_candidate_submission(
                node_data_dir.expect("candidate join requires NodeHost state"),
                &evidence,
                &fresh_node_signature,
                &signature,
            )
            .map_err(|error| eyre::eyre!("persist candidate registration: {error}"))?;
            durable_submission = Some(DurableJoinSubmissionV1::Candidate(persisted));
        } else if join_transport == JoinTransport::AuthorizedNodeHost
            && offer_key_state == JoinOfferKeyState::Keyless
        {
            let persisted = persist_committed_join_submission(
                node_data_dir.expect("committed join requires NodeHost state"),
                evm_signer.address(),
                &evidence,
                &fresh_node_signature,
                &signature,
            )
            .map_err(|error| eyre::eyre!("persist committed registration: {error}"))?;
            durable_submission = Some(DurableJoinSubmissionV1::Committed(persisted));
        }
        (evidence, fresh_node_signature, signature)
    };
    let evidence = evidence_value
        .encode_canonical()
        .map_err(|error| eyre::eyre!("encode canonical V1 evidence: {error}"))?;
    let node_id_hash = match AttestationEvidenceV1::decode_canonical(&evidence)
        .map_err(|error| eyre::eyre!("re-decode canonical V1 evidence: {error}"))?
    {
        AttestationEvidenceV1::Dcap(value) => value.intent.node_id,
        AttestationEvidenceV1::GramineDirectDev(value) => value.intent.node_id,
    }
    .node_id_hash()
    .map_err(|error| eyre::eyre!("hash V1 node identity: {error}"))?;
    let call = ITeeRegistry::registerEnclaveCall {
        evidence: evidence.clone().into(),
        nodeSignature: node_signature.to_vec().into(),
        enclaveSignature: enclave_signature.to_vec().into(),
        validatorNodeBinding: validator_binding.into(),
        validatorSignature: validator_signature.to_vec().into(),
        nodeBindingSignature: node_binding_signature.to_vec().into(),
    }
    .abi_encode();
    let gas_limit = TeeRegistryGasScheduleV1::normative()
        .maximum_transaction_gas(
            RegistryMutatorV1::RegisterEnclave,
            call.len(),
            evidence.len(),
            policy.measurement_rules.len(),
            policy.attestation_mode,
        )
        .map_err(|error| eyre::eyre!("calculate V1 registration gas: {error}"))?;

    // The global EVM signer owns both the address-to-NodeHost association and
    // the transaction envelope. No second node or renewal EVM key exists.
    let calldata_hash = keccak256(&call);
    let (tx_hash, from_block) = if enclave.is_candidate() {
        let node_data_dir = node_data_dir.expect("candidate join requires NodeHost state");
        let relay = if let Some(durable) = load_replacement_candidate_relay(node_data_dir)
            .map_err(|error| eyre::eyre!("load durable candidate relay: {error}"))?
        {
            if durable.calldata_hash() != calldata_hash {
                eyre::bail!("durable candidate transaction targets different calldata");
            }
            durable
        } else {
            if resumes_finalized_target {
                eyre::bail!(
                    "finalized candidate binding has no durable pre-relay transaction checkpoint"
                );
            }
            let relay_signer = RelaySignerV1::new(private_key_hex)?;
            if relay_signer.address() != evm_signer.address() {
                eyre::bail!("global EVM signer address is inconsistent");
            }
            let account_nonce = client
                .eth_get_transaction_count(evm_signer.address())
                .await?;
            let gas_price = buffered_gas_price(client.eth_gas_price().await?);
            let required_balance = gas_price.saturating_mul(U256::from(gas_limit));
            let balance = client.eth_get_balance(evm_signer.address()).await?;
            if balance < required_balance {
                eyre::bail!(
                    "TEE join EVM signer {} has {balance} but needs at least {required_balance}",
                    evm_signer.address()
                );
            }
            let raw = relay_signer.sign_renewal(
                rpc_chain_id,
                account_nonce,
                gas_price,
                gas_limit,
                abi::TEE_REGISTRY_ADDR,
                &call,
            )?;
            persist_replacement_candidate_relay(node_data_dir, calldata_hash, &raw.raw_transaction)
                .map_err(|error| eyre::eyre!("persist exact candidate transaction: {error}"))?
        };
        let exact = ExactJoinRelayV1 {
            transaction_hash: relay.transaction_hash(),
            raw_transaction: relay.raw_transaction().to_vec(),
        };
        let from_block = client.eth_block_number().await?;
        (
            relay_exact_join_transaction(client, &exact, resumes_finalized_target).await?,
            from_block,
        )
    } else if durable_submission
        .as_ref()
        .is_some_and(DurableJoinSubmissionV1::is_committed)
    {
        let node_data_dir = node_data_dir.expect("committed join requires NodeHost state");
        let relay = if let Some(durable) = load_committed_join_relay(node_data_dir)
            .map_err(|error| eyre::eyre!("load durable committed relay: {error}"))?
        {
            if durable.calldata_hash() != calldata_hash {
                eyre::bail!("durable committed transaction targets different calldata");
            }
            durable
        } else {
            match plan_missing_committed_relay(resumes_finalized_target, offer_key_state)? {
                MissingCommittedRelayPlan::CleanupReadyExact => {
                    drop(enclave);
                    let mut reopened =
                        connect_committed_node_host_enclave(enclave_socket, node_data_dir)
                            .map_err(|error| {
                                eyre::eyre!("reopen completed committed enclave: {error}")
                            })?;
                    match reopened.request(&EnclaveRequest::GetPublicKeys)? {
                        EnclaveResponse::PublicKeys {
                            offer_key_ready: true,
                            recipient_x25519_pub,
                            ..
                        } if recipient_x25519_pub == expected_offer_pub => {}
                        other => eyre::bail!(
                            "cleanup recovery did not reopen the exact permanent offer key: {other:?}"
                        ),
                    }
                    persist_authorized_join_admission_anchor_v1(
                        node_data_dir,
                        &finalized,
                        node_id_hash,
                        enclave_id,
                        intent_hash,
                    )?;
                    clear_committed_join_checkpoint(node_data_dir, intent_hash).map_err(
                        |error| eyre::eyre!("clear recovered committed join checkpoint: {error}"),
                    )?;
                    println!(
                        "[ok] finalized tee join cleanup recovered without another transaction or onboarding ingest"
                    );
                    return Ok(());
                }
                MissingCommittedRelayPlan::ConstructAndPersist => {}
            }
            let relay_signer = RelaySignerV1::new(private_key_hex)?;
            if relay_signer.address() != evm_signer.address() {
                eyre::bail!("global EVM signer address is inconsistent");
            }
            let account_nonce = client
                .eth_get_transaction_count(evm_signer.address())
                .await?;
            let gas_price = buffered_gas_price(client.eth_gas_price().await?);
            let required_balance = gas_price.saturating_mul(U256::from(gas_limit));
            let balance = client.eth_get_balance(evm_signer.address()).await?;
            if balance < required_balance {
                eyre::bail!(
                    "TEE join EVM signer {} has {balance} but needs at least {required_balance}",
                    evm_signer.address()
                );
            }
            let from_block = client.eth_block_number().await?;
            let raw = relay_signer.sign_renewal(
                rpc_chain_id,
                account_nonce,
                gas_price,
                gas_limit,
                abi::TEE_REGISTRY_ADDR,
                &call,
            )?;
            persist_committed_join_relay(
                node_data_dir,
                calldata_hash,
                from_block,
                &raw.raw_transaction,
            )
            .map_err(|error| eyre::eyre!("persist exact committed transaction: {error}"))?
        };
        let exact = ExactJoinRelayV1 {
            transaction_hash: relay.transaction_hash(),
            raw_transaction: relay.raw_transaction().to_vec(),
        };
        (
            relay_exact_join_transaction(client, &exact, resumes_finalized_target).await?,
            relay.from_block(),
        )
    } else {
        let from_block = client.eth_block_number().await?;
        (
            evm_signer
                .send_tx_with_gas(client, abi::TEE_REGISTRY_ADDR, call, U256::ZERO, gas_limit)
                .await
                .wrap_err("V1 registerEnclave submission failed")?,
            from_block,
        )
    };
    println!(
        "V1 registerEnclave submitted by {}: {tx_hash}",
        evm_signer.address()
    );

    let finalized_join = await_finalized_join_target(
        client,
        &binding_selector,
        &intent,
        Duration::from_secs(timeout_secs),
    )
    .await?;

    let resp = if completion_plan.ingest_offer_key {
        // Accept only the artifact indexed by the canonical node identity
        // proved in the same transaction.
        let topic0 = format!(
            "0x{}",
            hex::encode(ITeeRegistry::OfferKeySealedForRegistryV1::SIGNATURE_HASH)
        );
        let topic1 = format!("0x{}", hex::encode(node_id_hash));
        match policy.attestation_mode {
            AttestationMode::DcapRequired => {
                let expected = ExpectedOnboardingBindingV1 {
                    selector: binding_selector.clone(),
                    chain_id: policy.chain_id,
                    genesis_hash: policy.genesis_hash,
                    node_id_hash,
                    enclave_id,
                    intent_hash,
                    recipient_x25519,
                    tribute_offer_public: expected_offer_pub,
                    key_epoch,
                    tribute_offer_epoch,
                };
                let finalized = await_finalized_onboarding_v1(
                    &CliFinalityRpc(client),
                    &tx_hash,
                    &expected,
                    Duration::from_secs(timeout_secs),
                )
                .await?;
                let artifact = finalized.artifact.encode_canonical().map_err(|code| {
                    eyre::eyre!("encode finalized artifact: {:#06x}", code.code())
                })?;
                println!(
                    "exact onboarding finalized at height {} (artifact {} bytes)",
                    finalized.finalized_height,
                    artifact.len()
                );
                enclave
                    .request(&EnclaveRequest::IngestDcapOnboardingArtifactV1 {
                        artifact,
                        expected_intent_hash: intent_hash,
                        expected_tribute_offer_public: expected_offer_pub,
                        expected_key_epoch: key_epoch,
                        expected_tribute_offer_epoch: tribute_offer_epoch,
                    })
                    .map_err(|error| {
                        eyre::eyre!("enclave purpose-bound onboarding ingest failed: {error}")
                    })?
            }
            AttestationMode::GramineDirectDev => {
                let sealed = poll_offer_key_sealed(
                    client,
                    &topic0,
                    &topic1,
                    &tx_hash,
                    expected_offer_pub,
                    from_block,
                    timeout_secs,
                )
                .await?;
                enclave
                    .request(&EnclaveRequest::IngestSealedOfferKeyForRegistry {
                        sealed,
                        expected_tribute_offer_public: expected_offer_pub,
                        chain_id: B256::from(policy.chain_id),
                        tribute_offer_epoch,
                    })
                    .map_err(|error| eyre::eyre!("development onboarding ingest failed: {error}"))?
            }
        }
    } else {
        EnclaveResponse::OfferKeyForRegistryIngested {
            tribute_offer_public: expected_offer_pub,
        }
    };
    match resp {
        EnclaveResponse::OfferKeyForRegistryIngested {
            tribute_offer_public,
        } => {
            if join_transport == JoinTransport::AuthorizedNodeHost {
                let node_data_dir = node_data_dir.ok_or_else(|| {
                    eyre::eyre!("authenticated onboarding lost its required node data directory")
                })?;
                persist_authorized_join_admission_anchor_v1(
                    node_data_dir,
                    &finalized_join,
                    node_id_hash,
                    enclave_id,
                    intent_hash,
                )?;
                let promotion = if completion_plan.promote_candidate {
                    let exact =
                        read_finalized_registry_view_v1(&CliFinalityRpc(client), &binding_selector)
                            .await
                            .wrap_err("read finalized candidate promotion binding")?;
                    let binding = exact.binding.ok_or_else(|| {
                        eyre::eyre!("finalized Registry lost the candidate binding")
                    })?;
                    if !finalized_binding_matches_intent(&binding, &intent)? {
                        eyre::bail!(
                            "finalized Registry binding differs from the durable candidate intent"
                        );
                    }
                    Some(FinalizedReplacementBindingV1 {
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
                } else {
                    None
                };
                drop(enclave);
                if let Some(finalized_binding) = promotion {
                    let authorization = construct_finalized_replacement_authorization_v1(
                        node_data_dir,
                        &finalized_binding,
                    )
                    .map_err(|error| {
                        eyre::eyre!("authorize finalized candidate promotion: {error}")
                    })?;
                    promote_replacement_candidate(node_data_dir, &authorization).map_err(
                        |error| eyre::eyre!("promote finalized rejoin candidate: {error}"),
                    )?;
                }
                let mut reopened =
                    connect_committed_node_host_enclave(enclave_socket, node_data_dir)
                        .map_err(|error| eyre::eyre!("reopen committed enclave: {error}"))?;
                match reopened.request(&EnclaveRequest::GetPublicKeys)? {
                    EnclaveResponse::PublicKeys {
                        offer_key_ready: true,
                        recipient_x25519_pub,
                        ..
                    } if recipient_x25519_pub == expected_offer_pub => {}
                    other => {
                        return Err(eyre::eyre!(
                            "durable onboarding reopen did not expose the exact permanent offer key: {other:?}"
                        ));
                    }
                }
                if durable_submission
                    .as_ref()
                    .is_some_and(DurableJoinSubmissionV1::is_committed)
                {
                    clear_committed_join_checkpoint(node_data_dir, intent_hash).map_err(
                        |error| eyre::eyre!("clear completed committed join checkpoint: {error}"),
                    )?;
                }
            }
            if join_transport == JoinTransport::AuthorizedNodeHost {
                println!(
                    "[OK] offer key durably installed and the authenticated enclave connection reopened (offer_public 0x{}). \
                     You can now start outbe-chain node.",
                    hex::encode(tribute_offer_public)
                );
            } else {
                println!(
                    "[OK] development offer key installed in enclave (offer_public 0x{}). \
                     You can now start the separate GramineDirectDev node.",
                    hex::encode(tribute_offer_public)
                );
            }
            Ok(())
        }
        EnclaveResponse::Error { message } => {
            Err(eyre::eyre!("enclave rejected the offer key: {message}"))
        }
        other => Err(eyre::eyre!("unexpected enclave response: {other:?}")),
    }
}

async fn await_finalized_join_target(
    client: &(impl Rpc + Sync),
    selector: &NodeBindingSelectorV1,
    intent: &RegistrationIntentV1,
    timeout: Duration,
) -> Result<FinalizedRegistryChainViewV1> {
    let started = tokio::time::Instant::now();
    loop {
        let view = read_finalized_registry_view_v1(&CliFinalityRpc(client), selector).await?;
        match view.binding.as_ref() {
            Some(binding) if finalized_binding_matches_intent(binding, intent)? => return Ok(view),
            Some(binding) if view.schedule.finalized_timestamp < binding.valid_until => {
                eyre::bail!("finalized Registry contains a competing live join binding")
            }
            _ => {}
        }
        if started.elapsed() >= timeout {
            eyre::bail!(
                "timed out after {}s waiting for exact finalized tee join binding",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// `eth_call` a view returning a single `uint256`.
async fn call_u256(client: &(impl Rpc + Sync), call: Vec<u8>) -> Result<U256> {
    let result = client.eth_call(abi::TEE_REGISTRY_ADDR, &call).await?;
    U256::abi_decode(&result).wrap_err("decode uint256")
}

fn parse_nonzero_b256(value: &str, argument: &'static str) -> Result<B256> {
    let value = B256::from(parse_hex_array::<32>(value, argument)?);
    if value.is_zero() {
        return Err(eyre::eyre!("{argument} must be nonzero"));
    }
    Ok(value)
}

fn parse_hex_array<const N: usize>(value: &str, argument: &'static str) -> Result<[u8; N]> {
    let encoded = value.strip_prefix("0x").unwrap_or(value);
    let decoded = hex::decode(encoded).wrap_err_with(|| format!("decode {argument} as hex"))?;
    decoded.try_into().map_err(|decoded: Vec<u8>| {
        eyre::eyre!(
            "{argument} must contain exactly {N} bytes, got {}",
            decoded.len()
        )
    })
}

fn load_secp256k1_key_file(path: &std::path::Path) -> Result<k256::ecdsa::SigningKey> {
    let metadata =
        fs::metadata(path).wrap_err_with(|| format!("stat Reth P2P secret {}", path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 130 {
        return Err(eyre::eyre!(
            "Reth P2P secret {} must be a bounded regular file",
            path.display()
        ));
    }
    let encoded = Zeroizing::new(
        fs::read(path).wrap_err_with(|| format!("read Reth P2P secret {}", path.display()))?,
    );
    parse_secp256k1_key_bytes(encoded.as_ref())
}

fn compressed_public_key(key: &k256::ecdsa::SigningKey) -> Result<[u8; 33]> {
    key.verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .map_err(|_| eyre::eyre!("Reth P2P public key is not compressed SEC1-33"))
}

fn ensure_signer_matches_node_id(key: &k256::ecdsa::SigningKey, node_id: &NodeIdV1) -> Result<()> {
    if compressed_public_key(key)? != node_id.reth_p2p_public {
        eyre::bail!("Reth P2P secret does not match the committed NodeHost identity");
    }
    Ok(())
}

fn parse_secp256k1_key_bytes(encoded: &[u8]) -> Result<k256::ecdsa::SigningKey> {
    let secret = if encoded.len() == 32 {
        let mut secret = Zeroizing::new([0_u8; 32]);
        secret.copy_from_slice(encoded);
        secret
    } else {
        let text = std::str::from_utf8(encoded)
            .wrap_err("Reth P2P secret is neither raw bytes nor UTF-8 hex")?;
        let text = text.trim();
        let text = text.strip_prefix("0x").unwrap_or(text);
        let decoded = Zeroizing::new(hex::decode(text).wrap_err("decode Reth P2P secret as hex")?);
        if decoded.len() != 32 {
            return Err(eyre::eyre!(
                "Reth P2P secret must contain exactly 32 bytes, got {}",
                decoded.len()
            ));
        }
        let mut secret = Zeroizing::new([0_u8; 32]);
        secret.copy_from_slice(decoded.as_ref());
        secret
    };
    k256::ecdsa::SigningKey::from_bytes((&*secret).into())
        .map_err(|error| eyre::eyre!("invalid Reth P2P secp256k1 secret: {error}"))
}

fn sign_node_hash(
    key: &k256::ecdsa::SigningKey,
    hash: B256,
) -> std::result::Result<[u8; 65], String> {
    let (signature, recovery_id): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = key
        .sign_prehash(hash.as_slice())
        .map_err(|error| format!("secp256k1 signing failed: {error}"))?;
    let mut encoded = [0_u8; 65];
    encoded[..64].copy_from_slice(&signature.to_bytes());
    encoded[64] = recovery_id.to_byte();
    Ok(encoded)
}

fn development_identity_v1(
    policy: &TeePolicyV1,
    node_id: &NodeIdV1,
    recipient_x25519: [u8; 32],
    attestation_ed25519: [u8; 32],
    noise_responder_x25519: [u8; 32],
) -> Result<(B256, B256)> {
    let mut enclave_keys = [0_u8; 96];
    enclave_keys[..32].copy_from_slice(&recipient_x25519);
    enclave_keys[32..64].copy_from_slice(&attestation_ed25519);
    enclave_keys[64..].copy_from_slice(&noise_responder_x25519);

    let mut enclave_preimage = Vec::with_capacity(ENCLAVE_ID_DOMAIN_V1.len() + enclave_keys.len());
    enclave_preimage.extend_from_slice(ENCLAVE_ID_DOMAIN_V1);
    enclave_preimage.extend_from_slice(&enclave_keys);
    let enclave_id = keccak256(enclave_preimage);

    let node_id_hash = node_id
        .node_id_hash()
        .map_err(|error| eyre::eyre!("invalid development node identity: {error}"))?;
    let mut authorization_preimage =
        Vec::with_capacity(DEV_NODE_HOST_DOMAIN_V1.len() + 32 + 32 + 32 + enclave_keys.len());
    authorization_preimage.extend_from_slice(DEV_NODE_HOST_DOMAIN_V1);
    authorization_preimage.extend_from_slice(&policy.chain_id);
    authorization_preimage.extend_from_slice(policy.genesis_hash.as_slice());
    authorization_preimage.extend_from_slice(node_id_hash.as_slice());
    authorization_preimage.extend_from_slice(&enclave_keys);
    Ok((enclave_id, keccak256(authorization_preimage)))
}

/// Poll `eth_getLogs` for the V1 onboarding artifact emitted by the exact
/// submitted transaction and canonical node identity.
async fn poll_offer_key_sealed(
    client: &(impl Rpc + Sync),
    topic0: &str,
    topic1: &str,
    transaction_hash: &str,
    expected_offer_public: [u8; 32],
    from_block: u64,
    timeout_secs: u64,
) -> Result<Vec<u8>> {
    let from = format!("0x{from_block:x}");
    let topics = [Some(topic0.to_string()), Some(topic1.to_string())];
    let deadline = Duration::from_secs(timeout_secs);
    let start = tokio::time::Instant::now();
    let mut receipt_block = None;
    loop {
        if let Some(receipt) = client
            .eth_get_transaction_receipt(transaction_hash)
            .await
            .wrap_err("poll V1 registration transaction receipt")?
        {
            let receipt_hash = receipt
                .get("transactionHash")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| eyre::eyre!("V1 registration receipt has no transactionHash"))?;
            if !receipt_hash.eq_ignore_ascii_case(transaction_hash) {
                return Err(eyre::eyre!(
                    "V1 registration receipt transaction hash mismatch"
                ));
            }
            let encoded_status = receipt
                .get("status")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| eyre::eyre!("V1 registration receipt has no status"))?;
            let status = encoded_status.strip_prefix("0x").unwrap_or(encoded_status);
            let status =
                u64::from_str_radix(status, 16).wrap_err("parse V1 registration receipt status")?;
            match status {
                0 => {
                    let block = receipt
                        .get("blockNumber")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown");
                    return Err(eyre::eyre!(
                        "V1 registerEnclave transaction {transaction_hash} reverted in block {block}"
                    ));
                }
                1 => {}
                other => {
                    return Err(eyre::eyre!(
                        "V1 registration receipt has non-canonical status {other}"
                    ));
                }
            }
            let block = receipt
                .get("blockNumber")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| eyre::eyre!("V1 registration receipt has no blockNumber"))?;
            receipt_block = Some(block.to_owned());
        }
        let log_from = receipt_block.as_deref().unwrap_or(&from);
        let log_to = receipt_block.as_deref().unwrap_or("latest");
        let logs = client
            .eth_get_logs(abi::TEE_REGISTRY_ADDR, &topics, log_from, log_to)
            .await
            .wrap_err("poll V1 onboarding event logs")?;
        for log in &logs {
            if let Some(sealed) = matching_offer_key_log(
                log,
                topic0,
                topic1,
                transaction_hash,
                expected_offer_public,
            )? {
                return Ok(sealed);
            }
        }
        if start.elapsed() >= deadline {
            return Err(eyre::eyre!(
                "timed out after {timeout_secs}s waiting for the matching \
                 OfferKeySealedForRegistryV1 transaction event"
            ));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn matching_offer_key_log(
    log: &serde_json::Value,
    topic0: &str,
    topic1: &str,
    transaction_hash: &str,
    expected_offer_public: [u8; 32],
) -> Result<Option<Vec<u8>>> {
    let Some(actual_transaction_hash) = log
        .get("transactionHash")
        .and_then(serde_json::Value::as_str)
    else {
        return Err(eyre::eyre!(
            "OfferKeySealedForRegistryV1 log has no transactionHash"
        ));
    };
    if !actual_transaction_hash.eq_ignore_ascii_case(transaction_hash) {
        return Ok(None);
    }

    let topics = log
        .get("topics")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| eyre::eyre!("matching onboarding log has no topics array"))?;
    if topics.len() != 2
        || topics[0]
            .as_str()
            .is_none_or(|value| !value.eq_ignore_ascii_case(topic0))
        || topics[1]
            .as_str()
            .is_none_or(|value| !value.eq_ignore_ascii_case(topic1))
    {
        return Err(eyre::eyre!(
            "matching transaction emitted a non-canonical onboarding event topic set"
        ));
    }
    let data_hex = log
        .get("data")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("matching onboarding log has no data field"))?;
    let data = hex::decode(data_hex.strip_prefix("0x").unwrap_or(data_hex))
        .wrap_err("decode V1 onboarding event data hex")?;
    decode_offer_key_sealed_data(&data, expected_offer_public).map(Some)
}

fn decode_offer_key_sealed_data(data: &[u8], expected_offer_public: [u8; 32]) -> Result<Vec<u8>> {
    if data.len() < 64 || U256::from_be_slice(&data[..32]) != U256::from(32) {
        return Err(eyre::eyre!(
            "V1 onboarding event has non-canonical bytes offset"
        ));
    }
    let encoded_len = U256::from_be_slice(&data[32..64]);
    if encoded_len > U256::from(MAX_SEALED_OFFER_KEY_FOR_REGISTRY_BYTES) {
        return Err(eyre::eyre!(
            "V1 onboarding artifact exceeds its {}-byte cap",
            MAX_SEALED_OFFER_KEY_FOR_REGISTRY_BYTES
        ));
    }
    let len = encoded_len.to::<usize>();
    if len < MIN_SEALED_OFFER_KEY_FOR_REGISTRY_BYTES {
        return Err(eyre::eyre!(
            "V1 onboarding artifact is shorter than its canonical framing"
        ));
    }
    let padded_len = len
        .checked_add(31)
        .map(|value| value / 32 * 32)
        .ok_or_else(|| eyre::eyre!("V1 onboarding artifact padding overflow"))?;
    let expected_data_len = 64_usize
        .checked_add(padded_len)
        .ok_or_else(|| eyre::eyre!("V1 onboarding event length overflow"))?;
    if data.len() != expected_data_len {
        return Err(eyre::eyre!(
            "V1 onboarding event data length is non-canonical"
        ));
    }
    let sealed_end = 64 + len;
    if data[sealed_end..].iter().any(|byte| *byte != 0) {
        return Err(eyre::eyre!("V1 onboarding event has nonzero ABI padding"));
    }
    let sealed = data[64..sealed_end].to_vec();
    if sealed.get(..32) != Some(expected_offer_public.as_slice()) {
        return Err(eyre::eyre!(
            "V1 onboarding artifact does not commit the active offer public key"
        ));
    }
    Ok(sealed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Bytes;
    use outbe_rpc::test_support::{
        ExpectedRpcCall, RecordedRpcCall, RecordedRpcResponse, RecordingRpc,
    };

    fn sealed_artifact(offer_public: [u8; 32]) -> Vec<u8> {
        let mut sealed = vec![0x44; MIN_SEALED_OFFER_KEY_FOR_REGISTRY_BYTES];
        sealed[..32].copy_from_slice(&offer_public);
        sealed
    }

    #[test]
    fn finalized_join_anchor_binds_the_exact_finalized_view_and_registry_binding() {
        let view = FinalizedRegistryViewV1 {
            chain_id: U256::from(676_u64).to_be_bytes(),
            genesis_hash: B256::repeat_byte(0x11),
            block_number: 91,
            block_hash: B256::repeat_byte(0x12),
            state_root: B256::repeat_byte(0x13),
            consensus_timestamp: 19_000,
        };
        let binding = RenewalBindingV1 {
            node_id_hash: B256::repeat_byte(0x21),
            enclave_id: B256::repeat_byte(0x22),
            binding_id: B256::repeat_byte(0x23),
            intent_hash: B256::repeat_byte(0x24),
            evidence_hash: B256::repeat_byte(0x25),
            policy_hash: B256::repeat_byte(0x26),
            binding_version: 2,
            registration_version: 3,
            renewal_nonce: 0,
            transition_nonce: 0,
            lease_started_at: 18_000,
            valid_until: 20_000,
            collateral_valid_until: 20_000,
            recipient_x25519: B256::repeat_byte(0x31),
            attestation_ed25519: B256::repeat_byte(0x32),
            noise_responder_x25519: B256::repeat_byte(0x33),
            mrenclave: B256::repeat_byte(0x34),
            mrsigner: B256::repeat_byte(0x35),
            isv_prod_id: 1,
            isv_svn: 2,
            platform_tcb_status: 1,
            verdict_hash: B256::repeat_byte(0x36),
            node_host_authorization_hash: B256::repeat_byte(0x37),
        };

        assert_eq!(
            finalized_join_admission_anchor_v1(&view, &binding),
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
        );
    }

    #[test]
    fn binding_id_is_exact_and_nonzero() {
        assert!(parse_nonzero_b256(&"11".repeat(32), "binding").is_ok());
        assert!(parse_nonzero_b256(&"00".repeat(32), "binding").is_err());
        assert!(parse_nonzero_b256(&"11".repeat(31), "binding").is_err());
    }

    #[test]
    fn validator_node_binding_uses_the_global_transaction_signer() {
        let signer = crate::tx::TxSigner::new(
            "11d7b7a4b68f4f6a9f4ec50a4f3b1e6f6294e46147e37030262830716725f9a3",
        )
        .unwrap();
        let (binding, binding_hash, signature) = authorize_validator_node_binding(
            U256::from(676_u64).to_be_bytes(),
            B256::repeat_byte(0x31),
            B256::repeat_byte(0x42),
            &signer,
        )
        .unwrap();

        assert_eq!(binding.validator, signer.address().into_array());
        assert_eq!(binding.binding_hash().unwrap(), binding_hash);
        assert!(binding.verify_validator_signature(&signature));
    }

    #[test]
    fn join_transport_uses_node_host_for_sgx_without_dcap() {
        assert_eq!(
            select_join_transport(AttestationMode::DcapRequired, true).unwrap(),
            JoinTransport::AuthorizedNodeHost
        );
        assert!(select_join_transport(AttestationMode::DcapRequired, false).is_err());
        assert_eq!(
            select_join_transport(AttestationMode::GramineDirectDev, true).unwrap(),
            JoinTransport::AuthorizedNodeHost
        );
        assert_eq!(
            select_join_transport(AttestationMode::GramineDirectDev, false).unwrap(),
            JoinTransport::Development
        );
    }

    #[test]
    fn join_classifies_resident_offer_key_before_relay() {
        let expected_offer_pub = [0x41; 32];
        let public_keys = |offer_key_ready, recipient_x25519_pub| EnclaveResponse::PublicKeys {
            offer_key_ready,
            recipient_x25519_pub,
            attestation_pub: [0x42; 32],
            noise_static_pub: [0x43; 32],
            tee_bls_pub: vec![0x44; 48],
            dkg_enc_pub: [0x45; 32],
            dkg_enc_sig: vec![0x46; 96],
        };

        assert_eq!(
            classify_join_offer_key_state(
                public_keys(true, expected_offer_pub),
                expected_offer_pub,
            )
            .unwrap(),
            JoinOfferKeyState::ReadyExact
        );
        assert_eq!(
            classify_join_offer_key_state(public_keys(false, [0x47; 32]), expected_offer_pub)
                .unwrap(),
            JoinOfferKeyState::Keyless
        );

        let mismatch =
            classify_join_offer_key_state(public_keys(true, [0x48; 32]), expected_offer_pub)
                .unwrap_err();
        assert!(mismatch
            .to_string()
            .contains("resident permanent offer key does not match finalized TeeRegistry"));

        let unexpected =
            classify_join_offer_key_state(EnclaveResponse::Ack, expected_offer_pub).unwrap_err();
        assert!(unexpected
            .to_string()
            .contains("expected enclave PublicKeys"));
    }

    #[test]
    fn join_completion_matrix_preserves_candidate_promotion_without_duplicate_ingest() {
        assert_eq!(
            plan_join_completion(JoinOfferKeyState::Keyless, false),
            JoinCompletionPlan {
                ingest_offer_key: true,
                promote_candidate: false,
            }
        );
        assert_eq!(
            plan_join_completion(JoinOfferKeyState::Keyless, true),
            JoinCompletionPlan {
                ingest_offer_key: true,
                promote_candidate: true,
            }
        );
        assert_eq!(
            plan_join_completion(JoinOfferKeyState::ReadyExact, false),
            JoinCompletionPlan {
                ingest_offer_key: false,
                promote_candidate: false,
            }
        );
        assert_eq!(
            plan_join_completion(JoinOfferKeyState::ReadyExact, true),
            JoinCompletionPlan {
                ingest_offer_key: false,
                promote_candidate: true,
            }
        );
    }

    #[tokio::test]
    async fn finalized_durable_join_does_not_relay_a_second_transaction() {
        let raw_transaction = vec![0x62, 0x63];
        let transaction_hash = keccak256(&raw_transaction);
        let relay = ExactJoinRelayV1 {
            transaction_hash,
            raw_transaction,
        };
        let rpc = RecordingRpc::new([]);

        assert_eq!(
            relay_exact_join_transaction(&rpc, &relay, true)
                .await
                .unwrap(),
            format!("0x{}", hex::encode(transaction_hash))
        );
        rpc.assert_done();
        assert!(rpc.recorded_calls().is_empty());
    }

    #[tokio::test]
    async fn pending_durable_join_relays_only_the_exact_raw_transaction() {
        let raw_transaction = vec![0x71, 0x72];
        let transaction_hash = keccak256(&raw_transaction);
        let relay = ExactJoinRelayV1 {
            transaction_hash,
            raw_transaction: raw_transaction.clone(),
        };
        let encoded_hash = format!("0x{}", hex::encode(transaction_hash));
        let rpc = RecordingRpc::new([ExpectedRpcCall::ok(
            RecordedRpcCall::EthSendRawTransaction {
                raw_tx: raw_transaction,
            },
            RecordedRpcResponse::Text(encoded_hash.clone()),
        )]);

        assert_eq!(
            relay_exact_join_transaction(&rpc, &relay, false)
                .await
                .unwrap(),
            encoded_hash
        );
        rpc.assert_done();
    }

    #[tokio::test]
    async fn already_known_durable_join_preserves_the_exact_transaction_identity() {
        let raw_transaction = vec![0x73, 0x74];
        let transaction_hash = keccak256(&raw_transaction);
        let relay = ExactJoinRelayV1 {
            transaction_hash,
            raw_transaction: raw_transaction.clone(),
        };
        let encoded_hash = format!("0x{}", hex::encode(transaction_hash));
        let rpc = RecordingRpc::new([ExpectedRpcCall::err(
            RecordedRpcCall::EthSendRawTransaction {
                raw_tx: raw_transaction,
            },
            "already known",
        )]);

        assert_eq!(
            relay_exact_join_transaction(&rpc, &relay, false)
                .await
                .unwrap(),
            encoded_hash
        );
        rpc.assert_done();
    }

    #[tokio::test]
    async fn nonce_too_low_is_accepted_only_for_the_exact_durable_join_receipt() {
        let raw_transaction = vec![0x75, 0x76];
        let transaction_hash = keccak256(&raw_transaction);
        let relay = ExactJoinRelayV1 {
            transaction_hash,
            raw_transaction: raw_transaction.clone(),
        };
        let encoded_hash = format!("0x{}", hex::encode(transaction_hash));
        let rpc = RecordingRpc::new([
            ExpectedRpcCall::err(
                RecordedRpcCall::EthSendRawTransaction {
                    raw_tx: raw_transaction,
                },
                "nonce too low",
            ),
            ExpectedRpcCall::ok(
                RecordedRpcCall::EthGetTransactionReceipt {
                    transaction_hash: encoded_hash.clone(),
                },
                RecordedRpcResponse::OptionalValue(Some(serde_json::json!({
                    "transactionHash": encoded_hash,
                }))),
            ),
        ]);

        assert_eq!(
            relay_exact_join_transaction(&rpc, &relay, false)
                .await
                .unwrap(),
            format!("0x{}", hex::encode(transaction_hash))
        );
        rpc.assert_done();
    }

    #[test]
    fn committed_submission_rejects_a_different_global_evm_signer() {
        let original = Address::repeat_byte(0x41);
        let replacement = Address::repeat_byte(0x42);

        ensure_durable_join_registration_caller(Some(original), original).unwrap();
        ensure_durable_join_registration_caller(None, replacement).unwrap();
        assert!(
            ensure_durable_join_registration_caller(Some(original), replacement)
                .unwrap_err()
                .to_string()
                .contains("different global --private-key")
        );
    }

    #[test]
    fn missing_committed_relay_is_only_recoverable_after_ready_exact_completion() {
        assert_eq!(
            plan_missing_committed_relay(true, JoinOfferKeyState::ReadyExact).unwrap(),
            MissingCommittedRelayPlan::CleanupReadyExact
        );
        assert_eq!(
            plan_missing_committed_relay(false, JoinOfferKeyState::Keyless).unwrap(),
            MissingCommittedRelayPlan::ConstructAndPersist
        );
        assert!(
            plan_missing_committed_relay(true, JoinOfferKeyState::Keyless)
                .unwrap_err()
                .to_string()
                .contains("no durable pre-relay transaction checkpoint")
        );
    }

    #[test]
    fn expired_rejoin_uses_exact_next_binding_and_registration_versions() {
        let mut binding = test_renewal_binding();
        binding.binding_version = 7;
        binding.registration_version = 11;
        binding.renewal_nonce = 5;
        binding.transition_nonce = 3;
        assert_eq!(
            registration_counters(Some(&binding)).unwrap(),
            (8, 12, 5, 3)
        );
        assert_eq!(registration_counters(None).unwrap(), (1, 0, 0, 0));
    }

    #[test]
    fn tee_join_rejects_a_live_binding_but_accepts_the_deadline_as_expired() {
        let mut binding = test_renewal_binding();
        binding.valid_until = 200;
        assert!(ensure_joinable_binding(Some(&binding), 199).is_err());
        assert!(ensure_joinable_binding(Some(&binding), 200).is_ok());
        assert!(ensure_joinable_binding(Some(&binding), 201).is_ok());
        assert!(ensure_joinable_binding(None, 199).is_ok());
    }

    fn test_renewal_binding() -> outbe_operator::tee::RenewalBindingV1 {
        outbe_operator::tee::RenewalBindingV1 {
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
            isv_svn: 1,
            platform_tcb_status: 0,
            verdict_hash: B256::repeat_byte(12),
            node_host_authorization_hash: B256::repeat_byte(13),
        }
    }

    #[test]
    fn full_node_reth_identity_accepts_equivalent_raw_and_hex_key_files() {
        let directory = tempfile::tempdir().unwrap();
        let secret = [0x61; 32];
        let raw_path = directory.path().join("reth-p2p.raw");
        let hex_path = directory.path().join("reth-p2p.hex");
        fs::write(&raw_path, secret).unwrap();
        fs::write(&hex_path, format!("0x{}\n", hex::encode(secret))).unwrap();

        let raw = load_secp256k1_key_file(&raw_path).unwrap();
        let hex = load_secp256k1_key_file(&hex_path).unwrap();
        let expected = k256::ecdsa::SigningKey::from_bytes((&secret).into()).unwrap();
        assert_eq!(
            raw.verifying_key().to_encoded_point(true),
            expected.verifying_key().to_encoded_point(true)
        );
        assert_eq!(
            hex.verifying_key().to_encoded_point(true),
            expected.verifying_key().to_encoded_point(true)
        );
    }

    #[test]
    fn full_node_reth_identity_rejects_invalid_secret_files() {
        let directory = tempfile::tempdir().unwrap();
        let empty = directory.path().join("empty");
        let wrong_width = directory.path().join("wrong-width.hex");
        let zero = directory.path().join("zero.raw");
        fs::write(&empty, []).unwrap();
        fs::write(&wrong_width, "11".repeat(31)).unwrap();
        fs::write(&zero, [0_u8; 32]).unwrap();

        assert!(load_secp256k1_key_file(&empty).is_err());
        assert!(load_secp256k1_key_file(&wrong_width).is_err());
        assert!(load_secp256k1_key_file(&zero).is_err());
    }

    #[test]
    fn onboarding_event_decoder_requires_canonical_bounded_abi_and_offer_key() {
        let offer_public = [0x22; 32];
        let sealed = sealed_artifact(offer_public);
        let encoded = Bytes::from(sealed.clone()).abi_encode();
        assert_eq!(
            decode_offer_key_sealed_data(&encoded, offer_public).unwrap(),
            sealed
        );

        let mut bad_offset = encoded.clone();
        bad_offset[31] = 0x40;
        assert!(decode_offer_key_sealed_data(&bad_offset, offer_public).is_err());

        let mut bad_padding = encoded.clone();
        *bad_padding.last_mut().unwrap() = 1;
        assert!(decode_offer_key_sealed_data(&bad_padding, offer_public).is_err());

        let mut trailing = encoded.clone();
        trailing.extend_from_slice(&[0; 32]);
        assert!(decode_offer_key_sealed_data(&trailing, offer_public).is_err());

        assert!(decode_offer_key_sealed_data(&encoded, [0x23; 32]).is_err());
    }

    #[test]
    fn onboarding_log_is_correlated_to_transaction_and_topics() {
        let offer_public = [0x33; 32];
        let sealed = sealed_artifact(offer_public);
        let data = Bytes::from(sealed.clone()).abi_encode();
        let log = serde_json::json!({
            "transactionHash": "0xaaaa",
            "topics": ["0xtopic0", "0xtopic1"],
            "data": format!("0x{}", hex::encode(data)),
        });
        assert_eq!(
            matching_offer_key_log(&log, "0xtopic0", "0xtopic1", "0xAAAA", offer_public,).unwrap(),
            Some(sealed.clone())
        );
        assert_eq!(
            matching_offer_key_log(&log, "0xtopic0", "0xtopic1", "0xbbbb", offer_public,).unwrap(),
            None
        );

        let wrong_topic = serde_json::json!({
            "transactionHash": "0xaaaa",
            "topics": ["0xtopic0", "0xwrong"],
            "data": format!("0x{}", hex::encode(Bytes::from(sealed).abi_encode())),
        });
        assert!(matching_offer_key_log(
            &wrong_topic,
            "0xtopic0",
            "0xtopic1",
            "0xaaaa",
            offer_public,
        )
        .is_err());
    }

    #[tokio::test]
    async fn onboarding_poll_reports_reverted_receipt_before_event_timeout() {
        let transaction_hash = "0xaaaa";
        let client = crate::rpc::mock::MockRpc {
            transaction_receipt: Ok(Some(serde_json::json!({
                "transactionHash": transaction_hash,
                "blockNumber": "0xc",
                "status": "0x0",
                "logs": [],
            }))),
            logs: Ok(Vec::new()),
            ..Default::default()
        };

        let error = poll_offer_key_sealed(
            &client,
            "0xtopic0",
            "0xtopic1",
            transaction_hash,
            [0x33; 32],
            11,
            0,
        )
        .await
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("reverted"), "unexpected error: {message}");
        assert!(message.contains("0xc"), "missing block number: {message}");
    }
}
