use super::status;
use crate::errors::ValidatorSetError;
use crate::precompile::IValidatorSet;
use crate::schema::ValidatorSet;
use crate::state_machine::{self, P2pInfo, StakeProjection, ValidatorLifecycle, ValidatorState};
use alloy_primitives::{Address, B256, U256};
use outbe_primitives::consensus_p2p::{
    decode_versioned, MAX_P2P_ADDRESS_ENCODED_LEN, P2P_ADDRESS_VERSION_V1,
};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::slashing_journal::{record as journal_record, JournalRecord};
use outbe_primitives::time::iso8601_now;
use outbe_primitives::validators::{validator_registration_message, VALIDATOR_REGISTRATION_DST};
use std::num::NonZeroU64;
use tracing::info;

/// The 48-byte consensus key and the 96-byte BLS proof of possession of a
/// registration call. The parser reads the 48-byte key length first.
pub fn decode_registration_key_and_proof<'a>(
    consensus_pubkey: &[u8],
    signature: &'a [u8],
) -> Result<([u8; 48], &'a [u8; 96])> {
    if consensus_pubkey.len() != 48 {
        return Err(ValidatorSetError::ConsensusPubkeyWrongLength.into());
    }
    let pubkey: [u8; 48] = consensus_pubkey[..48]
        .try_into()
        .map_err(|_| PrecompileError::from(ValidatorSetError::ConsensusPubkeyConversionFailed))?;
    if signature.len() != 96 {
        return Err(ValidatorSetError::BlsPopWrongLength.into());
    }
    let sig: &[u8; 96] = signature[..96]
        .try_into()
        .map_err(|_| PrecompileError::from(ValidatorSetError::BlsSignatureConversionFailed))?;
    Ok((pubkey, sig))
}

fn decode_registration_p2p(version: u8, encoded: &[u8]) -> Result<P2pInfo> {
    if encoded.is_empty() {
        return Ok(P2pInfo::Unset);
    }
    if version != P2P_ADDRESS_VERSION_V1 {
        return Err(ValidatorSetError::UnsupportedP2pAddressVersion { version }.into());
    }
    if encoded.len() > MAX_P2P_ADDRESS_ENCODED_LEN {
        return Err(ValidatorSetError::P2pAddressPayloadTooLong {
            max_len: MAX_P2P_ADDRESS_ENCODED_LEN,
        }
        .into());
    }
    let decoded =
        decode_versioned(version, encoded).map_err(ValidatorSetError::invalid_p2p_address)?;
    Ok(P2pInfo::V1(decoded))
}

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

    let pk =
        PublicKey::from_bytes(pubkey_bytes).map_err(|_| ValidatorSetError::InvalidBlsPublicKey)?;
    let sig =
        Signature::from_bytes(sig_bytes).map_err(|_| ValidatorSetError::InvalidBlsSignature)?;

    let message = validator_registration_message(chain_id, validator_addr, radicle_node_id);
    let result = sig.verify(true, &message, VALIDATOR_REGISTRATION_DST, &[], &pk, true);
    if result != BLST_ERROR::BLST_SUCCESS {
        return Err(ValidatorSetError::InvalidBlsRegistrationSignature.into());
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
            return Err(ValidatorSetError::UnauthorizedOwnerOrValidator.into());
        }
        if self.address_to_index.read(&validator_addr)? == 0 {
            return Err(ValidatorSetError::ValidatorNotRegistered.into());
        }
        if version != P2P_ADDRESS_VERSION_V1 {
            return Err(ValidatorSetError::UnsupportedP2pAddressVersion { version }.into());
        }
        if encoded.len() > MAX_P2P_ADDRESS_ENCODED_LEN {
            return Err(ValidatorSetError::P2pAddressPayloadTooLong {
                max_len: MAX_P2P_ADDRESS_ENCODED_LEN,
            }
            .into());
        }
        let decoded =
            decode_versioned(version, encoded).map_err(ValidatorSetError::invalid_p2p_address)?;

        let before = self.validator_state(validator_addr)?;
        let lifecycle = state_machine::with_p2p(before.lifecycle().clone(), P2pInfo::V1(decoded))?;
        let after = before.clone().with_lifecycle(lifecycle)?;
        let guard = self.storage.checkpoint_guard()?;
        self.persist_validator_state_delta(&before, &after)?;
        guard.commit()?;
        Ok(())
    }

    /// Returns the stored versioned P2P address payload, if one is registered.
    pub fn get_p2p_address(&self, validator_addr: Address) -> Result<Option<(u8, Vec<u8>)>> {
        let state = self.validator_state(validator_addr)?;
        let Some(p2p) = state.p2p() else {
            return Err(ValidatorSetError::ValidatorNotRegistered.into());
        };
        if matches!(p2p, P2pInfo::Unset) {
            return Ok(None);
        }
        Ok(Some(p2p.encode_stored()))
    }

    /// Registers a new validator (or re-registers an `INACTIVE` validator) with
    /// initial stake at or above `minimum` and transitions directly to
    /// `WaitingForReadiness` (`PENDING`).
    pub fn register_with_initial_stake(
        &mut self,
        caller: Address,
        validator_addr: Address,
        (consensus_pubkey, radicle_node_id, bls_signature): (&[u8; 48], B256, Option<&[u8; 96]>),
        (p2p_version, p2p_encoded): (u8, &[u8]),
        (bonded, minimum): (U256, U256),
    ) -> Result<()> {
        let p2p = decode_registration_p2p(p2p_version, p2p_encoded)?;
        self.register_validator_inner(&RegistrationRequest {
            caller,
            validator: validator_addr,
            consensus_pubkey,
            radicle_node_id,
            bls_signature,
            p2p,
            bonded,
            minimum,
            allow_bootstrap_without_pop: false,
        })
    }

    /// Registers a validator with BLS proof-of-possession verification in
    /// unit tests (without requiring pre-existing Staking bond on an Absent
    /// record).
    #[cfg(test)]
    pub fn register_validator_with_sig(
        &mut self,
        caller: Address,
        validator_addr: Address,
        consensus_pubkey: &[u8; 48],
        radicle_node_id: B256,
        bls_signature: Option<&[u8; 96]>,
    ) -> Result<()> {
        let bonded = self.read_authoritative_bonded(&validator_addr)?;
        self.register_validator_inner(&RegistrationRequest {
            caller,
            validator: validator_addr,
            consensus_pubkey,
            radicle_node_id,
            bls_signature,
            p2p: P2pInfo::Unset,
            bonded,
            minimum: U256::ZERO,
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
            return Err(ValidatorSetError::ZeroConsensusPublicKey.into());
        }

        // Only the owner or the validator may register at the ValidatorSet
        // boundary. External callers enter through Staking::stake.
        if request.caller != owner && request.caller != request.validator {
            return Err(ValidatorSetError::UnauthorizedOwnerOrValidator.into());
        }
        self.ensure_not_operational_delegate(request.validator)?;
        let radicle_owner = self.available_radicle_owner(request)?;
        self.verify_proof_of_possession(request)?;
        self.ensure_consensus_key_available(request)?;
        Ok(radicle_owner)
    }

    /// Returns the current owner of the requested Radicle NodeId, which must
    /// be non-zero and free or already owned by the validator.
    fn available_radicle_owner(&self, request: &RegistrationRequest<'_>) -> Result<Address> {
        if request.radicle_node_id.is_zero() {
            return Err(ValidatorSetError::ZeroRadicleNodeId.into());
        }
        let radicle_owner = self
            .radicle_node_id_to_validator
            .read(&request.radicle_node_id)?;
        if !radicle_owner.is_zero() && radicle_owner != request.validator {
            return Err(ValidatorSetError::RadicleNodeIdAlreadyRegistered.into());
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
            Err(ValidatorSetError::RegistrationRequiresBlsPop.into())
        } else {
            Ok(())
        }
    }

    /// Verifies that the BLS pubkey is not already used by another validator.
    /// Without this check, two validators could register the same BLS key,
    /// causing undefined behavior during DKG/reshare.
    fn ensure_consensus_key_available(&self, request: &RegistrationRequest<'_>) -> Result<()> {
        let pk_hash = Self::consensus_pubkey_hash(request.consensus_pubkey);
        let existing_owner = self.consensus_pubkey_hash_to_address.read(&pk_hash)?;
        if !existing_owner.is_zero() && existing_owner != request.validator {
            return Err(ValidatorSetError::ConsensusPubkeyAlreadyRegistered.into());
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

        let guard = self.storage.checkpoint_guard()?;
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
        self.emit(IValidatorSet::ValidatorPending {
            validator: validator_addr,
        })?;
        guard.commit()?;

        publish_registration(
            validator_addr,
            existing_index.get(),
            target.block_number,
            RegistrationPath::Reregistered,
        );
        Ok(())
    }

    /// The re-registered `WaitingForReadiness` state of an INACTIVE validator
    /// and the reverse-lookup hash of its old consensus key.
    fn reregistered_state(
        &self,
        request: &RegistrationRequest<'_>,
        target: &RegistrationTarget,
    ) -> Result<(ValidatorState, B256)> {
        let validator_addr = request.validator;
        let existing_state = &target.existing_state;
        let ValidatorLifecycle::Inactive(inactive) = existing_state.lifecycle().clone() else {
            return Err(ValidatorSetError::ValidatorAlreadyRegistered.into());
        };
        let stored_node_id = self.val_radicle_node_id.read(&validator_addr)?;
        if stored_node_id != request.radicle_node_id {
            return Err(ValidatorSetError::InactiveValidatorMustKeepRadicleNodeId.into());
        }
        if target.radicle_owner != validator_addr {
            return Err(ValidatorSetError::InconsistentRadicleNodeIdReverseBinding.into());
        }
        self.ensure_reregistration_cooldown(existing_state, target.block_number)?;
        let old_pubkey = existing_state
            .consensus_pubkey()
            .ok_or(ValidatorSetError::RegisteredValidatorMissingConsensusPubkey)?;
        let old_pk_hash = Self::consensus_pubkey_hash(old_pubkey);
        let stake = StakeProjection::new(request.bonded, None);
        let lifecycle = ValidatorLifecycle::WaitingForReadiness(state_machine::reregister(
            inactive,
            (*request.consensus_pubkey, request.p2p.clone()),
            (stake, request.minimum),
            target.block_number,
        )?);
        let after = existing_state.clone().with_lifecycle(lifecycle)?;
        Ok((after, old_pk_hash))
    }

    /// Rejects a validator that exited in the current epoch
    /// (`deactivated_at > epoch_start_block`) or before the re-registration
    /// cooldown expires.
    fn ensure_reregistration_cooldown(
        &self,
        existing_state: &ValidatorState,
        block_number: u64,
    ) -> Result<()> {
        let deactivated_at = existing_state
            .history()
            .ok_or(ValidatorSetError::RegisteredValidatorMissingHistory)?
            .last_deactivated_at_height();
        let epoch_start = self.epoch_start_block.read()?;
        if deactivated_at.is_some_and(|height| height > epoch_start) {
            return Err(ValidatorSetError::ReregistrationCooldownNotExpired.into());
        }
        let cooldown = self.config_reregistration_cooldown.read()?;
        if cooldown == 0 {
            return Ok(());
        }
        let ready_at = deactivated_at
            .map(|height| {
                height
                    .checked_add(u64::from(cooldown))
                    .ok_or(ValidatorSetError::ReregistrationCooldownHeightOverflow)
            })
            .transpose()?;
        if ready_at.is_some_and(|height| block_number < height) {
            return Err(ValidatorSetError::ReregistrationCooldownNotExpired.into());
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

        // Construct and persist the complete first-time registry bundle.
        let stake = StakeProjection::new(request.bonded, None);
        let registered_state = state_machine::register(
            target.existing_state.clone(),
            registry_index,
            (*request.consensus_pubkey, request.p2p.clone()),
            (stake, request.minimum),
            target.block_number,
        )?;

        let guard = self.storage.checkpoint_guard()?;
        if target.radicle_owner == validator_addr {
            return Err(ValidatorSetError::UnregisteredValidatorRetainsRadicleReservation.into());
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
        self.emit(IValidatorSet::ValidatorPending {
            validator: validator_addr,
        })?;
        guard.commit()?;

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
            return Err(ValidatorSetError::MaxValidatorsReached.into());
        }
        let new_index = count
            .checked_add(1)
            .ok_or(ValidatorSetError::ValidatorCountOverflow)?;
        let registry_index = NonZeroU64::new(new_index as u64)
            .ok_or(ValidatorSetError::ValidatorRegistryIndexZero)?;
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
    pub(super) p2p: P2pInfo,
    pub(super) bonded: U256,
    pub(super) minimum: U256,
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
    crate::metrics::record_validator_status(validator, status::PENDING);
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
