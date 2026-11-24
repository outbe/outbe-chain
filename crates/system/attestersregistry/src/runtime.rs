//! Runtime implementation for the AttestersRegistry native precompile.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::error::Result;

use crate::errors::AttestersRegistryError;
use crate::precompile::IAttestersRegistry;
use crate::schema::{AttesterRecord, AttesterRecordEntryExt, AttestersRegistryContract};

impl AttestersRegistryContract<'_> {
    /// Seeds the initial admin directly (for genesis / tests).
    pub fn bootstrap_admin(&mut self, admin: Address) -> Result<()> {
        if admin == Address::ZERO {
            return Err(AttestersRegistryError::ZeroAddress.into());
        }
        self.admin.write(admin)
    }

    fn require_admin(&self, caller: Address) -> Result<()> {
        let admin = self.admin.read()?;
        if admin == Address::ZERO || caller != admin {
            return Err(AttestersRegistryError::NotAdmin.into());
        }
        Ok(())
    }

    /// Reads the raw stored fields of an attester record, returning defaults when absent.
    pub fn read_record(&self, attester: Address) -> Result<AttesterRecord> {
        let entry = self.records.entry(attester);
        Ok(AttesterRecord {
            attester,
            active: entry.active().read()?,
            permission_mask: entry.permission_mask().read()?,
            registered_at: entry.registered_at().read()?,
            is_rotation_candidate: entry.is_rotation_candidate().read()?,
            consensus_key: entry.consensus_key().read()?,
            p2p_addr: entry.p2p_addr().read()?,
        })
    }

    /// Registers or re-registers an attester with permissions and optional consensus identity.
    pub fn register(
        &mut self,
        caller: Address,
        attester: Address,
        permission_mask: u32,
        is_rotation_candidate: bool,
        consensus_key: B256,
        p2p_addr: U256,
    ) -> Result<()> {
        self.require_admin(caller)?;
        if attester == Address::ZERO {
            return Err(AttestersRegistryError::ZeroAddress.into());
        }
        if permission_mask == 0 {
            return Err(AttestersRegistryError::InvalidMask.into());
        }
        if is_rotation_candidate && consensus_key == B256::ZERO {
            return Err(AttestersRegistryError::RotationCandidateNeedsConsensusKey.into());
        }

        let block_number = self.storage.block_number()?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let entry = self.records.entry(attester);
            let prev_active = entry.active().read()?;
            let prev_registered_at = entry.registered_at().read()?;

            if !prev_active && prev_registered_at == 0 {
                self.all_actors.push(attester)?;
            }

            entry.active().write(true)?;
            entry.permission_mask().write(permission_mask)?;
            entry.registered_at().write(block_number)?;
            entry.is_rotation_candidate().write(is_rotation_candidate)?;
            entry.consensus_key().write(consensus_key)?;
            entry.p2p_addr().write(p2p_addr)?;

            self.emit(IAttestersRegistry::AttesterRegistered {
                attester,
                permissionMask: permission_mask,
                registeredAt: block_number,
                isRotationCandidate: is_rotation_candidate,
            })?;
            self.emit(IAttestersRegistry::AttesterConsensusIdentitySet {
                attester,
                consensusKey: consensus_key,
                p2pAddr: p2p_addr,
            })?;

            self.try_activate(block_number)?;
            Ok(())
        })
    }

    /// Revokes a registered attester, clearing its active flag.
    pub fn revoke(&mut self, caller: Address, attester: Address) -> Result<()> {
        self.require_admin(caller)?;
        let entry = self.records.entry(attester);
        if entry.registered_at().read()? == 0 {
            return Err(AttestersRegistryError::NotRegistered { attester }.into());
        }

        let block_number = self.storage.block_number()?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.records.entry(attester).active().write(false)?;
            self.emit(IAttestersRegistry::AttesterRevoked { attester })?;
            self.try_activate(block_number)?;
            Ok(())
        })
    }

    /// Updates an active attester's permission mask.
    pub fn update_mask(&mut self, caller: Address, attester: Address, new_mask: u32) -> Result<()> {
        self.require_admin(caller)?;
        let entry = self.records.entry(attester);
        if !entry.active().read()? {
            return Err(AttestersRegistryError::NotRegistered { attester }.into());
        }
        if new_mask == 0 {
            return Err(AttestersRegistryError::InvalidMask.into());
        }

        let old_mask = entry.permission_mask().read()?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.records
                .entry(attester)
                .permission_mask()
                .write(new_mask)?;
            self.emit(IAttestersRegistry::AttesterMaskUpdated {
                attester,
                oldMask: old_mask,
                newMask: new_mask,
            })?;
            Ok(())
        })
    }

    /// Promotes or demotes a registered attester's rotation candidacy.
    pub fn set_rotation_candidate(
        &mut self,
        caller: Address,
        attester: Address,
        is_rotation_candidate: bool,
    ) -> Result<()> {
        self.require_admin(caller)?;
        let entry = self.records.entry(attester);
        if entry.registered_at().read()? == 0 {
            return Err(AttestersRegistryError::NotRegistered { attester }.into());
        }
        let consensus_key = entry.consensus_key().read()?;
        if is_rotation_candidate && consensus_key == B256::ZERO {
            return Err(AttestersRegistryError::RotationCandidateNeedsConsensusKey.into());
        }

        let block_number = self.storage.block_number()?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.records
                .entry(attester)
                .is_rotation_candidate()
                .write(is_rotation_candidate)?;
            self.emit(IAttestersRegistry::AttesterRotationCandidacyUpdated {
                attester,
                isRotationCandidate: is_rotation_candidate,
            })?;
            self.try_activate(block_number)?;
            Ok(())
        })
    }

    /// Updates a registered attester's consensus key and p2p address.
    pub fn set_consensus_identity(
        &mut self,
        caller: Address,
        attester: Address,
        consensus_key: B256,
        p2p_addr: U256,
    ) -> Result<()> {
        self.require_admin(caller)?;
        let entry = self.records.entry(attester);
        if entry.registered_at().read()? == 0 {
            return Err(AttestersRegistryError::NotRegistered { attester }.into());
        }
        if entry.is_rotation_candidate().read()? && consensus_key == B256::ZERO {
            return Err(AttestersRegistryError::RotationCandidateNeedsConsensusKey.into());
        }

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let entry = self.records.entry(attester);
            entry.consensus_key().write(consensus_key)?;
            entry.p2p_addr().write(p2p_addr)?;
            self.emit(IAttestersRegistry::AttesterConsensusIdentitySet {
                attester,
                consensusKey: consensus_key,
                p2pAddr: p2p_addr,
            })?;
            Ok(())
        })
    }

    /// Requests a forced reshare of the threshold material for the current committee.
    pub fn force_reshare(&mut self, caller: Address, skip_dealers: Vec<Address>) -> Result<u64> {
        self.require_admin(caller)?;
        let block_number = self.storage.block_number()?;
        let next_gen = self.reshare_generation.read()?.wrapping_add(1);

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.reshare_generation.write(next_gen)?;
            self.reshare_skip_dealers.clear()?;
            for dealer in &skip_dealers {
                self.reshare_skip_dealers.push(*dealer)?;
            }
            self.emit(IAttestersRegistry::ReshareRequested {
                generation: next_gen,
                blockNumber: block_number,
                skipDealers: skip_dealers,
            })?;
            Ok(next_gen)
        })
    }

    /// Records a committee member's attestation for a requested reshare generation.
    pub fn attest_generation(
        &mut self,
        caller: Address,
        generation: u64,
        output_hash: B256,
    ) -> Result<()> {
        let rec = self.read_record(caller)?;
        if rec.registered_at == 0 {
            return Err(AttestersRegistryError::NotRegistered { attester: caller }.into());
        }
        if !rec.active || !rec.is_rotation_candidate {
            return Err(AttestersRegistryError::NotRotationCandidate { attester: caller }.into());
        }
        if output_hash == B256::ZERO {
            return Err(AttestersRegistryError::ZeroOutputHash.into());
        }
        let requested = self.reshare_generation.read()?;
        if generation != requested {
            return Err(AttestersRegistryError::WrongGeneration {
                attested: generation,
                requested,
            }
            .into());
        }
        if generation <= self.activated_generation.read()? {
            return Err(AttestersRegistryError::GenerationAlreadyActive { generation }.into());
        }
        if self.attested.map(&generation).read(&caller)? {
            return Err(AttestersRegistryError::AlreadyAttested {
                generation,
                attester: caller,
            }
            .into());
        }

        let block_number = self.storage.block_number()?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.attested.map(&generation).write(&caller, true)?;
            self.attested_output_hash
                .map(&generation)
                .write(&caller, output_hash)?;
            let count = self.effective_attest_count(generation, output_hash)?;
            self.emit(IAttestersRegistry::GenerationAttested {
                generation,
                attester: caller,
                outputHash: output_hash,
                count,
            })?;
            self.try_activate(block_number)?;
            Ok(())
        })
    }

    fn try_activate(&mut self, block_number: u64) -> Result<()> {
        let generation = self.reshare_generation.read()?;
        if generation <= self.activated_generation.read()? {
            return Ok(());
        }
        let live = self.rotation_candidates_sorted()?;
        let need = quorum_for(live.len());
        if need == 0 {
            return Ok(());
        }

        let mut candidate = B256::ZERO;
        let mut lead: usize = 0;
        let output_hashes = self.attested_output_hash.map(&generation);
        for member in &live {
            let h = output_hashes.read(member)?;
            if lead == 0 {
                candidate = h;
                lead = 1;
            } else if h == candidate {
                lead += 1;
            } else {
                lead -= 1;
            }
        }
        if candidate == B256::ZERO {
            return Ok(());
        }
        let count = self.count_for(generation, candidate, &live)?;
        if count >= need {
            self.activated_generation.write(generation)?;
            self.activated_output_hash.write(candidate)?;
            self.emit(IAttestersRegistry::GenerationActivated {
                generation,
                outputHash: candidate,
                attestations: count,
                blockNumber: block_number,
            })?;
        }
        Ok(())
    }

    /// Initiates a two-step admin transfer.
    pub fn initiate_admin_transfer(&mut self, caller: Address, new_admin: Address) -> Result<()> {
        self.require_admin(caller)?;
        if new_admin == Address::ZERO {
            return Err(AttestersRegistryError::ZeroAddress.into());
        }
        let current = self.admin.read()?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.pending_admin.write(new_admin)?;
            self.emit(IAttestersRegistry::AdminTransferInitiated {
                from: current,
                to: new_admin,
            })?;
            Ok(())
        })
    }

    /// Completes a two-step admin transfer.
    pub fn accept_admin_transfer(&mut self, caller: Address) -> Result<()> {
        let pending = self.pending_admin.read()?;
        if pending == Address::ZERO || caller != pending {
            return Err(AttestersRegistryError::NotAdmin.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.admin.write(pending)?;
            self.pending_admin.write(Address::ZERO)?;
            self.emit(IAttestersRegistry::AdminTransferCompleted { newAdmin: pending })?;
            Ok(())
        })
    }

    // -----------------------------------------------------------------------
    // Views
    // -----------------------------------------------------------------------

    pub fn is_active(&self, attester: Address) -> Result<bool> {
        self.records.entry(attester).active().read()
    }

    pub fn has_permission(
        &self,
        attester: Address,
        bit: IAttestersRegistry::PermissionBit,
    ) -> Result<bool> {
        let entry = self.records.entry(attester);
        if !entry.active().read()? {
            return Ok(false);
        }
        let mask = entry.permission_mask().read()?;
        let bit_index = u8::from(bit);
        Ok((mask >> bit_index) & 1 == 1)
    }

    pub fn all_actors(&self) -> Result<Vec<Address>> {
        self.all_actors.read_all()
    }

    pub fn actor_count(&self) -> Result<U256> {
        Ok(U256::from(self.all_actors.len()?))
    }

    pub fn rotation_candidates_sorted(&self) -> Result<Vec<Address>> {
        let actors = self.all_actors.read_all()?;
        let mut out = Vec::new();
        for actor in actors {
            let entry = self.records.entry(actor);
            if entry.active().read()? && entry.is_rotation_candidate().read()? {
                out.push(actor);
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    pub fn attestation_quorum(&self) -> Result<u32> {
        Ok(quorum_for(self.rotation_candidates_sorted()?.len()))
    }

    pub fn effective_attest_count(&self, generation: u64, output_hash: B256) -> Result<u32> {
        let live = self.rotation_candidates_sorted()?;
        self.count_for(generation, output_hash, &live)
    }

    fn count_for(&self, generation: u64, output_hash: B256, live: &[Address]) -> Result<u32> {
        let output_hashes = self.attested_output_hash.map(&generation);
        let mut count: u32 = 0;
        for member in live {
            if output_hashes.read(member)? == output_hash {
                count = count.saturating_add(1);
            }
        }
        Ok(count)
    }
}

fn quorum_for(n: usize) -> u32 {
    let n = u32::try_from(n).unwrap_or(u32::MAX);
    if n == 0 {
        return 0;
    }
    n - (n - 1) / 3
}
