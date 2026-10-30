use outbe_common::nft_card::{self, Card, Trait, AMOUNT_PRECISION, PRICE_PRECISION};

use crate::constants::{TOKEN_DESCRIPTION, TOKEN_NAME};
use crate::schema::{GemState, GemView};

pub(crate) fn token_uri(view: &GemView, now: u64) -> String {
    let item = &view.item;
    let id = nft_card::short_id(item.gem_id);
    let settled = item.is_settled();
    let called_at = view.called_at();
    let deadline = (called_at != 0 && !settled).then(|| view.settlement_deadline());
    let state = match view.effective_state(now) {
        s if s == GemState::Forfeited as u8 => nft_card::FORFEITED,
        s if s == GemState::Called as u8 => nft_card::CALLED,
        s if s == GemState::Settled as u8 => nft_card::SETTLED,
        _ => nft_card::ISSUED,
    };

    let mut rows = vec![
        (
            "Promis Load",
            nft_card::amount_grouped(item.promis_load_minor, AMOUNT_PRECISION),
        ),
        (
            "Entry Price",
            nft_card::amount_grouped(item.entry_price_minor, PRICE_PRECISION),
        ),
    ];
    rows.push((
        "Call Price",
        nft_card::amount_grouped(view.terms.call_price_minor, PRICE_PRECISION),
    ));
    let mut traits = vec![
        Trait::text("State", state.label),
        Trait::text(
            "Series",
            format!("{}-{}", item.series_id, item.sub_series_id),
        ),
        Trait::amount("Entry Price", item.entry_price_minor, PRICE_PRECISION),
        Trait::amount("Call Price", view.terms.call_price_minor, PRICE_PRECISION),
        Trait::amount("Promis Load", item.promis_load_minor, AMOUNT_PRECISION),
        Trait::currency("Issuance Currency", item.issuance_currency),
        Trait::currency("Reference Currency", item.reference_currency),
        Trait::date("Issued At", item.issued_at),
    ];
    if let Some(deadline) = deadline {
        rows.push(("Call Deadline", nft_card::timestamp_utc(deadline)));
        traits.push(Trait::date("Called At", called_at));
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
