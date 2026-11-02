//! Whitelist for pre-exec hook events published via the `HookEvents` system tx.

use alloy_primitives::{Address, Log};
use alloy_sol_types::SolEvent;

use crate::addresses::{STABLECOIN_FACTORY_ADDRESS, STAKING_ADDRESS, UPDATE_ADDRESS, VOTE_ADDRESS};

/// Contract addresses whose pre-exec hook events are copied into the mandatory
/// [`SystemTx::HookEvents`](crate::system_tx::SystemTx::HookEvents)
/// begin-zone system transaction receipt.
pub const HOOK_EVENT_RECEIPT_ADDRESSES: &[Address] = &[
    VOTE_ADDRESS,
    UPDATE_ADDRESS,
    STABLECOIN_FACTORY_ADDRESS,
    // Pre-exec retirement emits UnbondingStarted here. Rewards and NOD stay tracing-only.
    STAKING_ADDRESS,
];

/// Returns `true` when `address` is whitelisted for hook-event receipt publication.
pub fn is_hook_event_receipt_address(address: Address) -> bool {
    HOOK_EVENT_RECEIPT_ADDRESSES.contains(&address)
}

/// Splits hook events into receipt-visible (whitelisted) and tracing-only buckets.
pub fn partition_hook_events(events: &[Log]) -> (Vec<Log>, Vec<Log>) {
    let mut receipt_visible = Vec::new();
    let mut tracing_only = Vec::new();
    for event in events {
        // Only the opt-in registry events. Exposing every registry hook log
        // would change receipts when replaying historical policy activations.
        let tee_registry_hook_event = event.address == crate::addresses::TEE_REGISTRY_ADDRESS
            && matches!(
                event.data.topics().first().copied(),
                Some(
                    crate::tee_registry_abi_v1::ITeeRegistryV1::EnclaveUpgradeMissedV1::SIGNATURE_HASH
                        | crate::tee_registry_abi_v1::ITeeRegistryV1::EnclaveBindingRevokedV1::SIGNATURE_HASH
                )
            );
        if is_hook_event_receipt_address(event.address) || tee_registry_hook_event {
            receipt_visible.push(event.clone());
        } else {
            tracing_only.push(event.clone());
        }
    }
    (receipt_visible, tracing_only)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Bytes, Log, LogData, B256};

    use crate::addresses::{NOD_ADDRESS, REWARDS_ADDRESS, STAKING_ADDRESS};

    use super::*;

    fn log_at(address: Address) -> Log {
        Log {
            address,
            data: LogData::new(vec![B256::ZERO], Bytes::new()).expect("test log data"),
        }
    }

    #[test]
    fn partition_keeps_only_whitelisted_addresses_for_receipt() {
        use crate::addresses::TEE_REGISTRY_ADDRESS;
        use crate::tee_registry_abi_v1::ITeeRegistryV1;

        let upgrade_missed_log = Log {
            address: TEE_REGISTRY_ADDRESS,
            data: LogData::new(
                vec![ITeeRegistryV1::EnclaveUpgradeMissedV1::SIGNATURE_HASH],
                Bytes::new(),
            )
            .expect("test log data"),
        };
        let binding_revoked_log = Log {
            address: TEE_REGISTRY_ADDRESS,
            data: LogData::new(
                vec![ITeeRegistryV1::EnclaveBindingRevokedV1::SIGNATURE_HASH],
                Bytes::new(),
            )
            .expect("test log data"),
        };
        let (whitelisted, tracing_only) = partition_hook_events(&[
            log_at(REWARDS_ADDRESS),
            log_at(VOTE_ADDRESS),
            log_at(UPDATE_ADDRESS),
            log_at(NOD_ADDRESS),
            log_at(alloy_primitives::address!(
                "0x0000000000000000000000000000000000001018"
            )),
            log_at(STABLECOIN_FACTORY_ADDRESS),
            log_at(TEE_REGISTRY_ADDRESS),
            upgrade_missed_log.clone(),
            binding_revoked_log.clone(),
            log_at(STAKING_ADDRESS),
        ]);
        assert!(
            whitelisted.iter().any(|log| log.address == STAKING_ADDRESS),
            "staking address must be receipt-visible; tracing_only={}",
            tracing_only
                .iter()
                .any(|log| log.address == STAKING_ADDRESS)
        );
        assert_eq!(whitelisted.len(), 6);
        assert_eq!(whitelisted[0].address, VOTE_ADDRESS);
        assert_eq!(whitelisted[1].address, UPDATE_ADDRESS);
        assert_eq!(whitelisted[2].address, STABLECOIN_FACTORY_ADDRESS);
        assert_eq!(whitelisted[3], upgrade_missed_log);
        assert_eq!(whitelisted[4], binding_revoked_log);
        assert_eq!(whitelisted[5].address, STAKING_ADDRESS);
        // The rights' sweeps run in CycleTick, so a hook never emits for them.
        // TeeRegistry logs other than the opt-in hook events stay tracing-only.
        assert_eq!(tracing_only.len(), 4);
        assert_eq!(tracing_only[0].address, REWARDS_ADDRESS);
        assert_eq!(tracing_only[1].address, NOD_ADDRESS);
        assert_eq!(
            tracing_only[2].address,
            alloy_primitives::address!("0x0000000000000000000000000000000000001018")
        );
        assert_eq!(tracing_only[3].address, TEE_REGISTRY_ADDRESS);
    }
}
