//! Validator public-key discovery and identity derivation.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use k256::ecdsa::SigningKey;
use rand_core::{OsRng, RngCore as _};
use serde_json::{json, Value};

use crate::error::GenesisError;
use crate::keccak::keccak256;
use crate::network::DEFAULT_CONSENSUS_P2P_PORT;
use crate::seed_codec::parse_u64;

/// Derive the uncompressed 64-byte secp256k1 public key coordinates `(x || y)` from a 32-byte hex scalar.
pub fn secp256k1_uncompressed_pubkey(hex_scalar: &str, path: &Path) -> Result<[u8; 64], GenesisError> {
    let raw = hex_scalar
        .trim()
        .strip_prefix("0x")
        .or_else(|| hex_scalar.trim().strip_prefix("0X"))
        .unwrap_or_else(|| hex_scalar.trim());
    if raw.len() != 64 {
        return Err(GenesisError::validation(format!(
            "{} must hold a 32-byte secp256k1 key in hex",
            path.display()
        )));
    }
    let key_bytes = hex::decode(raw).map_err(|_| {
        GenesisError::validation(format!(
            "{} must hold a 32-byte secp256k1 key in hex",
            path.display()
        ))
    })?;
    let signing_key = SigningKey::from_slice(&key_bytes)
        .map_err(|_| GenesisError::validation("secp256k1 scalar out of range"))?;
    let point = signing_key.verifying_key().to_encoded_point(false);
    let bytes = point.as_bytes();
    if bytes.len() != 65 || bytes[0] != 0x04 {
        return Err(GenesisError::validation("invalid secp256k1 public key point"));
    }
    let mut out = [0u8; 64];
    out.copy_from_slice(&bytes[1..]);
    Ok(out)
}

/// Derive a `0x`-prefixed lowercase EVM address from a secp256k1 private key file.
pub fn evm_address_from_key_file(path: &Path) -> Result<String, GenesisError> {
    let text = fs::read_to_string(path).map_err(|err| GenesisError::io(path, err))?;
    let pubkey = secp256k1_uncompressed_pubkey(&text, path)?;
    let hash = keccak256(&pubkey);
    Ok(format!("0x{}", hex::encode(&hash[12..])))
}

/// Return the enode node id (128 hex chars), minting `reth-p2p-secret.hex` when absent.
pub fn ensure_reth_p2p_secret(directory: &Path) -> Result<String, GenesisError> {
    let path = directory.join("reth-p2p-secret.hex");
    let raw = if path.is_file() {
        let content = fs::read_to_string(&path).map_err(|err| GenesisError::io(&path, err))?;
        let trimmed = content.trim();
        let clean = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
            .unwrap_or(trimmed);
        if clean.len() != 64 {
            return Err(GenesisError::validation(format!(
                "{} must hold 32 bytes of hex",
                path.display()
            )));
        }
        clean.to_owned()
    } else {
        mint_secp256k1_hex(&path)?
    };
    let pubkey = secp256k1_uncompressed_pubkey(&raw, &path)?;
    Ok(hex::encode(pubkey))
}

fn mint_secp256k1_hex(path: &Path) -> Result<String, GenesisError> {
    loop {
        let mut candidate = [0u8; 32];
        OsRng.fill_bytes(&mut candidate);
        if SigningKey::from_slice(&candidate).is_ok() {
            let raw = hex::encode(candidate);
            fs::write(path, &raw).map_err(|err| GenesisError::io(path, err))?;
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(path, perms).map_err(|err| GenesisError::io(path, err))?;
            return Ok(raw);
        }
    }
}

/// Parse an OpenSSH `ssh-ed25519` public key file and return its 32-byte NodeId as 64 hex chars.
pub fn radicle_node_id_from_public_key(path: &Path) -> Result<String, GenesisError> {
    let content = fs::read_to_string(path)
        .map_err(|_| GenesisError::validation(format!("invalid Radicle public key {}", path.display())))?;
    let mut parts = content.split_whitespace();
    let Some(algorithm) = parts.next() else {
        return Err(GenesisError::validation(format!(
            "invalid Radicle public key {}",
            path.display()
        )));
    };
    let Some(encoded) = parts.next() else {
        return Err(GenesisError::validation(format!(
            "invalid Radicle public key {}",
            path.display()
        )));
    };
    if algorithm != "ssh-ed25519" {
        return Err(GenesisError::validation(format!(
            "Radicle public key {} must use ssh-ed25519",
            path.display()
        )));
    }
    let payload = BASE64.decode(encoded).map_err(|_| {
        GenesisError::validation(format!(
            "invalid Radicle public key encoding in {}",
            path.display()
        ))
    })?;
    let (kind, offset) = read_ssh_field(&payload, 0, path)?;
    let (node_id, end) = read_ssh_field(&payload, offset, path)?;
    if kind != b"ssh-ed25519" || node_id.len() != 32 || end != payload.len() {
        return Err(GenesisError::validation(format!(
            "invalid Ed25519 Radicle public key {}",
            path.display()
        )));
    }
    Ok(hex::encode(node_id))
}

fn read_ssh_field<'a>(
    payload: &'a [u8],
    offset: usize,
    path: &Path,
) -> Result<(&'a [u8], usize), GenesisError> {
    if offset + 4 > payload.len() {
        return Err(GenesisError::validation(format!(
            "truncated Radicle public key {}",
            path.display()
        )));
    }
    let len = u32::from_be_bytes([
        payload[offset],
        payload[offset + 1],
        payload[offset + 2],
        payload[offset + 3],
    ]) as usize;
    let start = offset + 4;
    let end = start + len;
    if end > payload.len() {
        return Err(GenesisError::validation(format!(
            "truncated Radicle public key {}",
            path.display()
        )));
    }
    Ok((&payload[start..end], end))
}

/// Invoke `outbe-keygen show-pubkey --key <signing_key>` to obtain the 48-byte BLS MinPk public key.
pub fn bls_public_key(keygen_binary: &str, signing_key: &Path) -> Result<String, GenesisError> {
    let output = Command::new(keygen_binary)
        .args(["show-pubkey", "--key"])
        .arg(signing_key)
        .output()
        .map_err(|err| GenesisError::Subprocess {
            program: keygen_binary.to_owned(),
            details: err.to_string(),
        })?;
    if !output.status.success() {
        return Err(GenesisError::Subprocess {
            program: keygen_binary.to_owned(),
            details: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("public key:") {
            let value = rest.trim().strip_prefix("0x").unwrap_or_else(|| rest.trim());
            if value.len() != 96 {
                return Err(GenesisError::validation(format!(
                    "{}: BLS public key is not 48 bytes",
                    signing_key.display()
                )));
            }
            return Ok(value.to_owned());
        }
    }
    Err(GenesisError::validation(format!(
        "outbe-keygen printed no public key for {}",
        signing_key.display()
    )))
}

/// Build the founder validator manifest from `config["validators"]` and `keys_dir`.
pub fn discover_validators(
    config: &Value,
    keys_dir: &Path,
    keygen_binary: &str,
) -> Result<Vec<Value>, GenesisError> {
    let default_port = config
        .get("consensus_p2p_port")
        .map(parse_u64)
        .transpose()?
        .unwrap_or(DEFAULT_CONSENSUS_P2P_PORT as u64);
    let entries = config
        .get("validators")
        .and_then(Value::as_array)
        .ok_or_else(|| GenesisError::validation("`validators` must be a list"))?;
    let mut validators = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let entry_str = entry
            .as_str()
            .ok_or_else(|| GenesisError::validation(format!("validators[{index}] must be a string")))?
            .trim();
        let (host, port) = if let Some((h, p)) = entry_str.split_once(':') {
            let parsed_port: u64 = p.parse().map_err(|_| {
                GenesisError::validation(format!("invalid port in validators[{index}]: {entry_str}"))
            })?;
            (h, parsed_port)
        } else {
            (entry_str, default_port)
        };
        let directory = keys_dir.join(format!("validator-{index}"));
        if !directory.is_dir() {
            return Err(GenesisError::validation(format!(
                "missing key directory for validator {index}: {}",
                directory.display()
            )));
        }
        let signing_key = directory.join("signing-key.hex");
        let evm_key = directory.join("evm-key.hex");
        let radicle_pub = directory.join("radicle").join("keys").join("radicle.pub");
        for required in [&signing_key, &evm_key, &radicle_pub] {
            if !required.is_file() {
                return Err(GenesisError::validation(format!(
                    "validator {index}: {} not found; generate the bundle with `outbe-keygen validator --output-dir {}`",
                    required.display(),
                    directory.display()
                )));
            }
        }
        validators.push(json!({
            "address": evm_address_from_key_file(&evm_key)?,
            "public_key": bls_public_key(keygen_binary, &signing_key)?,
            "radicle_node_id": format!("0x{}", radicle_node_id_from_public_key(&radicle_pub)?),
            "p2p_address": format!("{host}:{port}"),
        }));
    }
    Ok(validators)
}
