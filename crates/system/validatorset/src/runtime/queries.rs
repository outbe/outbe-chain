//! Read projections of the validator registry: ABI records, filtered views,
//! counts, epoch metadata and reverse lookups.

use super::{status, EpochSnapshot, ValidatorParticipation, ValidatorRecord};
use crate::schema::ValidatorSet;
use crate::state_machine::ValidatorLifecycle;
use alloy_primitives::{Address, B256};
use outbe_primitives::error::{PrecompileError, Result};

/// Registry membership projections: ABI records, lifecycle-filtered views
/// and their counts.
impl ValidatorSet<'_> {
    fn read_validator_record(&self, addr: Address) -> Result<ValidatorRecord> {
        let state = self.validator_state(addr)?;
        let stored_status = state.stored_status().ok_or_else(|| {
            PrecompileError::Fatal("cannot project absent validator into ValidatorRecord".into())
        })?;
        let history = state.history().ok_or_else(|| {
            PrecompileError::Fatal("registered validator is missing history".into())
        })?;
        let consensus_pubkey = state.consensus_pubkey().copied().ok_or_else(|| {
            PrecompileError::Fatal("registered validator is missing consensus public key".into())
        })?;
        Ok(ValidatorRecord {
            validator_address: addr,
            consensus_pubkey,
            stake: state.bonded_stake(),
            status: stored_status,
            slash_count: history.slash_count(),
            missed_blocks: history.missed_blocks(),
            missed_votes: history.missed_votes(),
            blocks_proposed: history.blocks_proposed(),
            joined_at_height: history.joined_at_height(),
            deactivated_at_height: history.last_deactivated_at_height().unwrap_or(0),
            unbonding_end: state.unbonding_end_hint().unwrap_or(0),
            has_bls_share: state.has_bls_share(),
        })
    }

    /// Returns the full ABI-compatible record for a validator address, or
    /// `None` if it has no registry identity. Unknown status bytes fail closed.
    pub fn get_validator(&self, addr: Address) -> Result<Option<ValidatorRecord>> {
        if self.address_to_index.read(&addr)? == 0 {
            return Ok(None);
        }
        Ok(Some(self.read_validator_record(addr)?))
    }

    /// Returns all registered validators, including inactive and exiting ones.
    pub fn get_all_validators(&self) -> Result<Vec<ValidatorRecord>> {
        let addresses = self.registered_validator_addresses()?;
        let mut result = Vec::with_capacity(addresses.len());
        for addr in addresses {
            result.push(self.read_validator_record(addr)?);
        }
        Ok(result)
    }

    fn validator_addresses_matching(
        &self,
        predicate: impl Fn(&ValidatorLifecycle) -> bool,
    ) -> Result<Vec<Address>> {
        let mut result = Vec::new();
        for addr in self.registered_validator_addresses()? {
            if predicate(&self.validator_lifecycle(addr)?) {
                result.push(addr);
            }
        }
        Ok(result)
    }

    fn get_validators_matching(
        &self,
        predicate: impl Fn(&ValidatorLifecycle) -> bool,
    ) -> Result<Vec<ValidatorRecord>> {
        self.validator_addresses_matching(predicate)?
            .into_iter()
            .map(|addr| self.read_validator_record(addr))
            .collect()
    }

    /// Returns only validators with `status == ACTIVE`.
    pub fn get_active_validators(&self) -> Result<Vec<ValidatorRecord>> {
        self.get_validators_matching(ValidatorLifecycle::is_active_status)
    }

    /// Returns validators eligible for the NEXT consensus committee: `Active`
    /// plus readiness-confirmed `Joining`. This set excludes `WaitingForReadiness`,
    /// `Exiting`, and both jailed phases. Boundary activation grants each included
    /// joiner a share while changing it to `Active` atomically.
    pub fn get_reshare_target_set(&self) -> Result<Vec<ValidatorRecord>> {
        let all = self.get_validators_matching(ValidatorLifecycle::is_reshare_target)?;
        let mut target = Vec::new();
        for v in all {
            if v.status == status::ACTIVE
                || (v.status == status::PENDING
                    && self.ocomp_registration(v.validator_address)?.is_some())
            {
                target.push(v);
            }
        }
        Ok(target)
    }

    /// Returns validators with `status == PENDING`. These are staked joiners
    /// admitted to the validator set but not yet granted a threshold share. Used to
    /// admit them to consensus P2P as SECONDARY peers, so they can sync to head
    /// before the reshare that makes them signers. They are NOT consensus
    /// participants (no share).
    pub fn get_pending_validators(&self) -> Result<Vec<ValidatorRecord>> {
        self.get_validators_matching(ValidatorLifecycle::is_pending)
    }

    /// Returns validators admitted to consensus P2P as secondary peers:
    /// `WaitingForStake`, both pending phases, and both jailed phases. This view
    /// controls network admission only. A live share independently gates the
    /// current-participant predicate. Downstream code drops peers without P2P
    /// information.
    pub fn get_admitted_non_consensus_validators(&self) -> Result<Vec<ValidatorRecord>> {
        self.get_validators_matching(ValidatorLifecycle::is_secondary_admission)
    }

    /// Returns validators in the current consensus set.
    ///
    /// `Exiting` and `JailRetained` validators retain consensus accountability
    /// until a successful boundary excludes them and clears their BLS share.
    pub fn get_active_consensus_set(&self) -> Result<Vec<ValidatorRecord>> {
        self.get_validators_matching(ValidatorLifecycle::is_current_consensus_participant)
    }

    /// Returns the number of active validators.
    pub fn active_validator_count(&self) -> Result<u32> {
        let count: u32 = self
            .validator_addresses_matching(ValidatorLifecycle::is_active_status)?
            .len()
            .try_into()
            .map_err(|_| PrecompileError::Revert("active validator count exceeds u32".into()))?;
        Ok(count)
    }

    /// number of validators currently in the `REGISTERED` (self-registered,
    /// not-yet-staked) state. Used to bound the free, permissionless
    /// self-registration Sybil surface. See [`MAX_SELF_REGISTERED_UNSTAKED`].
    pub fn registered_count(&self) -> Result<u32> {
        let count: u32 = self
            .validator_addresses_matching(ValidatorLifecycle::is_registered_status)?
            .len()
            .try_into()
            .map_err(|_| {
                PrecompileError::Revert("registered validator count exceeds u32".into())
            })?;
        Ok(count)
    }

    /// Returns the number of validators in the active consensus set.
    pub fn active_consensus_count(&self) -> Result<u32> {
        let count: u32 = self
            .validator_addresses_matching(ValidatorLifecycle::is_current_consensus_participant)?
            .len()
            .try_into()
            .map_err(|_| PrecompileError::Revert("active consensus count exceeds u32".into()))?;
        Ok(count)
    }

    /// Returns true if the validator is a current consensus participant.
    pub fn is_consensus_participant(&self, addr: Address) -> Result<bool> {
        Ok(self
            .validator_lifecycle(addr)?
            .is_current_consensus_participant())
    }

    /// Returns the number of entries in the dense validator registry.
    pub fn validator_count(&self) -> Result<u32> {
        self.validator_count.read()
    }

    /// Returns registered addresses in dense registry-index order.
    pub fn registered_validator_addresses(&self) -> Result<Vec<Address>> {
        let count = self.validator_count.read()?;
        let mut addresses = Vec::with_capacity(count as usize);
        for index in 1..=u64::from(count) {
            let address = self.validator_address_at(index)?.ok_or_else(|| {
                PrecompileError::Fatal(format!(
                    "validator registry index {index} is empty below validator_count {count}"
                ))
            })?;
            addresses.push(address);
        }
        Ok(addresses)
    }

    /// Resolves a one-based dense registry index and checks its reverse mapping.
    pub fn validator_address_at(&self, index: u64) -> Result<Option<Address>> {
        let count = u64::from(self.validator_count.read()?);
        if index == 0 || index > count {
            return Ok(None);
        }
        let address = self.index_to_address.read(&index)?;
        if address.is_zero() {
            return Err(PrecompileError::Fatal(format!(
                "validator registry index {index} is empty below validator_count {count}"
            )));
        }
        let reverse_index = self.address_to_index.read(&address)?;
        if reverse_index != index {
            return Err(PrecompileError::Fatal(format!(
                "validator registry reverse index mismatch for {address}: expected {index}, got {reverse_index}"
            )));
        }
        Ok(Some(address))
    }

    /// Returns the registered validator's consensus identity key.
    pub fn consensus_pubkey_of(&self, addr: Address) -> Result<Option<[u8; 48]>> {
        Ok(self.validator_state(addr)?.consensus_pubkey().copied())
    }

    /// Returns participation counters, preserving the legacy all-zero result for
    /// an address that has no registry history.
    pub fn participation(&self, addr: Address) -> Result<ValidatorParticipation> {
        let state = self.validator_state(addr)?;
        let Some(history) = state.history() else {
            return Ok(ValidatorParticipation::default());
        };
        Ok(ValidatorParticipation {
            blocks_proposed: history.blocks_proposed(),
            missed_blocks: history.missed_blocks(),
            missed_votes: history.missed_votes(),
        })
    }

    /// Returns `true` if the address is a registered validator.
    pub fn is_validator(&self, addr: Address) -> Result<bool> {
        Ok(self.validator_state(addr)?.is_registered())
    }
}

/// Metadata of the current consensus set and epoch.
impl ValidatorSet<'_> {
    /// Returns whether there is a pending validator set change that consensus should detect.
    pub fn has_pending_set_change(&self) -> Result<bool> {
        self.pending_set_change.read()
    }

    /// Returns the persisted active-set hash without exposing its storage slot.
    pub fn active_consensus_set_hash(&self) -> Result<B256> {
        self.active_consensus_set_hash.read()
    }

    /// Returns the current epoch as `u64`. An oversized persisted value is
    /// deterministic state corruption and therefore fails closed.
    pub fn current_epoch_u64(&self) -> Result<u64> {
        self.epoch_number
            .read()?
            .try_into()
            .map_err(|_| PrecompileError::Fatal("ValidatorSet.epoch_number exceeds u64".into()))
    }

    /// Returns all public epoch metadata as one consistent read projection.
    pub fn epoch_snapshot(&self) -> Result<EpochSnapshot> {
        Ok(EpochSnapshot {
            number: self.epoch_number.read()?,
            start_timestamp: self.epoch_start_timestamp.read()?,
            start_block: self.epoch_start_block.read()?,
            length_blocks: self.config_epoch_length_blocks.read()?,
        })
    }
}

/// Reverse lookups from consensus keys and Radicle NodeIds to validators.
impl ValidatorSet<'_> {
    /// Looks up a validator address by consensus pubkey hash.
    ///
    /// The hash is `keccak256(48-byte BLS MinPk pubkey)`.
    pub fn lookup_by_pubkey_hash(&self, pubkey_hash: B256) -> Result<Address> {
        self.consensus_pubkey_hash_to_address.read(&pubkey_hash)
    }

    /// Returns the Radicle NodeId bound to `validator`, or zero when absent.
    pub fn get_radicle_node_id(&self, validator: Address) -> Result<B256> {
        self.val_radicle_node_id.read(&validator)
    }

    /// Returns the validator that owns `node_id`, or zero when unbound.
    pub fn validator_by_radicle_node_id(&self, node_id: B256) -> Result<Address> {
        self.radicle_node_id_to_validator.read(&node_id)
    }
}
