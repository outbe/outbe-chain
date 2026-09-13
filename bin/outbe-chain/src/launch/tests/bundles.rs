#[test]
fn ocomp_bundle_catalog_can_rotate_from_v1_v2_to_v2_v3() {
    let v1 = alloy_primitives::B256::repeat_byte(0x11);
    let v2 = alloy_primitives::B256::repeat_byte(0x22);
    let v3 = alloy_primitives::B256::repeat_byte(0x33);
    let installed = std::collections::BTreeMap::from([(v2, ()), (v3, ())]);
    let configured = format!("{v2:#x},{v3:#x}");

    let ordered = super::ordered_installed_ocomp_bundle_hashes(v1, &installed, Some(&configured))
        .expect("post-genesis adjacent authorities should not force V1");

    assert_eq!(ordered, vec![v2, v3]);
}

#[test]
fn post_genesis_two_bundle_catalog_requires_explicit_lane_order() {
    let v1 = alloy_primitives::B256::repeat_byte(0x11);
    let v2 = alloy_primitives::B256::repeat_byte(0x22);
    let v3 = alloy_primitives::B256::repeat_byte(0x33);
    let installed = std::collections::BTreeMap::from([(v2, ()), (v3, ())]);

    let error = super::ordered_installed_ocomp_bundle_hashes(v1, &installed, None)
        .expect_err("hash order must be explicit after genesis V1 is retired");

    assert!(error
        .to_string()
        .contains("OCOMP_PROTOCOL_BUNDLE_HASHES is required"));
}

#[test]
fn configured_ocomp_bundle_hashes_are_exact_lowercase_and_unique() {
    let hash = alloy_primitives::B256::repeat_byte(0xab);
    assert_eq!(
        super::parse_ocomp_bundle_hashes(&format!("{hash:#x}"))
            .expect("canonical hash should parse"),
        vec![hash]
    );
    assert!(
        super::parse_ocomp_bundle_hashes(&format!("{hash:#x},{hash:#x}"))
            .expect_err("duplicate must fail")
            .to_string()
            .contains("duplicate")
    );
    assert!(super::parse_ocomp_bundle_hashes(
        "0xABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABAB"
    )
    .expect_err("uppercase must fail")
    .to_string()
    .contains("lowercase"));
}
