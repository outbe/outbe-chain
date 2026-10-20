use super::*;
use crate::schema::terminal_outcome;
use crate::terminal::{CapacityForfeitureReceipt, MissedOfferingReceipt};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::time::WorldwideDay as WwdKey;
use outbe_tribute::runtime::RetirementOutcome;

#[test]
fn terminal_receipt_view_stays_fatal_on_unknown_stored_outcome() {
    // Anti-regression: a persisted tag outside the writer domain is storage
    // corruption. Corruption on a read path stays `Fatal` deliberately
    // (same policy as `getWorldwideDay`'s status/day-type tag checks).
    with_contract(|metadosis| {
        let wwd = WwdKey::new(20270301);
        let mut malformed = vec![0; crate::terminal::codec::COMMON_LEN];
        malformed[..6].copy_from_slice(b"OMTR\x00\x01");
        malformed[6] = 7;
        malformed[7..11].copy_from_slice(&wwd.value().to_be_bytes());
        malformed[107] = 1;
        metadosis
            .worldwide_day_terminal_receipts
            .get_bytes(&wwd)
            .write(&malformed)
            .unwrap();

        let call = IMetadosis::getWorldwideDayTerminalReceiptCall { wwd: wwd.value() };
        assert!(matches!(
            metadosis_dispatch(
                metadosis.storage.clone(),
                &call.abi_encode(),
                Address::ZERO,
                U256::ZERO,
            ),
            Err(PrecompileError::Revert(_))
        ));
    });
}

#[test]
fn terminal_receipt_writers_only_emit_known_outcomes() {
    with_contract(|metadosis| {
        let missed = WwdKey::new(20270302);
        metadosis
            .write_missed_offering_receipt(MissedOfferingReceipt {
                worldwide_day: missed,
                value_routed: U256::from(5_u8),
                promis_limit_before: U256::ZERO,
                promis_limit_after: U256::from(5_u8),
                retirement: RetirementOutcome::NotPresent,
                block_number: 7,
            })
            .unwrap();

        let forfeited = WwdKey::new(20270303);
        metadosis
            .write_capacity_forfeiture_receipt(CapacityForfeitureReceipt {
                worldwide_day: forfeited,
                max_retained_wwds: 2,
                retained_count_before: 2,
                value_routed: U256::from(9_u8),
                promis_limit_before: U256::ZERO,
                promis_limit_after: U256::from(9_u8),
                forfeited_count: 3,
                forfeited_nominal: U256::from(30_u8),
                source_generation: 1,
                retired_generation: 2,
                retirement: RetirementOutcome::Requested,
                block_number: 7,
            })
            .unwrap();

        for (wwd, expected) in [
            (missed, terminal_outcome::MISSED_OFFERING),
            (forfeited, terminal_outcome::CAPACITY_FORFEITURE),
        ] {
            let stored = metadosis.read_terminal_receipt(wwd).unwrap().unwrap();
            assert_eq!(stored.outcome(), expected);
            assert!(
                matches!(
                    stored.outcome(),
                    terminal_outcome::MISSED_OFFERING | terminal_outcome::CAPACITY_FORFEITURE
                ),
                "writers must never persist a tag the reader treats as corruption"
            );
        }
    });
}
