//! Actions that change any of our UUPS contracts in the same way.

use alloy_primitives::{Address, Bytes};
use alloy_sol_types::SolCall;
use outbe_hyperlanecontroller::RemoteCall;
use serde::Deserialize;

use super::{Calls, Operation};
use crate::sol_ext::IUUPSUpgradeable;

/// Upgrades the UUPS `proxy` on `chain_id` to `implementation`:
/// `proxy.upgradeToAndCall(implementation, call_data)`.
///
/// An empty `call_data` is a plain upgrade. Otherwise it can call a
/// `reinitializer` of the new implementation. The current implementation with
/// the call data of an admin function changes the configuration of a
/// contract that has no typed action. `upgradeToAndCall` keeps
/// `msg.sender`, so the `onlyOwner` check of that function passes.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpgradeContract {
    pub chain_id: u32,
    pub proxy: Address,
    pub implementation: Address,
    pub call_data: Bytes,
}

impl Operation for UpgradeContract {
    fn is_valid(&self, _outbe: u64) -> bool {
        self.chain_id != 0 && !self.proxy.is_zero() && !self.implementation.is_zero()
    }

    fn calls(&self, outbe: u32) -> Calls {
        let upgrade = IUUPSUpgradeable::upgradeToAndCallCall {
            newImplementation: self.implementation,
            data: self.call_data.clone(),
        }
        .abi_encode();
        Calls::on(
            self.chain_id,
            outbe,
            vec![RemoteCall::new(self.proxy, upgrade)],
        )
    }
}
