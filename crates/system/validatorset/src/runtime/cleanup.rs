use crate::schema::ValidatorSet;
use crate::state_machine::{self, ValidatorHistory, ValidatorLifecycle};
use alloy_primitives::{Address, B256, U256};
use outbe_primitives::error::{PrecompileError, Result};

impl ValidatorSet<'_> {
    /// Removes INACTIVE validator entries from the registry via swap-remove.
    ///
    /// `max_removals` caps how many entries are cleaned per call (0 = unlimited).
    /// Returns the number of entries removed.
    pub fn cleanup_inactive_validators(&mut self, max_removals: u32) -> Result<u32> {
        let guard = self.storage.checkpoint_guard();
        let mut count = self.validator_count.read()?;
        let mut removed = 0u32;
        let mut i = 1u64;
        let current_height = self.storage.block_number()?;
        let cooldown = u64::from(self.config_reregistration_cooldown.read()?);

        while i <= count as u64 {
            if max_removals > 0 && removed >= max_removals {
                break;
            }
            let addr = self.index_to_address.read(&i)?;
            if addr.is_zero() {
                i += 1;
                continue;
            }
            let state = self.validator_state(addr)?;
            let ValidatorLifecycle::Inactive(inactive) = state.lifecycle().clone() else {
                i += 1;
                continue;
            };
            let deactivated_at = state
                .history()
                .and_then(ValidatorHistory::last_deactivated_at_height)
                .ok_or_else(|| {
                    PrecompileError::Fatal(format!(
                        "inactive validator {addr} has no deactivation height"
                    ))
                })?;
            let cleanup_at = deactivated_at
                .checked_add(cooldown)
                .ok_or_else(|| PrecompileError::Fatal("inactive cleanup height overflow".into()))?;
            if current_height < cleanup_at {
                i += 1;
                continue;
            }

            if self.val_ocomp_recovery_deadline.read(&addr)? != 0 {
                i += 1;
                continue;
            }
            debug_assert_eq!(state_machine::cleanup(inactive), ValidatorLifecycle::Absent);
            // Clear all per-validator storage
            self.clear_validator_storage(&addr)?;

            // Swap with last entry
            let count_u64 = count as u64;
            if i < count_u64 {
                let last_addr = self.index_to_address.read(&count_u64)?;
                self.index_to_address.write(&i, last_addr)?;
                self.address_to_index.write(&last_addr, i)?;
            }
            // Clear the last slot
            self.index_to_address.write(&count_u64, Address::ZERO)?;
            self.address_to_index.write(&addr, 0)?;
            count -= 1;
            removed += 1;
            // Don't increment i - the swapped-in entry needs checking
        }

        self.validator_count.write(count)?;
        guard.commit();
        Ok(removed)
    }

    /// Clears all per-validator storage fields for an address.
    fn clear_validator_storage(&mut self, addr: &Address) -> Result<()> {
        let pubkey = self.read_consensus_pubkey(addr)?;
        let pk_hash = Self::consensus_pubkey_hash(&pubkey);
        self.consensus_pubkey_hash_to_address
            .write(&pk_hash, Address::ZERO)?;
        let radicle_node_id = self.val_radicle_node_id.read(addr)?;
        if !radicle_node_id.is_zero() {
            self.radicle_node_id_to_validator
                .write(&radicle_node_id, Address::ZERO)?;
        }
        self.val_radicle_node_id.write(addr, B256::ZERO)?;

        self.write_consensus_pubkey(addr, &[0u8; 48])?;
        self.val_stake.write(addr, U256::ZERO)?;
        self.val_status.write(addr, 0)?;
        self.val_slash_count.write(addr, 0)?;
        self.val_missed_blocks.write(addr, 0)?;
        self.val_missed_votes.write(addr, 0)?;
        self.val_blocks_proposed.write(addr, 0)?;
        self.val_joined_at_height.write(addr, 0)?;
        self.val_deactivated_at_height.write(addr, 0)?;
        self.val_unbonding_end.write(addr, 0)?;
        self.val_has_bls_share.write(addr, false)?;
        self.val_p2p_address_version.write(addr, 0)?;
        self.val_p2p_address_payload.get_bytes(addr).clear()?;
        // Stale-join + jail per-validator state must be cleared too, so a future
        // re-registration at the same address starts clean (a leaked
        // `val_join_confirmed = true` would bypass the stale-join guard). The
        // admitted V1 OCOMP key and reverse reservation deliberately survive:
        // key_epoch=1 has no rotation, loss, or recovery path.
        self.val_join_confirmed.write(addr, false)?;
        self.val_jailed_at_height.write(addr, 0)?;
        self.val_ocomp_miss_count.write(addr, 0)?;
        self.val_ocomp_recovery_deadline.write(addr, 0)?;
        Ok(())
    }
}
