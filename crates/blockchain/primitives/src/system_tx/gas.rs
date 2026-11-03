use alloy_primitives::Bytes;

use crate::protocol_schedule::OutbeProtocolSchedule;

use super::{
    SystemTx, SystemTxError, SYSTEM_TX_NON_ZERO_BYTE_GAS, SYSTEM_TX_VISIBLE_GAS_FLOOR,
    SYSTEM_TX_ZERO_BYTE_GAS,
};

/// Invalid header capacity or accounted gas under the end-block reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BlockGasBudgetError {
    #[error("block gas limit {block_gas_limit} is below end-block reservation {reserved_gas}")]
    BlockGasLimitBelowReserve {
        block_gas_limit: u64,
        reserved_gas: u64,
    },
    #[error("accounted pre-end gas {consumed_gas} exceeds capacity {pre_end_capacity}")]
    PreEndGasExceeded {
        consumed_gas: u64,
        pre_end_capacity: u64,
    },
}

/// Height-aware gas capacity without changing the header or EVM gas limit.
///
/// All values use `u64`, bounded by the header gas limit and matching executor
/// counters. Genesis and the block-1 bootstrap do not reserve end-block gas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockGasBudget {
    block_gas_limit: u64,
    reserved_gas: u64,
    pre_end_capacity: u64,
}

impl BlockGasBudget {
    pub fn new(block_number: u64, block_gas_limit: u64) -> Result<Self, BlockGasBudgetError> {
        let reserved_gas = if block_number >= 2 {
            OutbeProtocolSchedule::END_BLOCK_RESERVED_GAS
        } else {
            0
        };
        let pre_end_capacity = block_gas_limit.checked_sub(reserved_gas).ok_or(
            BlockGasBudgetError::BlockGasLimitBelowReserve {
                block_gas_limit,
                reserved_gas,
            },
        )?;
        Ok(Self {
            block_gas_limit,
            reserved_gas,
            pre_end_capacity,
        })
    }

    #[must_use]
    pub const fn reserved_gas(&self) -> u64 {
        self.reserved_gas
    }

    #[must_use]
    pub const fn pre_end_capacity(&self) -> u64 {
        self.pre_end_capacity
    }

    /// Remaining admission capacity after authoritative committed accounting.
    pub fn remaining_pre_end_gas(&self, consumed_gas: u64) -> Result<u64, BlockGasBudgetError> {
        self.pre_end_capacity.checked_sub(consumed_gas).ok_or(
            BlockGasBudgetError::PreEndGasExceeded {
                consumed_gas,
                pre_end_capacity: self.pre_end_capacity,
            },
        )
    }

    /// Reserved minimum plus all unused earlier capacity, not consumed gas.
    pub fn end_capacity(&self, consumed_gas: u64) -> Result<u64, BlockGasBudgetError> {
        self.remaining_pre_end_gas(consumed_gas)?;
        self.block_gas_limit.checked_sub(consumed_gas).ok_or(
            BlockGasBudgetError::PreEndGasExceeded {
                consumed_gas,
                pre_end_capacity: self.pre_end_capacity,
            },
        )
    }
}

/// Ethereum-compatible visible gas limit for a system tx envelope.
///
/// Outbe executes the system precompile internally with
/// [`SYSTEM_TX_ARTIFACT_GAS_LIMIT`](super::SYSTEM_TX_ARTIFACT_GAS_LIMIT).
/// The signed transaction stored in the block body only needs to be valid as an
/// Ethereum legacy envelope. Charging intrinsic calldata gas keeps system txs visible to
/// generic replay/import tooling without exposing the 10B internal lane.
pub fn system_tx_intrinsic_gas(calldata: &[u8]) -> Result<u64, SystemTxError> {
    calldata
        .iter()
        .try_fold(SYSTEM_TX_VISIBLE_GAS_FLOOR, |gas, byte| {
            let byte_gas = if *byte == 0 {
                SYSTEM_TX_ZERO_BYTE_GAS
            } else {
                SYSTEM_TX_NON_ZERO_BYTE_GAS
            };
            gas.checked_add(byte_gas)
        })
        .ok_or(SystemTxError::VisibleGasOverflow {
            len: calldata.len(),
        })
}

/// Backwards-compatible name for the intrinsic gas of a system envelope.
pub fn system_tx_visible_gas_limit(calldata: &[u8]) -> Result<u64, SystemTxError> {
    system_tx_intrinsic_gas(calldata)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SystemTxVisibleGasEntry {
    intrinsic_gas: u64,
    protocol_precharge: u64,
    gas_limit: u64,
}

/// Deterministic visible-gas allocation for the complete begin-system zone.
///
/// System envelopes reserve intrinsic gas plus any schedule-hashed protocol
/// precharge. `CycleTick` receives the remaining pre-end capacity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemTxVisibleGasPlan {
    entries: Vec<SystemTxVisibleGasEntry>,
    total_envelope_gas: u64,
}

impl SystemTxVisibleGasPlan {
    pub fn new(
        block_number: u64,
        block_gas_limit: u64,
        system_txs: &[Bytes],
    ) -> Result<Self, SystemTxError> {
        let budget = BlockGasBudget::new(block_number, block_gas_limit)?;
        let mut entries = Vec::with_capacity(system_txs.len());
        let mut required_total = 0u64;
        let mut cycle_ordinal = None;

        for (ordinal, calldata) in system_txs.iter().enumerate() {
            let decoded = SystemTx::decode(calldata)?;
            if matches!(decoded, SystemTx::CycleTick) && cycle_ordinal.replace(ordinal).is_some() {
                return Err(SystemTxError::DuplicateCycleTickGasBudget);
            }
            let intrinsic_gas = system_tx_intrinsic_gas(calldata)?;
            let protocol_precharge = system_tx_protocol_precharge(&decoded)?;
            let gas_limit = intrinsic_gas
                .checked_add(protocol_precharge)
                .ok_or(SystemTxError::VisibleGasPlanOverflow)?;
            required_total = required_total
                .checked_add(gas_limit)
                .ok_or(SystemTxError::VisibleGasPlanOverflow)?;
            entries.push(SystemTxVisibleGasEntry {
                intrinsic_gas,
                protocol_precharge,
                gas_limit,
            });
        }

        let remainder = budget.remaining_pre_end_gas(required_total)?;
        let total_envelope_gas = if let Some(ordinal) = cycle_ordinal {
            let entry = entries
                .get_mut(ordinal)
                .ok_or(SystemTxError::DuplicateCycleTickGasBudget)?;
            entry.gas_limit = entry
                .gas_limit
                .checked_add(remainder)
                .ok_or(SystemTxError::VisibleGasPlanOverflow)?;
            budget.pre_end_capacity()
        } else {
            required_total
        };

        Ok(Self {
            entries,
            total_envelope_gas,
        })
    }

    #[must_use]
    pub fn intrinsic_gas(&self, ordinal: usize) -> Option<u64> {
        self.entries.get(ordinal).map(|entry| entry.intrinsic_gas)
    }

    #[must_use]
    pub fn gas_limit(&self, ordinal: usize) -> Option<u64> {
        self.entries.get(ordinal).map(|entry| entry.gas_limit)
    }

    #[must_use]
    pub fn protocol_precharge(&self, ordinal: usize) -> Option<u64> {
        self.entries
            .get(ordinal)
            .map(|entry| entry.protocol_precharge)
    }

    pub fn execution_gas_limit(&self, ordinal: usize) -> Result<Option<u64>, SystemTxError> {
        let Some(entry) = self.entries.get(ordinal) else {
            return Ok(None);
        };
        let required_gas = entry
            .intrinsic_gas
            .checked_add(entry.protocol_precharge)
            .ok_or(SystemTxError::GasLimitBelowIntrinsic {
                gas_limit: entry.gas_limit,
                intrinsic_gas: u64::MAX,
            })?;
        let execution_gas = entry
            .gas_limit
            .checked_sub(entry.intrinsic_gas)
            .and_then(|remaining| remaining.checked_sub(entry.protocol_precharge))
            .ok_or(SystemTxError::GasLimitBelowIntrinsic {
                gas_limit: entry.gas_limit,
                intrinsic_gas: required_gas,
            })?;
        Ok(Some(execution_gas))
    }

    #[must_use]
    pub const fn total_envelope_gas(&self) -> u64 {
        self.total_envelope_gas
    }
}

fn system_tx_protocol_precharge(input: &SystemTx) -> Result<u64, SystemTxError> {
    let SystemTx::TeeBootstrap { payload } = input else {
        return Ok(0);
    };
    payload
        .protocol_precharge(
            &crate::tee_attestation_v1::SystemGasScheduleV1::normative(),
            &crate::tee_attestation_v1::TeeRegistryGasScheduleV1::normative(),
        )
        .map_err(SystemTxError::codec)
}

#[cfg(test)]
mod tests {
    use super::{SystemTxVisibleGasEntry, SystemTxVisibleGasPlan};
    use crate::system_tx::SystemTxError;

    #[test]
    fn gas_plan_subtraction_is_checked() {
        let plan = SystemTxVisibleGasPlan {
            entries: vec![SystemTxVisibleGasEntry {
                intrinsic_gas: 80,
                protocol_precharge: 30,
                gas_limit: 100,
            }],
            total_envelope_gas: 100,
        };

        assert_eq!(
            plan.execution_gas_limit(0),
            Err(SystemTxError::GasLimitBelowIntrinsic {
                gas_limit: 100,
                intrinsic_gas: 110,
            })
        );
    }
}
