//! Persistent host-side authorization for one production node enclave.
//!
//! The private NodeHost Noise key is write-once. A canonical public manifest is
//! first written as `pending`, committed by the enclave, then promoted to the
//! restart record. A crash after enclave commit but before promotion is closed
//! by reconnecting with the pending record and promoting it only on success.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::tee_attestation_v1::{
    EnclaveInitializationManifestV1, EnclaveProfile, NodeIdV1,
};

use crate::{AuthorizedEnclaveClient, NodeHostNoiseKey, TransportError};

pub const NODE_HOST_DIRECTORY_V1: &str = "tee-node-host-v1";
pub const NODE_HOST_NOISE_KEY_V1: &str = "noise-initiator.key";
pub const NODE_HOST_MANIFEST_V1: &str = "initialization-manifest.bin";
const NODE_HOST_PENDING_MANIFEST_V1: &str = "initialization-manifest.pending";
const MAX_INITIALIZATION_MANIFEST_BYTES: u64 = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValidatorNodeHostIdentityV1 {
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub validator: Address,
    pub consensus_bls_public: [u8; 48],
}

impl ValidatorNodeHostIdentityV1 {
    fn into_common(self) -> NodeHostIdentityV1 {
        NodeHostIdentityV1 {
            chain_id: self.chain_id,
            genesis_hash: self.genesis_hash,
            enclave_profile: EnclaveProfile::Validator,
            node_id: NodeIdV1::Validator {
                address: self.validator.into_array(),
                bls_minpk_public: self.consensus_bls_public,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FullNodeNodeHostIdentityV1 {
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub reth_p2p_public: [u8; 33],
}

impl FullNodeNodeHostIdentityV1 {
    fn into_common(self) -> NodeHostIdentityV1 {
        NodeHostIdentityV1 {
            chain_id: self.chain_id,
            genesis_hash: self.genesis_hash,
            enclave_profile: EnclaveProfile::FullNode,
            node_id: NodeIdV1::FullNode {
                reth_p2p_public: self.reth_p2p_public,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NodeHostIdentityV1 {
    chain_id: u64,
    genesis_hash: B256,
    enclave_profile: EnclaveProfile,
    node_id: NodeIdV1,
}

impl NodeHostIdentityV1 {
    fn chain_id_word(&self) -> [u8; 32] {
        U256::from(self.chain_id).to_be_bytes()
    }
}

/// Connect to the one initialized validator enclave, or perform its one-time
/// initialization when no committed host manifest exists.
///
/// `node_data_dir` is the resolved chain-specific reth data directory. The
/// function owns only its fixed `tee-node-host-v1` child. A committed manifest
/// is never replaced: losing or replacing the enclave identity is an explicit
/// operator decision, not an implicit startup recovery path.
pub fn connect_or_initialize_validator_enclave<F>(
    endpoint: &str,
    node_data_dir: &Path,
    identity: ValidatorNodeHostIdentityV1,
    sign_authorization: F,
) -> Result<AuthorizedEnclaveClient, TransportError>
where
    F: Fn(B256) -> Result<[u8; 65], String>,
{
    connect_or_initialize_enclave(
        endpoint,
        node_data_dir,
        identity.into_common(),
        sign_authorization,
    )
}

/// Connect to the one initialized full-node enclave, or perform its one-time
/// initialization when no committed host manifest exists.
///
/// The authorization signer must be the exact persistent Reth P2P key whose
/// compressed public key is carried by `identity`.
pub fn connect_or_initialize_full_node_enclave<F>(
    endpoint: &str,
    node_data_dir: &Path,
    identity: FullNodeNodeHostIdentityV1,
    sign_authorization: F,
) -> Result<AuthorizedEnclaveClient, TransportError>
where
    F: Fn(B256) -> Result<[u8; 65], String>,
{
    connect_or_initialize_enclave(
        endpoint,
        node_data_dir,
        identity.into_common(),
        sign_authorization,
    )
}

fn connect_or_initialize_enclave<F>(
    endpoint: &str,
    node_data_dir: &Path,
    identity: NodeHostIdentityV1,
    sign_authorization: F,
) -> Result<AuthorizedEnclaveClient, TransportError>
where
    F: Fn(B256) -> Result<[u8; 65], String>,
{
    validate_identity(&identity)?;
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;

    let committed_exists = path_exists(&paths.manifest)?;
    let pending_exists = path_exists(&paths.pending_manifest)?;
    let key_exists = path_exists(&paths.noise_key)?;
    if (committed_exists || pending_exists) && !key_exists {
        return Err(TransportError::Codec(
            "NodeHost manifest exists but its persistent Noise key is missing; refusing implicit recovery"
                .into(),
        ));
    }
    if committed_exists && pending_exists {
        return Err(TransportError::Codec(
            "both committed and pending NodeHost manifests exist; startup state is ambiguous"
                .into(),
        ));
    }

    let node_host = if key_exists {
        NodeHostNoiseKey::load(&paths.noise_key)?
    } else {
        NodeHostNoiseKey::create_new(&paths.noise_key)?
    };

    if committed_exists {
        let manifest = read_manifest(&paths.manifest)?;
        validate_manifest_identity(&manifest, &identity, &node_host)?;
        return AuthorizedEnclaveClient::connect_endpoint(endpoint, &manifest, &node_host);
    }

    if pending_exists {
        let manifest = read_manifest(&paths.pending_manifest)?;
        validate_manifest_identity(&manifest, &identity, &node_host)?;
        if let Ok(client) =
            AuthorizedEnclaveClient::connect_endpoint(endpoint, &manifest, &node_host)
        {
            promote_pending_manifest(&paths)?;
            return Ok(client);
        }
        let signature = sign_manifest(&manifest, &sign_authorization)?;
        let client = AuthorizedEnclaveClient::initialize_endpoint(
            endpoint, &manifest, &signature, &node_host,
        )?;
        promote_pending_manifest(&paths)?;
        return Ok(client);
    }

    let challenge = AuthorizedEnclaveClient::discover_endpoint(endpoint)?;
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: identity.chain_id_word(),
        genesis_hash: identity.genesis_hash,
        enclave_profile: identity.enclave_profile,
        node_id: identity.node_id.clone(),
        initialization_challenge: challenge.challenge,
        node_host_noise_x25519: node_host.public(),
        recipient_x25519: challenge.recipient_x25519,
        attestation_ed25519: challenge.attestation_ed25519,
        noise_responder_x25519: challenge.noise_responder_x25519,
    };
    validate_manifest_identity(&manifest, &identity, &node_host)?;
    write_manifest_once(&paths.pending_manifest, &manifest, &paths.root)?;
    let signature = sign_manifest(&manifest, &sign_authorization)?;
    let client =
        AuthorizedEnclaveClient::initialize_endpoint(endpoint, &manifest, &signature, &node_host)?;
    promote_pending_manifest(&paths)?;
    Ok(client)
}

fn validate_identity(identity: &NodeHostIdentityV1) -> Result<(), TransportError> {
    if identity.chain_id == 0 || identity.genesis_hash.is_zero() {
        return Err(TransportError::Codec(
            "NodeHost identity contains a zero chain identity".into(),
        ));
    }
    identity
        .node_id
        .node_id_hash()
        .map_err(|error| TransportError::Codec(error.to_string()))?;
    Ok(())
}

fn validate_manifest_identity(
    manifest: &EnclaveInitializationManifestV1,
    identity: &NodeHostIdentityV1,
    node_host: &NodeHostNoiseKey,
) -> Result<(), TransportError> {
    manifest
        .encode_canonical()
        .map_err(|error| TransportError::Codec(error.to_string()))?;
    if manifest.chain_id != identity.chain_id_word()
        || manifest.genesis_hash != identity.genesis_hash
        || manifest.enclave_profile != identity.enclave_profile
        || manifest.node_id != identity.node_id
        || manifest.node_host_noise_x25519 != node_host.public()
    {
        return Err(TransportError::Codec(
            "persisted NodeHost manifest does not match this node startup identity".into(),
        ));
    }
    Ok(())
}

fn sign_manifest<F>(
    manifest: &EnclaveInitializationManifestV1,
    sign_authorization: &F,
) -> Result<[u8; 65], TransportError>
where
    F: Fn(B256) -> Result<[u8; 65], String>,
{
    let hash = manifest
        .authorization_hash()
        .map_err(|error| TransportError::Codec(error.to_string()))?;
    let signature = sign_authorization(hash).map_err(TransportError::Codec)?;
    if !manifest.verify_node_signature(&signature) {
        return Err(TransportError::Codec(
            "node signer produced an invalid NodeHost manifest signature".into(),
        ));
    }
    Ok(signature)
}

fn read_manifest(path: &Path) -> Result<EnclaveInitializationManifestV1, TransportError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.len() > MAX_INITIALIZATION_MANIFEST_BYTES
    {
        return Err(TransportError::Codec(
            "NodeHost manifest must be an owner-only bounded regular file".into(),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)?;
    EnclaveInitializationManifestV1::decode_canonical(&bytes)
        .map_err(|error| TransportError::Codec(error.to_string()))
}

fn write_manifest_once(
    path: &Path,
    manifest: &EnclaveInitializationManifestV1,
    directory: &Path,
) -> Result<(), TransportError> {
    let bytes = manifest
        .encode_canonical()
        .map_err(|error| TransportError::Codec(error.to_string()))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn promote_pending_manifest(paths: &NodeHostPaths) -> Result<(), TransportError> {
    if path_exists(&paths.manifest)? {
        return Err(TransportError::Codec(
            "refusing to replace a committed NodeHost manifest".into(),
        ));
    }
    fs::rename(&paths.pending_manifest, &paths.manifest)?;
    File::open(&paths.root)?.sync_all()?;
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<(), TransportError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir()
                || metadata.permissions().mode() & 0o777 != 0o700
                || metadata.uid() != rustix::process::geteuid().as_raw()
            {
                return Err(TransportError::Codec(
                    "NodeHost state path must be a current-user directory with mode 0700".into(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            DirBuilder::new().mode(0o700).create(path)?;
            if let Some(parent) = path.parent() {
                File::open(parent)?.sync_all()?;
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn path_exists(path: &Path) -> Result<bool, TransportError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

struct NodeHostPaths {
    root: PathBuf,
    noise_key: PathBuf,
    manifest: PathBuf,
    pending_manifest: PathBuf,
}

impl NodeHostPaths {
    fn new(node_data_dir: &Path) -> Self {
        let root = node_data_dir.join(NODE_HOST_DIRECTORY_V1);
        Self {
            noise_key: root.join(NODE_HOST_NOISE_KEY_V1),
            manifest: root.join(NODE_HOST_MANIFEST_V1),
            pending_manifest: root.join(NODE_HOST_PENDING_MANIFEST_V1),
            root,
        }
    }
}
