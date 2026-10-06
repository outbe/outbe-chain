//! Explicit signer or shareless verifier provisioning from CLI material.
use super::*;

pub(super) fn load_manual_material(
    args: &ConsensusArgs,
    key_backend: &bls::KeyBackend,
    startup_dkg_context: StartupDkgContext,
) -> Result<Option<ThresholdMaterial>> {
    if let Some(material) = load_cli_signer(args, key_backend, startup_dkg_context)? {
        return Ok(Some(material));
    }
    load_cli_verifier(args, key_backend)
}

fn load_cli_signer(
    args: &ConsensusArgs,
    key_backend: &bls::KeyBackend,
    startup_dkg_context: StartupDkgContext,
) -> Result<Option<ThresholdMaterial>> {
    // Path 2: Load from CLI args (fresh bootstrap / manual provisioning).
    if let (Some(share_path), Some(poly_path)) = (&args.signing_share, &args.public_polynomial) {
        let signing_share = bls::load_signing_share(share_path, key_backend)
            .wrap_err("failed to load BLS signing share")?;
        let polynomial = bls::load_public_polynomial(poly_path, key_backend)
            .wrap_err("failed to load BLS public polynomial")?;
        let cli_dkg_output = if let Some(output_path) = &args.dkg_output {
            let output = bls::load_dkg_output(output_path, key_backend)
                .wrap_err("failed to load BLS DKG output")?;
            bls::validate_dkg_triplet(&signing_share, &polynomial, &output)
                .wrap_err("CLI DKG material triplet is inconsistent")?;
            Some(output)
        } else {
            None
        };

        if startup_dkg_context.recovered_dkg_output_hash.is_some() && cli_dkg_output.is_none() {
            warn!(
                share_path = %share_path.display(),
                poly_path = %poly_path.display(),
                recovered_dkg_output_hash = ?startup_dkg_context.recovered_dkg_output_hash,
                "CLI DKG material lacks required output for recovered chain boundary"
            );
            return Err(missing_current_threshold_material_error(
                "CLI DKG material lacks the output required by the latest finalized boundary",
            ));
        }

        if !vrf_material_matches_recovered_boundary(&polynomial, startup_dkg_context)
            || cli_dkg_output.as_ref().is_some_and(|output| {
                !dkg_output_matches_recovered_boundary(output, startup_dkg_context)
            })
        {
            warn!(
                share_path = %share_path.display(),
                poly_path = %poly_path.display(),
                local_vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
                local_dkg_output_hash = ?cli_dkg_output.as_ref().map(dkg_manager::dkg_output_hash),
                recovered_vrf_group_public_key = ?startup_dkg_context.recovered_vrf_group_public_key,
                recovered_dkg_output_hash = ?startup_dkg_context.recovered_dkg_output_hash,
                "CLI DKG material is stale for the latest finalized boundary"
            );
            return Err(missing_current_threshold_material_error(
                "CLI DKG material is stale for the latest finalized boundary",
            ));
        }

        info!(
            vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
            "threshold material ready from CLI args"
        );
        return Ok(Some(ThresholdMaterial::Ready {
            signing_share,
            polynomial,
            last_dkg_output: cli_dkg_output,
            bootstrap_from_live_dkg: false,
        }));
    }

    Ok(None)
}

fn load_cli_verifier(
    args: &ConsensusArgs,
    key_backend: &bls::KeyBackend,
) -> Result<Option<ThresholdMaterial>> {
    if args.signing_share.is_some() {
        return Ok(None);
    }
    if let (Some(poly_path), Some(output_path)) = (&args.public_polynomial, &args.dkg_output) {
        let polynomial = bls::load_public_polynomial(poly_path, key_backend)
            .wrap_err("failed to load BLS public polynomial for verifier-join")?;
        let output = bls::load_dkg_output(output_path, key_backend)
            .wrap_err("failed to load BLS DKG output for verifier-join")?;
        info!(
            vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
            "verifier-join: no threshold share; running consensus in VERIFIER mode \
             (follow + verify) until the next reshare grants a share"
        );
        return Ok(Some(ThresholdMaterial::VerifierOnly {
            polynomial,
            last_dkg_output: Some(output),
        }));
    }
    Ok(None)
}
