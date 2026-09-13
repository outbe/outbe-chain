//! Restart steps used by `features/validator_lifecycle.feature`. An ACTIVE validator's DKG share lives on
//! disk (keys-dir), not the enclave. Killing and restarting ONLY the node (the
//! enclave container stays up) must resume signing from the persisted share
//! WITHOUT a fresh DKG ceremony.

use cucumber::given;

use crate::features::common::boot_localnet;

use crate::world::World;

/// Put the freeze boundary inside the bounded restart scenario while leaving
/// enough activation grace for a real-SGX ceremony to recover.
#[given("a fresh localnet with a restartable DKG window")]
fn restartable_dkg_setup(world: &mut World) {
    boot_localnet(
        world,
        6,
        &[
            ("TESTNET_EPOCH_LENGTH_BLOCKS", "120".to_string()),
            ("TESTNET_DKG_PREPARE_WINDOW_BLOCKS", "60".to_string()),
            ("TESTNET_DKG_ACTIVATION_GRACE_BLOCKS", "120".to_string()),
        ],
    );
}

#[cfg(test)]
mod restart_observation_tests;

mod observations;
use observations::{
    restart_assert_live, restart_capture_incarnations, restart_check_logs, restart_finalize_hash,
    restart_fresh_checkpoint, restart_pin_before, restart_ports, restart_public_state,
    restart_remaining_tries, restart_replacement_matches, restart_require_membership,
    restart_sealed_node_evidence, restart_snapshot,
};

#[cfg(test)]
use observations::{
    restart_require_markers, restart_same_registered_identity, restart_validate_membership,
    RestartPublicState,
};

mod frozen_round;
use frozen_round::{
    restart_inflight_round, restart_install_replacement, restart_joiner_pair,
    restart_retain_frozen_target, restart_validate_frozen_target, RestartFrozenRound,
    RestartFrozenTarget,
};

#[cfg(test)]
use frozen_round::restart_target_commitment;

mod signing;
use signing::{restart_activation, restart_prove_signing, wait_for_dkg_retry_snapshot};

#[cfg(test)]
use signing::{restart_boundary_transition, restart_signing_comparison};

mod active;

mod committee;
use committee::committee_checkpoint_json;

mod joiner;
use joiner::pending_boundary_at;

mod registered;

mod active_reshare;
