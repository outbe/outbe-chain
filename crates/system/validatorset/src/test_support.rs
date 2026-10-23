//! Shared schema assertions and test setup.
//! One path reaches ACTIVE through the production certified-boundary hook.
//! `test_activate_validator_canonically` persists ACTIVE directly.
//! That path writes no snapshot, no `pending_set_change`, and no event.
//!
//! This module is deliberately feature-gated. Production callers must use the
//! named lifecycle commands in [`crate::runtime`], while cross-crate tests use
//! semantic fixture seams without receiving raw storage-slot handles.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::addresses::STAKING_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::types::Mapping;
use outbe_primitives::storage::StorageHandle;

use crate::{
    errors::ValidatorSetError,
    hooks::{activate_boundary_atomic, BoundaryActivationInputs},
    runtime::status,
    schema::{ValidatorSet, AUTHORITATIVE_BONDED_BASE_SLOT},
    state::{
        write_committee_snapshot, CommitteeEntry, CommitteeSnapshot,
        COMMITTEE_SNAPSHOT_RETAIN_EPOCHS,
    },
    state_machine::{self, StakeProjection, ValidatorHistory, ValidatorLifecycle},
    EpochSnapshot,
};

/// Move one registered test validator through the real admission transitions,
/// stopping at confirmed PENDING so a separately-constructed boundary artifact
/// can promote it.
pub fn admit_validator_for_boundary(
    validators: &mut ValidatorSet<'_>,
    validator: Address,
) -> Result<()> {
    let record = validators
        .get_validator(validator)?
        .ok_or(ValidatorSetError::TestValidatorNotRegistered)?;
    match record.status {
        status::REGISTERED => validators.mark_pending(validator)?,
        status::PENDING | status::ACTIVE => {}
        other => {
            return Err(ValidatorSetError::TestBoundaryCannotAdmitStatus { status: other }.into())
        }
    }

    if validators.val_status.read(&validator)? == status::PENDING
        && !validators.val_join_confirmed.read(&validator)?
    {
        validators.confirm_validator_ready(validator)?;
    }
    Ok(())
}

/// Promote one registered test validator by running the same atomic boundary
/// hook used by production. Existing active validators remain in the incoming
/// set. Participant order is the canonical BLS-public-key order.
pub fn activate_validator_via_boundary(
    validators: &mut ValidatorSet<'_>,
    validator: Address,
) -> Result<B256> {
    admit_validator_for_boundary(validators, validator)?;

    let mut incoming: Vec<_> = validators
        .get_active_consensus_set()?
        .into_iter()
        .filter(|member| member.status == status::ACTIVE)
        .collect();
    if !incoming
        .iter()
        .any(|member| member.validator_address == validator)
    {
        incoming.push(
            validators
                .get_validator(validator)?
                .ok_or(ValidatorSetError::TestValidatorDisappeared)?,
        );
    }
    incoming.sort_by_key(|member| member.consensus_pubkey);
    let new_active_set: Vec<Address> = incoming
        .iter()
        .map(|member| member.validator_address)
        .collect();
    let snapshot = CommitteeSnapshot {
        committee: incoming
            .iter()
            .map(|member| CommitteeEntry {
                address: member.validator_address,
                consensus_pubkey: member.consensus_pubkey,
            })
            .collect(),
        vrf_material_version: 0,
        vrf_group_public_key_bytes: Vec::new(),
        vrf_public_polynomial_hash: B256::ZERO,
    };
    let mut active_hash_preimage = Vec::with_capacity(8 + new_active_set.len() * 20);
    active_hash_preimage.extend_from_slice(&(new_active_set.len() as u64).to_be_bytes());
    for address in &new_active_set {
        active_hash_preimage.extend_from_slice(address.as_slice());
    }
    let active_set_hash = keccak256(active_hash_preimage);
    let incoming_epoch = validators.current_epoch_u64()?;
    let freeze_height = validators.storage.block_number()?;
    let storage = validators.storage.clone();
    let (_, incoming_key) = activate_boundary_atomic(
        storage,
        &BoundaryActivationInputs {
            outgoing: None,
            incoming_epoch,
            incoming: snapshot,
            freeze_height,
            new_active_set,
            active_set_hash,
            tee_expired_target_exclusions: Vec::new(),
        },
    )?;
    Ok(incoming_key)
}

impl ValidatorSet<'_> {
    /// Admission-only form for tests whose subject is a separately encoded
    /// certified boundary artifact.
    pub fn admit_validator_for_boundary_for_test(&mut self, validator: Address) -> Result<()> {
        admit_validator_for_boundary(self, validator)
    }

    /// Inherent form used to migrate legacy workspace fixtures without giving
    /// each crate its own activation setup.
    pub fn activate_validator_via_boundary_for_test(&mut self, validator: Address) -> Result<B256> {
        activate_validator_via_boundary(self, validator)
    }

    /// Promotes a PENDING (`WaitingForReadiness`) fixture through a boundary
    /// and checks that it is ACTIVE afterwards.
    pub fn promote_pending_validator_for_test(&mut self, validator: Address) -> Result<()> {
        if !matches!(
            self.validator_lifecycle(validator)?,
            ValidatorLifecycle::WaitingForReadiness(_)
        ) {
            return Err(ValidatorSetError::FixtureNotPendingBeforePromotion { validator }.into());
        }
        self.activate_validator_via_boundary_for_test(validator)?;
        if !self.validator_lifecycle(validator)?.is_active_status() {
            return Err(ValidatorSetError::FixtureNotActiveAfterPromotion { validator }.into());
        }
        Ok(())
    }
}

impl ValidatorSet<'_> {
    /// Sets `owner` as the registry owner, then allows 100 validators.
    pub fn test_configure_registry(&mut self, owner: Address) -> Result<()> {
        self.config_owner.write(owner)?;
        self.set_config_max_validators(100)
    }

    /// Registers and activates each snapshot member that is not yet a
    /// validator, in snapshot order. Then writes `snapshot` for every retained
    /// epoch, so any test epoch resolves to this committee.
    pub fn test_seed_committee_ring(
        &mut self,
        owner: Address,
        snapshot: &CommitteeSnapshot,
    ) -> Result<()> {
        for member in &snapshot.committee {
            if !self.is_validator(member.address)? {
                self.register_validator(owner, member.address, &member.consensus_pubkey)?;
                self.activate_validator_via_boundary_for_test(member.address)?;
            }
        }
        for epoch in 0..COMMITTEE_SNAPSHOT_RETAIN_EPOCHS {
            write_committee_snapshot(self.storage.clone(), epoch, snapshot)?;
        }
        Ok(())
    }

    /// Registers a fixture through the explicit bootstrap-only no-PoP seam.
    ///
    /// The underlying registration path is unavailable in production builds.
    /// Ordinary callers must submit a valid proof of possession.
    pub fn test_register_validator_without_pop(
        &mut self,
        validator: Address,
        consensus_pubkey: &[u8; 48],
    ) -> Result<()> {
        let owner = self.config_owner.read()?;
        self.register_validator(owner, validator, consensus_pubkey)
    }

    /// Registers `validator` with `consensus_pubkey` through the no-PoP seam,
    /// sets its bonded stake projection, then activates it through a boundary.
    pub fn test_register_active_validator(
        &mut self,
        validator: Address,
        consensus_pubkey: &[u8; 48],
        bonded: U256,
    ) -> Result<()> {
        self.test_register_validator_without_pop(validator, consensus_pubkey)?;
        self.test_set_stake_projection(validator, StakeProjection::new(bonded, None))?;
        self.activate_validator_via_boundary_for_test(validator)?;
        Ok(())
    }

    /// Moves a registered fixture through the canonical typed join path.
    ///
    /// This intentionally does not expose constructors for lifecycle payloads.
    /// The real transition functions carry identity, P2P data and history
    /// forward from the current registered state.
    pub fn test_activate_validator_canonically(
        &mut self,
        address: Address,
        stake: StakeProjection,
        minimum: U256,
    ) -> Result<()> {
        if stake.bonded() < minimum {
            return Err(ValidatorSetError::FixtureActivationStakeBelowMinimum {
                bonded: stake.bonded(),
                minimum,
            }
            .into());
        }

        let before = self.validator_state(address)?;
        let with_stake = state_machine::with_stake(before.lifecycle().clone(), stake)?;
        let active = match with_stake {
            ValidatorLifecycle::WaitingForStake(waiting) => {
                let waiting = state_machine::reach_minimum(waiting, stake, minimum)?;
                state_machine::activate_at_boundary(state_machine::confirm_ready(waiting))
            }
            ValidatorLifecycle::WaitingForReadiness(waiting) => {
                state_machine::activate_at_boundary(state_machine::confirm_ready(waiting))
            }
            ValidatorLifecycle::Joining(joining) => state_machine::activate_at_boundary(joining),
            ValidatorLifecycle::Active(active) => active,
            lifecycle => {
                return Err(ValidatorSetError::FixtureActivationRequiresJoinPath {
                    status: lifecycle.stored_status(),
                }
                .into());
            }
        };
        let after = before
            .clone()
            .with_lifecycle(ValidatorLifecycle::Active(active))?;
        self.persist_validator_state_delta(&before, &after)?;
        self.test_seed_authoritative_bonded(address, stake.bonded())
    }

    /// Calls the consensus-validated boundary seam from a test without
    /// exposing it in production or falling back to the legacy owner path.
    pub fn test_activate_validated_boundary_set(
        &mut self,
        new_active_set: &[Address],
        active_set_hash: B256,
        freeze_height: u64,
    ) -> Result<()> {
        self.activate_validated_boundary_set(new_active_set, active_set_hash, freeze_height)
    }

    /// Calls the consensus-validated boundary seam with certified TEE-expiry
    /// exclusions from a test without exposing it in production.
    pub fn test_activate_validated_boundary_set_with_expiry_exclusions(
        &mut self,
        new_active_set: &[Address],
        active_set_hash: B256,
        freeze_height: u64,
        tee_expired_target_exclusions: &[Address],
    ) -> Result<()> {
        self.activate_validated_boundary_set_with_expiry_exclusions(
            new_active_set,
            active_set_hash,
            freeze_height,
            tee_expired_target_exclusions,
        )
    }

    /// Stores the bonded projection in `Staking.stake_amount`.
    /// ValidatorSet persistence does not write that balance.
    pub fn test_set_stake_projection(
        &mut self,
        address: Address,
        stake: StakeProjection,
    ) -> Result<()> {
        let before = self.validator_state(address)?;
        let lifecycle = state_machine::with_stake(before.lifecycle().clone(), stake)?;
        let after = before.clone().with_lifecycle(lifecycle)?;
        self.persist_validator_state_delta(&before, &after)?;
        self.test_seed_authoritative_bonded(address, stake.bonded())
    }

    /// Writes `Staking.stake_amount` for a fixture. Production methods must not.
    pub fn test_seed_authoritative_bonded(&self, address: Address, bonded: U256) -> Result<()> {
        Mapping::<Address, U256>::new(
            U256::from(AUTHORITATIVE_BONDED_BASE_SLOT),
            STAKING_ADDRESS,
            self.storage.clone(),
        )
        .write(&address, bonded)
    }

    /// Replaces retained historical counters/heights for a registered fixture.
    pub fn test_set_history(&mut self, address: Address, history: ValidatorHistory) -> Result<()> {
        let before = self.validator_state(address)?;
        if !before.is_registered() {
            return Err(ValidatorSetError::TestFixtureHistoryRequiresRegisteredValidator.into());
        }
        let lifecycle = state_machine::with_history(before.lifecycle().clone(), history)?;
        let after = before.clone().with_lifecycle(lifecycle)?;
        self.persist_validator_state_delta(&before, &after)
    }
}

/// Raw single-field storage writes that bypass the ValidatorSet lifecycle
/// rules. Corruption tests and fixtures that need exact epoch or set-change
/// metadata use them. A caller imports this trait to opt in, so every bypass of
/// the typed transitions is visible at the call site.
pub trait StorageOverrides {
    /// Replaces epoch metadata as one test fixture bundle.
    fn test_set_epoch_snapshot(&mut self, epoch: EpochSnapshot) -> Result<()>;

    fn test_set_pending_set_change(&mut self, pending: bool) -> Result<()>;

    fn test_set_active_consensus_set_hash(&mut self, hash: B256) -> Result<()>;

    /// Deliberately installs malformed P2P storage for a fail-closed corruption
    /// test. Normal fixtures must use `set_p2p_address`.
    fn test_corrupt_p2p_storage(
        &mut self,
        address: Address,
        version: u8,
        payload: &[u8],
    ) -> Result<()>;
}

impl StorageOverrides for ValidatorSet<'_> {
    fn test_set_epoch_snapshot(&mut self, epoch: EpochSnapshot) -> Result<()> {
        self.epoch_number.write(epoch.number)?;
        self.epoch_start_timestamp.write(epoch.start_timestamp)?;
        self.epoch_start_block.write(epoch.start_block)?;
        self.config_epoch_length_blocks.write(epoch.length_blocks)
    }

    fn test_set_pending_set_change(&mut self, pending: bool) -> Result<()> {
        self.pending_set_change.write(pending)
    }

    fn test_set_active_consensus_set_hash(&mut self, hash: B256) -> Result<()> {
        self.active_consensus_set_hash.write(hash)
    }

    fn test_corrupt_p2p_storage(
        &mut self,
        address: Address,
        version: u8,
        payload: &[u8],
    ) -> Result<()> {
        self.val_p2p_address_version.write(&address, version)?;
        self.val_p2p_address_payload
            .get_bytes(&address)
            .write(payload)
    }
}

/// Reads the lifecycle of `validator` through a new ValidatorSet view of
/// `storage`.
pub fn test_lifecycle_of(
    storage: StorageHandle<'_>,
    validator: Address,
) -> Result<ValidatorLifecycle> {
    ValidatorSet::new(storage).validator_lifecycle(validator)
}

/// Pin the committee snapshot schema independently to its storage ABI slots.
pub fn assert_committee_snapshot_schema_slots(vs: &ValidatorSet<'_>) {
    assert_eq!(vs.committee_snapshot_exists.base_slot(), U256::from(31u64));
    assert_eq!(vs.committee_snapshot_len.base_slot(), U256::from(32u64));
    assert_eq!(
        vs.committee_snapshot_address_at.base_slot(),
        U256::from(33u64)
    );
    assert_eq!(
        vs.committee_snapshot_pubkey_lo_at.base_slot(),
        U256::from(34u64)
    );
    assert_eq!(
        vs.committee_snapshot_pubkey_hi_at.base_slot(),
        U256::from(35u64)
    );
    assert_eq!(
        vs.committee_snapshot_vrf_material_version.base_slot(),
        U256::from(36u64)
    );
    assert_eq!(
        vs.committee_snapshot_vrf_group_public_key_hash.base_slot(),
        U256::from(37u64)
    );
    assert_eq!(
        vs.committee_snapshot_vrf_group_public_key_len.base_slot(),
        U256::from(38u64)
    );
    assert_eq!(
        vs.committee_snapshot_vrf_group_public_key_chunk_at
            .base_slot(),
        U256::from(39u64)
    );
    assert_eq!(
        vs._reserved_committee_snapshot_slot_40.slot(),
        U256::from(40u64),
    );
}
