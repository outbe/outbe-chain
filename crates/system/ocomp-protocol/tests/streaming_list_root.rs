use outbe_ocomp_protocol::{
    ordered_list_root, registry::ListKind, OrderedListLimits, StreamingOrderedListRoot,
};

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
    let mut root =
        StreamingOrderedListRoot::new(ListKind::UnitSpecificationsArtifacts, 3).unwrap();
    root.push(b"first", 64).unwrap();
    root.push(b"second", 64).unwrap();
    assert!(root.finish().is_err());

    let mut root =
        StreamingOrderedListRoot::new(ListKind::UnitSpecificationsArtifacts, 1).unwrap();
    root.push(b"first", 64).unwrap();
    assert!(root.push(b"extra", 64).is_err());
}
