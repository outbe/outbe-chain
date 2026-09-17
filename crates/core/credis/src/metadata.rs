use outbe_common::nft_card::{self, Card, Trait};
use outbe_primitives::error::Result;

use crate::constants::{TOKEN_DESCRIPTION, TOKEN_NAME};
use crate::runtime::settlement_deadline;
use crate::schema::{CredisContract, CredisState, Position};

/// The position's `tokenURI` at block time `now`; accrued interest is evaluated then.
pub(crate) fn token_uri(position: &Position, now: u64) -> Result<String> {
    let lifecycle = position.lifecycle_state()?;
    let state = match lifecycle {
        CredisState::Open => nft_card::OPEN,
        CredisState::Called => nft_card::CALLED,
        CredisState::Settled => nft_card::SETTLED,
        CredisState::Void => nft_card::VOID,
    };
    let accrued_interest = CredisContract::accrued_interest(position, now)?;

    let mut rows = vec![
        ("Principal", nft_card::amount_grouped(position.principal)),
        (
            "Outstanding",
            nft_card::amount_grouped(position.outstanding),
        ),
        (
            "Accrued Interest",
            nft_card::amount_grouped(accrued_interest),
        ),
        (
            "Entry Price",
            nft_card::amount_grouped(position.entry_price),
        ),
        ("Call Price", nft_card::amount_grouped(position.call_price)),
    ];
    let mut traits = vec![
        Trait::text("State", state.label),
        Trait::amount("Principal", position.principal),
        Trait::amount("Outstanding", position.outstanding),
        Trait::amount("Accrued Interest", accrued_interest),
        Trait::amount("Entry Price", position.entry_price),
        Trait::amount("Call Price", position.call_price),
        Trait::amount("Policy Rate", position.policy_rate),
        Trait::amount("Collateral", position.collateral),
        Trait::amount("Collateral Locked", position.collateral_locked),
        Trait::integer("Issuance Currency", position.issuance_currency),
        Trait::integer("Reference Currency", position.reference_currency),
        Trait::text("Asset", position.asset.to_string()),
        Trait::text("CCA", position.cca.to_string()),
        Trait::date("Originated At", position.originated_at),
    ];
    if lifecycle == CredisState::Called {
        let deadline = settlement_deadline(position);
        rows.push(("Settlement Deadline", nft_card::timestamp_utc(deadline)));
        traits.push(Trait::date("Called At", position.called_at));
        traits.push(Trait::date("Settlement Deadline", deadline));
    }

    let id = nft_card::short_id(position.position_id);
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
