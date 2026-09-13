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

use std::path::PathBuf;

use clap::Subcommand;
use eyre::Result;

use crate::rpc::Rpc;

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
        /// Exact final seeded genesis.json used to derive the release-measured
        /// network, policy schedule and epoch-0 finality anchor.
        #[arg(long)]
        genesis: PathBuf,
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
                genesis,
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
                        genesis: &genesis,
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

#[cfg(test)]
mod tests;

mod identity;
use identity::{
    authorize_validator_node_binding, compressed_public_key, development_identity_v1,
    ensure_signer_matches_node_id, load_secp256k1_key_file, parse_nonzero_b256, pubkey,
    sign_node_hash,
};

mod rpc;
use rpc::{
    call_u256, json_b256_field, json_hex_array, json_hex_bytes, json_hex_u256_field,
    json_hex_u64_field, CliFinalityRpc,
};

mod renewal;
use renewal::{renew, renewal_status};

mod upgrade;
use upgrade::{upgrade_copy_root, upgrade_prepare, upgrade_status, upgrade_submit};

mod join;

use super::require_signer;

use join::{join, TeeJoinArgs};
