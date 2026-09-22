use outbe_common::nft_card::{self, Card, Trait, AMOUNT_PRECISION, PRICE_PRECISION};
use outbe_primitives::error::Result;

use crate::api;
use crate::constants::{TOKEN_DESCRIPTION, TOKEN_NAME};
use crate::schema::{NodBucketState, NodContract, NodItemState};

/// Qualification, the call and the sealed call terms live on the bucket, as in `nodData`.
pub(crate) fn token_uri(
    nod: &NodContract<'_>,
    item: &NodItemState,
    bucket: &NodBucketState,
) -> Result<String> {
    let called_at = nod.bucket_called_at.read(&item.bucket_key)?;
    let terms = nod.read_call_terms(item.bucket_key)?;
    let deadline = api::settlement_deadline_of(called_at, terms.call_notice_period);
    let call_price = terms.call_price;
    let called = called_at != 0 && !item.is_settled;
    let state = if item.is_settled {
        nft_card::SETTLED
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
            "Gratis Load",
            nft_card::amount_grouped(item.gratis_load_minor, AMOUNT_PRECISION),
        ),
        (
            "Entry Price",
            nft_card::amount_grouped(bucket.entry_price_minor, PRICE_PRECISION),
        ),
    ];
    if state == nft_card::ISSUED {
        rows.push((
            "Floor Price",
            nft_card::amount_grouped(item.floor_price_minor, PRICE_PRECISION),
        ));
    }
    rows.push((
        "Call Price",
        nft_card::amount_grouped(call_price, PRICE_PRECISION),
    ));
    let mut traits = vec![
        Trait::text("State", state.label),
        Trait::integer("Worldwide Day", item.worldwide_day.value()),
        Trait::integer("League", item.league_id),
        Trait::amount("Entry Price", bucket.entry_price_minor, PRICE_PRECISION),
        Trait::amount("Floor Price", item.floor_price_minor, PRICE_PRECISION),
        Trait::amount("Call Price", call_price, PRICE_PRECISION),
        Trait::amount("Gratis Load", item.gratis_load_minor, AMOUNT_PRECISION),
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
