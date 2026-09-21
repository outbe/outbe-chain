use crate::*;

pub(crate) fn load_reth_p2p_node_host_signer(
    network: &reth_node_core::args::NetworkArgs,
    default_secret_path: PathBuf,
) -> eyre::Result<(k256::ecdsa::SigningKey, [u8; 33])> {
    let reth_p2p_secret = network
        .secret_key(default_secret_path)
        .wrap_err("failed to load persistent Reth P2P identity for TEE")?;
    let signing = k256::ecdsa::SigningKey::from_slice(reth_p2p_secret.secret_bytes().as_slice())
        .map_err(|error| eyre::eyre!("invalid Reth P2P signing key: {error}"))?;
    let reth_p2p_public = signing
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .map_err(|_| eyre::eyre!("Reth P2P public key is not compressed SEC1-33"))?;
    Ok((signing, reth_p2p_public))
}
