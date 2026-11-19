//! Read-only status inputs.

use alloy_primitives::Address;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
enum NodeIdParseError {
    #[error("node id is not valid hex")]
    Hex(#[source] hex::FromHexError),
    #[error("node id must be 33-byte compressed secp256k1")]
    Length,
    #[error("node id is not a canonical compressed secp256k1 key")]
    NotCanonical,
}

fn parse_compressed_node_id(raw: &str) -> Result<[u8; 33], NodeIdParseError> {
    let hex_text = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(raw);
    let bytes = hex::decode(hex_text).map_err(NodeIdParseError::Hex)?;
    let node_id: [u8; 33] = bytes.try_into().map_err(|_| NodeIdParseError::Length)?;
    if k256::PublicKey::from_sec1_bytes(&node_id).is_err() {
        return Err(NodeIdParseError::NotCanonical);
    }
    Ok(node_id)
}

#[derive(clap::Args)]
#[command(group(clap::ArgGroup::new("binding_selector")
    .args(["node_data_dir", "node_id", "validator"])
    .multiple(false)))]
pub struct StatusArgs {
    /// Derive the NodeHost selector from `<dir>/discovery-secret`.
    /// Omit, with `--node-id` and `--validator`, for network status without local files.
    #[arg(long)]
    pub(super) node_data_dir: Option<PathBuf>,
    /// Compressed secp256k1 P2P public key that selects the NodeHost binding.
    #[arg(long, value_parser = parse_compressed_node_id)]
    pub(super) node_id: Option<[u8; 33]>,
    /// Validator address that selects the enclave binding.
    #[arg(long)]
    pub(super) validator: Option<Address>,
    /// Custody format of `<dir>/discovery-secret`: plaintext, encrypted, or os-level.
    #[arg(
        long = "reth-key-backend",
        default_value = "plaintext",
        value_name = "BACKEND"
    )]
    pub(super) reth_key_backend: String,
    /// Passphrase for an encrypted `<dir>/discovery-secret`.
    /// You can also provide it through the RETH_P2P_PASSPHRASE environment variable.
    #[arg(
        long = "reth-passphrase",
        env = "RETH_P2P_PASSPHRASE",
        value_name = "SECRET"
    )]
    pub(super) reth_passphrase: Option<String>,
    /// Emit warning once an unsafe lease is this many blocks from freeze (requires a binding selector).
    #[arg(long, default_value_t = 600, requires = "binding_selector")]
    pub(super) warning_blocks: u64,
    /// Emit critical once an unsafe lease is this many blocks from freeze (requires a binding selector).
    #[arg(long, default_value_t = 120, requires = "binding_selector")]
    pub(super) critical_blocks: u64,
}
