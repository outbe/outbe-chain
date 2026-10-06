//! Restart precedence and recovery of durable pending threshold material.
use super::*;

pub(super) fn load_local_material(
    args: &ConsensusArgs,
    key_backend: &bls::KeyBackend,
    startup_dkg_context: StartupDkgContext,
) -> Result<Option<ThresholdMaterial>> {
    let Some(keys_dir) = args.keys_dir.as_ref() else {
        return Ok(None);
    };
    let saved_state_error = match load_committed_material(
        keys_dir,
        key_backend,
        startup_dkg_context,
    ) {
        Ok(Some(material)) => return Ok(Some(material)),
        Ok(None) => None,
        Err(error) => {
            warn!(
                %error,
                keys_dir = %keys_dir.display(),
                "saved DKG state is incomplete or corrupt; checking pending DKG state before failing"
            );
            Some(error)
        }
    };
    if let Some(material) = recover_pending_material(keys_dir, key_backend, startup_dkg_context)? {
        return Ok(Some(material));
    }
    if let Some(error) = saved_state_error {
        return Err(error)
            .wrap_err("saved DKG state failed to load and pending state could not be promoted");
    }
    validate_local_fallback(args, startup_dkg_context)?;
    Ok(None)
}

fn load_committed_material(
    keys_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
    startup_dkg_context: StartupDkgContext,
) -> Result<Option<ThresholdMaterial>> {
    if let Some((signing_share, polynomial, output)) = load_saved_dkg_state(keys_dir, key_backend)?
    {
        if vrf_material_matches_recovered_boundary(&polynomial, startup_dkg_context)
            && dkg_output_matches_recovered_boundary(&output, startup_dkg_context)
        {
            info!(
                keys_dir = %keys_dir.display(),
                vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                "threshold material ready from saved DKG state"
            );
            return Ok(Some(ThresholdMaterial::Ready {
                signing_share,
                polynomial,
                last_dkg_output: Some(output),
                bootstrap_from_live_dkg: false,
            }));
        }
        warn!(
            keys_dir = %keys_dir.display(),
            local_vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
            local_dkg_output_hash = %dkg_manager::dkg_output_hash(&output),
            recovered_vrf_group_public_key = ?startup_dkg_context.recovered_vrf_group_public_key,
            recovered_dkg_output_hash = ?startup_dkg_context.recovered_dkg_output_hash,
            "saved DKG material is stale for the latest finalized boundary; checking pending DKG state"
        );
    }

    Ok(None)
}

fn recover_pending_material(
    keys_dir: &std::path::Path,
    key_backend: &bls::KeyBackend,
    startup_dkg_context: StartupDkgContext,
) -> Result<Option<ThresholdMaterial>> {
    let pending_state = match load_pending_dkg_state(keys_dir, key_backend) {
        Ok(state) => state,
        Err(error) => {
            warn!(
                %error,
                keys_dir = %keys_dir.display(),
                "pending DKG state is incomplete or corrupt; ignoring pending material"
            );
            None
        }
    };
    if let Some((signing_share, polynomial, output)) = pending_state {
        if startup_dkg_context.recovered_dkg_output_hash.is_some()
            && vrf_material_matches_recovered_boundary(&polynomial, startup_dkg_context)
            && dkg_output_matches_recovered_boundary(&output, startup_dkg_context)
        {
            if startup_dkg_context.recovered_boundary_finalized {
                promote_recovered_pending_material(
                    DkgStateStore::new(keys_dir, key_backend),
                    DkgStateMaterial {
                        share: &signing_share,
                        polynomial: &polynomial,
                        output: &output,
                    },
                )?;
            } else {
                info!(
                    keys_dir = %keys_dir.display(),
                    vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                    dkg_output_hash = %dkg_manager::dkg_output_hash(&output),
                    "threshold material ready from durable pending DKG state and boundary snapshot"
                );
            }
            return Ok(Some(ThresholdMaterial::Ready {
                signing_share,
                polynomial,
                last_dkg_output: Some(output),
                bootstrap_from_live_dkg: false,
            }));
        }
        warn!(
            keys_dir = %keys_dir.display(),
            local_vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
            local_dkg_output_hash = %dkg_manager::dkg_output_hash(&output),
            recovered_vrf_group_public_key = ?startup_dkg_context.recovered_vrf_group_public_key,
            recovered_dkg_output_hash = ?startup_dkg_context.recovered_dkg_output_hash,
            "pending DKG material is not finalized for the latest boundary"
        );
    }

    Ok(None)
}

fn promote_recovered_pending_material(
    store: DkgStateStore<'_>,
    material: DkgStateMaterial<'_>,
) -> Result<()> {
    let DkgStateStore {
        directory: keys_dir,
        key_backend,
    } = store;
    let polynomial = material.polynomial;
    let output = material.output;
    save_dkg_state(DkgStateStore::new(keys_dir, key_backend), material)
        .wrap_err("failed to promote pending DKG state after boundary finalization")?;
    remove_pending_dkg_state(keys_dir);
    clear_pending_dkg_boundary(keys_dir);
    dkg_actor::DkgRetryStore::in_keys_dir(keys_dir, key_backend.clone())
        .clear()
        .wrap_err("failed to retire recovered DKG retry state")?;
    info!(
        keys_dir = %keys_dir.display(),
        vrf_group_public_key = %vrf_group_public_key_hash(polynomial),
        dkg_output_hash = %dkg_manager::dkg_output_hash(output),
        "threshold material ready from promoted pending DKG state"
    );
    Ok(())
}

fn validate_local_fallback(
    args: &ConsensusArgs,
    startup_dkg_context: StartupDkgContext,
) -> Result<()> {
    if startup_dkg_context.has_chain_finalized_dkg_boundary()
        && !startup_dkg_context.recovered_boundary_finalized
    {
        return Err(eyre::eyre!(
            "pending DKG boundary snapshot was recovered but matching DKG material is unavailable"
        ));
    }

    if startup_dkg_context.has_chain_finalized_dkg_boundary()
        && !(args.signing_share.is_none()
            && args.public_polynomial.is_some()
            && args.dkg_output.is_some())
    {
        return Err(missing_current_threshold_material_error(
            "saved and pending DKG material do not match the latest finalized boundary",
        ));
    }
    Ok(())
}
