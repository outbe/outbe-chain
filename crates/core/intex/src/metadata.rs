use outbe_common::nft_card::{self, Card, Trait, PRICE_PRECISION};

use crate::constants::{SETTLED_TOKEN_DESCRIPTION, SETTLED_TOKEN_NAME};
use crate::schema::SeriesRecord;

/// The Settled card of a series. Its later lifecycle no longer moves settled units, so
/// the card shows no call and no deadline.
pub(crate) fn settled_token_uri(series: &SeriesRecord) -> String {
    let id = series.series_id.to_string();
    let promis_load = nft_card::amount_grouped(series.promis_load_minor, 0);
    let rows = vec![
        ("Promis Load", promis_load),
        (
            "Entry Price",
            nft_card::amount_grouped(series.entry_price_minor, PRICE_PRECISION),
        ),
        (
            "Call Price",
            nft_card::amount_grouped(series.call_price_minor, PRICE_PRECISION),
        ),
    ];
    let traits = [
        Trait::text("State", nft_card::SETTLED.label),
        Trait::integer("Worldwide Day", series.worldwide_day.value()),
        Trait::currency("Issuance Currency", series.issuance_currency),
        Trait::currency("Reference Currency", series.reference_currency),
        Trait::amount("Entry Price", series.entry_price_minor, PRICE_PRECISION),
        Trait::amount("Call Price", series.call_price_minor, PRICE_PRECISION),
        Trait::amount("Promis Load", series.promis_load_minor, 0),
    ];
    let title = SETTLED_TOKEN_NAME.to_ascii_uppercase();
    let card = Card {
        title: &title,
        subtitle: &id,
        state: nft_card::SETTLED,
        rows,
    };
    nft_card::token_uri(
        &format!("{SETTLED_TOKEN_NAME} {id}"),
        SETTLED_TOKEN_DESCRIPTION,
        &card,
        &traits,
    )
}
