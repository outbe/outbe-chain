//! The upgrade of any of our UUPS contracts.

use crate::precompile::ICrosschainBridge::Call;
use alloy_primitives::{Address, Bytes};
use alloy_sol_types::{sol, SolCall};

use super::call;

sol! {
    /// Upgrade function of an OpenZeppelin `UUPSUpgradeable` proxy.
    interface IUUPSUpgradeable {
        function upgradeToAndCall(address newImplementation, bytes data) external payable;
    }
}

/// `proxy.upgradeToAndCall(implementation, data)`. `upgradeToAndCall` keeps
/// `msg.sender`, so `data` can also call an `onlyOwner` function of the
/// current implementation.
pub(super) fn upgrade(proxy: Address, implementation: Address, data: Bytes) -> Call {
    call(
        proxy,
        IUUPSUpgradeable::upgradeToAndCallCall {
            newImplementation: implementation,
            data,
        }
        .abi_encode(),
    )
}
