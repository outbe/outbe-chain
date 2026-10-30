//! VRF/DKG freshness gate.
//!
//! [`VrfSafetyGate`] is process-local. Proposer and verify checks share
//! `vrf_expiry_height` and do not share sticky [`RandomnessStatus::Expired`].
//!
//! - Proposer [`VrfSafetyGate::ensure_block_allowed`] rejects when
//!   `randomness_status == Expired` or `block_height > vrf_expiry_height`.
//!   `Expired` sticks for this process. [`VrfSafetyGate::new`] after a restart
//!   starts at `Healthy` and does not restore it.
//! - Verify [`VrfSafetyGate::ensure_verify_height_allowed`] is
//!   restart-deterministic. It checks only the numeric ceiling
//!   `block_height <= vrf_expiry_height` and rejects when
//!   `block_height > vrf_expiry_height`. It ignores process-local sticky
//!   [`RandomnessStatus::Expired`].
//!
//! `vrf_expiry_height` is
//! `next_planned_activation_height + activation_grace_blocks`. The
//! validator-set store does not keep that height.
//!
//! Importers:
//! 1. [`crate::application::handler`] — proposer emission
//!    (`ensure_block_allowed`) and verify prechecks
//!    (`ensure_verify_height_allowed`).
//! 2. [`crate::finalization::actor`] — `mark_degraded` after a missing VRF
//!    seed, and `snapshot` for the bridge `ConsensusStatus`.

use std::sync::{Arc, Mutex};

use crate::errors::VrfSafetyError;
use commonware_cryptography::bls12381::{
    self, dkg::feldman_desmedt::Output, primitives::variant::MinSig,
};
use eyre::Result;
use outbe_primitives::consensus::RandomnessStatus;

#[derive(Debug, Clone)]
pub struct VrfSafetySnapshot {
    pub randomness_status: RandomnessStatus,
    pub vrf_material_version: u64,
    pub last_dkg_activation_height: u64,
    pub next_planned_activation_height: u64,
    pub vrf_expiry_height: u64,
}

#[derive(Debug, Clone)]
struct VrfSafetyState {
    snapshot: VrfSafetySnapshot,
    /// Active DKG output contains revealed shares, or a completed ceremony
    /// has no clean output yet. `Expired` still wins in [`VrfSafetyGate::snapshot`].
    active_material_revealed: bool,
}

#[derive(Debug, Clone)]
pub struct VrfSafetyGate {
    inner: Arc<Mutex<VrfSafetyState>>,
}

impl VrfSafetyGate {
    pub fn new(
        vrf_material_version: u64,
        last_dkg_activation_height: u64,
        next_planned_activation_height: u64,
        activation_grace_blocks: u64,
    ) -> Self {
        let vrf_expiry_height =
            next_planned_activation_height.saturating_add(activation_grace_blocks);
        Self {
            inner: Arc::new(Mutex::new(VrfSafetyState {
                snapshot: VrfSafetySnapshot {
                    randomness_status: RandomnessStatus::Healthy,
                    vrf_material_version,
                    last_dkg_activation_height,
                    next_planned_activation_height,
                    vrf_expiry_height,
                },
                active_material_revealed: false,
            })),
        }
    }

    pub fn snapshot(&self) -> VrfSafetySnapshot {
        self.with_state(|state| effective_snapshot(state))
    }

    /// Record whether the active output contains revealed shares.
    ///
    /// A clean output clears the flag and, unless the gate is `Expired`,
    /// turns `Degraded` back into `Healthy`. A revealed output sets the flag
    /// and `Degraded`.
    pub fn apply_revealed_output(&self, output: &Output<MinSig, bls12381::PublicKey>) {
        self.with_state(|state| {
            state.active_material_revealed = !output.revealed().is_empty();
            if state.snapshot.randomness_status == RandomnessStatus::Expired {
                return;
            }
            if state.active_material_revealed {
                state.snapshot.randomness_status = RandomnessStatus::Degraded;
            } else if state.snapshot.randomness_status == RandomnessStatus::Degraded {
                state.snapshot.randomness_status = RandomnessStatus::Healthy;
            }
        });
    }

    /// Apply [`Self::apply_revealed_output`] when an output is present.
    pub fn apply_optional_revealed_output(
        &self,
        output: Option<&Output<MinSig, bls12381::PublicKey>>,
    ) {
        if let Some(output) = output {
            self.apply_revealed_output(output);
        }
    }

    /// Apply the active output after a ceremony handoff (`Some(output)` updates
    /// from `output.revealed()`, `None` marks carried-forward/missing material
    /// as degraded).
    pub fn apply_activated_ceremony_output(
        &self,
        output: Option<&Output<MinSig, bls12381::PublicKey>>,
    ) {
        match output {
            Some(output) => self.apply_revealed_output(output),
            None => self.apply_pending_ceremony_output(None),
        }
    }

    /// D3-B: a finished ceremony degrades immediately when its output is
    /// missing or contains revealed shares. A clean output leaves the active
    /// key unchanged until activation calls [`Self::apply_revealed_output`].
    pub fn apply_pending_ceremony_output(
        &self,
        output: Option<&Output<MinSig, bls12381::PublicKey>>,
    ) {
        let degrades_active_key = match output {
            None => true,
            Some(output) => !output.revealed().is_empty(),
        };
        if !degrades_active_key {
            return;
        }
        self.with_state(|state| {
            state.active_material_revealed = true;
            if state.snapshot.randomness_status != RandomnessStatus::Expired {
                state.snapshot.randomness_status = RandomnessStatus::Degraded;
            }
        });
    }

    pub fn note_preparing(
        &self,
        dkg_cycle: u64,
        freeze_height: u64,
        planned_activation_height: u64,
        activation_grace_blocks: u64,
    ) {
        tracing::info!(
            dkg_cycle,
            freeze_height,
            planned_activation_height,
            "VRF/DKG rotation entered prepare window"
        );
        self.update_status(
            RandomnessStatus::Preparing,
            planned_activation_height,
            activation_grace_blocks,
        );
    }

    pub fn note_pending_activation(
        &self,
        planned_activation_height: u64,
        activation_grace_blocks: u64,
    ) {
        self.update_status(
            RandomnessStatus::PendingActivation,
            planned_activation_height,
            activation_grace_blocks,
        );
    }

    pub fn note_grace(&self, planned_activation_height: u64, activation_grace_blocks: u64) {
        self.update_status(
            RandomnessStatus::Grace,
            planned_activation_height,
            activation_grace_blocks,
        );
    }

    pub fn note_activated(
        &self,
        vrf_material_version: u64,
        activation_height: u64,
        next_planned_activation_height: u64,
        activation_grace_blocks: u64,
    ) {
        let next_expiry_height =
            next_planned_activation_height.saturating_add(activation_grace_blocks);
        self.with_state(|state| {
            if state.snapshot.randomness_status == RandomnessStatus::Expired {
                return;
            }
            state.snapshot = VrfSafetySnapshot {
                randomness_status: forward_status(state, RandomnessStatus::Healthy),
                vrf_material_version,
                last_dkg_activation_height: activation_height,
                next_planned_activation_height,
                vrf_expiry_height: next_expiry_height,
            };
        });
    }

    pub fn mark_degraded(&self) {
        self.with_state(|state| {
            if state.snapshot.randomness_status != RandomnessStatus::Expired {
                state.snapshot.randomness_status = RandomnessStatus::Degraded;
            }
        });
    }

    pub fn mark_expired(&self, current_height: u64) {
        self.with_state(|state| {
            state.snapshot.randomness_status = RandomnessStatus::Expired;
        });
        crate::metrics::record_vrf_randomness_expired();
        tracing::error!(
            current_height,
            "VRF material expired before DKG activation; validator progress must stop"
        );
    }

    pub fn ensure_block_allowed(&self, block_height: u64) -> Result<()> {
        let snapshot = self.snapshot();
        if snapshot.randomness_status == RandomnessStatus::Expired
            || block_height > snapshot.vrf_expiry_height
        {
            return Err(VrfSafetyError::MaterialExpired {
                block_height,
                expiry_height: snapshot.vrf_expiry_height,
            }
            .into());
        }
        Ok(())
    }

    /// Restart-deterministic verify check. Sticky `Expired` does not reject.
    pub fn ensure_verify_height_allowed(&self, block_height: u64) -> Result<()> {
        let expiry_height = self.snapshot().vrf_expiry_height;
        if block_height > expiry_height {
            return Err(VrfSafetyError::MaterialExpired {
                block_height,
                expiry_height,
            }
            .into());
        }
        Ok(())
    }

    fn update_status(
        &self,
        status: RandomnessStatus,
        planned_activation_height: u64,
        activation_grace_blocks: u64,
    ) {
        let expiry_height = planned_activation_height.saturating_add(activation_grace_blocks);
        self.with_state(|state| {
            if state.snapshot.randomness_status == RandomnessStatus::Expired {
                return;
            }
            state.snapshot.randomness_status = forward_status(state, status);
            state.snapshot.next_planned_activation_height = planned_activation_height;
            state.snapshot.vrf_expiry_height = expiry_height;
        });
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut VrfSafetyState) -> T) -> T {
        let mut state = match self.inner.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                tracing::error!("VrfSafetyGate mutex poisoned, recovering");
                poisoned.into_inner()
            }
        };
        f(&mut state)
    }
}

fn effective_snapshot(state: &VrfSafetyState) -> VrfSafetySnapshot {
    let mut snapshot = state.snapshot.clone();
    if state.active_material_revealed && snapshot.randomness_status != RandomnessStatus::Expired {
        snapshot.randomness_status = RandomnessStatus::Degraded;
    }
    snapshot
}

/// Keep `Degraded` while revealed material is active. `Expired` is handled by
/// the caller and is not passed here.
fn forward_status(state: &VrfSafetyState, status: RandomnessStatus) -> RandomnessStatus {
    if state.active_material_revealed {
        RandomnessStatus::Degraded
    } else {
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_rejects_blocks_after_expiry() {
        let gate = VrfSafetyGate::new(0, 0, 10, 2);
        assert!(gate.ensure_block_allowed(12).is_ok());
        assert!(gate.ensure_block_allowed(13).is_err());
    }

    #[test]
    fn activation_resets_expiry() {
        let gate = VrfSafetyGate::new(0, 0, 10, 2);
        gate.note_activated(1, 11, 21, 2);
        let snapshot = gate.snapshot();
        assert_eq!(snapshot.randomness_status, RandomnessStatus::Healthy);
        assert_eq!(snapshot.vrf_material_version, 1);
        assert_eq!(snapshot.last_dkg_activation_height, 11);
        assert_eq!(snapshot.next_planned_activation_height, 21);
        assert_eq!(snapshot.vrf_expiry_height, 23);
    }

    #[test]
    fn expired_status_is_sticky_across_forward_notes() {
        let gate = VrfSafetyGate::new(0, 0, 10, 2);
        gate.mark_expired(13);

        gate.note_preparing(1, 14, 20, 3);
        assert_eq!(gate.snapshot().randomness_status, RandomnessStatus::Expired);

        gate.note_pending_activation(20, 3);
        assert_eq!(gate.snapshot().randomness_status, RandomnessStatus::Expired);

        gate.note_grace(20, 3);
        assert_eq!(gate.snapshot().randomness_status, RandomnessStatus::Expired);

        gate.note_activated(1, 20, 30, 3);
        let snapshot = gate.snapshot();
        assert_eq!(snapshot.randomness_status, RandomnessStatus::Expired);
        assert_eq!(snapshot.vrf_material_version, 0);
        assert_eq!(snapshot.last_dkg_activation_height, 0);
        assert_eq!(snapshot.next_planned_activation_height, 10);
        assert_eq!(snapshot.vrf_expiry_height, 12);
    }

    #[test]
    fn vrf_gate_verify_verdict_survives_restart() {
        // `new(version, last_activation, next_planned, grace)`: expiry = 10 + 2.
        let node_a = VrfSafetyGate::new(0, 0, 10, 2);
        node_a.note_preparing(1, 8, 10, 2);
        node_a.mark_expired(12);
        let node_b = VrfSafetyGate::new(0, 0, 10, 2);

        let expiry = node_a.snapshot().vrf_expiry_height;
        assert_eq!(expiry, node_b.snapshot().vrf_expiry_height);
        assert_eq!(expiry, 12);

        for height in [0, expiry] {
            assert!(node_a.ensure_verify_height_allowed(height).is_ok());
            assert!(node_b.ensure_verify_height_allowed(height).is_ok());
        }
        assert_material_expired(
            node_a.ensure_verify_height_allowed(expiry + 1),
            expiry + 1,
            expiry,
        );
        assert_material_expired(
            node_b.ensure_verify_height_allowed(expiry + 1),
            expiry + 1,
            expiry,
        );

        assert!(node_a.ensure_block_allowed(expiry).is_err());
        assert!(node_b.ensure_block_allowed(expiry).is_ok());
        assert!(node_b.ensure_block_allowed(expiry + 1).is_err());
    }

    fn assert_material_expired(result: Result<()>, block_height: u64, expiry_height: u64) {
        let error = result.expect_err("height above VRF expiry");
        let expired = error
            .downcast_ref::<VrfSafetyError>()
            .expect("VrfSafetyError");
        assert!(matches!(
            expired,
            VrfSafetyError::MaterialExpired {
                block_height: height,
                expiry_height: expiry,
            } if *height == block_height && *expiry == expiry_height
        ));
    }
}
