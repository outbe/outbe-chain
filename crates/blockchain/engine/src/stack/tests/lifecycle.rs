//! `UpstreamBootstrap -> Verifier -> Signer` lifecycle seams.
use super::*;
use crate::stack::epoch::marshal_recovery::validator_marshal_epocher;
use crate::stack::epoch::supervisor::future_epoch_hint_height;
use crate::stack::follower::{bootstrap_ready_from_facts, BootstrapFacts};
use commonware_consensus::types::Epocher as _;

const OFFER: B256 = B256::repeat_byte(0x0f);

fn ready_facts() -> BootstrapFacts {
    BootstrapFacts {
        finalized_height: 120,
        local_key_in_validator_set: true,
        finalized_boundary_present: true,
        on_chain_offer: OFFER,
        resident_offer: Some(OFFER),
        tee_lease_armed: true,
    }
}

fn rotation(length: u64, grace: u64) -> DkgRotationParams {
    DkgRotationParams {
        epoch_length_blocks: length,
        prepare_window_blocks: 10,
        activation_grace_blocks: grace,
    }
}

#[test]
fn bootstrap_ready_requires_every_fact_above_genesis() {
    assert!(bootstrap_ready_from_facts(ready_facts()));
    for broken in [
        BootstrapFacts {
            local_key_in_validator_set: false,
            ..ready_facts()
        },
        BootstrapFacts {
            finalized_boundary_present: false,
            ..ready_facts()
        },
        BootstrapFacts {
            on_chain_offer: B256::ZERO,
            resident_offer: Some(B256::ZERO),
            ..ready_facts()
        },
        BootstrapFacts {
            resident_offer: None,
            ..ready_facts()
        },
        BootstrapFacts {
            resident_offer: Some(B256::repeat_byte(0x01)),
            ..ready_facts()
        },
        BootstrapFacts {
            tee_lease_armed: false,
            ..ready_facts()
        },
    ] {
        assert!(!bootstrap_ready_from_facts(broken), "{broken:?}");
    }
}

#[test]
fn bootstrap_ready_at_genesis_requires_only_genesis_membership() {
    let genesis = BootstrapFacts {
        finalized_height: 0,
        local_key_in_validator_set: true,
        finalized_boundary_present: false,
        on_chain_offer: B256::ZERO,
        resident_offer: None,
        tee_lease_armed: false,
    };
    assert!(bootstrap_ready_from_facts(genesis));
    assert!(!bootstrap_ready_from_facts(BootstrapFacts {
        local_key_in_validator_set: false,
        ..genesis
    }));
}

#[test]
fn validator_marshal_epocher_recovers_from_the_latest_db_boundary() {
    // Epoch 3 activated at carrier 181 (L = 60, no grace delay).
    let epocher = validator_marshal_epocher(Some((181, 3)), rotation(60, 5));
    let epoch_of = |height: u64| {
        epocher
            .containing(Height::new(height))
            .map(|info| info.epoch().get())
    };
    assert_eq!(
        epocher.activation_height(Epoch::new(3)),
        Some(Height::new(181))
    );
    assert_eq!(epoch_of(181), Some(3));
    assert_eq!(epoch_of(246), Some(3), "open epoch extends to L + grace");
    // Heights below the anchor stay unknown (recorded decision).
    assert_eq!(epoch_of(180), None);

    // A grace-delayed boundary observed on activation closes epoch 3 exactly.
    epocher
        .observe_boundary(Epoch::new(4), Height::new(243))
        .unwrap();
    assert_eq!(epoch_of(242), Some(3));
    assert_eq!(epoch_of(243), Some(4));
    // The finalization path and the supervisor may both record it.
    epocher
        .observe_boundary(Epoch::new(4), Height::new(243))
        .unwrap();
    assert!(epocher
        .observe_boundary(Epoch::new(4), Height::new(244))
        .is_err());
}

#[test]
fn validator_marshal_epocher_starts_at_genesis_for_epoch_zero() {
    for boundary in [None, Some((1, 0))] {
        let epocher = validator_marshal_epocher(boundary, rotation(60, 0));
        assert_eq!(
            epocher.containing(Height::new(60)).unwrap().epoch(),
            Epoch::new(0),
            "block 60 is the last block of epoch 0"
        );
        epocher
            .observe_boundary(Epoch::new(1), Height::new(61))
            .unwrap();
        assert_eq!(
            epocher.containing(Height::new(61)).unwrap().epoch(),
            Epoch::new(1)
        );
    }
}

#[test]
fn future_epoch_vote_hints_the_earliest_possible_boundary() {
    let epocher = validator_marshal_epocher(Some((181, 3)), rotation(60, 5));
    assert_eq!(
        future_epoch_hint_height(&epocher, Epoch::new(3), Epoch::new(4)),
        Some(Height::new(241))
    );
    assert_eq!(
        future_epoch_hint_height(&epocher, Epoch::new(3), Epoch::new(9)),
        Some(Height::new(241))
    );
    for stale in [Epoch::new(2), Epoch::new(3)] {
        assert_eq!(
            future_epoch_hint_height(&epocher, Epoch::new(3), stale),
            None
        );
    }
    // The epocher does not know an epoch below its anchor.
    assert_eq!(
        future_epoch_hint_height(&epocher, Epoch::new(2), Epoch::new(3)),
        None
    );
}
