use super::super::*;

pub(in crate::stack) fn radicle_signer_enabled(
    gate: RadicleVotingGate,
    has_share: bool,
) -> Result<bool> {
    match gate {
        RadicleVotingGate::Verifier => Ok(false),
        RadicleVotingGate::SignerAllowed => Ok(has_share),
        RadicleVotingGate::Fatal(error) => {
            let reason = match error {
                RadicleVotingGateError::SidecarUnavailable => "sidecar unavailable",
                RadicleVotingGateError::LocalNodeIdUnavailable => "local NodeId unavailable",
                RadicleVotingGateError::ActiveBindingMissing => "active binding missing",
                RadicleVotingGateError::BindingMismatch => "binding mismatch",
            };
            Err(eyre::eyre!("Radicle voting gate is fatal: {reason}"))
        }
    }
}

pub(in crate::stack) async fn wait_for_radicle_role_change(
    updates: &mut tokio::sync::watch::Receiver<outbe_radicle::integration::RadicleStatusSnapshot>,
    current_signer: bool,
    has_share: bool,
) -> Result<bool> {
    loop {
        let desired_signer = radicle_signer_enabled(updates.borrow().voting_gate, has_share)?;
        if desired_signer != current_signer {
            return Ok(desired_signer);
        }
        if updates.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

pub(in crate::stack) fn epoch_validation_inputs(
    epoch: Epoch,
    participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
    validator_set: &validators::ValidatorSet,
    recovered_boundary: Option<&DkgBoundaryArtifact>,
    vrf_materials: &VrfMaterialProvider<MinSig>,
) -> Result<(HybridScheme<MinSig>, Vec<alloy_primitives::Address>)> {
    let verifier_scheme = HybridScheme::<MinSig>::verifier_with_vrf_provider(
        &config::outbe_app_namespace(),
        participants.clone(),
        vrf_materials.clone(),
    )
    .ok_or_else(|| {
        eyre::eyre!("failed to build verifier scheme for validator set (epoch {epoch})")
    })?;
    // Simplex participant indices follow ordered::Set pubkey ordering, not the
    // original validator_set order; certificate signer bitmaps use this order.
    // On restart/live-join with a recovered DKG boundary, provider-latest state
    // may include an unfinalized membership-changing head. Use the boundary's
    // own participant-index-aligned address vector for that recovered epoch.
    let ordered_addresses = match recovered_boundary {
        Some(boundary) => ordered_addresses_from_recovered_boundary(participants, boundary)?,
        None => ordered_validator_addresses(participants, validator_set)?,
    };
    Ok((verifier_scheme, ordered_addresses))
}

pub(in crate::stack) fn register_epoch_validation_providers(
    epoch: Epoch,
    participants: &commonware_utils::ordered::Set<bls12381::PublicKey>,
    validator_set: &validators::ValidatorSet,
    recovered_boundary: Option<&DkgBoundaryArtifact>,
    vrf_materials: &VrfMaterialProvider<MinSig>,
    certificate_scheme_provider: &HybridSchemeProvider<MinSig>,
    committee_provider: &CommitteeProvider,
) -> Result<()> {
    let (verifier_scheme, ordered_addresses) = epoch_validation_inputs(
        epoch,
        participants,
        validator_set,
        recovered_boundary,
        vrf_materials,
    )?;
    let _ = certificate_scheme_provider.register(epoch, verifier_scheme);
    let _ = committee_provider.register(epoch, ordered_addresses);
    Ok(())
}

pub(in crate::stack) fn validate_validator_evm_signer(
    args: &ConsensusArgs,
    signing_key: &bls12381::PrivateKey,
    consensus_validator_set: &validators::ValidatorSet,
    reshare_target_validator_set: &validators::ValidatorSet,
    recovered_committee: Option<(
        &commonware_utils::ordered::Set<bls12381::PublicKey>,
        &DkgBoundaryArtifact,
    )>,
    shareless_verifier: bool,
) -> Result<Option<EthAddress>> {
    let Some(evm_key_path) = args.effective_validator_evm_key()? else {
        return Ok(None);
    };
    let signer =
        outbe_primitives::signer::OutbeEvmSigner::from_file(&evm_key_path).wrap_err_with(|| {
            format!(
                "failed to load validator EVM key from {}",
                evm_key_path.display()
            )
        })?;
    let signer_address = signer.address();

    if let Some((participants, boundary)) = recovered_committee {
        let ordered_addresses =
            ordered_addresses_from_recovered_boundary(participants, boundary)
                .wrap_err("failed to validate recovered DKG boundary committee for EVM signer")?;
        let local_public_key = signing_key.public_key();
        let Some(participant_index) = participants.position(&local_public_key) else {
            if shareless_verifier {
                let Some(target_index) = reshare_target_validator_set
                    .addresses
                    .iter()
                    .position(|address| *address == signer_address)
                else {
                    info!(
                    target: "outbe_engine::stack",
                                           %signer_address,
                                           epoch = boundary.epoch,
                                           "shareless verifier is not yet in the canonical reshare target; retaining no proposer identity"
                                       );
                    return Ok(None);
                };
                let target_public_key = reshare_target_validator_set
                    .public_keys
                    .get(target_index)
                    .ok_or_else(|| {
                        eyre::eyre!(
                            "current reshare target is missing the BLS public key for EVM address {}",
                            signer_address
                        )
                    })?;
                ensure!(
                    target_public_key == &local_public_key,
                    "validator EVM key address {} belongs to a different BLS consensus key in the current reshare target",
                    signer_address
                );
                info!(
                    %signer_address,
                    epoch = boundary.epoch,
                    "shareless validator identity matches the canonical reshare target; \
                     the node remains authority-free until DKG grants its threshold share"
                );
                return Ok(Some(signer_address));
            }
            eyre::bail!(
                "local BLS key is not in recovered DKG boundary committee for epoch {}; \
                 refusing latest-state EVM signer authorization",
                boundary.epoch
            );
        };
        let expected_address = ordered_addresses.get(participant_index).ok_or_else(|| {
            eyre::eyre!(
                "recovered DKG boundary address mapping missing participant index {}",
                participant_index
            )
        })?;
        ensure!(
            *expected_address == signer_address,
            "validator EVM key address {} does not match recovered DKG boundary address {} \
             for local BLS consensus key",
            signer_address,
            expected_address
        );
        info!(
            address = %signer_address,
            epoch = boundary.epoch,
            "validated validator EVM signer against recovered DKG boundary"
        );
        return Ok(Some(signer_address));
    }

    let authorized = consensus_validator_set
        .addresses
        .iter()
        .position(|address| *address == signer_address)
        .map(|index| {
            (
                &consensus_validator_set.public_keys,
                index,
                "active consensus participant set",
            )
        })
        .or_else(|| {
            reshare_target_validator_set
                .addresses
                .iter()
                .position(|address| *address == signer_address)
                .map(|index| {
                    (
                        &reshare_target_validator_set.public_keys,
                        index,
                        "current reshare target",
                    )
                })
        });
    let Some((public_keys, index, source_set)) = authorized else {
        if shareless_verifier {
            info!(
                %signer_address,
                "verifier-join: EVM signer is not yet in the on-chain validator set; the node \
                 syncs as a verifier and resolves its proposer address once a reshare grants \
                 it a share"
            );
            return Ok(None);
        }
        eyre::bail!(
            "validator EVM key address {} is neither in the active consensus participant set nor the active validator set",
            signer_address
        );
    };

    let local_public_key = signing_key.public_key();
    let Some(registered_public_key) = public_keys.get(index) else {
        eyre::bail!(
            "validator set missing BLS public key for EVM address {}",
            signer_address
        );
    };
    ensure!(
        registered_public_key == &local_public_key,
        "validator EVM key address {} belongs to a different BLS consensus key",
        signer_address
    );
    let address = signer_address;
    info!(
        address = %address,
        source_set,
        "validated validator EVM signer"
    );
    Ok(Some(address))
}
