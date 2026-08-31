//! Validator / operator handles: key material and identity for committee nodes.
//!
//! Replaces the `e2e_vkey`/`e2e_v0key` key readers and the address helpers in
//! the legacy localnet contract (and on-chain status, wired as the
//! lifecycle flows land). A validator is addressed by index (`validator-<i>`);
//! an `Operator` is just an active validator acting as proposer/voter.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use alloy_primitives::{Address, Bytes, B256};
use eyre::{eyre, Result, WrapErr};

use crate::internal::config::Config;

/// A committee validator, identified by its 0-based index and data dir.
#[derive(Debug, Clone)]
pub struct Validator {
    pub index: usize,
    dir: PathBuf,
}

impl Validator {
    /// The EOA private key (`validator-<i>/evm-key.hex`), `0x`-prefixed - matches
    /// the shell `"0x$(tr -d '[:space:]' < evm-key.hex)"`.
    pub fn evm_key(&self) -> Result<String> {
        let path = self.evm_key_path();
        let raw =
            std::fs::read_to_string(&path).wrap_err_with(|| format!("reading evm key {path:?}"))?;
        let hex: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
        Ok(if hex.starts_with("0x") {
            hex
        } else {
            format!("0x{hex}")
        })
    }

    /// Path of the validator's EOA key.
    pub fn evm_key_path(&self) -> PathBuf {
        self.dir.join("evm-key.hex")
    }

    /// Path of the validator's individual MinPk BLS key.
    pub fn signing_key_path(&self) -> PathBuf {
        self.dir.join("signing-key.hex")
    }

    /// Install a prepared identity into an otherwise-unprovisioned validator
    /// directory. Existing secret files are never overwritten.
    pub fn install_registration_identity(&self, identity: &RegistrationIdentity) -> Result<()> {
        identity.install_at(&self.dir)
    }
}

/// Reusable self-registration material.
///
/// It intentionally owns the exact EOA, BLS private key, public key and PoP
/// bytes in memory, so a test may destroy and rebuild a localnet with another
/// valid chain ID and submit the *same* proof again. The custom `Debug`
/// implementation never prints either private key.
#[derive(Clone)]
pub struct RegistrationIdentity {
    address: Address,
    evm_private_key: String,
    bls_private_key: String,
    bls_public_key: Bytes,
    radicle_node_id: B256,
    radicle_secret_key: String,
    radicle_public_key: String,
    registration_signature: Bytes,
    reth_p2p_secret: String,
}

impl fmt::Debug for RegistrationIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegistrationIdentity")
            .field("address", &self.address)
            .field("bls_public_key", &self.bls_public_key)
            .field("radicle_node_id", &self.radicle_node_id)
            .field("registration_signature", &self.registration_signature)
            .field("evm_private_key", &"<redacted>")
            .field("bls_private_key", &"<redacted>")
            .field("radicle_secret_key", &"<redacted>")
            .field("reth_p2p_secret", &"<redacted>")
            .finish()
    }
}

impl RegistrationIdentity {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        address: Address,
        evm_private_key: String,
        bls_private_key: String,
        bls_public_key: Bytes,
        radicle_node_id: B256,
        radicle_secret_key: String,
        radicle_public_key: String,
        registration_signature: Bytes,
        reth_p2p_secret: String,
    ) -> Self {
        Self {
            address,
            evm_private_key,
            bls_private_key,
            bls_public_key,
            radicle_node_id,
            radicle_secret_key,
            radicle_public_key,
            registration_signature,
            reth_p2p_secret,
        }
    }

    pub fn address(&self) -> Address {
        self.address
    }

    pub fn evm_key(&self) -> &str {
        &self.evm_private_key
    }

    pub fn bls_public_key(&self) -> &Bytes {
        &self.bls_public_key
    }

    pub fn radicle_node_id(&self) -> B256 {
        self.radicle_node_id
    }

    pub(crate) fn radicle_secret_key(&self) -> &str {
        &self.radicle_secret_key
    }

    pub(crate) fn radicle_public_key(&self) -> &str {
        &self.radicle_public_key
    }

    pub fn registration_signature(&self) -> &Bytes {
        &self.registration_signature
    }

    pub fn reth_p2p_secret(&self) -> &str {
        &self.reth_p2p_secret
    }

    pub(crate) fn install_at(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        let radicle_keys = dir.join("radicle/keys");
        fs::create_dir_all(&radicle_keys)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(dir.join("radicle"), fs::Permissions::from_mode(0o700))?;
            fs::set_permissions(&radicle_keys, fs::Permissions::from_mode(0o700))?;
        }
        let secrets = [
            (dir.join("signing-key.hex"), self.bls_private_key.as_str()),
            (
                dir.join("evm-key.hex"),
                trim_hex_prefix(&self.evm_private_key),
            ),
            (
                dir.join("reth-p2p-secret.hex"),
                self.reth_p2p_secret.as_str(),
            ),
            (
                radicle_keys.join("radicle"),
                self.radicle_secret_key.as_str(),
            ),
            (
                radicle_keys.join("radicle.pub"),
                self.radicle_public_key.as_str(),
            ),
        ];
        if let Some((path, _)) = secrets.iter().find(|(path, _)| path.exists()) {
            return Err(eyre!(
                "refusing to overwrite identity file {}",
                path.display()
            ));
        }

        let mut installed = Vec::with_capacity(secrets.len());
        for (path, value) in secrets {
            if let Err(error) = write_secret_no_clobber(&path, value) {
                for installed_path in installed {
                    let _ = fs::remove_file(installed_path);
                }
                return Err(error);
            }
            installed.push(path);
        }
        Ok(())
    }
}

/// An operator role - a thin wrapper over the validator that proposes/casts.
#[derive(Debug, Clone)]
pub struct Operator(pub Validator);

impl Operator {
    pub fn evm_key(&self) -> Result<String> {
        self.0.evm_key()
    }
}

/// Accessor for the committee's validators/operators and its size.
#[derive(Debug, Clone)]
pub struct Validators {
    cfg: Config,
    /// Committee size the environment bootstraps (`--validators`).
    size: usize,
}

impl Validators {
    pub(crate) fn new(cfg: Config, size: usize) -> Self {
        Self { cfg, size }
    }

    /// Committee size the environment bootstraps.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Validator by 0-based index.
    pub fn get(&self, i: usize) -> Validator {
        Validator {
            index: i,
            dir: self.cfg.validator_dir(i),
        }
    }

    /// Validator by `"validator-<N>"` name (as used in the feature files).
    pub fn by_name(&self, name: &str) -> Result<Validator> {
        Ok(self.get(parse_index(name)?))
    }

    /// Operator by `"validator-<N>"` name.
    pub fn operator(&self, name: &str) -> Result<Operator> {
        Ok(Operator(self.by_name(name)?))
    }

    /// HTTP RPC port of the primary committee node (validator-0). Each peer's
    /// ports live in its own block - see [`crate::internal::ports`].
    pub fn primary_port(&self) -> u16 {
        self.cfg.primary_port()
    }

    /// HTTP RPC port of validator index `i` (committee or joiner).
    pub fn http_port(&self, i: usize) -> u16 {
        self.cfg.http_port(i)
    }

    /// Chain-specific node data directory for lifecycle evidence probes.
    pub fn data_dir(&self, i: usize) -> PathBuf {
        self.cfg.validator_dir(i).join("data")
    }

    /// The joiner's index (one past the committee, e.g. `validator-4` for N=4).
    pub fn joiner_index(&self) -> usize {
        self.size
    }

    /// The joiner validator handle (its key material lives at `validator-<N>/`).
    pub fn joiner(&self) -> Validator {
        self.get(self.size)
    }

    /// HTTP RPC ports of every committee node (validator-0..N-1).
    pub fn committee_ports(&self) -> Vec<u16> {
        (0..self.size).map(|i| self.cfg.http_port(i)).collect()
    }

    /// HTTP RPC ports of the non-primary committee nodes (validators 1..N-1).
    pub fn peer_ports(&self) -> Vec<u16> {
        (1..self.size).map(|i| self.cfg.http_port(i)).collect()
    }
}

fn parse_index(name: &str) -> Result<usize> {
    name.strip_prefix("validator-")
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| eyre!("expected 'validator-<N>', got '{name}'"))
}

fn trim_hex_prefix(value: &str) -> &str {
    value.strip_prefix("0x").unwrap_or(value)
}

fn write_secret_no_clobber(path: &Path, value: &str) -> Result<()> {
    if path.exists() {
        return Err(eyre!(
            "refusing to overwrite identity file {}",
            path.display()
        ));
    }
    let file_name = path
        .file_name()
        .ok_or_else(|| eyre!("identity path has no filename: {}", path.display()))?
        .to_string_lossy();
    let temporary = path.with_file_name(format!(".{file_name}.tmp.{}", std::process::id()));

    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .wrap_err_with(|| format!("create temporary identity file {}", temporary.display()))?;
        file.write_all(value.as_bytes())?;
        file.sync_all()?;
        if path.exists() {
            return Err(eyre!(
                "refusing to overwrite identity file {}",
                path.display()
            ));
        }
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_debug_redacts_secret_material() {
        let identity = RegistrationIdentity::new(
            Address::ZERO,
            "0xevm-secret".into(),
            "bls-secret".into(),
            Bytes::from_static(&[1; 48]),
            B256::repeat_byte(3),
            "radicle-secret".into(),
            "radicle-public".into(),
            Bytes::from_static(&[2; 96]),
            "p2p-secret".into(),
        );
        let debug = format!("{identity:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("evm-secret"));
        assert!(!debug.contains("bls-secret"));
        assert!(!debug.contains("radicle-secret"));
        assert!(!debug.contains("p2p-secret"));
    }
}
