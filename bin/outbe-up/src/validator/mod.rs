use std::{fs, path::Path};

use alloy_primitives::{keccak256, Address};
use eyre::{Context, Result};
use k256::ecdsa::SigningKey;
use zeroize::Zeroizing;

pub mod assets;
pub mod bundle;
pub mod config;
pub mod install;
pub mod journal;
pub mod prepare;
pub mod promote;
pub mod rpc;
pub mod service;
pub mod snapshot;
pub mod status;
pub mod sync;
pub mod wait;

pub const LOCAL_RPC: &str = "http://127.0.0.1:8545";
pub const FULL_NODE: &str = "outbe-full-node.service";
pub const VALIDATOR_TARGET: &str = "outbe-validator.target";
pub const VALIDATOR_SERVICE: &str = "outbe-validator.service";
pub const ENCLAVE_SERVICE: &str = "outbe-enclave.service";
pub const RENEW_TIMER: &str = "outbe-tee-renew.timer";

pub(crate) fn validator_address(path: &Path) -> Result<Address> {
    let encoded = Zeroizing::new(
        fs::read_to_string(path)
            .wrap_err_with(|| format!("failed to read validator key: {}", path.display()))?,
    );
    let mut key = Zeroizing::new([0_u8; 32]);
    hex::decode_to_slice(
        encoded.trim().strip_prefix("0x").unwrap_or(encoded.trim()),
        key.as_mut(),
    )
    .wrap_err("validator EVM key is not 32-byte hex")?;
    let signing = SigningKey::from_slice(key.as_ref()).wrap_err("invalid validator EVM key")?;
    let public = signing.verifying_key().to_encoded_point(false);
    let raw = public
        .as_bytes()
        .get(1..)
        .filter(|bytes| bytes.len() == 64)
        .ok_or_else(|| eyre::eyre!("invalid validator public key"))?;
    Ok(Address::from_slice(&keccak256(raw)[12..]))
}

pub(crate) fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| eyre::eyre!("path is not valid UTF-8: {}", path.display()))
}
