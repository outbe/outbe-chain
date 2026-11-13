//! The upgrade of our UUPS contracts on the listed chains. It is
//! not part of the protocol update.
//!
//! The vote tally runs where sub-calls are not available. So an approved
//! proposal only stores the hash of its payload. Permissionless
//! `applyApproved(proposalId, payload)` runs it later: it asks CrosschainBridge
//! (`0x…EE15`), the owner of the contracts, to call
//! `proxy.upgradeToAndCall(implementation, callData)` for each upgrade. On
//! Outbe that is one `callLocal` for each upgrade. Each other chain gets one
//! `callRemote` message with all its upgrades, through the Interchain Account.
//! This address is in the owner-caller list of CrosschainBridge.
//!
//! A batch runs no earlier than one governance voting window after its
//! proposal (OIP-00382). A short voting window makes the vote faster, not the
//! upgrade.

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::addresses::CROSSCHAIN_BRIDGE_ADDRESS;
use outbe_primitives::error::Result;
use outbe_system_common::vote_target::{decode_stored_proposal_payload, read_vote_proposal};
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
///
/// ```json
/// {"operation":"upgradeContracts", "upgrades":[
///   {"chainId":11155111, "proxy":"0x…", "implementation":"0x…", "callData":"0x"},
///   {"chainId":97, "proxy":"0x…", "implementation":"0x…", "callData":"0x"}
/// ]}
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeContractsPayload {
    pub operation: String,
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
    /// Stores the hash of an approved `upgradeContracts` payload.
    pub(crate) fn approve_contract_upgrade(
        &self,
        proposal_id: U256,
        payload: &[u8],
        block_number: u64,
    ) -> Result<()> {
        self.approved_upgrades
            .write(&proposal_id, keccak256(payload))?;
        self.approved_upgrade_at.write(&proposal_id, block_number)
    }

    /// Runs an approved batch once, inside one checkpoint: every upgrade is
    /// sent, or none. The batch runs no earlier than its unlock height (see
    /// `contract_upgrade_unlock_height`). Before that height the call
    /// reverts and the approval stays. A batch that waited longer than the
    /// voting window after its unlock height is dropped.
    pub fn apply_approved(&mut self, proposal_id: U256, payload: &[u8]) -> Result<()> {
        let approved = self.approved_upgrades.read(&proposal_id)?;
        if approved == B256::ZERO {
            return Err(UpdateError::UnknownApprovedUpgrade { proposal_id }.into());
        }
        if keccak256(payload) != approved {
            return Err(UpdateError::ApprovedPayloadMismatch { proposal_id }.into());
        }
        let window = outbe_chain_constants::get_governance_voting_window_blocks();
        let unlock_height = self.contract_upgrade_unlock_height(proposal_id, window)?;
        let block_number = self.storage.block_number()?;
        if block_number < unlock_height {
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
            if block_number > unlock_height.saturating_add(window) {
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

    /// The first block at which an approved batch can run:
    /// `max(approved_at, created_height + window)`. A proposal with a short
    /// voting window gets a faster vote, but its upgrade still waits one full
    /// `window` after the proposal.
    fn contract_upgrade_unlock_height(&self, proposal_id: U256, window: u64) -> Result<u64> {
        let proposal = read_vote_proposal(self.storage.clone(), proposal_id)?.ok_or_else(|| {
            UpdateError::VoteTargetInvariantFailure(format!(
                "approved contract upgrade {proposal_id} has no Vote proposal record"
            ))
        })?;
        let approved_at = self.approved_upgrade_at.read(&proposal_id)?;
        Ok(approved_at.max(proposal.created_height.saturating_add(window)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::UpdateVotePayload;

    #[test]
    fn upgrade_contracts_payload_requires_the_operation_and_valid_unique_upgrades() {
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
            "upgrades": [upgrade(11_155_111, 1), upgrade(97, 1), upgrade(97, 3)],
        });
        assert!(matches!(
            UpdateVotePayload::from_value(&valid).unwrap(),
            UpdateVotePayload::UpgradeContracts(_)
        ));
        let zero = format!("{:#x}", Address::ZERO);
        let mut cases = vec![
            serde_json::json!({"operation": "setBridgePaused", "upgrades": [upgrade(97, 1)]}),
            serde_json::json!({"operation": UPGRADE_CONTRACTS_OPERATION, "upgrades": []}),
            serde_json::json!({
                "operation": UPGRADE_CONTRACTS_OPERATION,
                "upgrades": [upgrade(97, 1), upgrade(97, 1)],
            }),
        ];
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
                UpdateVotePayload::from_value(&case).unwrap_err(),
                UpdateError::InvalidPayload,
                "{case}"
            );
        }
    }
}
