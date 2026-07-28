//! Startup loading of the consensus-bound OCOMP fork install.

use std::sync::Arc;

use alloy_primitives::{Bytes, B256};
use outbe_metadosis::ocomp::fork::OcompForkInstallV1;
use outbe_ocomp_protocol::profile::poc_schema_limits;
use outbe_primitives::OutbeHeader;
use reth_chainspec::ChainSpec;

/// `genesis.config` key in the selected chain manifest.
pub const OCOMP_FORK_INSTALL_GENESIS_KEY: &str = "ocompForkInstallV1";

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenesisOcompForkInstallV1 {
    canonical_bytes: Bytes,
    install_hash: B256,
}

/// Loads and fully validates the immutable fork binding from the selected
/// chain spec. Absence keeps OCOMP consensus phases disabled.
pub fn load_ocomp_fork_install(
    chain_spec: &ChainSpec<OutbeHeader>,
) -> eyre::Result<Option<Arc<OcompForkInstallV1>>> {
    let extra = &chain_spec.genesis.config.extra_fields;
    let Some(parsed) =
        extra.get_deserialized::<GenesisOcompForkInstallV1>(OCOMP_FORK_INSTALL_GENESIS_KEY)
    else {
        return Ok(None);
    };
    let manifest = parsed
        .map_err(|error| eyre::eyre!("invalid genesis config OCOMP fork install: {error}"))?;
    let limits = poc_schema_limits();
    let install = OcompForkInstallV1::decode_canonical(&manifest.canonical_bytes, &limits)
        .map_err(|error| eyre::eyre!("invalid canonical OCOMP fork install: {error}"))?;
    install
        .validate_for_chain(chain_spec.chain().id(), chain_spec.genesis_hash(), &limits)
        .map_err(|error| eyre::eyre!("OCOMP fork install does not bind selected chain: {error}"))?;
    let actual_hash = install
        .install_hash(&limits)
        .map_err(|error| eyre::eyre!("hash OCOMP fork install: {error}"))?;
    if actual_hash != manifest.install_hash {
        eyre::bail!(
            "OCOMP fork install hash mismatch: manifest {}, canonical {}",
            manifest.install_hash,
            actual_hash
        );
    }
    Ok(Some(Arc::new(install)))
}
