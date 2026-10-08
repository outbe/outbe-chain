//! Reuse the co-hosted node's committed private session for calculation reads.

use alloy_primitives::U256;
use outbe_tee::TransportError;
use std::path::Path;

/// This path reads the existing owner-only NodeHost identity. It cannot create
/// another identity, install a network key, or expose a plaintext RPC method.
pub fn install_private_tribute_reader(
    endpoint: &str,
    node_data_dir: &Path,
    chain_id: u64,
) -> Result<(), TransportError> {
    let (manifest, node_host) =
        outbe_tee::node_host::committed_node_host_session_material(node_data_dir)?;
    if manifest.chain_id != U256::from(chain_id).to_be_bytes::<32>() {
        return Err(TransportError::Codec(
            "private Tribute reader chain differs from committed NodeHost".into(),
        ));
    }
    let client = outbe_tee::connect_committed_node_host_enclave(endpoint, node_data_dir)?;
    outbe_tee::install_authorized_enclave_client(
        client,
        endpoint.to_owned(),
        node_data_dir.to_owned(),
        manifest,
        node_host,
    )
    .map_err(|error| TransportError::EnclaveError(error.to_string()))
}
