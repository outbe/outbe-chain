//! Runtime implementation for the EnclaveRegistry native precompile.

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use ed25519_dalek::{Signature as Ed25519Signature, VerifyingKey as Ed25519VerifyingKey};
use k256::ecdsa::{RecoveryId, Signature as K256Signature, VerifyingKey as K256VerifyingKey};
use outbe_attestersregistry::{AttestersRegistryContract, IAttestersRegistry};
use outbe_primitives::error::Result;
use outbe_system_common::addresses::{ATTESTERS_REGISTRY_ADDRESS, ENCLAVE_REGISTRY_ADDRESS};
use sha2::{Digest as _, Sha256, Sha512};

use crate::errors::EnclaveRegistryError;
use crate::precompile::IEnclaveRegistry;
use crate::schema::{
    DeploymentRecord, DeploymentRecordEntryExt, EnclaveRegistryContract, ProviderPolicyRecord,
    ProviderPolicyRecordEntryExt,
};
use crate::snp::{verify_snp_report, MAX_CERT_LEN, REPORT_LEN};

pub const DEFAULT_RETIRING_GRACE: u64 = 7200;
pub const MIN_SCHEMA: u16 = 6;
pub const REGISTRATION_VERSION: u8 = 1;
pub const REGISTRATION_VERSION_BOUND: u8 = 2;

const MAX_REGISTRATION: usize = 2048;
const MAX_MANIFEST: usize = 4096;
const MAX_ENTRIES: usize = 64;
const SIGNATURE_LEN: usize = 65;
const ENCLAVE_SIG_LEN: usize = 64;
const MEASUREMENT_LEN: usize = 48;
const MIN_PAYLOAD: usize = 1 + 32 + 32 + 4 + 4 + 33;

const DST: &[u8] = b"bdp-enclave-v1";
const TAG_IDENTITY: u8 = 0x01;
const TAG_ATTESTER_BINDING: u8 = 0x15;
const TAG_REGISTRATION: u8 = 0x21;

const POLICY_MIGRATE_MA: u64 = 1u64 << 18;
const POLICY_DEBUG: u64 = 1u64 << 19;

const STATUS_NONE: u8 = 0;
const STATUS_ACTIVE: u8 = 1;
const STATUS_RETIRING: u8 = 2;

/// `keccak256("eip1967.proxy.admin") - 1`
const PROXY_ADMIN_SLOT: U256 = U256::from_be_bytes(alloy_primitives::hex!(
    "b53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d6103"
));

/// Half of secp256k1 curve order `n / 2` for EIP-2 low-s enforcement.
const SECP256K1_HALF_N: U256 = U256::from_be_bytes(alloy_primitives::hex!(
    "7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0"
));

const EXTERNAL_ATTESTER_CALL_GAS: u64 = 100_000;

/// Strict Ed25519 signature verification (rejects non-canonical encodings and small-order keys).
pub fn verify_ed25519_strict(pk: B256, sig: &[u8], message: &[u8]) -> bool {
    let Ok(sig_bytes): std::result::Result<[u8; 64], _> = sig.try_into() else {
        return false;
    };
    let Ok(verifying_key) = Ed25519VerifyingKey::from_bytes(pk.as_ref()) else {
        return false;
    };
    let signature = Ed25519Signature::from_bytes(&sig_bytes);
    verifying_key.verify_strict(message, &signature).is_ok()
}

/// Byte-wise `tcb >= floor`: every component byte of `TCB_VERSION` is at or above `floor`'s.
pub fn tcb_at_least(tcb: u64, floor: u64) -> bool {
    let tcb_bytes = tcb.to_le_bytes();
    let floor_bytes = floor.to_le_bytes();
    tcb_bytes
        .iter()
        .zip(floor_bytes.iter())
        .all(|(t, f)| t >= f)
}

impl EnclaveRegistryContract<'_> {
    /// Seeds initial genesis/test configuration directly.
    pub fn bootstrap(
        &mut self,
        admin: Address,
        attesters_registry: Address,
        devnet_admin_register: bool,
    ) -> Result<()> {
        if admin == Address::ZERO || attesters_registry == Address::ZERO {
            return Err(EnclaveRegistryError::ZeroAddress.into());
        }
        self.admin.write(admin)?;
        self.attesters_registry.write(attesters_registry)?;
        self.max_schema.write(MIN_SCHEMA)?;
        self.min_registration_version
            .write(REGISTRATION_VERSION_BOUND)?;
        self.retiring_grace.write(DEFAULT_RETIRING_GRACE)?;
        self.devnet_admin_register.write(devnet_admin_register)?;
        Ok(())
    }

    /// ABI `initialize(_admin, _attestersRegistry)` preserving `SystemProxy` admin gating.
    pub fn initialize(
        &mut self,
        caller: Address,
        admin: Address,
        attesters_registry: Address,
    ) -> Result<()> {
        if self.admin.read()? != Address::ZERO {
            return Err(EnclaveRegistryError::AlreadyInitialized.into());
        }
        let proxy_admin_word = self
            .storage
            .sload(ENCLAVE_REGISTRY_ADDRESS, PROXY_ADMIN_SLOT)?;
        let proxy_admin = Address::from_word(B256::from(proxy_admin_word));
        if proxy_admin == Address::ZERO || caller != proxy_admin {
            return Err(EnclaveRegistryError::NotAdmin.into());
        }
        if admin == Address::ZERO || attesters_registry == Address::ZERO {
            return Err(EnclaveRegistryError::ZeroAddress.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.admin.write(admin)?;
            self.attesters_registry.write(attesters_registry)?;
            self.max_schema.write(MIN_SCHEMA)?;
            self.min_registration_version
                .write(REGISTRATION_VERSION_BOUND)?;
            self.retiring_grace.write(DEFAULT_RETIRING_GRACE)?;
            Ok(())
        })
    }

    fn require_admin(&self, caller: Address) -> Result<()> {
        let admin = self.admin.read()?;
        if admin == Address::ZERO || caller != admin {
            return Err(EnclaveRegistryError::NotAdmin.into());
        }
        Ok(())
    }

    fn attester_is_active(&self, attester: Address) -> Result<bool> {
        let reg_addr = self.attesters_registry.read()?;
        if reg_addr == ATTESTERS_REGISTRY_ADDRESS {
            return AttestersRegistryContract::new(self.storage.clone()).is_active(attester);
        }
        if reg_addr == Address::ZERO {
            return Ok(false);
        }
        let calldata = IAttestersRegistry::isActiveCall { attester }.abi_encode();
        match self.storage.staticcall_with_gas(
            reg_addr,
            Bytes::from(calldata),
            EXTERNAL_ATTESTER_CALL_GAS,
        ) {
            Ok(returndata) => IAttestersRegistry::isActiveCall::abi_decode_returns(&returndata)
                .map_err(|_| EnclaveRegistryError::AttesterNotActive.into()),
            Err(_) => Ok(false),
        }
    }

    fn attester_has_submit_su(&self, attester: Address) -> Result<bool> {
        let reg_addr = self.attesters_registry.read()?;
        if reg_addr == ATTESTERS_REGISTRY_ADDRESS {
            return AttestersRegistryContract::new(self.storage.clone()).has_permission(
                attester,
                IAttestersRegistry::PermissionBit::SubmitSpendingUnit,
            );
        }
        if reg_addr == Address::ZERO {
            return Ok(false);
        }
        let calldata = IAttestersRegistry::hasPermissionCall {
            attester,
            bit: IAttestersRegistry::PermissionBit::SubmitSpendingUnit,
        }
        .abi_encode();
        match self.storage.staticcall_with_gas(
            reg_addr,
            Bytes::from(calldata),
            EXTERNAL_ATTESTER_CALL_GAS,
        ) {
            Ok(returndata) => {
                IAttestersRegistry::hasPermissionCall::abi_decode_returns(&returndata)
                    .map_err(|_| EnclaveRegistryError::AttesterNotPermitted.into())
            }
            Err(_) => Ok(false),
        }
    }

    // -----------------------------------------------------------------------
    // Registration & lifecycle
    // -----------------------------------------------------------------------

    /// Registers an enclave deployment after verifying attester, SEV-SNP hardware,
    /// allowlisted certified measurement, and runtime manifest policy.
    pub fn register(
        &mut self,
        caller: Address,
        registration: &[u8],
        report: &[u8],
        vcek: &[u8],
        product: u8,
        manifest: &[u8],
    ) -> Result<()> {
        if registration.len() > MAX_REGISTRATION
            || report.len() != REPORT_LEN
            || vcek.len() > MAX_CERT_LEN
            || manifest.len() > MAX_MANIFEST
        {
            return Err(EnclaveRegistryError::InputTooLarge.into());
        }

        // 1. The attester.
        let (attester, enclave_pk, rm_digest, certified) =
            self.check_attester(caller, registration)?;

        // 2. The payload.
        if self.deployments.entry(enclave_pk).status().read()? != STATUS_NONE {
            return Err(EnclaveRegistryError::AlreadyRegistered { enclave_pk }.into());
        }

        // 3. The hardware.
        let (mh, chip, tcb, line) =
            self.verify_report(enclave_pk, rm_digest, report, vcek, product)?;

        // 4. The measurement.
        if !self.allowed_measurements.read(&mh)? {
            return Err(EnclaveRegistryError::MeasurementNotAllowed {
                measurement_hash: mh,
            }
            .into());
        }
        if !certified.contains(&mh) {
            return Err(EnclaveRegistryError::MeasurementNotCertified {
                measurement_hash: mh,
            }
            .into());
        }

        // 5. The manifest and its provider.
        let (provider_id, base_url_hash) = self.check_manifest(manifest, rm_digest)?;

        // 6. The record.
        let block_number = self.storage.block_number()?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.create_record(
                enclave_pk,
                attester,
                rm_digest,
                provider_id,
                base_url_hash,
                mh,
                block_number,
            )?;
            let entry = self.deployments.entry(enclave_pk);
            entry.tcb().write(tcb)?;
            entry.product().write(line)?;
            self.deployment_chip_ids.list(&enclave_pk).push(chip)?;
            self.emit(IEnclaveRegistry::DeploymentRegistered {
                enclavePk: enclave_pk,
                attester,
                providerId: provider_id,
                measurementHash: mh,
                rmDigest: rm_digest,
            })?;
            Ok(())
        })
    }

    /// Certifies another allowlisted launch measurement for a live deployment.
    pub fn add_measurement(
        &mut self,
        caller: Address,
        enclave_pk: B256,
        report: &[u8],
        vcek: &[u8],
        product: u8,
    ) -> Result<()> {
        if report.len() != REPORT_LEN || vcek.len() > MAX_CERT_LEN {
            return Err(EnclaveRegistryError::InputTooLarge.into());
        }
        let d = self.live_by_attester(caller, enclave_pk)?;
        let (mh, chip, tcb) = self.verify_live(&d, enclave_pk, report, vcek, product)?;
        if !self.allowed_measurements.read(&mh)? {
            return Err(EnclaveRegistryError::MeasurementNotAllowed {
                measurement_hash: mh,
            }
            .into());
        }
        let measurements = self.deployment_measurements.list(&enclave_pk).read_all()?;
        if measurements.contains(&mh) {
            return Err(EnclaveRegistryError::AlreadyKnownMeasurement {
                measurement_hash: mh,
            }
            .into());
        }
        if measurements.len() >= MAX_ENTRIES {
            return Err(EnclaveRegistryError::TooManyEntries.into());
        }

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.deployment_measurements.list(&enclave_pk).push(mh)?;
            self.absorb(enclave_pk, d.tcb, chip, tcb)?;
            self.emit(IEnclaveRegistry::MeasurementAdded {
                enclavePk: enclave_pk,
                measurementHash: mh,
                chipIdHash: chip,
            })?;
            Ok(())
        })
    }

    /// Re-attests a live deployment under a measurement it already certifies.
    pub fn reattest(
        &mut self,
        caller: Address,
        enclave_pk: B256,
        report: &[u8],
        vcek: &[u8],
        product: u8,
    ) -> Result<()> {
        if report.len() != REPORT_LEN || vcek.len() > MAX_CERT_LEN {
            return Err(EnclaveRegistryError::InputTooLarge.into());
        }
        let d = self.live_by_attester(caller, enclave_pk)?;
        let (mh, chip, tcb) = self.verify_live(&d, enclave_pk, report, vcek, product)?;
        let measurements = self.deployment_measurements.list(&enclave_pk).read_all()?;
        if !measurements.contains(&mh) {
            return Err(EnclaveRegistryError::UnknownMeasurement {
                measurement_hash: mh,
            }
            .into());
        }

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let new_tcb = self.absorb(enclave_pk, d.tcb, chip, tcb)?;
            self.emit(IEnclaveRegistry::Reattested {
                enclavePk: enclave_pk,
                measurementHash: mh,
                chipIdHash: chip,
                tcb: new_tcb,
            })?;
            Ok(())
        })
    }

    /// Starts retiring an Active deployment (stays active for `retiring_grace` blocks).
    pub fn retire(&mut self, caller: Address, enclave_pk: B256) -> Result<()> {
        let entry = self.deployments.entry(enclave_pk);
        if entry.status().read()? != STATUS_ACTIVE {
            return Err(EnclaveRegistryError::DeploymentNotLive { enclave_pk }.into());
        }
        if caller != entry.attester().read()? {
            return Err(EnclaveRegistryError::NotTheAttester.into());
        }
        let retiring_until = self
            .storage
            .block_number()?
            .saturating_add(self.retiring_grace.read()?);

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let entry = self.deployments.entry(enclave_pk);
            entry.status().write(STATUS_RETIRING)?;
            entry.retiring_until().write(retiring_until)?;
            self.emit(IEnclaveRegistry::DeploymentRetiring {
                enclavePk: enclave_pk,
                retiringUntil: retiring_until,
            })?;
            Ok(())
        })
    }

    /// Revokes a deployment outright (admin only), clearing its record and arrays.
    pub fn revoke(&mut self, caller: Address, enclave_pk: B256) -> Result<()> {
        self.require_admin(caller)?;
        if self.deployments.entry(enclave_pk).status().read()? == STATUS_NONE {
            return Err(EnclaveRegistryError::NotRegistered { enclave_pk }.into());
        }

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.deployments.delete(enclave_pk)?;
            self.deployment_measurements.list(&enclave_pk).clear()?;
            self.deployment_chip_ids.list(&enclave_pk).clear()?;
            self.emit(IEnclaveRegistry::DeploymentRevoked {
                enclavePk: enclave_pk,
            })?;
            Ok(())
        })
    }

    /// Devnet-only admin registration without hardware checks.
    #[allow(clippy::too_many_arguments)]
    pub fn admin_register(
        &mut self,
        caller: Address,
        enclave_pk: B256,
        attester: Address,
        rm_digest: B256,
        provider_id: B256,
        base_url_hash: B256,
        measurement_hash: B256,
    ) -> Result<()> {
        self.require_admin(caller)?;
        if !self.devnet_admin_register.read()? {
            return Err(EnclaveRegistryError::DevnetRegistrationDisabled.into());
        }
        if attester == Address::ZERO {
            return Err(EnclaveRegistryError::ZeroAddress.into());
        }
        if self.deployments.entry(enclave_pk).status().read()? != STATUS_NONE {
            return Err(EnclaveRegistryError::AlreadyRegistered { enclave_pk }.into());
        }

        let block_number = self.storage.block_number()?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.create_record(
                enclave_pk,
                attester,
                rm_digest,
                provider_id,
                base_url_hash,
                measurement_hash,
                block_number,
            )?;
            self.emit(IEnclaveRegistry::DeploymentRegistered {
                enclavePk: enclave_pk,
                attester,
                providerId: provider_id,
                measurementHash: measurement_hash,
                rmDigest: rm_digest,
            })?;
            Ok(())
        })
    }

    // -----------------------------------------------------------------------
    // Admin setters
    // -----------------------------------------------------------------------

    pub fn set_measurement(
        &mut self,
        caller: Address,
        measurement_hash: B256,
        allowed: bool,
    ) -> Result<()> {
        self.require_admin(caller)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.allowed_measurements
                .write(&measurement_hash, allowed)?;
            self.emit(IEnclaveRegistry::MeasurementSet {
                measurementHash: measurement_hash,
                allowed,
            })?;
            Ok(())
        })
    }

    pub fn set_provider(
        &mut self,
        caller: Address,
        provider_id: B256,
        policy: &IEnclaveRegistry::ProviderPolicy,
    ) -> Result<()> {
        self.require_admin(caller)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let entry = self.providers.entry(provider_id);
            entry.allowed().write(policy.allowed)?;
            entry.require_release().write(policy.requireRelease)?;
            entry.base_url_hash().write(policy.baseUrlHash)?;
            entry.environment_hash().write(policy.environmentHash)?;
            self.emit(IEnclaveRegistry::ProviderSet {
                providerId: provider_id,
                policy: policy.clone(),
            })?;
            Ok(())
        })
    }

    pub fn set_tcb_floor(&mut self, caller: Address, product: u8, floor: u64) -> Result<()> {
        self.require_admin(caller)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.tcb_floor.write(&product, floor)?;
            self.emit(IEnclaveRegistry::TcbFloorSet {
                product,
                tcbFloor: floor,
            })?;
            Ok(())
        })
    }

    pub fn revoke_chip(&mut self, caller: Address, chip_id_hash: B256) -> Result<()> {
        self.require_admin(caller)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.revoked_chips.write(&chip_id_hash, true)?;
            self.emit(IEnclaveRegistry::ChipIdRevoked {
                chipIdHash: chip_id_hash,
            })?;
            Ok(())
        })
    }

    pub fn set_retiring_grace(&mut self, caller: Address, blocks: u64) -> Result<()> {
        self.require_admin(caller)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.retiring_grace.write(blocks)?;
            self.emit(IEnclaveRegistry::RetiringGraceSet { blocks })?;
            Ok(())
        })
    }

    pub fn set_max_schema(&mut self, caller: Address, schema: u16) -> Result<()> {
        self.require_admin(caller)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.max_schema.write(schema)?;
            self.emit(IEnclaveRegistry::MaxSchemaSet { maxSchema: schema })?;
            Ok(())
        })
    }

    pub fn set_min_registration_version(&mut self, caller: Address, version: u8) -> Result<()> {
        self.require_admin(caller)?;
        if !(REGISTRATION_VERSION..=REGISTRATION_VERSION_BOUND).contains(&version) {
            return Err(EnclaveRegistryError::UnsupportedRegistrationVersion { version }.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.min_registration_version.write(version)?;
            self.emit(IEnclaveRegistry::MinRegistrationVersionSet { version })?;
            Ok(())
        })
    }

    pub fn initiate_admin_transfer(&mut self, caller: Address, new_admin: Address) -> Result<()> {
        self.require_admin(caller)?;
        if new_admin == Address::ZERO {
            return Err(EnclaveRegistryError::ZeroAddress.into());
        }
        let current = self.admin.read()?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.pending_admin.write(new_admin)?;
            self.emit(IEnclaveRegistry::AdminTransferInitiated {
                from: current,
                to: new_admin,
            })?;
            Ok(())
        })
    }

    pub fn accept_admin_transfer(&mut self, caller: Address) -> Result<()> {
        let pending = self.pending_admin.read()?;
        if pending == Address::ZERO || caller != pending {
            return Err(EnclaveRegistryError::NotAdmin.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.admin.write(pending)?;
            self.pending_admin.write(Address::ZERO)?;
            self.emit(IEnclaveRegistry::AdminTransferCompleted { newAdmin: pending })?;
            Ok(())
        })
    }

    // -----------------------------------------------------------------------
    // Views
    // -----------------------------------------------------------------------

    pub fn read_provider(&self, provider_id: B256) -> Result<ProviderPolicyRecord> {
        let entry = self.providers.entry(provider_id);
        Ok(ProviderPolicyRecord {
            provider_id,
            allowed: entry.allowed().read()?,
            require_release: entry.require_release().read()?,
            base_url_hash: entry.base_url_hash().read()?,
            environment_hash: entry.environment_hash().read()?,
        })
    }

    pub fn read_deployment_record(&self, enclave_pk: B256) -> Result<DeploymentRecord> {
        let entry = self.deployments.entry(enclave_pk);
        Ok(DeploymentRecord {
            enclave_pk,
            attester: entry.attester().read()?,
            rm_digest: entry.rm_digest().read()?,
            provider_id: entry.provider_id().read()?,
            base_url_hash: entry.base_url_hash().read()?,
            registered_at: entry.registered_at().read()?,
            retiring_until: entry.retiring_until().read()?,
            status: entry.status().read()?,
            tcb: entry.tcb().read()?,
            product: entry.product().read()?,
        })
    }

    pub fn get_deployment(&self, enclave_pk: B256) -> Result<IEnclaveRegistry::Deployment> {
        let d = self.read_deployment_record(enclave_pk)?;
        let status = match d.status {
            STATUS_ACTIVE => IEnclaveRegistry::DeploymentStatus::Active,
            STATUS_RETIRING => IEnclaveRegistry::DeploymentStatus::Retiring,
            _ => IEnclaveRegistry::DeploymentStatus::None,
        };
        Ok(IEnclaveRegistry::Deployment {
            attester: d.attester,
            rmDigest: d.rm_digest,
            providerId: d.provider_id,
            baseUrlHash: d.base_url_hash,
            registeredAt: d.registered_at,
            retiringUntil: d.retiring_until,
            status,
            tcb: d.tcb,
            product: d.product,
            measurements: self.deployment_measurements.list(&enclave_pk).read_all()?,
            chipIds: self.deployment_chip_ids.list(&enclave_pk).read_all()?,
        })
    }

    pub fn is_active(&self, enclave_pk: B256) -> Result<bool> {
        let d = self.read_deployment_record(enclave_pk)?;
        let block_number = self.storage.block_number()?;
        let live_status = d.status == STATUS_ACTIVE
            || (d.status == STATUS_RETIRING && block_number <= d.retiring_until);
        if !live_status {
            return Ok(false);
        }
        if !self.attester_is_active(d.attester)? || !self.attester_has_submit_su(d.attester)? {
            return Ok(false);
        }
        if !self.providers.entry(d.provider_id).allowed().read()? {
            return Ok(false);
        }
        let floor = self.tcb_floor.read(&d.product)?;
        if !tcb_at_least(d.tcb, floor) {
            return Ok(false);
        }
        let measurements = self.deployment_measurements.list(&enclave_pk).read_all()?;
        let mut has_allowed_measurement = false;
        for mh in measurements {
            if self.allowed_measurements.read(&mh)? {
                has_allowed_measurement = true;
                break;
            }
        }
        if !has_allowed_measurement {
            return Ok(false);
        }
        let chip_ids = self.deployment_chip_ids.list(&enclave_pk).read_all()?;
        for chip in chip_ids {
            if self.revoked_chips.read(&chip)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn is_active_for(
        &self,
        enclave_pk: B256,
        attester: Address,
        provider_id: B256,
    ) -> Result<bool> {
        let entry = self.deployments.entry(enclave_pk);
        if entry.attester().read()? != attester || entry.provider_id().read()? != provider_id {
            return Ok(false);
        }
        self.is_active(enclave_pk)
    }

    pub fn measurements_of(&self, enclave_pk: B256) -> Result<Vec<B256>> {
        self.deployment_measurements.list(&enclave_pk).read_all()
    }

    pub fn chip_ids_of(&self, enclave_pk: B256) -> Result<Vec<B256>> {
        self.deployment_chip_ids.list(&enclave_pk).read_all()
    }

    pub fn all_enclaves(&self, offset: U256, limit: U256) -> Result<Vec<B256>> {
        let len = self.all_enclaves.len()?;
        let n = u64::from(len);
        let Ok(offset_u64) = u64::try_from(offset) else {
            return Ok(Vec::new());
        };
        if offset_u64 >= n {
            return Ok(Vec::new());
        }
        let remaining = n - offset_u64;
        let count = u64::try_from(limit).unwrap_or(remaining).min(remaining);
        let start = offset_u64 as u32;
        let end = (offset_u64 + count) as u32;
        let mut out = Vec::with_capacity(count as usize);
        for idx in start..end {
            if let Some(pk) = self.all_enclaves.get(idx)? {
                out.push(pk);
            }
        }
        Ok(out)
    }

    pub fn enclave_count(&self) -> Result<U256> {
        Ok(U256::from(self.all_enclaves.len()?))
    }

    // -----------------------------------------------------------------------
    // Internal verification steps
    // -----------------------------------------------------------------------

    fn check_attester(
        &self,
        caller: Address,
        registration: &[u8],
    ) -> Result<(Address, B256, B256, Vec<B256>)> {
        if registration.len() < MIN_PAYLOAD + SIGNATURE_LEN {
            return Err(EnclaveRegistryError::MalformedRegistration.into());
        }
        let end = registration.len() - SIGNATURE_LEN;
        let payload = &registration[..end];
        let sig = &registration[end..];

        let r = &sig[0..32];
        let s = &sig[32..64];
        let v = sig[64];
        if (v != 27 && v != 28) || U256::from_be_slice(s) > SECP256K1_HALF_N {
            return Err(EnclaveRegistryError::InvalidRegistrationSignature.into());
        }

        let mut preimage = Vec::with_capacity(DST.len() + 1 + payload.len());
        preimage.extend_from_slice(DST);
        preimage.push(TAG_REGISTRATION);
        preimage.extend_from_slice(payload);
        let digest = keccak256(&preimage);

        let attester = recover_eth_address(digest.as_slice(), r, s, v)
            .ok_or(EnclaveRegistryError::InvalidRegistrationSignature)?;

        let version = payload[0];
        let min_version = self.min_registration_version.read()?;
        if (version != REGISTRATION_VERSION && version != REGISTRATION_VERSION_BOUND)
            || version < min_version
        {
            return Err(EnclaveRegistryError::UnsupportedRegistrationVersion { version }.into());
        }

        let enclave_pk = B256::from_slice(&payload[1..33]);
        let rm_digest = B256::from_slice(&payload[33..65]);
        let mut i = 65usize;

        let n = read_u32_le(payload, i) as usize;
        i += 4;
        if n > (payload.len() - i) / (4 + MEASUREMENT_LEN) {
            return Err(EnclaveRegistryError::MalformedRegistration.into());
        }
        let mut certified = Vec::with_capacity(n);
        for _ in 0..n {
            if read_u32_le(payload, i) as usize != MEASUREMENT_LEN {
                return Err(EnclaveRegistryError::MalformedRegistration.into());
            }
            i += 4;
            certified.push(keccak256(&payload[i..i + MEASUREMENT_LEN]));
            i += MEASUREMENT_LEN;
        }

        if i + 4 > payload.len() {
            return Err(EnclaveRegistryError::MalformedRegistration.into());
        }
        let m = read_u32_le(payload, i) as usize;
        i += 4;
        for _ in 0..m {
            if i + 4 > payload.len() {
                return Err(EnclaveRegistryError::MalformedRegistration.into());
            }
            let name_len = read_u32_le(payload, i) as usize;
            i = i
                .checked_add(4)
                .and_then(|v| v.checked_add(name_len))
                .and_then(|v| v.checked_add(4))
                .ok_or(EnclaveRegistryError::MalformedRegistration)?;
            if i > payload.len() {
                return Err(EnclaveRegistryError::MalformedRegistration.into());
            }
        }

        let tail = if version == REGISTRATION_VERSION_BOUND {
            33 + ENCLAVE_SIG_LEN
        } else {
            33
        };
        if i + tail != payload.len() {
            return Err(EnclaveRegistryError::MalformedRegistration.into());
        }
        let embedded_addr = address_of_compressed_secp256k1(&payload[i..i + 33])
            .ok_or(EnclaveRegistryError::AttesterKeyMismatch)?;
        if embedded_addr != attester {
            return Err(EnclaveRegistryError::AttesterKeyMismatch.into());
        }
        if caller != attester {
            return Err(EnclaveRegistryError::NotTheAttester.into());
        }
        if !self.attester_is_active(attester)? {
            return Err(EnclaveRegistryError::AttesterNotActive.into());
        }
        if !self.attester_has_submit_su(attester)? {
            return Err(EnclaveRegistryError::AttesterNotPermitted.into());
        }

        if version == REGISTRATION_VERSION_BOUND {
            let enclave_sig = &payload[i + 33..];
            let mut binding_msg = Vec::with_capacity(DST.len() + 1 + 20);
            binding_msg.extend_from_slice(DST);
            binding_msg.push(TAG_ATTESTER_BINDING);
            binding_msg.extend_from_slice(attester.as_slice());
            if !verify_ed25519_strict(enclave_pk, enclave_sig, &binding_msg) {
                return Err(EnclaveRegistryError::EnclaveDidNotBindTheAttester {
                    enclave_pk,
                    attester,
                }
                .into());
            }
        }

        Ok((attester, enclave_pk, rm_digest, certified))
    }

    fn verify_report(
        &self,
        enclave_pk: B256,
        rm_digest: B256,
        report: &[u8],
        vcek: &[u8],
        product: u8,
    ) -> Result<(B256, B256, u64, u8)> {
        let fields = verify_snp_report(report, vcek, product)
            .map_err(|_| EnclaveRegistryError::ReportRefused)?;
        let line = fields.product.id();

        let mut hasher = Sha512::new();
        hasher.update(DST);
        hasher.update([TAG_IDENTITY]);
        hasher.update(enclave_pk.as_slice());
        hasher.update(rm_digest.as_slice());
        let expected_binding = hasher.finalize();
        if fields.report_data.as_slice() != expected_binding.as_slice() {
            return Err(EnclaveRegistryError::IdentityBindingMismatch.into());
        }

        if (fields.policy & POLICY_DEBUG) != 0 {
            return Err(EnclaveRegistryError::DebugPolicy.into());
        }
        if (fields.policy & POLICY_MIGRATE_MA) != 0 {
            return Err(EnclaveRegistryError::MigrationPolicy.into());
        }
        let chip = keccak256(fields.chip_id);
        if self.revoked_chips.read(&chip)? {
            return Err(EnclaveRegistryError::ChipRevoked { chip_id_hash: chip }.into());
        }
        if fields.vmpl != 0 {
            return Err(EnclaveRegistryError::NotVmpl0.into());
        }
        let floor = self.tcb_floor.read(&line)?;
        if !tcb_at_least(fields.reported_tcb, floor) {
            return Err(EnclaveRegistryError::TcbBelowFloor {
                reported: fields.reported_tcb,
                floor,
            }
            .into());
        }
        let mh = keccak256(fields.measurement);
        Ok((mh, chip, fields.reported_tcb, line))
    }

    fn verify_live(
        &self,
        d: &DeploymentRecord,
        enclave_pk: B256,
        report: &[u8],
        vcek: &[u8],
        product: u8,
    ) -> Result<(B256, B256, u64)> {
        let (mh, chip, tcb, line) =
            self.verify_report(enclave_pk, d.rm_digest, report, vcek, product)?;
        if line != d.product {
            return Err(EnclaveRegistryError::ProductMismatch {
                reported: line,
                recorded: d.product,
            }
            .into());
        }
        Ok((mh, chip, tcb))
    }

    fn check_manifest(&self, manifest: &[u8], rm_digest: B256) -> Result<(B256, B256)> {
        let actual_digest: [u8; 32] = Sha256::digest(manifest).into();
        if actual_digest != *rm_digest {
            return Err(EnclaveRegistryError::ManifestMismatch.into());
        }
        if manifest.len() < 2 {
            return Err(EnclaveRegistryError::MalformedManifest.into());
        }
        let schema = u16::from_le_bytes([manifest[0], manifest[1]]);
        let max_schema = self.max_schema.read()?;
        if schema < MIN_SCHEMA || schema > max_schema {
            return Err(EnclaveRegistryError::UnsupportedManifestSchema { schema }.into());
        }

        let (provider, i) = read_len_prefixed(manifest, 2)?;
        let (profile, i) = read_len_prefixed(manifest, i)?;
        let (base_url, i) = read_len_prefixed(manifest, i)?;
        let (environment, _) = read_len_prefixed(manifest, i)?;

        let provider_id = keccak256(provider);
        let p = self.read_provider(provider_id)?;
        if !p.allowed {
            return Err(EnclaveRegistryError::ProviderNotAllowed { provider_id }.into());
        }
        let base_url_hash = keccak256(base_url);
        if base_url_hash != p.base_url_hash {
            return Err(EnclaveRegistryError::BaseUrlMismatch.into());
        }
        if p.environment_hash != B256::ZERO && keccak256(environment) != p.environment_hash {
            return Err(EnclaveRegistryError::EnvironmentNotAllowed.into());
        }
        if p.require_release && profile != b"release" {
            return Err(EnclaveRegistryError::NotReleaseBuild.into());
        }
        Ok((provider_id, base_url_hash))
    }

    #[allow(clippy::too_many_arguments)]
    fn create_record(
        &mut self,
        enclave_pk: B256,
        attester: Address,
        rm_digest: B256,
        provider_id: B256,
        base_url_hash: B256,
        measurement_hash: B256,
        block_number: u64,
    ) -> Result<()> {
        let entry = self.deployments.entry(enclave_pk);
        entry.attester().write(attester)?;
        entry.rm_digest().write(rm_digest)?;
        entry.provider_id().write(provider_id)?;
        entry.base_url_hash().write(base_url_hash)?;
        entry.registered_at().write(block_number)?;
        entry.retiring_until().write(0)?;
        entry.status().write(STATUS_ACTIVE)?;
        entry.tcb().write(0)?;
        entry.product().write(0)?;
        self.deployment_measurements
            .list(&enclave_pk)
            .push(measurement_hash)?;
        self.all_enclaves.push(enclave_pk)?;
        Ok(())
    }

    fn live_by_attester(&self, caller: Address, enclave_pk: B256) -> Result<DeploymentRecord> {
        let d = self.read_deployment_record(enclave_pk)?;
        if d.status != STATUS_ACTIVE && d.status != STATUS_RETIRING {
            return Err(EnclaveRegistryError::DeploymentNotLive { enclave_pk }.into());
        }
        if caller != d.attester {
            return Err(EnclaveRegistryError::NotTheAttester.into());
        }
        Ok(d)
    }

    fn absorb(
        &mut self,
        enclave_pk: B256,
        current_tcb: u64,
        chip: B256,
        reported_tcb: u64,
    ) -> Result<u64> {
        let chips = self.deployment_chip_ids.list(&enclave_pk).read_all()?;
        if !chips.contains(&chip) {
            if chips.len() >= MAX_ENTRIES {
                return Err(EnclaveRegistryError::TooManyEntries.into());
            }
            self.deployment_chip_ids.list(&enclave_pk).push(chip)?;
        }
        let next_tcb = if tcb_at_least(reported_tcb, current_tcb) {
            self.deployments
                .entry(enclave_pk)
                .tcb()
                .write(reported_tcb)?;
            reported_tcb
        } else {
            current_tcb
        };
        Ok(next_tcb)
    }
}

fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn read_len_prefixed(data: &[u8], offset: usize) -> Result<(&[u8], usize)> {
    if offset + 4 > data.len() {
        return Err(EnclaveRegistryError::MalformedManifest.into());
    }
    let len = read_u32_le(data, offset) as usize;
    let start = offset + 4;
    let next = start
        .checked_add(len)
        .ok_or(EnclaveRegistryError::MalformedManifest)?;
    if next > data.len() {
        return Err(EnclaveRegistryError::MalformedManifest.into());
    }
    Ok((&data[start..next], next))
}

fn address_of_compressed_secp256k1(pk: &[u8]) -> Option<Address> {
    if pk.len() != 33 || (pk[0] != 0x02 && pk[0] != 0x03) {
        return None;
    }
    let verifying_key = K256VerifyingKey::from_sec1_bytes(pk).ok()?;
    let uncompressed = verifying_key.to_encoded_point(false);
    let uncompressed_bytes = uncompressed.as_bytes();
    if uncompressed_bytes.len() != 65 || uncompressed_bytes[0] != 0x04 {
        return None;
    }
    let hash = keccak256(&uncompressed_bytes[1..]);
    Some(Address::from_slice(&hash[12..]))
}

fn recover_eth_address(prehash: &[u8], r: &[u8], s: &[u8], v: u8) -> Option<Address> {
    let mut rs = [0u8; 64];
    rs[..32].copy_from_slice(r);
    rs[32..].copy_from_slice(s);
    let sig = K256Signature::from_slice(&rs).ok()?;
    let recid = RecoveryId::from_byte(v.checked_sub(27)?)?;
    let verifying_key = K256VerifyingKey::recover_from_prehash(prehash, &sig, recid).ok()?;
    let uncompressed = verifying_key.to_encoded_point(false);
    let hash = keccak256(&uncompressed.as_bytes()[1..]);
    Some(Address::from_slice(&hash[12..]))
}
