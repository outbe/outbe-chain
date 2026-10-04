//! Freeze a deterministic target and supervise durable DKG ceremonies.
use super::*;

impl<E> EpochSupervisor<E>
where
    E: BufferPooler
        + Clock
        + CryptoRng
        + Network
        + Resolver
        + Spawner
        + Storage
        + Metrics
        + Send
        + Sync
        + 'static,
{
    pub(super) async fn retry_frozen_rotation(
        &mut self,
        ctx: &E,
        target: FrozenDkgTarget,
    ) -> Result<()> {
        info!(
            dkg_cycle = target.dkg_cycle,
            planned_activation_height = target.planned_activation_height,
            "retrying DKG for frozen target"
        );
        self.rotation.reshare_in_progress = true;
        outbe_consensus::metrics::record_dkg_status(1);

        match self.rotation.dkg_mux.register(target.dkg_cycle).await {
            Ok((dkg_tx, dkg_rx)) => {
                self.launch_rotation_ceremony(
                    ctx,
                    &target,
                    (dkg_tx, dkg_rx),
                    CeremonyAttempt::Retry,
                )?;
            }
            Err(e) => {
                warn!(?e, "failed to register DKG subchannel for retry");
                self.rotation.reshare_in_progress = false;
                self.rotation.retry_frozen_dkg = true;
            }
        }
        Ok(())
    }
}
