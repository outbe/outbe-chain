//! Inbound ABI of the CrosschainGov precompile at `0x…EE14`.
//!
//! The selectors, events and errors of this module and of `outbe-crosschaingov`
//! share one Solidity interface. This module emits the Hyperlane events and
//! errors, so the binding lives here.

use alloy_sol_types::sol;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/ICrosschainGov.sol"
);
