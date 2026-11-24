//! Runtime implementation for the SpendingUnitV2 native precompile.

use alloy_primitives::{Address, Bytes, FixedBytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_attestersregistry::{AttestersRegistryContract, IAttestersRegistry};
use outbe_enclaveregistry::{verify_ed25519_strict, EnclaveRegistryContract, IEnclaveRegistry};
use outbe_fsepochregistry::{
    verify_eip2537_min_sig, EpochKey, FsEpochRegistryContract, IFsEpochRegistry, EIP2537_G1_LEN,
};
use outbe_primitives::error::Result;
use outbe_system_common::addresses::{
    ATTESTERS_REGISTRY_ADDRESS, ENCLAVE_REGISTRY_ADDRESS, FS_EPOCH_REGISTRY_ADDRESS,
};
use sha2::{Digest as _, Sha256};

use crate::errors::SpendingUnitV2Error;
use crate::precompile::ISpendingUnitV2;
use crate::schema::{SpendingUnitV2Contract, SpendingUnitV2Record, SpendingUnitV2RecordEntryExt};
use crate::worldwide_day::{day_number, today_utc_plus_14};

pub const VERSION: &str = "4.0.0";
pub const MAX_SEALS: usize = 5;
pub const POOL_RETENTION_DAYS: u64 = 28;
const POOL_AFTER_DAY_SLACK: u64 = 2;
const SETTLE_SIG_LEN: usize = 64;
const ONE_ETHER_ATTO: u64 = 1_000_000_000_000_000_000;

const BDP_DST: &[u8] = b"bdp-enclave-v1";
const TAG_SETTLEMENT: u8 = 0x12;
const HANDLE_PREFIX: &[u8] = b"bdp-fs-handle-v4";
const SEAL_ID_DOMAIN: &[u8] = b"bdp-seal-id-v3";
const SETTLEMENT_ID_DOMAIN: &[u8] = b"bdp-settlement-id-v1";

/// BN254 scalar field modulus `r`.
pub const BN254_FR_MODULUS: U256 = U256::from_be_bytes(alloy_primitives::hex!(
    "30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001"
));

const EXTERNAL_CALL_GAS: u64 = 200_000;

/// `IERC165` interface id (`0x01ffc9a7`).
const IERC165_INTERFACE_ID: [u8; 4] = [0x01, 0xff, 0xc9, 0xa7];

const fn xor4(a: [u8; 4], b: [u8; 4]) -> [u8; 4] {
    [a[0] ^ b[0], a[1] ^ b[1], a[2] ^ b[2], a[3] ^ b[3]]
}

/// `type(ICommitmentToken).interfaceId` = `exists(uint256)` ^ `total()`.
const ICOMMITMENT_TOKEN_INTERFACE_ID: [u8; 4] = xor4(
    ISpendingUnitV2::existsCall::SELECTOR,
    ISpendingUnitV2::totalCall::SELECTOR,
);

/// `type(ISpendingUnitV2).interfaceId` = XOR of the 6 functions declared directly on `ISpendingUnitV2`.
const ISPENDING_UNIT_V2_INTERFACE_ID: [u8; 4] = xor4(
    xor4(
        xor4(
            ISpendingUnitV2::submitSUCall::SELECTOR,
            ISpendingUnitV2::multicallCall::SELECTOR,
        ),
        xor4(
            ISpendingUnitV2::getDataCall::SELECTOR,
            ISpendingUnitV2::settlementsCall::SELECTOR,
        ),
    ),
    xor4(
        ISpendingUnitV2::enclaveRegistryCall::SELECTOR,
        ISpendingUnitV2::fsEpochRegistryCall::SELECTOR,
    ),
);

/// `SHA-256("bdp-seal-id-v3" || ctx || u64le(sequence) || u32le(pool_epoch) || u64le(count) || f_fold)`.
pub fn seal_id(ctx: B256, sequence: u64, pool_epoch: u32, count: u64, f_fold: B256) -> B256 {
    let mut h = Sha256::new();
    h.update(SEAL_ID_DOMAIN);
    h.update(ctx.as_slice());
    h.update(sequence.to_le_bytes());
    h.update(pool_epoch.to_le_bytes());
    h.update(count.to_le_bytes());
    h.update(f_fold.as_slice());
    B256::from_slice(&h.finalize())
}

/// `SHA-256("bdp-settlement-id-v1" || ctx || u64le(sequence))`.
pub fn settlement_id(ctx: B256, sequence: u64) -> B256 {
    let mut h = Sha256::new();
    h.update(SETTLEMENT_ID_DOMAIN);
    h.update(ctx.as_slice());
    h.update(sequence.to_le_bytes());
    B256::from_slice(&h.finalize())
}

/// Builds the enclave's Ed25519 settlement signing preimage (matching `bdp-seal-codec` byte-for-byte).
pub fn settlement_message(
    mint: &ISpendingUnitV2::Mint,
    batch: &ISpendingUnitV2::Batch,
    rm_digest: B256,
) -> Result<Vec<u8>> {
    let root_len = u32::try_from(batch.commitmentRoot.len())
        .map_err(|_| SpendingUnitV2Error::MalformedInput)?;
    let mut msg = Vec::with_capacity(
        BDP_DST.len() + 1 + 32 + 32 + 4 + batch.commitmentRoot.len() + 8 + 2 + 8 + 8 + 8 + 8 + 32,
    );
    msg.extend_from_slice(BDP_DST);
    msg.push(TAG_SETTLEMENT);
    msg.extend_from_slice(batch.ctx.as_slice());
    msg.extend_from_slice(rm_digest.as_slice());
    msg.extend_from_slice(&root_len.to_le_bytes());
    msg.extend_from_slice(batch.commitmentRoot.as_ref());
    msg.extend_from_slice(&batch.sequence.to_le_bytes());
    msg.extend_from_slice(&mint.currency.to_le_bytes());
    msg.extend_from_slice(&mint.amountBase.to_le_bytes());
    msg.extend_from_slice(&mint.amountAtto.to_le_bytes());
    msg.extend_from_slice(&batch.worldwideDay.to_le_bytes());
    msg.extend_from_slice(&batch.count.to_le_bytes());
    msg.extend_from_slice(batch.fFold.as_slice());
    Ok(msg)
}

fn require_canonical(value: B256) -> Result<()> {
    if U256::from_be_bytes(value.0) >= BN254_FR_MODULUS {
        return Err(SpendingUnitV2Error::NonCanonicalFieldElement { value }.into());
    }
    Ok(())
}

impl SpendingUnitV2Contract<'_> {
    /// Seeds initial genesis/test configuration directly.
    pub fn bootstrap(
        &mut self,
        owner: Address,
        attester_registry: Address,
        enclave_registry: Address,
        fs_epoch_registry: Address,
    ) -> Result<()> {
        if owner == Address::ZERO {
            return Err(SpendingUnitV2Error::OwnableInvalidOwner { owner }.into());
        }
        if attester_registry == Address::ZERO
            || enclave_registry == Address::ZERO
            || fs_epoch_registry == Address::ZERO
        {
            return Err(SpendingUnitV2Error::ZeroAttesterRegistry.into());
        }
        self.owner.write(owner)?;
        self.paused.write(false)?;
        self.attester_registry.write(attester_registry)?;
        self.enclave_registry.write(enclave_registry)?;
        self.fs_epoch_registry.write(fs_epoch_registry)?;
        Ok(())
    }

    fn require_owner(&self, caller: Address) -> Result<()> {
        let owner = self.owner.read()?;
        if owner == Address::ZERO || caller != owner {
            return Err(SpendingUnitV2Error::OwnableUnauthorizedAccount { account: caller }.into());
        }
        Ok(())
    }

    fn check_permitted_attester(&self, caller: Address) -> Result<()> {
        let reg_addr = self.attester_registry.read()?;
        if reg_addr == ATTESTERS_REGISTRY_ADDRESS {
            let reg = AttestersRegistryContract::new(self.storage.clone());
            if !reg.is_active(caller)? {
                return Err(SpendingUnitV2Error::AttesterNotActive.into());
            }
            if !reg.has_permission(
                caller,
                IAttestersRegistry::PermissionBit::SubmitSpendingUnit,
            )? {
                return Err(SpendingUnitV2Error::AttesterNotPermitted.into());
            }
            return Ok(());
        }
        if reg_addr == Address::ZERO {
            return Err(SpendingUnitV2Error::AttesterNotActive.into());
        }

        let is_active = self
            .storage
            .staticcall_with_gas(
                reg_addr,
                Bytes::from(IAttestersRegistry::isActiveCall { attester: caller }.abi_encode()),
                EXTERNAL_CALL_GAS,
            )
            .ok()
            .and_then(|ret| IAttestersRegistry::isActiveCall::abi_decode_returns(&ret).ok())
            .unwrap_or(false);
        if !is_active {
            return Err(SpendingUnitV2Error::AttesterNotActive.into());
        }

        let permitted = self
            .storage
            .staticcall_with_gas(
                reg_addr,
                Bytes::from(
                    IAttestersRegistry::hasPermissionCall {
                        attester: caller,
                        bit: IAttestersRegistry::PermissionBit::SubmitSpendingUnit,
                    }
                    .abi_encode(),
                ),
                EXTERNAL_CALL_GAS,
            )
            .ok()
            .and_then(|ret| IAttestersRegistry::hasPermissionCall::abi_decode_returns(&ret).ok())
            .unwrap_or(false);
        if !permitted {
            return Err(SpendingUnitV2Error::AttesterNotPermitted.into());
        }
        Ok(())
    }

    fn check_enclave_and_rm_digest(
        &self,
        enclave_pk: B256,
        attester: Address,
        provider_id: B256,
    ) -> Result<B256> {
        let reg_addr = self.enclave_registry.read()?;
        if reg_addr == ENCLAVE_REGISTRY_ADDRESS {
            let reg = EnclaveRegistryContract::new(self.storage.clone());
            if !reg.is_active_for(enclave_pk, attester, provider_id)? {
                return Err(SpendingUnitV2Error::EnclaveNotActiveFor {
                    enclave_pk,
                    attester,
                }
                .into());
            }
            return Ok(reg.read_deployment_record(enclave_pk)?.rm_digest);
        }
        if reg_addr == Address::ZERO {
            return Err(SpendingUnitV2Error::EnclaveNotActiveFor {
                enclave_pk,
                attester,
            }
            .into());
        }

        let active = self
            .storage
            .staticcall_with_gas(
                reg_addr,
                Bytes::from(
                    IEnclaveRegistry::isActiveForCall {
                        enclavePk: enclave_pk,
                        attester,
                        providerId: provider_id,
                    }
                    .abi_encode(),
                ),
                EXTERNAL_CALL_GAS,
            )
            .ok()
            .and_then(|ret| IEnclaveRegistry::isActiveForCall::abi_decode_returns(&ret).ok())
            .unwrap_or(false);
        if !active {
            return Err(SpendingUnitV2Error::EnclaveNotActiveFor {
                enclave_pk,
                attester,
            }
            .into());
        }

        let dep_bytes = self.storage.staticcall_with_gas(
            reg_addr,
            Bytes::from(
                IEnclaveRegistry::getDeploymentCall {
                    enclavePk: enclave_pk,
                }
                .abi_encode(),
            ),
            EXTERNAL_CALL_GAS,
        )?;
        let dep = IEnclaveRegistry::getDeploymentCall::abi_decode_returns(&dep_bytes)
            .map_err(|_| SpendingUnitV2Error::MalformedInput)?;
        Ok(dep.rmDigest)
    }

    fn fetch_epoch_key(&self, pool_epoch: u32) -> Result<EpochKey> {
        let reg_addr = self.fs_epoch_registry.read()?;
        if reg_addr == FS_EPOCH_REGISTRY_ADDRESS {
            return FsEpochRegistryContract::new(self.storage.clone()).key_for(pool_epoch);
        }
        let ret_bytes = self.storage.staticcall_with_gas(
            reg_addr,
            Bytes::from(IFsEpochRegistry::keyForCall { e: pool_epoch }.abi_encode()),
            EXTERNAL_CALL_GAS,
        )?;
        let ret = IFsEpochRegistry::keyForCall::abi_decode_returns(&ret_bytes)
            .map_err(|_| SpendingUnitV2Error::MalformedInput)?;
        Ok(EpochKey {
            pk_att: ret.pkAtt.to_vec(),
            dst: ret.dst.to_vec(),
            start_wwd: ret.startWwd,
            end_wwd_exclusive: ret.endWwdExclusive,
        })
    }

    // -----------------------------------------------------------------------
    // Core settlement
    // -----------------------------------------------------------------------

    /// Settles one sealed batch into one SpendingUnitV2.
    pub fn submit_su(
        &mut self,
        caller: Address,
        mint: &ISpendingUnitV2::Mint,
        batch: &ISpendingUnitV2::Batch,
        seals: &[ISpendingUnitV2::Seal],
    ) -> Result<()> {
        self.check_permitted_attester(caller)?;
        if self.paused.read()? {
            return Err(SpendingUnitV2Error::EnforcedPause.into());
        }

        if batch.settleSig.len() != SETTLE_SIG_LEN || seals.is_empty() || seals.len() > MAX_SEALS {
            return Err(SpendingUnitV2Error::MalformedInput.into());
        }
        if self.committed.read(&mint.suId)? {
            return Err(SpendingUnitV2Error::AlreadyExists.into());
        }
        if mint.currency == 0 {
            return Err(SpendingUnitV2Error::InvalidCurrency.into());
        }
        if (mint.amountBase == 0 && mint.amountAtto == 0) || mint.amountAtto >= ONE_ETHER_ATTO {
            return Err(SpendingUnitV2Error::InvalidAmount.into());
        }
        if mint.derivedOwner == B256::ZERO {
            return Err(SpendingUnitV2Error::InvalidDerivedOwner.into());
        }
        require_canonical(mint.derivedOwner)?;
        require_canonical(B256::from(mint.suId))?;

        // 1. Enclave live for this attester & provider.
        let rm_digest =
            self.check_enclave_and_rm_digest(mint.enclavePk, caller, mint.providerId)?;

        // 2. Committee seals.
        let batch_day =
            day_number(batch.worldwideDay).ok_or(SpendingUnitV2Error::MalformedInput)?;
        let today = today_utc_plus_14(self.storage.now_secs()?);

        let mut seal_ids = Vec::with_capacity(seals.len());
        let mut total_count: u128 = 0;
        let mut total_fold = U256::ZERO;

        for (i, seal) in seals.iter().enumerate() {
            if seal.handleSig.len() != EIP2537_G1_LEN
                || (i > 0 && seal.poolEpoch <= seals[i - 1].poolEpoch)
            {
                return Err(SpendingUnitV2Error::MalformedInput.into());
            }
            let sid = seal_id(
                batch.ctx,
                batch.sequence,
                seal.poolEpoch,
                seal.count,
                seal.fFold,
            );
            self.verify_pool(caller, batch, batch_day, today, sid, seal)?;
            seal_ids.push(sid);
            total_count += u128::from(seal.count);
            total_fold = total_fold.add_mod(U256::from_be_bytes(seal.fFold.0), BN254_FR_MODULUS);
        }

        // 3. Batch totals match the seals.
        if total_count != u128::from(batch.count) || B256::from(total_fold) != batch.fFold {
            return Err(SpendingUnitV2Error::BatchTotalsMismatch.into());
        }

        // 4. Enclave Ed25519 settlement signature.
        let msg = settlement_message(mint, batch, rm_digest)?;
        if !verify_ed25519_strict(mint.enclavePk, batch.settleSig.as_ref(), &msg) {
            return Err(SpendingUnitV2Error::SettlementSignatureInvalid {
                enclave_pk: mint.enclavePk,
            }
            .into());
        }

        // 5. Consume context & mint SU atomically.
        let settled = self.settlements.read(&batch.ctx)?;
        if settled != B256::ZERO {
            return Err(SpendingUnitV2Error::AlreadySettled {
                ctx: batch.ctx,
                settled_id: settled,
            }
            .into());
        }
        if mint.suId.is_zero() {
            return Err(SpendingUnitV2Error::InvalidTokenId.into());
        }
        let sid_batch = settlement_id(batch.ctx, batch.sequence);

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.settlements.write(&batch.ctx, sid_batch)?;
            self.committed.write(&mint.suId, true)?;
            let next_total = self.total_committed.read()?.wrapping_add(U256::from(1u64));
            self.total_committed.write(next_total)?;

            let entry = self.entities.entry(mint.suId);
            entry.stored_su_id().write(mint.suId)?;
            entry.derived_owner().write(mint.derivedOwner)?;
            entry.attester_address().write(caller)?;
            entry.referrer_address().write(mint.referrerAddress)?;
            entry.currency().write(mint.currency)?;
            entry.worldwide_day().write(batch.worldwideDay)?;
            entry.amount_base().write(mint.amountBase)?;
            entry.amount_atto().write(mint.amountAtto)?;
            entry.ctx().write(batch.ctx)?;
            entry.seal_id().write(sid_batch)?;

            self.emit(ISpendingUnitV2::SUCreated {
                suId: mint.suId,
                ctx: batch.ctx,
                settlementId: sid_batch,
                attester: caller,
                derivedOwner: mint.derivedOwner,
                worldwideDay: batch.worldwideDay,
                currency: mint.currency,
                amountBase: mint.amountBase,
                amountAtto: mint.amountAtto,
            })?;
            for (seal, sid) in seals.iter().zip(seal_ids.iter()) {
                self.emit(ISpendingUnitV2::SealSettled {
                    ctx: batch.ctx,
                    sealId: *sid,
                    poolEpoch: seal.poolEpoch,
                })?;
            }
            Ok(())
        })
    }

    fn verify_pool(
        &self,
        caller: Address,
        batch: &ISpendingUnitV2::Batch,
        batch_day: u64,
        today: u64,
        sid: B256,
        seal: &ISpendingUnitV2::Seal,
    ) -> Result<()> {
        let epoch_key = self.fetch_epoch_key(seal.poolEpoch)?;
        let pool_start = day_number(u64::from(epoch_key.start_wwd))
            .ok_or(SpendingUnitV2Error::MalformedInput)?;
        if today > pool_start.saturating_add(POOL_RETENTION_DAYS) {
            return Err(SpendingUnitV2Error::PoolEpochRetired {
                pool_epoch: seal.poolEpoch,
            }
            .into());
        }
        if pool_start > batch_day.saturating_add(POOL_AFTER_DAY_SLACK) {
            return Err(SpendingUnitV2Error::PoolEpochAfterDay {
                pool_epoch: seal.poolEpoch,
                worldwide_day: batch.worldwideDay,
            }
            .into());
        }

        // `HANDLE_PREFIX || attester(20) || ctx(32) || u32le(32) || seal_id(32) || u64le(count) || f_fold(32) || u32le(pool_epoch)`
        let mut msg = Vec::with_capacity(HANDLE_PREFIX.len() + 20 + 32 + 4 + 32 + 8 + 32 + 4);
        msg.extend_from_slice(HANDLE_PREFIX);
        msg.extend_from_slice(caller.as_slice());
        msg.extend_from_slice(batch.ctx.as_slice());
        msg.extend_from_slice(&32u32.to_le_bytes());
        msg.extend_from_slice(sid.as_slice());
        msg.extend_from_slice(&seal.count.to_le_bytes());
        msg.extend_from_slice(seal.fFold.as_slice());
        msg.extend_from_slice(&seal.poolEpoch.to_le_bytes());

        if !verify_eip2537_min_sig(
            &epoch_key.pk_att,
            &msg,
            seal.handleSig.as_ref(),
            &epoch_key.dst,
        ) {
            return Err(SpendingUnitV2Error::HandleSignatureInvalid {
                pool_epoch: seal.poolEpoch,
            }
            .into());
        }
        Ok(())
    }

    /// Executes a batch of encoded `submitSU` calls atomically.
    pub fn multicall(&mut self, caller: Address, calls: &[Bytes]) -> Result<Vec<Bytes>> {
        for (i, item) in calls.iter().enumerate() {
            if item.len() < 4 || item[..4] != ISpendingUnitV2::submitSUCall::SELECTOR {
                return Err(SpendingUnitV2Error::MulticallMethodNotAllowed {
                    index: U256::from(i),
                }
                .into());
            }
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let mut results = Vec::with_capacity(calls.len());
            for item in calls {
                let decoded = ISpendingUnitV2::submitSUCall::abi_decode(item.as_ref())
                    .map_err(|_| SpendingUnitV2Error::MalformedInput)?;
                self.submit_su(caller, &decoded.mint, &decoded.batch, &decoded.seals)?;
                results.push(Bytes::new());
            }
            Ok(results)
        })
    }

    // -----------------------------------------------------------------------
    // Admin (Ownable, Pausable, AttesterGated)
    // -----------------------------------------------------------------------

    pub fn pause(&mut self, caller: Address) -> Result<()> {
        self.require_owner(caller)?;
        if self.paused.read()? {
            return Err(SpendingUnitV2Error::EnforcedPause.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.paused.write(true)?;
            self.emit(ISpendingUnitV2::Paused { account: caller })?;
            Ok(())
        })
    }

    pub fn unpause(&mut self, caller: Address) -> Result<()> {
        self.require_owner(caller)?;
        if !self.paused.read()? {
            return Err(SpendingUnitV2Error::ExpectedPause.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.paused.write(false)?;
            self.emit(ISpendingUnitV2::Unpaused { account: caller })?;
            Ok(())
        })
    }

    pub fn transfer_ownership(&mut self, caller: Address, new_owner: Address) -> Result<()> {
        self.require_owner(caller)?;
        if new_owner == Address::ZERO {
            return Err(SpendingUnitV2Error::OwnableInvalidOwner { owner: new_owner }.into());
        }
        let previous = self.owner.read()?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.owner.write(new_owner)?;
            self.emit(ISpendingUnitV2::OwnershipTransferred {
                previousOwner: previous,
                newOwner: new_owner,
            })?;
            Ok(())
        })
    }

    pub fn renounce_ownership(&mut self, caller: Address) -> Result<()> {
        self.require_owner(caller)?;
        let previous = self.owner.read()?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.owner.write(Address::ZERO)?;
            self.emit(ISpendingUnitV2::OwnershipTransferred {
                previousOwner: previous,
                newOwner: Address::ZERO,
            })?;
            Ok(())
        })
    }

    pub fn set_attesters_registry(&mut self, caller: Address, registry: Address) -> Result<()> {
        self.require_owner(caller)?;
        if registry == Address::ZERO {
            return Err(SpendingUnitV2Error::ZeroAttesterRegistry.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.attester_registry.write(registry)?;
            self.emit(ISpendingUnitV2::RegistryUpdated { registry })?;
            Ok(())
        })
    }

    // -----------------------------------------------------------------------
    // Views
    // -----------------------------------------------------------------------

    pub fn read_entity(&self, su_id: U256) -> Result<SpendingUnitV2Record> {
        let entry = self.entities.entry(su_id);
        Ok(SpendingUnitV2Record {
            su_id,
            stored_su_id: entry.stored_su_id().read()?,
            derived_owner: entry.derived_owner().read()?,
            attester_address: entry.attester_address().read()?,
            referrer_address: entry.referrer_address().read()?,
            currency: entry.currency().read()?,
            worldwide_day: entry.worldwide_day().read()?,
            amount_base: entry.amount_base().read()?,
            amount_atto: entry.amount_atto().read()?,
            ctx: entry.ctx().read()?,
            seal_id: entry.seal_id().read()?,
        })
    }

    pub fn get_data(&self, su_id: U256) -> Result<ISpendingUnitV2::SpendingUnitV2Entity> {
        let r = self.read_entity(su_id)?;
        Ok(ISpendingUnitV2::SpendingUnitV2Entity {
            suId: r.stored_su_id,
            derivedOwner: r.derived_owner,
            attesterAddress: r.attester_address,
            referrerAddress: r.referrer_address,
            currency: r.currency,
            worldwideDay: r.worldwide_day,
            amountBase: r.amount_base,
            amountAtto: r.amount_atto,
            ctx: r.ctx,
            sealId: r.seal_id,
        })
    }

    pub fn supports_interface(interface_id: FixedBytes<4>) -> bool {
        let id = interface_id.0;
        id == IERC165_INTERFACE_ID
            || id == ICOMMITMENT_TOKEN_INTERFACE_ID
            || id == ISPENDING_UNIT_V2_INTERFACE_ID
    }
}
