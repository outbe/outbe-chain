use alloy_primitives::B256;

use crate::{
    list::{
        ordered_list_root, streaming_ordered_list_membership_proof, OrderedListLimits,
        OrderedListProofTarget,
    },
    profile::poc_schema_limits,
    result::NodActionV1,
    ListKind,
};

/// Build the ordered Nod-action root and one membership proof per action for test fixtures.
#[must_use]
pub fn nod_action_population(actions: &[NodActionV1]) -> (B256, Vec<Vec<B256>>) {
    let limits = poc_schema_limits();
    let count = u32::try_from(actions.len()).expect("fixture Nod action count fits u32");
    let encoded = actions
        .iter()
        .map(|action| action.encode_canonical_record(&limits).unwrap())
        .collect::<Vec<_>>();
    let root = ordered_list_root(
        ListKind::NodActions,
        &encoded,
        OrderedListLimits::new(512, limits.max_bounded_bytes, 1 << 20),
    )
    .unwrap();
    let proofs = (0..count)
        .map(|ordinal| {
            streaming_ordered_list_membership_proof(
                OrderedListProofTarget::new(ListKind::NodActions, count, ordinal),
                encoded.iter(),
                limits.max_bounded_bytes,
            )
            .unwrap()
        })
        .collect();
    (root, proofs)
}
