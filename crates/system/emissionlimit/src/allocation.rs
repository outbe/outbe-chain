use alloy_primitives::U256;
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
};

/// Emission sink allocation percentages (integer, denominator = 100).
///
/// (Phase 4) replaced the legacy `(Validator 4 %, AgentReward
/// 8 %, Metadosis 88 %)` table with a five-pool split that sums to
/// 20 %, leaving 80 % for the terminal Metadosis sink. The CCA and
/// Merchant pools are pure accumulators on dedicated system addresses
/// and only AgentReward owns WAA / SRA.
pub const VALIDATOR_REWARD_PCT: u64 = 4;
pub const WAA_REWARD_PCT: u64 = 4;
pub const SRA_REWARD_PCT: u64 = 4;
pub const CCA_REWARD_PCT: u64 = 4;
pub const MERCHANT_REWARD_PCT: u64 = 4;

pub const PERCENT_DENOMINATOR: u64 = 100;

/// Typed day-emission sinks. These are fixed, hard-fork governed
/// extension points, not dynamically registered runtime plugins.
///
/// replaced the per-block 3-sink table (`Validator 4 %`,
/// `AgentReward 8 %`, `Metadosis 88 %`) with the day 6-sink table
/// `(Validator 4 %, WAA 4 %, SRA 4 %, CCA 4 %, Merchant 4 %, Metadosis
/// terminal)`. The validator pool is forwarded to `outbe-rewards::api`
/// by the Cycle handler; WAA / SRA / CCA / Merchant are routed through
/// `outbe_agentreward::distribute_daily`; the residue and the terminal
/// 80 % land on Metadosis through [`crate::block::dispatch_terminal_remainder_at`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmissionSinkId {
    Validator,
    Waa,
    Sra,
    Cca,
    Merchant,
    Metadosis,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmissionSinkSpec {
    pub id: EmissionSinkId,
    /// Fixed percentage of the day cap. `None` marks the terminal
    /// remainder sink.
    pub pct: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmissionAllocation {
    pub id: EmissionSinkId,
    pub amount: U256,
}

pub const ACTIVE_EMISSION_SINKS: [EmissionSinkSpec; 6] = [
    EmissionSinkSpec {
        id: EmissionSinkId::Validator,
        pct: Some(VALIDATOR_REWARD_PCT),
    },
    EmissionSinkSpec {
        id: EmissionSinkId::Waa,
        pct: Some(WAA_REWARD_PCT),
    },
    EmissionSinkSpec {
        id: EmissionSinkId::Sra,
        pct: Some(SRA_REWARD_PCT),
    },
    EmissionSinkSpec {
        id: EmissionSinkId::Cca,
        pct: Some(CCA_REWARD_PCT),
    },
    EmissionSinkSpec {
        id: EmissionSinkId::Merchant,
        pct: Some(MERCHANT_REWARD_PCT),
    },
    EmissionSinkSpec {
        id: EmissionSinkId::Metadosis,
        pct: None,
    },
];

pub fn active_emission_sinks() -> &'static [EmissionSinkSpec] {
    &ACTIVE_EMISSION_SINKS
}

/// Allocates emission across the active static sink table.
pub fn allocate_emission(total: U256) -> Result<Vec<EmissionAllocation>> {
    allocate_emission_with_specs(total, active_emission_sinks())
}

/// Allocates emission across a static sink table.
///
/// Fixed percentage sinks are rounded down with integer arithmetic. The single
/// terminal sink receives all remaining dust and unallocated percentage.
pub fn allocate_emission_with_specs(
    total: U256,
    specs: &[EmissionSinkSpec],
) -> Result<Vec<EmissionAllocation>> {
    validate_sink_specs(specs)?;

    let hundred = U256::from(PERCENT_DENOMINATOR);
    let mut fixed_total = U256::ZERO;
    let mut allocations = Vec::with_capacity(specs.len());

    for spec in specs {
        let amount = match spec.pct {
            Some(pct) => {
                let amount = total * U256::from(pct) / hundred;
                fixed_total += amount;
                amount
            }
            None => total - fixed_total,
        };

        allocations.push(EmissionAllocation {
            id: spec.id,
            amount,
        });
    }

    Ok(allocations)
}

/// Applies non-terminal sinks under local checkpoints and sends all failed or
/// unused amounts to the terminal sink.
pub fn dispatch_allocations(
    storage: StorageHandle,
    allocations: &[EmissionAllocation],
    mut apply: impl FnMut(EmissionSinkId, U256) -> Result<U256>,
) -> Result<()> {
    let Some((terminal, non_terminal)) = allocations.split_last() else {
        return Ok(());
    };

    let mut terminal_extra = U256::ZERO;

    for allocation in non_terminal {
        if allocation.amount.is_zero() {
            continue;
        }

        match storage.with_checkpoint(|| {
            let unused_amount = apply(allocation.id, allocation.amount)?;
            if unused_amount > allocation.amount {
                return Err(PrecompileError::Revert(
                    "emission sink returned more unused amount than it received".into(),
                ));
            }
            Ok(unused_amount)
        }) {
            Ok(unused_amount) => terminal_extra += unused_amount,
            Err(error) => {
                tracing::warn!(
                    target: "outbe::emissionlimit",
                    sink = ?allocation.id,
                    amount = %allocation.amount,
                    error = %error,
                    "non-terminal emission sink failed; rolling allocation into terminal sink"
                );
                terminal_extra += allocation.amount;
            }
        }
    }

    let terminal_amount = terminal.amount + terminal_extra;
    if terminal_amount.is_zero() {
        return Ok(());
    }

    let terminal_unused = apply(terminal.id, terminal_amount)?;
    if !terminal_unused.is_zero() {
        return Err(PrecompileError::Revert(
            "terminal emission sink returned unused amount".into(),
        ));
    }

    Ok(())
}

fn validate_sink_specs(specs: &[EmissionSinkSpec]) -> Result<()> {
    if specs.is_empty() {
        return Ok(());
    }

    let mut fixed_pct_sum = 0u64;
    let mut terminal_index = None;

    for (idx, spec) in specs.iter().enumerate() {
        match spec.pct {
            Some(pct) => {
                fixed_pct_sum = fixed_pct_sum.checked_add(pct).ok_or_else(|| {
                    PrecompileError::Revert("emission fixed percentage overflow".into())
                })?;
            }
            None => {
                if terminal_index.replace(idx).is_some() {
                    return Err(PrecompileError::Revert(
                        "emission sink table must have one terminal sink".into(),
                    ));
                }
            }
        }
    }

    if terminal_index != Some(specs.len() - 1) {
        return Err(PrecompileError::Revert(
            "emission terminal sink must be the final sink".into(),
        ));
    }

    if fixed_pct_sum > PERCENT_DENOMINATOR {
        return Err(PrecompileError::Revert(
            "emission fixed percentages exceed 100".into(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, Address};
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;

    const CHAIN_ID: u64 = 1;
    const TEST_ADDRESS: Address = address!("0x1111111111111111111111111111111111111111");

    #[test]
    fn test_allocation_invariant() {
        let total = U256::from(16384u64) * U256::from(10u64).pow(U256::from(18u64));
        let allocations = allocate_emission(total).unwrap();
        let sum = allocation_sum(&allocations);
        assert_eq!(sum, total, "allocation sum must equal total");
    }

    #[test]
    fn test_allocation_percentages() {
        // Day 6-sink table: 5×4 % + Metadosis terminal 80 %.
        let total = U256::from(10000u64);
        let allocations = allocate_emission(total).unwrap();
        for sink in [
            EmissionSinkId::Validator,
            EmissionSinkId::Waa,
            EmissionSinkId::Sra,
            EmissionSinkId::Cca,
            EmissionSinkId::Merchant,
        ] {
            assert_eq!(
                allocation_for(&allocations, sink),
                U256::from(400u64),
                "{sink:?} should be 4 % of total"
            );
        }
        assert_eq!(
            allocation_for(&allocations, EmissionSinkId::Metadosis),
            U256::from(8000u64),
            "Metadosis should receive the remaining 80 %"
        );
    }

    /// Regression: allocation must preserve invariant for values > 2^53
    /// where f64 would lose precision.
    #[test]
    fn test_allocation_invariant_large_values() {
        let boundary = U256::from(9007199254740992u64);
        for offset in [0u64, 1, 7, 999, 123456789] {
            let total = boundary + U256::from(offset);
            let allocations = allocate_emission(total).unwrap();
            assert_eq!(
                allocation_sum(&allocations),
                total,
                "allocation invariant broken for total = 2^53 + {offset}"
            );
        }

        let huge = U256::from(10u64).pow(U256::from(30u64));
        let allocations = allocate_emission(huge).unwrap();
        assert_eq!(
            allocation_sum(&allocations),
            huge,
            "allocation invariant broken for 10^30"
        );
    }

    #[test]
    fn test_allocation_deterministic_exact() {
        let total = U256::from(16384u64) * U256::from(10u64).pow(U256::from(18u64));
        let first = allocate_emission(total).unwrap();
        let second = allocate_emission(total).unwrap();
        assert_eq!(first, second, "allocation must be deterministic");
    }

    #[test]
    fn test_allocation_rejects_invalid_sink_tables() {
        assert!(allocate_emission_with_specs(U256::from(100u64), &[])
            .unwrap()
            .is_empty());

        let no_terminal = [EmissionSinkSpec {
            id: EmissionSinkId::Validator,
            pct: Some(100),
        }];
        assert!(allocate_emission_with_specs(U256::from(100u64), &no_terminal).is_err());

        let two_terminals = [
            EmissionSinkSpec {
                id: EmissionSinkId::Validator,
                pct: None,
            },
            EmissionSinkSpec {
                id: EmissionSinkId::Metadosis,
                pct: None,
            },
        ];
        assert!(allocate_emission_with_specs(U256::from(100u64), &two_terminals).is_err());

        let over_allocated = [
            EmissionSinkSpec {
                id: EmissionSinkId::Validator,
                pct: Some(80),
            },
            EmissionSinkSpec {
                id: EmissionSinkId::Waa,
                pct: Some(30),
            },
            EmissionSinkSpec {
                id: EmissionSinkId::Metadosis,
                pct: None,
            },
        ];
        assert!(allocate_emission_with_specs(U256::from(100u64), &over_allocated).is_err());
    }

    #[test]
    fn test_allocation_rounding_dust_goes_to_terminal_sink() {
        // 5 × 4 % = 20 % → each non-terminal sink gets floor(101 * 4 / 100) = 4.
        // Sum of fixed shares = 20. Metadosis terminal absorbs the
        // remainder = 81, including the rounding dust (101 - 20 = 81).
        let total = U256::from(101u64);
        let allocations = allocate_emission(total).unwrap();

        for sink in [
            EmissionSinkId::Validator,
            EmissionSinkId::Waa,
            EmissionSinkId::Sra,
            EmissionSinkId::Cca,
            EmissionSinkId::Merchant,
        ] {
            assert_eq!(
                allocation_for(&allocations, sink),
                U256::from(4u64),
                "{sink:?} should be 4 % of total"
            );
        }
        assert_eq!(
            allocation_for(&allocations, EmissionSinkId::Metadosis),
            U256::from(81u64),
            "Metadosis terminal absorbs the dust"
        );
        assert_eq!(allocation_sum(&allocations), total);
    }

    #[test]
    fn test_non_terminal_sink_failure_falls_back_to_terminal() {
        let allocations = [
            EmissionAllocation {
                id: EmissionSinkId::Validator,
                amount: U256::from(100u64),
            },
            EmissionAllocation {
                id: EmissionSinkId::Waa,
                amount: U256::from(200u64),
            },
            EmissionAllocation {
                id: EmissionSinkId::Metadosis,
                amount: U256::from(700u64),
            },
        ];
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);

        StorageHandle::enter(&mut storage, |storage| {
            dispatch_allocations(storage.clone(), &allocations, |id, amount| match id {
                EmissionSinkId::Validator => {
                    storage.sstore(TEST_ADDRESS, U256::from(1u64), amount)?;
                    Err(PrecompileError::Revert("validator sink failed".into()))
                }
                EmissionSinkId::Waa => {
                    storage.sstore(TEST_ADDRESS, U256::from(2u64), amount)?;
                    Ok(U256::ZERO)
                }
                EmissionSinkId::Metadosis => {
                    storage.sstore(TEST_ADDRESS, U256::from(3u64), amount)?;
                    Ok(U256::ZERO)
                }
                EmissionSinkId::Sra | EmissionSinkId::Cca | EmissionSinkId::Merchant => {
                    Ok(U256::ZERO)
                }
            })
            .unwrap();

            assert_eq!(
                storage.sload(TEST_ADDRESS, U256::from(1u64)).unwrap(),
                U256::ZERO,
                "failed sink writes must be reverted"
            );
            assert_eq!(
                storage.sload(TEST_ADDRESS, U256::from(2u64)).unwrap(),
                U256::from(200u64)
            );
            assert_eq!(
                storage.sload(TEST_ADDRESS, U256::from(3u64)).unwrap(),
                U256::from(800u64),
                "terminal sink must receive base remainder plus failed sink amount"
            );
        });
    }

    #[test]
    fn test_non_terminal_unused_amount_falls_back_to_terminal() {
        let allocations = [
            EmissionAllocation {
                id: EmissionSinkId::Validator,
                amount: U256::from(1000u64),
            },
            EmissionAllocation {
                id: EmissionSinkId::Metadosis,
                amount: U256::from(9000u64),
            },
        ];
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);

        StorageHandle::enter(&mut storage, |storage| {
            dispatch_allocations(storage.clone(), &allocations, |id, amount| match id {
                EmissionSinkId::Validator => {
                    let unused = U256::from(400u64);
                    storage.sstore(TEST_ADDRESS, U256::from(1u64), amount - unused)?;
                    Ok(unused)
                }
                EmissionSinkId::Metadosis => {
                    storage.sstore(TEST_ADDRESS, U256::from(2u64), amount)?;
                    Ok(U256::ZERO)
                }
                EmissionSinkId::Waa
                | EmissionSinkId::Sra
                | EmissionSinkId::Cca
                | EmissionSinkId::Merchant => Ok(U256::ZERO),
            })
            .unwrap();

            assert_eq!(
                storage.sload(TEST_ADDRESS, U256::from(1u64)).unwrap(),
                U256::from(600u64),
                "successful sink should keep only the used amount"
            );
            assert_eq!(
                storage.sload(TEST_ADDRESS, U256::from(2u64)).unwrap(),
                U256::from(9400u64),
                "terminal sink must receive base remainder plus unused amount"
            );
        });
    }

    #[test]
    fn test_invalid_unused_amount_reverts_sink_and_falls_back_full_amount() {
        let allocations = [
            EmissionAllocation {
                id: EmissionSinkId::Validator,
                amount: U256::from(100u64),
            },
            EmissionAllocation {
                id: EmissionSinkId::Metadosis,
                amount: U256::from(900u64),
            },
        ];
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);

        StorageHandle::enter(&mut storage, |storage| {
            dispatch_allocations(storage.clone(), &allocations, |id, amount| match id {
                EmissionSinkId::Validator => {
                    storage.sstore(TEST_ADDRESS, U256::from(1u64), amount)?;
                    Ok(amount + U256::from(1u64))
                }
                EmissionSinkId::Metadosis => {
                    storage.sstore(TEST_ADDRESS, U256::from(2u64), amount)?;
                    Ok(U256::ZERO)
                }
                EmissionSinkId::Waa
                | EmissionSinkId::Sra
                | EmissionSinkId::Cca
                | EmissionSinkId::Merchant => Ok(U256::ZERO),
            })
            .unwrap();

            assert_eq!(
                storage.sload(TEST_ADDRESS, U256::from(1u64)).unwrap(),
                U256::ZERO,
                "invalid unused amount must revert the non-terminal sink"
            );
            assert_eq!(
                storage.sload(TEST_ADDRESS, U256::from(2u64)).unwrap(),
                U256::from(1000u64),
                "terminal sink must receive the full invalid sink allocation"
            );
        });
    }

    #[test]
    fn test_terminal_sink_failure_is_fatal_to_pipeline() {
        let allocations = [
            EmissionAllocation {
                id: EmissionSinkId::Validator,
                amount: U256::from(100u64),
            },
            EmissionAllocation {
                id: EmissionSinkId::Metadosis,
                amount: U256::from(900u64),
            },
        ];
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);

        StorageHandle::enter(&mut storage, |storage| {
            let result =
                dispatch_allocations(storage.clone(), &allocations, |id, amount| match id {
                    EmissionSinkId::Validator => {
                        storage.sstore(TEST_ADDRESS, U256::from(1u64), amount)?;
                        Ok(U256::ZERO)
                    }
                    EmissionSinkId::Metadosis => {
                        storage.sstore(TEST_ADDRESS, U256::from(2u64), amount)?;
                        Err(PrecompileError::Revert("metadosis sink failed".into()))
                    }
                    EmissionSinkId::Waa
                    | EmissionSinkId::Sra
                    | EmissionSinkId::Cca
                    | EmissionSinkId::Merchant => Ok(U256::ZERO),
                });

            assert!(result.is_err(), "terminal failure must fail the pipeline");
        });
    }

    #[test]
    fn test_terminal_unused_amount_is_fatal_to_pipeline() {
        let allocations = [EmissionAllocation {
            id: EmissionSinkId::Metadosis,
            amount: U256::from(1000u64),
        }];
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);

        StorageHandle::enter(&mut storage, |storage| {
            let result = dispatch_allocations(storage.clone(), &allocations, |_, amount| {
                storage.sstore(TEST_ADDRESS, U256::from(1u64), amount)?;
                Ok(U256::from(1u64))
            });

            assert!(result.is_err(), "terminal sink cannot return unused amount");
        });
    }

    fn allocation_sum(allocations: &[EmissionAllocation]) -> U256 {
        allocations
            .iter()
            .map(|allocation| allocation.amount)
            .fold(U256::ZERO, |acc, amount| acc + amount)
    }

    fn allocation_for(allocations: &[EmissionAllocation], id: EmissionSinkId) -> U256 {
        allocations
            .iter()
            .find(|allocation| allocation.id == id)
            .map(|allocation| allocation.amount)
            .unwrap_or(U256::ZERO)
    }
}
