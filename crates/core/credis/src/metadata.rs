use outbe_common::nft_card::{self, Card, Trait, AMOUNT_PRECISION, PRICE_PRECISION};
use outbe_primitives::error::Result;

use crate::constants::{TOKEN_DESCRIPTION, TOKEN_NAME};
use crate::runtime::{effective_state, settlement_deadline};
use crate::schema::{Credis, CredisContract, CredisState};

/// The Credis's `tokenURI` at block time `now`. It evaluates accrued interest at `now`.
pub(crate) fn token_uri(record: &Credis, now: u64) -> Result<String> {
    let lifecycle = record.lifecycle_state()?;
    let state = match effective_state(record, now)? {
        CredisState::Issued => nft_card::ISSUED,
        CredisState::Called => nft_card::CALLED,
        CredisState::Settled => nft_card::SETTLED,
        CredisState::Forfeited => nft_card::FORFEITED,
    };
    let accrued_interest = CredisContract::accrued_interest(record, now)?;

    let mut rows = vec![
        (
            "Principal",
            nft_card::amount_grouped(record.principal_minor, AMOUNT_PRECISION),
        ),
        (
            "Outstanding",
            nft_card::amount_grouped(record.outstanding_principal_minor, AMOUNT_PRECISION),
        ),
        (
            "Accrued Interest",
            nft_card::amount_grouped(accrued_interest, AMOUNT_PRECISION),
        ),
        (
            "Entry Price",
            nft_card::amount_grouped(record.entry_price_minor, PRICE_PRECISION),
        ),
        (
            "Call Anchor",
            nft_card::amount_grouped(record.call_anchor_price_minor, PRICE_PRECISION),
        ),
        (
            "Call Price",
            nft_card::amount_grouped(record.call_price_minor, PRICE_PRECISION),
        ),
    ];
    let mut traits = vec![
        Trait::text("State", state.label),
        Trait::amount("Principal", record.principal_minor, AMOUNT_PRECISION),
        Trait::amount(
            "Outstanding",
            record.outstanding_principal_minor,
            AMOUNT_PRECISION,
        ),
        Trait::amount("Accrued Interest", accrued_interest, AMOUNT_PRECISION),
        Trait::amount("Entry Price", record.entry_price_minor, PRICE_PRECISION),
        Trait::amount(
            "Call Anchor",
            record.call_anchor_price_minor,
            PRICE_PRECISION,
        ),
        Trait::amount("Call Price", record.call_price_minor, PRICE_PRECISION),
        Trait::amount("Policy Rate", record.policy_rate, PRICE_PRECISION),
        Trait::amount("Collateral", record.gratis_minor, AMOUNT_PRECISION),
        Trait::amount(
            "Collateral Locked",
            record.outstanding_gratis_minor,
            AMOUNT_PRECISION,
        ),
        Trait::integer("Issuance Currency", record.issuance_currency),
        Trait::integer("Reference Currency", record.reference_currency),
        Trait::text("Asset", record.asset.to_string()),
        Trait::text("CCA", record.cca.to_string()),
        Trait::date("Issued At", record.issued_at),
    ];
    if lifecycle == CredisState::Called {
        let deadline = settlement_deadline(record);
        rows.push(("Settlement Deadline", nft_card::timestamp_utc(deadline)));
        traits.push(Trait::date("Called At", record.called_at));
        traits.push(Trait::date("Settlement Deadline", deadline));
    }

    let id = nft_card::short_id(record.credis_id);
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
