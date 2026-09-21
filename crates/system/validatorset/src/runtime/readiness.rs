use super::registered_status;
use crate::schema::ValidatorSet;
use crate::state_machine::{self, ValidatorLifecycle};
use alloy_primitives::{keccak256, Address};
use outbe_ocomp_protocol::{
    committee::{validator_identity_hash_v1, OcompKeyRegistrationV1},
    profile::poc_schema_limits,
};
use outbe_primitives::error::{PrecompileError, Result};
use std::collections::BTreeSet;

impl ValidatorSet<'_> {
    /// Stale-join guard: a PENDING joiner confirms, on-chain, that its node has
    /// caught up to head and is ready to be frozen into the next DKG reshare
    /// target. The operator sends this only after `outbe_syncStatus` shows the
    /// node at the finalized tip; until then the joiner stays PENDING and is
    /// excluded from [`Self::get_reshare_target_set`]. Caller must be the
    /// validator itself and currently PENDING.
    pub fn confirm_validator_ready(
        &mut self,
        caller: Address,
        encoded_registration: &[u8],
    ) -> Result<()> {
        let before = self.validator_state(caller)?;
        let lifecycle = match before.lifecycle().clone() {
            ValidatorLifecycle::WaitingForReadiness(waiting) => {
                ValidatorLifecycle::Joining(state_machine::confirm_ready(waiting))
            }
            ValidatorLifecycle::Joining(joining) => ValidatorLifecycle::Joining(joining),
            ValidatorLifecycle::Absent => {
                return Err(PrecompileError::Revert("validator not registered".into()));
            }
            lifecycle => {
                return Err(PrecompileError::Revert(format!(
                    "confirmValidatorReady requires PENDING status, got {}",
                    registered_status(&lifecycle)?
                )))
            }
        };
        let limits = poc_schema_limits();
        let registration = OcompKeyRegistrationV1::decode_canonical(encoded_registration, &limits)
            .map_err(|error| {
                PrecompileError::Revert(format!("invalid OCOMP registration: {error}"))
            })?;
        if registration.core.chain_id != self.storage.chain_id()? {
            return Err(PrecompileError::Revert(
                "OCOMP registration chain id mismatch".into(),
            ));
        }
        if registration.core.genesis_hash != self.storage.genesis_hash()? {
            return Err(PrecompileError::Revert(
                "OCOMP registration genesis hash mismatch".into(),
            ));
        }
        let consensus_pubkey = before.consensus_pubkey().ok_or_else(|| {
            PrecompileError::Fatal("registered validator is missing consensus pubkey".into())
        })?;
        let expected_identity = validator_identity_hash_v1(caller, consensus_pubkey)
            .map_err(|error| PrecompileError::Fatal(format!("validator identity hash: {error}")))?;
        if registration.core.validator_identity_hash != expected_identity {
            return Err(PrecompileError::Revert(
                "OCOMP registration validator identity mismatch".into(),
            ));
        }

        let existing_registration = self.ocomp_registration(caller)?;
        let key_hash = keccak256(registration.core.ocomp_public_key_sec1);
        if let Some(existing) = existing_registration.as_ref() {
            let pinned_key_hash = keccak256(existing.core.ocomp_public_key_sec1);
            if pinned_key_hash != key_hash {
                return Err(PrecompileError::Revert(
                    "OCOMP public key is immutable in key_epoch 1".into(),
                ));
            }
            let pinned_owner = self.ocomp_key_hash_to_validator.read(&pinned_key_hash)?;
            if pinned_owner != caller {
                return Err(PrecompileError::Fatal(format!(
                    "OCOMP key reservation for {caller} is inconsistent"
                )));
            }
        }
        let existing_owner = self.ocomp_key_hash_to_validator.read(&key_hash)?;
        if !existing_owner.is_zero() && existing_owner != caller {
            return Err(PrecompileError::Revert(
                "OCOMP public key already registered by another validator".into(),
            ));
        }

        let guard = self.storage.checkpoint_guard();
        self.val_ocomp_registration
            .get_bytes(&caller)
            .write(encoded_registration)?;
        self.ocomp_key_hash_to_validator.write(&key_hash, caller)?;
        let after = before.clone().with_lifecycle(lifecycle)?;
        self.persist_validator_state_delta(&before, &after)?;
        // Re-signal so consensus schedules a reshare now that a confirmed joiner
        // is eligible (the stake-time signal may already have lapsed).
        self.pending_set_change.write(true)?;
        guard.commit();
        crate::metrics::record_pending_set_change(true);
        Ok(())
    }

    /// Imports the chain-manifest OCOMP key material for the ordered genesis
    /// ACTIVE set. The manifest vector is not membership authority: it must
    /// cover the already-persisted ACTIVE ValidatorSet exactly and in order.
    ///
    /// This is purpose-built for the one-time OCOMP lifecycle activation. A
    /// byte-identical replay is accepted; partial or conflicting pre-existing
    /// state is fatal rather than repaired.
    pub fn initialize_founder_ocomp_registrations(
        &mut self,
        registrations: &[OcompKeyRegistrationV1],
    ) -> Result<()> {
        let active = self.get_active_validators()?;
        if active.is_empty() || active.len() != registrations.len() {
            return Err(PrecompileError::Fatal(format!(
                "OCOMP founder registrations must exactly cover ACTIVE ValidatorSet: {} registrations for {} validators",
                registrations.len(),
                active.len()
            )));
        }

        let chain_id = self.storage.chain_id()?;
        let genesis_hash = self.storage.genesis_hash()?;
        let limits = poc_schema_limits();
        let mut identities = BTreeSet::new();
        let mut keys = BTreeSet::new();
        let mut prepared = Vec::new();
        prepared
            .try_reserve_exact(registrations.len())
            .map_err(|_| PrecompileError::Fatal("allocate OCOMP founder import".into()))?;

        let mut exact_existing = 0usize;
        for (validator, registration) in active.iter().zip(registrations) {
            registration
                .validate_proof_of_possession(&limits)
                .map_err(|error| {
                    PrecompileError::Fatal(format!(
                        "invalid OCOMP founder proof of possession: {error}"
                    ))
                })?;
            if registration.core.chain_id != chain_id
                || registration.core.genesis_hash != genesis_hash
            {
                return Err(PrecompileError::Fatal(
                    "OCOMP founder registration chain binding mismatch".into(),
                ));
            }
            let expected_identity = validator_identity_hash_v1(
                validator.validator_address,
                &validator.consensus_pubkey,
            )
            .map_err(|error| {
                PrecompileError::Fatal(format!("derive OCOMP founder validator identity: {error}"))
            })?;
            if registration.core.validator_identity_hash != expected_identity {
                return Err(PrecompileError::Fatal(format!(
                    "OCOMP founder registration identity mismatch for {}",
                    validator.validator_address
                )));
            }
            if !identities.insert(expected_identity)
                || !keys.insert(registration.core.ocomp_public_key_sec1)
            {
                return Err(PrecompileError::Fatal(
                    "OCOMP founder registrations contain duplicate identity or key".into(),
                ));
            }

            let encoded = registration.encode_canonical(&limits).map_err(|error| {
                PrecompileError::Fatal(format!(
                    "encode canonical OCOMP founder registration: {error}"
                ))
            })?;
            let key_hash = keccak256(registration.core.ocomp_public_key_sec1);
            let stored = self.ocomp_registration(validator.validator_address)?;
            let owner = self.ocomp_key_hash_to_validator.read(&key_hash)?;
            match stored {
                Some(existing)
                    if existing == *registration && owner == validator.validator_address =>
                {
                    exact_existing += 1;
                }
                None if owner.is_zero() => {}
                _ => {
                    return Err(PrecompileError::Fatal(format!(
                        "partial or conflicting OCOMP founder state for {}",
                        validator.validator_address
                    )));
                }
            }
            prepared.push((validator.validator_address, key_hash, encoded));
        }

        if exact_existing == registrations.len() {
            return Ok(());
        }
        if exact_existing != 0 {
            return Err(PrecompileError::Fatal(
                "partial OCOMP founder registration import is fatal".into(),
            ));
        }

        let guard = self.storage.checkpoint_guard();
        for (validator, key_hash, encoded) in prepared {
            self.val_ocomp_registration
                .get_bytes(&validator)
                .write(&encoded)?;
            self.ocomp_key_hash_to_validator
                .write(&key_hash, validator)?;
        }
        guard.commit();
        Ok(())
    }
}
