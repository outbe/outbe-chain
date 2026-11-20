//! Host-side collateral acquisition compatibility boundary.

use crate::TransportError;
use outbe_primitives::tee_attestation_v1::DcapCollateralComponentV1;

/// Acquire canonical collateral using the extracted DCAP implementation.
pub fn acquire_dcap_collateral_v1(
    quote: &[u8],
) -> Result<Vec<DcapCollateralComponentV1>, TransportError> {
    outbe_dcap::host_collateral::acquire_dcap_collateral_v1(quote).map_err(Into::into)
}
