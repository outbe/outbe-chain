//! Startup loading of the consensus-bound OCOMP fork install.

use std::sync::Arc;

use alloy_primitives::{Bytes, B256};
use outbe_metadosis::config::{OcompForkInstallClassification, OcompForkInstallV1};
use outbe_metadosis::proof_layout::METADOSIS_STORAGE_LAYOUT_V1_HASH;
use outbe_ocomp_protocol::profile::poc_schema_limits;
use outbe_primitives::OutbeHeader;
use reth_chainspec::ChainSpec;

/// `genesis.config` key in the selected chain manifest.
pub const OCOMP_FORK_INSTALL_GENESIS_KEY: &str = "ocompForkInstallV1";
pub const METADOSIS_STORAGE_LAYOUT_GENESIS_KEY: &str = "metadosisStorageLayoutV1";
pub const GENESIS_ACTIVE_OCOMP_HEIGHT: u64 = 1;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenesisOcompForkInstallV1 {
    canonical_bytes: Bytes,
    install_hash: B256,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenesisMetadosisStorageLayoutV1 {
    layout_hash: B256,
}

/// Tooling/fixture parser for the immutable fork binding in the selected
/// chain spec. Production fresh-devnet startup must call
/// [`require_genesis_active_ocomp_fork_install`] and reject absence.
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

/// Strict fresh-devnet Metadosis startup contract.
pub fn require_genesis_active_ocomp_fork_install(
    chain_spec: &ChainSpec<OutbeHeader>,
) -> eyre::Result<Arc<OcompForkInstallV1>> {
    let install = require_startup_ocomp_fork_install(chain_spec)?;
    if install.classification != OcompForkInstallClassification::Measurement
        || install.activation_height != GENESIS_ACTIVE_OCOMP_HEIGHT
    {
        eyre::bail!(
            "fresh-devnet OCOMP install must be Measurement at block {}, got {:?} at {}",
            GENESIS_ACTIVE_OCOMP_HEIGHT,
            install.classification,
            install.activation_height
        );
    }
    Ok(install)
}

/// Production parser contract shared by the new fresh devnet and the frozen
/// OCOMP public-closure chain profile.
pub fn require_startup_ocomp_fork_install(
    chain_spec: &ChainSpec<OutbeHeader>,
) -> eyre::Result<Arc<OcompForkInstallV1>> {
    let install = load_ocomp_fork_install(chain_spec)?.ok_or_else(|| {
        eyre::eyre!("chain genesis config is missing required {OCOMP_FORK_INSTALL_GENESIS_KEY}")
    })?;
    match install.classification {
        OcompForkInstallClassification::Measurement => {
            if install.activation_height != GENESIS_ACTIVE_OCOMP_HEIGHT {
                eyre::bail!(
                    "fresh-devnet Measurement OCOMP install must activate at block {}, got {}",
                    GENESIS_ACTIVE_OCOMP_HEIGHT,
                    install.activation_height
                );
            }
            require_fresh_devnet_metadosis_layout(chain_spec)?;
        }
        OcompForkInstallClassification::Final => {
            // `validate_for_chain` already pins Final to the frozen OCOMP
            // activation height. This is the existing closure profile, not the
            // fresh-devnet Metadosis profile.
        }
    }
    Ok(install)
}

fn require_fresh_devnet_metadosis_layout(chain_spec: &ChainSpec<OutbeHeader>) -> eyre::Result<()> {
    let extra = &chain_spec.genesis.config.extra_fields;
    let parsed = extra
        .get_deserialized::<GenesisMetadosisStorageLayoutV1>(METADOSIS_STORAGE_LAYOUT_GENESIS_KEY)
        .ok_or_else(|| {
            eyre::eyre!(
                "fresh-devnet genesis config is missing required \
                 {METADOSIS_STORAGE_LAYOUT_GENESIS_KEY}"
            )
        })?;
    let manifest =
        parsed.map_err(|error| eyre::eyre!("invalid genesis Metadosis storage layout: {error}"))?;
    if manifest.layout_hash != METADOSIS_STORAGE_LAYOUT_V1_HASH {
        eyre::bail!(
            "fresh-devnet Metadosis storage layout hash mismatch: manifest {}, node {}",
            manifest.layout_hash,
            METADOSIS_STORAGE_LAYOUT_V1_HASH
        );
    }
    Ok(())
}
