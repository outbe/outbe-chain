mod authority;
mod lineage;

pub(crate) use authority::validate_protocol_authority;
pub use authority::{OcompProtocolAuthorityV1, OcompSuccessorV1};

use authority::{
    decode_authority, encode_authority, protocol_error, validate_successor, CanonicalAuthorityParts,
};

use lineage::retention_deadline;

use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::{profile::ProtocolBundleV1, SchemaLimits, OCB1_HEADER_LEN};
use outbe_primitives::error::Result;

use crate::{
    errors::corruption, precompile::IOcompRegistry, profile::OcompRequestProfile,
    schema::OcompRegistry,
};

struct StoredAuthorityHeader {
    profile_len: usize,
    bundle_len: usize,
    bundle_hash: B256,
}

impl StoredAuthorityHeader {
    fn read(registry: &OcompRegistry<'_>) -> Result<Self> {
        Ok(Self {
            profile_len: registry.active_request_profile.len()?,
            bundle_len: registry.active_protocol_bundle.len()?,
            bundle_hash: registry.active_protocol_bundle_hash.read()?,
        })
    }

    fn is_empty(&self) -> bool {
        self.profile_len == 0 && self.bundle_len == 0 && self.bundle_hash.is_zero()
    }

    fn validate_complete(&self) -> Result<()> {
        if self.profile_len == 0 || self.bundle_len == 0 || self.bundle_hash.is_zero() {
            return Err(corruption("OCOMP Registry active authority is partial"));
        }
        Ok(())
    }

    fn validate_byte_cap(&self, limits: &SchemaLimits) -> Result<()> {
        let max = limits
            .codec
            .max_allocation_bytes
            .checked_add(OCB1_HEADER_LEN)
            .ok_or_else(|| corruption("OCOMP Registry authority byte cap overflow"))?;
        if self.profile_len > max || self.bundle_len > max {
            return Err(corruption(
                "OCOMP Registry active authority exceeds byte cap",
            ));
        }
        Ok(())
    }
}

impl OcompRegistry<'_> {
    pub fn initialize_genesis_authority(
        &mut self,
        authority: &OcompProtocolAuthorityV1,
        install_hash: B256,
        activation_height: u64,
        current_height: u64,
        limits: &SchemaLimits,
    ) -> Result<()> {
        self.validate_genesis_install_context(install_hash, activation_height, current_height)?;
        validate_protocol_authority(authority, limits)?;
        self.validate_chain_identity(&authority.request_profile)?;
        if self.is_genesis_authority_installed(
            authority,
            install_hash,
            activation_height,
            limits,
        )? {
            return Ok(());
        }
        let encoded = CanonicalAuthorityParts::encode(authority, limits)?;
        self.write_genesis_authority(authority, &encoded, install_hash, activation_height, limits)
    }

    fn validate_genesis_install_context(
        &self,
        install_hash: B256,
        activation_height: u64,
        current_height: u64,
    ) -> Result<()> {
        if activation_height == 0
            || current_height != activation_height
            || self.storage.block_number()? != current_height
        {
            return Err(corruption(
                "OCOMP Registry install attempted outside its activation height",
            ));
        }
        if install_hash.is_zero() {
            return Err(corruption("OCOMP Registry install hash must be non-zero"));
        }
        Ok(())
    }

    fn validate_chain_identity(&self, profile: &OcompRequestProfile) -> Result<()> {
        if self.storage.chain_id()? != profile.chain_id
            || self.storage.genesis_hash()? != profile.genesis_hash
        {
            return Err(corruption("OCOMP Registry chain identity mismatch"));
        }
        Ok(())
    }

    fn is_genesis_authority_installed(
        &self,
        authority: &OcompProtocolAuthorityV1,
        install_hash: B256,
        activation_height: u64,
        limits: &SchemaLimits,
    ) -> Result<bool> {
        match self.active_authority(limits)? {
            Some(existing) => {
                self.validate_genesis_replay(
                    &existing,
                    authority,
                    install_hash,
                    activation_height,
                )?;
                Ok(true)
            }
            None => {
                self.validate_empty_genesis_install()?;
                Ok(false)
            }
        }
    }

    fn validate_genesis_replay(
        &self,
        existing: &OcompProtocolAuthorityV1,
        authority: &OcompProtocolAuthorityV1,
        install_hash: B256,
        activation_height: u64,
    ) -> Result<()> {
        if existing != authority
            || self.install_hash.read()? != install_hash
            || self.activation_height.read()? != activation_height
        {
            return Err(corruption("OCOMP Registry genesis authority is immutable"));
        }
        Ok(())
    }

    fn validate_empty_genesis_install(&self) -> Result<()> {
        if !self.install_hash.read()?.is_zero() || self.activation_height.read()? != 0 {
            return Err(corruption("OCOMP Registry genesis authority is partial"));
        }
        Ok(())
    }

    fn write_genesis_authority(
        &mut self,
        authority: &OcompProtocolAuthorityV1,
        encoded: &CanonicalAuthorityParts,
        install_hash: B256,
        activation_height: u64,
        limits: &SchemaLimits,
    ) -> Result<()> {
        let bundle_hash = authority.request_profile.protocol_bundle_hash;
        let checkpoint = self.storage.checkpoint_guard();
        self.active_request_profile
            .write(&encoded.request_profile)?;
        self.active_protocol_bundle
            .write(&encoded.protocol_bundle)?;
        self.active_protocol_bundle_hash.write(bundle_hash)?;
        self.install_hash.write(install_hash)?;
        self.activation_height.write(activation_height)?;
        self.emit(IOcompRegistry::OcompProtocolAuthorityInstalled {
            protocolBundleHash: bundle_hash,
            installHash: install_hash,
            activationHeight: activation_height,
        })?;
        if self.active_authority(limits)? != Some(authority.clone()) {
            return Err(corruption("OCOMP Registry authority write/read mismatch"));
        }
        checkpoint.commit();
        Ok(())
    }

    pub fn active_authority(
        &self,
        limits: &SchemaLimits,
    ) -> Result<Option<OcompProtocolAuthorityV1>> {
        let header = StoredAuthorityHeader::read(self)?;
        if header.is_empty() {
            return Ok(None);
        }
        header.validate_complete()?;
        header.validate_byte_cap(limits)?;
        let request_profile =
            OcompRequestProfile::decode_canonical(&self.active_request_profile.read()?, limits)?;
        let protocol_bundle =
            ProtocolBundleV1::decode_canonical(&self.active_protocol_bundle.read()?, limits)
                .map_err(protocol_error)?;
        let authority = OcompProtocolAuthorityV1 {
            request_profile,
            protocol_bundle,
        };
        validate_protocol_authority(&authority, limits)?;
        if authority.request_profile.protocol_bundle_hash != header.bundle_hash {
            return Err(corruption(
                "OCOMP Registry active bundle hash slot is inconsistent",
            ));
        }
        Ok(Some(authority))
    }

    pub fn staged_successor(
        &self,
        limits: &SchemaLimits,
    ) -> Result<Option<(U256, OcompSuccessorV1)>> {
        let bytes = self.staged_successor.read()?;
        let proposal_id = self.staged_proposal_id.read()?;
        match (bytes.is_empty(), proposal_id.is_zero()) {
            (true, true) => Ok(None),
            (false, false) => Ok(Some((
                proposal_id,
                OcompSuccessorV1::decode_canonical(&bytes, limits)?,
            ))),
            _ => Err(corruption("OCOMP Registry staged successor is partial")),
        }
    }

    pub fn stage_successor(
        &mut self,
        proposal_id: U256,
        successor: &OcompSuccessorV1,
        limits: &SchemaLimits,
    ) -> Result<()> {
        if proposal_id.is_zero() {
            return Err(corruption("OCOMP successor proposal id must be non-zero"));
        }
        let current_height = self.storage.block_number()?;
        let active = self
            .active_authority(limits)?
            .ok_or_else(|| corruption("OCOMP Registry is not initialized"))?;
        validate_successor(&active, successor, current_height, limits)?;
        if !self.retiring_authority.is_empty()? {
            return Err(corruption(
                "OCOMP predecessor retirement must finish before staging another successor",
            ));
        }
        let encoded = successor.encode_canonical(limits)?;
        if let Some((stored_id, stored)) = self.staged_successor(limits)? {
            if stored_id == proposal_id && stored == *successor {
                return Ok(());
            }
            return Err(corruption("another OCOMP successor is already staged"));
        }
        let checkpoint = self.storage.checkpoint_guard();
        self.staged_successor.write(&encoded)?;
        self.staged_proposal_id.write(proposal_id)?;
        self.emit(IOcompRegistry::OcompSuccessorStaged {
            proposalId: proposal_id,
            protocolBundleHash: successor.authority.request_profile.protocol_bundle_hash,
            activationHeight: successor.activation_height,
        })?;
        checkpoint.commit();
        Ok(())
    }

    pub fn discard_staged_successor(&mut self, proposal_id: U256) -> Result<()> {
        let Some((stored_id, _)) = self.staged_successor(&poc_limits())? else {
            return Ok(());
        };
        if stored_id != proposal_id {
            return Err(corruption(
                "cannot discard another Update proposal's OCOMP successor",
            ));
        }
        self.staged_successor.clear()?;
        self.staged_proposal_id.delete()
    }

    pub fn promote_staged_successor(
        &mut self,
        proposal_id: U256,
        current_height: u64,
        limits: &SchemaLimits,
    ) -> Result<()> {
        if self.storage.block_number()? != current_height {
            return Err(corruption(
                "OCOMP successor activation height does not match storage context",
            ));
        }
        let Some((stored_id, successor)) = self.staged_successor(limits)? else {
            return Err(corruption("OCOMP successor is not staged"));
        };
        if stored_id != proposal_id || successor.activation_height != current_height {
            return Err(corruption(
                "OCOMP successor activation proposal or height mismatch",
            ));
        }
        if !self.retiring_authority.is_empty()? {
            return Err(corruption(
                "OCOMP Registry already has a retiring predecessor",
            ));
        }
        let active = self
            .active_authority(limits)?
            .ok_or_else(|| corruption("OCOMP Registry is not initialized"))?;
        validate_successor(
            &active,
            &successor,
            current_height.saturating_sub(1),
            limits,
        )?;
        let old_hash = active.request_profile.protocol_bundle_hash;
        let new_hash = successor.authority.request_profile.protocol_bundle_hash;
        let old_encoded = encode_authority(&active, limits)?;
        let encoded = CanonicalAuthorityParts::encode(&successor.authority, limits)?;
        let checkpoint = self.storage.checkpoint_guard();
        self.write_successor_slots(&old_encoded, &encoded, new_hash, current_height)?;
        if self.live_lineage_count.read(&old_hash)? == 0 {
            self.retention_until
                .write(&old_hash, retention_deadline(current_height, &active)?)?;
        }
        self.emit(IOcompRegistry::OcompSuccessorActivated {
            proposalId: proposal_id,
            predecessorProtocolBundleHash: old_hash,
            protocolBundleHash: new_hash,
            activationHeight: current_height,
        })?;
        checkpoint.commit();
        Ok(())
    }

    fn write_successor_slots(
        &self,
        old_encoded: &[u8],
        encoded: &CanonicalAuthorityParts,
        new_hash: B256,
        current_height: u64,
    ) -> Result<()> {
        self.retiring_authority.write(old_encoded)?;
        self.active_request_profile
            .write(&encoded.request_profile)?;
        self.active_protocol_bundle
            .write(&encoded.protocol_bundle)?;
        self.active_protocol_bundle_hash.write(new_hash)?;
        self.activation_height.write(current_height)?;
        self.staged_successor.clear()?;
        self.staged_proposal_id.delete()
    }

    pub fn authority_by_bundle_hash(
        &self,
        bundle_hash: B256,
        limits: &SchemaLimits,
    ) -> Result<Option<OcompProtocolAuthorityV1>> {
        if let Some(active) = self.active_authority(limits)? {
            if active.request_profile.protocol_bundle_hash == bundle_hash {
                return Ok(Some(active));
            }
        }
        let retiring = self.retiring_authority.read()?;
        if retiring.is_empty() {
            return Ok(None);
        }
        let authority = decode_authority(&retiring, limits)?;
        if authority.request_profile.protocol_bundle_hash == bundle_hash {
            Ok(Some(authority))
        } else {
            Ok(None)
        }
    }

    pub fn retiring_authority(
        &self,
        limits: &SchemaLimits,
    ) -> Result<Option<OcompProtocolAuthorityV1>> {
        let encoded = self.retiring_authority.read()?;
        if encoded.is_empty() {
            Ok(None)
        } else {
            decode_authority(&encoded, limits).map(Some)
        }
    }
}

fn poc_limits() -> SchemaLimits {
    crate::profile::poc_schema_limits()
}
