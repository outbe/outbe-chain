//! Chunk numbering of a day's issuance: one numbered run per chain, in send order.

use alloy_primitives::{Address, U256};

use crate::runtime::{self, IssuanceLeg};
use crate::sol_ext::IOriginRouter::IssuanceInstructionsParams;

fn leg(chain_id: u32, series: u32, recipients: usize) -> IssuanceLeg {
    let mut payload = IssuanceInstructionsParams {
        seriesId: [series as u8; 14].into(),
        worldwideDay: 20_260_101,
        issuedIntexCount: 1,
        promisLoadMinor: 1,
        entryPriceMinor: 0,
        floorPriceMinor: 0,
        callNoticePeriod: 0,
        issuanceCurrency: 840,
        referenceCurrency: 840,
        callWindow: 0,
        callThreshold: 0,
        callPriceMinor: 0,
        recipients: Vec::new(),
        quantities: Vec::new(),
    };
    for i in 0..recipients {
        payload
            .recipients
            .push(Address::from([(i % 250) as u8 + 1; 20]));
        payload.quantities.push(U256::from(1u64));
    }
    IssuanceLeg { chain_id, payload }
}

#[test]
fn a_chains_messages_form_one_run_even_when_another_chain_interleaves() {
    // Chain 10's two full messages are separated by chain 20's in the packed order; the
    // chunk run must still be 10:[0/2, 1/2] and 20:[0/1].
    let packed =
        runtime::pack_issuance_messages(vec![leg(10, 1, 64), leg(20, 1, 5), leg(10, 2, 64)]);
    let chunked = runtime::chunk_issuance_messages(packed);

    let shape: Vec<(u32, Vec<usize>)> = chunked
        .iter()
        .map(|(chain, messages)| {
            (
                *chain,
                messages
                    .iter()
                    .map(|m| m.iter().map(|s| s.recipients.len()).sum())
                    .collect(),
            )
        })
        .collect();
    assert_eq!(shape, vec![(10, vec![64, 64]), (20, vec![5])]);
}

#[test]
fn a_single_message_day_is_chunk_zero_of_one() {
    let chunked =
        runtime::chunk_issuance_messages(runtime::pack_issuance_messages(vec![leg(7, 1, 3)]));
    assert_eq!(chunked.len(), 1);
    assert_eq!(chunked[0].0, 7);
    assert_eq!(chunked[0].1.len(), 1);
}
