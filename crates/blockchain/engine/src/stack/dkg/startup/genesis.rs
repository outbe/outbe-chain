//! The one-time interactive genesis ceremony, admitted only by the startup proof gate.
use super::*;

pub(super) async fn run_genesis_dkg<C>(
    clock: C,
    key_backend: &bls::KeyBackend,
    request: ThresholdMaterialRequest<'_>,
    dkg_sender: impl P2pSender<PublicKey = bls12381::PublicKey>,
    dkg_receiver: impl P2pReceiver<PublicKey = bls12381::PublicKey>,
) -> Result<ThresholdMaterial>
where
    C: Clock,
{
    let ThresholdMaterialRequest {
        args,
        signing_key,
        validator_set,
        context: startup_dkg_context,
    } = request;
    // Path 3: Run interactive DKG ceremony.
    let local_pk = signing_key.public_key();
    let startup_participants: commonware_utils::ordered::Set<bls12381::PublicKey> = validator_set
        .public_keys
        .clone()
        .into_iter()
        .try_collect()
        .map_err(|e| eyre::eyre!("invalid participant set: {e}"))?;
    let local_key_in_current_consensus_set = startup_participants.position(&local_pk).is_some();
    match startup_dkg_mode(startup_dkg_context, local_key_in_current_consensus_set) {
        StartupDkgMode::LiveJoinRequired => {
            warn!(
                last_execution_height = startup_dkg_context.last_execution_height,
                has_finalized_dkg_boundary = startup_dkg_context.has_chain_finalized_dkg_boundary(),
                local_key_in_current_consensus_set,
                "no current threshold material is available for existing-chain startup"
            );
            return Err(missing_current_threshold_material_error(
                "no current threshold material is available for existing-chain startup",
            ));
        }
        StartupDkgMode::InitialGenesisDkg => {}
    }

    info!("no threshold material available - running DKG ceremony (NO BLOCKS until complete)");

    let dkg_result = dkg_actor::run_initial_dkg_durable(
        &clock,
        signing_key,
        startup_participants,
        None, // initial: no previous output
        None, // initial: no previous share
        0,    // initial: round 0
        None,
        None,
        dkg_retry_store(args, key_backend)?,
        dkg_sender,
        dkg_receiver,
    )
    .await
    .wrap_err("DKG ceremony failed")?;

    let polynomial = dkg_result.output.public().clone();
    let signing_share = dkg_result.share;
    info!(
        vrf_group_public_key = %vrf_group_public_key_hash(&polynomial),
        "initial DKG ceremony completed; threshold material ready"
    );

    // Save DKG state to keys_dir for future restarts.
    if let Some(ref keys_dir) = args.keys_dir {
        let save_result = save_dkg_state(
            DkgStateStore::new(keys_dir, key_backend),
            DkgStateMaterial {
                share: &signing_share,
                polynomial: &polynomial,
                output: &dkg_result.output,
            },
        );
        if let Err(e) = save_result {
            warn!(
                ?e,
                "failed to save DKG state to disk (node will need to re-run DKG on restart)"
            );
        } else {
            info!(keys_dir = %keys_dir.display(), "saved DKG state to disk");
        }
    } else {
        warn!("no --consensus.keys-dir set, DKG state will not be persisted");
    }

    info!("DKG ceremony complete - threshold material obtained via P2P");

    Ok(ThresholdMaterial::Ready {
        signing_share,
        polynomial,
        last_dkg_output: Some(dkg_result.output),
        bootstrap_from_live_dkg: true,
    })
}
