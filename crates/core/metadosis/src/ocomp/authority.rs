use outbe_primitives::error::{PrecompileError, Result};

use crate::{constants::MAX_RETAINED_WWDS, schema::MetadosisContract};

use super::{
    activation::validate_activation_authority,
    profile::{poc_schema_limits, OcompRequestProfile},
};

pub(crate) fn require_active_ocomp_profile(
    metadosis: &MetadosisContract<'_>,
) -> Result<OcompRequestProfile> {
    let limits = poc_schema_limits();
    let profile = metadosis
        .read_ocomp_request_profile(&limits)?
        .ok_or_else(|| fatal("fresh-devnet Metadosis requires a genesis-active OCOMP profile"))?;
    let authority = metadosis
        .read_ocomp_activation_authority(&limits)?
        .ok_or_else(|| {
            fatal("fresh-devnet Metadosis requires complete OCOMP activation authority")
        })?;
    validate_activation_authority(
        &profile,
        &authority.bundle,
        &authority.result_committee,
        &limits,
    )?;
    if usize::from(profile.capacity_profile.max_pending_jobs) != MAX_RETAINED_WWDS {
        return Err(fatal(
            "genesis OCOMP retained capacity differs from Metadosis derived bound",
        ));
    }
    Ok(profile)
}

fn fatal(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Fatal(message.into())
}
