use super::*;

fn leg(chain_id: u32, series: u32, recipients: usize) -> runtime::IssuanceLeg {
    let mut payload = crate::sol_ext::IOriginRouter::IssuanceInstructionsParams {
        seriesId: sid(series).into(),
        worldwideDay: series,
        issuedIntexCount: 1,
        promisLoadMinor: PROMIS_LOAD_MINOR,
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
    runtime::IssuanceLeg { chain_id, payload }
}

fn shape(
    messages: &[(
        u32,
        Vec<crate::sol_ext::IOriginRouter::IssuanceInstructionsParams>,
    )],
) -> Vec<(u32, usize, usize)> {
    messages
        .iter()
        .map(|(chain, series)| {
            (
                *chain,
                series.len(),
                series.iter().map(|s| s.recipients.len()).sum(),
            )
        })
        .collect()
}

#[test]
fn a_chains_series_travel_together_up_to_the_message_caps() {
    // Nine empty-recipient series on one chain: the series cap alone splits them.
    let legs: Vec<_> = (1..=9u32).map(|s| leg(10, s, 0)).collect();
    assert_eq!(
        shape(&runtime::pack_issuance_messages(legs)),
        vec![(10, MAX_SERIES_PER_MESSAGE, 0), (10, 1, 0)]
    );
}

#[test]
fn a_message_never_carries_more_winners_than_the_wire_allows() {
    // Two series of 40 winners each: together they exceed the recipient cap, so the
    // second starts a new message rather than overfilling the first.
    let legs = vec![leg(10, 1, 40), leg(10, 2, 40)];
    assert_eq!(
        shape(&runtime::pack_issuance_messages(legs)),
        vec![(10, 1, 40), (10, 1, 40)]
    );
}

#[test]
fn one_series_with_more_winners_than_a_message_spans_several() {
    // 150 winners of one series on one chain: three messages, and every piece repeats
    // the series so whichever arrives first can create it.
    let messages = runtime::pack_issuance_messages(vec![leg(10, 1, 150)]);
    assert_eq!(
        shape(&messages),
        vec![
            (10, 1, MAX_RECIPIENTS_PER_MESSAGE),
            (10, 1, MAX_RECIPIENTS_PER_MESSAGE),
            (10, 1, 150 - 2 * MAX_RECIPIENTS_PER_MESSAGE)
        ]
    );
    for (_, series) in &messages {
        assert_eq!(SeriesId::from(series[0].seriesId), sid(1));
    }
}

#[test]
fn a_chains_series_batch_even_when_another_chain_comes_between_them() {
    // A day emits its legs series by series, so one chain's legs are never adjacent.
    // Both of chain 10's series still travel together, or the batching would do
    // nothing precisely when a day has several currency pairs.
    let legs = vec![leg(10, 1, 2), leg(20, 1, 3), leg(10, 2, 1)];
    assert_eq!(
        shape(&runtime::pack_issuance_messages(legs)),
        vec![(10, 2, 3), (20, 1, 3)]
    );
}

#[test]
fn a_chains_chunks_form_one_run_even_when_another_chain_interleaves() {
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
    assert_eq!(chunked[0].1[0][0].recipients.len(), 3);
}
