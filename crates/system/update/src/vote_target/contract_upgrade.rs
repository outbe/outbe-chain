//! The upgrade of our UUPS contracts on the listed chains. It is
//! not part of the protocol update.
//!
//! The vote tally runs where sub-calls are not available. So an approved
//! proposal only stores the hash of its payload and enters a queue.
//! `applyApproved(proposalId, payload)` runs it later. The begin-block step of
//! Vote calls it when the execution delay ends, and anyone else may call it
//! too. It asks CrosschainBridge
//! (`0x…EE15`), the owner of the contracts, to call
//! `proxy.upgradeToAndCall(implementation, callData)` for each upgrade. On
//! Outbe that is one `callLocal` for each upgrade. Each other chain gets one
//! `callRemote` message with all its upgrades, through the Interchain Account.
//! This address is in the owner-caller list of CrosschainBridge.

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::addresses::{CROSSCHAIN_BRIDGE_ADDRESS, UPDATE_ADDRESS};
use outbe_system_common::vote_target::{decode_stored_proposal_payload, PayloadApprovals};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::UpdateError;
use crate::precompile::IUpdate;
use crate::schema::Update;
use crate::sol_ext::{ICrosschainBridge, IUUPSUpgradeable};

/// Vote operation that upgrades our UUPS contracts on one or more chains.
pub const UPGRADE_CONTRACTS_OPERATION: &str = "upgradeContracts";

/// One upgrade: the UUPS proxy `proxy` on `chain_id` moves to
/// `implementation`, then runs `call_data` (`proxy.upgradeToAndCall`). Empty
/// `call_data` is a plain upgrade. With the current implementation,
/// `call_data` runs an owner function, for example a configuration change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ContractUpgrade {
    pub chain_id: u32,
    pub proxy: Address,
    pub implementation: Address,
    pub call_data: Bytes,
}

impl ContractUpgrade {
    /// The chain, the proxy and the implementation are nonzero.
    fn has_fields(&self) -> bool {
        self.chain_id != 0 && !self.proxy.is_zero() && !self.implementation.is_zero()
    }
}

/// JSON payload that upgrades a batch of contracts on the listed chains in
/// one proposal. The upgrades on one chain run in the listed order.
/// `executionDelayBlocks` is the number of blocks between the approval and the
/// first block where `applyApproved` may run the batch.
///
/// ```json
/// {"operation":"upgradeContracts", "executionDelayBlocks":28800, "upgrades":[
///   {"chainId":11155111, "proxy":"0x…", "implementation":"0x…", "callData":"0x"},
///   {"chainId":97, "proxy":"0x…", "implementation":"0x…", "callData":"0x"}
/// ]}
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpgradeContractsPayload {
    pub operation: String,
    pub execution_delay_blocks: u64,
    pub upgrades: Vec<ContractUpgrade>,
}

impl UpgradeContractsPayload {
    pub fn from_value(payload: &Value) -> std::result::Result<Self, UpdateError> {
        let decoded: Self =
            serde_json::from_value(payload.clone()).map_err(|_| UpdateError::InvalidPayload)?;
        decoded.validate()?;
        Ok(decoded)
    }

    /// Requires the operation, at least one upgrade, nonzero fields, and each
    /// proxy at most once on a chain.
    pub fn validate(&self) -> std::result::Result<(), UpdateError> {
        let mut seen = std::collections::HashSet::new();
        let valid = self.operation == UPGRADE_CONTRACTS_OPERATION
            && !self.upgrades.is_empty()
            && self
                .upgrades
                .iter()
                .all(|u| u.has_fields() && seen.insert((u.chain_id, u.proxy)));
        if !valid {
            return Err(UpdateError::InvalidPayload);
        }
        Ok(())
    }
}

impl UpgradeContractsPayload {
    /// The calls to CrosschainBridge that run the upgrades: the `callLocal`
    /// calls first, then one `callRemote` for each other chain, in the order
    /// of its first upgrade. `outbe` is the chain id of this chain.
    /// `upgradeToAndCall` keeps `msg.sender`, so `call_data` can also call an
    /// owner function of the implementation.
    fn bridge_calls(&self, outbe: u64) -> Vec<Bytes> {
        let mut local = Vec::new();
        let mut remote: Vec<(u32, Vec<ICrosschainBridge::Call>)> = Vec::new();
        for upgrade in &self.upgrades {
            let call = ICrosschainBridge::Call {
                to: upgrade.proxy,
                value: U256::ZERO,
                data: IUUPSUpgradeable::upgradeToAndCallCall {
                    newImplementation: upgrade.implementation,
                    data: upgrade.call_data.clone(),
                }
                .abi_encode()
                .into(),
            };
            if u64::from(upgrade.chain_id) == outbe {
                local.push(
                    ICrosschainBridge::callLocalCall { call }
                        .abi_encode()
                        .into(),
                );
            } else if let Some((_, calls)) = remote.iter_mut().find(|(d, _)| *d == upgrade.chain_id)
            {
                calls.push(call);
            } else {
                remote.push((upgrade.chain_id, vec![call]));
            }
        }
        local.extend(remote.into_iter().map(|(domain, calls)| {
            ICrosschainBridge::callRemoteCall { domain, calls }
                .abi_encode()
                .into()
        }));
        local
    }
}

impl Update<'_> {
    /// Stores the hash of an approved `upgradeContracts` payload and its
    /// unlock block, and queues the proposal for the begin-block step.
    pub(crate) fn approve_contract_upgrade(
        &self,
        proposal_id: U256,
        payload: &[u8],
        unlock_height: u64,
    ) -> Result<()> {
        self.approved_upgrades
            .write(&proposal_id, keccak256(payload))?;
        self.upgrade_unlock_at.write(&proposal_id, unlock_height)?;
        self.queued_contract_upgrades.push(proposal_id)
    }

    /// Runs an approved batch once, inside one checkpoint: every upgrade is
    /// sent, or none. The batch runs only after the execution delay of its
    /// payload. A batch that waited longer than one voting window after that
    /// delay is dropped.
    pub fn apply_approved(&mut self, proposal_id: U256, payload: &[u8]) -> Result<()> {
        let approved = self.approved_upgrades.read(&proposal_id)?;
        if approved == B256::ZERO {
            return Err(UpdateError::UnknownApprovedUpgrade { proposal_id }.into());
        }
        if keccak256(payload) != approved {
            return Err(UpdateError::ApprovedPayloadMismatch { proposal_id }.into());
        }
        let unlock_height = self.upgrade_unlock_at.read(&proposal_id)?;
        if self.storage.block_number()? < unlock_height {
            return Err(UpdateError::ContractUpgradeLocked {
                proposal_id,
                unlock_height,
            }
            .into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            // Consume first. The checkpoint restores it if an upgrade fails.
            self.approved_upgrades.write(&proposal_id, B256::ZERO)?;
            if self.storage.block_number()?
                > outbe_chain_constants::approved_action_expiry(unlock_height)
            {
                return self.emit(IUpdate::ContractUpgradeExpired {
                    proposalId: proposal_id,
                });
            }
            let value = decode_stored_proposal_payload(payload, "Update")?;
            let batch = UpgradeContractsPayload::from_value(&value)
                .map_err(UpdateError::stored_proposal_payload_invalid)?;
            for call in batch.bridge_calls(self.storage.chain_id()?) {
                self.storage
                    .call(CROSSCHAIN_BRIDGE_ADDRESS, U256::ZERO, call)?;
            }
            self.emit(IUpdate::ContractUpgradeApplied {
                proposalId: proposal_id,
            })
        })
    }
}

/// Vote begin-block step: calls `applyApproved` for each queued batch whose
/// execution delay ended. The sub-call runs in the frame of Update, so Update
/// is the caller of CrosschainBridge.
pub fn apply_due_contract_upgrades(storage: &StorageHandle<'_>, block_number: u64) -> Result<()> {
    let update = Update::new(storage.clone());
    PayloadApprovals {
        target: UPDATE_ADDRESS,
        module: "Update",
        queue: &update.queued_contract_upgrades,
        hashes: &update.approved_upgrades,
        unlocks: &update.upgrade_unlock_at,
    }
    .apply_due(storage, block_number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::UpdateVotePayload;

    #[test]
    fn upgrade_contracts_payload_requires_the_operation_the_delay_and_valid_unique_upgrades() {
        let upgrade = |chain_id: u32, proxy: u8| {
            serde_json::json!({
                "chainId": chain_id,
                "proxy": format!("{:#x}", Address::repeat_byte(proxy)),
                "implementation": format!("{:#x}", Address::repeat_byte(2)),
                "callData": "0x",
            })
        };
        let valid = serde_json::json!({
            "operation": UPGRADE_CONTRACTS_OPERATION,
            "executionDelayBlocks": outbe_chain_constants::MIN_GOVERNANCE_EXECUTION_DELAY_BLOCKS,
            "upgrades": [upgrade(11_155_111, 1), upgrade(97, 1), upgrade(97, 3)],
        });
        assert!(matches!(
            UpdateVotePayload::from_value(&valid).unwrap(),
            UpdateVotePayload::UpgradeContracts(_)
        ));
        let zero = format!("{:#x}", Address::ZERO);
        let mut cases = Vec::new();
        for (field, value) in [
            ("operation", serde_json::json!("setBridgePaused")),
            ("executionDelayBlocks", serde_json::json!(49)),
            ("upgrades", serde_json::json!([])),
            (
                "upgrades",
                serde_json::json!([upgrade(97, 1), upgrade(97, 1)]),
            ),
        ] {
            let mut case = valid.clone();
            case[field] = value;
            cases.push(case);
        }
        let mut no_delay = valid.clone();
        no_delay
            .as_object_mut()
            .unwrap()
            .remove("executionDelayBlocks");
        cases.push(no_delay);
        for (field, value) in [
            ("chainId", serde_json::json!(0)),
            ("proxy", serde_json::json!(zero)),
            ("implementation", serde_json::json!(zero)),
            ("extra", serde_json::json!(true)),
        ] {
            let mut case = valid.clone();
            case["upgrades"][0][field] = value;
            cases.push(case);
        }
        let mut no_call_data = valid;
        no_call_data["upgrades"][0]
            .as_object_mut()
            .unwrap()
            .remove("callData");
        cases.push(no_call_data);
        for case in cases {
            assert_eq!(
                UpdateVotePayload::from_value(&case)
                    .and_then(|payload| payload.validate(0))
                    .unwrap_err(),
                UpdateError::InvalidPayload,
                "{case}"
            );
        }
    }
}
