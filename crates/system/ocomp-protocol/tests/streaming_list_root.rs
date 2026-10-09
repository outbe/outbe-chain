use outbe_ocomp_protocol::list::{
    leaf_hash, node_hash, pad_hash, streaming_ordered_list_membership_proof,
    try_streaming_ordered_list_membership_proof, verify_ordered_list_membership,
    OrderedListProofTarget,
};
use outbe_ocomp_protocol::{
    ordered_list_root, registry::ListKind, OrderedListLimits, ProtocolError,
    StreamingOrderedListRoot,
};

#[derive(Debug, Eq, PartialEq)]
enum CatalogReadError {
    Source,
    Protocol(ProtocolError),
}

impl From<ProtocolError> for CatalogReadError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

fn proof_error(
    real_count: u32,
    target_index: u32,
    items: Vec<Result<&'static [u8], CatalogReadError>>,
    max_item_bytes: usize,
) -> CatalogReadError {
    try_streaming_ordered_list_membership_proof(
        OrderedListProofTarget::new(
            ListKind::UnitSpecificationsArtifacts,
            real_count,
            target_index,
        ),
        items,
        max_item_bytes,
    )
    .unwrap_err()
}

#[test]
fn streaming_root_matches_the_frozen_ordered_list_scheme_without_a_catalog_vector() {
    let limits = OrderedListLimits::new(16, 64, 16 * 32);
    for count in 1_u32..=9 {
        let items = (0..count)
            .map(|index| format!("unit-{index}").into_bytes())
            .collect::<Vec<_>>();
        let expected =
            ordered_list_root(ListKind::UnitSpecificationsArtifacts, &items, limits).unwrap();

        let mut streaming =
            StreamingOrderedListRoot::new(ListKind::UnitSpecificationsArtifacts, count).unwrap();
        for item in &items {
            streaming.push(item, 64).unwrap();
        }
        assert_eq!(streaming.finish().unwrap(), expected);
    }
}

#[test]
fn streaming_root_requires_the_exact_declared_population() {
    let mut root = StreamingOrderedListRoot::new(ListKind::UnitSpecificationsArtifacts, 3).unwrap();
    root.push(b"first", 64).unwrap();
    root.push(b"second", 64).unwrap();
    assert!(root.finish().is_err());

    let mut root = StreamingOrderedListRoot::new(ListKind::UnitSpecificationsArtifacts, 1).unwrap();
    root.push(b"first", 64).unwrap();
    assert!(root.push(b"extra", 64).is_err());
}

#[test]
fn ordered_list_membership_binds_item_position_population_and_root() {
    let kind = ListKind::UnitSpecificationsArtifacts;
    let items = [
        b"first".as_slice(),
        b"second".as_slice(),
        b"third".as_slice(),
    ];
    let expected =
        ordered_list_root(kind, &items, OrderedListLimits::new(16, 64, 16 * 32)).unwrap();
    let left_root = node_hash(
        kind,
        1,
        0,
        leaf_hash(kind, 0, items[0]).unwrap(),
        leaf_hash(kind, 1, items[1]).unwrap(),
    )
    .unwrap();
    let proof = [pad_hash(kind, 3).unwrap(), left_root];

    verify_ordered_list_membership(
        OrderedListProofTarget::new(kind, 3, 2),
        items[2],
        &proof,
        expected,
    )
    .unwrap();
    assert!(verify_ordered_list_membership(
        OrderedListProofTarget::new(kind, 3, 2),
        b"changed",
        &proof,
        expected
    )
    .is_err());
    assert!(verify_ordered_list_membership(
        OrderedListProofTarget::new(kind, 3, 1),
        items[2],
        &proof,
        expected
    )
    .is_err());
    assert!(verify_ordered_list_membership(
        OrderedListProofTarget::new(kind, 4, 2),
        items[2],
        &proof,
        expected
    )
    .is_err());

    let mut changed_proof = proof;
    changed_proof[0] = leaf_hash(kind, 3, b"not padding").unwrap();
    assert!(verify_ordered_list_membership(
        OrderedListProofTarget::new(kind, 3, 2),
        items[2],
        &changed_proof,
        expected
    )
    .is_err());
}

#[test]
fn ordered_list_verifier_preserves_first_error_and_root_binding() {
    let kind = ListKind::UnitSpecificationsArtifacts;
    let expected = ordered_list_root(
        kind,
        &[b"first".as_slice()],
        OrderedListLimits::new(1, 64, 32),
    )
    .unwrap();
    let bounds = ProtocolError::InvalidInvariant("ordered-list membership bounds");
    let padded_count = ProtocolError::IntegerOverflow {
        what: "ordered-list membership padded count",
    };
    let proof_height = ProtocolError::InvalidInvariant("ordered-list membership proof height");
    let root = ProtocolError::InvalidInvariant("ordered-list membership root");

    assert_eq!(
        verify_ordered_list_membership(
            OrderedListProofTarget::new(kind, 0, 0),
            b"wrong",
            &[],
            expected
        )
        .unwrap_err(),
        bounds
    );
    assert_eq!(
        verify_ordered_list_membership(
            OrderedListProofTarget::new(kind, 1, 1),
            b"wrong",
            &[],
            expected
        )
        .unwrap_err(),
        bounds
    );
    assert_eq!(
        verify_ordered_list_membership(
            OrderedListProofTarget::new(kind, 1, 0),
            b"wrong",
            &[expected],
            alloy_primitives::B256::ZERO
        )
        .unwrap_err(),
        bounds
    );
    assert_eq!(
        verify_ordered_list_membership(
            OrderedListProofTarget::new(kind, u32::MAX, 0),
            b"wrong",
            &[],
            expected
        )
        .unwrap_err(),
        padded_count
    );
    assert_eq!(
        verify_ordered_list_membership(
            OrderedListProofTarget::new(kind, 3, 0),
            b"wrong",
            &[],
            expected
        )
        .unwrap_err(),
        proof_height
    );
    assert_eq!(
        verify_ordered_list_membership(
            OrderedListProofTarget::new(kind, 1, 0),
            b"wrong",
            &[],
            expected
        )
        .unwrap_err(),
        root
    );
}

#[test]
fn streaming_membership_proof_uses_exact_order_with_bounded_frontier_memory() {
    let kind = ListKind::UnitSpecificationsArtifacts;
    for count in [1_u32, 2, 3, 257] {
        for target in [0, count / 2, count - 1] {
            let proof = streaming_ordered_list_membership_proof(
                OrderedListProofTarget::new(kind, count, target),
                (0..count).map(|index| index.to_be_bytes()),
                4,
            )
            .unwrap();
            let target_item = target.to_be_bytes();
            let expected = {
                let mut root = StreamingOrderedListRoot::new(kind, count).unwrap();
                for index in 0..count {
                    root.push(&index.to_be_bytes(), 4).unwrap();
                }
                root.finish().unwrap()
            };
            verify_ordered_list_membership(
                OrderedListProofTarget::new(kind, count, target),
                &target_item,
                &proof,
                expected,
            )
            .unwrap();
            assert!(proof.len() <= u32::BITS as usize);
        }
    }
}

#[test]
fn streaming_membership_proof_rejects_invalid_population_before_item_errors() {
    let bounds = CatalogReadError::Protocol(ProtocolError::InvalidInvariant(
        "streaming ordered-list membership bounds",
    ));
    let exact_count = CatalogReadError::Protocol(ProtocolError::InvalidInvariant(
        "streaming ordered-list membership exact item count",
    ));
    assert_eq!(
        proof_error(0, 0, vec![Err(CatalogReadError::Source)], 1),
        bounds
    );
    assert_eq!(
        proof_error(1, 1, vec![Err(CatalogReadError::Source)], 1),
        bounds
    );
    assert_eq!(proof_error(2, 0, vec![Ok(b"a".as_slice())], 1), exact_count);
    assert_eq!(
        proof_error(
            1,
            0,
            vec![Ok(b"a".as_slice()), Err(CatalogReadError::Source)],
            1,
        ),
        exact_count
    );
}

#[test]
fn streaming_membership_proof_preserves_item_cap_and_catalog_error() {
    assert_eq!(
        proof_error(
            2,
            0,
            vec![Ok(b"aa".as_slice()), Err(CatalogReadError::Source)],
            1,
        ),
        CatalogReadError::Protocol(ProtocolError::CapacityExceeded {
            what: "ordered-list item bytes",
            limit: 1,
            actual: 2,
        })
    );
    assert_eq!(
        proof_error(1, 0, vec![Err(CatalogReadError::Source)], 1),
        CatalogReadError::Source
    );
}

#[test]
fn streaming_membership_proof_matches_an_independent_three_leaf_path() {
    let kind = ListKind::UnitSpecificationsArtifacts;
    let items = [
        b"first".as_slice(),
        b"second".as_slice(),
        b"third".as_slice(),
    ];
    let left_root = node_hash(
        kind,
        1,
        0,
        leaf_hash(kind, 0, items[0]).unwrap(),
        leaf_hash(kind, 1, items[1]).unwrap(),
    )
    .unwrap();
    let expected_path = [pad_hash(kind, 3).unwrap(), left_root];
    let actual_path =
        streaming_ordered_list_membership_proof(OrderedListProofTarget::new(kind, 3, 2), items, 16)
            .unwrap();

    assert_eq!(actual_path, expected_path);
    let expected_root =
        ordered_list_root(kind, &items, OrderedListLimits::new(3, 16, 128)).unwrap();
    verify_ordered_list_membership(
        OrderedListProofTarget::new(kind, 3, 2),
        items[2],
        &actual_path,
        expected_root,
    )
    .unwrap();
}
