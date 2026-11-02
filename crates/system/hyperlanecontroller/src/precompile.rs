//! Inbound ABI of the precompile at the controller address. The
//! `crosschaingov` crate serves the address and dispatches these selectors.

use alloy_sol_types::sol;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IHyperlaneController.sol"
);
