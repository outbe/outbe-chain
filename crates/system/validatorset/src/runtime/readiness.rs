use super::{registered_status, ValidatorRecord};
use crate::schema::ValidatorSet;
use crate::state_machine::{self, ValidatorLifecycle, ValidatorState};
use alloy_primitives::{keccak256, Address, B256};
use outbe_ocomp_protocol::{
    committee::{validator_identity_hash_v1, OcompKeyRegistrationV1},
    profile::poc_schema_limits,
    SchemaLimits,
};
use outbe_primitives::error::{PrecompileError, Result};
use std::collections::BTreeSet;

impl ValidatorSet<'_> {
    /// Stale-join guard: a PENDING joiner confirms, on-chain, that its node has
    /// caught up to head and is ready to be frozen into the next DKG reshare
    /// target. The operator sends this only after `outbe_syncStatus` shows the
    /// node at the finalized tip. Until then the joiner stays PENDING and is
    /// excluded from [`Self::get_reshare_target_set`]. Caller must be the
    /// validator itself and currently PENDING.
    pub fn confirm_validator_ready(
        &mut self,
        caller: Address,
        encoded_registration: &[u8],
    ) -> Result<()> {
        let before = self.validator_state(caller)?;
        let lifecycle = ready_lifecycle(before.lifecycle().clone())?;
        let registration = self.decode_bound_registration(caller, &before, encoded_registration)?;
        let key_hash = self.available_ocomp_key_hash(caller, &registration)?;

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

    /// Decodes an OCOMP registration and requires it to bind this chain, this
    /// genesis and the validator's consensus identity, in that order.
    fn decode_bound_registration(
        &self,
        caller: Address,
        before: &ValidatorState,
        encoded_registration: &[u8],
    ) -> Result<OcompKeyRegistrationV1> {
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
        Ok(registration)
    }

    /// The reverse-lookup hash of the registration's OCOMP key. A key pinned
    /// by an earlier registration must not change, and no other validator may
    /// own the key.
    fn available_ocomp_key_hash(
        &self,
        caller: Address,
        registration: &OcompKeyRegistrationV1,
    ) -> Result<B256> {
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
        Ok(key_hash)
    }

    /// Imports the chain-manifest OCOMP key material for the ordered genesis
    /// ACTIVE set. The manifest vector is not membership authority: it must
    /// cover the already-persisted ACTIVE ValidatorSet exactly and in order.
    ///
    /// This is purpose-built for the one-time OCOMP lifecycle activation. This
    /// function accepts a byte-identical replay. Partial or conflicting
    /// pre-existing state is fatal and is not repaired.
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

        let mut import = FounderImport {
            chain_id: self.storage.chain_id()?,
            genesis_hash: self.storage.genesis_hash()?,
            limits: poc_schema_limits(),
            identities: BTreeSet::new(),
            keys: BTreeSet::new(),
        };
        let mut prepared = Vec::new();
        prepared
            .try_reserve_exact(registrations.len())
            .map_err(|_| PrecompileError::Fatal("allocate OCOMP founder import".into()))?;

        let mut exact_existing = 0usize;
        for (validator, registration) in active.iter().zip(registrations) {
            let founder = import.prepare(validator, registration)?;
            if self.founder_state_is_exact(&founder, registration)? {
                exact_existing += 1;
            }
            prepared.push(founder);
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
        for founder in prepared {
            self.val_ocomp_registration
                .get_bytes(&founder.validator)
                .write(&founder.encoded)?;
            self.ocomp_key_hash_to_validator
                .write(&founder.key_hash, founder.validator)?;
        }
        guard.commit();
        Ok(())
    }

    /// Whether the founder's registration and key reservation already exist
    /// byte-identically. An absent founder with a free key is not exact. Any
    /// other stored state is fatal.
    fn founder_state_is_exact(
        &self,
        founder: &PreparedFounder,
        registration: &OcompKeyRegistrationV1,
    ) -> Result<bool> {
        let stored = self.ocomp_registration(founder.validator)?;
        let owner = self.ocomp_key_hash_to_validator.read(&founder.key_hash)?;
        match stored {
            Some(existing) if existing == *registration && owner == founder.validator => Ok(true),
            None if owner.is_zero() => Ok(false),
            _ => Err(PrecompileError::Fatal(format!(
                "partial or conflicting OCOMP founder state for {}",
                founder.validator
            ))),
        }
    }
}

/// The readiness-confirmed lifecycle: a PENDING joiner confirms, and a
/// confirmed joiner stays as it is.
fn ready_lifecycle(lifecycle: ValidatorLifecycle) -> Result<ValidatorLifecycle> {
    match lifecycle {
        ValidatorLifecycle::WaitingForReadiness(waiting) => Ok(ValidatorLifecycle::Joining(
            state_machine::confirm_ready(waiting),
        )),
        ValidatorLifecycle::Joining(joining) => Ok(ValidatorLifecycle::Joining(joining)),
        ValidatorLifecycle::Absent => {
            Err(PrecompileError::Revert("validator not registered".into()))
        }
        lifecycle => Err(PrecompileError::Revert(format!(
            "confirmValidatorReady requires PENDING status, got {}",
            registered_status(&lifecycle)?
        ))),
    }
}

/// The chain binding of one founder import and the identities and keys that
/// earlier founders of the same import already use.
struct FounderImport {
    chain_id: u64,
    genesis_hash: B256,
    limits: SchemaLimits,
    identities: BTreeSet<B256>,
    keys: BTreeSet<[u8; 33]>,
}

/// One founder registration checked and encoded for the import.
struct PreparedFounder {
    validator: Address,
    key_hash: B256,
    encoded: Vec<u8>,
}

impl FounderImport {
    /// Checks a founder registration against the ACTIVE record at the same
    /// position: proof of possession, chain binding, identity, then
    /// uniqueness. Returns its canonical encoding and key hash.
    fn prepare(
        &mut self,
        validator: &ValidatorRecord,
        registration: &OcompKeyRegistrationV1,
    ) -> Result<PreparedFounder> {
        registration
            .validate_proof_of_possession(&self.limits)
            .map_err(|error| {
                PrecompileError::Fatal(format!(
                    "invalid OCOMP founder proof of possession: {error}"
                ))
            })?;
        if registration.core.chain_id != self.chain_id
            || registration.core.genesis_hash != self.genesis_hash
        {
            return Err(PrecompileError::Fatal(
                "OCOMP founder registration chain binding mismatch".into(),
            ));
        }
        let expected_identity =
            validator_identity_hash_v1(validator.validator_address, &validator.consensus_pubkey)
                .map_err(|error| {
                    PrecompileError::Fatal(format!(
                        "derive OCOMP founder validator identity: {error}"
                    ))
                })?;
        if registration.core.validator_identity_hash != expected_identity {
            return Err(PrecompileError::Fatal(format!(
                "OCOMP founder registration identity mismatch for {}",
                validator.validator_address
            )));
        }
        if !self.identities.insert(expected_identity)
            || !self.keys.insert(registration.core.ocomp_public_key_sec1)
        {
            return Err(PrecompileError::Fatal(
                "OCOMP founder registrations contain duplicate identity or key".into(),
            ));
        }

        let encoded = registration
            .encode_canonical(&self.limits)
            .map_err(|error| {
                PrecompileError::Fatal(format!(
                    "encode canonical OCOMP founder registration: {error}"
                ))
            })?;
        Ok(PreparedFounder {
            validator: validator.validator_address,
            key_hash: keccak256(registration.core.ocomp_public_key_sec1),
            encoded,
        })
    }
}
