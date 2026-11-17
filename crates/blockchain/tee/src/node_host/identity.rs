#[cfg(test)]
use super::{locked_state::lock_node_host_state, path_exists};
use super::{
    read_owned_bounded_file, reconcile_replacement_state, NodeHostIdentityKey, NodeHostPaths,
};
use crate::TransportError;
use outbe_primitives::tee_attestation_v1::{
    EnclaveInitializationManifestV1, NetworkBindingV1, NodeIdV1,
};
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

/// Load the one committed production manifest after applying the same bounded,
/// owner-only NodeHost state checks that startup uses. Missing, pending or
/// inconsistent state is an error. This function never creates or recovers an
/// enclave identity.
#[cfg(test)]
pub(super) fn load_committed_enclave_manifest_v1(
    node_data_dir: &Path,
) -> Result<EnclaveInitializationManifestV1, TransportError> {
    let (paths, _state_lock) = lock_node_host_state(node_data_dir)?;
    if !path_exists(&paths.manifest)?
        || !path_exists(&paths.identity_key)?
        || path_exists(&paths.pending_manifest)?
    {
        return Err(TransportError::Codec(
            "one committed production NodeHost manifest is required".into(),
        ));
    }
    let (manifest, _) = read_committed_identity(&paths)?;
    Ok(manifest)
}

pub(super) fn read_committed_identity(
    paths: &NodeHostPaths,
) -> Result<(EnclaveInitializationManifestV1, NodeHostIdentityKey), TransportError> {
    let node_host = NodeHostIdentityKey::load(&paths.identity_key)?;
    reconcile_replacement_state(paths, &node_host)?;
    let manifest = read_manifest(&paths.manifest)?;
    if manifest.node_host_noise_x25519 != node_host.public() {
        return Err(TransportError::Codec(
            "committed manifest does not match the persistent NodeHost key".into(),
        ));
    }
    Ok((manifest, node_host))
}

pub(super) fn read_manifest(
    path: &Path,
) -> Result<EnclaveInitializationManifestV1, TransportError> {
    let bytes = read_owned_bounded_file(path, MAX_INITIALIZATION_MANIFEST_BYTES, "manifest")?;
    EnclaveInitializationManifestV1::decode_canonical(&bytes).map_err(TransportError::codec)
}

impl NodeHostIdentityV1 {
    /// Authorization uses only the persistent host keys, not the process enclave identity.
    pub fn authorization_witness_v1(
        &self,
        node_host: &NodeHostIdentityKey,
    ) -> crate::in_process::Result<
        outbe_primitives::tee_attestation_v1::NodeHostAuthorizationWitnessV1,
    > {
        use outbe_primitives::tee_attestation_v1::NodeHostAuthorizationWitnessV1;
        let witness = NodeHostAuthorizationWitnessV1 {
            chain_id: self.network_binding.chain_id,
            genesis_hash: self.network_binding.genesis_hash,
            attestation_mode: self.network_binding.attestation_mode,
            node_id: self.node_id(),
            node_host_noise_x25519: node_host.public(),
        };
        witness.encode_canonical()?;
        Ok(witness)
    }
}

#[cfg(test)]
pub(super) mod process_identity_tests {
    use super::*;
    use alloy_primitives::B256;
    use outbe_primitives::tee_attestation_v1::*;

    pub(crate) fn manifest(seed: u8, host: [u8; 32]) -> EnclaveInitializationManifestV1 {
        let node = k256::ecdsa::SigningKey::from_slice(&[3; 32]).unwrap();
        EnclaveInitializationManifestV1 {
            chain_id: alloy_primitives::U256::from(424242).to_be_bytes(),
            genesis_hash: B256::repeat_byte(2),
            attestation_mode: AttestationMode::NoAttestationDev,
            node_id: NodeIdV1 {
                reth_p2p_public: node
                    .verifying_key()
                    .to_encoded_point(true)
                    .as_bytes()
                    .try_into()
                    .unwrap(),
            },
            initialization_challenge: [seed; 32],
            node_host_noise_x25519: host,
            recipient_x25519: [seed; 32],
            noise_responder_x25519: [seed; 32],
            attestation_ed25519: ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
                .verifying_key()
                .to_bytes(),
        }
    }

    #[test]
    fn host_authorization_is_stable_across_ephemeral_enclave_restarts() {
        let root = tempfile::tempdir().unwrap();
        let host = NodeHostIdentityKey::create_new(&root.path().join("host.key")).unwrap();
        let active = manifest(9, host.public());
        let identity = NodeHostIdentityV1 {
            network_binding: active.network_binding(),
            reth_p2p_public: active.node_id.reth_p2p_public,
        };
        let expected = identity.authorization_witness_v1(&host).unwrap();
        for process in [active, manifest(10, host.public())] {
            assert_eq!(
                NodeHostAuthorizationWitnessV1::from_manifest(&process).unwrap(),
                expected
            );
        }
        let loaded = NodeHostIdentityKey::load(&root.path().join("host.key")).unwrap();
        assert_eq!(
            identity.authorization_witness_v1(&loaded).unwrap(),
            expected
        );
    }
}
