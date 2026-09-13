use alloy_primitives::Bytes;

use super::{
    SystemTxError, SystemTxInputV2, SystemTxKind, OCOMP_LIFECYCLE_CE_GAS_RESERVE,
    SYSTEM_TX_NON_ZERO_BYTE_GAS, SYSTEM_TX_VISIBLE_GAS_FLOOR, SYSTEM_TX_ZERO_BYTE_GAS,
};

/// Ethereum-compatible visible gas limit for a system tx envelope.
///
/// Outbe executes the system precompile with
/// [`SYSTEM_TX_ARTIFACT_GAS_LIMIT`](super::SYSTEM_TX_ARTIFACT_GAS_LIMIT) internally, but the signed transaction
/// stored in the block body only needs to be valid as an Ethereum legacy
/// envelope. Charging intrinsic calldata gas keeps system txs visible to
/// generic replay/import tooling without exposing the 100M internal lane.
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
/// precharge and phase-specific compressed-entity budget. `CycleTick` receives
/// the remaining block gas while preserving every mandatory phase reserve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemTxVisibleGasPlan {
    entries: Vec<SystemTxVisibleGasEntry>,
    total_envelope_gas: u64,
}

impl SystemTxVisibleGasPlan {
    pub fn new(
        block_gas_limit: u64,
        system_txs: &[(SystemTxKind, Bytes)],
    ) -> Result<Self, SystemTxError> {
        let mut entries = Vec::with_capacity(system_txs.len());
        let mut required_total = 0u64;
        let mut cycle_ordinal = None;

        for (ordinal, (kind, calldata)) in system_txs.iter().enumerate() {
            let decoded = SystemTxInputV2::decode(calldata)?;
            let actual = decoded.kind();
            if actual != *kind {
                return Err(SystemTxError::CalldataKindMismatch {
                    expected: *kind,
                    actual,
                });
            }
            if *kind == SystemTxKind::CycleTick && cycle_ordinal.replace(ordinal).is_some() {
                return Err(SystemTxError::DuplicateCycleTickGasBudget);
            }
            let intrinsic_gas = system_tx_intrinsic_gas(calldata)?;
            let protocol_precharge = system_tx_protocol_precharge(&decoded)?;
            let ce_gas_reserve = system_tx_ce_gas_reserve(*kind);
            let gas_limit = intrinsic_gas
                .checked_add(protocol_precharge)
                .and_then(|gas| gas.checked_add(ce_gas_reserve))
                .ok_or(SystemTxError::VisibleGasPlanExceedsBlock {
                    required_gas: u64::MAX,
                    block_gas_limit,
                })?;
            required_total = required_total.checked_add(gas_limit).ok_or(
                SystemTxError::VisibleGasPlanExceedsBlock {
                    required_gas: u64::MAX,
                    block_gas_limit,
                },
            )?;
            entries.push(SystemTxVisibleGasEntry {
                intrinsic_gas,
                protocol_precharge,
                gas_limit,
            });
        }

        let remainder = block_gas_limit.checked_sub(required_total).ok_or(
            SystemTxError::VisibleGasPlanExceedsBlock {
                required_gas: required_total,
                block_gas_limit,
            },
        )?;
        let total_envelope_gas = if let Some(ordinal) = cycle_ordinal {
            let entry = entries
                .get_mut(ordinal)
                .ok_or(SystemTxError::DuplicateCycleTickGasBudget)?;
            entry.gas_limit = entry.gas_limit.checked_add(remainder).ok_or(
                SystemTxError::VisibleGasPlanExceedsBlock {
                    required_gas: required_total,
                    block_gas_limit,
                },
            )?;
            block_gas_limit
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

    #[must_use]
    pub fn ce_gas_limit(&self, ordinal: usize) -> Option<u64> {
        self.entries
            .get(ordinal)
            .map(|entry| entry.gas_limit - entry.intrinsic_gas - entry.protocol_precharge)
    }

    #[must_use]
    pub const fn total_envelope_gas(&self) -> u64 {
        self.total_envelope_gas
    }
}

const fn system_tx_ce_gas_reserve(kind: SystemTxKind) -> u64 {
    match kind {
        SystemTxKind::OcompLifecycleBegin => OCOMP_LIFECYCLE_CE_GAS_RESERVE,
        _ => 0,
    }
}

fn system_tx_protocol_precharge(input: &SystemTxInputV2) -> Result<u64, SystemTxError> {
    let SystemTxInputV2::TeeBootstrap { payload } = input else {
        return Ok(0);
    };
    payload
        .protocol_precharge(
            &crate::tee_attestation_v1::SystemGasScheduleV1::normative(),
            &crate::tee_attestation_v1::TeeRegistryGasScheduleV1::normative(),
        )
        .map_err(|error| SystemTxError::Codec(error.to_string()))
}
