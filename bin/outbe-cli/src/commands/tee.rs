//! `outbe-cli tee` — V1 TEE registration for a joining validator or full node.
//!
//! Pre-start flow: before launching `outbe-chain node` on a
//! TEE-bootstrapped chain, the joiner registers its enclave on-chain
//! (`registerEnclave(bytes,bytes,bytes)`), reads the deterministically sealed offer
//! key from its own transaction log (`OfferKeySealedForRegistryV1`), and installs
//! it in its enclave. Only
//! then can the node execute offer blocks. Mirrors `secretd tx register auth` +
//! `q register seed` + `configure-secret`, run before `secretd start`.

use std::{fs, path::PathBuf, time::Duration};

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use clap::{Subcommand, ValueEnum};
use eyre::{Result, WrapErr};
use k256::ecdsa::signature::hazmat::PrehashSigner as _;
use outbe_operator::{
    rpc::{FinalityRpc, RenewalRpc},
    tee::{
        await_finalized_onboarding_v1, copy_same_platform_sealed_root_and_checkpoint_v1,
        inspect_upgrade_journal_v1, prepare_upgrade_journal_v1,
        read_finalized_staged_successor_policy_v1, read_renewal_status_v1, run_renewal_once_v1,
        run_upgrade_submission_v1, ExpectedOnboardingBindingV1, NodeBindingSelectorV1,
        RenewalModeV1, RenewalServiceConfigV1, UpgradeContextV1,
    },
    tx::RelaySignerV1,
};
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, AttestationMode, AttestationOperationV1, DcapEvidenceV1, EnclaveProfile,
    GramineDirectEvidenceV1, NodeIdV1, RegistrationIntentV1, RegistryMutatorV1, TeePolicyV1,
    TeeRegistryGasScheduleV1, ENCLAVE_ID_DOMAIN_V1,
};
use outbe_tee::protocol::{
    EnclaveRequest, EnclaveResponse, MAX_SEALED_OFFER_KEY_FOR_REGISTRY_BYTES,
    MIN_SEALED_OFFER_KEY_FOR_REGISTRY_BYTES,
};
use outbe_tee::{
    acquire_dcap_collateral_v1, load_committed_enclave_manifest_v1, EnclaveClient,
    FullNodeNodeHostIdentityV1, ReplacementCandidateEnclaveV1, RuntimeEnclaveClient,
    ValidatorNodeHostIdentityV1,
};
use zeroize::Zeroizing;

use crate::abi::{self, ITeeRegistry};
use crate::rpc::Rpc;

const DEV_NODE_HOST_DOMAIN_V1: &[u8] = b"outbe/tee/dev-node-host/v1";

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

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum TeeNodeProfile {
    Validator,
    FullNode,
}

#[derive(Subcommand)]
pub enum TeeCmd {
    /// Register this node's enclave on-chain and install the offer key it is sealed.
    /// Run BEFORE `outbe-chain node` when joining a running TEE-bootstrapped chain.
    Join {
        /// Enclave sidecar endpoint: a UDS path or a `host:port` (Gramine) address.
        #[arg(long)]
        enclave_socket: String,
        /// Node identity registered by this intent.
        #[arg(long, value_enum)]
        profile: TeeNodeProfile,
        /// Resolved chain-specific Reth data directory. Required by
        /// DcapRequired NodeHost initialization.
        #[arg(long)]
        node_data_dir: Option<PathBuf>,
        /// Validator node-authority EVM key. Defaults to the global relay
        /// private key. Supplying both proves that an unrelated account may relay.
        #[arg(long)]
        node_private_key: Option<String>,
        /// Validator Commonware BLS MinPk public key as exactly 48 bytes of hex.
        #[arg(long)]
        consensus_bls_public: Option<String>,
        /// Full-node persistent Reth P2P secp256k1 secret file.
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
    /// Generate, durably journal and submit one renewal through the same
    /// reducer used by the node's automatic worker.
    RenewNow {
        /// Production enclave sidecar endpoint.
        #[arg(long)]
        enclave_socket: String,
        /// Resolved chain-specific node data directory containing NodeHost state.
        #[arg(long)]
        node_data_dir: PathBuf,
        /// Validator node-authority key. This is distinct from the global
        /// funded relay key and defaults to it only when explicitly supplied.
        #[arg(long)]
        node_private_key: Option<String>,
        /// Full-node persistent Reth P2P secret file.
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
        node_private_key: Option<String>,
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
    /// prepare exact transition bytes and submit them through the funded relay.
    UpgradeSubmit {
        #[arg(long)]
        candidate_enclave_socket: String,
        #[arg(long)]
        node_data_dir: PathBuf,
        #[arg(long)]
        node_private_key: Option<String>,
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
    /// and assert it MATCHES the enclave — exits non-zero on a registry-vs-enclave
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
                profile,
                node_data_dir,
                node_private_key,
                consensus_bls_public,
                reth_p2p_secret_key,
                binding_id,
                valid_until,
                timeout_secs,
            } => {
                join(
                    client,
                    private_key,
                    &enclave_socket,
                    profile,
                    node_data_dir.as_deref(),
                    node_private_key.as_deref(),
                    consensus_bls_public.as_deref(),
                    reth_p2p_secret_key.as_deref(),
                    &binding_id,
                    valid_until,
                    timeout_secs,
                )
                .await
            }
            TeeCmd::RenewNow {
                enclave_socket,
                node_data_dir,
                node_private_key,
                reth_p2p_secret_key,
            } => {
                renew_now(
                    client,
                    private_key,
                    &enclave_socket,
                    &node_data_dir,
                    node_private_key.as_deref(),
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
                node_private_key,
                reth_p2p_secret_key,
            } => {
                upgrade_prepare(
                    client,
                    private_key,
                    &candidate_enclave_socket,
                    &node_data_dir,
                    &active_tee_dir,
                    &candidate_tee_dir,
                    node_private_key.as_deref(),
                    reth_p2p_secret_key.as_deref(),
                )
                .await
            }
            TeeCmd::UpgradeCopyRoot { node_data_dir } => upgrade_copy_root(&node_data_dir),
            TeeCmd::UpgradeSubmit {
                candidate_enclave_socket,
                node_data_dir,
                node_private_key,
                reth_p2p_secret_key,
                binding_id,
                valid_until,
            } => {
                upgrade_submit(
                    client,
                    private_key,
                    &candidate_enclave_socket,
                    &node_data_dir,
                    node_private_key.as_deref(),
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

async fn renew_now(
    client: &(impl Rpc + Sync),
    relay_private_key: Option<&str>,
    enclave_socket: &str,
    node_data_dir: &std::path::Path,
    node_private_key: Option<&str>,
    reth_p2p_secret_key: Option<&std::path::Path>,
) -> Result<()> {
    let relay_key = relay_private_key.ok_or_else(|| {
        eyre::eyre!("tee renew-now requires the global --private-key funded relay signer")
    })?;
    let relay = RelaySignerV1::new(relay_key)?;
    let manifest = load_committed_enclave_manifest_v1(node_data_dir)
        .map_err(|error| eyre::eyre!("load committed NodeHost manifest: {error}"))?;
    let rpc_chain_id = client.eth_chain_id().await?;
    let (selector, mut enclave, node_signing_key) = match &manifest.node_id {
        NodeIdV1::Validator {
            address,
            bls_minpk_public,
        } => {
            if reth_p2p_secret_key.is_some() {
                eyre::bail!("--reth-p2p-secret-key is invalid for a Validator manifest");
            }
            let key = node_private_key.or(relay_private_key).ok_or_else(|| {
                eyre::eyre!("Validator renewal requires --node-private-key or the global signer")
            })?;
            let signing = crate::tx::TxSigner::new(key)?.key().clone();
            let enclave = outbe_tee::connect_or_initialize_validator_enclave(
                enclave_socket,
                node_data_dir,
                ValidatorNodeHostIdentityV1 {
                    chain_id: rpc_chain_id,
                    genesis_hash: manifest.genesis_hash,
                    validator: Address::from(*address),
                    consensus_bls_public: *bls_minpk_public,
                },
                |hash| sign_node_hash(&signing, hash),
            )
            .map_err(|error| eyre::eyre!("connect Validator NodeHost enclave: {error}"))?;
            (
                NodeBindingSelectorV1::Validator(Address::from(*address)),
                enclave,
                signing,
            )
        }
        NodeIdV1::FullNode { reth_p2p_public } => {
            if node_private_key.is_some() {
                eyre::bail!("--node-private-key is invalid for a FullNode manifest");
            }
            let path = reth_p2p_secret_key
                .ok_or_else(|| eyre::eyre!("FullNode renewal requires --reth-p2p-secret-key"))?;
            let signing = load_secp256k1_key_file(path)?;
            let enclave = outbe_tee::connect_or_initialize_full_node_enclave(
                enclave_socket,
                node_data_dir,
                FullNodeNodeHostIdentityV1 {
                    chain_id: rpc_chain_id,
                    genesis_hash: manifest.genesis_hash,
                    reth_p2p_public: *reth_p2p_public,
                },
                |hash| sign_node_hash(&signing, hash),
            )
            .map_err(|error| eyre::eyre!("connect FullNode NodeHost enclave: {error}"))?;
            (
                NodeBindingSelectorV1::FullNode(*reth_p2p_public),
                enclave,
                signing,
            )
        }
    };
    let config = RenewalServiceConfigV1 {
        node_data_dir: node_data_dir.to_path_buf(),
        selector,
        manifest,
    };
    let signer = |hash| {
        sign_node_hash(&node_signing_key, hash)
            .map_err(|error| eyre::eyre!("node authority signing failed: {error}"))
    };
    let outcome = run_renewal_once_v1(
        &CliFinalityRpc(client),
        &relay,
        &mut enclave,
        &signer,
        &config,
        RenewalModeV1::Manual,
    )
    .await?;
    println!("{outcome:#?}");
    Ok(())
}

async fn renewal_status(
    client: &(impl Rpc + Sync),
    node_data_dir: &std::path::Path,
    warning_blocks: u64,
    critical_blocks: u64,
) -> Result<()> {
    let manifest = load_committed_enclave_manifest_v1(node_data_dir)
        .map_err(|error| eyre::eyre!("load committed NodeHost manifest: {error}"))?;
    let selector = match manifest.node_id {
        NodeIdV1::Validator { address, .. } => {
            NodeBindingSelectorV1::Validator(Address::from(address))
        }
        NodeIdV1::FullNode { reth_p2p_public } => NodeBindingSelectorV1::FullNode(reth_p2p_public),
    };
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
    relay_private_key: Option<&str>,
    candidate_enclave_socket: &str,
    node_data_dir: &std::path::Path,
    active_tee_dir: &std::path::Path,
    candidate_tee_dir: &std::path::Path,
    node_private_key: Option<&str>,
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
        relay_private_key,
        node_private_key,
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
    relay_private_key: Option<&str>,
    candidate_enclave_socket: &str,
    node_data_dir: &std::path::Path,
    node_private_key: Option<&str>,
    reth_p2p_secret_key: Option<&std::path::Path>,
    binding_id: &str,
    valid_until: u64,
) -> Result<()> {
    let relay_private_key = relay_private_key.ok_or_else(|| {
        eyre::eyre!("tee upgrade-submit requires the global --private-key funded relay signer")
    })?;
    let relay = RelaySignerV1::new(relay_private_key)?;
    let active = load_committed_enclave_manifest_v1(node_data_dir)
        .map_err(|error| eyre::eyre!("load committed NodeHost manifest: {error}"))?;
    let rpc_chain_id = client.eth_chain_id().await?;
    let (mut candidate, node_signing_key, selector) = connect_upgrade_candidate_v1(
        candidate_enclave_socket,
        node_data_dir,
        &active,
        rpc_chain_id,
        Some(relay_private_key),
        node_private_key,
        reth_p2p_secret_key,
    )?;
    let signer = |hash| {
        sign_node_hash(&node_signing_key, hash)
            .map_err(|error| eyre::eyre!("node authority signing failed: {error}"))
    };
    let outcome = run_upgrade_submission_v1(
        &CliFinalityRpc(client),
        &relay,
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
    relay_private_key: Option<&str>,
    node_private_key: Option<&str>,
    reth_p2p_secret_key: Option<&std::path::Path>,
) -> Result<(
    ReplacementCandidateEnclaveV1,
    k256::ecdsa::SigningKey,
    NodeBindingSelectorV1,
)> {
    if active.chain_id != U256::from(rpc_chain_id).to_be_bytes() {
        eyre::bail!("committed NodeHost manifest chain id does not match eth_chainId");
    }
    match &active.node_id {
        NodeIdV1::Validator {
            address,
            bls_minpk_public,
        } => {
            if reth_p2p_secret_key.is_some() {
                eyre::bail!("--reth-p2p-secret-key is invalid for a Validator upgrade");
            }
            let key = node_private_key.or(relay_private_key).ok_or_else(|| {
                eyre::eyre!("Validator upgrade requires --node-private-key or the global signer")
            })?;
            let signing = crate::tx::TxSigner::new(key)?.key().clone();
            let candidate = outbe_tee::prepare_validator_enclave_replacement_candidate(
                candidate_enclave_socket,
                node_data_dir,
                ValidatorNodeHostIdentityV1 {
                    chain_id: rpc_chain_id,
                    genesis_hash: active.genesis_hash,
                    validator: Address::from(*address),
                    consensus_bls_public: *bls_minpk_public,
                },
                |hash| sign_node_hash(&signing, hash),
            )
            .map_err(|error| eyre::eyre!("prepare Validator candidate B: {error}"))?;
            Ok((
                candidate,
                signing,
                NodeBindingSelectorV1::Validator(Address::from(*address)),
            ))
        }
        NodeIdV1::FullNode { reth_p2p_public } => {
            if node_private_key.is_some() {
                eyre::bail!("--node-private-key is invalid for a FullNode upgrade");
            }
            let path = reth_p2p_secret_key
                .ok_or_else(|| eyre::eyre!("FullNode upgrade requires --reth-p2p-secret-key"))?;
            let signing = load_secp256k1_key_file(path)?;
            let candidate = outbe_tee::prepare_full_node_enclave_replacement_candidate(
                candidate_enclave_socket,
                node_data_dir,
                FullNodeNodeHostIdentityV1 {
                    chain_id: rpc_chain_id,
                    genesis_hash: active.genesis_hash,
                    reth_p2p_public: *reth_p2p_public,
                },
                |hash| sign_node_hash(&signing, hash),
            )
            .map_err(|error| eyre::eyre!("prepare FullNode candidate B: {error}"))?;
            Ok((
                candidate,
                signing,
                NodeBindingSelectorV1::FullNode(*reth_p2p_public),
            ))
        }
    }
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
            "on-chain tributeOfferPublicKey == 0 — chain is not TEE-bootstrapped yet, \
             nothing to diff against"
        ));
    }
    let onchain_bytes: [u8; 32] = onchain.to_be_bytes();
    println!(
        "on-chain tributeOfferPublicKey (slot-1): 0x{}",
        hex::encode(onchain_bytes)
    );
    if onchain_bytes == offer_pub {
        println!("✓ MATCH — enclave resident offer key == on-chain registry");
        Ok(())
    } else {
        Err(eyre::eyre!(
            "✗ MISMATCH — enclave offer key 0x{} != on-chain 0x{}",
            hex::encode(offer_pub),
            hex::encode(onchain_bytes)
        ))
    }
}

// CLI command handler: args map 1:1 to `tee join` flags.
#[allow(clippy::too_many_arguments)]
async fn join(
    client: &(impl Rpc + Sync),
    private_key: Option<&str>,
    enclave_socket: &str,
    profile: TeeNodeProfile,
    node_data_dir: Option<&std::path::Path>,
    node_private_key: Option<&str>,
    consensus_bls_public: Option<&str>,
    reth_p2p_secret_key: Option<&std::path::Path>,
    binding_id: &str,
    valid_until: u64,
    timeout_secs: u64,
) -> Result<()> {
    let relay = super::require_signer(private_key)?;
    let policy = active_policy_v1(client).await?;
    let rpc_chain_id = client.eth_chain_id().await?;
    if policy.chain_id != U256::from(rpc_chain_id).to_be_bytes() {
        return Err(eyre::eyre!(
            "active V1 policy chain id does not match eth_chainId"
        ));
    }
    let binding_id = parse_nonzero_b256(binding_id, "--binding-id")?;
    let now = latest_block_timestamp(client).await?;
    let lease = valid_until
        .checked_sub(now)
        .ok_or_else(|| eyre::eyre!("--valid-until is not in the future"))?;
    if lease < policy.minimum_lease || lease > policy.maximum_lease {
        return Err(eyre::eyre!(
            "--valid-until lease {lease}s is outside active policy range {}..={}s",
            policy.minimum_lease,
            policy.maximum_lease
        ));
    }

    let (node_id, node_signing_key) = match profile {
        TeeNodeProfile::Validator => {
            if reth_p2p_secret_key.is_some() {
                return Err(eyre::eyre!(
                    "--reth-p2p-secret-key is only valid for --profile full-node"
                ));
            }
            let consensus = parse_hex_array::<48>(
                consensus_bls_public.ok_or_else(|| {
                    eyre::eyre!("--profile validator requires --consensus-bls-public")
                })?,
                "--consensus-bls-public",
            )?;
            let node_key = node_private_key.or(private_key).ok_or_else(|| {
                eyre::eyre!(
                    "validator node authority requires --node-private-key or the global --private-key"
                )
            })?;
            let node_signer = crate::tx::TxSigner::new(node_key)?;
            (
                NodeIdV1::Validator {
                    address: node_signer.address().into_array(),
                    bls_minpk_public: consensus,
                },
                node_signer.key().clone(),
            )
        }
        TeeNodeProfile::FullNode => {
            if node_private_key.is_some() || consensus_bls_public.is_some() {
                return Err(eyre::eyre!(
                    "validator node-authority arguments are invalid for --profile full-node"
                ));
            }
            let path = reth_p2p_secret_key
                .ok_or_else(|| eyre::eyre!("--profile full-node requires --reth-p2p-secret-key"))?;
            let signing = load_secp256k1_key_file(path)?;
            let public: [u8; 33] = signing
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .map_err(|_| eyre::eyre!("Reth P2P public key is not compressed SEC1-33"))?;
            (
                NodeIdV1::FullNode {
                    reth_p2p_public: public,
                },
                signing,
            )
        }
    };
    let enclave_profile = match profile {
        TeeNodeProfile::Validator => EnclaveProfile::Validator,
        TeeNodeProfile::FullNode => EnclaveProfile::FullNode,
    };
    let binding_selector = match &node_id {
        NodeIdV1::Validator { address, .. } => {
            NodeBindingSelectorV1::Validator(Address::from(*address))
        }
        NodeIdV1::FullNode { reth_p2p_public } => NodeBindingSelectorV1::FullNode(*reth_p2p_public),
    };
    let node_id_for_reopen = node_id.clone();

    // Read the permanent chain key before generating a fresh quote. Its exact
    // value is later authenticated again inside the recipient enclave.
    let offer_pub_u256 = call_u256(
        client,
        ITeeRegistry::tributeOfferPublicKeyCall {}.abi_encode(),
    )
    .await?;
    if offer_pub_u256.is_zero() {
        return Err(eyre::eyre!(
            "chain has no bootstrapped tribute offer key (tributeOfferPublicKey == 0) — \
             not a TEE chain, or it has not bootstrapped TEE yet"
        ));
    }
    let expected_offer_pub: [u8; 32] = offer_pub_u256.to_be_bytes();
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

    let (
        mut enclave,
        recipient_x25519,
        attestation_ed25519,
        noise_responder_x25519,
        enclave_id,
        node_host_authorization_hash,
    ) = match policy.attestation_mode {
        AttestationMode::DcapRequired => {
            let node_data_dir = node_data_dir
                .ok_or_else(|| eyre::eyre!("DcapRequired tee join requires --node-data-dir"))?;
            fs::create_dir_all(node_data_dir).wrap_err_with(|| {
                format!(
                    "create DcapRequired node data directory {}",
                    node_data_dir.display()
                )
            })?;
            let signer = |hash| sign_node_hash(&node_signing_key, hash);
            let client = match (&node_id, profile) {
                (
                    NodeIdV1::Validator {
                        address,
                        bls_minpk_public,
                    },
                    TeeNodeProfile::Validator,
                ) => outbe_tee::connect_or_initialize_validator_enclave(
                    enclave_socket,
                    node_data_dir,
                    ValidatorNodeHostIdentityV1 {
                        chain_id: rpc_chain_id,
                        genesis_hash: policy.genesis_hash,
                        validator: Address::from(*address),
                        consensus_bls_public: *bls_minpk_public,
                    },
                    signer,
                ),
                (NodeIdV1::FullNode { reth_p2p_public }, TeeNodeProfile::FullNode) => {
                    outbe_tee::connect_or_initialize_full_node_enclave(
                        enclave_socket,
                        node_data_dir,
                        FullNodeNodeHostIdentityV1 {
                            chain_id: rpc_chain_id,
                            genesis_hash: policy.genesis_hash,
                            reth_p2p_public: *reth_p2p_public,
                        },
                        signer,
                    )
                }
                _ => unreachable!("profile and node id are constructed together"),
            }
            .map_err(|error| eyre::eyre!("production NodeHost initialization failed: {error}"))?;
            let manifest = load_committed_enclave_manifest_v1(node_data_dir)
                .map_err(|error| eyre::eyre!("load committed NodeHost manifest: {error}"))?;
            if manifest.chain_id != policy.chain_id
                || manifest.genesis_hash != policy.genesis_hash
                || manifest.enclave_profile != enclave_profile
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
                RuntimeEnclaveClient::Production(client),
                manifest.recipient_x25519,
                manifest.attestation_ed25519,
                manifest.noise_responder_x25519,
                enclave_id,
                authorization,
            )
        }
        AttestationMode::GramineDirectDev => {
            if node_data_dir.is_some() {
                return Err(eyre::eyre!(
                    "GramineDirectDev does not consume production --node-data-dir NodeHost state"
                ));
            }
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
                RuntimeEnclaveClient::Development(development),
                recipient,
                attestation,
                noise,
                enclave_id,
                authorization,
            )
        }
    };

    let intent = RegistrationIntentV1 {
        chain_id: policy.chain_id,
        genesis_hash: policy.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: policy.attestation_mode,
        policy_hash,
        enclave_profile,
        node_id,
        enclave_id,
        binding_id,
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: valid_until,
        recipient_x25519,
        attestation_ed25519,
        noise_responder_x25519,
        node_host_authorization_hash,
    };
    let intent_hash = intent
        .intent_hash()
        .map_err(|error| eyre::eyre!("invalid V1 registration intent: {error}"))?;
    let node_signature = sign_node_hash(&node_signing_key, intent_hash)
        .map_err(|error| eyre::eyre!("sign V1 node intent: {error}"))?;
    let (evidence, enclave_signature) = match &mut enclave {
        RuntimeEnclaveClient::Production(client) => {
            let generated = client
                .generate_dcap_quote(&intent)
                .map_err(|error| eyre::eyre!("generate intent-bound DCAP quote: {error}"))?;
            let components = acquire_dcap_collateral_v1(&generated.quote_body)
                .map_err(|error| eyre::eyre!("acquire canonical DCAP collateral: {error}"))?;
            let signature = generated.enclave_signature;
            (
                AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
                    intent,
                    quote: generated.quote_body,
                    components,
                    transition_key_ready_proof: generated.transition_key_ready_proof,
                }),
                signature,
            )
        }
        RuntimeEnclaveClient::Development(client) => {
            let signature = client
                .sign_registration_intent_dev_v1(&intent)
                .map_err(|error| eyre::eyre!("sign development V1 intent: {error}"))?;
            (
                AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
                    intent,
                    dev_attestation_public: attestation_ed25519,
                    dev_signature: signature,
                }),
                signature,
            )
        }
    };
    let evidence = evidence
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

    // Any funded account may relay this transaction; the exact node and enclave
    // proofs above are the only registration authority.
    let from_block = client.eth_block_number().await?;
    let tx_hash = relay
        .send_tx_with_gas(client, abi::TEE_REGISTRY_ADDR, call, U256::ZERO, gas_limit)
        .await
        .wrap_err("V1 registerEnclave submission failed")?;
    println!(
        "V1 registerEnclave submitted by {}: {tx_hash}",
        relay.address()
    );

    // Accept only the artifact indexed by the canonical node identity proved in
    // the same transaction.
    let topic0 = format!(
        "0x{}",
        hex::encode(ITeeRegistry::OfferKeySealedForRegistryV1::SIGNATURE_HASH)
    );
    let topic1 = format!("0x{}", hex::encode(node_id_hash));
    let resp = match policy.attestation_mode {
        AttestationMode::DcapRequired => {
            let expected = ExpectedOnboardingBindingV1 {
                selector: binding_selector,
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
            let artifact = finalized
                .artifact
                .encode_canonical()
                .map_err(|code| eyre::eyre!("encode finalized artifact: {:#06x}", code.code()))?;
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
    };
    match resp {
        EnclaveResponse::OfferKeyForRegistryIngested {
            tribute_offer_public,
        } => {
            if policy.attestation_mode == AttestationMode::DcapRequired {
                drop(enclave);
                let node_data_dir = node_data_dir.ok_or_else(|| {
                    eyre::eyre!("DcapRequired onboarding lost its required node data directory")
                })?;
                let signer = |hash| sign_node_hash(&node_signing_key, hash);
                let mut reopened = match (&node_id_for_reopen, profile) {
                    (
                        NodeIdV1::Validator {
                            address,
                            bls_minpk_public,
                        },
                        TeeNodeProfile::Validator,
                    ) => outbe_tee::connect_or_initialize_validator_enclave(
                        enclave_socket,
                        node_data_dir,
                        ValidatorNodeHostIdentityV1 {
                            chain_id: rpc_chain_id,
                            genesis_hash: policy.genesis_hash,
                            validator: Address::from(*address),
                            consensus_bls_public: *bls_minpk_public,
                        },
                        signer,
                    ),
                    (NodeIdV1::FullNode { reth_p2p_public }, TeeNodeProfile::FullNode) => {
                        outbe_tee::connect_or_initialize_full_node_enclave(
                            enclave_socket,
                            node_data_dir,
                            FullNodeNodeHostIdentityV1 {
                                chain_id: rpc_chain_id,
                                genesis_hash: policy.genesis_hash,
                                reth_p2p_public: *reth_p2p_public,
                            },
                            signer,
                        )
                    }
                    _ => unreachable!("profile and node id were constructed together"),
                }
                .map_err(|error| eyre::eyre!("reopen initialized enclave: {error}"))?;
                match reopened.request(&EnclaveRequest::GetPublicKeys)? {
                    EnclaveResponse::PublicKeys {
                        offer_key_ready: true,
                        recipient_x25519_pub,
                        ..
                    } if recipient_x25519_pub == expected_offer_pub => {}
                    other => {
                        return Err(eyre::eyre!(
                            "durable onboarding reopen did not expose the exact permanent offer key: {other:?}"
                        ))
                    }
                }
            }
            if policy.attestation_mode == AttestationMode::DcapRequired {
                println!(
                    "✓ offer key durably installed and the authenticated enclave connection reopened (offer_public 0x{}). \
                     You can now start outbe-chain node.",
                    hex::encode(tribute_offer_public)
                );
            } else {
                println!(
                    "✓ development offer key installed in enclave (offer_public 0x{}). \
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

/// `eth_call` a view returning a single `uint256`.
async fn call_u256(client: &(impl Rpc + Sync), call: Vec<u8>) -> Result<U256> {
    let result = client.eth_call(abi::TEE_REGISTRY_ADDR, &call).await?;
    U256::abi_decode(&result).wrap_err("decode uint256")
}

async fn active_policy_v1(client: &(impl Rpc + Sync)) -> Result<TeePolicyV1> {
    let result = client
        .eth_call(
            abi::TEE_REGISTRY_ADDR,
            &ITeeRegistry::activePolicyV1Call {}.abi_encode(),
        )
        .await
        .wrap_err("read active V1 attestation policy")?;
    let canonical: Bytes = ITeeRegistry::activePolicyV1Call::abi_decode_returns(&result)
        .wrap_err("decode activePolicyV1 return")?;
    TeePolicyV1::decode_canonical(&canonical)
        .map_err(|error| eyre::eyre!("active V1 attestation policy is non-canonical: {error}"))
}

async fn latest_block_timestamp(client: &(impl Rpc + Sync)) -> Result<u64> {
    let block = client
        .eth_get_latest_block()
        .await
        .wrap_err("read latest block for registration lease")?;
    let encoded = block
        .get("timestamp")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("latest block has no hexadecimal timestamp"))?;
    let encoded = encoded.strip_prefix("0x").unwrap_or(encoded);
    if encoded.is_empty() {
        return Err(eyre::eyre!("latest block timestamp is empty"));
    }
    u64::from_str_radix(encoded, 16).wrap_err("parse latest block timestamp")
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
        }
        let logs = client
            .eth_get_logs(abi::TEE_REGISTRY_ADDR, &topics, &from, "latest")
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

    fn sealed_artifact(offer_public: [u8; 32]) -> Vec<u8> {
        let mut sealed = vec![0x44; MIN_SEALED_OFFER_KEY_FOR_REGISTRY_BYTES];
        sealed[..32].copy_from_slice(&offer_public);
        sealed
    }

    #[test]
    fn binding_id_is_exact_and_nonzero() {
        assert!(parse_nonzero_b256(&"11".repeat(32), "binding").is_ok());
        assert!(parse_nonzero_b256(&"00".repeat(32), "binding").is_err());
        assert!(parse_nonzero_b256(&"11".repeat(31), "binding").is_err());
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
