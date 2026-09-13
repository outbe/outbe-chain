use super::ensure_private_directory;
use super::path_exists;
use super::read_owned_bounded_file;
use super::reconcile_replacement_state;
use super::write_manifest_once;
use super::NodeHostPaths;
use super::NodeHostStateLock;

use crate::AuthorizedEnclaveClient;

use crate::NodeHostNoiseKey;
use crate::TransportError;

use alloy_primitives::B256;

use outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1;
use outbe_primitives::tee_attestation_v1::NetworkBindingV1;
use outbe_primitives::tee_attestation_v1::NodeIdV1;

use std::fs;

use std::fs::File;

use std::path::Path;

pub(super) const MAX_INITIALIZATION_MANIFEST_BYTES: u64 = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeHostIdentityV1 {
    pub network_binding: NetworkBindingV1,
    pub reth_p2p_public: [u8; 33],
}

impl NodeHostIdentityV1 {
    pub(super) fn node_id(&self) -> NodeIdV1 {
        NodeIdV1 {
            reth_p2p_public: self.reth_p2p_public,
        }
    }
}

/// Connect to the one initialized NodeHost enclave, or perform its one-time
/// initialization when no committed host manifest exists.
///
/// `node_data_dir` is the resolved chain-specific reth data directory. The
/// function owns only its fixed `tee-node-host-v1` child. A committed manifest
/// is never replaced: losing or replacing the enclave identity is an explicit
/// operator decision, not an implicit startup recovery path.
pub fn connect_or_initialize_node_host_enclave<F>(
    endpoint: &str,
    node_data_dir: &Path,
    identity: NodeHostIdentityV1,
    sign_authorization: F,
) -> Result<AuthorizedEnclaveClient, TransportError>
where
    F: Fn(B256) -> Result<[u8; 65], String>,
{
    connect_or_initialize_enclave(endpoint, node_data_dir, identity, sign_authorization)
}

/// Reconnect to the one already committed NodeHost identity. This path never
/// creates state and is used by later startup stages after the node entrypoint
/// has resolved and committed the persistent Reth P2P identity.
pub fn connect_committed_node_host_enclave(
    endpoint: &str,
    node_data_dir: &Path,
) -> Result<AuthorizedEnclaveClient, TransportError> {
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    if !path_exists(&paths.manifest)? || !path_exists(&paths.noise_key)? {
        return Err(TransportError::Codec(
            "one committed production NodeHost manifest is required".into(),
        ));
    }
    let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
    reconcile_replacement_state(&paths, &node_host)?;
    let manifest = read_manifest(&paths.manifest)?;
    AuthorizedEnclaveClient::connect_endpoint(endpoint, &manifest, &node_host)
}

/// Load the one committed production manifest after applying the same bounded,
/// owner-only NodeHost state checks used by startup. Missing, pending or
/// inconsistent state is an error; this function never creates or recovers an
/// enclave identity.
pub fn load_committed_enclave_manifest_v1(
    node_data_dir: &Path,
) -> Result<EnclaveInitializationManifestV1, TransportError> {
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    if !path_exists(&paths.manifest)?
        || !path_exists(&paths.noise_key)?
        || path_exists(&paths.pending_manifest)?
    {
        return Err(TransportError::Codec(
            "one committed production NodeHost manifest is required".into(),
        ));
    }
    let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
    reconcile_replacement_state(&paths, &node_host)?;
    let manifest = read_manifest(&paths.manifest)?;
    if manifest.node_host_noise_x25519 != node_host.public() {
        return Err(TransportError::Codec(
            "committed manifest does not match the persistent NodeHost key".into(),
        ));
    }
    Ok(manifest)
}

/// Load the committed manifest AND the persistent NodeHost Noise key for the
/// process-global enclave session. Same bounded, owner-only state checks as
/// [`load_committed_enclave_manifest_v1`]; called once at install time so the
/// session can later reconnect without re-acquiring the NodeHost file lock in
/// the hot path (the committed manifest is write-once, and a legitimately
/// replaced enclave fails the Noise-IK handshake against the cached responder
/// static - fail-closed, requiring the operator restart that replacement
/// already demands).
pub fn committed_node_host_session_material(
    node_data_dir: &Path,
) -> Result<(EnclaveInitializationManifestV1, NodeHostNoiseKey), TransportError> {
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    if !path_exists(&paths.manifest)?
        || !path_exists(&paths.noise_key)?
        || path_exists(&paths.pending_manifest)?
    {
        return Err(TransportError::Codec(
            "one committed production NodeHost manifest is required".into(),
        ));
    }
    let node_host = NodeHostNoiseKey::load(&paths.noise_key)?;
    reconcile_replacement_state(&paths, &node_host)?;
    let manifest = read_manifest(&paths.manifest)?;
    if manifest.node_host_noise_x25519 != node_host.public() {
        return Err(TransportError::Codec(
            "committed manifest does not match the persistent NodeHost key".into(),
        ));
    }
    Ok((manifest, node_host))
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
    let _state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;

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
        reconcile_replacement_state(&paths, &node_host)?;
    }

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
        chain_id: identity.network_binding.chain_id,
        genesis_hash: identity.network_binding.genesis_hash,
        attestation_mode: identity.network_binding.attestation_mode,
        node_id: identity.node_id(),
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

pub(super) fn validate_identity(identity: &NodeHostIdentityV1) -> Result<(), TransportError> {
    identity
        .network_binding
        .encode_canonical()
        .map_err(|error| TransportError::Codec(error.to_string()))?;
    identity
        .node_id()
        .node_id_hash()
        .map_err(|error| TransportError::Codec(error.to_string()))?;
    Ok(())
}

pub(super) fn validate_manifest_identity(
    manifest: &EnclaveInitializationManifestV1,
    identity: &NodeHostIdentityV1,
    node_host: &NodeHostNoiseKey,
) -> Result<(), TransportError> {
    manifest
        .encode_canonical()
        .map_err(|error| TransportError::Codec(error.to_string()))?;
    if manifest.network_binding() != identity.network_binding
        || manifest.node_id != identity.node_id()
        || manifest.node_host_noise_x25519 != node_host.public()
    {
        return Err(TransportError::Codec(
            "persisted NodeHost manifest does not match this node startup identity".into(),
        ));
    }
    Ok(())
}

pub(super) fn sign_manifest<F>(
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

pub(super) fn read_manifest(
    path: &Path,
) -> Result<EnclaveInitializationManifestV1, TransportError> {
    let bytes = read_owned_bounded_file(path, MAX_INITIALIZATION_MANIFEST_BYTES, "manifest")?;
    EnclaveInitializationManifestV1::decode_canonical(&bytes)
        .map_err(|error| TransportError::Codec(error.to_string()))
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
