use alloy_primitives::B256;
use outbe_ocomp_protocol::SchemaLimits;
use outbe_primitives::error::Result;

use super::{authority::decode_authority, OcompProtocolAuthorityV1};
use crate::{errors::corruption, precompile::IOcompRegistry, schema::OcompRegistry};

impl OcompRegistry<'_> {
    /// Pins a fresh lineage to the current active bundle. Exact replay returns
    /// the existing pin and never silently follows a later active bundle.
    pub fn pin_lineage(&mut self, lineage: B256, limits: &SchemaLimits) -> Result<B256> {
        if lineage.is_zero() {
            return Err(corruption("OCOMP lineage id must be non-zero"));
        }
        let existing = self.lineage_bundle.read(&lineage)?;
        if !existing.is_zero() {
            if self.authority_by_bundle_hash(existing, limits)?.is_none() {
                return Err(corruption("OCOMP lineage references an unavailable bundle"));
            }
            return Ok(existing);
        }
        let active = self
            .active_authority(limits)?
            .ok_or_else(|| corruption("OCOMP Registry is not initialized"))?;
        let bundle_hash = active.request_profile.protocol_bundle_hash;
        let count = self.live_lineage_count.read(&bundle_hash)?;
        let next = count
            .checked_add(1)
            .ok_or_else(|| corruption("OCOMP live lineage count overflow"))?;
        let checkpoint = self.storage.checkpoint_guard();
        self.lineage_bundle.write(&lineage, bundle_hash)?;
        self.live_lineage_count.write(&bundle_hash, next)?;
        checkpoint.commit();
        Ok(bundle_hash)
    }

    /// Pins a retry/successor lineage to the exact bundle of its predecessor.
    /// The predecessor must still be live. Its absence is fatal, and the pin
    /// never uses the current active authority as a fallback.
    pub fn pin_inherited_lineage(
        &mut self,
        lineage: B256,
        predecessor_lineage: B256,
        limits: &SchemaLimits,
    ) -> Result<B256> {
        if lineage.is_zero() || predecessor_lineage.is_zero() || lineage == predecessor_lineage {
            return Err(corruption("invalid OCOMP inherited lineage binding"));
        }
        let inherited = self
            .resolve_lineage(predecessor_lineage)?
            .ok_or_else(|| corruption("OCOMP predecessor lineage is not pinned"))?;
        if self.authority_by_bundle_hash(inherited, limits)?.is_none() {
            return Err(corruption(
                "OCOMP predecessor lineage authority is unavailable",
            ));
        }
        let existing = self.lineage_bundle.read(&lineage)?;
        if !existing.is_zero() {
            return if existing == inherited {
                Ok(existing)
            } else {
                Err(corruption("OCOMP inherited lineage binding changed"))
            };
        }
        let count = self.live_lineage_count.read(&inherited)?;
        let next = count
            .checked_add(1)
            .ok_or_else(|| corruption("OCOMP live lineage count overflow"))?;
        let checkpoint = self.storage.checkpoint_guard();
        self.lineage_bundle.write(&lineage, inherited)?;
        self.live_lineage_count.write(&inherited, next)?;
        checkpoint.commit();
        Ok(inherited)
    }

    pub fn resolve_lineage(&self, lineage: B256) -> Result<Option<B256>> {
        let bundle_hash = self.lineage_bundle.read(&lineage)?;
        Ok((!bundle_hash.is_zero()).then_some(bundle_hash))
    }

    pub fn release_lineage(
        &mut self,
        lineage: B256,
        current_height: u64,
        limits: &SchemaLimits,
    ) -> Result<bool> {
        let Some(bundle_hash) = self.resolve_lineage(lineage)? else {
            return Ok(false);
        };
        let count = self.live_lineage_count.read(&bundle_hash)?;
        let next = count
            .checked_sub(1)
            .ok_or_else(|| corruption("OCOMP live lineage count underflow"))?;
        let checkpoint = self.storage.checkpoint_guard();
        self.lineage_bundle.get(&lineage).delete()?;
        self.live_lineage_count.write(&bundle_hash, next)?;
        if next == 0 {
            let retiring = self.retiring_authority.read()?;
            if !retiring.is_empty() {
                let authority = decode_authority(&retiring, limits)?;
                if authority.request_profile.protocol_bundle_hash == bundle_hash {
                    self.retention_until.write(
                        &bundle_hash,
                        retention_deadline(current_height, &authority)?,
                    )?;
                }
            }
        }
        checkpoint.commit();
        Ok(true)
    }

    pub fn try_retire_predecessor(
        &mut self,
        current_height: u64,
        limits: &SchemaLimits,
    ) -> Result<bool> {
        let bytes = self.retiring_authority.read()?;
        if bytes.is_empty() {
            return Ok(false);
        }
        if self.storage.block_number()? != current_height {
            return Err(corruption(
                "OCOMP retirement height does not match storage context",
            ));
        }
        let authority = decode_authority(&bytes, limits)?;
        let bundle_hash = authority.request_profile.protocol_bundle_hash;
        if self.live_lineage_count.read(&bundle_hash)? != 0 {
            return Ok(false);
        }
        let deadline = self.retention_until.read(&bundle_hash)?;
        if deadline == 0 || current_height < deadline {
            return Ok(false);
        }
        let checkpoint = self.storage.checkpoint_guard();
        self.retiring_authority.clear()?;
        self.retention_until.get(&bundle_hash).delete()?;
        self.emit(IOcompRegistry::OcompProtocolAuthorityRetired {
            protocolBundleHash: bundle_hash,
            retiredAt: current_height,
        })?;
        checkpoint.commit();
        Ok(true)
    }
}

pub(super) fn retention_deadline(
    current_height: u64,
    authority: &OcompProtocolAuthorityV1,
) -> Result<u64> {
    current_height
        .checked_add(
            authority
                .request_profile
                .capacity_profile
                .source_retention_after_terminal_blocks,
        )
        .ok_or_else(|| corruption("OCOMP predecessor retention height overflow"))
}
