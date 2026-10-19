use alloy_primitives::U256;
use outbe_common::nft_card::{self, Card, Trait, PRICE_PRECISION};
use outbe_primitives::error::Result;

use crate::api;
use crate::constants::{TOKEN_DESCRIPTION, TOKEN_NAME};
use crate::schema::{EffectiveState, NodContract, NodGroupState, NodItemState};

/// The call and the sealed call terms live on the group, as in `getNod`.
pub(crate) fn token_uri(
    nod: &NodContract<'_>,
    item: &NodItemState,
    group: &NodGroupState,
    now: U256,
) -> Result<String> {
    let called_at = nod.group_called_at.read(&item.group_key)?;
    let terms = nod.read_call_terms(item.group_key)?;
    let deadline = api::settlement_deadline_of(called_at, terms.call_notice_period_seconds);
    let call_price = terms.call_price_minor;
    let called = called_at != 0 && !item.is_settled;
    let state = match api::effective_state(item, called_at, deadline, now) {
        EffectiveState::Settled => nft_card::SETTLED,
        EffectiveState::Forfeited => nft_card::FORFEITED,
        EffectiveState::Called => nft_card::CALLED,
        EffectiveState::Issued => nft_card::ISSUED,
    };

    let hex = format!("{:064x}", item.nod_id.to_u256());
    let id = format!("{}-{}", item.worldwide_day, &hex[8..16]);
    let mut rows = vec![
        ("Gratis Load", "Encrypted".into()),
        (
            "Entry Price",
            nft_card::amount_grouped(group.entry_price_minor, PRICE_PRECISION),
        ),
    ];
    rows.push((
        "Call Price",
        nft_card::amount_grouped(call_price, PRICE_PRECISION),
    ));
    let mut traits = vec![
        Trait::text("State", state.label),
        Trait::integer("Worldwide Day", item.worldwide_day.value()),
        Trait::integer("League", item.league_id),
        Trait::amount("Entry Price", group.entry_price_minor, PRICE_PRECISION),
        Trait::amount("Call Price", call_price, PRICE_PRECISION),
    ];
    traits.extend(encrypted_traits(item));
    traits.extend([
        Trait::integer("Issuance Currency", item.issuance_currency),
        Trait::integer("Reference Currency", item.reference_currency),
    ]);
    if called {
        traits.push(Trait::date("Called At", called_at));
        rows.push(("Settlement Deadline", nft_card::timestamp_utc(deadline)));
        traits.push(Trait::date("Settlement Deadline", deadline));
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

fn encrypted_traits(item: &NodItemState) -> [Trait; 4] {
    [
        Trait::text(
            "Encrypted Gratis Load",
            alloy_primitives::hex::encode_prefixed(&item.encrypted.encrypted_gratis_amount),
        ),
        Trait::text(
            "Encrypted Creator Public Key",
            alloy_primitives::hex::encode_prefixed(&item.encrypted.encrypted_creator_public_key),
        ),
        Trait::text(
            "Encryption Binding",
            item.encrypted.encryption_binding.to_string(),
        ),
        Trait::integer("Chain ID", item.encrypted.terms.chain_id),
    ]
}
