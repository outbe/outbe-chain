//! Build a Simplex engine with the epoch's authority and continuity.
use super::super::*;
use super::runtime::*;
use super::transport::EpochSubchannels;

/// Build the leader-elector config for an epoch start.
///
/// Epoch 0 has no previous finalized certificate, so view 1 uses the one-time
/// genesis round-robin exception. Every later epoch must start from the last
/// finalized certificate of the previous epoch so that view 1 continues to use
/// VRF-derived leader selection rather than silently falling back to round-robin.
pub(in crate::stack) fn epoch_elector_config(
    epoch: Epoch,
    continuity: &ReporterContinuity,
    vrf_materials: VrfMaterialProvider<MinSig>,
) -> Result<HybridRandom<MinSig>> {
    if epoch.get() == 0 {
        return Ok(HybridRandom::with_vrf_materials(vrf_materials));
    }

    let snapshot = continuity.snapshot();
    if snapshot.last_finalized_view == 0 {
        warn!(
            epoch = epoch.get(),
            "starting epoch without reporter continuity; leader election will use active VRF material until a certificate is finalized"
        );
        return Ok(HybridRandom::with_vrf_materials(vrf_materials));
    }
    let seed = snapshot.last_vrf_seed.unwrap_or_default();
    if seed.is_empty() {
        Ok(HybridRandom::with_vrf_materials(vrf_materials))
    } else {
        Ok(HybridRandom::with_bootstrap_seed_and_vrf_materials(
            seed,
            vrf_materials,
        ))
    }
}

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
    pub(super) async fn start_engine(
        &mut self,
        ctx: &E,
        channels: EpochSubchannels<E>,
    ) -> Result<(commonware_runtime::Handle<()>, bool)> {
        let outbe_consensus::epoch_subchannels::EpochSubchannels {
            vote, cert, res, ..
        } = channels;
        // -- b. Build HybridScheme for this epoch ------------------------
        use commonware_consensus::simplex::elector::Config as ElectorConfig;
        let radicle_signer = radicle_signer_enabled(
            self.radicle_status.snapshot().voting_gate,
            self.state.signing_share.is_some(),
        )?;
        let scheme = if radicle_signer {
            HybridScheme::<MinSig>::signer_with_vrf_provider(
                &config::outbe_app_namespace(),
                self.state.participants.clone(),
                self.signing_key.clone(),
                self.vrf_materials.clone(),
            )
            .ok_or_else(|| {
                eyre::eyre!(
                    "signing key or BLS share invalid for validator set (epoch {current_epoch})",
                    current_epoch = self.state.current_epoch
                )
            })?
        } else {
            // Verifier mode (no threshold share this epoch): the engine follows and
            // verifies finalized blocks - driving its execution layer to sync - but
            // cannot propose or sign. `me()` is None, so the simplex engine never
            // invokes signing. The node acquires a share at the next reshare, after
            // which the next epoch iteration rebuilds this scheme as a signer (Stage 4).
            info!(
            target: "outbe_engine::stack",
                           epoch = %self.state.current_epoch,
                           "no threshold share for this epoch - running consensus engine in VERIFIER mode"
                       );
            HybridScheme::<MinSig>::verifier_with_vrf_provider(
                &config::outbe_app_namespace(),
                self.state.participants.clone(),
                self.vrf_materials.clone(),
            )
            .ok_or_else(|| {
                eyre::eyre!(
                    "verifier scheme invalid for validator set (epoch {current_epoch}): \
                     polynomial total ({}) must equal participant count ({})",
                    self.vrf_materials.active_polynomial_total().unwrap_or(0),
                    self.state.participants.len(),
                    current_epoch = self.state.current_epoch
                )
            })?
        };

        // -- c. Create reporter for this epoch ---------------------------
        let recovered_boundary_for_epoch = self
            .recovered_boundary_artifact
            .as_ref()
            .filter(|artifact| artifact.epoch == self.state.current_epoch.get());
        let (verifier_scheme, ordered_addresses) = epoch_validation_inputs(
            EpochValidationCommittee {
                epoch: self.state.current_epoch,
                participants: &self.state.participants,
                validator_set: &self.state.validator_set,
                recovered_boundary: recovered_boundary_for_epoch,
            },
            &self.vrf_materials,
        )?;

        let elector_config = epoch_elector_config(
            self.state.current_epoch,
            &self.reporter_continuity,
            self.vrf_materials.clone(),
        )?;
        let reporter_elector = elector_config.clone().build(&self.state.participants);

        let _ = self
            .certificate_scheme_provider
            .register(self.state.current_epoch, verifier_scheme.clone());
        let _ = self
            .elector_config_provider
            .register(self.state.current_epoch, elector_config.clone());
        let _ = self
            .committee_provider
            .register(self.state.current_epoch, ordered_addresses.clone());

        let outbe_reporter = OutbeReporter::new(
            self.reporter_continuity.clone(),
            ordered_addresses,
            self.finalization_mailbox.clone(),
            Some(self.bridge.clone()),
            verifier_scheme,
            reporter_elector,
            self.state.current_epoch,
            std::sync::Arc::new(self.finalized_parent_cert_store.clone()),
            self.finalize_verify_mailbox.clone(),
        );

        // Combine OutbeReporter + marshal mailbox as a joint Simplex reporter.
        // Both receive Activity events including Finalization:
        // - OutbeReporter: bridge/VRF/missed-proposer processing AND
        // `Activity::Certification` -> CertifiedParentProofStore.
        // - Marshal: finalized block delivery -> executor -> ack -> recovery truth.
        //   Marshal's mailbox drops Certification via its `_ => return;` arm
        //   (monorepo `consensus/src/marshal/core/mailbox.rs:396-410`), so
        //   ordering between Outbe and marshal does not need to be sequential;
        //   `Reporters::from((outbe, marshal))` runs both via `futures::join!`
        //   and Outbe is the sole persistent consumer of Certification.
        let combined_reporter = Reporters::from((outbe_reporter, self.marshal_mailbox.clone()));

        // -- d. Resolve the Simplex genesis floor -------------------------
        // commonware 2026.5.0 removed `Automaton::genesis(epoch)`; the
        // genesis anchor is now an explicit `simplex::Config.floor`. We must
        // feed the byte-identical value the old `handle_genesis(epoch)`
        // returned:
        //   * epoch 0  -> the chain genesis block hash (`Digest(genesis_hash)`),
        //     the parent of `view = 1` for the bootstrap engine.
        //   * epoch > 0 -> the canonical last-finalized block's hash (the
        //     continuity anchor read from `FinalizationView`), the parent of
        //     `view = 1` for the restarted engine.
        // We use `Floor::Genesis(digest)` in both cases (never
        // `Floor::Finalized`) so behaviour matches the prior synthetic
        // `parent_view = 0` resolution path.
        //
        // The bounded-wait guard below preserves the prior epoch-restart
        // invariant: for `epoch > 0` we must not start the engine until the
        // FinalizationActor has published the boundary block's anchor, or the
        // floor (and Phase 1 finalized-round proof key) would be missing. The
        // 5s deadline accommodates transient races between the
        // FinalizationActor and the DKG-manager-driven epoch advance.
        let floor_digest = super::continuity::resolve_epoch_floor(
            ctx,
            &self.finalization_view,
            self.state.current_epoch,
            self.genesis_hash,
        )
        .await?;

        // -- e. Build engine config --------------------------------------
        let simplex_cfg = simplex::Config {
            scheme,
            elector: elector_config,
            blocker: self.oracle.clone(),
            automaton: self.application.clone(),
            relay: self.application.clone(),
            forward: simplex::ForwardPolicy::Disabled,
            reporter: combined_reporter,
            strategy: commonware_parallel::Sequential,
            partition: format!("outbe-simplex-{}", self.state.current_epoch),
            mailbox_size: nonzero_usize(config::ENGINE_MAILBOX_SIZE, "ENGINE_MAILBOX_SIZE")?,
            epoch: self.state.current_epoch,
            floor: simplex::Floor::Genesis(floor_digest),
            replay_buffer: nonzero_usize(config::REPLAY_BUFFER, "REPLAY_BUFFER")?,
            write_buffer: nonzero_usize(config::WRITE_BUFFER, "WRITE_BUFFER")?,
            page_cache: self.page_cache.clone(),
            leader_timeout: self.bt.leader_timeout,
            certification_timeout: self.bt.certification_timeout,
            timeout_retry: config::DEFAULT_NULLIFY_REBROADCAST,
            view_retention: ViewDelta::new(u64::from(config::ACTIVITY_TIMEOUT)),
            skip: commonware_consensus::simplex::SkipPolicy::Disabled,
            track_historical_votes: true,
            fetch_timeout: config::DEFAULT_PEER_RESPONSE_TIMEOUT,
        };

        // -- f. Start engine ---------------------------------------------
        let engine = simplex::Engine::new(
            ctx.child("engine")
                .with_attribute("epoch", self.state.current_epoch),
            simplex_cfg,
        );
        let engine_handle_task = engine.start(vote, cert, res);

        info!(epoch = %self.state.current_epoch, "simplex engine started - blocks can now be produced");

        Ok((engine_handle_task, radicle_signer))
    }
}
