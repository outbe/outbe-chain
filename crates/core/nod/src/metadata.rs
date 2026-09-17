use alloy_primitives::U256;
use outbe_common::nft_card::{self, Card, Trait, AMOUNT_PRECISION, PRICE_PRECISION};
use outbe_primitives::error::Result;

use crate::api;
use crate::constants::{CALL_RATE_PCT, TOKEN_DESCRIPTION, TOKEN_NAME};
use crate::schema::{NodBucketState, NodContract, NodItemState};

/// The Nod's `tokenURI` at block time `now`. Qualification and the call live on
/// the bucket, so both are read from there; a called Nod past its settlement
/// deadline reads as Expired until the call scan forfeits it.
pub(crate) fn token_uri(
    nod: &NodContract<'_>,
    item: &NodItemState,
    bucket: &NodBucketState,
    now: u64,
) -> Result<String> {
    let called_at = nod.bucket_called_at.read(&item.bucket_key)?;
    let deadline = api::settlement_deadline_of(
        called_at,
        nod.callable_bucket_call_notice_period
            .read(&item.bucket_key)?,
    );
    let sealed_call_price = nod.callable_bucket_call_price.read(&item.bucket_key)?;
    let call_price = if sealed_call_price.is_zero() {
        bucket
            .entry_price_minor
            .saturating_mul(U256::from(100 + CALL_RATE_PCT))
            / U256::from(100u64)
    } else {
        sealed_call_price
    };
    let cost_amount = api::settlement_cost_minor(bucket.entry_price_minor, item.gratis_load_minor)?;
    let called = called_at != 0 && !item.is_settled;
    let state = if item.is_settled {
        nft_card::SETTLED
    } else if called && now > deadline {
        nft_card::EXPIRED
    } else if called {
        nft_card::CALLED
    } else if bucket.is_qualified {
        nft_card::QUALIFIED
    } else {
        nft_card::ISSUED
    };

    let hex = format!("{:064x}", item.nod_id.to_u256());
    let id = format!("{}-{}", item.worldwide_day, &hex[8..16]);
    let mut rows = vec![
        (
            "Entry Price",
            nft_card::amount_grouped(bucket.entry_price_minor, PRICE_PRECISION),
        ),
        (
            "Call Price",
            nft_card::amount_grouped(call_price, PRICE_PRECISION),
        ),
        (
            "Gratis Load",
            nft_card::amount_grouped(item.gratis_load_minor, AMOUNT_PRECISION),
        ),
        (
            "Cost Amount",
            nft_card::amount_grouped(cost_amount, AMOUNT_PRECISION),
        ),
        ("Worldwide Day", item.worldwide_day.to_string()),
    ];
    let mut traits = vec![
        Trait::text("State", state.label),
        Trait::integer("Worldwide Day", item.worldwide_day.value()),
        Trait::integer("League", item.league_id),
        Trait::amount("Entry Price", bucket.entry_price_minor, PRICE_PRECISION),
        Trait::amount("Floor Price", item.floor_price_minor, PRICE_PRECISION),
        Trait::amount("Call Price", call_price, PRICE_PRECISION),
        Trait::amount("Gratis Load", item.gratis_load_minor, AMOUNT_PRECISION),
        Trait::amount("Cost Amount", cost_amount, AMOUNT_PRECISION),
        Trait::integer("Issuance Currency", item.issuance_currency),
        Trait::integer("Reference Currency", item.reference_currency),
    ];
    if called {
        traits.push(Trait::date("Called At", called_at));
        if deadline != u64::MAX {
            rows.push(("Settlement Deadline", nft_card::timestamp_utc(deadline)));
            traits.push(Trait::date("Settlement Deadline", deadline));
        }
    }

    let title = TOKEN_NAME.to_ascii_uppercase();
    let card = Card {
        title: &title,
        subtitle: &id,
        state,
        rows,
    };
    Ok(nft_card::token_uri(
        &format!("{TOKEN_NAME} {id}"),
        TOKEN_DESCRIPTION,
        &card,
        &traits,
    ))
}
