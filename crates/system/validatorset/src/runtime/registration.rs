use super::{status, MAX_SELF_REGISTERED_UNSTAKED};
use crate::precompile::IValidatorSet;
use crate::schema::ValidatorSet;
use crate::state_machine::{self, P2pInfo, ValidatorLifecycle, ValidatorState};
use alloy_primitives::{Address, B256};
use outbe_primitives::consensus_p2p::{
    decode_versioned, MAX_P2P_ADDRESS_ENCODED_LEN, P2P_ADDRESS_VERSION_V1,
};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::slashing_journal::{iso8601_now, record as journal_record, JournalRecord};
use outbe_primitives::validators::{validator_registration_message, VALIDATOR_REGISTRATION_DST};
use std::num::NonZeroU64;
use tracing::info;

/// Verifies a BLS MinPk registration signature.
///
/// Uses the `blst` crate directly to verify the signature without needing
/// the full commonware cryptography stack in the EVM precompile crate.
///
/// The signed message is 61 bytes.
/// Byte 0 is the registration version.
/// Bytes 1..=8 are `chain_id` in big-endian order.
/// Bytes 9..=28 are the validator address.
/// Bytes 29..=60 are the Radicle node id.
fn verify_bls_registration_sig(
    pubkey_bytes: &[u8; 48],
    sig_bytes: &[u8; 96],
    chain_id: u64,
    validator_addr: Address,
    radicle_node_id: B256,
) -> Result<()> {
    use blst::min_pk::{PublicKey, Signature};
    use blst::BLST_ERROR;

    let pk = PublicKey::from_bytes(pubkey_bytes)
        .map_err(|_| PrecompileError::Revert("invalid BLS public key".into()))?;
    let sig = Signature::from_bytes(sig_bytes)
        .map_err(|_| PrecompileError::Revert("invalid BLS signature".into()))?;

    let message = validator_registration_message(chain_id, validator_addr, radicle_node_id);
    let result = sig.verify(true, &message, VALIDATOR_REGISTRATION_DST, &[], &pk, true);
    if result != BLST_ERROR::BLST_SUCCESS {
        return Err(PrecompileError::Revert(
            "invalid BLS registration signature".into(),
        ));
    }
    Ok(())
}

impl ValidatorSet<'_> {
    /// Stores a validator's versioned Commonware P2P address payload.
    ///
    /// The stable ABI is Outbe-owned `(version, bytes)`, not Commonware's raw
    /// codec. This function fully validates the payload before any storage write.
    pub fn set_p2p_address(
        &mut self,
        caller: Address,
        validator_addr: Address,
        version: u8,
        encoded: &[u8],
    ) -> Result<()> {
        let owner = self.config_owner.read()?;
        if caller != owner && caller != validator_addr {
            return Err(PrecompileError::Revert(
                "unauthorized: caller must be owner or validator itself".into(),
            ));
        }
        if self.address_to_index.read(&validator_addr)? == 0 {
            return Err(PrecompileError::Revert("validator not registered".into()));
        }
        if version != P2P_ADDRESS_VERSION_V1 {
            return Err(PrecompileError::Revert(format!(
                "unsupported p2p address version {version}"
            )));
        }
        if encoded.len() > MAX_P2P_ADDRESS_ENCODED_LEN {
            return Err(PrecompileError::Revert(format!(
                "p2p address payload exceeds max length {}",
                MAX_P2P_ADDRESS_ENCODED_LEN
            )));
        }
        let decoded = decode_versioned(version, encoded)
            .map_err(|err| PrecompileError::Revert(format!("invalid p2p address: {err}")))?;

        let before = self.validator_state(validator_addr)?;
        let lifecycle = state_machine::with_p2p(before.lifecycle().clone(), P2pInfo::V1(decoded))?;
        let after = before.clone().with_lifecycle(lifecycle)?;
        let guard = self.storage.checkpoint_guard();
        self.persist_validator_state_delta(&before, &after)?;
        guard.commit();
        Ok(())
    }

    /// Returns the stored versioned P2P address payload, if one is registered.
    pub fn get_p2p_address(&self, validator_addr: Address) -> Result<Option<(u8, Vec<u8>)>> {
        let state = self.validator_state(validator_addr)?;
        let Some(p2p) = state.p2p() else {
            return Err(PrecompileError::Revert("validator not registered".into()));
        };
        if matches!(p2p, P2pInfo::Unset) {
            return Ok(None);
        }
        Ok(Some(p2p.encode_stored()))
    }

    /// Registers a new validator with BLS proof-of-possession verification.
    ///
    /// When `bls_signature` is `Some`, verifies that the BLS MinPk key signed the
    /// chain-bound registration message under
    /// `OUTBE_VALIDATOR_REGISTRATION_V2`.
    /// This production API rejects `None`. Genesis is storage-seeded.
    /// Feature-gated tests use [`Self::register_validator`] explicitly.
    ///
    /// `consensus_pubkey` is a 48-byte BLS12-381 MinPk public key.
    /// `bls_signature` is an optional 96-byte BLS MinPk signature.
    pub fn register_validator_with_sig(
        &mut self,
        caller: Address,
        validator_addr: Address,
        consensus_pubkey: &[u8; 48],
        radicle_node_id: B256,
        bls_signature: Option<&[u8; 96]>,
    ) -> Result<()> {
        self.register_validator_inner(&RegistrationRequest {
            caller,
            validator: validator_addr,
            consensus_pubkey,
            radicle_node_id,
            bls_signature,
            allow_bootstrap_without_pop: false,
        })
    }

    pub(super) fn register_validator_inner(
        &mut self,
        request: &RegistrationRequest<'_>,
    ) -> Result<()> {
        let radicle_owner = self.authorize_registration(request)?;
        let target = RegistrationTarget {
            existing_state: self.validator_state(request.validator)?,
            radicle_owner,
            block_number: self.storage.block_number()?,
        };
        if let Some(existing_index) = target.existing_state.registry_index() {
            return self.reregister_inactive(request, target, existing_index);
        }
        self.register_first_time(request, target)
    }

    /// Checks every registration precondition that does not depend on the
    /// registry state of the validator. Returns the current owner of the
    /// requested Radicle NodeId.
    fn authorize_registration(&self, request: &RegistrationRequest<'_>) -> Result<Address> {
        let owner = self.config_owner.read()?;
        if *request.consensus_pubkey == [0; 48] {
            return Err(PrecompileError::Revert(
                "consensus public key must not be zero".into(),
            ));
        }

        // Authorization: owner or self-registration
        if request.caller != owner && request.caller != request.validator {
            return Err(PrecompileError::Revert(
                "unauthorized: caller must be owner or validator itself".into(),
            ));
        }
        self.ensure_not_operational_delegate(request.validator)?;
        let radicle_owner = self.available_radicle_owner(request)?;
        self.verify_proof_of_possession(request)?;
        self.ensure_self_registration_capacity(request)?;
        self.ensure_consensus_key_available(request)?;
        Ok(radicle_owner)
    }

    /// Returns the current owner of the requested Radicle NodeId, which must
    /// be non-zero and free or already owned by the validator.
    fn available_radicle_owner(&self, request: &RegistrationRequest<'_>) -> Result<Address> {
        if request.radicle_node_id.is_zero() {
            return Err(PrecompileError::Revert(
                "Radicle NodeId must not be zero".into(),
            ));
        }
        let radicle_owner = self
            .radicle_node_id_to_validator
            .read(&request.radicle_node_id)?;
        if !radicle_owner.is_zero() && radicle_owner != request.validator {
            return Err(PrecompileError::Revert(
                "Radicle NodeId already registered by another validator".into(),
            ));
        }
        Ok(radicle_owner)
    }

    /// Every runtime registration requires proof of possession. The only
    /// no-PoP path is the feature-gated bootstrap/test helper. Normal owner
    /// authority does not weaken the consensus-key invariant.
    fn verify_proof_of_possession(&self, request: &RegistrationRequest<'_>) -> Result<()> {
        if let Some(sig_bytes) = request.bls_signature {
            verify_bls_registration_sig(
                request.consensus_pubkey,
                sig_bytes,
                self.storage.chain_id()?,
                request.validator,
                request.radicle_node_id,
            )
        } else if !request.allow_bootstrap_without_pop {
            Err(PrecompileError::Revert(
                "validator registration requires BLS proof-of-possession signature".into(),
            ))
        } else {
            Ok(())
        }
    }

    /// Bounds the free, permissionless self-registration Sybil surface.
    ///
    /// The consensus P2P secondary tier admits a self-registered REGISTERED
    /// node (the TEE verifier flow). So cap how many unstaked REGISTERED
    /// validators can exist at once, far below `config_max_validators`. An
    /// attacker then cannot fill the validator set (or the consensus P2P set)
    /// with free Sybils. Owner registrations (`caller == owner`) bypass this
    /// cap. This check runs before any state mutation (including the
    /// re-registration path), so an over-cap self-registration never consumes
    /// a registration slot.
    fn ensure_self_registration_capacity(&self, request: &RegistrationRequest<'_>) -> Result<()> {
        if request.caller == request.validator
            && self.registered_count()? >= MAX_SELF_REGISTERED_UNSTAKED
        {
            return Err(PrecompileError::Revert(
                "self-registration limit reached: too many unstaked REGISTERED validators \
                 (owner may register directly)"
                    .into(),
            ));
        }
        Ok(())
    }

    /// Verifies that the BLS pubkey is not already used by another validator.
    /// Without this check, two validators could register the same BLS key,
    /// causing undefined behavior during DKG/reshare.
    fn ensure_consensus_key_available(&self, request: &RegistrationRequest<'_>) -> Result<()> {
        let pk_hash = Self::consensus_pubkey_hash(request.consensus_pubkey);
        let existing_owner = self.consensus_pubkey_hash_to_address.read(&pk_hash)?;
        if !existing_owner.is_zero() && existing_owner != request.validator {
            return Err(PrecompileError::Revert(
                "BLS consensus pubkey already registered by another validator".into(),
            ));
        }
        Ok(())
    }

    /// Re-registers an INACTIVE validator under its existing registry index.
    fn reregister_inactive(
        &mut self,
        request: &RegistrationRequest<'_>,
        target: RegistrationTarget,
        existing_index: NonZeroU64,
    ) -> Result<()> {
        let validator_addr = request.validator;
        let (after, old_pk_hash) = self.reregistered_state(request, &target)?;
        let pk_hash = Self::consensus_pubkey_hash(request.consensus_pubkey);

        let guard = self.storage.checkpoint_guard();
        self.consensus_pubkey_hash_to_address
            .write(&old_pk_hash, Address::ZERO)?;
        self.consensus_pubkey_hash_to_address
            .write(&pk_hash, validator_addr)?;
        self.persist_registry_state_delta(&target.existing_state, &after)?;
        self.pending_set_change.write(true)?;
        self.emit(IValidatorSet::ValidatorRegistered {
            validator: validator_addr,
            index: existing_index.get(),
        })?;
        guard.commit();

        publish_registration(
            validator_addr,
            existing_index.get(),
            target.block_number,
            RegistrationPath::Reregistered,
        );
        Ok(())
    }

    /// The re-registered `WaitingForStake` state of an INACTIVE validator and
    /// the reverse-lookup hash of its old consensus key.
    ///
    /// The checks run in this order: INACTIVE status, kept Radicle NodeId,
    /// consistent reverse binding, cooldown, consensus key, transition, and
    /// no open OCOMP recovery window.
    fn reregistered_state(
        &self,
        request: &RegistrationRequest<'_>,
        target: &RegistrationTarget,
    ) -> Result<(ValidatorState, B256)> {
        let validator_addr = request.validator;
        let existing_state = &target.existing_state;
        let ValidatorLifecycle::Inactive(inactive) = existing_state.lifecycle().clone() else {
            return Err(PrecompileError::Revert(
                "validator already registered".into(),
            ));
        };
        let stored_node_id = self.val_radicle_node_id.read(&validator_addr)?;
        if stored_node_id != request.radicle_node_id {
            return Err(PrecompileError::Revert(
                "inactive validator must keep its Radicle NodeId until final cleanup".into(),
            ));
        }
        if target.radicle_owner != validator_addr {
            return Err(PrecompileError::Fatal(
                "registered validator has inconsistent Radicle NodeId reverse binding".into(),
            ));
        }
        self.ensure_reregistration_cooldown(existing_state, target.block_number)?;
        let old_pubkey = existing_state.consensus_pubkey().ok_or_else(|| {
            PrecompileError::Fatal("registered validator is missing consensus pubkey".into())
        })?;
        let old_pk_hash = Self::consensus_pubkey_hash(old_pubkey);
        let lifecycle = ValidatorLifecycle::WaitingForStake(state_machine::reregister(
            inactive,
            *request.consensus_pubkey,
            target.block_number,
        )?);
        let after = existing_state.clone().with_lifecycle(lifecycle)?;
        if self.val_ocomp_recovery_deadline.read(&validator_addr)? != 0 {
            return Err(PrecompileError::Revert(
                "cannot re-register while an OCOMP recovery window is open".into(),
            ));
        }
        Ok((after, old_pk_hash))
    }

    /// Requires the configured re-registration cooldown to have passed since
    /// the last deactivation.
    fn ensure_reregistration_cooldown(
        &self,
        existing_state: &ValidatorState,
        block_number: u64,
    ) -> Result<()> {
        let cooldown = self.config_reregistration_cooldown.read()?;
        if cooldown == 0 {
            return Ok(());
        }
        let deactivated_at = existing_state
            .history()
            .ok_or_else(|| {
                PrecompileError::Fatal("registered validator is missing history".into())
            })?
            .last_deactivated_at_height();
        let ready_at = deactivated_at
            .map(|height| {
                height.checked_add(u64::from(cooldown)).ok_or_else(|| {
                    PrecompileError::Fatal("re-registration cooldown height overflow".into())
                })
            })
            .transpose()?;
        if ready_at.is_some_and(|height| block_number < height) {
            return Err(PrecompileError::Revert(
                "re-registration cooldown not expired".into(),
            ));
        }
        Ok(())
    }

    /// Registers an absent validator under the next dense registry index.
    fn register_first_time(
        &mut self,
        request: &RegistrationRequest<'_>,
        target: RegistrationTarget,
    ) -> Result<()> {
        let validator_addr = request.validator;
        let (new_index, registry_index) = self.next_registry_index()?;
        let new_index_u64 = registry_index.get();

        // Construct and persist the complete first-time registry bundle. The
        // typed decoder guarantees that absent addresses carry no stake residue.
        let registered_state = state_machine::register(
            target.existing_state.clone(),
            registry_index,
            *request.consensus_pubkey,
            target.block_number,
        )?;

        let guard = self.storage.checkpoint_guard();
        if target.radicle_owner == validator_addr {
            return Err(PrecompileError::Fatal(
                "unregistered validator retains a Radicle NodeId reservation".into(),
            ));
        }
        self.address_to_index
            .write(&validator_addr, new_index_u64)?;
        self.index_to_address
            .write(&new_index_u64, validator_addr)?;
        self.persist_registry_state_delta(&target.existing_state, &registered_state)?;

        // Pubkey reverse lookup (keyed by keccak256 of full 48-byte pubkey)
        let pk_hash = Self::consensus_pubkey_hash(request.consensus_pubkey);
        self.consensus_pubkey_hash_to_address
            .write(&pk_hash, validator_addr)?;
        self.val_radicle_node_id
            .write(&validator_addr, request.radicle_node_id)?;
        self.radicle_node_id_to_validator
            .write(&request.radicle_node_id, validator_addr)?;

        // Increment count
        self.validator_count.write(new_index)?;

        // Signal pending set change so consensus detects the new validator
        self.pending_set_change.write(true)?;
        self.emit(IValidatorSet::ValidatorRegistered {
            validator: validator_addr,
            index: new_index_u64,
        })?;
        guard.commit();

        publish_registration(
            validator_addr,
            new_index_u64,
            target.block_number,
            RegistrationPath::FirstTime,
        );
        Ok(())
    }

    /// Checks capacity and returns the next 1-based registry index, as the new
    /// validator count and as the non-zero registry index.
    fn next_registry_index(&self) -> Result<(u32, NonZeroU64)> {
        let count = self.validator_count.read()?;
        let max = self.config_max_validators.read()?;
        if max > 0 && count >= max {
            return Err(PrecompileError::Revert("max validators reached".into()));
        }
        let new_index = count
            .checked_add(1)
            .ok_or_else(|| PrecompileError::Fatal("validator count overflow".into()))?;
        let registry_index = NonZeroU64::new(new_index as u64).ok_or_else(|| {
            PrecompileError::Fatal("validator registry index must be non-zero".into())
        })?;
        Ok((new_index, registry_index))
    }
}

/// One validator registration request.
pub(super) struct RegistrationRequest<'a> {
    pub(super) caller: Address,
    pub(super) validator: Address,
    pub(super) consensus_pubkey: &'a [u8; 48],
    pub(super) radicle_node_id: B256,
    pub(super) bls_signature: Option<&'a [u8; 96]>,
    /// Only the feature-gated bootstrap/test helper may skip proof of
    /// possession.
    pub(super) allow_bootstrap_without_pop: bool,
}

/// The registry facts read once before a registration selects its path.
struct RegistrationTarget {
    existing_state: ValidatorState,
    radicle_owner: Address,
    block_number: u64,
}

/// The registry path of a committed registration.
#[derive(Clone, Copy)]
enum RegistrationPath {
    FirstTime,
    Reregistered,
}

/// Records the metrics, journal entry and log of a committed registration.
fn publish_registration(validator: Address, index: u64, block_number: u64, path: RegistrationPath) {
    crate::metrics::record_validator_status(validator, status::REGISTERED);
    crate::metrics::record_validator_register(
        validator,
        matches!(path, RegistrationPath::Reregistered),
    );
    crate::metrics::record_pending_set_change(true);
    match path {
        RegistrationPath::Reregistered => {
            journal_record(JournalRecord::ValidatorReregistered {
                wall_clock: iso8601_now(),
                block_number,
                validator: format!("{validator:?}"),
                index,
            });
            info!(
                target: "outbe::validatorset",
                event = "validator_reregistered",
                validator = %validator,
                index,
                block_number,
                "validator re-registered (was INACTIVE, lifecycle metadata reset)",
            );
        }
        RegistrationPath::FirstTime => {
            journal_record(JournalRecord::ValidatorRegistered {
                wall_clock: iso8601_now(),
                block_number,
                validator: format!("{validator:?}"),
                index,
            });
            info!(
                target: "outbe::validatorset",
                event = "validator_registered",
                validator = %validator,
                index,
                block_number,
                "validator registered (first-time)",
            );
        }
    }
}
