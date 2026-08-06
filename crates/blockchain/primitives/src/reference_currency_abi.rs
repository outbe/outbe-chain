//! Canonical `IReferenceCurrency` ABI, mirroring
//! `contracts/tokens/src/interfaces/IReferenceCurrency.sol`.
//!
//! Asset tokens implement it so a caller can staticcall the token for its ISO
//! 4217 numeric code and use that code to resolve the oracle price
//! (`outbe_oracle::api::coen_rate_for`). One generated interface pins the
//! selector across every module that makes that sub-call.

use alloy_sol_types::sol;

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IReferenceCurrency {
        function isoCode() external view returns (uint16);
    }
}
