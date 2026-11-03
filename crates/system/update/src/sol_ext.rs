//! Outbound sub-call ABI surfaces: the owner precompile (`ICrosschainBridge`)
//! that runs a contract upgrade, and the upgrade function of our UUPS proxies.
//! This is NOT the precompile's own inbound ABI (`precompile::IUpdate`).

use alloy_sol_types::sol;

sol!("../../../contracts/shared/src/IUUPSUpgradeable.sol");

sol!(
    #![sol(alloy_sol_types = alloy_sol_types)]
    "../../../contracts/precompiles/src/ICrosschainBridge.sol"
);
