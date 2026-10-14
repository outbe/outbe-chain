//! Startup loading of the consensus-bound OCOMP fork install.

use std::sync::Arc;

use alloy_primitives::{Bytes, B256};
use outbe_metadosis::config::{
    OcompForkInstallClassification, OcompForkInstallV1, OCOMP_AWAITING_FINALITY_DEADLINE_BLOCKS,
};
use outbe_metadosis::proof_layout::METADOSIS_STORAGE_LAYOUT_V1_HASH;
use outbe_ocomp_protocol::profile::poc_schema_limits;
use outbe_primitives::OutbeHeader;
use reth_chainspec::ChainSpec;

use crate::errors::OcompForkInstallError;

/// `genesis.config` key in the selected chain manifest.
pub const OCOMP_FORK_INSTALL_GENESIS_KEY: &str = "ocompForkInstallV1";
pub const METADOSIS_STORAGE_LAYOUT_GENESIS_KEY: &str = "metadosisStorageLayoutV1";
pub const EPOCH_LENGTH_BLOCKS_GENESIS_KEY: &str = "epochLengthBlocks";
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
/// chain spec. Production startup must call
/// [`require_startup_ocomp_fork_install`], which rejects absence and accepts a
/// `Measurement` or a `Final` install.
pub fn load_ocomp_fork_install(
    chain_spec: &ChainSpec<OutbeHeader>,
) -> eyre::Result<Option<Arc<OcompForkInstallV1>>> {
    let extra = &chain_spec.genesis.config.extra_fields;
    let Some(parsed) =
        extra.get_deserialized::<GenesisOcompForkInstallV1>(OCOMP_FORK_INSTALL_GENESIS_KEY)
    else {
        return Ok(None);
    };
    let manifest = parsed.map_err(OcompForkInstallError::invalid_genesis_config)?;
    let limits = poc_schema_limits();
    let install = OcompForkInstallV1::decode_canonical(&manifest.canonical_bytes, &limits)
        .map_err(OcompForkInstallError::invalid_canonical_install)?;
    install
        .validate_for_chain(chain_spec.chain().id(), chain_spec.genesis_hash(), &limits)
        .map_err(OcompForkInstallError::chain_binding_mismatch)?;
    let actual_hash = install
        .install_hash(&limits)
        .map_err(OcompForkInstallError::hash_install)?;
    if actual_hash != manifest.install_hash {
        return Err(OcompForkInstallError::InstallHashMismatch {
            manifest: manifest.install_hash,
            canonical: actual_hash,
        }
        .into());
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
        return Err(OcompForkInstallError::InvalidFreshDevnetInstall {
            expected_height: GENESIS_ACTIVE_OCOMP_HEIGHT,
            classification: install.classification,
            actual_height: install.activation_height,
        }
        .into());
    }
    Ok(install)
}

/// Production parser contract shared by the new fresh devnet and the frozen
/// OCOMP public-closure chain profile.
pub fn require_startup_ocomp_fork_install(
    chain_spec: &ChainSpec<OutbeHeader>,
) -> eyre::Result<Arc<OcompForkInstallV1>> {
    let install =
        load_ocomp_fork_install(chain_spec)?.ok_or(OcompForkInstallError::MissingGenesisKey {
            key: OCOMP_FORK_INSTALL_GENESIS_KEY,
        })?;
    match install.classification {
        OcompForkInstallClassification::Measurement => {
            if install.activation_height != GENESIS_ACTIVE_OCOMP_HEIGHT {
                return Err(OcompForkInstallError::InvalidMeasurementActivationHeight {
                    expected_height: GENESIS_ACTIVE_OCOMP_HEIGHT,
                    actual_height: install.activation_height,
                }
                .into());
            }
            require_fresh_devnet_metadosis_layout(chain_spec)?;
        }
        OcompForkInstallClassification::Final => {
            // `validate_for_chain` already pins Final to the frozen OCOMP
            // activation height. This is the existing closure profile, not the
            // fresh-devnet Metadosis profile.
        }
    }
    require_ocomp_snapshot_retention_horizon(chain_spec, &install)?;
    Ok(install)
}

fn require_ocomp_snapshot_retention_horizon(
    chain_spec: &ChainSpec<OutbeHeader>,
    install: &OcompForkInstallV1,
) -> eyre::Result<()> {
    let epoch_length_blocks = chain_spec
        .genesis
        .config
        .extra_fields
        .get_deserialized::<u64>(EPOCH_LENGTH_BLOCKS_GENESIS_KEY)
        .transpose()
        .map_err(|error| {
            OcompForkInstallError::invalid_genesis_field(EPOCH_LENGTH_BLOCKS_GENESIS_KEY, error)
        })?
        .ok_or(OcompForkInstallError::MissingOcompGenesisKey {
            key: EPOCH_LENGTH_BLOCKS_GENESIS_KEY,
        })?;
    let retained_epoch_span = outbe_validatorset::COMMITTEE_SNAPSHOT_RETAIN_EPOCHS
        .checked_sub(1)
        .and_then(|epochs| epochs.checked_mul(epoch_length_blocks))
        .ok_or(OcompForkInstallError::SnapshotRetentionHorizonOverflow)?;
    let required_job_lifetime = OCOMP_AWAITING_FINALITY_DEADLINE_BLOCKS
        .checked_add(outbe_ocomp_protocol::state::RESULT_VOTE_MIN_FINALITY_DEPTH)
        .and_then(|blocks| {
            blocks.checked_add(
                install
                    .request_profile
                    .capacity_profile
                    .result_deadline_blocks,
            )
        })
        .ok_or(OcompForkInstallError::MaximumJobLifetimeOverflow)?;
    if retained_epoch_span <= required_job_lifetime {
        return Err(OcompForkInstallError::SnapshotRetentionHorizonTooShort {
            retain_epochs: outbe_validatorset::COMMITTEE_SNAPSHOT_RETAIN_EPOCHS,
            epoch_length_blocks,
            retained_epoch_span,
            awaiting_finality_deadline_blocks: OCOMP_AWAITING_FINALITY_DEADLINE_BLOCKS,
            min_finality_depth: outbe_ocomp_protocol::state::RESULT_VOTE_MIN_FINALITY_DEPTH,
            result_deadline_blocks: install
                .request_profile
                .capacity_profile
                .result_deadline_blocks,
            required_job_lifetime,
        }
        .into());
    }
    Ok(())
}

fn require_fresh_devnet_metadosis_layout(chain_spec: &ChainSpec<OutbeHeader>) -> eyre::Result<()> {
    let extra = &chain_spec.genesis.config.extra_fields;
    let parsed = extra
        .get_deserialized::<GenesisMetadosisStorageLayoutV1>(METADOSIS_STORAGE_LAYOUT_GENESIS_KEY)
        .ok_or(OcompForkInstallError::MissingFreshDevnetGenesisKey {
            key: METADOSIS_STORAGE_LAYOUT_GENESIS_KEY,
        })?;
    let manifest = parsed.map_err(OcompForkInstallError::invalid_metadosis_storage_layout)?;
    if manifest.layout_hash != METADOSIS_STORAGE_LAYOUT_V1_HASH {
        return Err(OcompForkInstallError::MetadosisStorageLayoutHashMismatch {
            manifest: manifest.layout_hash,
            node: METADOSIS_STORAGE_LAYOUT_V1_HASH,
        }
        .into());
    }
    Ok(())
}
