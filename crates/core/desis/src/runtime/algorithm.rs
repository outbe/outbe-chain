use super::*;

/// Sort chain-tagged bids: descending rate, ascending timestamp on tie. The sort is
/// stable, so remaining ties keep the snapshot's chain order. The order is deterministic.
pub(super) fn sort_bids(bids: &mut [(u32, BidData)]) {
    bids.sort_by(|(_, a), (_, b)| {
        b.intex_bid_rate
            .cmp(&a.intex_bid_rate)
            .then_with(|| a.timestamp.cmp(&b.timestamp))
    });
}

/// Native/WCOEN escrow amount for `qty` Intexes at `rate` (1e6 fixed-point)
/// against the six-decimal per-Intex escrow basis. The protocol result crosses
/// into 18-decimal payment units exactly once, saturating to u128.
pub(crate) fn rate_lock(qty: u64, basis: u128, rate: u32) -> u128 {
    let amount = (U256::from(qty)
        .saturating_mul(U256::from(basis))
        .saturating_mul(U256::from(rate))
        / U256::from(SCALE_1E6_U64))
    .saturating_mul(NATIVE_UNITS_PER_PROTOCOL_UNIT);
    u128::try_from(amount).unwrap_or(u128::MAX)
}

/// Uniform-rate clearing: allocate sorted bids until no `desis_limit_units` remain. The
/// clearing rate is the last allocated bid's. lock/pay uses the shared scale-1e6 denominator.
pub(super) fn calculate_clearing(
    bids: &[(u32, BidData)],
    config: &AuctionConfig,
    desis_limit_units: u32,
    min_qty: u16,
) -> ClearingResult {
    let len = bids.len();
    let mut winners: Vec<Address> = Vec::with_capacity(len);
    let mut winner_quantities: Vec<alloy_primitives::U256> = Vec::with_capacity(len);
    let mut winner_chains: Vec<u32> = Vec::with_capacity(len);
    let mut winner_currencies: Vec<(u16, u16)> = Vec::with_capacity(len);
    let mut partial_winner = None;
    let mut won_by_index: Vec<u32> = vec![0u32; len];

    let escrow_basis = config.escrow_basis_minor();
    let mut total_allocated: u32 = 0;
    let mut clearing_rate: u32 = config.min_intex_bid_rate;

    for (i, (chain_id, bid)) in bids.iter().enumerate() {
        if total_allocated >= desis_limit_units {
            break;
        }
        if bid.intex_bid_rate < config.min_intex_bid_rate {
            continue;
        }
        if bid.intex_quantity < min_qty {
            continue;
        }

        let allocatable = desis_limit_units - total_allocated;
        let allocated = (bid.intex_quantity as u32).min(allocatable);

        if allocated > 0 {
            if allocated < u32::from(bid.intex_quantity) {
                partial_winner = Some(winners.len());
            }
            winners.push(bid.bidder_address);
            winner_quantities.push(alloy_primitives::U256::from(allocated));
            winner_chains.push(*chain_id);
            winner_currencies.push((bid.issuance_currency, bid.reference_currency));
            won_by_index[i] = allocated;
            total_allocated += allocated;
            clearing_rate = bid.intex_bid_rate;
        }
    }

    let mut all_bidders: Vec<Address> = Vec::with_capacity(len);
    let mut refunded_amounts: Vec<u128> = Vec::with_capacity(len);
    let mut paid_amounts: Vec<u128> = Vec::with_capacity(len);
    let mut bidder_chains: Vec<u32> = Vec::with_capacity(len);

    for (i, (chain_id, bid)) in bids.iter().enumerate() {
        all_bidders.push(bid.bidder_address);
        bidder_chains.push(*chain_id);
        let (paid, refunded) = paid_and_refunded(bid, won_by_index[i], escrow_basis, clearing_rate);
        paid_amounts.push(paid);
        refunded_amounts.push(refunded);
    }

    ClearingResult {
        issued_units: total_allocated,
        clearing_rate,
        winners,
        winner_quantities,
        winner_chains,
        winner_currencies,
        partial_winner,
        all_bidders,
        refunded_amounts,
        paid_amounts,
        bidder_chains,
    }
}

fn paid_and_refunded(
    bid: &BidData,
    won: u32,
    escrow_basis: u128,
    clearing_rate: u32,
) -> (u128, u128) {
    // locked = quantity * escrow_basis * rate / 1_000_000 (escrowed at bid time).
    let locked = rate_lock(
        u64::from(bid.intex_quantity),
        escrow_basis,
        bid.intex_bid_rate,
    );
    if won > 0 {
        // Uniform clearing: winners pay at the clearing rate. Refund the rest.
        let paid = rate_lock(u64::from(won), escrow_basis, clearing_rate);
        let refunded = locked.saturating_sub(paid);
        (paid, refunded)
    } else {
        (0, locked)
    }
}
