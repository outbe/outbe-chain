use outbe_common::nft_card::{self, Card, Trait, AMOUNT_PRECISION, PRICE_PRECISION};

use crate::constants::{TOKEN_DESCRIPTION, TOKEN_NAME};
use crate::schema::{GemData, GemState};

pub(crate) fn token_uri(item: &GemData) -> String {
    let id = nft_card::short_id(item.gem_id);
    let settled = item.state == GemState::Settled as u8;
    let deadline = (item.called_at != 0 && !settled)
        .then(|| item.called_at + u64::from(item.call_notice_period_seconds));
    let state = match item.state {
        s if s == GemState::Qualified as u8 => nft_card::QUALIFIED,
        s if s == GemState::Called as u8 => nft_card::CALLED,
        s if s == GemState::Settled as u8 => nft_card::SETTLED,
        _ => nft_card::ISSUED,
    };

    let mut rows = vec![
        (
            "Entry Price",
            nft_card::amount_grouped(item.entry_price_minor, PRICE_PRECISION),
        ),
        (
            "Call Price",
            nft_card::amount_grouped(item.call_price_minor, PRICE_PRECISION),
        ),
        (
            "Promis Load",
            nft_card::amount_grouped(item.promis_load_minor, AMOUNT_PRECISION),
        ),
    ];
    let mut traits = vec![
        Trait::text("State", state.label),
        gem_type(item.gem_type),
        Trait::amount("Entry Price", item.entry_price_minor, PRICE_PRECISION),
        Trait::amount("Floor Price", item.floor_price_minor, PRICE_PRECISION),
        Trait::amount("Call Price", item.call_price_minor, PRICE_PRECISION),
        Trait::amount("Promis Load", item.promis_load_minor, AMOUNT_PRECISION),
        Trait::integer("Issuance Currency", item.issuance_currency),
        Trait::integer("Reference Currency", item.reference_currency),
        Trait::date("Issued At", item.issued_at),
    ];
    if let Some(deadline) = deadline {
        rows.push(("Call Deadline", nft_card::timestamp_utc(deadline)));
        traits.push(Trait::date("Called At", item.called_at));
        traits.push(Trait::date("Call Deadline", deadline));
    }

    let title = TOKEN_NAME.to_ascii_uppercase();
    let card = Card {
        title: &title,
        subtitle: &id,
        state,
        rows,
    };
    nft_card::token_uri(
        &format!("{TOKEN_NAME} {id}"),
        TOKEN_DESCRIPTION,
        &card,
        &traits,
    )
}

// Mirrors `outbe_gemfactory::schema::GemTypes`, which this crate cannot depend on.
fn gem_type(gem_type: u8) -> Trait {
    let label = match gem_type {
        0 => "Genesis",
        1 => "Validator",
        2 => "SRA",
        3 => "Wallet",
        4 => "CCA",
        5 => "Merchant",
        other => return Trait::integer("Gem Type", other),
    };
    Trait::text("Gem Type", label)
}
