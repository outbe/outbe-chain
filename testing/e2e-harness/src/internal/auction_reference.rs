//! Bounded uniform auction reference using only submitted bids and input terms.

use alloy_primitives::{Address, U256};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Bid {
    pub chain_id: u64,
    pub bidder: Address,
    pub quantity: u16,
    pub rate: u32,
    pub timestamp: u32,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Allocation {
    pub bid: Bid,
    pub units: u32,
    pub locked: U256,
    pub paid: U256,
    pub refund: U256,
}

#[derive(Debug, Serialize)]
pub(crate) struct Clearing {
    pub rate: u32,
    pub units: u32,
    pub demand: u64,
    pub allocations: Vec<Allocation>,
}

fn payment(quantity: u32, load: u128, rate: u32) -> U256 {
    let protocol_units = U256::from(quantity)
        .checked_mul(U256::from(load))
        .and_then(|v| v.checked_mul(U256::from(rate)))
        .expect("bounded fixture payment")
        / U256::from(1_000_000);
    let native = protocol_units
        .checked_mul(U256::from(1_000_000_000_000u64))
        .expect("native fixture payment");
    assert!(
        native <= U256::from(u128::MAX),
        "fixture does not exercise saturated escrow"
    );
    native
}

/// Input order is the frozen target order, then each target's submitted order.
/// Stable sorting preserves that order when rate and reveal timestamp tie.
pub(crate) fn clear(
    mut bids: Vec<Bid>,
    supply: u32,
    load: u128,
    min_rate: u32,
    min_qty: u16,
) -> Clearing {
    assert!(bids.len() <= 4, "bounded two-bidder/two-chain fixture");
    assert!(load > 0);
    let demand = bids.iter().map(|b| u64::from(b.quantity)).sum();
    bids.sort_by_key(|bid| (std::cmp::Reverse(bid.rate), bid.timestamp));
    let mut remaining = supply;
    let mut rate = min_rate;
    let mut allocations = Vec::new();
    for bid in bids {
        let units = if bid.rate >= min_rate && bid.quantity >= min_qty {
            remaining.min(u32::from(bid.quantity))
        } else {
            0
        };
        remaining -= units;
        if units > 0 {
            rate = bid.rate;
        }
        allocations.push(Allocation {
            bid,
            units,
            locked: U256::ZERO,
            paid: U256::ZERO,
            refund: U256::ZERO,
        });
    }
    for allocation in &mut allocations {
        allocation.locked = payment(
            u32::from(allocation.bid.quantity),
            load,
            allocation.bid.rate,
        );
        allocation.paid = payment(allocation.units, load, rate);
        allocation.refund = allocation
            .locked
            .checked_sub(allocation.paid)
            .expect("uniform clearing payment does not exceed bid lock");
    }
    Clearing {
        rate,
        units: supply - remaining,
        demand,
        allocations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bid(chain_id: u64, quantity: u16, rate: u32, timestamp: u32) -> Bid {
        Bid {
            chain_id,
            bidder: Address::repeat_byte(chain_id as u8),
            quantity,
            rate,
            timestamp,
        }
    }

    #[test]
    fn scarcity_uses_rate_then_time_then_frozen_order_and_partial_fill() {
        let bids = vec![
            bid(1, 30, 800_000, 11),
            bid(1, 40, 700_000, 12),
            bid(2, 30, 800_000, 10),
            bid(2, 40, 700_000, 12),
        ];
        let result = clear(bids.clone(), 75, 1, 0, 1);
        assert_eq!(
            (result.rate, result.units, result.demand),
            (700_000, 75, 140)
        );
        assert_eq!(
            result
                .allocations
                .iter()
                .map(|a| (a.bid.chain_id, a.units))
                .collect::<Vec<_>>(),
            vec![(2, 30), (1, 30), (1, 15), (2, 0)]
        );
        let expected = [(24, 21, 3), (24, 21, 3), (28, 10, 18), (28, 0, 28)];
        for (actual, (locked, paid, refund)) in result.allocations.iter().zip(expected) {
            let native = U256::from(1_000_000_000_000u64);
            assert_eq!(
                (actual.locked, actual.paid, actual.refund),
                (
                    U256::from(locked) * native,
                    U256::from(paid) * native,
                    U256::from(refund) * native
                )
            );
        }
        let no_sale = clear(bids, 0, 1, 0, 1);
        assert_eq!((no_sale.rate, no_sale.units), (0, 0));
        assert!(no_sale
            .allocations
            .iter()
            .all(|a| a.paid.is_zero() && a.refund == a.locked));
    }

    #[test]
    fn minimums_exclude_bids_before_allocating_and_floor_before_native_conversion() {
        let result = clear(
            vec![
                bid(1, 1, 900_000, 1),
                bid(2, 3, 700_000, 2),
                bid(3, 3, 600_000, 3),
            ],
            10,
            7,
            700_000,
            2,
        );
        assert_eq!((result.rate, result.units), (700_000, 3));
        assert_eq!(
            result
                .allocations
                .iter()
                .map(|a| a.units)
                .collect::<Vec<_>>(),
            vec![0, 3, 0]
        );
        assert_eq!(
            result.allocations[1].paid,
            U256::from(14_000_000_000_000u64)
        );
    }
}
