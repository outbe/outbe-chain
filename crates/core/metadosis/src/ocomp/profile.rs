use alloy_primitives::B256;
use outbe_ocomp_protocol::SchemaLimits;
use outbe_primitives::error::Result;

pub use outbe_ocompregistry::{poc_schema_limits, OcompRequestProfile};

use crate::schema::MetadosisContract;

impl MetadosisContract<'_> {
    pub fn read_ocomp_request_profile(
        &self,
        limits: &SchemaLimits,
    ) -> Result<Option<OcompRequestProfile>> {
        Ok(
            outbe_ocompregistry::OcompRegistry::new(self.storage.clone())
                .active_authority(limits)?
                .map(|authority| authority.request_profile),
        )
    }

    pub(crate) fn read_ocomp_request_profile_for_bundle(
        &self,
        bundle_hash: B256,
        limits: &SchemaLimits,
    ) -> Result<Option<OcompRequestProfile>> {
        Ok(
            outbe_ocompregistry::OcompRegistry::new(self.storage.clone())
                .authority_by_bundle_hash(bundle_hash, limits)?
                .map(|authority| authority.request_profile),
        )
    }
}
